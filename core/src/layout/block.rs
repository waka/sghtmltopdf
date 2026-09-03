//! Block Formatting Context: containing blockに基づく幅計算と、
//! ブロック要素の縦積み配置(CSS2.1 §10.3.3, §9.4.1の簡略版)。
use std::borrow::Cow;
use std::collections::HashMap;
use std::rc::Rc;

use crate::fonts::FontCollection;
use crate::html::NodeId;
use crate::pdf::PreparedImage;
use crate::style::{
    BorderCollapse, BorderStyle, BoxSizing, BreakBetween, BreakInside, CaptionSide, Clear,
    ComputedStyle, Display, Float, Length, LengthPercentage, LengthPercentageOrAuto, MaxSize,
    Position,
};

use super::box_tree::{BoxContent, ImageBoxContent, LayoutBox, TableSection};
use super::flex::layout_flex;
use super::float_ctx::FloatContext;
use super::geometry::{EdgeSizes, FragmentPosition, Layout, Rect};
use super::grid::{layout_grid, LaidOutGrid};
use super::inline::{apply_text_overflow, finish_line, layout_inline_content, shape_run, LineBox};
use super::table::layout_table;

/// マーカー(`list-style-position: outside`)と内容のcontent edgeの間の固定の隙間(px)。
const LIST_MARKER_GAP: f32 = 8.0;

#[derive(Debug, Clone)]
pub struct LaidOutBox {
    pub node: Option<NodeId>,
    pub layout: Layout,
    /// このボックスの`break-before`/`break-after`/`break-inside`/`orphans`/`widows`の
    /// 計算値(ページ分割の判断にのみ使う。無名ボックスは`ComputedStyle`の
    /// 初期値=`auto`/`auto`/`auto`/2/2)。
    pub fragmentation: FragmentationHints,
    /// このボックスが実際に描画される背景色・枠線を持つか。
    /// `paginate.rs`が、ページをまたいで分割されるコンテナの装飾フラグメント
    /// (背景・枠線の再現、モジュールdoc参照)を生成する必要があるかどうかの
    /// 判断に使う。
    pub has_visible_decoration: bool,
    /// `float: left/right`が指定されている要素かどうか。`paginate.rs`が
    /// フロー外要素として特別扱いする判定に使う。
    pub is_float: bool,
    pub content: LaidOutContent,
    /// `display: list-item`のマーカー(箇条書きの記号・番号)。
    /// シェイピング済みの`TextRun`1つを持つ`LineBox`として表現し、
    /// `pdf::document::render_line`をそのまま再利用して描画する。
    /// ページ分割でこのボックスが複数ページにまたがる場合、先頭フラグメントにのみ残す(`paginate.rs`)。
    pub marker: Option<Box<LineBox>>,
}

/// [`LaidOutBox`]が持つCSS Fragmentation関連の計算値。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FragmentationHints {
    pub break_before: BreakBetween,
    pub break_after: BreakBetween,
    pub break_inside: BreakInside,
    pub orphans: u32,
    pub widows: u32,
}

impl From<&ComputedStyle> for FragmentationHints {
    fn from(style: &ComputedStyle) -> Self {
        Self {
            break_before: style.break_before,
            break_after: style.break_after,
            break_inside: style.break_inside,
            orphans: style.orphans,
            widows: style.widows,
        }
    }
}

impl Default for FragmentationHints {
    fn default() -> Self {
        Self::from(&ComputedStyle::default())
    }
}

#[derive(Debug, Clone)]
pub enum LaidOutContent {
    Blocks(Vec<LaidOutBox>),
    Inline(Vec<LineBox>),
    Table(LaidOutTable),
    /// `display: flex`
    Flex(Vec<LaidOutBox>),
    /// `display: grid`。ページ分割の単位である行帯を持つ。
    Grid(LaidOutGrid),
    /// `<img>`。フェッチ・デコードに失敗していれば`None`(空の置換要素として扱い、何も描画しない)。
    Image(Option<Rc<PreparedImage>>),
}

/// レイアウト済みのテーブル全体(任意のcaption+行の並び)。
#[derive(Debug, Clone)]
pub struct LaidOutTable {
    /// `Box`は`LaidOutBox`→`LaidOutContent::Table`→`LaidOutTable`の再帰を
    /// 間接参照で断ち切るために必要。
    pub caption: Option<Box<LaidOutBox>>,
    pub caption_side: CaptionSide,
    pub rows: Vec<LaidOutTableRow>,
}

/// レイアウト済みのテーブル行1行分。
#[derive(Debug, Clone)]
pub struct LaidOutTableRow {
    /// 元の`display: table-row`要素。無名行(CSSの無名ボックス生成規則で
    /// 作られた行)は`None`。
    pub node: Option<NodeId>,
    pub cells: Vec<LaidOutBox>,
    /// この行が属するセクション。`paginate`が`<thead>`の
    /// 行を各ページの先頭へ複製するために使う。
    pub section: TableSection,
}

/// 絶対配置されたボックス1つ分。
/// `laid`はcontaining block基準(絶対座標)でレイアウト済みで、
/// ページ分割層がこれを属するページへオーバーレイとして配置する。
#[derive(Debug, Clone)]
pub struct PositionedBox {
    pub laid: LaidOutBox,
    pub kind: PositionedKind,
}

/// 絶対配置ボックスの配置先の種別。
#[derive(Debug, Clone, Copy)]
pub enum PositionedKind {
    /// `position: fixed`。全ページのコンテンツ領域に、レイアウト時の座標
    /// そのままで繰り返す。
    Fixed,
    /// `position: absolute`でpositioned祖先が無い場合。最初のページの
    /// コンテンツ領域に、レイアウト時の座標そのままで置く。
    AbsoluteInitial,
    /// `position: absolute`でpositioned祖先がある場合。祖先(`node`)が最初に
    /// 現れたページに、祖先padding boxのページ内位置と`padding_box_origin`
    /// (レイアウト時の祖先padding box左上)の差分だけずらして置く。
    AbsoluteAncestor {
        node: NodeId,
        padding_box_origin: (f32, f32),
    },
}

/// レイアウト中に持ち回る絶対配置のコンテキスト。
/// `float_ctx`と同じく`&mut`で子孫へ渡す。
pub(super) struct PosCtx<'a> {
    /// 現在の`absolute`のcontaining block(最も近いpositioned祖先の
    /// padding box、無ければ最初のページのコンテンツ領域)。
    abs_cb: AbsCB,
    /// `fixed`のcontaining block = ページのコンテンツ領域の`(width, height)`。
    page_size: (f32, f32),
    /// 収集した絶対配置ボックス。
    out: &'a mut Vec<PositionedBox>,
}

/// `absolute`のcontaining block。
#[derive(Debug, Clone, Copy)]
enum AbsCB {
    /// initial containing block(positioned祖先が無い)= 最初のページの
    /// コンテンツ領域。原点は`(0, 0)`。
    InitialPage,
    /// positioned祖先のpadding box(絶対座標)。
    Ancestor { node: NodeId, rect: Rect },
}

impl<'a> PosCtx<'a> {
    pub(super) fn new(out: &'a mut Vec<PositionedBox>, page_size: (f32, f32)) -> Self {
        Self {
            abs_cb: AbsCB::InitialPage,
            page_size,
            out,
        }
    }
}

/// [`layout_document`]の絶対配置対応版。通常フローの`LaidOutBox`と、
/// 絶対配置ボックス([`PositionedBox`])のリストを返す。`page_size`は
/// `(content_width, content_height)`で、`fixed`のcontaining blockとして使う。
pub fn layout_document_positioned(
    root: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    page_size: (f32, f32),
) -> (LaidOutBox, Vec<PositionedBox>) {
    let mut absolutes = Vec::new();
    let mut pos = PosCtx::new(&mut absolutes, page_size);
    let laid =
        layout_document_from_positioned(root, styles, fonts, page_size.0, 0.0, 0.0, &mut pos);
    (laid, absolutes)
}

/// ページ幅を初期containing blockとして、ボックスツリー全体をレイアウトする。
pub fn layout_document(
    root: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    page_width: f32,
) -> LaidOutBox {
    layout_document_from(root, styles, fonts, page_width, 0.0, 0.0)
}

/// [`layout_document`]のバリアント: 原点`(0.0, 0.0)`からではなく、
/// `(start_x, start_y)`からレイアウトを開始する。
pub fn layout_document_from(
    root: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    start_x: f32,
    start_y: f32,
) -> LaidOutBox {
    // 絶対配置を集めない後方互換の入り口(既存の呼び出し元・テスト用)。
    let mut absolutes = Vec::new();
    let mut pos = PosCtx::new(&mut absolutes, (containing_width, 0.0));
    layout_document_from_positioned(
        root,
        styles,
        fonts,
        containing_width,
        start_x,
        start_y,
        &mut pos,
    )
}

/// [`layout_document_from`]の絶対配置対応版。
/// `pos`に絶対配置ボックスを集める。
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_document_from_positioned(
    root: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    start_x: f32,
    start_y: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    // `layout_document`/`layout_document_from`1回の呼び出し全体で1つの
    // `FloatContext`を共有する。
    let mut float_ctx = FloatContext::new();
    layout_box(
        root,
        styles,
        fonts,
        containing_width,
        &mut float_ctx,
        start_x,
        start_y,
        pos,
    )
}

/// `<caption>`(通常のwidth解決を経る、`table.rs`のcaption配置専用)や
/// block.rs内部の再帰呼び出しで使う。
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_box(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    float_ctx: &mut FloatContext,
    x: f32,
    y: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    layout_box_impl(
        b,
        styles,
        fonts,
        containing_width,
        None,
        None,
        float_ctx,
        x,
        y,
        pos,
    )
}

/// テーブルセルなど、通常の`width`解決(auto/margin計算)を経ずに
/// content-boxの幅を直接指定してレイアウトしたい場合に使う
/// ([`super::table`]専用)。
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_box_with_forced_width(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    forced_content_width: f32,
    float_ctx: &mut FloatContext,
    x: f32,
    y: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    layout_box_impl(
        b,
        styles,
        fonts,
        containing_width,
        Some(forced_content_width),
        None,
        float_ctx,
        x,
        y,
        pos,
    )
}

/// `layout_box_with_forced_width`の高さ版拡張。幅・高さの両方を強制する
/// ([`super::flex`]専用)。taffyが確定した各flexアイテムの幅・高さで実際の
/// `LaidOutBox`を得る最終レイアウトパスに使う。`align-items: stretch`(既定値)
/// でtaffyがアイテムをコンテナの高さへ引き伸ばした場合、この高さ強制によって
/// 背景色・枠線も引き伸ばされた高さ分だけ描画される。
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_box_with_forced_size(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    forced_content_width: f32,
    forced_content_height: f32,
    float_ctx: &mut FloatContext,
    x: f32,
    y: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    layout_box_impl(
        b,
        styles,
        fonts,
        containing_width,
        Some(forced_content_width),
        Some(forced_content_height),
        float_ctx,
        x,
        y,
        pos,
    )
}

/// 結果を捨てる採寸パス専用のラッパー。flexコンテナがtaffyへ内在サイズを
/// 返すために、同じアイテムを何度もレイアウトし直す経路で使う。
///
/// 採寸で見つかった`absolute`/`fixed`は捨てる。最終レイアウトパスが同じ
/// 子孫をもう一度通って本物の`PosCtx`へ集めるので、ここで集めると同じ
/// ボックスが何重にも登録されてしまう。
#[allow(clippy::too_many_arguments)]
pub(super) fn measure_box_with_forced_width(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    forced_content_width: f32,
    float_ctx: &mut FloatContext,
    x: f32,
    y: f32,
) -> LaidOutBox {
    let mut sink = Vec::new();
    let mut pos = PosCtx::new(&mut sink, (0.0, 0.0));
    layout_box_with_forced_width(
        b,
        styles,
        fonts,
        containing_width,
        forced_content_width,
        float_ctx,
        x,
        y,
        &mut pos,
    )
}

/// `b`のcontent幅・margin・padding・borderを解決する(置換要素のauto-size
/// 適用込み)。`layout_box_impl`本体と、float配置のための事前幅計算
/// (`layout_float_child`)の両方から呼ばれる共通ロジック。
fn layout_out_of_flow_child(
    child: &LayoutBox,
    child_style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    pos: &mut PosCtx,
) {
    let (cb_rect, kind) = if child_style.position == Position::Fixed {
        (
            Rect {
                x: 0.0,
                y: 0.0,
                width: pos.page_size.0,
                height: pos.page_size.1,
            },
            PositionedKind::Fixed,
        )
    } else {
        match pos.abs_cb {
            AbsCB::InitialPage => (
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: pos.page_size.0,
                    height: pos.page_size.1,
                },
                PositionedKind::AbsoluteInitial,
            ),
            AbsCB::Ancestor { node, rect } => (
                rect,
                PositionedKind::AbsoluteAncestor {
                    node,
                    padding_box_origin: (rect.x, rect.y),
                },
            ),
        }
    };

    let padding = resolve_padding(child_style, cb_rect.width);
    let border = resolve_border(child_style);
    let margin_left = resolve_lpa_or_zero(child_style.margin_left, cb_rect.width);
    let margin_right = resolve_lpa_or_zero(child_style.margin_right, cb_rect.width);
    let non_content_width =
        margin_left + border.left + padding.left + padding.right + border.right + margin_right;

    let has_left = !matches!(child_style.left, LengthPercentageOrAuto::Auto);
    let has_right = !matches!(child_style.right, LengthPercentageOrAuto::Auto);
    let left = resolve_lpa_or_zero(child_style.left, cb_rect.width);
    let right = resolve_lpa_or_zero(child_style.right, cb_rect.width);

    // content幅の解決。`min-width`/`max-width`は
    // 求めた使用幅をクランプする形で効かせる。
    let content_width = match child_style.width {
        LengthPercentageOrAuto::LengthPercentage(lp) => {
            let w = resolve_lp(lp, cb_rect.width);
            if child_style.box_sizing == BoxSizing::BorderBox {
                (w - padding.left - padding.right - border.left - border.right).max(0.0)
            } else {
                w
            }
        }
        LengthPercentageOrAuto::Auto if has_left && has_right => {
            (cb_rect.width - left - right - non_content_width).max(0.0)
        }
        LengthPercentageOrAuto::Auto => {
            let avail = (cb_rect.width - non_content_width).max(0.0);
            // 絶対配置の`width: auto`もshrink-to-fitなので、高さが確定していれば
            // `aspect-ratio`から幅を導ける。
            aspect_ratio_width(child_style, &padding, &border).unwrap_or_else(|| {
                shrink_to_fit_content_width(child, styles, fonts, child_style, avail)
            })
        }
    };
    let content_width = clamp_used_width(
        child_style,
        cb_rect.width,
        padding.left + padding.right,
        border.left + border.right,
        content_width,
    );

    let margin_box_width = non_content_width + content_width;
    // マージンボックス左上のx。
    let margin_box_x = if has_left {
        cb_rect.x + left
    } else if has_right {
        cb_rect.x + cb_rect.width - margin_box_width - right
    } else {
        cb_rect.x
    };
    let has_top = !matches!(child_style.top, LengthPercentageOrAuto::Auto);
    let has_bottom = !matches!(child_style.bottom, LengthPercentageOrAuto::Auto);
    let top = resolve_lpa_or_zero(child_style.top, cb_rect.height);
    let bottom = resolve_lpa_or_zero(child_style.bottom, cb_rect.height);
    // まず`top`(無ければcb上端)でレイアウトする。
    let margin_box_y = cb_rect.y + if has_top { top } else { 0.0 };

    let mut float_ctx = FloatContext::new();
    let mut laid = layout_box_with_forced_width(
        child,
        styles,
        fonts,
        cb_rect.width,
        content_width,
        &mut float_ctx,
        margin_box_x,
        margin_box_y,
        pos,
    );

    // `bottom`指定(かつ`top`未指定)は、レイアウト後の高さが分かってから
    // 下端合わせで再配置する。
    if !has_top && has_bottom && cb_rect.height > 0.0 {
        let mbh = laid.layout.margin_box_height();
        let target_y = cb_rect.y + cb_rect.height - mbh - bottom;
        shift_box_y_in_place(&mut laid, margin_box_y - target_y);
    }
    pos.out.push(PositionedBox { laid, kind });
}

/// shrink-to-fit(内容に合わせた)content幅。`display: inline-block`の
/// アトミックボックスとfloatの`width: auto`で共有する。CSS2.1のpreferred
/// minimum widthは持たないため`min(preferred, available)`に簡略化する(内容が
/// availableを超えると折り返す)。
pub(super) fn shrink_to_fit_content_width(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    style: &ComputedStyle,
    available_width: f32,
) -> f32 {
    let _ = style;
    let natural = super::table::measure_natural_content_width(b, styles, fonts);
    natural.min(available_width).max(0.0)
}

fn resolve_box_geometry(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    forced_content_width: Option<f32>,
) -> (ComputedStyle, EdgeSizes, EdgeSizes, EdgeSizes, f32) {
    let mut style = box_style(b, styles).into_owned();
    if let BoxContent::Image(image_content) = &b.content {
        apply_replaced_element_auto_size(&mut style, image_content, containing_width);
    }

    let padding = resolve_padding(&style, containing_width);
    let border = resolve_border(&style);
    let (content_width, margin_left, margin_right) = match forced_content_width {
        Some(w) => (
            w,
            resolve_lpa_or_zero(style.margin_left, containing_width),
            resolve_lpa_or_zero(style.margin_right, containing_width),
        ),
        // floatが明示`width`を持つ場合は
        // `resolve_width_and_horizontal_margins`を使わない: あの関数の
        // 「over-constrained」規則(width/margin-left/margin-right全てが非
        // auto=`margin`省略時のデフォルト0も含むときにmargin-rightを残り
        // 幅いっぱいに再計算する、CSS2.1 §10.3.3の通常フロー用ルール)を
        // 素通しすると、再計算後の巨大なmargin-rightが
        // `margin_box_width`(float配置計算に使う占有幅)に混入してしまう。
        // floatにはこの再計算規則が無い(CSS2.1 §10.3.5、auto marginは単純に
        // 0)ため、ここでは迂回する。
        None if style.float != Float::None
            && !matches!(style.width, LengthPercentageOrAuto::Auto) =>
        {
            let width = resolve_lpa_or_zero(style.width, containing_width);
            // `box-sizing: border-box`の場合の変換。通常フロー用の
            // `resolve_width_and_horizontal_margins`と
            // 同じ調整をここでも行う。
            let width = if style.box_sizing == BoxSizing::BorderBox {
                (width - padding.left - padding.right - border.left - border.right).max(0.0)
            } else {
                width
            };
            (
                clamp_used_width(
                    &style,
                    containing_width,
                    padding.left + padding.right,
                    border.left + border.right,
                    width,
                ),
                resolve_lpa_or_zero(style.margin_left, containing_width),
                resolve_lpa_or_zero(style.margin_right, containing_width),
            )
        }
        // floatで`width: auto`は内容に合わせて縮む(shrink-to-fit、CSS2.1§
        // 10.3.5)。通常フロー用の
        // `resolve_width_and_horizontal_margins`(containing widthいっぱいに
        // 広げる)には落とさない。margin autoはfloatでは0。
        None if style.float != Float::None => {
            let available = (containing_width
                - resolve_lpa_or_zero(style.margin_left, containing_width)
                - resolve_lpa_or_zero(style.margin_right, containing_width)
                - padding.left
                - padding.right
                - border.left
                - border.right)
                .max(0.0);
            // 高さが確定していれば`aspect-ratio`から幅を導ける。
            let width = aspect_ratio_width(&style, &padding, &border).unwrap_or_else(|| {
                shrink_to_fit_content_width(b, styles, fonts, &style, available)
            });
            (
                clamp_used_width(
                    &style,
                    containing_width,
                    padding.left + padding.right,
                    border.left + border.right,
                    width,
                ),
                resolve_lpa_or_zero(style.margin_left, containing_width),
                resolve_lpa_or_zero(style.margin_right, containing_width),
            )
        }
        None => resolve_width_and_horizontal_margins(
            &style,
            containing_width,
            padding.left + padding.right,
            border.left + border.right,
        ),
    };
    let margin = EdgeSizes {
        top: resolve_lpa_or_zero(style.margin_top, containing_width),
        right: margin_right,
        bottom: resolve_lpa_or_zero(style.margin_bottom, containing_width),
        left: margin_left,
    };

    (style, padding, border, margin, content_width)
}

#[allow(clippy::too_many_arguments)]
fn layout_box_impl(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    forced_content_width: Option<f32>,
    forced_content_height: Option<f32>,
    float_ctx: &mut FloatContext,
    x: f32,
    y: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    let (style, padding, border, mut margin, content_width) =
        resolve_box_geometry(b, styles, fonts, containing_width, forced_content_width);

    let content_x = x + margin.left + border.left + padding.left;
    let mut content_y = y + margin.top + border.top + padding.top;

    // positioned要素(relative/absolute/fixed)は、子孫の`absolute`の
    // containing blockを自分のpadding boxにする。高さは循環を避けるため
    // 使わない(bottom配置は非対応)。
    let saved_cb = pos.abs_cb;
    if style.position != Position::Static {
        if let Some(node) = b.node {
            pos.abs_cb = AbsCB::Ancestor {
                node,
                rect: Rect {
                    x: content_x - padding.left,
                    y: content_y - padding.top,
                    width: padding.left + content_width + padding.right,
                    height: 0.0,
                },
            };
        }
    }

    let (mut content, content_height) = match &b.content {
        BoxContent::Blocks(children) => {
            let mut cursor_y = content_y;
            let mut max_float_bottom = content_y;
            let mut laid_children: Vec<LaidOutBox> = Vec::with_capacity(children.len());
            for child in children {
                let child_style = box_style(child, styles);

                // `position: absolute`/`fixed`はフロー外(スペースを
                // 占めない)。containing block
                // 基準で配置して`pos.out`へ集める。
                if child_style.position.is_out_of_flow() {
                    layout_out_of_flow_child(child, &child_style, styles, fonts, pos);
                    continue;
                }

                if child_style.clear != Clear::None {
                    cursor_y = float_ctx.clearance(child_style.clear, cursor_y);
                }

                if child_style.float != Float::None {
                    // floatはフローに参加しない(CSS2.1 9.5): マージン相殺の対象外、
                    // `cursor_y`は進めない。`float_ctx`は子・孫にも共有されるため、
                    // このBFC内の以降の通常フロー・インラインコンテンツから
                    // 回り込み判定に見える。
                    let child_laid = layout_float_child(
                        child,
                        &child_style,
                        styles,
                        fonts,
                        content_width,
                        float_ctx,
                        content_x,
                        cursor_y,
                        pos,
                    );
                    let float_top = child_laid.layout.content.y
                        - child_laid.layout.padding.top
                        - child_laid.layout.border.top
                        - child_laid.layout.margin.top;
                    max_float_bottom =
                        max_float_bottom.max(float_top + child_laid.layout.margin_box_height());
                    laid_children.push(child_laid);
                    continue;
                }

                let child_margin_top = resolve_lpa_or_zero(child_style.margin_top, content_width);

                // 隣接兄弟間のマージン相殺(CSS2.1 §8.3.1)。前の兄弟のmargin-bottomと
                // この子のmargin-topを、単純な加算ではなく「正の最大値+負の最小値」
                // で相殺した1つの間隔に置き換える。floatはフローに参加しないため
                // 対象外(直前の非float子を探す)。
                if let Some(prev) = laid_children.iter().rev().find(|c| !c.is_float) {
                    let prev_margin_bottom = prev.layout.margin.bottom;
                    let collapsed = collapse_adjacent_margins(prev_margin_bottom, child_margin_top);
                    cursor_y -= prev_margin_bottom + child_margin_top - collapsed;
                }

                let child_laid = layout_box(
                    child,
                    styles,
                    fonts,
                    content_width,
                    float_ctx,
                    content_x,
                    cursor_y,
                    pos,
                );
                cursor_y += child_laid.layout.margin_box_height();
                laid_children.push(child_laid);
            }
            // 直接の子floatが通常フローより下に伸びていれば、その分だけ
            // auto-heightを拡張する(CSS2.1 10.6.7の浅い実装、孫要素には
            // 伝播しない、既知の簡略化)。
            let auto_height = cursor_y.max(max_float_bottom) - content_y;
            let height = resolve_used_height(&style, &padding, &border, content_width, auto_height);
            (LaidOutContent::Blocks(laid_children), height)
        }
        BoxContent::Inline(spans) => {
            let mut lines = layout_inline_content(
                spans,
                styles,
                fonts,
                content_width,
                content_x,
                content_y,
                Some(&*float_ctx),
                // 無名ボックスには自身のスタイルが無い(`style`は初期値)。
                b.node.is_some().then_some(&style),
                pos,
            );
            // 行内の`display: inline-block`ボックスは、行の位置が確定した
            // この時点で最終座標へ移動させる。
            place_atomic_inlines(&mut lines);
            // `text-overflow: ellipsis`は行組みの後処理として適用する。
            apply_text_overflow(&mut lines, &style, content_width, fonts);
            let lines_height: f32 = lines.iter().map(|line| line.rect.height).sum();
            let height =
                resolve_used_height(&style, &padding, &border, content_width, lines_height);
            (LaidOutContent::Inline(lines), height)
        }
        BoxContent::Table(table) => {
            // `display: table`のセルは新しいBlock Formatting Contextを
            // 確立する(CSS2.1 9.4.1)ため、外側の`float_ctx`とは独立させる。
            // `border-spacing`は`border-collapse: collapse`とは排他なので、
            // collapseの場合はここで0に潰してから渡す。
            let (h_spacing, v_spacing) = if style.border_collapse == BorderCollapse::Collapse {
                (0.0, 0.0)
            } else {
                (
                    style.border_spacing_horizontal.0,
                    style.border_spacing_vertical.0,
                )
            };
            let (laid_table, table_height) = layout_table(
                table,
                styles,
                fonts,
                content_width,
                style.table_layout,
                h_spacing,
                v_spacing,
                content_x,
                content_y,
                pos,
            );
            let height =
                resolve_used_height(&style, &padding, &border, content_width, table_height);
            (LaidOutContent::Table(laid_table), height)
        }
        BoxContent::Grid(grid) => {
            // グリッドコンテナもflex/tableと同様に新しいフォーマッティング
            // コンテキストを確立する。
            let (laid_grid, grid_height) = layout_grid(
                grid,
                styles,
                fonts,
                &style,
                content_width,
                content_x,
                content_y,
                pos,
            );
            let height = resolve_used_height(&style, &padding, &border, content_width, grid_height);
            (LaidOutContent::Grid(laid_grid), height)
        }
        BoxContent::Flex(flex) => {
            // `display: table`と同様、flexコンテナは新しいフォーマッティング
            // コンテキストを確立する(`float`はflexアイテムに効果を持たない、
            // CSS仕様通り)ため、外側の`float_ctx`とは独立させる。
            let (items, flex_height) = layout_flex(
                flex,
                styles,
                fonts,
                &style,
                content_width,
                content_x,
                content_y,
                pos,
            );
            let height = resolve_used_height(&style, &padding, &border, content_width, flex_height);
            (LaidOutContent::Flex(items), height)
        }
        BoxContent::Image(image_content) => {
            // `apply_replaced_element_auto_size`が呼ばれた場合、widthが両方
            // autoだったケースは既に具体的なLengthへ差し替え済みなので、
            // `resolve_height`は`Some`を返す(高さゼロは、内在サイズが
            // 得られない=フェッチ・デコード失敗時の妥当な既定)。
            // `min-height`/`max-height`のクランプは他の内容種別と同じく
            // 効くが、アスペクト比の維持は行わない。
            let height = resolve_used_height(&style, &padding, &border, content_width, 0.0);
            (LaidOutContent::Image(image_content.image.clone()), height)
        }
    };
    // 子孫のレイアウトが終わったのでcontaining blockを復元する。
    pos.abs_cb = saved_cb;
    // taffyが確定した高さを最終レイアウトパスでそのまま反映する
    // (`layout_box_with_forced_size`専用)。
    let mut content_height = forced_content_height.unwrap_or(content_height);

    // 親子間・空ブロックのマージン相殺。`layout.margin`を実効(相殺後)値に、
    // `content.y`/`content_height`をそれに合わせて調整する。親子相殺は
    // `height: auto`のブロックのみ対象(既知の簡略化)。
    let height_is_auto =
        forced_content_height.is_none() && matches!(style.height, LengthPercentageOrAuto::Auto);
    apply_margin_collapse(
        &mut content,
        &mut content_height,
        &mut margin,
        &mut content_y,
        &border,
        &padding,
        height_is_auto,
    );

    // `position: relative`の視覚的オフセット。後続兄弟の`cursor_y`計算は
    // `margin_box_height`(座標に依存しない)を使うため、ここでcontent
    // 座標をずらしても後続要素のフローには影響しない。
    let (offset_x, offset_y) = if style.position == Position::Relative {
        resolve_relative_offset(&style, content_width)
    } else {
        (0.0, 0.0)
    };

    let marker = b.marker.as_deref().and_then(|text| {
        layout_list_marker(
            text,
            &style,
            fonts,
            content_x + offset_x,
            content_y + offset_y,
        )
        .map(Box::new)
    });

    LaidOutBox {
        node: b.node,
        layout: Layout {
            content: Rect {
                x: content_x + offset_x,
                y: content_y + offset_y,
                width: content_width,
                height: content_height,
            },
            padding,
            border,
            margin,
            fragment: FragmentPosition::Whole,
        },
        fragmentation: FragmentationHints::from(&style),
        has_visible_decoration: has_visible_decoration(&style, &border),
        is_float: style.float != Float::None,
        content,
        marker,
    }
}

/// `display: list-item`のマーカー(`list-style-position: outside`、または
/// ブロック子を持つため`inside`からフォールバックした場合)をレイアウトする。
/// マーカーはcontent boxの外側(左のgutter)に独立して配置するだけなので、
/// `b`の内容が`BoxContent::Inline`/`Blocks`のどちらでも同じロジックで扱える。
///
/// 実装は通常のテキストランと全く同じシェイピング(`shape_run`)を再利用し、
/// 結果を`runs`が1つだけの`LineBox`として返す。これにより描画側
/// (`pdf::document::render_line`)を一切変更せずに再利用できる。
fn layout_list_marker(
    text: &str,
    style: &ComputedStyle,
    fonts: &FontCollection,
    content_x: f32,
    content_y: f32,
) -> Option<LineBox> {
    let first_char = text.chars().next()?;
    let font_index = fonts.select_for_char(
        &style.font_family,
        style.font_weight,
        style.font_style,
        first_char,
    )?;
    let run = shape_run(text, font_index, fonts, style);
    let width = run.width;
    let height = run.line_height;
    Some(finish_line(
        vec![run],
        Vec::new(),
        width,
        content_x - LIST_MARKER_GAP - width,
        content_y,
        height,
        fonts,
    ))
}

/// float子要素を配置する。幅解決は`resolve_box_geometry`で(実際のレイアウトと)
/// 二重に行う——`float_ctx.place`が配置座標を決めるにはmargin box幅が先に
/// 必要なため([`<img>`]のような置換要素のauto-size解決も含めて正確な幅を
/// 得る必要があり、事前計算を省略できない)。
#[allow(clippy::too_many_arguments)]
fn layout_float_child(
    child: &LayoutBox,
    child_style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    float_ctx: &mut FloatContext,
    containing_left: f32,
    preferred_top: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    let (_, padding, border, margin, child_content_width) =
        resolve_box_geometry(child, styles, fonts, containing_width, None);
    let margin_box_width = margin.left
        + border.left
        + padding.left
        + child_content_width
        + padding.right
        + border.right
        + margin.right;

    let (float_x, float_y) = float_ctx.place(
        child_style.float,
        preferred_top,
        containing_left,
        containing_left + containing_width,
        margin_box_width,
    );

    let child_laid = layout_box(
        child,
        styles,
        fonts,
        containing_width,
        float_ctx,
        float_x,
        float_y,
        pos,
    );
    float_ctx.register(
        child_style.float,
        float_x,
        float_y,
        margin_box_width,
        child_laid.layout.margin_box_height(),
    );
    child_laid
}

/// `position: relative`のtop/right/bottom/leftから視覚的オフセット`(dx, dy)`を
/// 解決する。優先順位はCSS仕様通り`top` > `bottom`、`left` > `right`。
fn resolve_relative_offset(style: &ComputedStyle, containing_width: f32) -> (f32, f32) {
    let resolve =
        |primary: LengthPercentageOrAuto, secondary: LengthPercentageOrAuto, basis: f32| {
            match primary {
                LengthPercentageOrAuto::LengthPercentage(lp) => resolve_lp(lp, basis),
                LengthPercentageOrAuto::Auto => match secondary {
                    LengthPercentageOrAuto::LengthPercentage(lp) => -resolve_lp(lp, basis),
                    LengthPercentageOrAuto::Auto => 0.0,
                },
            }
        };
    let dx = resolve(style.left, style.right, containing_width);
    let dy = resolve(style.top, style.bottom, 0.0);
    (dx, dy)
}

/// `style`/`border`(計算済みの太さ)の組み合わせが、実際に何か描画するか。
/// 背景色があるか、4辺のいずれかで太さが正かつ`border-style`が`none`でない
/// 場合に`true`(`pdf::document::render_box_decoration`が実際に描画する
/// 条件と同じ)。
pub(crate) fn has_visible_decoration(style: &ComputedStyle, border: &EdgeSizes) -> bool {
    if style.background_color.alpha > 0.0 {
        return true;
    }
    // `background-image`のみを持つ要素(背景色・枠線なし)も、`place_split`が
    // 装飾フラグメント(`node`付きの`LaidOutBox`)を生成する対象に含めない
    // 限り`collect_image_uses`/`render_box`から参照できず描画されない。
    if style.background_image.is_some() {
        return true;
    }
    [
        (border.top, style.border_top_style),
        (border.right, style.border_right_style),
        (border.bottom, style.border_bottom_style),
        (border.left, style.border_left_style),
    ]
    .into_iter()
    .any(|(width, border_style)| width > 0.0 && border_style != BorderStyle::None)
}

/// ボックスの計算済みスタイル。
///
/// 実要素のスタイルは`styles`が持つものをそのまま借りる。`ComputedStyle`は
/// 1KBを超えるうえ`font_family: Vec<String>`を持つため、複製するとレイアウト
/// のたびにヒープ確保が積み上がる(表のセルでは1セルあたり3回呼ばれる)。
/// 書き換えたい呼び出し側だけが`into_owned`で複製する。
pub(super) fn box_style<'a>(
    b: &LayoutBox,
    styles: &'a HashMap<NodeId, Rc<ComputedStyle>>,
) -> Cow<'a, ComputedStyle> {
    match b.node {
        Some(node) => Cow::Borrowed(&styles[&node]),
        // 無名ボックス(CSS2.1 9.2.1.1)。マージン/パディング/枠線を持たないblock。
        None => Cow::Owned(ComputedStyle {
            display: Display::Block,
            ..ComputedStyle::default()
        }),
    }
}

pub(super) fn resolve_lp(lp: LengthPercentage, basis: f32) -> f32 {
    match lp {
        LengthPercentage::Length(px) => px,
        LengthPercentage::Percentage(fraction) => fraction * basis,
        LengthPercentage::Calc { px, percent } => px + percent * basis,
    }
}

pub(crate) fn resolve_lpa_or_zero(lpa: LengthPercentageOrAuto, basis: f32) -> f32 {
    match lpa {
        LengthPercentageOrAuto::Auto => 0.0,
        LengthPercentageOrAuto::LengthPercentage(lp) => resolve_lp(lp, basis),
    }
}

/// `min-width`/`max-width`による使用幅のクランプ。
///
/// `max`→`min`の順に適用するので、`min-width > max-width`のときは`min-width`が
/// 勝つ(CSS2.1 §10.4の手順と同じ結果)。`box-sizing: border-box`の場合、
/// `min-*`/`max-*`の指定値もborder-box基準なので、`width`と同じく
/// padding+borderを引いてcontent-box相当へ変換してから比較する。
pub(crate) fn clamp_used_width(
    style: &ComputedStyle,
    containing_width: f32,
    padding_lr: f32,
    border_lr: f32,
    width: f32,
) -> f32 {
    let to_content_box = |v: f32| {
        if style.box_sizing == BoxSizing::BorderBox {
            (v - padding_lr - border_lr).max(0.0)
        } else {
            v
        }
    };

    let mut used = width;
    if let MaxSize::LengthPercentage(lp) = style.max_width {
        used = used.min(to_content_box(resolve_lp(lp, containing_width)));
    }
    used.max(to_content_box(resolve_lp(
        style.min_width,
        containing_width,
    )))
    .max(0.0)
}

/// `min-height`/`max-height`による使用高さのクランプ。パーセンテージ指定は
/// containing blockの高さが不定なため無視する(`height`のパーセンテージが
/// 無視されるのと同じ扱い)。
pub(crate) fn clamp_used_height(
    style: &ComputedStyle,
    padding_tb: f32,
    border_tb: f32,
    height: f32,
) -> f32 {
    let to_content_box = |v: f32| {
        if style.box_sizing == BoxSizing::BorderBox {
            (v - padding_tb - border_tb).max(0.0)
        } else {
            v
        }
    };

    let mut used = height;
    if let MaxSize::LengthPercentage(lp) = style.max_height {
        if let Some(px) = definite_height_px(lp) {
            used = used.min(to_content_box(px));
        }
    }
    if let Some(px) = definite_height_px(style.min_height) {
        used = used.max(to_content_box(px));
    }
    used.max(0.0)
}

/// 高さ方向で使える絶対長(px)。パーセンテージ、およびパーセンテージ成分を持つ
/// `calc`は、containing blockの高さが不定なため`None`(=無視)を返す。
fn definite_height_px(lp: LengthPercentage) -> Option<f32> {
    match lp {
        LengthPercentage::Length(px) => Some(px),
        LengthPercentage::Percentage(_) => None,
        LengthPercentage::Calc { px, percent: 0.0 } => Some(px),
        LengthPercentage::Calc { .. } => None,
    }
}

pub(crate) fn resolve_padding(style: &ComputedStyle, basis: f32) -> EdgeSizes {
    EdgeSizes {
        top: resolve_lp(style.padding_top, basis),
        right: resolve_lp(style.padding_right, basis),
        bottom: resolve_lp(style.padding_bottom, basis),
        left: resolve_lp(style.padding_left, basis),
    }
}

/// `border-style: none`の辺は、`border-width`の指定に関わらず使用値が`0`になる
/// (CSS2.1 8.5.3)。レイアウト(幅計算)にもこの丸めが反映される必要がある。
pub(crate) fn resolve_border(style: &ComputedStyle) -> EdgeSizes {
    let width_or_zero = |width: Length, border_style: BorderStyle| {
        if border_style == BorderStyle::None {
            0.0
        } else {
            width.0
        }
    };
    EdgeSizes {
        top: width_or_zero(style.border_top_width, style.border_top_style),
        right: width_or_zero(style.border_right_width, style.border_right_style),
        bottom: width_or_zero(style.border_bottom_width, style.border_bottom_style),
        left: width_or_zero(style.border_left_width, style.border_left_style),
    }
}

/// 使用高さ = 「明示`height` → `aspect-ratio`による導出 →
/// `auto_height`(内容から求めた高さ)」の優先順で決めた値を、
/// `min-height`/`max-height`でクランプしたもの。
pub(crate) fn resolve_used_height(
    style: &ComputedStyle,
    padding: &EdgeSizes,
    border: &EdgeSizes,
    content_width: f32,
    auto_height: f32,
) -> f32 {
    let padding_tb = padding.top + padding.bottom;
    let border_tb = border.top + border.bottom;
    let height = resolve_height(style, padding_tb, border_tb)
        .or_else(|| aspect_ratio_height(style, padding, border, content_width))
        .unwrap_or(auto_height);
    clamp_used_height(style, padding_tb, border_tb, height)
}

/// `aspect-ratio`から導出したcontent高さ。比が無ければ`None`。比が適用される
/// 箱は`box-sizing`に従う。
fn aspect_ratio_height(
    style: &ComputedStyle,
    padding: &EdgeSizes,
    border: &EdgeSizes,
    content_width: f32,
) -> Option<f32> {
    let ratio = style.aspect_ratio.ratio?;
    if style.box_sizing == BoxSizing::BorderBox {
        let border_box_width =
            content_width + padding.left + padding.right + border.left + border.right;
        Some(
            (border_box_width / ratio - padding.top - padding.bottom - border.top - border.bottom)
                .max(0.0),
        )
    } else {
        Some(content_width / ratio)
    }
}

/// `aspect-ratio`から導出したcontent幅。高さが確定していて`width: auto`の
/// shrink-to-fit文脈(float / `inline-block` / 絶対配置 / `<img>`)で使う。通常
/// フローのブロックの`width: auto`はstretchが優先されるため呼ばない。
pub(crate) fn aspect_ratio_width(
    style: &ComputedStyle,
    padding: &EdgeSizes,
    border: &EdgeSizes,
) -> Option<f32> {
    let ratio = style.aspect_ratio.ratio?;
    let padding_tb = padding.top + padding.bottom;
    let border_tb = border.top + border.bottom;
    let content_height = resolve_height(style, padding_tb, border_tb)?;
    if style.box_sizing == BoxSizing::BorderBox {
        let border_box_height = content_height + padding_tb + border_tb;
        Some(
            (border_box_height * ratio - padding.left - padding.right - border.left - border.right)
                .max(0.0),
        )
    } else {
        Some(content_height * ratio)
    }
}

/// `height`が明示指定されていれば返す。`auto`および(containing blockの高さが
/// 不定なため)パーセンテージ指定は`None`とし、呼び出し側でコンテンツ高さを使う。
/// `box-sizing: border-box`の場合、指定値は border-box の高さを表すため
/// `padding_tb`/`border_tb`を引いてcontent-box相当に変換する。
fn resolve_height(style: &ComputedStyle, padding_tb: f32, border_tb: f32) -> Option<f32> {
    let LengthPercentageOrAuto::LengthPercentage(lp) = style.height else {
        return None;
    };
    let px = definite_height_px(lp)?;
    Some(if style.box_sizing == BoxSizing::BorderBox {
        (px - padding_tb - border_tb).max(0.0)
    } else {
        px
    })
}

/// 置換要素(`<img>`)のwidth/heightが両方`auto`の場合に限り、CSS2.2
/// §10.3.2/§10.6.2の簡略版(置換要素の内在サイズに基づく解決)を適用する:
/// HTML属性(`width`/`height`)→内在サイズ(デコード成功時)の優先順で決め、
/// 一方だけ値が得られる場合はアスペクト比を保って他方を導出する。
///
/// CSSで`width`/`height`のどちらか一方だけが明示指定されている場合は、使用比
/// (`aspect-ratio`指定、無ければ内在比)からもう一方を導出する。「幅が確定
/// &`height: auto`」は下流の[`resolve_used_height`]が
/// 導出するため、ここでは何もしない。
pub(super) fn apply_replaced_element_auto_size(
    style: &mut ComputedStyle,
    image: &ImageBoxContent,
    containing_width: f32,
) {
    // 内在比を計算スタイルへ焼き込む。以降の一般ロジックは
    // 「`style.aspect_ratio.ratio`があれば使う」だけでよくなる。
    if style.aspect_ratio.auto {
        if let Some(ratio) = intrinsic_ratio(image) {
            style.aspect_ratio.ratio = Some(ratio);
        }
    }

    let width_is_auto = matches!(style.width, LengthPercentageOrAuto::Auto);
    let height_is_auto = matches!(style.height, LengthPercentageOrAuto::Auto);

    let padding = resolve_padding(style, containing_width);
    let border = resolve_border(style);

    if !width_is_auto {
        return;
    }
    if !height_is_auto {
        // 高さ確定&`width: auto`。置換要素の`width: auto`はshrink-to-fit
        // (通常のブロックのstretchではない)ので、比から幅を導ける。
        if let Some(width) = aspect_ratio_width(style, &padding, &border) {
            style.width = LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(width));
        }
        return;
    }

    // 両方`auto`: HTML属性(`width`/`height`)→内在サイズ(デコード成功時)の
    // 優先順で決め、一方だけ値が得られる場合はアスペクト比を保って他方を導出する
    // (CSS2.2 §10.3.2/§10.6.2の簡略版)。
    let attr_size = (
        image.attr_width.map(|w| w as f32),
        image.attr_height.map(|h| h as f32),
    );
    let intrinsic_size = image
        .image
        .as_ref()
        .map(|prepared| (prepared.width, prepared.height));

    let (width, height) = match attr_size {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (
            w,
            derive_via_aspect_ratio(w, intrinsic_size.map(|(iw, ih)| (ih, iw))),
        ),
        (None, Some(h)) => (derive_via_aspect_ratio(h, intrinsic_size), h),
        (None, None) => intrinsic_size.unwrap_or((0.0, 0.0)),
    };

    style.width = LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(width));

    // `auto`なしの`aspect-ratio`指定(例: `aspect-ratio: 16 / 9`)は内在比より
    // 優先する。幅は内在幅のまま、高さだけ比で決め直す。
    let height = if style.aspect_ratio.auto {
        height
    } else {
        aspect_ratio_height(style, &padding, &border, width).unwrap_or(height)
    };
    style.height = LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(height));
}

/// 画像の内在アスペクト比(`width / height`)。デコードできていない、または
/// 高さが0の場合は`None`。
fn intrinsic_ratio(image: &ImageBoxContent) -> Option<f32> {
    let prepared = image.image.as_ref()?;
    (prepared.height > 0.0).then(|| prepared.width / prepared.height)
}

/// `known`(既知の1辺の長さ)から、`ratio_basis`(`(既知でない辺の内在長,
/// 既知の辺の内在長)`)を使ってアスペクト比を保った他方の辺を導出する。
/// 内在サイズが無い(デコード失敗)、または既知の辺の内在長が0の場合は0を返す
/// (呼び出し側で「サイズ不明」の意味になる)。
fn derive_via_aspect_ratio(known: f32, ratio_basis: Option<(f32, f32)>) -> f32 {
    match ratio_basis {
        Some((other_intrinsic, known_intrinsic)) if known_intrinsic > 0.0 => {
            known * other_intrinsic / known_intrinsic
        }
        _ => 0.0,
    }
}

/// 親子間・空ブロックのマージン相殺を適用する。
///
/// `margin`を実効(相殺後)値へ、`content.y`と`content_height`をそれに合わせて
/// 調整する。呼び出し側は、返された実効`margin`をそのまま`LaidOutBox`へ格納する
/// ことで、祖先の隣接兄弟相殺ループが多階層の相殺へ自然につながる。
fn apply_margin_collapse(
    content: &mut LaidOutContent,
    content_height: &mut f32,
    margin: &mut EdgeSizes,
    content_y: &mut f32,
    border: &EdgeSizes,
    padding: &EdgeSizes,
    height_is_auto: bool,
) {
    // 空ブロック: 高さ0・border/padding無し・子が空。自身の上下マージンを
    // 1つに相殺する(`margin_box_height`が相殺値1つ分になるよう上へ寄せる)。
    // 上の兄弟とはこの相殺値で相殺され、二重マージンを防ぐ。
    let content_is_empty = match content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => children.is_empty(),
        // 子を持たない`<div></div>`は`Inline`(空)になる。
        LaidOutContent::Inline(lines) => lines.is_empty(),
        LaidOutContent::Grid(grid) => grid.rows.iter().all(|row| row.items.is_empty()),
        LaidOutContent::Table(_) | LaidOutContent::Image(_) => false,
    };
    let is_empty_block = *content_height == 0.0
        && border.top == 0.0
        && border.bottom == 0.0
        && padding.top == 0.0
        && padding.bottom == 0.0
        && content_is_empty;
    if is_empty_block {
        let collapsed = collapse_adjacent_margins(margin.top, margin.bottom);
        margin.top = collapsed;
        margin.bottom = 0.0;
        return;
    }

    let LaidOutContent::Blocks(children) = content else {
        return;
    };

    // 親と最初の子: 親に上境界(border-top/padding-top)が無ければ、最初の非
    // float子の実効`margin-top`を親の外へ持ち上げて相殺する。
    if border.top == 0.0 && padding.top == 0.0 && height_is_auto {
        if let Some(first_top) = children
            .iter()
            .find(|c| !c.is_float)
            .map(|c| c.layout.margin.top)
        {
            let effective = collapse_adjacent_margins(margin.top, first_top);
            // 子はすべて、最初の子の`margin-top`が親contentから抜けた分だけ
            // 上へ動く。delta <= 0。
            let child_delta = effective - first_top - margin.top;
            for child in children.iter_mut() {
                shift_box_y_in_place(child, -child_delta);
            }
            *content_y += effective - margin.top;
            *content_height -= first_top;
            margin.top = effective;
        }
    }

    // 親と最後の子: 親に下境界(border-bottom/padding-bottom/明示height)が
    // 無ければ、最後の非float子の`margin-bottom`を
    // 親の外へ持ち上げて相殺する。
    if border.bottom == 0.0 && padding.bottom == 0.0 && height_is_auto {
        if let Some(last_bottom) = children
            .iter()
            .rev()
            .find(|c| !c.is_float)
            .map(|c| c.layout.margin.bottom)
        {
            let effective = collapse_adjacent_margins(margin.bottom, last_bottom);
            // 最後の子のmargin-bottomが親contentから抜けるので、その分縮める。
            *content_height -= last_bottom;
            margin.bottom = effective;
        }
    }
}

/// 2つの隣接するマージンを相殺(collapse)した結果の間隔を求める(CSS2.1 §8.3.1)。
/// 両方が非負なら大きい方、両方が負なら小さい方(絶対値が大きい方)、
/// 正負混在なら両者の単純な和(=正の最大値と負の最小値の和)になる。
fn collapse_adjacent_margins(a: f32, b: f32) -> f32 {
    let positive = a.max(0.0).max(b.max(0.0));
    let negative = a.min(0.0).min(b.min(0.0));
    positive + negative
}

/// CSS2.1 §10.3.3(block-level, non-replaced要素)の簡略版。
/// `margin-left + border-left + padding-left + width + padding-right + border-right + margin-right
/// = containing blockの幅`という制約から、`auto`な項目を埋める。
pub(crate) fn resolve_width_and_horizontal_margins(
    style: &ComputedStyle,
    containing_width: f32,
    padding_lr: f32,
    border_lr: f32,
) -> (f32, f32, f32) {
    let (width, margin_left, margin_right) =
        solve_horizontal(style, containing_width, padding_lr, border_lr, None);

    // `min-width`/`max-width`でクランプし、値が変わったら「その幅が明示指定
    // されていた」ものとして水平方向の等式を解き直す(CSS2.1 §10.4)。
    // こうしないと`width: auto; max-width: 600px; margin: 0 auto`のような
    // 指定で、auto幅の枝で0に潰れたmargin
    // autoがそのまま残り中央寄せされない。
    let clamped = clamp_used_width(style, containing_width, padding_lr, border_lr, width);
    if clamped == width {
        return (width, margin_left, margin_right);
    }
    solve_horizontal(
        style,
        containing_width,
        padding_lr,
        border_lr,
        Some(clamped),
    )
}

/// CSS2.1 §10.3.3の水平方向の等式(margin-left + border + padding + width +
/// margin-right = containing width)を解く。`used_width`に`Some`を渡すと、
/// その値(content-box基準、変換済み)が明示指定された`width`として扱われる
/// (min/max幅のクランプ後の解き直し用)。
fn solve_horizontal(
    style: &ComputedStyle,
    containing_width: f32,
    padding_lr: f32,
    border_lr: f32,
    used_width: Option<f32>,
) -> (f32, f32, f32) {
    let margin_left_is_auto = matches!(style.margin_left, LengthPercentageOrAuto::Auto);
    let margin_right_is_auto = matches!(style.margin_right, LengthPercentageOrAuto::Auto);

    let specified_width = match used_width {
        Some(w) => Some(w),
        None if matches!(style.width, LengthPercentageOrAuto::Auto) => None,
        None => {
            // `box-sizing: border-box`の場合、指定値はborder-boxの幅を
            // 表すため、padding+borderを引いてcontent-box
            // 相当に変換してから既存の等式へ渡す。
            let width = resolve_lpa_or_zero(style.width, containing_width);
            Some(if style.box_sizing == BoxSizing::BorderBox {
                (width - padding_lr - border_lr).max(0.0)
            } else {
                width
            })
        }
    };

    let Some(width) = specified_width else {
        let margin_left = resolve_lpa_or_zero(style.margin_left, containing_width);
        let margin_right = resolve_lpa_or_zero(style.margin_right, containing_width);
        let width =
            (containing_width - margin_left - border_lr - padding_lr - margin_right).max(0.0);
        return (width, margin_left, margin_right);
    };

    let remaining = (containing_width - border_lr - padding_lr - width).max(0.0);

    match (margin_left_is_auto, margin_right_is_auto) {
        (true, true) => {
            let half = remaining / 2.0;
            (width, half, half)
        }
        (true, false) => {
            let margin_right = resolve_lpa_or_zero(style.margin_right, containing_width);
            (width, (remaining - margin_right).max(0.0), margin_right)
        }
        (false, true) => {
            let margin_left = resolve_lpa_or_zero(style.margin_left, containing_width);
            (width, margin_left, (remaining - margin_left).max(0.0))
        }
        (false, false) => {
            // over-constrained(CSS2.1 §10.3.3): width/margin-left/margin-rightが
            // 全て明示指定されている場合、指定されたmargin-rightの値は無視し、
            // 等式(margin-left + border/padding + width + margin-right =
            // containing width)がちょうど成り立つよう使用値を再計算する
            // (負の値になることもある。`direction: rtl`時はmargin-left側を
            // 再計算すべきだが、rtl自体が未対応のため常にltr前提)。
            let margin_left = resolve_lpa_or_zero(style.margin_left, containing_width);
            let margin_right = containing_width - border_lr - padding_lr - width - margin_left;
            (width, margin_left, margin_right)
        }
    }
}

/// `b`の部分木全体のY座標を`delta`だけ平行移動した複製を返す。`paginate.rs`が
/// 1ページ全体の連続座標からページ内相対座標への変換に使う(`delta`を引く)。
/// `table.rs`がcaptionを`caption-side: bottom`で配置する際にも使う(`delta`に
/// 負の値を渡すことで下方向に移動する)。
/// 各行のアトミックインラインボックス(`display: inline-block`)を、行の
/// 確定した位置(`line.rect`とベースライン)に合わせて移動する。
///
/// `layout::inline`は行の縦位置が決まる前に中身をレイアウトする(原点0,0)ため、
/// ここでまとめて平行移動する。縦は「マージンボックスの下端がベースラインに
/// 乗る」ように置く。
fn place_atomic_inlines(lines: &mut [LineBox]) {
    for line in lines.iter_mut() {
        let baseline_y = line.rect.y + line.baseline;
        for atomic in line.atomics.iter_mut() {
            // マージンボックス左上の目標位置。
            let target_x = line.rect.x + atomic.x_offset;
            let target_y = baseline_y - atomic.baseline_shift - atomic.margin_box_height;
            // 現在のマージンボックス左上(原点0でレイアウトしてあるので、
            // content座標からmargin/border/paddingを引けば求まる)。
            let layout = atomic.content.layout;
            let current_x =
                layout.content.x - layout.padding.left - layout.border.left + -layout.margin.left;
            let current_y =
                layout.content.y - layout.padding.top - layout.border.top - layout.margin.top;
            // `shift_box_y`の`delta`は引く量(`shift_rect_y`が`y -= delta`)
            // である点に注意。`shift_box_x`は逆に足す量。
            shift_box_y_in_place(&mut atomic.content, current_y - target_y);
            shift_box_x_in_place(&mut atomic.content, target_x - current_x);
        }
    }
}

/// [`shift_box_y`]のx方向版(アトミックインラインボックスの水平配置に使う)。
pub(super) fn shift_box_x(b: &LaidOutBox, delta: f32) -> LaidOutBox {
    let mut shifted = b.clone();
    shift_box_x_in_place(&mut shifted, delta);
    shifted
}

/// [`shift_box_x`]のその場書き換え版([`shift_box_y_in_place`]と同じ理由)。
pub(super) fn shift_box_x_in_place(b: &mut LaidOutBox, delta: f32) {
    b.layout.content.x += delta;
    if let Some(marker) = &mut b.marker {
        marker.rect.x += delta;
    }

    match &mut b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children.iter_mut() {
                shift_box_x_in_place(child, delta);
            }
        }
        LaidOutContent::Grid(grid) => {
            for row in grid.rows.iter_mut() {
                for item in row.items.iter_mut() {
                    shift_box_x_in_place(item, delta);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines.iter_mut() {
                line.rect.x += delta;
                for atomic in line.atomics.iter_mut() {
                    shift_box_x_in_place(&mut atomic.content, delta);
                }
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &mut table.caption {
                shift_box_x_in_place(caption, delta);
            }
            for row in table.rows.iter_mut() {
                for cell in row.cells.iter_mut() {
                    shift_box_x_in_place(cell, delta);
                }
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

pub(super) fn shift_box_y(b: &LaidOutBox, delta: f32) -> LaidOutBox {
    let mut shifted = b.clone();
    shift_box_y_in_place(&mut shifted, delta);
    shifted
}

/// [`shift_box_y`]のその場書き換え版。
///
/// 再帰の各段で部分木を複製し直すと、深さぶん同じデータを作り直すことになり、
/// 時間もピークメモリも無駄に膨らむ。移動は借用したまま行えるので、複製は
/// 呼び出し側が必要なときだけ1回行う。
pub(super) fn shift_box_y_in_place(b: &mut LaidOutBox, delta: f32) {
    shift_rect_y(&mut b.layout.content, delta);
    if let Some(marker) = &mut b.marker {
        shift_rect_y(&mut marker.rect, delta);
    }

    match &mut b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children.iter_mut() {
                shift_box_y_in_place(child, delta);
            }
        }
        LaidOutContent::Grid(grid) => {
            for row in grid.rows.iter_mut() {
                // 行帯の上端/下端はアイテムと同じ座標空間にあるので、
                // `shift_rect_y`と同じ「`delta`を引く」向きで動かす。
                row.top -= delta;
                row.bottom -= delta;
                for item in row.items.iter_mut() {
                    shift_box_y_in_place(item, delta);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines.iter_mut() {
                shift_rect_y(&mut line.rect, delta);
                // 行内のアトミックボックスも行と一緒に動かす。
                for atomic in line.atomics.iter_mut() {
                    shift_box_y_in_place(&mut atomic.content, delta);
                }
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &mut table.caption {
                shift_box_y_in_place(caption, delta);
            }
            for row in table.rows.iter_mut() {
                for cell in row.cells.iter_mut() {
                    shift_box_y_in_place(cell, delta);
                }
            }
        }
        // `b.layout.content`の平行移動(この関数冒頭)だけで十分。画像は
        // `Inline`の行のような、それ自身が別途Rectを持つ子要素を持たない。
        LaidOutContent::Image(_) => {}
    }
}

fn shift_rect_y(rect: &mut Rect, delta: f32) {
    rect.y -= delta;
}

/// `b`自身の位置(`b.layout`)は変えず、その内容(子ボックス/行/テーブルの
/// 行・セル)だけを縦にシフトする。`shift_box_y`(自身含めた全体を平行移動)
/// とは別物として明確に区別する: テーブルセルの`vertical-align`実装では、
/// セル自身の高さ・位置は行の高さ均等化で既に確定済みで変えたくないが、
/// その内側の内容だけをtop/middle/bottom/baselineに応じて上下させたい。
pub(super) fn shift_content_vertical(b: &LaidOutBox, delta: f32) -> LaidOutBox {
    let mut b = b.clone();

    match &mut b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children.iter_mut() {
                *child = shift_box_y(child, delta);
            }
        }
        LaidOutContent::Grid(grid) => {
            for row in grid.rows.iter_mut() {
                row.top -= delta;
                row.bottom -= delta;
                for item in row.items.iter_mut() {
                    *item = shift_box_y(item, delta);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines.iter_mut() {
                shift_rect_y(&mut line.rect, delta);
                // 行内のアトミックボックスも行と一緒に動かす。
                for atomic in line.atomics.iter_mut() {
                    atomic.content = shift_box_y(&atomic.content, delta);
                }
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &mut table.caption {
                **caption = shift_box_y(caption, delta);
            }
            for row in table.rows.iter_mut() {
                for cell in row.cells.iter_mut() {
                    *cell = shift_box_y(cell, delta);
                }
            }
        }
        // `Image`は`Inline`の行のような、それ自身が別途Rectを持つ子要素を
        // 持たないため、動かす対象が無い(セルにネストした画像の
        // `vertical-align`は、セル内容全体を1つのブロックとして動かす形に
        // 委ねる)。
        LaidOutContent::Image(_) => {}
    }

    b
}

#[cfg(test)]
mod tests {
    use super::super::box_tree::build_box_tree;
    use super::*;
    use crate::fonts::Font;
    use crate::html::{self, Dom, NodeData};
    use crate::pdf::{ImagePlane, PlaneColorSpace};
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

    fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
        if b.node == Some(target) {
            return Some(b);
        }
        if let LaidOutContent::Blocks(children) = &b.content {
            for child in children {
                if let Some(found) = find_laid_out(child, target) {
                    return Some(found);
                }
            }
        }
        None
    }

    #[test]
    fn display_none_excludes_element_and_subtree() {
        let dom = html::parse(
            br#"<div><p class="hidden">hidden</p><p class="visible">visible</p></div>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".hidden { display: none; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let (hidden_p, visible_p) = (ps[0], ps[1]);

        assert!(find_box(&tree, hidden_p).is_none());
        assert!(find_box(&tree, visible_p).is_some());
    }

    #[test]
    fn mixed_block_and_inline_children_get_anonymous_block_wrapping() {
        let dom = html::parse(br#"<div class="outer">before <p>P</p> after</div>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);

        let div_box = find_box(&tree, divs[0]).expect("div box not found");
        let BoxContent::Blocks(children) = &div_box.content else {
            panic!("expected block container")
        };
        assert_eq!(children.len(), 3, "before-text / <p> / after-text");
        let joined_text = |content: &BoxContent| match content {
            BoxContent::Inline(spans) => spans.iter().map(|s| s.text.as_str()).collect::<String>(),
            BoxContent::Blocks(_)
            | BoxContent::Table(_)
            | BoxContent::Flex(_)
            | BoxContent::Grid(_)
            | BoxContent::Image(_) => {
                panic!("expected inline content")
            }
        };
        assert_eq!(joined_text(&children[0].content).trim(), "before");
        assert_eq!(children[1].node, Some(ps[0]));
        assert_eq!(joined_text(&children[2].content).trim(), "after");
    }

    #[test]
    fn auto_width_fills_containing_block_minus_margins() {
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".box { margin: 10px; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        // html: margin/padding/borderなし → content_width=800
        // body: UAデフォルトのmargin:8px → content_width=784
        // div: margin:10px → content_width=764
        assert_eq!(div_box.layout.margin.left, 10.0);
        assert_eq!(div_box.layout.content.width, 764.0);
        assert_eq!(div_box.layout.content.x, 18.0);
    }

    #[test]
    fn auto_margins_center_element_with_explicit_width() {
        let dom = html::parse(br#"<div class="centered"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".centered { width: 400px; margin: 0 auto; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(div_box.layout.content.width, 400.0);
        assert_eq!(div_box.layout.margin.left, div_box.layout.margin.right);
        assert_eq!(div_box.layout.margin.left, 192.0);
    }

    #[test]
    fn over_constrained_box_recalculates_margin_right_to_fit_the_containing_block() {
        // width/margin-left/margin-rightが全て明示指定され、かつ合計が
        // containing widthと一致しない(over-constrained)場合、CSS2.1 §10.3.3
        // に従い指定されたmargin-rightは無視され、等式が成り立つよう再計算される。
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        // containing width = 784(html:800, body margin:8pxずつ)。
        // width:300 + margin-left:50 + 指定margin-right:50 = 400 だが、
        // 784になるようmargin-rightは434に再計算されるはず。
        let author = parse_stylesheet(".box { width: 300px; margin: 0 50px 0 50px; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(div_box.layout.content.width, 300.0);
        assert_eq!(div_box.layout.margin.left, 50.0);
        assert_eq!(
            div_box.layout.margin.right, 434.0,
            "over-constrained margin-right should be recalculated, not the specified 50px"
        );
    }

    #[test]
    fn over_constrained_recalculation_can_produce_a_negative_margin_right() {
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        // containing width = 784。width自体がそれを埋め尽くすので、margin-leftの
        // 分だけ超過し、再計算後のmargin-rightは指定値(99px)と符号すら異なる
        // 負の値になるはず。
        let author = parse_stylesheet(".box { width: 784px; margin: 0 99px 0 30px; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(div_box.layout.margin.right, -30.0);
    }

    #[test]
    fn block_siblings_stack_vertically_by_content_height() {
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        let author =
            parse_stylesheet(".a { height: 50px; margin: 0; } .b { height: 30px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let a = find_laid_out(&laid, ps[0]).expect("p.a not found");
        let b = find_laid_out(&laid, ps[1]).expect("p.b not found");

        assert_eq!(
            b.layout.content.y,
            a.layout.content.y + a.layout.content.height
        );
    }

    #[test]
    fn equal_adjacent_margins_collapse_to_a_single_gap_instead_of_summing() {
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        // 両方とも上下16pxのマージン。相殺されていれば、border-box間の隙間は
        // 32px(単純な加算)ではなく16pxになるはず。
        let author = parse_stylesheet(
            ".a { height: 20px; margin: 16px 0; } .b { height: 20px; margin: 16px 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let a = find_laid_out(&laid, ps[0]).expect("p.a not found");
        let b = find_laid_out(&laid, ps[1]).expect("p.b not found");

        let gap =
            b.layout.border_box().y - (a.layout.border_box().y + a.layout.border_box().height);
        assert_eq!(
            gap, 16.0,
            "equal adjacent margins should collapse to their shared value"
        );
    }

    #[test]
    fn left_float_is_removed_from_normal_flow_and_placed_at_containing_left() {
        let dom = html::parse(
            br#"<div class="outer"><div class="f">F</div><div class="after">after</div></div>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .f { float: left; width: 100px; height: 50px; } \
             .after { height: 20px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let float_box = find_laid_out(&laid, divs[1]).expect("float box not found");
        let after_box = find_laid_out(&laid, divs[2]).expect("after box not found");

        assert!(float_box.is_float);
        assert_eq!(float_box.layout.content.x, 0.0);
        assert_eq!(float_box.layout.content.y, 0.0);
        // floatはフローに参加しないため、後続のブロックはfloatの高さ(50px)を
        // 無視してcontaining blockの先頭からすぐ配置される。
        assert_eq!(after_box.layout.content.y, 0.0);
    }

    #[test]
    fn right_float_is_placed_against_the_containing_right_edge() {
        let dom = html::parse(br#"<div class="outer"><div class="f">F</div></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } .f { float: right; width: 100px; height: 50px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let float_box = find_laid_out(&laid, divs[1]).expect("float box not found");

        assert_eq!(float_box.layout.content.x, 700.0);
        assert_eq!(float_box.layout.content.y, 0.0);
    }

    #[test]
    fn second_left_float_packs_next_to_the_first_instead_of_stacking() {
        let dom = html::parse(
            br#"<div class="outer"><div class="a">A</div><div class="b">B</div></div>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .a { float: left; width: 100px; height: 50px; } \
             .b { float: left; width: 100px; height: 30px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let a_box = find_laid_out(&laid, divs[1]).expect("a not found");
        let b_box = find_laid_out(&laid, divs[2]).expect("b not found");

        assert_eq!(a_box.layout.content.x, 0.0);
        assert_eq!(b_box.layout.content.x, 100.0);
        assert_eq!(b_box.layout.content.y, 0.0);
    }

    #[test]
    fn clear_pushes_the_element_below_the_float() {
        let dom = html::parse(
            br#"<div class="outer"><div class="f">F</div><div class="c">after</div></div>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .f { float: left; width: 100px; height: 50px; } \
             .c { clear: left; height: 20px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let cleared_box = find_laid_out(&laid, divs[2]).expect("cleared box not found");

        assert_eq!(cleared_box.layout.content.y, 50.0);
    }

    #[test]
    fn float_does_not_participate_in_adjacent_margin_collapsing() {
        let dom = html::parse(
            br#"<div class="outer">
                <div class="a">a</div><div class="f">F</div><div class="b">b</div>
                </div>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .a { height: 10px; margin: 0 0 20px 0; } \
             .f { float: left; width: 30px; height: 5px; } \
             .b { height: 10px; margin: 30px 0 0 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let a_box = find_laid_out(&laid, divs[1]).expect("a not found");
        let b_box = find_laid_out(&laid, divs[3]).expect("b not found");

        assert_eq!(a_box.layout.content.y, 0.0);
        // aとbの間にfloatを挟んでいても、直前の非float子(a)とのマージン相殺が
        // そのまま働く: max(20, 30) = 30。floatをマージン相殺の対象に含めて
        // しまうと(floatはmarginを持たないため0とみなされ)この値がずれる。
        assert_eq!(b_box.layout.content.y, 40.0);
    }

    #[test]
    fn container_auto_height_expands_to_include_a_taller_float_child() {
        let dom = html::parse(br#"<div class="outer"><div class="f">F</div></div>"#);
        let ua = user_agent_stylesheet();
        let author =
            parse_stylesheet("body { margin: 0; } .f { float: left; width: 50px; height: 200px; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let outer_box = find_laid_out(&laid, divs[0]).expect("outer not found");

        assert_eq!(outer_box.layout.content.height, 200.0);
    }

    #[test]
    fn position_relative_offsets_visual_position_without_affecting_siblings() {
        let dom = html::parse(
            br#"<div class="outer">
                <div class="a">a</div><div class="rel">b</div><div class="c">c</div>
                </div>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "body { margin: 0; } \
             .a { height: 10px; } \
             .rel { position: relative; top: 5px; left: 7px; height: 20px; } \
             .c { height: 10px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let rel_box = find_laid_out(&laid, divs[2]).expect("rel not found");
        let c_box = find_laid_out(&laid, divs[3]).expect("c not found");

        // 通常位置はx=0, y=10(aの下)だが、top:5px/left:7pxのオフセットが加わる。
        assert_eq!(rel_box.layout.content.x, 7.0);
        assert_eq!(rel_box.layout.content.y, 15.0);
        // cはrel要素本来の(オフセット前の)下端(10+20=30)を基準に配置され、
        // 視覚的オフセットの影響を受けない。
        assert_eq!(c_box.layout.content.y, 30.0);
    }

    #[test]
    fn unequal_adjacent_margins_collapse_to_the_larger_one() {
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".a { height: 20px; margin: 0 0 10px 0; } .b { height: 20px; margin: 24px 0 0 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let a = find_laid_out(&laid, ps[0]).expect("p.a not found");
        let b = find_laid_out(&laid, ps[1]).expect("p.b not found");

        let gap =
            b.layout.border_box().y - (a.layout.border_box().y + a.layout.border_box().height);
        assert_eq!(
            gap, 24.0,
            "collapsed gap should be the larger of the two margins"
        );
    }

    #[test]
    fn a_negative_margin_reduces_the_collapsed_gap() {
        let dom = html::parse(br#"<div><p class="a">A</p><p class="b">B</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".a { height: 20px; margin: 0 0 10px 0; } .b { height: 20px; margin: -4px 0 0 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let a = find_laid_out(&laid, ps[0]).expect("p.a not found");
        let b = find_laid_out(&laid, ps[1]).expect("p.b not found");

        let gap =
            b.layout.border_box().y - (a.layout.border_box().y + a.layout.border_box().height);
        assert_eq!(
            gap, 6.0,
            "positive + negative margins should sum (10 + (-4) = 6)"
        );
    }

    #[test]
    fn parent_and_first_child_top_margins_collapse_through_the_parent() {
        // 親に border-top/padding-top が無ければ、最初の子のmargin-top は親を
        // 突き抜けて親の margin-top と相殺する。
        let dom = html::parse(br#"<div class="outer"><p class="inner">x</p></div>"#);
        let ua = user_agent_stylesheet();
        let author =
            parse_stylesheet(".outer { margin: 0; } .inner { height: 20px; margin: 12px 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let outer = find_laid_out(&laid, divs[0]).expect("outer not found");
        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let inner = find_laid_out(&laid, ps[0]).expect("inner not found");

        // 相殺の結果、親の実効 margin-top は collapse(0, 12) = 12 になる。
        assert_eq!(outer.layout.margin.top, 12.0);
        // 子の border 上端は親の content 上端に一致する(間に余白なし)。
        assert_eq!(inner.layout.content.y, outer.layout.content.y);
        // 親の高さは子の内容分(子の margin は外へ出たので含まない)。
        assert_eq!(outer.layout.content.height, 20.0);
    }

    #[test]
    fn a_top_border_on_the_parent_prevents_the_collapse() {
        // 親に border-top があれば相殺は起きず、子の margin-top は親の中に入る。
        let dom = html::parse(br#"<div class="outer"><p class="inner">x</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".outer { margin: 0; border-top: 5px solid black; }              .inner { height: 20px; margin: 12px 0; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let outer = find_laid_out(&laid, divs[0]).expect("outer not found");
        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);
        let inner = find_laid_out(&laid, ps[0]).expect("inner not found");

        assert_eq!(outer.layout.margin.top, 0.0, "no collapse through a border");
        // 子は親の content 上端から margin-top 分下がる。
        assert_eq!(inner.layout.content.y, outer.layout.content.y + 12.0);
    }

    #[test]
    fn an_empty_block_collapses_its_own_top_and_bottom_margins() {
        // 空ブロック(高さ0・border/padding無し)の上下マージンは1つに
        // 相殺され、二重に効かない。
        let dom = html::parse(br#"<div class="empty"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".empty { margin: 30px 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let empty = find_laid_out(&laid, divs[0]).expect("empty not found");
        // margin box の高さは 60(=30+30)ではなく 30(相殺された1つ分)。
        assert_eq!(empty.layout.margin_box_height(), 30.0);
    }

    #[test]
    fn auto_height_block_sizes_to_children_content() {
        let dom = html::parse(br#"<div class="outer"><p class="inner">x</p></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".inner { height: 40px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let outer = find_laid_out(&laid, divs[0]).expect("outer div not found");

        assert_eq!(outer.layout.content.height, 40.0);
    }

    #[test]
    fn wrapped_inline_content_drives_auto_height() {
        // 十分な幅があれば1行、狭ければ複数行に折り返される。
        let dom = html::parse(br#"<p class="a">hello world</p>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();

        let mut ps = Vec::new();
        find_all(&dom, dom.document(), "p", &mut ps);

        let wide = layout_document(&tree, &styles, &fonts, 800.0);
        let p_wide = find_laid_out(&wide, ps[0]).expect("p not found");
        let LaidOutContent::Inline(lines_wide) = &p_wide.content else {
            panic!("expected inline content")
        };
        assert_eq!(lines_wide.len(), 1);

        let narrow = layout_document(&tree, &styles, &fonts, 60.0);
        let p_narrow = find_laid_out(&narrow, ps[0]).expect("p not found");
        let LaidOutContent::Inline(lines_narrow) = &p_narrow.content else {
            panic!("expected inline content")
        };
        assert_eq!(lines_narrow.len(), 2);

        assert!(p_narrow.layout.content.height > p_wide.layout.content.height);
    }

    #[test]
    fn padding_and_border_offset_content_box() {
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".box { width: 100px; margin: 0; padding: 5px; border: 2px solid black; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(div_box.layout.content.width, 100.0);
        assert_eq!(div_box.layout.padding.left, 5.0);
        assert_eq!(div_box.layout.border.left, 2.0);

        let border_box = div_box.layout.border_box();
        assert_eq!(border_box.width, 2.0 + 5.0 + 100.0 + 5.0 + 2.0);
    }

    #[test]
    fn box_sizing_border_box_makes_the_specified_width_include_padding_and_border() {
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".box { box-sizing: border-box; width: 100px; height: 60px; margin: 0; \
             padding: 5px; border: 2px solid black; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        // border-boxでは指定した100px/60pxがpadding+border込みの外寸になるため、
        // content-boxはその分小さくなる(100 - 2*5 - 2*2 = 86)。
        assert_eq!(div_box.layout.content.width, 100.0 - 2.0 * 5.0 - 2.0 * 2.0);
        assert_eq!(div_box.layout.content.height, 60.0 - 2.0 * 5.0 - 2.0 * 2.0);

        let border_box = div_box.layout.border_box();
        assert_eq!(border_box.width, 100.0);
        assert_eq!(border_box.height, 60.0);
    }

    #[test]
    fn box_sizing_border_box_clamps_to_zero_when_padding_and_border_exceed_the_specified_width() {
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".box { box-sizing: border-box; width: 5px; margin: 0; \
             padding: 10px; border: 10px solid black; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(div_box.layout.content.width, 0.0);
    }

    #[test]
    fn border_style_none_zeroes_out_the_used_border_width_in_layout() {
        // CSS2.1 8.5.3: border-styleがnoneの辺は、border-widthの指定に関わらず
        // 使用値が0になる(枠線が描画されないだけでなく、レイアウト上の
        // 幅計算にも影響しない)。
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".box { width: 100px; margin: 0; border-width: 5px; border-style: none; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(div_box.layout.border.left, 0.0);
        let border_box = div_box.layout.border_box();
        assert_eq!(border_box.width, 100.0);
    }

    #[test]
    fn fragmentation_hints_reflect_the_elements_computed_style() {
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            ".box { break-before: always; break-inside: avoid; orphans: 3; widows: 4; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");

        assert_eq!(
            div_box.fragmentation.break_before,
            super::BreakBetween::Always
        );
        assert_eq!(div_box.fragmentation.break_after, super::BreakBetween::Auto);
        assert_eq!(
            div_box.fragmentation.break_inside,
            super::BreakInside::Avoid
        );
        assert_eq!(div_box.fragmentation.orphans, 3);
        assert_eq!(div_box.fragmentation.widows, 4);
    }

    #[test]
    fn anonymous_boxes_get_default_fragmentation_hints() {
        // 無名ボックス(混在コンテンツの折り返し等)は対応するDOM要素を持たないため、
        // fragmentationヒントは常に初期値(auto/auto/auto/2/2)になるはず。
        let dom = html::parse(br#"<div class="outer">before <p>P</p> after</div>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let div_box = find_laid_out(&laid, divs[0]).expect("div box not found");
        let LaidOutContent::Blocks(children) = &div_box.content else {
            panic!("expected block container")
        };
        let anonymous = children
            .iter()
            .find(|c| c.node.is_none())
            .expect("expected an anonymous block wrapping the loose text");

        assert_eq!(anonymous.fragmentation, FragmentationHints::default());
    }

    fn image_prepared(width: f32, height: f32) -> Rc<PreparedImage> {
        Rc::new(PreparedImage {
            width,
            height,
            content: crate::pdf::PreparedContent::Raster {
                color: ImagePlane {
                    data: Vec::new(),
                    filter: pdf_writer::Filter::FlateDecode,
                    color_space: PlaneColorSpace::Rgb,
                    bits_per_component: 8,
                },
                alpha: None,
            },
        })
    }

    fn image_box(content: ImageBoxContent) -> LayoutBox {
        LayoutBox::anonymous(BoxContent::Image(content))
    }

    #[test]
    fn image_with_no_attrs_uses_intrinsic_size_when_decoded() {
        let tree = image_box(ImageBoxContent {
            image: Some(image_prepared(200.0, 100.0)),
            attr_width: None,
            attr_height: None,
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.width, 200.0);
        assert_eq!(laid.layout.content.height, 100.0);
    }

    #[test]
    fn image_width_attr_only_derives_height_via_aspect_ratio() {
        // 内在サイズは200x100(2:1)。width=50pxのみ指定 → height=25px。
        let tree = image_box(ImageBoxContent {
            image: Some(image_prepared(200.0, 100.0)),
            attr_width: Some(50),
            attr_height: None,
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.width, 50.0);
        assert_eq!(laid.layout.content.height, 25.0);
    }

    #[test]
    fn image_height_attr_only_derives_width_via_aspect_ratio() {
        let tree = image_box(ImageBoxContent {
            image: Some(image_prepared(200.0, 100.0)),
            attr_width: None,
            attr_height: Some(40),
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.height, 40.0);
        assert_eq!(laid.layout.content.width, 80.0);
    }

    #[test]
    fn image_with_both_attrs_ignores_the_intrinsic_aspect_ratio() {
        let tree = image_box(ImageBoxContent {
            image: Some(image_prepared(200.0, 100.0)),
            attr_width: Some(10),
            attr_height: Some(10),
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.width, 10.0);
        assert_eq!(laid.layout.content.height, 10.0);
    }

    #[test]
    fn failed_image_with_no_attrs_collapses_to_zero_size() {
        let tree = image_box(ImageBoxContent {
            image: None,
            attr_width: None,
            attr_height: None,
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.width, 0.0);
        assert_eq!(laid.layout.content.height, 0.0);
    }

    #[test]
    fn failed_image_with_explicit_attrs_still_reserves_the_specified_space() {
        // 取得失敗でもwidth/height属性があればそのサイズの空ボックスとして
        // 扱う(後続コンテンツが不意にレイアウトが詰まらないよう、指定サイズ
        // 分のスペースは確保する)。
        let tree = image_box(ImageBoxContent {
            image: None,
            attr_width: Some(50),
            attr_height: Some(50),
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.width, 50.0);
        assert_eq!(laid.layout.content.height, 50.0);
    }

    #[test]
    fn image_does_not_stretch_to_fill_the_containing_block_like_a_block_div_would() {
        // 通常のブロック要素はwidth:autoでcontaining blockいっぱいに広がるが、
        // 置換要素はそうならない(内在サイズをそのまま使う)ことの確認。
        let tree = image_box(ImageBoxContent {
            image: Some(image_prepared(50.0, 50.0)),
            attr_width: None,
            attr_height: None,
        });
        let laid = layout_document(&tree, &HashMap::new(), &test_fonts(), 800.0);

        assert_eq!(laid.layout.content.width, 50.0);
    }

    #[test]
    fn outside_marker_is_positioned_left_of_the_content_edge_with_a_fixed_gap() {
        let dom = html::parse(br#"<ul><li>text</li></ul>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let mut lis = Vec::new();
        find_all(&dom, dom.document(), "li", &mut lis);
        let li = find_laid_out(&laid, lis[0]).expect("li not found");

        let marker = li.marker.as_ref().expect("li should have a marker");
        assert_eq!(marker.runs.len(), 1);
        assert!(marker.rect.width > 0.0);
        assert_eq!(
            marker.rect.x,
            li.layout.content.x - LIST_MARKER_GAP - marker.rect.width
        );
        assert_eq!(
            marker.rect.y, li.layout.content.y,
            "marker should align with the top of the li's own content"
        );
    }

    #[test]
    fn list_style_type_none_produces_no_marker_in_the_laid_out_box() {
        let dom = html::parse(br#"<ul><li style="list-style-type: none;">text</li></ul>"#);
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, 800.0);

        let li = find(&dom, dom.document(), "li").expect("li not found");
        let li_laid = find_laid_out(&laid, li).expect("li not found");
        assert!(li_laid.marker.is_none());
    }

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }
}
