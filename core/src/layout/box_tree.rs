//! DOM+計算スタイルからレイアウトボックスツリーを構築する。
//!
//! `display: none`の要素(とその部分木)は除外する。ブロックコンテナの子が
//! block-levelとinline-level/テキストの混在になる場合は、CSSの無名ボックス生成
//! 規則(CSS2.1 9.2.1.1)に従い、連続するinline-levelの内容を無名ブロックボックスに
//! まとめる。無名ボックスは対応するDOMノードを持たないため`node: None`とする。

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::html::{Dom, NodeData, NodeId};
use crate::pdf::{ImageAssetCache, PreparedImage};
use crate::style::{
    CaptionSide, ComputedStyle, Display, LengthPercentage, LengthPercentageOrAuto,
    ListStylePosition, ListStyleType, Position, RgbaColor, WhiteSpace,
};

use super::white_space;

#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// 対応するDOM要素。無名ボックスの場合は`None`。
    pub node: Option<NodeId>,
    pub content: BoxContent,
    /// `display: list-item`のマーカー(箇条書きの記号・番号)テキスト。
    /// `list-style-position: inside`かつ内容が`BoxContent::Inline`の場合は
    /// 代わりに`content`の先頭`InlineSpan`へ直接埋め込むため、この場合は
    /// `None`のまま(二重描画を避ける)。それ以外(`outside`、またはブロック子を
    /// 持つ`inside`)はレイアウト層(`block.rs`)
    /// がこのフィールドを見て別途配置する。
    pub marker: Option<String>,
    /// 採寸結果のメモ。詳細は[`MeasureMemo`]。
    pub measured: MeasureMemo,
}

impl LayoutBox {
    /// 内容だけを持つボックス(無名ボックス・置換要素の入れ物)。
    pub fn anonymous(content: BoxContent) -> Self {
        Self {
            node: None,
            content,
            marker: None,
            measured: MeasureMemo::default(),
        }
    }

    /// `node`に対応するボックス。
    pub fn for_node(node: NodeId, content: BoxContent) -> Self {
        Self {
            node: Some(node),
            content,
            marker: None,
            measured: MeasureMemo::default(),
        }
    }
}

/// ボックス1つ分の採寸メモ。
///
/// 同じ部分木は何度も測り直される。flex/gridのアイテムは祖先の段ごとに測られ、
/// さらに高さを知るための捨てレイアウトが幅の候補ごとに走るため、メモが無いと
/// ネストの段数に対して指数的に増える。
///
/// メモした値はボックスの内容・計算スタイル・フォントだけで決まる。ツリーは
/// 文書ごとに組み直され、`resolve_images`のような内容の書き換えはレイアウト
/// 開始前に終わっているので、レイアウト中は不変になる。
#[derive(Debug, Clone, Default)]
pub struct MeasureMemo {
    /// 自然幅(max-content幅)。[`super::table::measure_natural_content_width`]が埋める。
    natural_width: Cell<Option<f32>>,
    /// content幅を決め打ちして組んだときのcontent高さ。flex/gridの採寸ブリッジが
    /// 埋める。
    ///
    /// キーはcontent幅とcontaining width(`(content, containing)`)の組。中身の
    /// パーセンテージ指定はcontaining widthを基準に解決されるので、同じcontent幅
    /// でもcontaining widthが違えば高さは変わりうる。1つのボックスに対して問われる
    /// 組は数種類しかないので、線形探索で足りる(ハッシュより速い)。
    heights: RefCell<Vec<(u32, u32, f32)>>,
}

impl MeasureMemo {
    pub(super) fn natural_width(&self) -> Option<f32> {
        self.natural_width.get()
    }

    pub(super) fn set_natural_width(&self, width: f32) {
        self.natural_width.set(Some(width));
    }

    pub(super) fn height(&self, content_width: f32, containing_width: f32) -> Option<f32> {
        let (cw, aw) = (content_width.to_bits(), containing_width.to_bits());
        self.heights
            .borrow()
            .iter()
            .find(|(w, a, _)| *w == cw && *a == aw)
            .map(|(_, _, h)| *h)
    }

    pub(super) fn set_height(&self, content_width: f32, containing_width: f32, height: f32) {
        self.heights.borrow_mut().push((
            content_width.to_bits(),
            containing_width.to_bits(),
            height,
        ));
    }
}

#[derive(Debug, Clone)]
pub enum BoxContent {
    Blocks(Vec<LayoutBox>),
    /// インラインフォーマッティングコンテキストの内容。
    Inline(Vec<InlineSpan>),
    /// `display: table`要素の内容(行・セル)。
    Table(TableBox),
    /// `display: flex`要素の内容(flexアイテムの並び)。
    Flex(FlexBox),
    Grid(GridBox),
    /// `<img>`要素(置換要素として扱う、[`resolve_images`]参照)。
    Image(ImageBoxContent),
}

/// `display: flex`要素から集めたflexアイテムの並び。各アイテムは通常の
/// ブロック子と同じ`LayoutBox`(子要素ごとに1個、`build_children_boxes`の無名
/// ボックス生成規則は適用しない)。
#[derive(Debug, Clone)]
pub struct FlexBox {
    pub items: Vec<LayoutBox>,
}

/// `display: grid`のコンテナ。構造は[`FlexBox`]と同じで、レイアウト時に渡す
/// taffyの`Style`だけが異なる。
#[derive(Debug, Clone)]
pub struct GridBox {
    pub items: Vec<LayoutBox>,
}

/// `<img>`要素のコンテンツ。`resolve_images`が構築する。
#[derive(Debug, Clone)]
pub struct ImageBoxContent {
    /// フェッチ・デコードに成功した場合の画像データ。失敗
    /// (ネットワークエラー・SSRFブロック・デコード不能等、いずれも同列)した
    /// 場合は`None`になり、レイアウトはこれを空の置換要素として扱う。
    pub image: Option<std::rc::Rc<crate::pdf::PreparedImage>>,
    /// `width`/`height`属性の値(px、HTML属性由来)
    pub attr_width: Option<u32>,
    pub attr_height: Option<u32>,
}

/// `display: table`要素から集めた行の並びと、任意の`caption`。
#[derive(Debug, Clone)]
pub struct TableBox {
    /// `display: table-caption`の子要素(`<caption>`)。複数ある場合は最初の
    /// 1つのみ採用する(既知の簡略化)。`Box`は`LayoutBox`→`BoxContent::Table`
    /// →`TableBox`の再帰を間接参照で断ち切るために必要(サイズが無限になる
    /// コンパイルエラーの回避)。
    pub caption: Option<Box<LayoutBox>>,
    /// captionの計算スタイルから読んだ`caption-side`(captionが無ければ初期値`Top`)。
    pub caption_side: CaptionSide,
    pub rows: Vec<TableRow>,
    /// `<colgroup>`/`<col>`由来の列幅ヒント(列インデックス順、`None`は指定なし)。
    /// `<col>`要素の計算スタイルの`width`をそのまま持つ。実際の列数より多い
    /// 分は`layout::table`側で切り捨て、少ない分は指定なしとして扱う。
    pub column_widths: Vec<Option<LengthPercentage>>,
}

/// テーブル行が属するセクション。`<thead>`/`<tbody>`/`<tfoot>`は専用の
/// `display`値を持たない「透明な入れ物」なので、
/// 入れ物の要素名から判定してここに残す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableSection {
    Head,
    #[default]
    Body,
    Foot,
}

/// `display: table-row`要素(`<tr>`)1行分。
#[derive(Debug, Clone)]
pub struct TableRow {
    /// 元の`display: table-row`要素。CSSの無名ボックス生成規則で作られた行は
    /// 対応するDOMノードを持たないため`None`。
    pub node: Option<NodeId>,
    pub cells: Vec<TableCell>,
    /// この行が属するセクション。ページ分割層が`<thead>`の行を各ページの
    /// 先頭に複製するために使う。
    pub section: TableSection,
}

/// `display: table-cell`要素(`<td>`/`<th>`)1セル分。
#[derive(Debug, Clone)]
pub struct TableCell {
    /// 元の`display: table-cell`要素。無名セルは`None`。
    pub node: Option<NodeId>,
    /// `colspan`属性の値(未指定または不正な値は1)。
    pub colspan: usize,
    /// `rowspan`属性の値(未指定または不正な値は1)。`rowspan="0"`(HTML5の
    /// 「以降の行末まで拡張」特殊値)は非対応、1として扱う。
    pub rowspan: usize,
    /// セル自身の内容(通常のブロック/インラインボックスと同じ構造)。
    pub content: LayoutBox,
}

/// 1つのDOMテキストノードに由来する、単一の計算スタイルを持つテキスト区間。
#[derive(Debug, Clone)]
pub struct InlineSpan {
    /// このテキストの元になったDOMテキストノード。`styles`から計算スタイルを
    /// 引く(`<b>`/`<span style="...">`等の祖先の宣言は、テキストノード自身の
    /// 計算スタイルに継承・カスケード済みなので、このノードのスタイルを見れば足りる)。
    pub node: NodeId,
    pub text: String,
    /// `::first-letter`用に分離された先頭1文字かどうか。`true`の場合、
    /// `node`の計算スタイルの`first_letter_style`(あれば)で一部プロパティが
    /// 上書きされる(`layout::inline::flatten_spans`が適用する)。
    pub is_first_letter: bool,
    /// `<br>`由来の強制改行かどうか。`true`のとき`text`は`"\n"`で、`node`は
    /// `<br>`要素自身(空行の高さをその計算スタイルから求めるため)。
    pub is_forced_break: bool,
    /// `display: inline-block`のアトミックボックス。`Some`のとき`text`は
    /// 空で、このスパンは「テキストではなく1つの箱」を表す。
    pub atomic: Option<Box<LayoutBox>>,
    /// このテキストを囲む`<a href>`のhref値。同じリンク配下に多数のランが
    /// 生成されるため`Rc`で共有する。
    pub link: Option<Rc<str>>,
    /// このテキストを囲むインライン要素(`<mark>`/`<span>`等)の
    /// `background-color`。無ければ透明。
    ///
    /// テキストノードの計算スタイルは親の非継承プロパティ(背景色を含む)まで
    /// クローンしている(`style::computed::compute_recursive`)ため、
    /// `styles[&span.node].background_color`を使うとブロックの背景まで
    /// インライン背景として塗ってしまう。ここでスパン構築時に「IFC内で
    /// 直近のインライン要素が指定した背景」だけを取り出して持たせる。
    pub background_color: RgbaColor,
    /// The `top`/`right`/`bottom`/`left` of the `position: relative` inline
    /// elements enclosing this text, outermost first. After line layout the runs
    /// are shifted visually by this much (`layout::inline`); nested ones add up.
    /// For the same reason as `background_color`, this cannot be read back from
    /// the computed style of the text node, which has inherited even the
    /// `position` of the block.
    pub relative_insets: Vec<RelativeInset>,
}

/// The `top`/`right`/`bottom`/`left` specified on a `position: relative`
/// element. Resolving them to an offset needs the containing block width, so the
/// caller does it (`layout::block::resolve_relative_offset`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RelativeInset {
    pub top: LengthPercentageOrAuto,
    pub right: LengthPercentageOrAuto,
    pub bottom: LengthPercentageOrAuto,
    pub left: LengthPercentageOrAuto,
}

impl RelativeInset {
    /// Returns the four sides if `style` is `position: relative`.
    pub fn of(style: &ComputedStyle) -> Option<Self> {
        (style.position == Position::Relative).then_some(Self {
            top: style.top,
            right: style.right,
            bottom: style.bottom,
            left: style.left,
        })
    }
}

impl InlineSpan {
    /// 通常のテキスト区間(囲むインライン要素の装飾なし)。
    fn text(node: NodeId, text: String) -> Self {
        Self::text_in_inline_context(node, text, &InlineContext::default())
    }

    /// 通常のテキスト区間(囲むインライン要素から受け継ぐ情報つき)。
    fn text_in_inline_context(node: NodeId, text: String, context: &InlineContext) -> Self {
        Self {
            node,
            text,
            is_first_letter: false,
            is_forced_break: false,
            atomic: None,
            link: context.link.clone(),
            background_color: context.background_color,
            relative_insets: context.relative_insets.clone(),
        }
    }

    /// `display: inline-block`のアトミックボックス。
    fn atomic(node: NodeId, atomic: LayoutBox) -> Self {
        Self {
            node,
            text: String::new(),
            is_first_letter: false,
            is_forced_break: false,
            atomic: Some(Box::new(atomic)),
            link: None,
            background_color: RgbaColor::TRANSPARENT,
            relative_insets: Vec::new(),
        }
    }

    /// `<br>`由来の強制改行。`text`を`"\n"`にしておくことで、
    /// `white-space: pre`の経路(`layout::inline::layout_pre_content`は
    /// `'\n'`で行を分割する)が改修なしで強制改行を処理できる。
    fn forced_break(node: NodeId) -> Self {
        Self {
            node,
            text: "\n".to_string(),
            is_first_letter: false,
            is_forced_break: true,
            atomic: None,
            link: None,
            background_color: RgbaColor::TRANSPARENT,
            relative_insets: Vec::new(),
        }
    }
}

/// インラインフォーマッティングコンテキストを下りながら受け継ぐ情報
/// (囲んでいるインライン要素に由来し、テキストノードの計算スタイルからは
/// 復元できないもの)。
#[derive(Debug, Clone)]
struct InlineContext {
    /// 直近の`<a href>`のhref。
    link: Option<Rc<str>>,
    /// 直近のインライン要素が指定した背景色。
    background_color: RgbaColor,
    /// What the enclosing `position: relative` inline elements specify,
    /// outermost first.
    relative_insets: Vec<RelativeInset>,
}

impl Default for InlineContext {
    fn default() -> Self {
        Self {
            link: None,
            background_color: RgbaColor::TRANSPARENT,
            relative_insets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildKind {
    /// `display: none`など、ボックスを生成しない。
    None,
    Block,
    Inline,
    /// 空白のみのテキストノード。インライン内容の間に挟まっている場合だけ
    /// 意味を持つ(単語間の空白として畳み込まれる)。ブロックの間や
    /// インライン内容の前後では捨てる(CSS2.1 9.2.2.1)。
    Whitespace,
}

pub fn build_box_tree(dom: &Dom, styles: &HashMap<NodeId, Rc<ComputedStyle>>) -> LayoutBox {
    let child_ids: Vec<NodeId> = dom.children(dom.document()).collect();
    LayoutBox::anonymous(BoxContent::Blocks(build_children_boxes(
        dom, styles, &child_ids, 1,
    )))
}

/// `node`単体(とその子孫)から[`LayoutBox`]を構築する。`build_box_tree`が
/// 文書全体を辿る際の内部処理だが、ストリーミング処理では
/// 「切り出したトップレベル要素1つ分だけ」の`LayoutBox`を作るために直接使う
/// (`build_box_tree`のように`dom.document()`の子全部を辿るのではなく、
/// 特定の`node`だけを対象にする)。
pub(crate) fn build_box_for_element(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
) -> Option<LayoutBox> {
    let style = styles.get(&node)?;
    if style.display == Display::None {
        return None;
    }
    if style.display == Display::Table {
        return Some(LayoutBox::for_node(
            node,
            BoxContent::Table(build_table_box(dom, styles, node)),
        ));
    }
    if style.display == Display::Flex {
        return Some(LayoutBox::for_node(
            node,
            BoxContent::Flex(build_flex_box(dom, styles, node)),
        ));
    }
    if style.display == Display::Grid {
        return Some(LayoutBox::for_node(
            node,
            BoxContent::Grid(GridBox {
                items: build_flex_box(dom, styles, node).items,
            }),
        ));
    }

    let child_ids: Vec<NodeId> = dom.children(node).collect();
    let has_block_child = child_ids
        .iter()
        .any(|&c| child_kind(dom, styles, c) == ChildKind::Block);

    let content = if has_block_child {
        // `::before`/`::after`はブロック子を持つ要素では非対応(簡略化)。
        // 無名ボックス生成規則との組み合わせが複雑になるため見送る。
        let list_item_start = read_list_item_start(dom, node);
        BoxContent::Blocks(build_children_boxes(
            dom,
            styles,
            &child_ids,
            list_item_start,
        ))
    } else {
        let mut spans = Vec::new();
        push_before_content(styles, node, &mut spans);
        for &child in &child_ids {
            match child_kind(dom, styles, child) {
                ChildKind::Inline => collect_spans(dom, styles, child, &mut spans),
                ChildKind::Whitespace => {
                    push_collapsible_whitespace(dom, styles, child, &mut spans)
                }
                ChildKind::Block | ChildKind::None => {}
            }
        }
        push_after_content(styles, node, &mut spans);
        apply_first_letter(node, style, &mut spans);
        // `Vec`は最初のpushで最小4要素分を確保する。テキスト1つだけの箱
        // (表のセルなど)が大量にある文書では、この余剰がそのまま積み上がる。
        spans.shrink_to_fit();
        BoxContent::Inline(spans)
    };

    Some(LayoutBox::for_node(node, content))
}

/// box tree構築後に呼び、`<img>`要素に対応するボックス(`child_kind`により
/// ブロック扱いされ、この時点では中身が空の`BoxContent::Inline(vec![])`に
/// なっている)を実際に[`BoxContent::Image`]へ差し替える。
///
/// `image_cache`がフェッチ・デコードを行う(I/Oを伴う)。同じ`src`は
/// `image_cache`内でメモ化されるため、同一画像が繰り返し参照されても
/// 実際のフェッチ・デコードは初回のみ。
pub fn resolve_images(tree: &mut LayoutBox, dom: &Dom, image_cache: &ImageAssetCache) {
    if let Some(node) = tree.node {
        if let NodeData::Element { name, .. } = &dom.node(node).data {
            if &*name.local == "img" {
                tree.content = BoxContent::Image(build_image_box_content(dom, node, image_cache));
                return; // <img>はvoid element(子を持たない)なので再帰不要。
            }
        }
    }

    match &mut tree.content {
        BoxContent::Blocks(children) => {
            for child in children {
                resolve_images(child, dom, image_cache);
            }
        }
        BoxContent::Table(table) => {
            if let Some(caption) = &mut table.caption {
                resolve_images(caption, dom, image_cache);
            }
            for row in &mut table.rows {
                for cell in &mut row.cells {
                    resolve_images(&mut cell.content, dom, image_cache);
                }
            }
        }
        BoxContent::Flex(flex) => {
            for item in &mut flex.items {
                resolve_images(item, dom, image_cache);
            }
        }
        BoxContent::Grid(grid) => {
            for item in &mut grid.items {
                resolve_images(item, dom, image_cache);
            }
        }
        // 行に参加するアトミックボックス(インラインの`<img>`・
        // `display: inline-block`)の中も辿る。辿らないとインライン画像が常に
        // 「取得失敗」扱いになる。
        BoxContent::Inline(spans) => {
            for span in spans {
                if let Some(atomic) = span.atomic.as_deref_mut() {
                    resolve_images(atomic, dom, image_cache);
                }
            }
        }
        BoxContent::Image(_) => {}
    }
}

/// `background-image`が指定された要素の、デコード済み画像を`NodeId`キーで
/// 引けるようにする側マップを構築する。`<img>`の[`resolve_images`]と異なり
/// box tree(`LayoutBox`)の中身は一切変更しない(背景画像はレイアウトのサイズ
/// 計算に影響しない、描画専用の情報のため)。DOM木の再走査も不要で、カスケード
/// 計算済みの`styles`を`background_image.is_some()`でフィルタするだけで済む。
///
/// フェッチ・デコードに失敗した要素は、その要素だけ背景画像なし扱いにして
/// マップに含めない(0014と同じフォールバック方針、文書全体は止めない)。
pub fn resolve_background_images(
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    image_cache: &ImageAssetCache,
) -> HashMap<NodeId, Rc<PreparedImage>> {
    let mut out = HashMap::new();
    for (&node, style) in styles {
        let Some(url) = &style.background_image else {
            continue;
        };
        if let Ok(image) = image_cache.get_or_decode(url) {
            out.insert(node, image);
        }
    }
    out
}

fn build_image_box_content(
    dom: &Dom,
    node: NodeId,
    image_cache: &ImageAssetCache,
) -> ImageBoxContent {
    let attrs = crate::img::read_img_attrs(dom, node);
    let image = attrs
        .as_ref()
        .and_then(|a| image_cache.get_or_decode(&a.src).ok());
    ImageBoxContent {
        image,
        attr_width: attrs.as_ref().and_then(|a| a.width),
        attr_height: attrs.as_ref().and_then(|a| a.height),
    }
}

/// `list_item_start`は、この子ボックス列の中で`display: list-item`の子を
/// 数える際の初期値(`<ol start="N">`のHTML属性、未指定は1)。この関数の呼び出し
/// 単位(=1つのコンテナの直接の子)がそのままカウンタのスコープになる
/// (入れ子の`<ol>`/`<ul>`はそれぞれ独立した呼び出しになるため、副作用的に
/// 1から数え直す)。
fn build_children_boxes(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    child_ids: &[NodeId],
    list_item_start: usize,
) -> Vec<LayoutBox> {
    let mut result = Vec::new();
    let mut pending_spans: Vec<InlineSpan> = Vec::new();
    let mut list_item_counter = list_item_start;

    for &child in child_ids {
        match child_kind(dom, styles, child) {
            ChildKind::None => {}
            ChildKind::Block => {
                flush_pending_spans(&mut pending_spans, &mut result);
                if let Some(mut b) = build_box_for_element(dom, styles, child) {
                    apply_list_item_marker(styles, child, &mut b, &mut list_item_counter);
                    result.push(b);
                }
            }
            ChildKind::Inline => collect_spans(dom, styles, child, &mut pending_spans),
            ChildKind::Whitespace => {
                push_collapsible_whitespace(dom, styles, child, &mut pending_spans)
            }
        }
    }
    flush_pending_spans(&mut pending_spans, &mut result);

    result
}

/// 空白のみのテキストノード(`ChildKind::Whitespace`)をスパン列に足す。
///
/// `<span>one</span> <span>two</span>`のように、インライン要素同士の間にある
/// 空白は単語間の空白として意味を持つ(行組みの段階で1個に畳み込まれる)ため、
/// 捨てずにスパンとして残す必要がある。一方、直前にインライン内容が無い場合
/// (ブロックの直後や親の先頭)は、畳み込みが効くならその空白は行頭に来るだけで
/// 結果に影響しないので足さない。空白だけが並んだ列から無名ボックスが
/// 作られないことは[`flush_pending_spans`]が保証する。
///
/// ただし`white-space: pre`では行頭の空白もそのまま残る(インデントとして
/// 意味を持つ)ため、この間引きをしてはいけない。`<pre>   <b>x</b>y</pre>`の
/// ように空白のみのテキストノードで始まる場合に効く。
fn push_collapsible_whitespace(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    // `white-space`は継承プロパティなので、テキストノード自身の計算スタイルに
    // 親の値が入っている。
    let preserves_leading_whitespace =
        styles.get(&node).map(|s| s.white_space) == Some(WhiteSpace::Pre);
    if out.is_empty() && !preserves_leading_whitespace {
        return;
    }
    // 元のテキストをそのまま渡す(`white-space: pre`の経路は空白の並びを
    // そのまま使うため、ここで1個に潰してはいけない)。
    collect_spans(dom, styles, node, out);
}

/// `node`の計算スタイルが`::first-letter`にマッチしていれば(`first_letter_style`
/// が`Some`)、`spans`のうち最初の非空白文字を含むspanから先頭1文字を分離し、
/// `is_first_letter: true`のspanとして直前に挿入する。
///
/// 既知の簡略化: 先頭の空白・約物のスキップは行わない(単純にテキストの
/// 最初の1文字を対象にする)。`spans`はホストの直接のテキスト内容のみを見るため、
/// ネストしたインライン要素の中から始まる内容には適用されない。
fn apply_first_letter(node: NodeId, style: &ComputedStyle, spans: &mut Vec<InlineSpan>) {
    if style.first_letter_style.is_none() {
        return;
    }
    let Some((span_index, char_len)) = spans
        .iter()
        .enumerate()
        .find_map(|(i, span)| span.text.chars().next().map(|c| (i, c.len_utf8())))
    else {
        return;
    };

    let first_letter_text = spans[span_index].text[..char_len].to_string();
    spans[span_index].text.replace_range(..char_len, "");
    spans.insert(
        span_index,
        InlineSpan {
            node,
            text: first_letter_text,
            is_first_letter: true,
            is_forced_break: false,
            atomic: None,
            link: spans[span_index].link.clone(),
            background_color: spans[span_index].background_color,
            relative_insets: spans[span_index].relative_insets.clone(),
        },
    );
}

/// `node`(`b`に対応する要素)が`display: list-item`であれば、カウンタを1つ
/// 進めた上でマーカーテキストを`b`に反映する。`list-style-position: inside`か
/// つ`b`の内容が`BoxContent::Inline`の場合は、`::before`と同じ要領で先頭に
/// `InlineSpan`として埋め込む(この場合`b.marker`は`None`のまま)。それ以外は
/// `b.marker`にテキストを持たせ、実際の
/// 配置はレイアウト層(`block.rs`)に委ねる。
fn apply_list_item_marker(
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    b: &mut LayoutBox,
    counter: &mut usize,
) {
    let Some(style) = styles.get(&node) else {
        return;
    };
    if style.display != Display::ListItem {
        return;
    }
    let n = *counter;
    *counter += 1;
    let Some(text) = format_list_marker(style.list_style_type, n) else {
        return;
    };

    if style.list_style_position == ListStylePosition::Inside {
        if let BoxContent::Inline(spans) = &mut b.content {
            spans.insert(0, InlineSpan::text(node, format!("{text} ")));
            return;
        }
    }
    b.marker = Some(text);
}

/// `list-style-type`からマーカーテキストを生成する。`None`はマーカーなし
/// (`list-style-type: none`)。
fn format_list_marker(list_style_type: ListStyleType, n: usize) -> Option<String> {
    match list_style_type {
        ListStyleType::None => None,
        ListStyleType::Disc => Some("•".to_string()),
        ListStyleType::Circle => Some("◦".to_string()),
        ListStyleType::Square => Some("▪".to_string()),
        ListStyleType::Decimal => Some(format!("{n}.")),
        ListStyleType::DecimalLeadingZero => Some(format!("{n:02}.")),
        ListStyleType::LowerRoman => {
            Some(format!("{}.", crate::numbering::to_roman(n).to_lowercase()))
        }
        ListStyleType::UpperRoman => Some(format!("{}.", crate::numbering::to_roman(n))),
        ListStyleType::LowerAlpha => {
            Some(format!("{}.", crate::numbering::to_alpha(n).to_lowercase()))
        }
        ListStyleType::UpperAlpha => Some(format!("{}.", crate::numbering::to_alpha(n))),
    }
}

/// `start`属性(`<ol start="N">`)を読む(未指定・0以下・非数値は1として扱う、
/// `read_colspan`/`read_rowspan`と同じ方針)。
fn read_list_item_start(dom: &Dom, node: NodeId) -> usize {
    let NodeData::Element { attrs, .. } = &dom.node(node).data else {
        return 1;
    };
    attrs
        .iter()
        .find(|attr| &*attr.name.local == "start")
        .and_then(|attr| attr.value.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// `table_node`(`display: table`)の子孫から`table-row`要素と`caption`を集めて
/// [`TableBox`]を組み立てる。
fn build_table_box(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    table_node: NodeId,
) -> TableBox {
    let mut rows = Vec::new();
    let mut caption_node = None;
    collect_table_rows(dom, styles, table_node, &mut rows, &mut caption_node);

    let caption_side = caption_node
        .and_then(|node| styles.get(&node))
        .map(|s| s.caption_side)
        .unwrap_or_default();
    let caption = caption_node
        .and_then(|node| build_box_for_element(dom, styles, node))
        .map(Box::new);

    TableBox {
        caption,
        caption_side,
        rows,
        column_widths: collect_column_widths(dom, styles, table_node),
    }
}

/// `<colgroup>`/`<col>`から列幅ヒントを列インデックス順に集める。
///
/// `<colgroup>`が`<col>`を子に持てばその`<col>`群を、持たなければ
/// `<colgroup>`自身を`span`属性の回数だけ列として展開する。テーブル直下の
/// `<col>`(`<colgroup>`を省略した書き方。html5everは`<colgroup>`を暗黙補完
/// するが、防御的に直下も見る)も同様に扱う。
fn collect_column_widths(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    table_node: NodeId,
) -> Vec<Option<LengthPercentage>> {
    let mut widths = Vec::new();

    fn push_column(
        styles: &HashMap<NodeId, Rc<ComputedStyle>>,
        node: NodeId,
        span: usize,
        out: &mut Vec<Option<LengthPercentage>>,
    ) {
        let width = match styles.get(&node).map(|s| s.width) {
            Some(LengthPercentageOrAuto::LengthPercentage(lp)) => Some(lp),
            _ => None,
        };
        for _ in 0..span {
            out.push(width);
        }
    }

    for child in dom.children(table_node) {
        let Some(local_name) = element_local_name(dom, child) else {
            continue;
        };
        match local_name.as_str() {
            "colgroup" => {
                let cols: Vec<NodeId> = dom
                    .children(child)
                    .filter(|&c| element_local_name(dom, c).as_deref() == Some("col"))
                    .collect();
                if cols.is_empty() {
                    push_column(styles, child, read_span(dom, child), &mut widths);
                } else {
                    for col in cols {
                        push_column(styles, col, read_span(dom, col), &mut widths);
                    }
                }
            }
            "col" => push_column(styles, child, read_span(dom, child), &mut widths),
            _ => {}
        }
    }

    widths
}

/// フォームコントロールの表示テキストを生成する。
///
/// `<input>`はvoid要素でテキストノードを持たないため、`value`/`placeholder`
/// 属性から生成する必要がある。`<select>`は選択中の`<option>`のテキストを
/// 表示する(`<option>`自身はUAスタイルシートで`display: none`のまま)。
fn push_form_control_content(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    let NodeData::Element { name, attrs, .. } = &dom.node(node).data else {
        return;
    };
    let attr = |key: &str| {
        attrs
            .iter()
            .find(|a| &*a.name.local == key)
            .map(|a| a.value.to_string())
    };

    let text = match &*name.local {
        "input" => {
            let input_type = attr("type").unwrap_or_else(|| "text".to_string());
            match input_type.trim().to_ascii_lowercase().as_str() {
                // チェックボックス・ラジオは枠と塗りだけで表す。
                "checkbox" | "radio" | "hidden" | "file" | "color" | "range" => None,
                "submit" => Some(attr("value").unwrap_or_else(|| "Submit".to_string())),
                "reset" => Some(attr("value").unwrap_or_else(|| "Reset".to_string())),
                _ => attr("value").or_else(|| attr("placeholder")),
            }
        }
        // `<select>`は`selected`が付いた`<option>`、無ければ最初の`<option>`。
        "select" => selected_option_text(dom, node),
        _ => None,
    };

    if let Some(text) = text.filter(|t| !t.is_empty()) {
        let mut span = InlineSpan::text(node, text);
        // 生成テキストは要素自身の計算スタイルで描画する(`::before`と同じ扱い)。
        span.background_color = styles
            .get(&node)
            .map(|s| s.background_color)
            .filter(|c| c.alpha > 0.0)
            .unwrap_or(RgbaColor::TRANSPARENT);
        out.push(span);
    }
}

/// `<select>`の表示テキスト(選択中の`<option>`、無ければ最初の`<option>`)。
fn selected_option_text(dom: &Dom, select: NodeId) -> Option<String> {
    let mut first: Option<String> = None;
    let mut stack: Vec<NodeId> = dom.children(select).collect();
    stack.reverse();
    while let Some(node) = stack.pop() {
        let NodeData::Element { name, attrs, .. } = &dom.node(node).data else {
            continue;
        };
        match &*name.local {
            "option" => {
                let text = collect_text_content(dom, node);
                if attrs.iter().any(|a| &*a.name.local == "selected") {
                    return Some(text);
                }
                if first.is_none() && !text.is_empty() {
                    first = Some(text);
                }
            }
            // `<optgroup>`の中の`<option>`も対象にする。
            "optgroup" => {
                let mut children: Vec<NodeId> = dom.children(node).collect();
                children.reverse();
                stack.extend(children);
            }
            _ => {}
        }
    }
    first
}

/// `node`以下のテキストノードを連結する(前後の空白は落とす)。
fn collect_text_content(dom: &Dom, node: NodeId) -> String {
    fn walk(dom: &Dom, node: NodeId, out: &mut String) {
        if let NodeData::Text { contents } = &dom.node(node).data {
            out.push_str(contents);
        }
        for child in dom.children(node) {
            walk(dom, child, out);
        }
    }
    let mut out = String::new();
    walk(dom, node, &mut out);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `display: inline-block`要素の中身を、
/// 通常のブロックと同じ規則で組み立てる。
fn build_inline_block_box(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
) -> Option<LayoutBox> {
    let child_ids: Vec<NodeId> = dom.children(node).collect();
    let has_block_child = child_ids
        .iter()
        .any(|&c| child_kind(dom, styles, c) == ChildKind::Block);

    let content = if has_block_child {
        BoxContent::Blocks(build_children_boxes(dom, styles, &child_ids, 1))
    } else {
        let style = styles.get(&node)?;
        let mut spans = Vec::new();
        push_before_content(styles, node, &mut spans);
        push_form_control_content(dom, styles, node, &mut spans);
        for &child in &child_ids {
            match child_kind(dom, styles, child) {
                ChildKind::Inline => collect_spans(dom, styles, child, &mut spans),
                ChildKind::Whitespace => {
                    push_collapsible_whitespace(dom, styles, child, &mut spans)
                }
                ChildKind::Block | ChildKind::None => {}
            }
        }
        push_after_content(styles, node, &mut spans);
        apply_first_letter(node, style, &mut spans);
        // `Vec`は最初のpushで最小4要素分を確保する。テキスト1つだけの箱
        // (表のセルなど)が大量にある文書では、この余剰がそのまま積み上がる。
        spans.shrink_to_fit();
        BoxContent::Inline(spans)
    };

    Some(LayoutBox::for_node(node, content))
}

/// `node`が`href`を持つ`<a>`要素であれば、その値。
/// `javascript:`スキームはリンクとして扱わない。
fn link_href(dom: &Dom, node: NodeId) -> Option<Rc<str>> {
    let NodeData::Element { name, attrs, .. } = &dom.node(node).data else {
        return None;
    };
    if &*name.local != "a" {
        return None;
    }
    let href = attrs
        .iter()
        .find(|attr| &*attr.name.local == "href")
        .map(|attr| attr.value.trim())
        .filter(|href| !href.is_empty())?;
    if href.len() >= 11 && href[..11].eq_ignore_ascii_case("javascript:") {
        return None;
    }
    Some(Rc::from(href))
}

fn element_local_name(dom: &Dom, node: NodeId) -> Option<String> {
    match &dom.node(node).data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

/// `<col span>`/`<colgroup span>`を読む(未指定・0以下・非数値は1、`colspan`と
/// 同じ寛容さ)。
fn read_span(dom: &Dom, node: NodeId) -> usize {
    let NodeData::Element { attrs, .. } = &dom.node(node).data else {
        return 1;
    };
    attrs
        .iter()
        .find(|attr| &*attr.name.local == "span")
        .and_then(|attr| attr.value.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// flexコンテナ(`node`)の子ごとに1個ずつflexアイテムを構築する。CSS仕様上
/// flexアイテムは各子要素ごとに独立して生成され、隣接するinline-level要素を
/// 1つの無名ボックスへまとめる規則(`build_children_boxes`)はflexコンテナの
/// 子要素には適用されないため、`build_box_for_element`を子要素ごとに直接呼ぶ。
/// 子要素自身の`display`値(`block`/`table`/入れ子の`flex`等)はそのまま
/// 尊重され、そのアイテムの中身のレイアウトに使われる(ネスト無制限)。
///
/// 要素で包まれていない裸のテキストは、連続する並びをまとめて1個の無名
/// flexアイテムにする(CSS Flexbox §4)。空白だけの並びからはアイテムを
/// 作らない。`display: none`の子はボックスを生成しないので、それを挟んだ
/// 前後のテキストは連続しているものとして1個にまとまる。
fn build_flex_box(dom: &Dom, styles: &HashMap<NodeId, Rc<ComputedStyle>>, node: NodeId) -> FlexBox {
    let mut items = Vec::new();
    let mut pending_spans: Vec<InlineSpan> = Vec::new();

    for child in dom.children(node) {
        match &dom.node(child).data {
            NodeData::Element { .. } => {
                if styles.get(&child).map(|s| s.display) == Some(Display::None) {
                    continue;
                }
                // 要素は必ず独立したアイテムになるので、ここまでに溜めた
                // テキストの並びを先に無名アイテムとして確定させる。
                flush_pending_spans(&mut pending_spans, &mut items);
                if let Some(item) = build_box_for_element(dom, styles, child) {
                    items.push(item);
                }
            }
            NodeData::Text { .. } => collect_spans(dom, styles, child, &mut pending_spans),
            _ => {}
        }
    }
    flush_pending_spans(&mut pending_spans, &mut items);

    FlexBox { items }
}

/// `node`の子を辿り、`table-row`を見つけたら行として収集し、`table-caption`を
/// 見つけたら(最初の1つだけ)`out_caption`に記録する。`thead`/`tbody`/`tfoot`
/// のような透過的な入れ物(`table-row`/`table-caption`でも`table`でもない要素)は
/// 素通りして再帰する。入れ子の`table`はそれ自体が別のテーブルなので
/// (その中の行は内側のテーブルに属する)ここでは再帰しない。
fn collect_table_rows(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<TableRow>,
    out_caption: &mut Option<NodeId>,
) {
    collect_table_rows_in_section(dom, styles, node, TableSection::Body, out, out_caption);
    // `<tfoot>`はHTML4では`<tbody>`より前に書く決まりだった。セクションが
    // 分かるようになったので、ソース順に関わらず末尾へ寄せる。安定
    // ソートなのでセクション内の順序は保たれる。
    out.sort_by_key(|row| match row.section {
        TableSection::Head => 0,
        TableSection::Body => 1,
        TableSection::Foot => 2,
    });
}

/// テーブル(またはその中の`<thead>`等の入れ物)の子が、テーブル構造の中で
/// 何として扱われるか。
enum TableChild {
    Row,
    Caption,
    /// `<thead>`/`<tbody>`/`<tfoot>`。専用の`display`値を持たない透明な入れ物
    /// なので、中の行をそのセクションとして集める。
    Section(TableSection),
    /// 行にもセクションにもならない子。無名の行・セルでくるむ対象
    /// (CSS2.1 17.2.1 規則2.1)。
    Content,
    /// ボックスを生成しない(`display: none`・列指定・コメント等)。
    Ignored,
}

fn table_child_kind(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
) -> TableChild {
    if !matches!(dom.node(node).data, NodeData::Element { .. }) {
        // テキストノードは内容として扱う(空白のみのものは、その並びが
        // 空白しか無ければ`flush_anonymous_row`が捨てる)。コメント等は無視。
        return match dom.node(node).data {
            NodeData::Text { .. } => TableChild::Content,
            _ => TableChild::Ignored,
        };
    }

    match styles.get(&node).map(|s| s.display) {
        Some(Display::TableRow) => TableChild::Row,
        Some(Display::TableCaption) => TableChild::Caption,
        Some(Display::None) | None => TableChild::Ignored,
        _ => match element_local_name(dom, node).as_deref() {
            Some("thead") => TableChild::Section(TableSection::Head),
            Some("tfoot") => TableChild::Section(TableSection::Foot),
            Some("tbody") => TableChild::Section(TableSection::Body),
            // 列を表すボックスは描画されず、無名ボックスも生成しない
            // (幅のヒントは`collect_column_widths`が別途読む)。
            Some("colgroup") | Some("col") => TableChild::Ignored,
            _ => TableChild::Content,
        },
    }
}

/// [`collect_table_rows`]の本体。`section`は「今いる入れ物」が示すセクション。
fn collect_table_rows_in_section(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    section: TableSection,
    out: &mut Vec<TableRow>,
    out_caption: &mut Option<NodeId>,
) {
    // 行にならない子が連続する区間。区切りに達したところで無名の行にまとめる。
    let mut pending: Vec<NodeId> = Vec::new();

    for child in dom.children(node) {
        match table_child_kind(dom, styles, child) {
            TableChild::Row => {
                flush_anonymous_row(dom, styles, &mut pending, section, out);
                out.push(build_table_row(dom, styles, child, section));
            }
            TableChild::Caption => {
                flush_anonymous_row(dom, styles, &mut pending, section, out);
                if out_caption.is_none() {
                    *out_caption = Some(child);
                }
            }
            TableChild::Section(child_section) => {
                flush_anonymous_row(dom, styles, &mut pending, section, out);
                collect_table_rows_in_section(dom, styles, child, child_section, out, out_caption);
            }
            TableChild::Content => pending.push(child),
            TableChild::Ignored => {}
        }
    }

    flush_anonymous_row(dom, styles, &mut pending, section, out);
}

/// 溜まっている「行にならない子」を1つの無名`table-row`にまとめて`out`へ積む
/// (CSS2.1 17.2.1 規則2.1)。空白のみの並びは行を作らずに捨てる(規則1の
/// 「無意味なボックスを取り除く」に相当)。
fn flush_anonymous_row(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    pending: &mut Vec<NodeId>,
    section: TableSection,
    out: &mut Vec<TableRow>,
) {
    if pending.is_empty() {
        return;
    }
    let children = std::mem::take(pending);
    if children
        .iter()
        .all(|&child| is_ignorable_whitespace(dom, child))
    {
        return;
    }
    let cells = build_row_cells(dom, styles, &children);
    if cells.is_empty() {
        return;
    }
    out.push(TableRow {
        node: None,
        cells,
        section,
    });
}

/// 空白のみのテキストノードか。テーブル構造の隙間(行やセルの間)にある
/// 空白は、ボックスを生成しない。
fn is_ignorable_whitespace(dom: &Dom, node: NodeId) -> bool {
    match &dom.node(node).data {
        NodeData::Text { contents } => white_space::is_collapsible_only(contents),
        _ => false,
    }
}

/// 行の子(`children`)からセル列を作る。`display: table-cell`はそのまま
/// セルになり、そうでない子は連続するかたまりごとに無名のセルでくるむ
/// (CSS2.1 17.2.1 規則2.2)。
fn build_row_cells(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    children: &[NodeId],
) -> Vec<TableCell> {
    let mut cells = Vec::new();
    let mut pending: Vec<NodeId> = Vec::new();

    for &child in children {
        let display = styles.get(&child).map(|s| s.display);
        if display == Some(Display::TableCell) {
            flush_anonymous_cell(dom, styles, &mut pending, &mut cells);
            cells.push(TableCell {
                node: Some(child),
                colspan: read_colspan(dom, child),
                rowspan: read_rowspan(dom, child),
                content: build_box_for_element(dom, styles, child)
                    .unwrap_or_else(|| LayoutBox::for_node(child, BoxContent::Inline(Vec::new()))),
            });
            continue;
        }
        if display == Some(Display::None) || !generates_a_box(dom, child) {
            continue;
        }
        pending.push(child);
    }
    flush_anonymous_cell(dom, styles, &mut pending, &mut cells);

    cells
}

/// 要素以外(コメント等)や列の指定はボックスを生成しない。
fn generates_a_box(dom: &Dom, node: NodeId) -> bool {
    match &dom.node(node).data {
        NodeData::Text { .. } => true,
        NodeData::Element { .. } => !matches!(
            element_local_name(dom, node).as_deref(),
            Some("colgroup") | Some("col")
        ),
        _ => false,
    }
}

/// 溜まっている「セルにならない子」を1つの無名`table-cell`にまとめる。
/// 空白のみの並びはセルを作らずに捨てる。
fn flush_anonymous_cell(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    pending: &mut Vec<NodeId>,
    cells: &mut Vec<TableCell>,
) {
    if pending.is_empty() {
        return;
    }
    let children = std::mem::take(pending);
    if children
        .iter()
        .all(|&child| is_ignorable_whitespace(dom, child))
    {
        return;
    }
    cells.push(TableCell {
        node: None,
        colspan: 1,
        rowspan: 1,
        content: LayoutBox::anonymous(BoxContent::Blocks(build_children_boxes(
            dom, styles, &children, 1,
        ))),
    });
}

fn build_table_row(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    row_node: NodeId,
    section: TableSection,
) -> TableRow {
    let children: Vec<NodeId> = dom.children(row_node).collect();
    TableRow {
        node: Some(row_node),
        cells: build_row_cells(dom, styles, &children),
        section,
    }
}

/// `colspan`属性を読む(未指定・0以下・非数値は1として扱う)。
fn read_colspan(dom: &Dom, node: NodeId) -> usize {
    let NodeData::Element { attrs, .. } = &dom.node(node).data else {
        return 1;
    };
    attrs
        .iter()
        .find(|attr| &*attr.name.local == "colspan")
        .and_then(|attr| attr.value.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

/// `rowspan`属性を読む(未指定・0以下・非数値は1として扱う。`rowspan="0"`の
/// 特殊値も非対応で1扱い、`read_colspan`と同じ方針)。
fn read_rowspan(dom: &Dom, node: NodeId) -> usize {
    let NodeData::Element { attrs, .. } = &dom.node(node).data else {
        return 1;
    };
    attrs
        .iter()
        .find(|attr| &*attr.name.local == "rowspan")
        .and_then(|attr| attr.value.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(1)
}

fn flush_pending_spans(pending: &mut Vec<InlineSpan>, result: &mut Vec<LayoutBox>) {
    // 空白のみのテキストからは無名ブロックを作らない(CSS2.1 9.2.2.1)。
    // ただしアトミックボックス(インラインの`<img>`・`display: inline-block`)は
    // `text`が空でも意味のある内容なので、1つでもあれば無名ブロックを作る
    let has_meaningful_content = pending
        .iter()
        .any(|span| span.atomic.is_some() || !white_space::is_collapsible_only(&span.text));
    if has_meaningful_content {
        let mut spans = std::mem::take(pending);
        spans.shrink_to_fit();
        result.push(LayoutBox::anonymous(BoxContent::Inline(spans)));
    }
    pending.clear();
}

fn child_kind(dom: &Dom, styles: &HashMap<NodeId, Rc<ComputedStyle>>, node: NodeId) -> ChildKind {
    match &dom.node(node).data {
        NodeData::Element { .. } => {
            let display = styles.get(&node).map(|s| s.display);
            if display == Some(Display::None) {
                return ChildKind::None;
            }
            match display {
                // `inline-block`は親の行に参加する(中身は
                // ブロックとしてレイアウトされる)。
                Some(Display::InlineBlock) => ChildKind::Inline,
                Some(Display::Block)
                | Some(Display::Table)
                | Some(Display::ListItem)
                | Some(Display::Flex)
                | Some(Display::Grid) => ChildKind::Block,
                Some(Display::Inline) => ChildKind::Inline,
                // table-row/table-cell/table-captionは`build_table_box`が専用に
                // 探索するため、通常のブロック/インライン走査では(不正な
                // マークアップ等でテーブル文脈の外に出現しない限り)出現しない。
                // 防御的に無視する。
                Some(Display::TableRow)
                | Some(Display::TableCell)
                | Some(Display::TableCaption) => ChildKind::None,
                Some(Display::None) | None => ChildKind::None,
            }
        }
        NodeData::Text { contents } => {
            // `&nbsp;`だけのテキストノードは「空白のみ」ではない(畳み込まれない
            // 内容を持つ)ので、`str::trim`ではなくCSSの分類で判定する。
            if white_space::is_collapsible_only(contents) {
                ChildKind::Whitespace
            } else {
                ChildKind::Inline
            }
        }
        _ => ChildKind::None,
    }
}

/// インライン要素の子孫を再帰的に辿り、テキストノードごとに[`InlineSpan`]を積む。
/// テキストノード自身の計算スタイルに、祖先のインライン要素(`<b>`/`<span>`等)の
/// カスケード・継承結果が反映済みのため、ここではノードIDを保持するだけでよい。
/// 各インライン要素の`::before`/`::after`生成コンテンツも、対応する子孫の
/// 前後にスパンとして挿入する。
fn collect_spans(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    collect_spans_in_context(dom, styles, node, &InlineContext::default(), out)
}

/// [`collect_spans`]の本体。`context`は「このノードを囲むインライン要素から
/// 受け継ぐ情報」(IFCの外側=ブロック側の指定は含まない)。
fn collect_spans_in_context(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    context: &InlineContext,
    out: &mut Vec<InlineSpan>,
) {
    match &dom.node(node).data {
        NodeData::Text { contents } => out.push(InlineSpan::text_in_inline_context(
            node,
            contents.clone(),
            context,
        )),
        NodeData::Element { name, .. } => {
            // インライン文脈の子孫にも`display: none`を効かせる。`child_kind`は
            // ブロック/インラインの振り分け時にしか呼ばれないため、ここで見ないと
            // 「インライン要素の中にある非表示要素」(例:
            // `<p>a <select><option>x</option></select> b</p>`)の子孫テキストが
            // 本文へ漏れる(UAスタイルシートによる非表示化の前提)。
            if styles.get(&node).map(|s| s.display) == Some(Display::None) {
                return;
            }
            // `<br>`は子を持たない強制改行マーカー。
            if &*name.local == "br" {
                out.push(InlineSpan::forced_break(node));
                return;
            }
            // `<wbr>`は子を持たない「ここで改行してよい」マーカー
            // (HTML仕様: line break opportunity)。ZWSPを1つ置くだけで、幅ゼロの
            // 改行機会という同じ意味になる(`layout::white_space`が改行機会と
            // して扱う)。ブラウザの実装も同様。
            if &*name.local == "wbr" {
                out.push(InlineSpan::text_in_inline_context(
                    node,
                    white_space::ZERO_WIDTH_SPACE.to_string(),
                    context,
                ));
                return;
            }
            // インラインの`<img>`(置換要素)も1つの箱として行に参加する。
            // 中身は`resolve_images`が後から`BoxContent::Image`へ差し替える。
            if &*name.local == "img" {
                out.push(InlineSpan::atomic(
                    node,
                    LayoutBox::for_node(node, BoxContent::Inline(Vec::new())),
                ));
                return;
            }
            // `display: inline-block`は1つの箱として行に参加する。中身は
            // 通常のブロックと同じ規則で構築する。
            if styles.get(&node).map(|s| s.display) == Some(Display::InlineBlock) {
                if let Some(mut atomic) = build_inline_block_box(dom, styles, node) {
                    atomic.marker = None;
                    out.push(InlineSpan::atomic(node, atomic));
                }
                return;
            }
            // このインライン要素自身が背景色を持つなら、以降の子孫はその背景で
            // 塗られる(入れ子の場合は内側が勝つ、CSSの背景の重なりの簡略化)。
            // `<a href>`のリンクも同様に、以降の子孫へ受け継がれる。
            let mut context = context.clone();
            if let Some(background) = styles
                .get(&node)
                .map(|s| s.background_color)
                .filter(|c| c.alpha > 0.0)
            {
                context.background_color = background;
            }
            if let Some(href) = link_href(dom, node) {
                context.link = Some(href);
            }
            // A `position: relative` inline element shifts the runs of all its
            // descendants together (#29).
            if let Some(inset) = styles.get(&node).and_then(|s| RelativeInset::of(s)) {
                context.relative_insets.push(inset);
            }
            push_before_content(styles, node, out);
            for child in dom.children(node) {
                collect_spans_in_context(dom, styles, child, &context, out);
            }
            push_after_content(styles, node, out);
        }
        _ => {}
    }
}

/// `node`に`::before`の生成コンテンツがあれば、その計算スタイルを引くための
/// ノードID(`node`自身)と共にスパンを積む。
fn push_before_content(
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    if let Some(text) = styles
        .get(&node)
        .and_then(|s| s.pseudo_before_content.as_ref())
    {
        out.push(InlineSpan::text(node, text.clone()));
    }
}

/// `node`に`::after`の生成コンテンツがあれば、その計算スタイルを引くための
/// ノードID(`node`自身)と共にスパンを積む。
fn push_after_content(
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    if let Some(text) = styles
        .get(&node)
        .and_then(|s| s.pseudo_after_content.as_ref())
    {
        out.push(InlineSpan::text(node, text.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;
    use crate::style::{
        compute_styles, parse_stylesheet, user_agent_stylesheet, RgbaColor, Stylesheet,
    };

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    fn find_inline_spans(b: &LayoutBox) -> Option<&Vec<InlineSpan>> {
        match &b.content {
            BoxContent::Inline(spans) => Some(spans),
            BoxContent::Blocks(children) => children.iter().find_map(find_inline_spans),
            BoxContent::Image(_) => None,
            BoxContent::Table(table) => table
                .caption
                .as_deref()
                .and_then(find_inline_spans)
                .or_else(|| {
                    table
                        .rows
                        .iter()
                        .flat_map(|row| &row.cells)
                        .find_map(|cell| find_inline_spans(&cell.content))
                }),
            BoxContent::Flex(flex) => flex.items.iter().find_map(find_inline_spans),
            BoxContent::Grid(grid) => grid.items.iter().find_map(find_inline_spans),
        }
    }

    #[test]
    fn an_inline_img_becomes_an_atomic_span_inside_the_text() {
        // `<img>`の既定displayはinlineなので、テキストと同じ
        // インラインボックスにアトミックボックスとして載る。
        let dom = html::parse(br#"<p>before <img src="a.png"> after</p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        let spans = find_inline_spans(p_box).expect("expected inline content");

        let atomic_count = spans.iter().filter(|s| s.atomic.is_some()).count();
        assert_eq!(atomic_count, 1, "the <img> should be one atomic span");
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text.replace(char::is_whitespace, ""), "beforeafter");
    }

    #[test]
    fn a_lone_inline_img_between_blocks_is_not_dropped() {
        // 回帰テスト: `flush_pending_spans`が「テキストが空白のみ」で無名ブロックを
        // 捨てていたため、`<p>`兄弟の間の裸の`<img>`が消えていた。
        let dom = html::parse(br#"<p>a</p><img src="x.png"><p>b</p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        fn count_atomics(b: &LayoutBox) -> usize {
            match &b.content {
                BoxContent::Inline(spans) => spans.iter().filter(|s| s.atomic.is_some()).count(),
                BoxContent::Blocks(children) => children.iter().map(count_atomics).sum(),
                _ => 0,
            }
        }
        assert_eq!(count_atomics(&tree), 1, "the lone <img> must survive");
    }

    #[test]
    fn a_block_img_is_still_a_block_replaced_element() {
        // `display: block`を明示した`<img>`は従来どおりブロック置換要素
        // (アトミックスパンにはならない)。
        let dom = html::parse(br#"<div><img src="a.png" style="display: block;"></div>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let img = find(&dom, dom.document(), "img").expect("img not found");
        let img_box = find_box(&tree, img).expect("the block <img> should have its own box");
        assert!(matches!(
            img_box.content,
            BoxContent::Inline(_) | BoxContent::Image(_)
        ));
    }

    #[test]
    fn inline_element_boundaries_are_preserved_as_separate_spans() {
        let dom = html::parse(br#"<p>before <b>bold</b> after</p>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let b = find(&dom, p, "b").expect("b not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        let spans = find_inline_spans(p_box).expect("expected inline content");

        assert_eq!(spans.len(), 3, "before-text / bold-text / after-text");
        assert_eq!(spans[0].text, "before ");
        assert_eq!(spans[1].text, "bold");
        assert_eq!(spans[2].text, " after");
        // 太字テキストのスパンは<b>の子テキストノード由来であり、<p>直下のテキストとは
        // 別のNodeIdを持つ(=別の計算スタイルを引ける)。
        assert_ne!(spans[0].node, spans[1].node);
        assert_eq!(dom.children(b).next(), Some(spans[1].node));
    }

    /// `<p>`(最初のもの)のスパン列のテキストを連結して返す。
    fn first_p_text(html_src: &[u8]) -> String {
        let dom = html::parse(html_src);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);
        let p = find(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        find_inline_spans(p_box)
            .expect("expected inline content")
            .iter()
            .map(|span| span.text.as_str())
            .collect()
    }

    #[test]
    fn whitespace_between_two_inline_elements_is_kept() {
        // 回帰テスト(issue #3): 空白のみのテキストノードを一律で捨てていたため、
        // インライン要素同士の間の単語間空白が消えて`onetwo`になっていた。
        assert_eq!(
            first_p_text(br#"<p><span>one</span> <span>two</span></p>"#),
            "one two"
        );
    }

    #[test]
    fn whitespace_between_inline_elements_is_kept_across_a_whole_run() {
        // 3つ以上並んだ場合も、間の空白がすべて残る。
        assert_eq!(
            first_p_text(br#"<p><b>one</b> <i>two</i> <span>three</span></p>"#),
            "one two three"
        );
    }

    #[test]
    fn a_newline_between_two_inline_elements_is_kept() {
        // 整形されたマークアップでよくある改行も、単語間の空白として残る
        // (1個の空白へ畳み込むのは行組み側`layout::inline`の仕事)。
        assert_eq!(
            first_p_text(b"<p><span>one</span>\n  <span>two</span></p>"),
            "one\n  two"
        );
    }

    #[test]
    fn a_non_breaking_space_between_two_inline_elements_is_kept() {
        // `&nbsp;`は`char::is_whitespace`が真になるため「空白のみのテキスト
        // ノード」として一緒に捨てられていた。
        assert_eq!(
            first_p_text("<p><span>one</span>\u{a0}<span>two</span></p>".as_bytes()),
            "one\u{a0}two"
        );
    }

    #[test]
    fn whitespace_before_the_first_inline_child_creates_no_span() {
        // 行頭に来るだけで結果に影響しない空白はスパンを作らない
        // (整形されたマークアップでスパンが無駄に増えないように)。末尾側は
        // 行組みが無視するので残っていてよい(`white-space: pre`では意味を持つ)。
        let dom = html::parse(b"<p>\n  <span>one</span>\n</p>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        let spans = find_inline_spans(p_box).expect("expected inline content");

        assert_eq!(
            spans[0].text, "one",
            "no span should precede the first word, got {spans:?}"
        );
    }

    #[test]
    fn leading_whitespace_is_kept_when_white_space_preserves_it() {
        // `white-space: pre`では行頭の空白もインデントとして意味を持つので、
        // 空白のみのテキストノードで始まっていても捨ててはいけない
        // (捨てていた頃は`<pre>   <b>x</b>y</pre>`が`xy`になっていた)。
        let dom = html::parse(b"<pre>   <b>x</b>y</pre>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let pre = find(&dom, dom.document(), "pre").expect("pre not found");
        let pre_box = find_box(&tree, pre).expect("pre box not found");
        let spans = find_inline_spans(pre_box).expect("expected inline content");

        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(text, "   xy", "the indentation must survive, got {spans:?}");
    }

    #[test]
    fn wbr_becomes_a_zero_width_space() {
        // `<wbr>`は「ここで改行してよい」だけを表す要素。ZWSPを1つ置いて
        // `layout::white_space`の改行機会の規則に載せる。
        let dom = html::parse(br#"<p>aaa<wbr>bbb</p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        let spans = find_inline_spans(p_box).expect("expected inline content");

        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(text, "aaa\u{200b}bbb");
        // `<br>`とは違い強制改行ではない(改行「機会」を足すだけ)。
        assert!(spans.iter().all(|span| !span.is_forced_break));
    }

    #[test]
    fn whitespace_between_block_siblings_creates_no_anonymous_box() {
        // ブロックの間の空白は従来どおりボックスを生成しない(CSS2.1 9.2.2.1)。
        let dom = html::parse(b"<div>\n  <p>a</p>\n  <p>b</p>\n</div>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let div = find(&dom, dom.document(), "div").expect("div not found");
        let div_box = find_box(&tree, div).expect("div box not found");
        let BoxContent::Blocks(children) = &div_box.content else {
            panic!("expected block children, got {:?}", div_box.content);
        };
        assert_eq!(children.len(), 2, "the two <p> only, got {children:?}");
    }

    #[test]
    fn span_style_reflects_ancestor_cascade_at_layout_time() {
        let dom = html::parse(br#"<p>plain <b style="color: rgb(9, 9, 9);">loud</b></p>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        let spans = find_inline_spans(p_box).expect("expected inline content");

        let loud_style = &styles[&spans[1].node];
        assert_eq!(
            loud_style.color,
            RgbaColor {
                red: 9,
                green: 9,
                blue: 9,
                alpha: 1.0
            }
        );
        assert_eq!(loud_style.font_weight, crate::style::FontWeight::Bold);
    }

    #[test]
    fn before_and_after_content_are_prepended_and_appended_as_spans() {
        // <span>はインライン要素なので、単独では自分自身のLayoutBoxを持たず
        // 祖先のブロックコンテナ(ここでは<body>)の平坦化されたスパン列に
        // 織り込まれる。それでも::before/::afterは正しく前後に挿入されるはず。
        let dom = html::parse(br#"<span class="badge">Text</span>"#);
        let ua = user_agent_stylesheet();
        let author = crate::style::parse_stylesheet(
            r#".badge::before { content: "["; } .badge::after { content: "]"; }"#,
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let span = find(&dom, dom.document(), "span").expect("span not found");
        let spans = find_inline_spans(&tree).expect("expected inline content");

        assert_eq!(spans.len(), 3, "before / text / after");
        assert_eq!(spans[0].text, "[");
        assert_eq!(spans[1].text, "Text");
        assert_eq!(spans[2].text, "]");
        // 生成コンテンツのスパンはホスト要素自身のノードIDを持つ
        // (=ホストの計算スタイルをそのまま流用する)。
        assert_eq!(spans[0].node, span);
        assert_eq!(spans[2].node, span);
    }

    #[test]
    fn element_without_before_after_rules_has_no_extra_spans() {
        let dom = html::parse(br#"<span>Text</span>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let spans = find_inline_spans(&tree).expect("expected inline content");

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Text");
    }

    #[test]
    fn display_none_inside_an_inline_context_contributes_no_spans() {
        let dom = html::parse(br#"<p>a <select><option>LEAK</option></select> b</p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let spans = find_inline_spans(&tree).expect("expected inline content");
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert!(
            !text.contains("LEAK"),
            "hidden descendants must not contribute text, got {text:?}"
        );
    }

    #[test]
    fn stray_table_cells_get_an_anonymous_row() {
        // CSS2.1 17.2.1 規則2.1: `table`直下の連続する`table-cell`は1つの
        // 無名`table-row`にまとまる。
        let dom = html::parse(
            br#"<div style="display: table">
                <div style="display: table-cell">alpha</div>
                <div style="display: table-cell">beta</div>
            </div>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "div").expect("table not found");
        let table_box = find_box(&tree, table_node).expect("table box not found");
        let BoxContent::Table(table) = &table_box.content else {
            panic!("expected a table box");
        };

        assert_eq!(table.rows.len(), 1, "one anonymous row for both cells");
        assert_eq!(table.rows[0].node, None, "the row has no DOM node");
        assert_eq!(table.rows[0].cells.len(), 2);
        assert!(
            table.rows[0].cells.iter().all(|cell| cell.node.is_some()),
            "the cells themselves are real elements"
        );
    }

    #[test]
    fn non_cell_children_get_an_anonymous_cell() {
        // CSS2.1 17.2.1 規則2.2: セルでない子は、連続するかたまりごとに
        // 1つの無名`table-cell`でくるまれる。
        let dom = html::parse(
            br#"<div style="display: table">
                <div style="display: table-row">
                    <div>alpha</div>
                    <div>beta</div>
                    <div style="display: table-cell">gamma</div>
                </div>
            </div>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "div").expect("table not found");
        let table_box = find_box(&tree, table_node).expect("table box not found");
        let BoxContent::Table(table) = &table_box.content else {
            panic!("expected a table box");
        };

        assert_eq!(table.rows.len(), 1);
        let cells = &table.rows[0].cells;
        assert_eq!(cells.len(), 2, "one anonymous cell + the explicit one");
        assert_eq!(
            cells[0].node, None,
            "alpha and beta share an anonymous cell"
        );
        assert!(cells[1].node.is_some());
        let BoxContent::Blocks(blocks) = &cells[0].content.content else {
            panic!("expected block content in the anonymous cell");
        };
        assert_eq!(blocks.len(), 2, "both blocks live in that one cell");
    }

    #[test]
    fn whitespace_between_table_children_creates_no_anonymous_row() {
        let dom = html::parse(
            br#"<table>
                <tr><td>alpha</td></tr>
                <tr><td>beta</td></tr>
            </table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let table_box = find_box(&tree, table_node).expect("table box not found");
        let BoxContent::Table(table) = &table_box.content else {
            panic!("expected a table box");
        };

        assert_eq!(table.rows.len(), 2, "only the two explicit rows");
        assert!(table.rows.iter().all(|row| row.node.is_some()));
    }

    #[test]
    fn table_rows_and_cells_are_collected_through_thead_tbody() {
        let dom = html::parse(
            br#"<table>
                <thead><tr><th>Name</th><th>Price</th></tr></thead>
                <tbody>
                    <tr><td>Apple</td><td>100</td></tr>
                    <tr><td>Banana</td><td>200</td></tr>
                </tbody>
            </table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let table_box = find_box(&tree, table_node).expect("table box not found");
        let BoxContent::Table(table) = &table_box.content else {
            panic!("expected a table box");
        };

        assert_eq!(table.rows.len(), 3, "thead + 2 tbody rows");
        assert_eq!(table.rows[0].cells.len(), 2);
        let first_cell_text = |content: &LayoutBox| match &content.content {
            BoxContent::Inline(spans) => spans.iter().map(|s| s.text.as_str()).collect::<String>(),
            _ => panic!("expected inline cell content"),
        };
        assert_eq!(first_cell_text(&table.rows[0].cells[0].content), "Name");
        assert_eq!(first_cell_text(&table.rows[1].cells[0].content), "Apple");
        assert_eq!(first_cell_text(&table.rows[2].cells[0].content), "Banana");
    }

    #[test]
    fn caption_content_is_collected_and_kept_separate_from_rows() {
        let dom = html::parse(
            br#"<table>
                <caption>Fruit Prices</caption>
                <tr><td>Apple</td><td>100</td></tr>
            </table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let table_box = find_box(&tree, table_node).expect("table box not found");
        let BoxContent::Table(table) = &table_box.content else {
            panic!("expected a table box");
        };

        assert_eq!(
            table.rows.len(),
            1,
            "caption should not be collected as a row"
        );
        let caption = table.caption.as_ref().expect("caption not found");
        let BoxContent::Inline(spans) = &caption.content else {
            panic!("expected inline caption content");
        };
        let text: String = spans.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(text, "Fruit Prices");
    }

    #[test]
    fn table_without_a_caption_has_none() {
        let dom = html::parse(br#"<table><tr><td>a</td></tr></table>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let table_box = find_box(&tree, table_node).expect("table box not found");
        let BoxContent::Table(table) = &table_box.content else {
            panic!("expected a table box");
        };
        assert!(table.caption.is_none());
    }

    #[test]
    fn colspan_attribute_is_read_from_the_cell() {
        let dom =
            html::parse(br#"<table><tr><td colspan="3">wide</td><td>narrow</td></tr></table>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let BoxContent::Table(table) = &find_box(&tree, table_node).unwrap().content else {
            panic!("expected a table box");
        };
        assert_eq!(table.rows[0].cells[0].colspan, 3);
        assert_eq!(table.rows[0].cells[1].colspan, 1);
    }

    #[test]
    fn invalid_or_missing_colspan_defaults_to_one() {
        let dom = html::parse(
            br#"<table><tr><td colspan="0">a</td><td colspan="not-a-number">b</td><td>c</td></tr></table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let BoxContent::Table(table) = &find_box(&tree, table_node).unwrap().content else {
            panic!("expected a table box");
        };
        for cell in &table.rows[0].cells {
            assert_eq!(cell.colspan, 1);
        }
    }

    #[test]
    fn rowspan_attribute_is_read_from_the_cell() {
        let dom = html::parse(
            br#"<table>
                <tr><td rowspan="2">tall</td><td>a</td></tr>
                <tr><td>b</td></tr>
            </table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let BoxContent::Table(table) = &find_box(&tree, table_node).unwrap().content else {
            panic!("expected a table box");
        };
        assert_eq!(table.rows[0].cells[0].rowspan, 2);
        assert_eq!(table.rows[0].cells[1].rowspan, 1);
        assert_eq!(table.rows[1].cells[0].rowspan, 1);
    }

    #[test]
    fn invalid_or_missing_rowspan_defaults_to_one() {
        let dom = html::parse(
            br#"<table><tr><td rowspan="0">a</td><td rowspan="not-a-number">b</td><td>c</td></tr></table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        let BoxContent::Table(table) = &find_box(&tree, table_node).unwrap().content else {
            panic!("expected a table box");
        };
        for cell in &table.rows[0].cells {
            assert_eq!(cell.rowspan, 1);
        }
    }

    #[test]
    fn nested_table_rows_belong_to_the_inner_table_only() {
        // 入れ子のtableの<tr>は、内側のtableに属し、外側のtableの行としては
        // 収集されないはず。
        let dom = html::parse(
            br#"<table id="outer"><tr><td>
                <table id="inner"><tr><td>nested</td></tr></table>
            </td></tr></table>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let outer_node = find(&dom, dom.document(), "table").expect("outer table not found");
        let BoxContent::Table(outer_table) = &find_box(&tree, outer_node).unwrap().content else {
            panic!("expected a table box");
        };

        assert_eq!(
            outer_table.rows.len(),
            1,
            "outer table should have exactly one row"
        );
        assert_eq!(outer_table.rows[0].cells.len(), 1);
        // 外側の唯一のセルの中身はブロックコンテナ(内側のtableを含む)であり、
        // 内側のtableの行が紛れ込んでいないはず。
        let BoxContent::Blocks(cell_children) = &outer_table.rows[0].cells[0].content.content
        else {
            panic!("expected the outer cell to contain a block (the nested table)")
        };
        assert_eq!(cell_children.len(), 1);
        let BoxContent::Table(inner_table) = &cell_children[0].content else {
            panic!("expected the nested table box")
        };
        assert_eq!(inner_table.rows.len(), 1);
    }

    fn find_box(b: &LayoutBox, target: NodeId) -> Option<&LayoutBox> {
        if b.node == Some(target) {
            return Some(b);
        }
        if let BoxContent::Blocks(children) = &b.content {
            for child in children {
                if let Some(found) = find_box(child, target) {
                    return Some(found);
                }
            }
        }
        None
    }

    fn jpeg_data_uri() -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/images/spike_gradient.jpg"
        );
        let bytes = std::fs::read(path).unwrap();
        format!("data:image/jpeg;base64,{}", STANDARD.encode(bytes))
    }

    #[test]
    fn resolve_background_images_decodes_only_nodes_with_background_image_set() {
        // `resolve_background_images`はDOM木の再走査をせず、カスケード
        // 計算済みの`styles`を`background_image.is_some()`で
        // フィルタするだけで側マップを構築できるはず。
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(&format!(
            r#"div {{ background-image: url("{}"); }}"#,
            jpeg_data_uri()
        ));
        let styles = compute_styles(&dom, &ua, &author);

        let image_cache = ImageAssetCache::new(std::path::PathBuf::from("."), false);
        let background_images = resolve_background_images(&styles, &image_cache);

        assert!(
            background_images.contains_key(&div),
            "div should have a decoded background image"
        );
        assert!(
            !background_images.contains_key(&p),
            "p has no background-image declared and should not be in the map"
        );
    }

    #[test]
    fn resolve_background_images_skips_a_failed_fetch_without_panicking() {
        let dom = html::parse(br#"<div></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(r#"div { background-image: url("does-not-exist.png"); }"#);
        let styles = compute_styles(&dom, &ua, &author);

        let image_cache = ImageAssetCache::new(std::path::PathBuf::from("."), false);
        let background_images = resolve_background_images(&styles, &image_cache);

        assert!(
            background_images.is_empty(),
            "a failed background-image fetch should be skipped, not panic"
        );
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

    #[test]
    fn list_items_are_numbered_in_document_order_and_reset_for_nested_lists() {
        let dom = html::parse(
            br#"<ol>
                <li>a</li>
                <li>b</li>
                <li><ol><li>nested-a</li><li>nested-b</li></ol></li>
            </ol>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let mut lis = Vec::new();
        find_all(&dom, dom.document(), "li", &mut lis);
        assert_eq!(lis.len(), 5, "3 top-level li + 2 nested li");

        assert_eq!(
            find_box(&tree, lis[0]).unwrap().marker.as_deref(),
            Some("1.")
        );
        assert_eq!(
            find_box(&tree, lis[1]).unwrap().marker.as_deref(),
            Some("2.")
        );
        // 3つ目の`li`はブロック子(入れ子の`ol`)を持つため、自身は
        // マーカーだけを持つ(内容は`BoxContent::Blocks`)。
        assert_eq!(
            find_box(&tree, lis[2]).unwrap().marker.as_deref(),
            Some("3.")
        );
        // 入れ子の`ol`は独立したカウンタスコープを持つため1から数え直す。
        assert_eq!(
            find_box(&tree, lis[3]).unwrap().marker.as_deref(),
            Some("1.")
        );
        assert_eq!(
            find_box(&tree, lis[4]).unwrap().marker.as_deref(),
            Some("2.")
        );
    }

    #[test]
    fn ol_start_attribute_sets_the_initial_counter_value() {
        let dom = html::parse(br#"<ol start="5"><li>a</li><li>b</li></ol>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let mut lis = Vec::new();
        find_all(&dom, dom.document(), "li", &mut lis);
        assert_eq!(
            find_box(&tree, lis[0]).unwrap().marker.as_deref(),
            Some("5.")
        );
        assert_eq!(
            find_box(&tree, lis[1]).unwrap().marker.as_deref(),
            Some("6.")
        );
    }

    #[test]
    fn list_style_type_none_suppresses_the_marker_but_still_advances_the_counter() {
        let dom = html::parse(br#"<ol><li style="list-style-type: none;">a</li><li>b</li></ol>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let mut lis = Vec::new();
        find_all(&dom, dom.document(), "li", &mut lis);
        assert_eq!(find_box(&tree, lis[0]).unwrap().marker, None);
        // `none`の項目もカウンタは1つ消費する(実際のブラウザの挙動に合わせる)。
        assert_eq!(
            find_box(&tree, lis[1]).unwrap().marker.as_deref(),
            Some("2.")
        );
    }

    #[test]
    fn list_style_position_inside_embeds_the_marker_as_the_first_inline_span() {
        let dom = html::parse(br#"<ul style="list-style-position: inside;"><li>text</li></ul>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let li = find(&dom, dom.document(), "li").expect("li not found");
        let li_box = find_box(&tree, li).expect("li box not found");
        // `inside`はspansへ埋め込むため、`marker`フィールド自体は`None`のまま。
        assert_eq!(li_box.marker, None);
        let BoxContent::Inline(spans) = &li_box.content else {
            panic!("expected inline content");
        };
        assert_eq!(spans.len(), 2, "marker span + original text span");
        assert_eq!(spans[0].text, "• ");
        assert_eq!(spans[1].text, "text");
    }

    #[test]
    fn list_style_position_inside_falls_back_to_a_separate_marker_when_li_has_block_children() {
        let dom =
            html::parse(br#"<ul style="list-style-position: inside;"><li><p>text</p></li></ul>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let li = find(&dom, dom.document(), "li").expect("li not found");
        let li_box = find_box(&tree, li).expect("li box not found");
        assert_eq!(li_box.marker.as_deref(), Some("•"));
        assert!(matches!(li_box.content, BoxContent::Blocks(_)));
    }

    #[test]
    fn format_list_marker_covers_all_list_style_types() {
        assert_eq!(format_list_marker(ListStyleType::None, 1), None);
        assert_eq!(
            format_list_marker(ListStyleType::Disc, 1).as_deref(),
            Some("•")
        );
        assert_eq!(
            format_list_marker(ListStyleType::Circle, 1).as_deref(),
            Some("◦")
        );
        assert_eq!(
            format_list_marker(ListStyleType::Square, 1).as_deref(),
            Some("▪")
        );
        assert_eq!(
            format_list_marker(ListStyleType::Decimal, 12).as_deref(),
            Some("12.")
        );
        assert_eq!(
            format_list_marker(ListStyleType::DecimalLeadingZero, 3).as_deref(),
            Some("03.")
        );
        assert_eq!(
            format_list_marker(ListStyleType::DecimalLeadingZero, 123).as_deref(),
            Some("123.")
        );
        assert_eq!(
            format_list_marker(ListStyleType::LowerRoman, 4).as_deref(),
            Some("iv.")
        );
        assert_eq!(
            format_list_marker(ListStyleType::UpperRoman, 1994).as_deref(),
            Some("MCMXCIV.")
        );
        assert_eq!(
            format_list_marker(ListStyleType::LowerAlpha, 27).as_deref(),
            Some("aa.")
        );
        assert_eq!(
            format_list_marker(ListStyleType::UpperAlpha, 26).as_deref(),
            Some("Z.")
        );
    }

    #[test]
    fn first_letter_splits_the_first_character_of_plain_text_into_its_own_span() {
        let dom = html::parse(br#"<p>Hello world</p>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet("p::first-letter { font-size: 2em; color: rgb(200, 0, 0); }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let text_node = dom.children(p).next().expect("p should have a text child");
        let spans = find_inline_spans(&tree).expect("expected inline content");

        assert_eq!(spans.len(), 2, "first-letter span + remainder span");
        assert_eq!(spans[0].text, "H");
        assert!(spans[0].is_first_letter);
        // 分割された先頭文字スパンは、::first-letterスタイルを引くためホスト要素(p)自身のノードIDを持つ。
        assert_eq!(spans[0].node, p);
        assert_eq!(spans[1].text, "ello world");
        assert!(!spans[1].is_first_letter);
        // 残り部分は元のテキストノードのIDのまま(分割前と変わらない)。
        assert_eq!(spans[1].node, text_node);
    }

    #[test]
    fn first_letter_is_not_split_off_without_a_matching_rule() {
        let dom = html::parse(br#"<p>Hello</p>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let spans = find_inline_spans(&tree).expect("expected inline content");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "Hello");
        assert!(!spans[0].is_first_letter);
    }

    fn flex_items(html_src: &str, css: &str) -> Vec<LayoutBox> {
        let dom = html::parse(html_src.as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
        let container = find(&dom, dom.document(), "div").expect("div not found");
        build_flex_box(&dom, &styles, container).items
    }

    fn item_text(item: &LayoutBox) -> String {
        find_inline_spans(item)
            .map(|spans| spans.iter().map(|s| s.text.as_str()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn bare_text_in_a_flex_container_becomes_an_anonymous_item() {
        // 回帰テスト: 以前は要素で包まれていないテキストを捨てていたため、
        // `<div class="seal">サンプル</div>`のような中身が消えていた。
        let items = flex_items(r#"<div class="f">bare text</div>"#, ".f { display: flex; }");

        assert_eq!(items.len(), 1, "expected one anonymous flex item");
        assert!(items[0].node.is_none(), "the item should be anonymous");
        assert_eq!(item_text(&items[0]), "bare text");
    }

    #[test]
    fn whitespace_only_text_in_a_flex_container_creates_no_item() {
        let items = flex_items(
            r#"<div class="f">   <p>x</p>   </div>"#,
            ".f { display: flex; }",
        );

        assert_eq!(items.len(), 1, "only the <p> should become an item");
        assert!(items[0].node.is_some());
    }

    #[test]
    fn a_text_run_next_to_an_element_becomes_a_separate_anonymous_item() {
        // 要素は必ず独立したアイテムになるので、その前後のテキストは
        // 別々の無名アイテムへ分かれる。
        let items = flex_items(
            r#"<div class="f">left<p>mid</p>right</div>"#,
            ".f { display: flex; }",
        );

        assert_eq!(items.len(), 3);
        assert_eq!(item_text(&items[0]), "left");
        assert!(items[1].node.is_some(), "the <p> keeps its own node");
        assert_eq!(item_text(&items[2]), "right");
    }

    #[test]
    fn contiguous_text_runs_merge_into_one_anonymous_item() {
        // `display: none`の子はボックスを作らないので、それを挟んだ前後の
        // テキストは連続しているものとして1つのアイテムにまとまる。
        let items = flex_items(
            r#"<div class="f">before<span class="hide">gone</span>after</div>"#,
            ".f { display: flex; } .hide { display: none; }",
        );

        assert_eq!(items.len(), 1);
        assert!(items[0].node.is_none());
        assert_eq!(item_text(&items[0]), "beforeafter");
    }

    #[test]
    fn bare_text_in_a_grid_container_becomes_an_anonymous_item() {
        // gridも`build_flex_box`でアイテムを集めるので同じ規則が効く。
        let dom = html::parse(r#"<div class="g">cellA<p>cellB</p></div>"#.as_bytes());
        let styles = compute_styles(
            &dom,
            &user_agent_stylesheet(),
            &parse_stylesheet(".g { display: grid; }"),
        );
        let tree = build_box_tree(&dom, &styles);
        let container = find(&dom, dom.document(), "div").expect("div not found");
        let container_box = find_box(&tree, container).expect("div box not found");

        let BoxContent::Grid(grid) = &container_box.content else {
            panic!("expected a grid container");
        };
        assert_eq!(grid.items.len(), 2);
        assert_eq!(item_text(&grid.items[0]), "cellA");
    }

    #[test]
    fn first_letter_handles_multibyte_characters_as_a_single_unit() {
        let dom = html::parse("<p>日本語のテスト</p>".as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet("p::first-letter { color: rgb(200, 0, 0); }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let spans = find_inline_spans(&tree).expect("expected inline content");
        assert_eq!(spans[0].text, "日");
        assert_eq!(spans[1].text, "本語のテスト");
    }

    /// 高さのメモはcontent幅とcontaining widthの組で引く。中身のパーセンテージは
    /// containing widthを基準に解決されるので、content幅が同じでも取り違えては
    /// いけない(ネストしたflex/gridでは、同じアイテムが違うcontaining widthで
    /// 何度も測られる)。
    #[test]
    fn the_height_memo_distinguishes_the_containing_width() {
        let memo = MeasureMemo::default();
        memo.set_height(100.0, 120.0, 40.0);

        assert_eq!(memo.height(100.0, 120.0), Some(40.0));
        assert_eq!(memo.height(100.0, 200.0), None);
        assert_eq!(memo.height(101.0, 120.0), None);
    }

    #[test]
    fn the_natural_width_memo_round_trips() {
        let memo = MeasureMemo::default();
        assert_eq!(memo.natural_width(), None);
        memo.set_natural_width(12.5);
        assert_eq!(memo.natural_width(), Some(12.5));
    }
}
