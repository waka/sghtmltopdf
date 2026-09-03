//! レイアウト済みのボックス木を、ページ残り高さに基づいて分割する。
//!
//! `break-before`/`break-after`/`break-inside`/`orphans`/`widows`を尊重しつつ、
//! ボックスがページに収まらない場合は以下の優先順で分割を試みる:
//! 1. `break-inside: avoid`かつ丸ごと1ページに収まる大きさなら、分割せず
//!    次ページの先頭へまるごと送る
//! 2. ブロックコンテナなら、その子ボックス単位で置き直す(各子の
//!    `break-before`/`break-after: always`もこの単位で強制改ページとして働く)
//! 3. 複数行のインラインコンテンツなら、行(line box)単位で分割する
//!    (`orphans`/`widows`を満たすよう、[`compute_orphans_widows_breaks`]が
//!    事前に分割点を調整する)
//! 4. それでも分割できない最小単位(空の要素・1行のみの内容)は次ページの
//!    先頭にまるごと送る(1ページに収まらないほど巨大な場合はそのままはみ出す)
//!
//! 子孫の`break-before`/`break-after: always`は、祖先の部分木がページ残り高さに
//! 収まる場合でも見逃してはならない(強制改ページはオーバーフローとは独立した
//! 明示的な指定のため)。[`subtree_requires_child_walk`]が、部分木内にこうした
//! 強制改ページが存在するかを事前に判定し、存在すれば「丸ごと1個のリーフとして
//! 配置する」高速経路を使わずに子要素単位の配置へフォールバックする。
//!
//! コンテナ自身がページをまたいで分割される場合でも、そのコンテナの背景・枠線は
//! 実際に子が配置された各ページごとに再現する(簡易的なボックスフラグメンテーション、
//! [`place_split`]参照)。「すでに決まった分割位置で、コンテナの装飾をどう
//! 引き継ぐか」は以下の簡易規則に従う:
//! - 上マージン/枠線/パディングは最初のフラグメントのみに適用する
//! - 下マージン/枠線/パディングは最後のフラグメントのみに適用する
//! - 左右の枠線/パディングは全フラグメントに適用する
//! - 背景色は各フラグメントの実際の内容範囲に対してそれぞれ塗る
//!
//! ページをまたがず1ページに収まり、かつ強制改ページも内包しない部分木は、
//! 元の構造を保ったまま配置される。

use std::collections::HashMap;
use std::rc::Rc;

use crate::fonts::FontCollection;
use crate::html::{Dom, NodeId};
use crate::style::{BreakBetween, BreakInside, ComputedStyle};

use crate::pdf::ImageAssetCache;

use super::block::{
    layout_document, layout_document_positioned, shift_box_x, shift_box_y, shift_box_y_in_place,
    FragmentationHints, LaidOutBox, LaidOutContent, LaidOutTable, LaidOutTableRow, PositionedBox,
    PositionedKind,
};
use super::box_tree::TableSection;
use super::box_tree::{build_box_tree, resolve_images};
use super::geometry::{EdgeSizes, FragmentPosition, Layout, Rect};
use super::grid::{LaidOutGrid, LaidOutGridRow};
use super::inline::LineBox;
use super::page::PageSettings;
use crate::style::CaptionSide;

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub boxes: Vec<LaidOutBox>,
}

/// ページ分割中の「未flushページバッファ + flush可否判定」を管理する状態。
///
/// [`place_split`]によるコンテナの装飾フラグメント(背景・枠線)挿入は、
/// そのコンテナの子要素すべてを配置し終えてから、既に`push`済みの
/// (一見「確定した」ように見える)ページへ遡って行われる(モジュールdoc
/// 参照)。そのため「新しいページが始まったら直前のページは確定」という
/// 単純な判定はできない。文書ルート自身もこの`place_split`を通るため、
/// 何も対策しなければ「文書全体の処理が終わるまでどのページも確定しない」
/// (=実質一括処理と同じ)になってしまう。
///
/// かわりに、現在処理中の(=呼び出しスタック上にある)`place_split`が
/// それぞれ「最初に触れた絶対ページインデックス」を`active_min_page`に
/// スタックとして積み、その最小値より前のページのみ安全にflushする
/// ([`Self::try_flush`])。あるページより後に開始したどのコンテナも、
/// そのページへ遡って書き込むことはない(`place_split`は自分がまたいだ
/// 範囲より前のページを触らない)ため、この判定で安全性が保証される。
/// [`PaginationState`]の永続部分。`on_flush`コールバック(ライフタイム付き)
/// を持たないため、[`StreamingPaginator`]のフィールドとして、複数の
/// `push_item`呼び出しをまたいで保持できる。
#[derive(Default)]
struct PaginationBuffer {
    /// まだflushしていない(=これ以上書き込まれないことがまだ保証できない)
    /// ページのバッファ。`buffer[0]`の絶対インデックスは`flushed`。
    buffer: Vec<Page>,
    /// これまでにflush済みのページ数(=次にflushすべきページの絶対
    /// インデックス)。
    flushed: usize,
    /// 現在アクティブな`place_split`呼び出しが最初に触れた絶対ページ
    /// インデックスのスタック(`enter_split`/`exit_split`でpush/pop)。
    active_min_page: Vec<usize>,
}

impl PaginationBuffer {
    fn new() -> Self {
        Self {
            buffer: vec![Page::default()],
            flushed: 0,
            active_min_page: Vec::new(),
        }
    }
}

/// [`PaginationBuffer`](永続部分)と、呼び出しごとに差し替え可能な
/// `on_flush`コールバックをまとめた、`place_box`等が実際に操作する対象。
///
/// [`place_split`]によるコンテナの装飾フラグメント(背景・枠線)挿入は、
/// そのコンテナの子要素すべてを配置し終えてから、既に`push`済みの
/// (一見「確定した」ように見える)ページへ遡って行われる(モジュールdoc
/// 参照)。そのため「新しいページが始まったら直前のページは確定」という
/// 単純な判定はできない。文書ルート自身もこの`place_split`を通るため、
/// 何も対策しなければ「文書全体の処理が終わるまでどのページも確定しない」
/// (=実質一括処理と同じ)になってしまう。
///
/// かわりに、現在処理中の(=呼び出しスタック上にある)`place_split`が
/// それぞれ「最初に触れた絶対ページインデックス」を`active_min_page`に
/// スタックとして積み、その最小値より前のページのみ安全にflushする
/// ([`Self::try_flush`])。あるページより後に開始したどのコンテナも、
/// そのページへ遡って書き込むことはない(`place_split`は自分がまたいだ
/// 範囲より前のページを触らない)ため、この判定で安全性が保証される。
struct PaginationState<'a> {
    inner: &'a mut PaginationBuffer,
    on_flush: &'a mut dyn FnMut(Page),
}

impl<'a> PaginationState<'a> {
    fn new(inner: &'a mut PaginationBuffer, on_flush: &'a mut dyn FnMut(Page)) -> Self {
        Self { inner, on_flush }
    }

    /// 絶対インデックス(文書全体を通じた0-originの通し番号)での現在の
    /// ページ数。
    fn len(&self) -> usize {
        self.inner.flushed + self.inner.buffer.len()
    }

    /// 現在アクティブな最後(最新)のページの絶対インデックス。
    fn current_index(&self) -> usize {
        self.len() - 1
    }

    fn last_mut(&mut self) -> &mut Page {
        self.inner
            .buffer
            .last_mut()
            .expect("バッファは常に1ページ以上を保持する")
    }

    fn last(&self) -> &Page {
        self.inner
            .buffer
            .last()
            .expect("バッファは常に1ページ以上を保持する")
    }

    /// 絶対インデックス`absolute`のページへの参照。まだflushされていない
    /// (=バッファ内にある)ことは呼び出し側が保証する
    /// (`active_min_page`で守られている範囲のみアクセスされる)。
    fn get(&self, absolute: usize) -> &Page {
        &self.inner.buffer[absolute - self.inner.flushed]
    }

    fn get_mut(&mut self, absolute: usize) -> &mut Page {
        &mut self.inner.buffer[absolute - self.inner.flushed]
    }

    /// 新しいページを開始する。
    fn push_new_page(&mut self) {
        self.inner.buffer.push(Page::default());
    }

    /// [`place_split`]に入る際、現在のページを「このコンテナが最初に
    /// 触れたページ」として記録する。
    fn enter_split(&mut self) {
        let idx = self.current_index();
        self.inner.active_min_page.push(idx);
    }

    /// [`place_split`]を抜ける際に対応する記録を取り除き、flush可能な
    /// ページがあれば`on_flush`へ渡す。
    fn exit_split(&mut self) {
        self.inner.active_min_page.pop();
        self.try_flush();
    }

    /// アクティブな`place_split`が1つもなければ最新ページ以外を、
    /// あれば「現在アクティブな全コンテナが最初に触れたページ」の最小値
    /// より前のページを、古い順に`on_flush`へ渡す。
    fn try_flush(&mut self) {
        let safe_until = self
            .inner
            .active_min_page
            .iter()
            .copied()
            .min()
            .unwrap_or_else(|| self.current_index());
        while self.inner.flushed < safe_until {
            let page = self.inner.buffer.remove(0);
            (self.on_flush)(page);
            self.inner.flushed += 1;
        }
    }
}

/// `root`(通常は[`super::layout_document`]の返り値)を、高さ`page_content_height`の
/// ページに分割する(一括版)。内部的には[`paginate_streaming`]にすべての
/// ページを`Vec`へ積ませるだけの薄いラッパー。
pub fn paginate(root: &mut LaidOutBox, page_content_height: f32) -> Vec<Page> {
    let mut result = Vec::new();
    paginate_streaming(root, page_content_height, &mut |page| result.push(page));
    result
}

/// [`paginate`]のストリーミング版。ページが確定するたびに`on_page`を呼ぶ。
///
/// 「確定」の判定は[`PaginationState`]のドキュメント参照。呼び出し元が
/// `on_page`の中でそのページに対応するDOMサブツリーを
/// [`crate::html::Dom::release_subtree`]で解放する、という使い方を想定する。
pub fn paginate_streaming(
    root: &mut LaidOutBox,
    page_content_height: f32,
    on_page: &mut dyn FnMut(Page),
) {
    let mut paginator = StreamingPaginator::new(page_content_height);
    for page in paginator.push_item(root) {
        on_page(page);
    }
    for page in paginator.finish() {
        on_page(page);
    }
}

/// 複数の`LaidOutBox`(通常は文書のトップレベルブロック要素ごと)を順に
/// [`Self::push_item`]で追加していき、[`Self::finish`]で残りのページを
/// すべてflushする、ストリーミング版のページ分割器。
///
/// [`paginate_streaming`]は「1つの完成した`LaidOutBox`ツリー全体」を一度に
/// 処理する前提だが、`StreamingPaginator`は複数回の呼び出しにまたがって、
/// 上から下へ流れる通常のページ分割(`cursor`・[`PaginationBuffer`]の
/// flush判定を含む)を継続できる。真のストリーミング入力で、`<body>`直下のトップレベル要素が確定するたびに、その要素
/// だけを`layout::layout_document_from`でレイアウトして`push_item`する、
/// という使い方を想定する。
///
/// `on_page`コールバックをフィールドとして保持せず、`push_item`/`finish`が
/// 確定したページを`Vec<Page>`として返す設計にしている(コールバックの
/// ライフタイムを構造体に持たせると、`Engine`のように複数回の呼び出しを
/// またいでこの構造体自体を保持したいユースケースで、自己参照的な借用の
/// 問題が生じるため)。
pub struct StreamingPaginator {
    buffer: PaginationBuffer,
    cursor: f32,
    page_height: f32,
    /// 直前に追加したアイテムが`break-after: always`を持っていたか。
    /// 次のアイテムを追加するときに改ページとして消費する。
    pending_break_after: bool,
}

impl StreamingPaginator {
    pub fn new(page_height: f32) -> Self {
        Self {
            buffer: PaginationBuffer::new(),
            cursor: 0.0,
            page_height,
            pending_break_after: false,
        }
    }

    /// 1つのアイテムを追加する。この呼び出しで確定したページを返す。
    ///
    /// アイテム自身の`break-before`/`break-after`はここで扱う。`place_box`が
    /// 見るのは子リストの中の強制改ページだけで、アイテム同士(=`<body>`直下の
    /// 兄弟)の関係は分割器の側にしか無いため。一括版で
    /// [`place_split`]が兄弟に対して行う判定と揃えている。
    pub fn push_item(&mut self, item: &mut LaidOutBox) -> Vec<Page> {
        let break_before =
            self.pending_break_after || item.fragmentation.break_before == BreakBetween::Always;
        // `break-after`は直後の兄弟の前で改ページするという意味なので、
        // ここでは記録だけして次の`push_item`で消費する。最後のアイテムの
        // `break-after`は`finish`が無視するため、末尾に空ページはできない。
        self.pending_break_after = item.fragmentation.break_after == BreakBetween::Always;

        let mut flushed = Vec::new();
        {
            let mut on_flush = |page: Page| flushed.push(page);
            let mut state = PaginationState::new(&mut self.buffer, &mut on_flush);
            // 現在のページに何も置かれていなければ、改ページしても空ページが
            // 増えるだけなので何もしない(先頭要素の`break-before`など)。
            if break_before && current_page_has_content(&state) {
                new_page(&mut state, &mut self.cursor);
            }
            place_box(item, self.page_height, &mut state, &mut self.cursor);
        }
        flushed
    }

    /// これ以上アイテムが無いことを伝え、残っている全ページを返す。
    pub fn finish(self) -> Vec<Page> {
        debug_assert!(
            self.buffer.active_min_page.is_empty(),
            "finishはすべてのplace_split呼び出しを抜けた後に呼ばれるはず"
        );
        self.buffer.buffer
    }
}

/// DOM+計算スタイルから、ボックスツリー構築・レイアウト・ページ分割までを一括で行う。
pub fn paginate_document(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
) -> Vec<Page> {
    let tree = build_box_tree(dom, styles);
    let (laid_out, positioned) = layout_document_positioned(
        &tree,
        styles,
        fonts,
        (settings.content_width(), settings.content_height()),
    );
    // box treeはレイアウトが終われば用済み。ページ分割中はレイアウト結果と
    // ページの両方を抱えるので、ここで手放しておかないとピークが跳ね上がる。
    drop(tree);
    let mut laid_out = laid_out;
    let mut pages = paginate(&mut laid_out, settings.content_height());
    apply_positioned_overlays(&mut pages, &positioned);
    pages
}

/// 絶対配置ボックス([`PositionedBox`])を、属するページへオーバーレイとして
/// 追加する。`page.boxes`の末尾に足すので、通常フローの上(最前面)に描かれる。
pub(crate) fn apply_positioned_overlays(pages: &mut [Page], positioned: &[PositionedBox]) {
    for pb in positioned {
        match pb.kind {
            // `fixed`は全ページのコンテンツ領域に、レイアウト座標そのまま。
            PositionedKind::Fixed => {
                for page in pages.iter_mut() {
                    page.boxes.push(pb.laid.clone());
                }
            }
            // positioned祖先が無い`absolute`は最初のページに。
            PositionedKind::AbsoluteInitial => {
                if let Some(first) = pages.first_mut() {
                    first.boxes.push(pb.laid.clone());
                }
            }
            // positioned祖先がある`absolute`は、祖先が現れたページに、祖先の
            // padding boxのページ内位置とレイアウト時位置の差分だけずらして置く。
            PositionedKind::AbsoluteAncestor {
                node,
                padding_box_origin,
            } => {
                if let Some((idx, (px, py))) = find_ancestor_padding_box_origin(pages, node) {
                    let dx = px - padding_box_origin.0;
                    let dy = py - padding_box_origin.1;
                    // `shift_box_y`のdeltaは「引く量」なので、下げるには`-dy`。
                    let shifted = shift_box_x(&shift_box_y(&pb.laid, -dy), dx);
                    pages[idx].boxes.push(shifted);
                }
            }
        }
    }
}

/// `node`が最初に現れるページと、そのpadding box左上のページ内座標を探す。
fn find_ancestor_padding_box_origin(pages: &[Page], node: NodeId) -> Option<(usize, (f32, f32))> {
    for (i, page) in pages.iter().enumerate() {
        for b in &page.boxes {
            if let Some(origin) = find_node_padding_origin(b, node) {
                return Some((i, origin));
            }
        }
    }
    None
}

fn find_node_padding_origin(b: &LaidOutBox, node: NodeId) -> Option<(f32, f32)> {
    if b.node == Some(node) {
        return Some((
            b.layout.content.x - b.layout.padding.left,
            b.layout.content.y - b.layout.padding.top,
        ));
    }
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => children
            .iter()
            .find_map(|c| find_node_padding_origin(c, node)),
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .find_map(|item| find_node_padding_origin(item, node)),
        LaidOutContent::Table(table) => table
            .caption
            .as_deref()
            .and_then(|c| find_node_padding_origin(c, node))
            .or_else(|| {
                table
                    .rows
                    .iter()
                    .flat_map(|r| &r.cells)
                    .find_map(|c| find_node_padding_origin(c, node))
            }),
        LaidOutContent::Inline(lines) => lines
            .iter()
            .flat_map(|l| &l.atomics)
            .find_map(|a| find_node_padding_origin(&a.content, node)),
        LaidOutContent::Image(_) => None,
    }
}

/// `Mode::Batch`用: 全ページを確定させてから絶対配置をオーバーレイして返す。
/// `fixed`の全ページ複製・`absolute`の祖先ページ解決は全ページが
/// 揃ってからでないとできないため、ストリーミング解放は行わない
pub fn paginate_document_with_absolutes(
    dom: &mut Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    image_cache: &ImageAssetCache,
) -> Vec<Page> {
    let mut tree = build_box_tree(dom, styles);
    resolve_images(&mut tree, dom, image_cache);
    let (laid_out, positioned) = layout_document_positioned(
        &tree,
        styles,
        fonts,
        (settings.content_width(), settings.content_height()),
    );
    let mut laid_out = laid_out;
    let mut pages = paginate(&mut laid_out, settings.content_height());
    apply_positioned_overlays(&mut pages, &positioned);
    pages
}

/// [`paginate_document`]のストリーミング版。ページが確定するたびに、
/// そのページに完全に収まった(これ以上分割されない)DOMサブツリーを
/// [`Dom::release_subtree`]で解放してから`on_page`を呼ぶ。
///
/// 現状のパイプライン(`compute_styles`→`build_box_tree`→`layout_document`は
/// いずれもDOM全体を一括で読む)では、この時点でスタイル計算・レイアウトは
/// 両方とも完了済みで、以後どのページの処理も`dom`を読み返すことはない。
/// そのため「兄弟・子孫セレクタの参照範囲を跨がない」制約は、ここでは常に
/// 満たされている(まだパースされていない後続要素が存在しないため)。
#[allow(clippy::too_many_arguments)]
pub fn paginate_document_streaming(
    dom: &mut Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    image_cache: &ImageAssetCache,
    on_page: &mut dyn FnMut(Page),
) {
    let mut tree = build_box_tree(dom, styles);
    resolve_images(&mut tree, dom, image_cache);
    let mut laid_out = layout_document(&tree, styles, fonts, settings.content_width());
    paginate_streaming(&mut laid_out, settings.content_height(), &mut |page| {
        release_completed_subtrees(dom, &page);
        on_page(page);
    });
}

/// `page`に含まれるボックスのうち、これ以上分割されない
/// (`FragmentPosition::Whole`または`Last`)ものに対応するDOMサブツリーを
/// [`Dom::release_subtree`]で解放する。
fn release_completed_subtrees(dom: &mut Dom, page: &Page) {
    for root in collect_completed_subtree_roots(page) {
        dom.release_subtree(root);
    }
}

/// `page`に含まれるボックスのうち、これ以上分割されない
/// (`FragmentPosition::Whole`または`Last`)ものに対応するDOMサブツリーの
/// ルートノードを集める。
///
/// [`release_completed_subtrees`](DOM解放)だけでなく、`Engine`が
/// `ComputedStyle`のマップから不要になったエントリを取り除く際にも同じ
/// 「もうこれ以上のページで参照されないノード」の判定が必要なため、
/// `page`を辿るロジック自体を独立させている。
pub(crate) fn collect_completed_subtree_roots(page: &Page) -> Vec<NodeId> {
    let mut roots = Vec::new();
    for b in &page.boxes {
        collect_completed_subtree_roots_in_box(b, &mut roots);
    }
    roots
}

fn collect_completed_subtree_roots_in_box(b: &LaidOutBox, roots: &mut Vec<NodeId>) {
    if let Some(node) = b.node {
        if matches!(
            b.layout.fragment,
            FragmentPosition::Whole | FragmentPosition::Last
        ) {
            // このノード以下は呼び出し元が再帰的に辿るため、子への再帰は不要。
            roots.push(node);
            return;
        }
    }
    // まだ完了していない(装飾フラグメントが`First`/`Middle`の)コンテナは
    // それ自体を完了扱いにできないが、実際に子要素が配置されたボックス
    // (`place_split`が生成する装飾フラグメントとは別に、そのページへ
    // 直接配置された子要素)は独立して完了している可能性があるため再帰する。
    match &b.content {
        LaidOutContent::Blocks(children) => {
            for child in children {
                collect_completed_subtree_roots_in_box(child, roots);
            }
        }
        // A flex container is atomic when it fits (treated the same way as
        // `display: table`). One taller than a page is split by `place_split`,
        // and its items are then placed directly on the page (a fragment is never
        // still a `Flex`), so seeing a `Flex` here always means the whole thing is
        // complete. A grid is split by row, but `place_grid` expresses the
        // completion of each fragment through `FragmentPosition`, so, as with a
        // table, there is no recursion here.
        LaidOutContent::Inline(_)
        | LaidOutContent::Table(_)
        | LaidOutContent::Flex(_)
        | LaidOutContent::Grid(_)
        | LaidOutContent::Image(_) => {}
    }
}

fn place_box(
    b: &mut LaidOutBox,
    page_height: f32,
    state: &mut PaginationState<'_>,
    cursor: &mut f32,
) {
    let height = b.layout.margin_box_height();
    let has_forced_break_inside = subtree_requires_child_walk(b);
    let orphans = b.fragmentation.orphans as usize;
    let widows = b.fragmentation.widows as usize;
    let break_inside_avoid = b.fragmentation.break_inside == BreakInside::Avoid;

    if *cursor + height <= page_height && !has_forced_break_inside {
        place_leaf(b, state, cursor);
        return;
    }

    // `break-inside: avoid`: 丸ごと(空の)1ページに収まる大きさで、かつ内部に
    // 強制改ページを内包しない場合は、分割せず次ページの先頭へまるごと送る。
    // 1ページに収まらないほど巨大な場合はこの限りではなく、best-effortで
    // 通常通り分割する(無限ループ・出力不能を避けるための例外)。
    // 現在のページに実際の内容が何もなければ(祖先のマージン分だけ`cursor`が
    // 進んでいるだけの場合を含む)、移動しても無意味なのでそのまま現在の
    // ページに置く。
    if break_inside_avoid
        && current_page_has_content(state)
        && height <= page_height
        && !has_forced_break_inside
    {
        new_page(state, cursor);
        place_leaf(b, state, cursor);
        return;
    }

    // 子を`&mut`で配る間、コンテナ自身のスカラ情報は別に控えておく。
    let mut container = SplitContainer::take_from(b);
    match &mut b.content {
        LaidOutContent::Blocks(children) if !children.is_empty() => {
            place_split(
                &mut container,
                children,
                page_height,
                state,
                cursor,
                |_i, child: &LaidOutBox| {
                    (
                        child.fragmentation.break_before == BreakBetween::Always,
                        child.fragmentation.break_after == BreakBetween::Always,
                    )
                },
                |child: &LaidOutBox| child.is_float,
                |child: &LaidOutBox| {
                    let top = margin_box_top(child);
                    (top, top + child.layout.margin_box_height())
                },
                |child, ph, ps, c| {
                    place_box(child, ph, ps, c);
                },
            );
            return;
        }
        // テーブルは行単位で分割する。これが無いと、
        // ページに収まらない行が描画されずに失われる。
        LaidOutContent::Table(table) if !table.rows.is_empty() => {
            place_table(&container, table, page_height, state, cursor);
            return;
        }
        // グリッドは行帯単位で分割する。
        LaidOutContent::Grid(grid) if grid.rows.len() > 1 => {
            place_grid(&container, grid, page_height, state, cursor);
            return;
        }
        LaidOutContent::Inline(lines) if lines.len() > 1 => {
            // `orphans`/`widows`を満たすため、行ごとの強制改ページ位置を
            // 事前に(オーバーフローによる自然な分割をシミュレートしながら)
            // 計算しておく。`place_split`が加える上マージン/枠線/パディング分
            // (`container_top_extra`)を、シミュレーションの初期カーソルにも
            // 反映しておかないと、実際の配置と分割点がずれてしまう。
            let initial_cursor = *cursor + container.top_extra();
            let forced_breaks =
                compute_orphans_widows_breaks(lines, orphans, widows, page_height, initial_cursor);
            place_split(
                &mut container,
                lines,
                page_height,
                state,
                cursor,
                // 行(line box)は`break-after`を持たない(次に置く場所は常に
                // 直後の行であり、コンテナを跨ぐ兄弟関係が無いため)。
                move |i, _line| (forced_breaks[i], false),
                // 行にfloatの概念は無い。
                |_line: &LineBox| false,
                |line: &LineBox| (line.rect.y, line.rect.y + line.rect.height),
                |line, ph, ps, c| {
                    place_line(line, ph, ps, c);
                },
            );
            return;
        }
        // A flex container is atomic as a rule: if it does not fit in what is
        // left of the page it is moved to the next page whole. A container taller
        // than a page, though, does not fit there either, and the overflow is
        // never painted and simply disappears (#18). In that case only, it is
        // split through the same path as a block, in units of bands: groups of
        // items that do not overlap vertically.
        LaidOutContent::Flex(children) if !children.is_empty() && height > page_height => {
            let mut bands = group_flex_items_into_bands(std::mem::take(children));
            place_split(
                &mut container,
                &mut bands,
                page_height,
                state,
                cursor,
                // Forced breaks are deliberately not given a meaning for bands.
                // A container that fits is atomic and never looks at the `break-*`
                // of its items, so honouring them only when it is split would make
                // the behaviour hard to predict.
                |_i, _band: &FlexBand| (false, false),
                |_band: &FlexBand| false,
                |band: &FlexBand| (band.top, band.bottom),
                |band, ph, ps, c| {
                    place_flex_band(band, ph, ps, c);
                },
            );
            return;
        }
        _ => {}
    }

    // 分割経路に入らなかったので、奪ったマーカーを戻してから最小単位として置く。
    b.marker = container.marker.take();
    // これ以上分割できない最小単位。ページに余白を使ってしまっていれば
    // 次ページの先頭へ送る(まっさらなページの先頭ならそのまま置く)。
    if *cursor > 0.0 {
        new_page(state, cursor);
    }
    place_leaf(b, state, cursor);
}

/// `b`のmargin boxの上端の絶対Y座標(`content.y`からmargin/border/padding分を
/// 引いたもの)。`place_leaf`/`extent_of`/`place_split`のfloat分岐が共通して使う。
fn margin_box_top(b: &LaidOutBox) -> f32 {
    b.layout.content.y - b.layout.padding.top - b.layout.border.top - b.layout.margin.top
}

/// `b`の部分木内(ブロックの子孫のみ、インライン行・テーブル内部は対象外)に、
/// `break-before`/`break-after: always`を持つボックスが存在するかどうか。
///
/// これが`true`の場合、`b`自身がページ残り高さに収まっていても「丸ごと1個の
/// リーフとして配置する」高速経路は使えない(強制改ページの位置を見逃して
/// しまうため)。テーブルの内部行・インライン行はここでの分割対象外
/// なので、`Blocks`のみ再帰する。
fn subtree_requires_child_walk(b: &LaidOutBox) -> bool {
    match &b.content {
        LaidOutContent::Blocks(children) => children.iter().any(|child| {
            child.fragmentation.break_before == BreakBetween::Always
                || child.fragmentation.break_after == BreakBetween::Always
                || subtree_requires_child_walk(child)
        }),
        // A flex container is atomic when it fits, and even when it is split for
        // being taller than a page the `break-*` of its items is not consulted.
        // Splitting a grid by row is `place_grid`'s job. Neither recurses here.
        LaidOutContent::Inline(_)
        | LaidOutContent::Table(_)
        | LaidOutContent::Flex(_)
        | LaidOutContent::Grid(_)
        | LaidOutContent::Image(_) => false,
    }
}

/// 複数行のインラインコンテンツを行単位で分割する際、`orphans`/`widows`を
/// 満たすよう、各行の直前に強制改ページを挿入すべきかを事前に計算する。
/// 戻り値`v`は`v[i] == true`なら「`lines[i]`の直前で改ページする」を意味する
/// (`place_split`の`break_hints`にそのまま渡す)。
///
/// オーバーフローによる自然な分割点(`place_line`が実際に行う判定と同じ、
/// `cursor > 0.0 && cursor + line.height > page_height`)を`lines`全体について
/// シミュレートしながら、各分割点で`orphans`(このページに残る行数)/`widows`
/// (次ページへ送られる行数)が足りているかを確認する:
/// - 両方満たされていれば、その自然な分割点をそのまま採用する(追加の
///   マーカーは不要。`place_line`自身が同じ判定で改ページするため)
/// - `orphans`が足りない場合、物理的にこのページへ収まる行を増やすことは
///   できないため、このページに置く予定だった行をまるごと次ページへ送る
///   (このページの先頭で強制改ページ)
/// - `orphans`は足りるが`widows`が足りない場合、分割点を
///   `lines.len() - widows`まで繰り上げる(まだ置いていない行を次ページへ
///   回すことで`widows`を確保する)
/// - 上記の繰り上げ後もなお`orphans`を満たせない場合は、`orphans`不足時と
///   同様にまるごと次ページへ送る
/// - 一度まるごと送った直後の分割点で再び条件を満たせない場合(1行だけで
///   ページの大半を占めるほど巨大な行が続く等)は、無限ループを避けるため
///   best-effortで自然な分割点を受け入れる(`orphans`/`widows`を諦める)
///
/// 行数が`orphans + widows`に満たないほど短い段落は、どの分割点を選んでも
/// 両方は満たせないため、結果的に段落全体が(現在のページに実内容があれば)
/// 次ページへまるごと送られる。
fn compute_orphans_widows_breaks(
    lines: &[LineBox],
    orphans: usize,
    widows: usize,
    page_height: f32,
    initial_cursor: f32,
) -> Vec<bool> {
    let n = lines.len();
    let mut force_break_before = vec![false; n];

    let mut cursor = initial_cursor;
    let mut page_start = 0usize;
    let mut i = 0usize;

    while i < n {
        let height = lines[i].rect.height;
        if !(cursor > 0.0 && cursor + height > page_height) {
            cursor += height;
            i += 1;
            continue;
        }

        let fit_count = i - page_start;
        let remaining = n - i;
        let orphans_ok = fit_count >= orphans;
        let widows_ok = remaining >= widows;

        if orphans_ok && widows_ok {
            // 自然な分割点をそのまま採用する(マーカーは不要)。
            page_start = i;
            cursor = 0.0;
            continue;
        }

        if force_break_before[page_start] {
            // 無限ループを避けるため、これ以上はbest-effortで自然な分割点を受け入れる。
            page_start = i;
            cursor = 0.0;
            continue;
        }

        if !orphans_ok {
            force_break_before[page_start] = true;
            cursor = 0.0;
            i = page_start;
            continue;
        }

        // orphans_ok == true, widows_ok == false: 分割点を繰り上げてwidowsを
        // 確保できないか試す。繰り上げ後もorphansを満たせないなら、
        // orphans不足時と同様にまるごと次ページへ送る。
        let candidate = n.saturating_sub(widows);
        if candidate >= page_start + orphans && candidate < i {
            force_break_before[candidate] = true;
            page_start = candidate;
            cursor = 0.0;
            i = candidate;
        } else {
            force_break_before[page_start] = true;
            cursor = 0.0;
            i = page_start;
        }
    }

    force_break_before
}

/// `b`が1ページに収まらない(または内部に強制改ページを内包する)ため、子要素
/// (`items`、`place_one`で1つずつ配置)単位で分割配置する。分割後、`b`自身の
/// 背景・枠線を各ページの実際の内容範囲に対して再現する装飾フラグメントを
/// 追加で挿入する。
///
/// `items`は`LaidOutBox`(ブロック子要素)または[`LineBox`](インライン行)のどちらか。
/// `break_hints`は各要素(とそのインデックス)について`(直前に強制改ページが
/// 必要か, 直後に強制改ページが必要か)`を返すコールバック(行には
/// `break-before`/`break-after`の概念がないため、呼び出し元は`orphans`/`widows`
/// から事前計算した配列をインデックスで引くコールバックを渡す)。
///
/// `is_float`/`item_margin_box_top`は、`items`のうちフロー外の要素(`float`)を
/// 判定するためのコールバック(`LineBox`側は常に`false`/`0.0`のダミー実装を
/// 渡す。行にfloatの概念は無い)。float項目は共有`cursor`を変更せず、
/// `shift_reference`(絶対Y→ページ内相対Yの変換係数)でシードした一時
/// カーソルを使って`place_one`へ再帰させる
/// (`place_leaf`/`place_line`/`new_page`はこの分岐のために一切変更しない)。
#[allow(clippy::too_many_arguments)]
/// 分割中のコンテナから、装飾フラグメントの生成に要る情報だけを取り出したもの。
///
/// 子(`items`)を`&mut`で配りながら、同じボックスの他のフィールドも読みたい
/// ため、借用が競合しないようスカラだけ先に複製しておく。
struct SplitContainer {
    node: Option<NodeId>,
    layout: Layout,
    has_visible_decoration: bool,
    /// マーカーは先頭フラグメントへ移す。`place_split`が取り出して使う。
    marker: Option<Box<LineBox>>,
}

impl SplitContainer {
    /// `b`からスカラ情報を取り出す(マーカーは所有権ごと奪う)。
    fn take_from(b: &mut LaidOutBox) -> Self {
        Self {
            node: b.node,
            layout: b.layout,
            has_visible_decoration: b.has_visible_decoration,
            marker: b.marker.take(),
        }
    }

    /// コンテナ自身の上マージン/枠線/パディングの合計。
    fn top_extra(&self) -> f32 {
        self.layout.margin.top + self.layout.border.top + self.layout.padding.top
    }
}

#[allow(clippy::too_many_arguments)]
fn place_split<T>(
    container: &mut SplitContainer,
    items: &mut [T],
    page_height: f32,
    state: &mut PaginationState<'_>,
    cursor: &mut f32,
    break_hints: impl Fn(usize, &T) -> (bool, bool),
    is_float: impl Fn(&T) -> bool,
    item_extent: impl Fn(&T) -> (f32, f32),
    place_one: impl Fn(&mut T, f32, &mut PaginationState<'_>, &mut f32),
) {
    let top_extra = container.top_extra();

    // Reserve room for the container's own top margin, border and padding
    // before the first fragment (no adjustment is made for the extreme case
    // where that room alone exceeds what is left of the page).
    *cursor += top_extra;

    // How much to subtract from an absolute y (the coordinates layout produced)
    // to get an in-page one (`*cursor`). The first in-flow child of a container
    // starts at the container's own content top (`b.layout.content.y`), which is
    // therefore the initial value. It is derived again from where each item
    // actually landed, so that it follows along when a page break resets
    // `*cursor`.
    let mut shift_reference = container.layout.content.y - *cursor;

    // A `b` that paints neither a background nor a border needs no decoration
    // fragment at all. Then there is nothing to track in `segments` and no
    // reason to hold pages back in `PaginationState` (`enter_split` and
    // `exit_split` are not called). Undecorated containers (`<html>`, `<body>`
    // and most wrapper `<div>`s) take this fast path, which is what makes
    // streaming flush often.
    let needs_decoration = container.has_visible_decoration;
    // An outside marker (`list-style-position: outside`) is painted on the first
    // fragment, so a fragment is still needed even without decoration. Skipping
    // it would drop the marker of an `li` split across pages.
    let needs_fragments = needs_decoration || container.marker.is_some();
    if needs_fragments {
        // Record the absolute index of the first page this container touches.
        state.enter_split();
    }

    struct Segment {
        page_index: usize,
        start_index: usize,
    }

    let mut current_page = state.current_index();
    let mut segments: Vec<Segment> = if needs_fragments {
        vec![Segment {
            page_index: current_page,
            start_index: state.get(current_page).boxes.len(),
        }]
    } else {
        Vec::new()
    };

    // Shared handling of a forced break (`break-before`/`break-after: always`):
    // start a new page and add the matching segment. `current_page` is updated
    // right here as well, so that a natural break from overflow is not counted
    // twice.
    let force_new_page = |state: &mut PaginationState<'_>,
                          cursor: &mut f32,
                          current_page: &mut usize,
                          segments: &mut Vec<Segment>| {
        new_page(state, cursor);
        *current_page = state.current_index();
        if needs_fragments {
            segments.push(Segment {
                page_index: *current_page,
                start_index: 0,
            });
        }
    };

    let item_count = items.len();
    // Right after a forced break, `shift_reference` still belongs to the
    // previous page. The next item goes to the top of the new one, so it is
    // derived again before use.
    let mut forced_page_start = false;
    for (i, item) in items.iter_mut().enumerate() {
        let (breaks_before, breaks_after) = break_hints(i, item);
        // If nothing has actually been placed on the current page (including the
        // case where `cursor` has only moved by an ancestor's margin), breaking
        // would only produce an empty page, so do nothing.
        if breaks_before && current_page_has_content(state) {
            force_new_page(state, cursor, &mut current_page, &mut segments);
            forced_page_start = true;
        }

        if is_float(item) {
            // A float takes no part in the flow, so it leaves the shared
            // `cursor` alone. Seeding a temporary cursor from `shift_reference`
            // makes the `shift = margin_box_top - *cursor` inside `place_one`
            // (that is, `place_box`) the same translation as the surrounding
            // flow, which puts the float at the right in-page position.
            let mut local_cursor = item_extent(item).0 - shift_reference;
            place_one(item, page_height, state, &mut local_cursor);
        } else {
            // Map the absolute coordinates layout produced straight into the
            // page. Stacking margin box heights instead would count margin boxes
            // that overlap through margin collapsing twice, reopening the very
            // space the collapse removed: a top margin hoisted out of a first
            // child is added once per ancestor level, and two collapsed sibling
            // margins are both added.
            if forced_page_start {
                shift_reference = item_extent(item).0 - *cursor;
                forced_page_start = false;
            }
            // Nothing above the top of the page is painted, so stop at 0.
            *cursor = (item_extent(item).0 - shift_reference).max(0.0);
            place_one(item, page_height, state, cursor);
            // `place_one` may have broken the page, so derive the factor again
            // from the result: the in-page coordinate of the item's bottom.
            shift_reference = item_extent(item).1 - *cursor;

            let now_page = state.current_index();
            if now_page != current_page {
                // Moved on to a new page. Nothing but this `b`'s own content can
                // get in, so the pages created here start at index 0.
                if needs_fragments {
                    for p in (current_page + 1)..=now_page {
                        segments.push(Segment {
                            page_index: p,
                            start_index: 0,
                        });
                    }
                }
                current_page = now_page;
            }
        }

        // Break only when there is something left to place, so that no empty
        // page is created after the last item.
        if breaks_after && i + 1 < item_count {
            force_new_page(state, cursor, &mut current_page, &mut segments);
            forced_page_start = true;
        }
    }

    // Hand the caller (the following sibling) the container's own bottom
    // (margin box) in page coordinates. Merely adding `padding-bottom` and the
    // rest would count a `margin-bottom` collapsed with the last child twice, or
    // drop an explicit `height` larger than the content. The cursor is never
    // pulled back above the bottom of the last item placed, so that overflowing
    // content does not end up under a sibling.
    let container_bottom = container.layout.content.y
        + container.layout.content.height
        + container.layout.padding.bottom
        + container.layout.border.bottom
        + container.layout.margin.bottom;
    *cursor = (container_bottom - shift_reference).max(*cursor);

    if !needs_fragments {
        return;
    }

    // Keep only the segments that actually received content (the first child
    // may have forced a break at the top of a page, for one, leaving the page
    // before it empty).
    let valid: Vec<&Segment> = segments
        .iter()
        .filter(|s| state.get(s.page_index).boxes.len() > s.start_index)
        .collect();

    let fragments: Vec<(usize, usize, LaidOutBox)> = valid
        .iter()
        .enumerate()
        .filter_map(|(i, seg)| {
            let is_first = i == 0;
            let is_last = i == valid.len() - 1;
            // An undecorated container only needs the first fragment, the one
            // carrying the marker; the rest would be empty boxes painting
            // nothing.
            if !needs_decoration && !is_first {
                return None;
            }
            let end_index = state.get(seg.page_index).boxes.len();
            let (top, bottom) =
                extent_of(&state.get(seg.page_index).boxes[seg.start_index..end_index]);
            let layout = fragment_layout(&container.layout, top, bottom, is_first, is_last);
            // The marker stays on `b`'s first fragment only, so that a box split
            // across pages does not paint it again on every later fragment. Its
            // coordinates are still the absolute ones from layout, so move them
            // into the fragment's in-page space, keeping the offset from the
            // container's content top.
            let marker = if is_first {
                container.marker.take().map(|mut marker| {
                    marker.rect.y -= container.layout.content.y - layout.content.y;
                    marker
                })
            } else {
                None
            };
            let decoration = LaidOutBox {
                node: container.node,
                layout,
                // A decoration-only fragment is never split again, so the
                // fragmentation hints mean nothing here (left at their default).
                fragmentation: FragmentationHints::default(),
                // The box itself holds no children (`Blocks(Vec::new())`) and is
                // never handed to `place_split` again. A fragment made only to
                // carry a marker paints neither background nor border.
                has_visible_decoration: needs_decoration,
                // A decoration fragment is not itself a float: even when `b` is
                // one, the fragment travels with the rest of `place_split`'s loop
                // as part of the normal flow, so this has to stay `false`.
                is_float: false,
                content: LaidOutContent::Blocks(Vec::new()),
                marker,
            };
            Some((seg.page_index, seg.start_index, decoration))
        })
        .collect();

    for (page_index, insert_index, decoration) in fragments {
        state
            .get_mut(page_index)
            .boxes
            .insert(insert_index, decoration);
    }

    // Every decoration fragment of this container is in place, so drop the
    // record. If that made more pages flushable, they go to `on_flush` here.
    state.exit_split();
}

/// `boxes`に実際に配置された子孫の、ページ内相対座標での垂直方向の union extent
/// (マージンボックスの上端の最小値・下端の最大値)を求める。
fn extent_of(boxes: &[LaidOutBox]) -> (f32, f32) {
    let mut top = f32::INFINITY;
    let mut bottom = f32::NEG_INFINITY;
    for b in boxes {
        let box_top = margin_box_top(b);
        let box_bottom = box_top + b.layout.margin_box_height();
        top = top.min(box_top);
        bottom = bottom.max(box_bottom);
    }
    (top, bottom)
}

/// コンテナ`original`のうち、1フラグメント分の装飾(背景・枠線)を描画するための
/// [`Layout`]を組み立てる。`content_y`/`content_bottom`はそのフラグメントの
/// コンテンツ領域の範囲(`is_first`なら上端はすでに`content_y`で確定済み、
/// `is_last`なら下端はまだ`padding-bottom`/`border-bottom`を含んでいない)。
///
/// `fragment`(→[`FragmentPosition`])には、`is_first`/`is_last`から求めた
/// 断片の位置を記録する。`border-radius`は計算スタイル側の値をそのまま使うため
/// (`Layout`は太さしか持たない)、レンダラ側([`crate::pdf::document`])が
/// この情報を見て「継続中の断片では角を丸めない」よう判断する。
fn fragment_layout(
    original: &Layout,
    content_y: f32,
    content_bottom: f32,
    is_first: bool,
    is_last: bool,
) -> Layout {
    let top_border = if is_first { original.border.top } else { 0.0 };
    let bottom_border = if is_last { original.border.bottom } else { 0.0 };
    let top_padding = if is_first { original.padding.top } else { 0.0 };
    let bottom_padding = if is_last {
        original.padding.bottom
    } else {
        0.0
    };
    let fragment = match (is_first, is_last) {
        (true, true) => FragmentPosition::Whole,
        (true, false) => FragmentPosition::First,
        (false, true) => FragmentPosition::Last,
        (false, false) => FragmentPosition::Middle,
    };

    Layout {
        content: Rect {
            x: original.content.x,
            y: content_y,
            width: original.content.width,
            height: (content_bottom - content_y).max(0.0),
        },
        padding: EdgeSizes {
            top: top_padding,
            right: original.padding.right,
            bottom: bottom_padding,
            left: original.padding.left,
        },
        border: EdgeSizes {
            top: top_border,
            right: original.border.right,
            bottom: bottom_border,
            left: original.border.left,
        },
        margin: EdgeSizes::default(),
        fragment,
    }
}

fn place_line(line: &LineBox, page_height: f32, state: &mut PaginationState<'_>, cursor: &mut f32) {
    if *cursor > 0.0 && *cursor + line.rect.height > page_height {
        new_page(state, cursor);
    }

    let shift = line.rect.y - *cursor;
    let mut translated = line.clone();
    translated.rect.y -= shift;
    // 行内の`display: inline-block`ボックスも行と一緒に動かす(行のrectだけを
    // 動かすと箱が元の位置に取り残される)。`shift_box_y`の`delta`は引く量
    // (`rect.y -= delta`)なので、行の`rect.y -= shift`と同じ移動量は
    // `shift`をそのまま渡せばよい。
    for atomic in translated.atomics.iter_mut() {
        atomic.content = shift_box_y(&atomic.content, shift);
    }

    let fragment = LaidOutBox {
        node: None,
        layout: Layout {
            content: translated.rect,
            ..Layout::default()
        },
        // 1行だけの合成ラッパーボックスなので、fragmentationヒントは持たない
        // (orphans/widowsの判断は呼び出し元(`place_split`)が行数単位で行う)。
        fragmentation: FragmentationHints::default(),
        has_visible_decoration: false,
        // 行(line box)にfloatの概念はない。
        is_float: false,
        content: LaidOutContent::Inline(vec![translated]),
        // 1行だけの合成ラッパー自体はlist-itemではないため常に`None`
        // (元のコンテナの`marker`は上の装飾フラグメント側で引き継ぐ)。
        marker: None,
    };
    *cursor += line.rect.height;
    state.last_mut().boxes.push(fragment);
}

/// テーブルを行単位でページへ分割して配置する。
///
/// 同じページに載る連続した行を1つの断片(`LaidOutContent::Table`を持つ
/// `LaidOutBox`)にまとめる。断片はテーブル自身のノードとジオメトリを引き
/// 継ぎ、`content.y`/`content.height`と`FragmentPosition`だけを差し替える。
/// グリッドコンテナを行帯単位でページへ配置する。`place_table`と同じ「断片を
/// 組み立てて確定する」構造で、単位が行帯になる。
///
/// 行帯の下端をまたぐアイテム(複数行にまたがるグリッドアイテム)がある境界では
/// 分割しない(テーブルの`rowspan`と同じ扱い)。
fn place_grid(
    container: &SplitContainer,
    grid: &LaidOutGrid,
    page_height: f32,
    state: &mut PaginationState<'_>,
    cursor: &mut f32,
) {
    let top_extra = container.top_extra();
    let bottom_extra = container.layout.padding.bottom
        + container.layout.border.bottom
        + container.layout.margin.bottom;

    let mut pending: Vec<LaidOutGridRow> = Vec::new();
    let mut shift = 0.0f32;
    let mut fragment_top = 0.0f32;
    let mut is_first_fragment = true;

    // 最初の断片の前に、コンテナ自身の上マージン/枠線/パディング分を確保する
    // (`place_split`/`place_table`と同じ扱い)。
    *cursor += top_extra;

    for (index, row) in grid.rows.iter().enumerate() {
        if pending.is_empty() {
            fragment_top = *cursor;
            shift = row.top - *cursor;

            // When the first row of a fragment does not fit in what is left of
            // the page, start at the top of the next one rather than overflow
            // (without this, a row laid down at the bottom of the page loses its
            // lower half). A row too tall for an empty page is placed as it is,
            // so that pagination keeps moving forward.
            if row.bottom - shift > page_height && current_page_has_content(state) {
                new_page(state, cursor);
                fragment_top = *cursor;
                shift = row.top - *cursor;
            }
        }

        // 直前の行帯からまたいでいるアイテムがあると、その境界では切れない。
        let can_break_before = index > 0 && !grid.rows[index - 1].spans_bottom;
        let row_bottom_on_page = row.bottom - shift;
        if row_bottom_on_page > page_height && !pending.is_empty() && can_break_before {
            flush_grid_fragment(
                container,
                &mut pending,
                fragment_top,
                *cursor,
                is_first_fragment,
                false,
                state,
            );
            is_first_fragment = false;
            new_page(state, cursor);
            fragment_top = *cursor;
            shift = row.top - *cursor;
        }

        pending.push(shift_grid_row_y(row, shift));
        *cursor = row.bottom - shift;
    }

    flush_grid_fragment(
        container,
        &mut pending,
        fragment_top,
        *cursor,
        is_first_fragment,
        true,
        state,
    );
    *cursor += bottom_extra;
}

/// Translates a row band, and the items inside it, along Y. As in `shift_box_y`,
/// `delta` is the amount to subtract (absolute y minus in-page y).
fn shift_grid_row_y(row: &LaidOutGridRow, delta: f32) -> LaidOutGridRow {
    LaidOutGridRow {
        items: row
            .items
            .iter()
            .map(|item| shift_box_y(item, delta))
            .collect(),
        top: row.top - delta,
        bottom: row.bottom - delta,
        spans_bottom: row.spans_bottom,
    }
}

/// [`place_grid`]が組み立てた行帯群を1つの断片としてページへ積む。
fn flush_grid_fragment(
    container: &SplitContainer,
    rows: &mut Vec<LaidOutGridRow>,
    fragment_top: f32,
    fragment_bottom: f32,
    is_first: bool,
    is_last: bool,
    state: &mut PaginationState<'_>,
) {
    if rows.is_empty() {
        return;
    }

    let mut layout = container.layout;
    layout.content.y = fragment_top
        + container.layout.margin.top
        + container.layout.border.top
        + container.layout.padding.top;
    layout.content.height = (fragment_bottom
        - fragment_top
        - container.layout.margin.top
        - container.layout.border.top
        - container.layout.padding.top)
        .max(0.0);
    layout.fragment = match (is_first, is_last) {
        (true, true) => FragmentPosition::Whole,
        (true, false) => FragmentPosition::First,
        (false, true) => FragmentPosition::Last,
        (false, false) => FragmentPosition::Middle,
    };

    let fragment = LaidOutBox {
        node: container.node,
        layout,
        fragmentation: FragmentationHints::default(),
        is_float: false,
        marker: None,
        has_visible_decoration: container.has_visible_decoration,
        content: LaidOutContent::Grid(LaidOutGrid {
            rows: std::mem::take(rows),
        }),
    };
    state.last_mut().boxes.push(fragment);
}

/// The unit a flex container taller than a page is split into: a band grouping
/// items that overlap vertically. In a column flex each item is a band; in a row
/// flex each flex line is. `top`/`bottom` are absolute y coordinates of the
/// margin box.
struct FlexBand {
    items: Vec<LaidOutBox>,
    top: f32,
    bottom: f32,
}

/// Tolerance (px) used when deciding where a band ends. The coordinates taffy
/// returns are floating point, so this keeps items that merely touch from being
/// mistaken for overlapping ones.
const FLEX_BAND_EPSILON: f32 = 0.01;

/// Groups flex items into bands by vertical overlap.
///
/// With `flex-direction: column` each item is a band; with a wrapping row flex
/// each flex line is; a row flex that does not wrap is usually a single band,
/// that is, atomic as before. The decision is purely geometric, so items of one
/// row that do not overlap vertically (`align-self` placing a short item at the
/// far end of a tall line, say) do end up in bands of their own, and a visual
/// order that differs from document order (through `order`, say) makes no
/// difference.
fn group_flex_items_into_bands(mut items: Vec<LaidOutBox>) -> Vec<FlexBand> {
    // Order by top edge (a stable sort, so items sharing a top edge keep their
    // document order).
    items.sort_by(|a, b| margin_box_top(a).total_cmp(&margin_box_top(b)));
    let mut bands: Vec<FlexBand> = Vec::new();
    for item in items {
        let top = margin_box_top(&item);
        let bottom = top + item.layout.margin_box_height();
        match bands.last_mut() {
            Some(band) if top < band.bottom - FLEX_BAND_EPSILON => {
                band.items.push(item);
                band.bottom = band.bottom.max(bottom);
            }
            _ => bands.push(FlexBand {
                items: vec![item],
                top,
                bottom,
            }),
        }
    }
    bands
}

/// Places one flex band on a page. Called from [`place_split`].
///
/// A band holding a single item (each item of a column flex) goes to
/// [`place_box`] as an ordinary box, so an item taller than a page is split
/// inside it like a block. A band with several items side by side is treated as
/// one leaf, to keep the items in the same position relative to each other: if
/// it does not fit in what is left it is moved to the top of the next page, and
/// if the band itself is taller than a page it overflows, as before.
fn place_flex_band(
    band: &mut FlexBand,
    page_height: f32,
    state: &mut PaginationState<'_>,
    cursor: &mut f32,
) {
    if let [item] = band.items.as_mut_slice() {
        place_box(item, page_height, state, cursor);
        return;
    }
    let height = band.bottom - band.top;
    if *cursor > 0.0 && *cursor + height > page_height {
        new_page(state, cursor);
    }
    let base = *cursor;
    for item in band.items.iter_mut() {
        let mut local_cursor = base + (margin_box_top(item) - band.top);
        place_leaf(item, state, &mut local_cursor);
    }
    *cursor = base + height;
}

fn place_table(
    container: &SplitContainer,
    table: &mut LaidOutTable,
    page_height: f32,
    state: &mut PaginationState<'_>,
    cursor: &mut f32,
) {
    let top_extra = container.top_extra();
    let bottom_extra = container.layout.padding.bottom
        + container.layout.border.bottom
        + container.layout.margin.bottom;

    // 断片に積む行と、その断片の絶対座標→ページ内座標への平行移動量。
    let mut pending: Vec<LaidOutTableRow> = Vec::new();
    let mut shift = 0.0f32;
    let mut fragment_top = 0.0f32;
    let mut is_first_fragment = true;

    // 2ページ目以降の先頭に複製する`<thead>`の行。見出しだけでページが埋まる
    // (=1行も進まない)場合は繰り返さない。
    // 見出し行は2ページ目以降に複製して置くので、ここだけは複製を持つ
    // (行数はたかが知れている)。本文の行は複製せずそのまま移す。
    let head_rows: Vec<LaidOutTableRow> = table
        .rows
        .iter()
        .filter(|row| row.section == TableSection::Head)
        .cloned()
        .collect();
    let head_top = head_rows.iter().map(table_row_top).fold(f32::MAX, f32::min);
    let head_bottom = head_rows
        .iter()
        .map(table_row_bottom)
        .fold(f32::MIN, f32::max);
    let head_height = if head_rows.is_empty() {
        0.0
    } else {
        head_bottom - head_top
    };
    let repeat_head = !head_rows.is_empty() && head_height < page_height;
    // `caption-side: top`のcaptionは最初の断片に付ける。
    let caption_is_top = table.caption_side == CaptionSide::Top;
    let mut pending_caption = table.caption.as_deref().filter(|_| caption_is_top).cloned();

    // 最初の断片の前に、コンテナ自身の上マージン/枠線/パディング分を確保する
    // (`place_split`と同じ扱い)。
    *cursor += top_extra;

    // captionの高さも最初の断片に含める。
    let caption_height = pending_caption
        .as_ref()
        .map(|c| c.layout.margin_box_height())
        .unwrap_or(0.0);

    let start_new_fragment = |cursor: &f32, first_row_top: f32, extra_above: f32| {
        // 断片の上端(ページ内座標)と、そこへ動かすための平行移動量。
        let top = *cursor;
        (top, first_row_top - extra_above - *cursor)
    };

    for (index, mut row) in std::mem::take(&mut table.rows).into_iter().enumerate() {
        let row_top = table_row_top(&row);
        let row_bottom = table_row_bottom(&row);
        let extra_above = if index == 0 { caption_height } else { 0.0 };

        if pending.is_empty() {
            let (top, s) = start_new_fragment(cursor, row_top, extra_above);
            fragment_top = top;
            shift = s;

            // As with the grid, start on the next page when the first row does
            // not fit in what is left (the caption height counts towards it).
            if row_bottom - shift > page_height && current_page_has_content(state) {
                new_page(state, cursor);
                let (top, s) = start_new_fragment(cursor, row_top, extra_above);
                fragment_top = top;
                shift = s;
            }
        }

        // この行を今のページに置くと溢れるなら、ここまでの断片を確定して改ページ。
        // 判定は「この断片に既に1行以上ある」こと(`current_page_has_content`では
        // ない): テーブルがページ先頭の唯一の内容でも行単位で分割する必要があり、
        // かつ空の断片を作らないことで必ず前進する。
        let row_bottom_on_page = row_bottom - shift;
        if row_bottom_on_page > page_height && !pending.is_empty() {
            flush_table_fragment(
                container,
                &mut pending,
                &mut pending_caption,
                fragment_top,
                *cursor,
                is_first_fragment,
                false,
                state,
            );
            is_first_fragment = false;
            new_page(state, cursor);
            fragment_top = *cursor;
            // 新しいページの先頭に見出し行を複製する(最初のページには元の
            // 行がそのまま置かれるので複製は2ページ目以降だけ)。
            if repeat_head {
                let head_shift = head_top - *cursor;
                for head_row in &head_rows {
                    pending.push(shift_table_row_y(head_row, head_shift));
                }
                *cursor += head_height;
            }
            shift = row_top - *cursor;
        }

        shift_table_row_y_in_place(&mut row, shift);
        pending.push(row);
        *cursor = row_bottom - shift;
    }

    // `caption-side: bottom`のcaptionは最後の断片に付ける。
    if !caption_is_top {
        if let Some(caption) = table.caption.as_deref() {
            let translated = shift_box_y(caption, shift);
            *cursor += caption.layout.margin_box_height();
            pending_caption = Some(translated);
        }
    }

    flush_table_fragment(
        container,
        &mut pending,
        &mut pending_caption,
        fragment_top,
        *cursor,
        is_first_fragment,
        true,
        state,
    );
    *cursor += bottom_extra;
}

/// [`place_table`]が組み立てた行群を1つの断片としてページへ積む。
#[allow(clippy::too_many_arguments)]
fn flush_table_fragment(
    container: &SplitContainer,
    rows: &mut Vec<LaidOutTableRow>,
    caption: &mut Option<LaidOutBox>,
    fragment_top: f32,
    fragment_bottom: f32,
    is_first: bool,
    is_last: bool,
    state: &mut PaginationState<'_>,
) {
    if rows.is_empty() && caption.is_none() {
        return;
    }

    let mut layout = container.layout;
    layout.content.y = fragment_top
        + container.layout.margin.top
        + container.layout.border.top
        + container.layout.padding.top;
    layout.content.height = (fragment_bottom
        - fragment_top
        - container.layout.margin.top
        - container.layout.border.top
        - container.layout.padding.top)
        .max(0.0);
    layout.fragment = match (is_first, is_last) {
        (true, true) => FragmentPosition::Whole,
        (true, false) => FragmentPosition::First,
        (false, true) => FragmentPosition::Last,
        (false, false) => FragmentPosition::Middle,
    };

    let fragment = LaidOutBox {
        node: container.node,
        layout,
        fragmentation: FragmentationHints::default(),
        has_visible_decoration: container.has_visible_decoration,
        is_float: false,
        content: LaidOutContent::Table(LaidOutTable {
            caption: caption.take().map(Box::new),
            caption_side: CaptionSide::Top,
            rows: std::mem::take(rows),
        }),
        marker: None,
    };
    state.last_mut().boxes.push(fragment);
}

/// テーブル行のマージンボックス上端(セルの中で最も上のもの)の絶対Y座標。
fn table_row_top(row: &LaidOutTableRow) -> f32 {
    row.cells
        .iter()
        .map(margin_box_top)
        .fold(f32::MAX, f32::min)
}

/// テーブル行のマージンボックス下端(セルの中で最も下のもの)の絶対Y座標。
fn table_row_bottom(row: &LaidOutTableRow) -> f32 {
    row.cells
        .iter()
        .map(|cell| margin_box_top(cell) + cell.layout.margin_box_height())
        .fold(f32::MIN, f32::max)
}

/// 行(のセル全部)をその場で縦に平行移動する。
fn shift_table_row_y_in_place(row: &mut LaidOutTableRow, shift: f32) {
    for cell in &mut row.cells {
        shift_box_y_in_place(cell, shift);
    }
}

/// 行(のセル全部)を縦に平行移動する。`shift`は`shift_box_y`と同じ「引く量」。
fn shift_table_row_y(row: &LaidOutTableRow, shift: f32) -> LaidOutTableRow {
    LaidOutTableRow {
        node: row.node,
        cells: row.cells.iter().map(|c| shift_box_y(c, shift)).collect(),
        section: row.section,
    }
}

/// これ以上分割しないボックスを、そのままページへ移す。
///
/// 中身(`content`/`marker`)は所有権ごと奪う。ここで複製すると、レイアウト
/// 結果とページ群が同時に存在することになり、大きな文書でピークメモリが
/// 倍増するため。奪ったあとの`b`は空の`Blocks`になり、呼び出し側は
/// 二度と中身を読まない(いずれの経路も直後にreturnする)。
fn place_leaf(b: &mut LaidOutBox, state: &mut PaginationState<'_>, cursor: &mut f32) {
    let shift = margin_box_top(b) - *cursor;
    let height = b.layout.margin_box_height();

    let mut translated = LaidOutBox {
        node: b.node,
        layout: b.layout,
        fragmentation: b.fragmentation,
        has_visible_decoration: b.has_visible_decoration,
        is_float: false,
        content: std::mem::replace(&mut b.content, LaidOutContent::Blocks(Vec::new())),
        marker: b.marker.take(),
    };
    shift_box_y_in_place(&mut translated, shift);

    *cursor += height;
    state.last_mut().boxes.push(translated);
}

fn new_page(state: &mut PaginationState<'_>, cursor: &mut f32) {
    state.push_new_page();
    // 装飾を持たないコンテナ(`enter_split`/`exit_split`を呼ばない)しか
    // 呼び出しスタックに無い場合、`exit_split`経由のflushが一度も発生しない
    // ため、新しいページが始まるたびにもここでflush判定を行う。これにより
    // 「装飾のない構造ではページが確定するそばから即座にflushされる」
    // 最も細かい粒度のストリーミングになる。
    state.try_flush();
    *cursor = 0.0;
}

/// 現在のページに実際に配置されたボックスが1つでもあるか。`cursor`は祖先の
/// マージン/枠線/パディング分だけ既に進んでいることがあるため(まだ何も
/// 描画されていなくても`cursor > 0.0`になり得る)、「強制改ページが本当に
/// 意味のある移動か(=現在のページを空のまま捨てずに済むか)」の判定には
/// `cursor`ではなくこちらを使う。
fn current_page_has_content(state: &PaginationState<'_>) -> bool {
    !state.last().boxes.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Font;
    use crate::html::{self, Dom, NodeData};
    use crate::layout::block::layout_document_from;
    use crate::layout::box_tree::build_box_for_element;
    use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet, Stylesheet};

    const TEST_FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    fn test_fonts() -> FontCollection {
        FontCollection::new(vec![
            Font::load(TEST_FONT_PATH).expect("should load bundled test font")
        ])
    }

    fn find_all(dom: &Dom, id: NodeId, tag: &str, out: &mut Vec<NodeId>) {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                out.push(id);
            }
        }
        for child in dom.children(id) {
            find_all(dom, child, tag, out);
        }
    }

    fn box_contains_node(b: &LaidOutBox, target: NodeId) -> bool {
        if b.node == Some(target) {
            return true;
        }
        if let LaidOutContent::Blocks(children) = &b.content {
            return children
                .iter()
                .any(|child| box_contains_node(child, target));
        }
        false
    }

    /// ページ内のボックスがページ高さの範囲内(多少の誤差を許容)に収まっているか
    /// を再帰的に確認する。
    fn assert_within_page(b: &LaidOutBox, page_height: f32) {
        let top = margin_box_top(b);
        assert!(top >= -0.01, "box top {top} should not be negative");
        assert!(
            top + b.layout.margin_box_height() <= page_height + 0.01,
            "box bottom should not exceed page height {page_height}"
        );
        if let LaidOutContent::Blocks(children) = &b.content {
            for child in children {
                assert_within_page(child, page_height);
            }
        }
    }

    #[test]
    fn page_settings_computes_content_area() {
        let settings = PageSettings::default();
        assert_eq!(
            settings.content_width(),
            settings.size.width - settings.margin.left - settings.margin.right
        );
        assert_eq!(
            settings.content_height(),
            settings.size.height - settings.margin.top - settings.margin.bottom
        );
    }

    #[test]
    fn short_document_fits_on_a_single_page_and_keeps_structure() {
        let dom = html::parse(br#"<p>hello</p>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 1);

        // 分割が発生しないため、無名ルート(node: None)ごと元の構造が保たれているはず。
        let mut htmls = Vec::new();
        find_all(&dom, dom.document(), "html", &mut htmls);
        assert_eq!(pages[0].boxes.len(), 1);
        assert_eq!(pages[0].boxes[0].node, None);
        assert!(box_contains_node(&pages[0].boxes[0], htmls[0]));
        // 分割されていないボックスは`Whole`(border-radiusを全角に適用してよい)。
        assert_eq!(pages[0].boxes[0].layout.fragment, FragmentPosition::Whole);
    }

    #[test]
    fn tall_content_distributes_across_multiple_pages_without_losing_items() {
        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(
            pages.len() > 1,
            "20 items of 100px should overflow a single page"
        );

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        assert_eq!(ps.len(), 20);
        for &p_id in &ps {
            let found_on_some_page = pages
                .iter()
                .any(|page| page.boxes.iter().any(|b| box_contains_node(b, p_id)));
            assert!(
                found_on_some_page,
                "p {p_id:?} should be placed on some page"
            );
        }

        for page in &pages {
            for b in &page.boxes {
                assert_within_page(b, settings.content_height());
            }
        }
    }

    #[test]
    fn float_taller_than_a_page_splits_across_pages_without_losing_items() {
        let mut html_src = String::from(r#"<div><div class="f">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div></div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .f { float: left; width: 100px; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(
            pages.len() > 1,
            "a float containing 20 items of 100px should overflow a single page \
             (floatのページ跨ぎを許容する)"
        );

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        assert_eq!(ps.len(), 20);
        for &p_id in &ps {
            let found_on_some_page = pages
                .iter()
                .any(|page| page.boxes.iter().any(|b| box_contains_node(b, p_id)));
            assert!(
                found_on_some_page,
                "p {p_id:?} inside the float should be placed on some page"
            );
        }

        for page in &pages {
            for b in &page.boxes {
                assert_within_page(b, settings.content_height());
            }
        }
    }

    #[test]
    fn float_is_translated_to_page_relative_coordinates_consistently_with_siblings() {
        let dom = html::parse(br#"<div><div class="a">a</div><div class="f">F</div></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .a { height: 50px; margin: 0; } \
             .f { float: left; width: 30px; height: 20px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 1);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);

        fn find_box(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
            if b.node == Some(target) {
                return Some(b);
            }
            if let LaidOutContent::Blocks(children) = &b.content {
                for child in children {
                    if let Some(found) = find_box(child, target) {
                        return Some(found);
                    }
                }
            }
            None
        }

        let float_box = pages[0]
            .boxes
            .iter()
            .find_map(|b| find_box(b, divs[2]))
            .expect("float box not found on the page");

        // block.rs側でfloatは、直前の兄弟`a`(height:50px)が通常フローで
        // 進めた`cursor_y`=50の地点(=`a`の直後)に配置される(floatはDOM順で
        // 見つかった時点のcursor_yを起点にするため)。改ページが発生していない
        // ため、この絶対Y座標がそのままページ内相対Y座標になるはず
        // (shift_referenceが正しく機能していれば、floatの位置がずれない)。
        assert_eq!(float_box.layout.content.y, 50.0);
    }

    #[test]
    fn long_paragraph_splits_across_pages_by_line() {
        let words: Vec<String> = (0..1000).map(|i| format!("word{i}")).collect();
        let html_src = format!("<p>{}</p>", words.join(" "));
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(
            pages.len() > 1,
            "1000 words should wrap into more lines than fit on one page"
        );

        let total_lines: usize = pages
            .iter()
            .flat_map(|page| &page.boxes)
            .filter_map(|b| match &b.content {
                LaidOutContent::Inline(lines) => Some(lines.len()),
                _ => None,
            })
            .sum();
        assert!(
            total_lines > 20,
            "1000 words should wrap into many lines total, got {total_lines}"
        );
        assert!(
            pages[0].boxes.len() > 1,
            "first page should hold multiple line fragments, got {}",
            pages[0].boxes.len()
        );
    }

    /// `page.boxes`の中から、`target`をnodeに持ち、かつ`LaidOutContent::Blocks(vec![])`
    /// (=装飾専用フラグメント)であるものを探す。
    fn find_decoration_fragment(page: &Page, target: NodeId) -> Option<&LaidOutBox> {
        page.boxes.iter().find(|b| {
            b.node == Some(target)
                && matches!(&b.content, LaidOutContent::Blocks(c) if c.is_empty())
        })
    }

    #[test]
    fn split_container_gets_a_decoration_fragment_on_every_page_it_spans() {
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".wrapper { border: 2px solid black; padding: 5px; margin: 0; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(
            pages.len() >= 3,
            "expected the wrapper to span at least 3 pages, got {}",
            pages.len()
        );

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let wrapper = divs[0];

        // すべてのページに、wrapperの装飾フラグメントが存在するはず
        // (背景・枠線がページをまたいでも失われないことの確認)。
        let decorations: Vec<&LaidOutBox> = pages
            .iter()
            .map(|page| {
                find_decoration_fragment(page, wrapper)
                    .expect("every page the wrapper spans should carry a decoration fragment")
            })
            .collect();

        // 最初のフラグメントだけが上枠線・上パディングを持つ。
        assert_eq!(decorations[0].layout.border.top, 2.0);
        assert_eq!(decorations[0].layout.padding.top, 5.0);
        assert_eq!(decorations[0].layout.fragment, FragmentPosition::First);
        // 最後のフラグメントだけが下枠線・下パディングを持つ。
        let last = decorations.last().unwrap();
        assert_eq!(last.layout.border.bottom, 2.0);
        assert_eq!(last.layout.padding.bottom, 5.0);
        assert_eq!(last.layout.fragment, FragmentPosition::Last);
        // 中間のフラグメントは`Middle`(border-radiusの角丸抑制に使う)。
        for decoration in &decorations[1..decorations.len() - 1] {
            assert_eq!(decoration.layout.fragment, FragmentPosition::Middle);
        }

        // 左右の枠線・パディングは全フラグメントに適用される。
        for decoration in &decorations {
            assert_eq!(decoration.layout.border.left, 2.0);
            assert_eq!(decoration.layout.padding.left, 5.0);
            assert!(decoration.layout.content.height > 0.0);
        }

        // 中間のフラグメント(最初でも最後でもないもの)は上下の枠線・パディングを持たない。
        for decoration in &decorations[1..decorations.len() - 1] {
            assert_eq!(decoration.layout.border.top, 0.0);
            assert_eq!(decoration.layout.border.bottom, 0.0);
            assert_eq!(decoration.layout.padding.top, 0.0);
            assert_eq!(decoration.layout.padding.bottom, 0.0);
        }

        // 中身の<p>群は引き続きすべて見つかるはず(装飾フラグメント追加による
        // 既存の子配置ロジックへの副作用がないことの回帰確認)。
        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        for &p_id in &ps {
            let found = pages
                .iter()
                .any(|page| page.boxes.iter().any(|b| box_contains_node(b, p_id)));
            assert!(found, "p {p_id:?} should still be placed on some page");
        }
    }

    #[test]
    fn split_container_without_visible_decoration_gets_no_decoration_fragment() {
        // 背景色も枠線もないコンテナは、装飾フラグメント自体を生成しない
        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(
            pages.len() > 1,
            "expected the wrapper to span multiple pages"
        );

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let wrapper = divs[0];

        for page in &pages {
            assert!(
                find_decoration_fragment(page, wrapper).is_none(),
                "a wrapper without background/border should not get a decoration fragment"
            );
        }
    }

    /// `target`をnodeに持ち、実際に内容を伴う(装飾専用フラグメントではない)
    /// ボックスがそのページにあるか。
    fn page_contains_content(page: &Page, target: NodeId) -> bool {
        page.boxes.iter().any(|b| box_contains_node(b, target))
    }

    #[test]
    fn break_before_always_forces_a_new_page_even_though_both_fit_on_one_page() {
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".a { height: 50px; margin: 0; } \
             .b { height: 50px; margin: 0; break-before: always; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(
            pages.len(),
            2,
            "break-before: always should force a new page even though both \
             paragraphs easily fit on a single page"
        );

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let (a, b) = (ps[0], ps[1]);

        assert!(page_contains_content(&pages[0], a));
        assert!(!page_contains_content(&pages[0], b));
        assert!(page_contains_content(&pages[1], b));
        assert!(!page_contains_content(&pages[1], a));
    }

    #[test]
    fn break_before_always_on_the_first_element_does_not_create_a_blank_leading_page() {
        let dom = html::parse(br#"<p class="a">A</p>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".a { height: 50px; margin: 0; break-before: always; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(
            pages.len(),
            1,
            "break-before: always on the very first element of the document \
             should not produce a blank leading page"
        );
    }

    #[test]
    fn break_after_always_forces_a_new_page_before_the_next_sibling() {
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".a { height: 50px; margin: 0; break-after: always; } \
             .b { height: 50px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 2);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let (a, b) = (ps[0], ps[1]);

        assert!(page_contains_content(&pages[0], a));
        assert!(page_contains_content(&pages[1], b));
        assert!(!page_contains_content(&pages[0], b));
    }

    #[test]
    fn break_after_always_on_the_last_element_does_not_create_a_trailing_blank_page() {
        let dom = html::parse(br#"<p class="a">A</p>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".a { height: 50px; margin: 0; break-after: always; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(
            pages.len(),
            1,
            "break-after: always on the very last element should not produce \
             a trailing blank page"
        );
    }

    #[test]
    fn nested_break_before_is_honored_even_when_the_whole_subtree_fits_on_one_page() {
        // wrapper divの中身は合計しても(既定のページ高さに比べれば)ごく小さく、
        // 「丸ごと1個のリーフとして配置する」高速経路の対象になり得る。それでも
        // 内部のbには`break-before: always`があるので見逃してはならない。
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".a { height: 10px; margin: 0; } \
             .b { height: 10px; margin: 0; break-before: always; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 2);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        assert!(page_contains_content(&pages[0], ps[0]));
        assert!(page_contains_content(&pages[1], ps[1]));
    }

    #[test]
    fn break_inside_avoid_moves_the_whole_block_to_the_next_page_instead_of_splitting() {
        let settings = PageSettings::default();
        // fillerでページ残り高さを、wrapperの合計高さ(400px)より小さく
        // (しかしwrapper単体は空のページになら丸ごと収まるように)調整する。
        let filler_height = settings.content_height() - 200.0;
        let html_src = r#"<div class="filler"></div>
               <div class="wrapper">
                   <p class="a">A</p><p class="b">B</p><p class="c">C</p><p class="d">D</p>
               </div>"#;
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(&format!(
            ".filler {{ height: {filler_height}px; margin: 0; }} \
             .wrapper {{ break-inside: avoid; margin: 0; }} \
             .a, .b, .c, .d {{ height: 100px; margin: 0; }}"
        ));
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(
            pages.len(),
            2,
            "the wrapper should move to a fresh second page instead of splitting"
        );

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        for &p in &ps {
            assert!(
                page_contains_content(&pages[1], p),
                "all paragraphs of the avoid-split wrapper should land on page 2"
            );
            assert!(!page_contains_content(&pages[0], p));
        }
    }

    #[test]
    fn break_inside_avoid_still_splits_when_the_element_is_taller_than_a_full_page() {
        // avoidは「できれば分割しない」という指定であり、1ページに収まらない
        // ほど巨大な場合はbest-effortで通常通り分割せざるを得ない。
        let settings = PageSettings::default();
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..30 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".wrapper { break-inside: avoid; margin: 0; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(
            pages.len() > 2,
            "a wrapper taller than a full page must still be split across pages \
             despite break-inside: avoid, got {} pages",
            pages.len()
        );
    }

    /// `word_count`語からなる段落が、明示`width`(px)でどう行分割されるかを
    /// 測定する(行数と、各行の高さが一様であること)。orphans/widowsの
    /// テストは、この一様な行高さを基準に`filler`の高さを逆算してページ内の
    /// 自然な分割点を狙い撃つ。実際のテスト本体でも対象の段落には同じ
    /// `width: {width}px; margin: 0;`を指定するため、ここでの測定値が
    /// そのまま使える(段落の`width`を明示指定するので、containing widthの
    /// 値そのものは折り返しに影響しない)。
    fn measure_paragraph_lines(word_count: usize, width: f32) -> (usize, f32) {
        let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
        let html_src = format!(r#"<p class="target">{}</p>"#, words.join(" "));
        let dom = html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(&format!(".target {{ width: {width}px; margin: 0; }}"));
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let tree = build_box_tree(&dom, &styles);
        let laid = layout_document(
            &tree,
            &styles,
            &fonts,
            PageSettings::default().content_width(),
        );

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let lines = find_inline_lines(&laid, ps[0]).expect("expected inline content");
        let height = lines[0].rect.height;
        assert!(
            lines.iter().all(|l| (l.rect.height - height).abs() < 0.01),
            "this test relies on every wrapped line having the same height"
        );
        (lines.len(), height)
    }

    fn find_inline_lines(b: &LaidOutBox, target: NodeId) -> Option<&Vec<LineBox>> {
        if b.node == Some(target) {
            if let LaidOutContent::Inline(lines) = &b.content {
                return Some(lines);
            }
        }
        match &b.content {
            LaidOutContent::Blocks(children) => {
                children.iter().find_map(|c| find_inline_lines(c, target))
            }
            _ => None,
        }
    }

    /// ページ分割後の行フラグメント(`place_line`が作る無名ラッパー)は元の
    /// 段落のNodeIdを持たないため、ページ上の行数はnodeで絞り込まず単純に
    /// 合計する(このテストのDOMには対象の段落以外にインライン内容を持つ
    /// 要素がないため、これで対象段落の行数と一致する)。
    fn count_inline_lines(b: &LaidOutBox) -> usize {
        match &b.content {
            LaidOutContent::Inline(lines) => lines.len(),
            LaidOutContent::Blocks(children) => children.iter().map(count_inline_lines).sum(),
            LaidOutContent::Table(_)
            | LaidOutContent::Flex(_)
            | LaidOutContent::Grid(_)
            | LaidOutContent::Image(_) => 0,
        }
    }

    fn lines_on_page(page: &Page) -> usize {
        page.boxes.iter().map(count_inline_lines).sum()
    }

    #[test]
    fn orphans_defers_the_whole_paragraph_when_too_few_lines_would_fit() {
        let word_count = 60;
        let width = 200.0;
        let (n, line_height) = measure_paragraph_lines(word_count, width);
        assert!(n >= 4, "expected several wrapped lines, got {n}");

        let settings = PageSettings::default();
        let orphans = 3usize;
        let widows = 1usize;
        // fillerでページ残り高さを、ちょうど1行分+半行分だけ残るよう調整する
        // (=自然には1行しか収まらない。orphans=3を満たせないはず)。
        let target_fit = 1usize;
        let desired_remaining = (target_fit as f32 + 0.5) * line_height;
        let filler_height = settings.content_height() - 8.0 - desired_remaining;

        let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
        let full_html = format!(
            r#"<div class="filler"></div><p class="target">{}</p>"#,
            words.join(" ")
        );
        let dom = html::parse(full_html.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(&format!(
            ".filler {{ height: {filler_height}px; margin: 0; }} \
             .target {{ width: {width}px; margin: 0; orphans: {orphans}; widows: {widows}; }}"
        ));
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(
            pages.len(),
            2,
            "the paragraph should move entirely to a second page"
        );

        assert_eq!(
            lines_on_page(&pages[0]),
            0,
            "orphans: {orphans} should prevent leaving only {target_fit} line(s) behind"
        );
        assert_eq!(lines_on_page(&pages[1]), n);
    }

    #[test]
    fn widows_pulls_lines_forward_to_avoid_stranding_too_few_on_the_next_page() {
        let word_count = 60;
        let width = 200.0;
        let (n, line_height) = measure_paragraph_lines(word_count, width);
        assert!(n >= 8, "expected several wrapped lines, got {n}");

        let settings = PageSettings::default();
        let orphans = 1usize;
        let widows = 3usize;
        // 自然な分割点では(n - 1)行がこのページに収まり、次ページには1行しか
        // 残らない想定(widows=3を満たせないはず)。
        let target_fit = n - 1;
        let desired_remaining = (target_fit as f32 + 0.5) * line_height;
        let filler_height = settings.content_height() - 8.0 - desired_remaining;

        let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
        let full_html = format!(
            r#"<div class="filler"></div><p class="target">{}</p>"#,
            words.join(" ")
        );
        let dom = html::parse(full_html.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(&format!(
            ".filler {{ height: {filler_height}px; margin: 0; }} \
             .target {{ width: {width}px; margin: 0; orphans: {orphans}; widows: {widows}; }}"
        ));
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 2);

        let on_page_2 = lines_on_page(&pages[1]);
        assert!(
            on_page_2 >= widows,
            "widows: {widows} should keep at least that many lines together on page 2, got {on_page_2}"
        );
        assert_eq!(lines_on_page(&pages[0]) + on_page_2, n);
    }

    #[test]
    fn paragraph_shorter_than_orphans_plus_widows_is_never_split() {
        let word_count = 3;
        // 幅を極端に狭くして、単語ごとに1行になるようにする(3行になるはず)。
        let width = 10.0;
        let (n, line_height) = measure_paragraph_lines(word_count, width);
        assert_eq!(n, 3, "expected each word to wrap onto its own line");

        let settings = PageSettings::default();
        // orphans+widows(4) > n(3)なので、どこで分割しても両方は満たせない。
        let orphans = 2usize;
        let widows = 2usize;
        let target_fit = 2usize;
        let desired_remaining = (target_fit as f32 + 0.5) * line_height;
        let filler_height = settings.content_height() - 8.0 - desired_remaining;

        let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
        let full_html = format!(
            r#"<div class="filler"></div><p class="target">{}</p>"#,
            words.join(" ")
        );
        let dom = html::parse(full_html.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(&format!(
            ".filler {{ height: {filler_height}px; margin: 0; }} \
             .target {{ width: {width}px; margin: 0; orphans: {orphans}; widows: {widows}; }}"
        ));
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 2);

        assert_eq!(
            lines_on_page(&pages[0]),
            0,
            "a paragraph shorter than orphans + widows should never be split"
        );
        assert_eq!(lines_on_page(&pages[1]), n);
    }

    /// [`PaginationState`]と[`place_box`]を直接呼び出し、`finish()`を呼ぶ
    /// 前の時点でバッファに何ページ残っているかを調べるテスト用ヘルパー。
    fn unflushed_buffer_len_after_place_box(laid_out: &LaidOutBox, page_height: f32) -> usize {
        let mut buffer = PaginationBuffer::new();
        let mut on_page = |_page: Page| {};
        let mut state = PaginationState::new(&mut buffer, &mut on_page);
        let mut cursor = 0.0f32;
        place_box(&mut laid_out.clone(), page_height, &mut state, &mut cursor);
        buffer.buffer.len()
    }

    #[test]
    fn streaming_flushes_undecorated_content_incrementally_not_all_at_finish() {
        // 装飾(背景色・枠線)を一切持たないコンテナだけの構造。
        let mut html_src = String::from("<div>");
        for i in 0..60 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let tree = build_box_tree(&dom, &styles);
        let mut laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());
        let page_height = settings.content_height();

        let total_pages = paginate(&mut laid_out, page_height).len();
        assert!(
            total_pages >= 5,
            "expected several pages, got {total_pages}"
        );

        // 装飾を持たないコンテナは`place_split`のenter_split/exit_splitを
        // 呼ばないため、`new_page`のたびに`PaginationState::try_flush`が
        // その場で発火し、直前のページが即座にflushされる(モジュールdoc・
        // `has_visible_decoration`参照)。そのため`place_box`のトップレベル
        // 呼び出しが完了した時点(`finish()`をまだ呼んでいない)でも、
        // バッファには「まだ書き込み中の最後の1ページ」しか残らないはず。
        // すべてのページが最後にまとめてflushされる実装だと、ここで
        // `total_pages`になってしまう。
        assert_eq!(
            unflushed_buffer_len_after_place_box(&laid_out, page_height),
            1,
            "pages should already be flushed incrementally before finish() is even called"
        );
    }

    #[test]
    fn streaming_still_flushes_down_to_one_page_when_a_decorated_wrapper_spans_many_pages() {
        // 背景・枠線を持つwrapperがページをまたぐ場合でも、`place_box`の
        // トップレベル呼び出しが完了すれば、そのwrapperの`place_split`は
        // 必ず自分のexit_splitまで実行し終えているはず(`place_split`は
        // 自身の処理を終えてから`exit_split`を呼んでreturnするため)。
        // そのため装飾の有無にかかわらず、`place_box`完了時点で
        // バッファに残るのは最後の1ページだけになるという不変条件は保たれる。
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".wrapper { border: 2px solid black; padding: 5px; margin: 0; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let tree = build_box_tree(&dom, &styles);
        let mut laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());
        let page_height = settings.content_height();

        let total_pages = paginate(&mut laid_out, page_height).len();
        assert!(
            total_pages >= 3,
            "expected the wrapper to span multiple pages, got {total_pages}"
        );

        assert_eq!(
            unflushed_buffer_len_after_place_box(&laid_out, page_height),
            1,
            "even a decorated wrapper should be fully resolved (and its earlier pages \
             flushed) by the time the top-level place_box call returns"
        );
    }

    #[test]
    fn paginate_streaming_matches_the_batched_version_for_a_decorated_spanning_wrapper() {
        // `PaginationState`のflush判定(モジュールdoc参照)が安全かどうかの
        // 正確性検証: 装飾フラグメントの遡り挿入が絡む、最も際どいケース
        // (`split_container_gets_a_decoration_fragment_on_every_page_it_spans`
        // と同じシナリオ)で、ストリーミング版と一括版が完全に同じ結果を
        // 返すことを確認する。もしflushが早すぎれば、装飾フラグメントの
        // 挿入先ページが既にflush済みでpanicするか、挿入自体が欠落する。
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".wrapper { border: 2px solid black; padding: 5px; margin: 0; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let tree = build_box_tree(&dom, &styles);
        let mut laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());
        let page_height = settings.content_height();

        let batched = paginate(&mut laid_out, page_height);
        let mut streamed = Vec::new();
        paginate_streaming(&mut laid_out, page_height, &mut |page| streamed.push(page));

        assert_eq!(batched.len(), streamed.len());
        assert!(batched.len() >= 3);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let wrapper = divs[0];

        for (b_page, s_page) in batched.iter().zip(streamed.iter()) {
            assert_eq!(b_page.boxes.len(), s_page.boxes.len());

            let b_dec = find_decoration_fragment(b_page, wrapper);
            let s_dec = find_decoration_fragment(s_page, wrapper);
            assert_eq!(
                b_dec.map(|d| d.layout.fragment),
                s_dec.map(|d| d.layout.fragment)
            );
            assert_eq!(
                b_dec.map(|d| d.layout.border.top),
                s_dec.map(|d| d.layout.border.top)
            );
            assert_eq!(
                b_dec.map(|d| d.layout.border.bottom),
                s_dec.map(|d| d.layout.border.bottom)
            );
            assert_eq!(
                b_dec.map(|d| d.layout.padding.top),
                s_dec.map(|d| d.layout.padding.top)
            );
        }
    }

    #[test]
    fn paginate_document_streaming_releases_paragraphs_as_their_page_flushes() {
        // 装飾のない20個の独立した<p>要素。各ページがflushされるたびに、
        // そのページに配置された<p>要素(とテキスト子孫)が解放されている
        // ことを確認する。
        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let mut dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        assert_eq!(ps.len(), 20);

        let mut flushed_pages = 0usize;
        paginate_document_streaming(
            &mut dom,
            &styles,
            &fonts,
            &settings,
            &ImageAssetCache::new(std::path::PathBuf::from("."), false),
            &mut |_page| {
                flushed_pages += 1;
            },
        );
        assert!(flushed_pages > 1, "expected multiple pages");

        // 全ページ処理後、20個の<p>要素すべてが解放されているはず
        // (装飾のないラッパーdivも、最後のページのflushで解放される)。
        for &p in &ps {
            assert!(
                dom.is_released(p),
                "paragraph {p:?} should be released once its page has flushed"
            );
        }
    }

    #[test]
    fn paginate_document_streaming_eventually_releases_a_spanning_wrapper() {
        // 背景・枠線を持つwrapperが複数ページにまたがる場合でも、
        // `paginate_document_streaming`(公開API)を通した全処理完了後には
        // wrapper自身のノードも解放されているはず(装飾フラグメントが
        // 最後のページで`Last`になった時点で解放される)。「最後のページ
        // より前では解放されない」という中間状態の直接検証は、下の
        // `wrapper_node_is_not_released_before_its_last_fragment_flushes`
        // で行う。
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let mut dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".wrapper { border: 2px solid black; padding: 5px; margin: 0; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let wrapper = divs[0];

        let mut flushed_pages = 0usize;
        paginate_document_streaming(
            &mut dom,
            &styles,
            &fonts,
            &settings,
            &ImageAssetCache::new(std::path::PathBuf::from("."), false),
            &mut |_page| {
                flushed_pages += 1;
            },
        );
        assert!(
            flushed_pages >= 3,
            "expected the wrapper to span at least 3 pages, got {flushed_pages}"
        );

        assert!(
            dom.is_released(wrapper),
            "the wrapper should be released once its final page has flushed"
        );
    }

    #[test]
    fn wrapper_node_is_not_released_before_its_last_fragment_flushes() {
        // 1ページ目がflushされた時点で、wrapperノードはまだ解放されて
        // いないことを`on_page`のコールバック内から直接観測する。
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let mut dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".wrapper { border: 2px solid black; padding: 5px; margin: 0; } \
             .item { height: 100px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let wrapper = divs[0];

        let tree = build_box_tree(&dom, &styles);
        let mut laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());
        let page_height = settings.content_height();
        let total_pages = paginate(&mut laid_out, page_height).len();
        assert!(total_pages >= 3);

        let mut flushed_pages = 0usize;
        let mut observed_mid_release = Vec::new();
        paginate_streaming(&mut laid_out, page_height, &mut |page| {
            flushed_pages += 1;
            release_completed_subtrees(&mut dom, &page);
            if flushed_pages < total_pages {
                observed_mid_release.push(dom.is_released(wrapper));
            }
        });

        assert!(
            observed_mid_release.iter().all(|&released| !released),
            "the wrapper must stay alive until its last fragment flushes, observed: \
             {observed_mid_release:?}"
        );
        assert!(dom.is_released(wrapper));
    }

    #[test]
    fn paragraphs_on_a_later_page_are_not_released_before_their_own_page_flushes() {
        // 早すぎる解放(まだ登場していないノードを誤って解放してしまう
        // バグ)がないことを、各<p>が実際にどのページに配置されるかを
        // 一括版で事前計算し、そのページより前の時点ではまだ解放されて
        // いないことを`on_page`のたびに確認する。
        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let mut dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        assert_eq!(ps.len(), 20);

        let tree = build_box_tree(&dom, &styles);
        let mut laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());
        let page_height = settings.content_height();

        let batched = paginate(&mut laid_out, page_height);
        assert!(batched.len() > 1, "expected multiple pages");
        let page_of: HashMap<NodeId, usize> = ps
            .iter()
            .map(|&p| {
                let idx = batched
                    .iter()
                    .position(|page| page.boxes.iter().any(|b| box_contains_node(b, p)))
                    .expect("every paragraph should land on some page");
                (p, idx)
            })
            .collect();

        let mut current_page_index = 0usize;
        paginate_streaming(&mut laid_out, page_height, &mut |page| {
            release_completed_subtrees(&mut dom, &page);
            for (&p, &expected_page) in &page_of {
                if expected_page > current_page_index {
                    assert!(
                        !dom.is_released(p),
                        "paragraph destined for page {expected_page} must not be released \
                         while only page {current_page_index} has flushed"
                    );
                }
            }
            current_page_index += 1;
        });
    }

    #[test]
    fn streaming_paginator_multiple_push_item_calls_match_a_single_combined_tree() {
        // 20個の<p>を「1つのツリーとして一括で処理する」場合と「1個ずつ
        // push_itemする」場合とで、最終的なページ数・内容が一致することを
        // 確認する(トップレベル要素単位のストリーミング入力で、
        // この2つの経路が同じ結果になることの土台となる検証)。
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        // 一括版: 20個の<p>を1つのdivにまとめてレイアウトする。
        let mut combined_html = String::from("<div>");
        for i in 0..20 {
            combined_html.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        combined_html.push_str("</div>");
        let combined_dom = html::parse(combined_html.as_bytes());
        let combined_styles = compute_styles(&combined_dom, &ua, &author);
        let combined_tree = build_box_tree(&combined_dom, &combined_styles);
        let mut combined_laid_out = layout_document(
            &combined_tree,
            &combined_styles,
            &fonts,
            settings.content_width(),
        );
        let batched_pages = paginate(&mut combined_laid_out, settings.content_height());
        assert!(batched_pages.len() > 1, "expected multiple pages");

        // push_item版: 同じ`combined_dom`/`combined_styles`から、<p>要素
        // それぞれのLayoutBoxを`build_box_for_element`で個別に切り出し、
        // 都度push_itemする。`html::parse`を20回呼ぶ形にすると、各回で
        // 独立した<html>/<body>が補完され、UAスタイルシートの
        // `body { margin: 8px; }`が20回分累積してしまい、push_itemロジック
        // 自体の検証にならないため、同じDOMから要素単位で切り出す。
        let mut ps = Vec::new();
        find_all(&combined_dom, combined_dom.document(), "p", &mut ps);
        assert_eq!(ps.len(), 20);

        let mut streamed_pages: Vec<Page> = Vec::new();
        let mut paginator = StreamingPaginator::new(settings.content_height());
        let mut start_y = 0.0f32;
        for &p_node in &ps {
            let item_box = build_box_for_element(&combined_dom, &combined_styles, p_node)
                .expect("p element should produce a LayoutBox");
            let item_laid_out = layout_document_from(
                &item_box,
                &combined_styles,
                &fonts,
                settings.content_width(),
                0.0,
                start_y,
            );
            start_y += item_laid_out.layout.margin_box_height();
            streamed_pages.extend(paginator.push_item(&mut item_laid_out.clone()));
        }
        streamed_pages.extend(paginator.finish());

        assert_eq!(
            batched_pages.len(),
            streamed_pages.len(),
            "pushing items one at a time should yield the same page count as a single combined tree"
        );
        for (batched, streamed) in batched_pages.iter().zip(streamed_pages.iter()) {
            assert_eq!(batched.boxes.len(), streamed.boxes.len());
        }
    }

    /// 同じ要素列を、一括版([`paginate`])と[`StreamingPaginator`]の
    /// `push_item`版の両方でページ分割し、それぞれのページ数を返す。
    ///
    /// DOMを1つだけ作って両方で使い回すのは、要素ごとに`html::parse`すると
    /// 補完される`<body>`のUAマージンが要素数ぶん累積してしまうため
    fn page_counts_both_ways(author_css: &str, items_html: &str) -> (usize, usize) {
        let author = parse_stylesheet(author_css);
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(format!("<div>{items_html}</div>").as_bytes());
        let styles = compute_styles(&dom, &ua, &author);

        let tree = build_box_tree(&dom, &styles);
        let mut laid_out = layout_document(&tree, &styles, &fonts, settings.content_width());
        let batched = paginate(&mut laid_out, settings.content_height()).len();

        let mut items = Vec::new();
        find_all(&dom, dom.document(), "p", &mut items);

        let mut paginator = StreamingPaginator::new(settings.content_height());
        let mut streamed = 0;
        let mut start_y = 0.0f32;
        for &node in &items {
            let item_box = build_box_for_element(&dom, &styles, node)
                .expect("p element should produce a LayoutBox");
            let mut item_laid_out = layout_document_from(
                &item_box,
                &styles,
                &fonts,
                settings.content_width(),
                0.0,
                start_y,
            );
            start_y += item_laid_out.layout.margin_box_height();
            streamed += paginator.push_item(&mut item_laid_out).len();
        }
        streamed += paginator.finish().len();

        (batched, streamed)
    }

    #[test]
    fn streaming_paginator_honors_break_after_between_items() {
        // `place_box`が見るのは子リストの中の強制改ページだけなので、
        // トップレベル要素同士(=`push_item`の呼び出し順)の`break-after`は
        // 分割器の側で扱わなければ無視されてしまう。
        let (batched, streamed) = page_counts_both_ways(
            ".brk { height: 50px; margin: 0; break-after: always; }",
            r#"<p class="brk">A</p><p class="brk">B</p><p class="brk">C</p>"#,
        );

        assert_eq!(
            batched, 3,
            "break-after: always on each of three short items should give three pages"
        );
        assert_eq!(
            streamed, batched,
            "pushing the same items one at a time must honor break-after too"
        );
    }

    #[test]
    fn streaming_paginator_honors_break_before_between_items() {
        let (batched, streamed) = page_counts_both_ways(
            ".a { height: 50px; margin: 0; } \
             .brk { height: 50px; margin: 0; break-before: always; }",
            r#"<p class="a">A</p><p class="brk">B</p><p class="brk">C</p>"#,
        );

        assert_eq!(batched, 3);
        assert_eq!(
            streamed, batched,
            "pushing the same items one at a time must honor break-before too"
        );
    }

    #[test]
    fn streaming_paginator_does_not_create_blank_pages_at_the_document_edges() {
        // 先頭の`break-before`と末尾の`break-after`は、移動先に何も無いので
        // 空ページを作ってはいけない(一括版と同じ扱い)。
        let (batched, streamed) = page_counts_both_ways(
            ".first { height: 50px; margin: 0; break-before: always; } \
             .last { height: 50px; margin: 0; break-after: always; }",
            r#"<p class="first">A</p><p class="last">B</p>"#,
        );

        assert_eq!(
            batched, 1,
            "a break-before on the first item and a break-after on the last one \
             should not create blank pages"
        );
        assert_eq!(streamed, batched);
    }

    // ===== テーブルの行単位ページ分割 =====

    /// `html_src`をページ分割し、ページごとのテーブル行数を返す。
    fn table_rows_per_page(html_src: &str) -> Vec<usize> {
        let dom = html::parse(html_src.as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let fonts = test_fonts();
        let settings = PageSettings::default();
        let pages = paginate_document(&dom, &styles, &fonts, &settings);

        fn count(b: &LaidOutBox) -> usize {
            match &b.content {
                LaidOutContent::Table(table) => table.rows.len(),
                LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                    children.iter().map(count).sum()
                }
                _ => 0,
            }
        }
        pages
            .iter()
            .map(|page| page.boxes.iter().map(count).sum())
            .collect()
    }

    fn rows_html(n: usize) -> String {
        let rows: String = (0..n).map(|i| format!("<tr><td>{i}</td></tr>")).collect();
        format!("<table>{rows}</table>")
    }

    #[test]
    fn a_table_is_split_row_by_row_instead_of_being_treated_as_atomic() {
        let counts = table_rows_per_page(&rows_html(120));
        assert!(counts.len() >= 2, "got {counts:?}");
        assert_eq!(counts.iter().sum::<usize>(), 120, "got {counts:?}");
        assert!(counts.iter().all(|&c| c > 0), "got {counts:?}");
    }

    #[test]
    fn table_fragments_are_marked_first_middle_last() {
        let dom = html::parse(rows_html(200).as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let fonts = test_fonts();
        let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());

        fn positions(b: &LaidOutBox, out: &mut Vec<FragmentPosition>) {
            match &b.content {
                LaidOutContent::Table(_) => out.push(b.layout.fragment),
                LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                    for c in children {
                        positions(c, out);
                    }
                }
                _ => {}
            }
        }
        let mut found = Vec::new();
        for page in &pages {
            for b in &page.boxes {
                positions(b, &mut found);
            }
        }
        assert!(
            found.len() >= 3,
            "expected several fragments, got {found:?}"
        );
        assert_eq!(found.first(), Some(&FragmentPosition::First));
        assert_eq!(found.last(), Some(&FragmentPosition::Last));
        assert!(found[1..found.len() - 1]
            .iter()
            .all(|p| *p == FragmentPosition::Middle));
    }

    #[test]
    fn rows_keep_their_order_and_spacing_after_being_split() {
        let dom = html::parse(rows_html(120).as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let fonts = test_fonts();
        let settings = PageSettings::default();
        let pages = paginate_document(&dom, &styles, &fonts, &settings);

        fn rows_of(b: &LaidOutBox, out: &mut Vec<f32>) {
            match &b.content {
                LaidOutContent::Table(table) => {
                    for row in &table.rows {
                        out.push(row.cells[0].layout.content.y);
                    }
                }
                LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                    for c in children {
                        rows_of(c, out);
                    }
                }
                _ => {}
            }
        }
        for page in &pages {
            let mut ys = Vec::new();
            for b in &page.boxes {
                rows_of(b, &mut ys);
            }
            // 各ページ内で行が上から順に並び、ページ高さに収まっている。
            assert!(ys.windows(2).all(|w| w[1] > w[0]), "got {ys:?}");
            assert!(
                ys.iter().all(|y| *y >= 0.0 && *y <= settings.size.height),
                "a row was placed outside the page: {ys:?}"
            );
        }
    }

    #[test]
    fn thead_rows_are_repeated_on_every_page_but_body_rows_are_not() {
        let rows: String = (0..120)
            .map(|i| format!("<tr><td>b{i}</td></tr>"))
            .collect();
        let html_src =
            format!("<table><thead><tr><td>H</td></tr></thead><tbody>{rows}</tbody></table>");
        let dom = html::parse(html_src.as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let fonts = test_fonts();
        let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());

        fn sections(b: &LaidOutBox, out: &mut Vec<TableSection>) {
            match &b.content {
                LaidOutContent::Table(table) => {
                    out.extend(table.rows.iter().map(|r| r.section));
                }
                LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                    for c in children {
                        sections(c, out);
                    }
                }
                _ => {}
            }
        }

        let mut head_total = 0;
        let mut body_total = 0;
        for page in &pages {
            let mut found = Vec::new();
            for b in &page.boxes {
                sections(b, &mut found);
            }
            assert_eq!(
                found.first(),
                Some(&TableSection::Head),
                "each page must start with the repeated header: {found:?}"
            );
            head_total += found.iter().filter(|s| **s == TableSection::Head).count();
            body_total += found.iter().filter(|s| **s == TableSection::Body).count();
        }
        assert_eq!(head_total, pages.len(), "one header row per page");
        assert_eq!(body_total, 120, "body rows must not be duplicated");
    }

    #[test]
    fn a_header_taller_than_the_page_is_not_repeated() {
        // 見出しだけでページが埋まると1行も進めなくなるため繰り返さない。
        let rows: String = (0..40).map(|i| format!("<tr><td>b{i}</td></tr>")).collect();
        let head: String = (0..80).map(|i| format!("<tr><td>h{i}</td></tr>")).collect();
        let html_src = format!("<table><thead>{head}</thead><tbody>{rows}</tbody></table>");
        let dom = html::parse(html_src.as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let fonts = test_fonts();
        let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());

        fn head_count(b: &LaidOutBox) -> usize {
            match &b.content {
                LaidOutContent::Table(table) => table
                    .rows
                    .iter()
                    .filter(|r| r.section == TableSection::Head)
                    .count(),
                LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                    children.iter().map(head_count).sum()
                }
                _ => 0,
            }
        }
        let total: usize = pages
            .iter()
            .map(|p| p.boxes.iter().map(head_count).sum::<usize>())
            .sum();
        assert_eq!(total, 80, "the oversized header must not be duplicated");
    }
}
