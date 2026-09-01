//! The Block Formatting Context: width calculation against the containing block, plus the
//! vertical stacking of block elements (a simplified CSS2.1 sections 10.3.3 and 9.4.1).
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

/// The fixed gap (px) between a marker (`list-style-position: outside`) and the content edge.
const LIST_MARKER_GAP: f32 = 8.0;

#[derive(Debug, Clone)]
pub struct LaidOutBox {
    pub node: Option<NodeId>,
    pub layout: Layout,
    /// This box's computed `break-before`/`break-after`/`break-inside`/`orphans`/`widows`
    /// (used only for pagination decisions; an anonymous box takes the `ComputedStyle`
    /// initial values, that is `auto`/`auto`/`auto`/2/2).
    pub fragmentation: FragmentationHints,
    /// Whether this box actually draws a background colour or borders.
    /// `paginate.rs` uses it to decide whether a container split across pages needs a
    /// decoration fragment generating (to reproduce the background and borders; see the
    /// module docs).
    pub has_visible_decoration: bool,
    /// Whether the element has `float: left/right`. `paginate.rs` uses it to decide whether
    /// to treat the box specially as out of flow.
    pub is_float: bool,
    pub content: LaidOutContent,
    /// The marker (bullet or number) for `display: list-item`.
    /// It is represented as a `LineBox` holding one already-shaped `TextRun`, so
    /// `pdf::document::render_line` is reused unchanged to draw it.
    /// When pagination splits this box across pages, it is kept only on the first fragment (`paginate.rs`).
    pub marker: Option<Box<LineBox>>,
}

/// The CSS Fragmentation computed values carried by a [`LaidOutBox`].
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
    /// `display: grid`. It holds the row bands, which are the unit of pagination.
    Grid(LaidOutGrid),
    /// An `<img>`. `None` if the fetch or decode failed (treated as an empty replaced element, drawing nothing).
    Image(Option<Rc<PreparedImage>>),
}

/// A whole laid-out table (an optional caption plus the sequence of rows).
#[derive(Debug, Clone)]
pub struct LaidOutTable {
    /// The `Box` is needed to break the `LaidOutBox` -> `LaidOutContent::Table` ->
    /// `LaidOutTable` recursion by indirection.
    pub caption: Option<Box<LaidOutBox>>,
    pub caption_side: CaptionSide,
    pub rows: Vec<LaidOutTableRow>,
}

/// One laid-out table row.
#[derive(Debug, Clone)]
pub struct LaidOutTableRow {
    /// The original `display: table-row` element. `None` for an anonymous row (one created
    /// by the CSS anonymous box generation rules).
    pub node: Option<NodeId>,
    pub cells: Vec<LaidOutBox>,
    /// The section this row belongs to. `paginate` uses it to repeat the `<thead>` rows at
    /// the top of every page.
    pub section: TableSection,
}

/// One absolutely positioned box.
/// `laid` is already laid out against its containing block (in absolute coordinates), and
/// the pagination layer overlays it onto the page it belongs to.
#[derive(Debug, Clone)]
pub struct PositionedBox {
    pub laid: LaidOutBox,
    pub kind: PositionedKind,
}

/// Which kind of destination an absolutely positioned box has.
#[derive(Debug, Clone, Copy)]
pub enum PositionedKind {
    /// `position: fixed`. Repeated in the content area of every page, at the coordinates it
    /// was laid out at.
    Fixed,
    /// `position: absolute` with no positioned ancestor. Placed in the first page's content
    /// area, at the coordinates it was laid out at.
    AbsoluteInitial,
    /// `position: absolute` with a positioned ancestor. Placed on the page where the
    /// ancestor (`node`) first appears, offset by the difference between the ancestor's
    /// padding box position on the page and `padding_box_origin` (its top left at layout time).
    AbsoluteAncestor {
        node: NodeId,
        padding_box_origin: (f32, f32),
    },
}

/// The absolute positioning context carried around during layout.
/// Passed to descendants by `&mut`, like `float_ctx`.
pub(super) struct PosCtx<'a> {
    /// The current containing block for `absolute` (the nearest positioned ancestor's
    /// padding box, or the first page's content area if there is none).
    abs_cb: AbsCB,
    /// The containing block for `fixed`: the `(width, height)` of the page's content area.
    page_size: (f32, f32),
    /// The absolutely positioned boxes collected.
    out: &'a mut Vec<PositionedBox>,
}

/// The containing block for `absolute`.
#[derive(Debug, Clone, Copy)]
enum AbsCB {
    /// The initial containing block (no positioned ancestor): the first page's content area.
    /// Its origin is `(0, 0)`.
    InitialPage,
    /// A positioned ancestor's padding box (in absolute coordinates).
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

/// The absolute-positioning-aware version of [`layout_document`]. Returns the normal-flow
/// `LaidOutBox` plus the list of absolutely positioned boxes ([`PositionedBox`]).
/// `page_size` is `(content_width, content_height)`, used as the containing block for `fixed`.
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

/// Lay the whole box tree out with the page width as the initial containing block.
pub fn layout_document(
    root: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    page_width: f32,
) -> LaidOutBox {
    layout_document_from(root, styles, fonts, page_width, 0.0, 0.0)
}

/// A variant of [`layout_document`] that starts laying out at `(start_x, start_y)` rather
/// than at the origin `(0.0, 0.0)`.
pub fn layout_document_from(
    root: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    start_x: f32,
    start_y: f32,
) -> LaidOutBox {
    // The backward-compatible entry point that collects no absolute positioning (for existing callers and tests).
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

/// The absolute-positioning-aware version of [`layout_document_from`].
/// Absolutely positioned boxes are collected into `pos`.
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
    // One `FloatContext` is shared across an entire call to
    // `layout_document`/`layout_document_from`.
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

/// Used for a `<caption>` (which goes through the normal width resolution; specific to
/// caption placement in `table.rs`) and for recursive calls within block.rs.
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

/// Used when the content-box width is to be given directly, without going through the
/// normal `width` resolution (auto and margin calculation), as for a table cell
/// (specific to [`super::table`]).
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

/// The height-forcing extension of `layout_box_with_forced_width`: it forces both width and
/// height (specific to [`super::flex`]). Used in the final layout pass that produces the
/// real `LaidOutBox` at the width and height taffy settled for each flex item. Where
/// `align-items: stretch` (the default) had taffy stretch an item to the container's height,
/// this forcing makes the background colour and borders cover the stretched height too.
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

/// A wrapper specific to the measuring pass, whose result is thrown away. Used on the path
/// where a flex container lays the same item out repeatedly to return an intrinsic size to taffy.
///
/// Any `absolute`/`fixed` found while measuring is discarded. The final layout pass walks
/// the same descendants and collects them into the real `PosCtx`, so collecting here would
/// register the same box several times over.
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

/// Resolve `b`'s content width, margins, padding and borders (including auto-sizing for a
/// replaced element). Shared logic called both from the body of `layout_box_impl` and from
/// the advance width calculation for float placement (`layout_float_child`).
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

    // Resolving the content width. `min-width`/`max-width` take effect as a clamp on the
    // used width found here.
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
            // `width: auto` on an absolutely positioned box is shrink-to-fit too, so with a
            // settled height the width follows from `aspect-ratio`.
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
    // The x of the top left of the margin box.
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
    // Lay out at `top` first (or the top of the cb if there is none).
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

    // A `bottom` setting (with no `top`) is repositioned to align to the bottom once the
    // laid-out height is known.
    if !has_top && has_bottom && cb_rect.height > 0.0 {
        let mbh = laid.layout.margin_box_height();
        let target_y = cb_rect.y + cb_rect.height - mbh - bottom;
        shift_box_y_in_place(&mut laid, margin_box_y - target_y);
    }
    pos.out.push(PositionedBox { laid, kind });
}

/// The shrink-to-fit (content-based) content width. Shared by the atomic box of
/// `display: inline-block` and by `width: auto` on a float. We have no CSS2.1 preferred
/// minimum width, so it is simplified to `min(preferred, available)` (content exceeding the
/// available width wraps).
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
        // When a float has an explicit `width`, `resolve_width_and_horizontal_margins` is not
        // used: passing its "over-constrained" rule straight through (which recomputes
        // margin-right to fill the remaining width when width, margin-left and margin-right
        // are all non-auto, including the default 0 for an omitted `margin`; CSS2.1 section
        // 10.3.3's rule for normal flow) would let the huge recomputed margin-right leak into
        // `margin_box_width`, which is the advance width used for float placement.
        // Floats have no such recomputation rule (CSS2.1 section 10.3.5: an auto margin is
        // simply 0), so it is bypassed here.
        None if style.float != Float::None
            && !matches!(style.width, LengthPercentageOrAuto::Auto) =>
        {
            let width = resolve_lpa_or_zero(style.width, containing_width);
            // The conversion for `box-sizing: border-box`. The same adjustment as the
            // normal-flow `resolve_width_and_horizontal_margins` is applied here too.
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
        // On a float, `width: auto` shrinks to the content (shrink-to-fit, CSS2.1 section
        // 10.3.5). It must not fall through to the normal-flow
        // `resolve_width_and_horizontal_margins`, which fills the containing width.
        // An auto margin is 0 on a float.
        None if style.float != Float::None => {
            let available = (containing_width
                - resolve_lpa_or_zero(style.margin_left, containing_width)
                - resolve_lpa_or_zero(style.margin_right, containing_width)
                - padding.left
                - padding.right
                - border.left
                - border.right)
                .max(0.0);
            // With a settled height, the width follows from `aspect-ratio`.
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

    // A positioned element (relative/absolute/fixed) makes its own padding box the
    // containing block for its descendants' `absolute`. The height is not used, to avoid a
    // cycle (bottom placement is unsupported).
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

                // `position: absolute`/`fixed` is out of flow (occupying no space).
                // It is placed against its containing block and collected into `pos.out`.
                if child_style.position.is_out_of_flow() {
                    layout_out_of_flow_child(child, &child_style, styles, fonts, pos);
                    continue;
                }

                if child_style.clear != Clear::None {
                    cursor_y = float_ctx.clearance(child_style.clear, cursor_y);
                }

                if child_style.float != Float::None {
                    // A float does not take part in the flow (CSS2.1 9.5): it is not subject
                    // to margin collapsing and does not advance `cursor_y`. `float_ctx` is
                    // shared with children and grandchildren, so later normal-flow and inline
                    // content in this BFC can see it for flow-around.
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

                // Margin collapsing between adjacent siblings (CSS2.1 section 8.3.1). The
                // previous sibling's margin-bottom and this child's margin-top are replaced by
                // a single gap: not their sum, but "the largest positive plus the smallest
                // negative". Floats do not take part in the flow and are excluded (we look for the previous non-float child).
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
            // If a direct float child extends below the normal flow, extend the auto height
            // by that much (a shallow implementation of CSS2.1 10.6.7 that does not propagate
            // to grandchildren; a known simplification).
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
                // An anonymous box has no style of its own (`style` holds the initial values).
                b.node.is_some().then_some(&style),
                pos,
            );
            // An inline-level `display: inline-block` box is moved to its final coordinates at
            // this point, once the line's position is settled.
            place_atomic_inlines(&mut lines);
            // `text-overflow: ellipsis` is applied as post-processing after line layout.
            apply_text_overflow(&mut lines, &style, content_width, fonts);
            let lines_height: f32 = lines.iter().map(|line| line.rect.height).sum();
            let height =
                resolve_used_height(&style, &padding, &border, content_width, lines_height);
            (LaidOutContent::Inline(lines), height)
        }
        BoxContent::Table(table) => {
            // A `display: table` cell establishes a new Block Formatting Context
            // (CSS2.1 9.4.1), so it is kept independent of the outer `float_ctx`.
            // `border-spacing` is mutually exclusive with `border-collapse: collapse`, so
            // under collapse it is squashed to 0 here before being passed on.
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
            // A grid container also establishes a new formatting context, like flex and
            // table.
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
            // Like `display: table`, a flex container establishes a new formatting context
            // (`float` has no effect on a flex item, per the CSS spec), so it is kept
            // independent of the outer `float_ctx`.
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
            // When `apply_replaced_element_auto_size` has run, the case where both widths
            // were auto has already been replaced with concrete Lengths, so `resolve_height`
            // returns `Some` (a height of zero being a sensible default when no intrinsic
            // size is available, that is when the fetch or decode failed).
            // The `min-height`/`max-height` clamps apply as for any other content kind, but
            // the aspect ratio is not preserved.
            let height = resolve_used_height(&style, &padding, &border, content_width, 0.0);
            (LaidOutContent::Image(image_content.image.clone()), height)
        }
    };
    // The descendants are laid out, so the containing block is restored.
    pos.abs_cb = saved_cb;
    // Reflect the height taffy settled directly in the final layout pass
    // (specific to `layout_box_with_forced_size`).
    let mut content_height = forced_content_height.unwrap_or(content_height);

    // Parent/child and empty-block margin collapsing. `layout.margin` becomes the effective
    // (collapsed) value, and `content.y`/`content_height` are adjusted to match. Parent/child
    // collapsing applies only to `height: auto` blocks (a known simplification).
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

    // The visual offset of `position: relative`. The `cursor_y` calculation for later
    // siblings uses `margin_box_height` (which does not depend on coordinates), so shifting
    // the content coordinates here does not affect the flow of what follows.
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

/// Lay out the marker of a `display: list-item` (with `list-style-position: outside`, or
/// having fallen back from `inside` because of block children). The marker is simply placed
/// independently outside the content box, in the left gutter, so the same logic handles
/// `b`'s content whether it is `BoxContent::Inline` or `Blocks`.
///
/// The implementation reuses exactly the same shaping as an ordinary text run (`shape_run`)
/// and returns the result as a `LineBox` with a single `runs` entry. That lets the drawing
/// side (`pdf::document::render_line`) be reused entirely unchanged.
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

/// Place a float child. The width resolution happens twice, here in `resolve_box_geometry`
/// and again in the real layout, because `float_ctx.place` needs the margin box width before
/// it can decide the placement coordinates (and getting an accurate width requires resolving
/// auto-size for a replaced element such as `<img>`, so the precomputation cannot be skipped).
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

/// Resolve the visual offset `(dx, dy)` from `position: relative`'s top/right/bottom/left.
/// The precedence is `top` over `bottom` and `left` over `right`, as the CSS spec says.
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

/// Whether the combination of `style` and `border` (the computed thicknesses) actually draws
/// anything. `true` when there is a background colour, or when any of the four edges has a
/// positive thickness and a `border-style` other than `none` (the same condition under which
/// `pdf::document::render_box_decoration` really draws).
pub(crate) fn has_visible_decoration(style: &ComputedStyle, border: &EdgeSizes) -> bool {
    if style.background_color.alpha > 0.0 {
        return true;
    }
    // An element with only a `background-image` (no background colour or borders) cannot be
    // reached by `collect_image_uses`/`render_box` and is never drawn unless `place_split`
    // includes it among the boxes it generates a decoration fragment for (a `LaidOutBox` with a `node`).
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

/// A box's computed style.
///
/// A real element's style is borrowed from `styles` as-is. A `ComputedStyle` is over 1KB and
/// holds a `font_family: Vec<String>`, so cloning it piles up heap allocations on every
/// layout (it is called three times per cell in a table).
/// Only a caller that wants to modify it clones, via `into_owned`.
pub(super) fn box_style<'a>(
    b: &LayoutBox,
    styles: &'a HashMap<NodeId, Rc<ComputedStyle>>,
) -> Cow<'a, ComputedStyle> {
    match b.node {
        Some(node) => Cow::Borrowed(&styles[&node]),
        // An anonymous box (CSS2.1 9.2.1.1): a block with no margins, padding or borders.
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

/// Clamping the used width by `min-width`/`max-width`.
///
/// They are applied `max` then `min`, so `min-width` wins when `min-width > max-width`
/// (the same result as the procedure in CSS2.1 section 10.4). Under `box-sizing: border-box`
/// the `min-*`/`max-*` values are border-box based too, so as with `width` the padding and
/// border are subtracted to bring them to content-box before comparing.
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

/// Clamping the used height by `min-height`/`max-height`. A percentage is ignored, the
/// containing block's height being indefinite (handled the same way as an ignored percentage `height`).
/// (handled the same way as an ignored percentage `height`).
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

/// The absolute length (px) usable in the vertical direction. A percentage, or a `calc` with a
/// percentage component, returns `None` (is ignored): the containing block height is indefinite.
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

/// An edge with `border-style: none` has a used value of `0` regardless of its `border-width`
/// (CSS2.1 8.5.3). That rounding has to be reflected in layout (width calculation) too.
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

/// The used height: the value chosen in the order "explicit `height`, then derived from
/// `aspect-ratio`, then `auto_height` (the height derived from the content)", clamped by
/// `min-height`/`max-height`.
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

/// The content height derived from `aspect-ratio`. `None` if there is no ratio. The box the
/// ratio applies to follows `box-sizing`.
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

/// The content width derived from `aspect-ratio`. Used where the height is settled and
/// `width: auto` is in a shrink-to-fit context (a float, `inline-block`, absolute positioning
/// or `<img>`). It is not called for `width: auto` on a normal-flow block, where stretch wins.
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

/// Return the `height` if it is given explicitly. `auto`, and a percentage (the containing
/// block's height being indefinite), give `None`, and the caller uses the content height
/// instead. Under `box-sizing: border-box` the value is the border-box height, so
/// `padding_tb`/`border_tb` are subtracted to bring it to content-box.
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

/// Only where both `width` and `height` of a replaced element (`<img>`) are `auto`, apply a
/// simplified version of CSS2.2 sections 10.3.2 and 10.6.2 (resolution from a replaced
/// element's intrinsic size): decide from the HTML attributes (`width`/`height`) first and the
/// intrinsic size (on a successful decode) second, deriving the other side from the ratio.
///
/// Where CSS gives exactly one of `width`/`height` explicitly, the other is derived from the
/// used ratio (the `aspect-ratio` setting, or the intrinsic ratio when there is none).
/// "A settled width with `height: auto`" is derived downstream by [`resolve_used_height`],
/// so nothing happens here.
pub(super) fn apply_replaced_element_auto_size(
    style: &mut ComputedStyle,
    image: &ImageBoxContent,
    containing_width: f32,
) {
    // Bake the intrinsic ratio into the computed style. The general logic downstream then
    // only has to say "use `style.aspect_ratio.ratio` if it is there".
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
        // A settled height with `width: auto`. `width: auto` on a replaced element is
        // shrink-to-fit (not an ordinary block's stretch), so the width follows from the ratio.
        if let Some(width) = aspect_ratio_width(style, &padding, &border) {
            style.width = LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(width));
        }
        return;
    }

    // Both `auto`: decide from the HTML attributes (`width`/`height`) first and the intrinsic
    // size (on a successful decode) second, deriving the other side from the aspect ratio when
    // only one value is available (a simplified CSS2.2 sections 10.3.2 and 10.6.2).
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

    // An `aspect-ratio` given without `auto` (`aspect-ratio: 16 / 9`, say) wins over the
    // intrinsic ratio. The width stays intrinsic and only the height is redecided by the ratio.
    let height = if style.aspect_ratio.auto {
        height
    } else {
        aspect_ratio_height(style, &padding, &border, width).unwrap_or(height)
    };
    style.height = LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(height));
}

/// The image's intrinsic aspect ratio (`width / height`). `None` when it has not been
/// decoded, or when the height is 0.
fn intrinsic_ratio(image: &ImageBoxContent) -> Option<f32> {
    let prepared = image.image.as_ref()?;
    (prepared.height > 0.0).then(|| prepared.width / prepared.height)
}

/// From `known` (the length of one known side), derive the other side preserving the aspect
/// ratio, using `ratio_basis` (the intrinsic length of the unknown side and of the known one).
/// Returns 0 when there is no intrinsic size (the decode failed) or the known side's
/// intrinsic length is 0 (which the caller reads as "size unknown").
fn derive_via_aspect_ratio(known: f32, ratio_basis: Option<(f32, f32)>) -> f32 {
    match ratio_basis {
        Some((other_intrinsic, known_intrinsic)) if known_intrinsic > 0.0 => {
            known * other_intrinsic / known_intrinsic
        }
        _ => 0.0,
    }
}

/// Apply parent/child and empty-block margin collapsing.
///
/// `margin` becomes the effective (collapsed) value, and `content.y` and `content_height`
/// are adjusted to match. By storing the returned effective `margin` on the `LaidOutBox`
/// as-is, the caller lets an ancestor's adjacent-sibling collapsing loop chain naturally into multi-level collapsing.
fn apply_margin_collapse(
    content: &mut LaidOutContent,
    content_height: &mut f32,
    margin: &mut EdgeSizes,
    content_y: &mut f32,
    border: &EdgeSizes,
    padding: &EdgeSizes,
    height_is_auto: bool,
) {
    // An empty block: height 0, no border or padding, no children. Its own top and bottom
    // margins collapse into one (moved up so `margin_box_height` is that single collapsed
    // value). It then collapses with the sibling above against that value, preventing a double margin.
    let content_is_empty = match content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => children.is_empty(),
        // A `<div></div>` with no children becomes an empty `Inline`.
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

    // Parent and first child: with no top boundary on the parent (no border-top or
    // padding-top), the first non-float child's effective `margin-top` is lifted out of the parent and collapsed.
    if border.top == 0.0 && padding.top == 0.0 && height_is_auto {
        if let Some(first_top) = children
            .iter()
            .find(|c| !c.is_float)
            .map(|c| c.layout.margin.top)
        {
            let effective = collapse_adjacent_margins(margin.top, first_top);
            // Every child moves up by however much the first child's `margin-top` left the
            // parent's content. delta <= 0.
            let child_delta = effective - first_top - margin.top;
            for child in children.iter_mut() {
                shift_box_y_in_place(child, -child_delta);
            }
            *content_y += effective - margin.top;
            *content_height -= first_top;
            margin.top = effective;
        }
    }

    // Parent and last child: with no bottom boundary on the parent (no border-bottom,
    // padding-bottom or explicit height), the last non-float child's `margin-bottom` is
    // lifted out of the parent and collapsed.
    if border.bottom == 0.0 && padding.bottom == 0.0 && height_is_auto {
        if let Some(last_bottom) = children
            .iter()
            .rev()
            .find(|c| !c.is_float)
            .map(|c| c.layout.margin.bottom)
        {
            let effective = collapse_adjacent_margins(margin.bottom, last_bottom);
            // The last child's margin-bottom leaves the parent's content, so it shrinks by that much.
            *content_height -= last_bottom;
            margin.bottom = effective;
        }
    }
}

/// Find the gap resulting from collapsing two adjacent margins (CSS2.1 section 8.3.1).
/// With both non-negative it is the larger; with both negative it is the smaller (the larger
/// in absolute value); with mixed signs it is their plain sum (the largest positive plus the smallest negative).
fn collapse_adjacent_margins(a: f32, b: f32) -> f32 {
    let positive = a.max(0.0).max(b.max(0.0));
    let negative = a.min(0.0).min(b.min(0.0));
    positive + negative
}

/// A simplified version of CSS2.1 section 10.3.3 (block-level, non-replaced elements).
/// `margin-left + border-left + padding-left + width + padding-right + border-right + margin-right
/// = the containing block's width`, fill in whichever items are `auto`.
pub(crate) fn resolve_width_and_horizontal_margins(
    style: &ComputedStyle,
    containing_width: f32,
    padding_lr: f32,
    border_lr: f32,
) -> (f32, f32, f32) {
    let (width, margin_left, margin_right) =
        solve_horizontal(style, containing_width, padding_lr, border_lr, None);

    // Clamp by `min-width`/`max-width`, and if the value changed, re-solve the horizontal
    // equation treating that width as though it had been given explicitly (CSS2.1 section
    // 10.4). Without that, a setting such as `width: auto; max-width: 600px; margin: 0 auto`
    // would keep the margin autos that the auto-width branch had squashed to 0, and never centre.

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

/// Solve CSS2.1 section 10.3.3's horizontal equation (margin-left + border + padding +
/// width + margin-right = the containing width). Passing `Some` for `used_width` treats that
/// value (content-box based and already converted) as an explicitly given `width`
/// (for re-solving after the min/max width clamp).
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
            // Under `box-sizing: border-box` the value is the border-box width, so padding
            // and border are subtracted to bring it to content-box before it is handed to the
            // existing equation.
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
            // Over-constrained (CSS2.1 section 10.3.3): where width, margin-left and
            // margin-right are all given explicitly, the margin-right value is ignored and
            // its used value is recomputed so that the equation (margin-left + border/padding
            // + width + margin-right = the containing width) holds exactly (it may come out
            // negative). Under `direction: rtl` margin-left should be recomputed instead, but
            // rtl itself is unsupported, so ltr is always assumed.
            let margin_left = resolve_lpa_or_zero(style.margin_left, containing_width);
            let margin_right = containing_width - border_lr - padding_lr - width - margin_left;
            (width, margin_left, margin_right)
        }
    }
}

/// Return a copy of `b`'s whole subtree with its Y coordinates translated by `delta`.
/// `paginate.rs` uses it to convert from continuous whole-page coordinates to within-page
/// coordinates (subtracting `delta`). `table.rs` also uses it to place a caption for
/// `caption-side: bottom` (passing a negative `delta` to move it downwards).
/// Move each line's atomic inline boxes (`display: inline-block`) to match the line's
/// settled position (`line.rect` and the baseline).
///
/// `layout::inline` lays their contents out before the line's vertical position is known
/// (at the origin 0,0), so they are all translated here. Vertically they are placed so that
/// the bottom of the margin box sits on the baseline.
fn place_atomic_inlines(lines: &mut [LineBox]) {
    for line in lines.iter_mut() {
        let baseline_y = line.rect.y + line.baseline;
        for atomic in line.atomics.iter_mut() {
            // The target position of the top left of the margin box.
            let target_x = line.rect.x + atomic.x_offset;
            let target_y = baseline_y - atomic.baseline_shift - atomic.margin_box_height;
            // The current top left of the margin box (laid out at the origin 0, so it follows
            // from the content coordinates minus margin/border/padding).
            let layout = atomic.content.layout;
            let current_x =
                layout.content.x - layout.padding.left - layout.border.left + -layout.margin.left;
            let current_y =
                layout.content.y - layout.padding.top - layout.border.top - layout.margin.top;
            // Note that `shift_box_y`'s `delta` is an amount to subtract (`shift_rect_y` does
            // `y -= delta`), whereas `shift_box_x`'s is an amount to add.
            shift_box_y_in_place(&mut atomic.content, current_y - target_y);
            shift_box_x_in_place(&mut atomic.content, target_x - current_x);
        }
    }
}

/// The x-direction counterpart of [`shift_box_y`] (used for horizontal placement of atomic inline boxes).
pub(super) fn shift_box_x(b: &LaidOutBox, delta: f32) -> LaidOutBox {
    let mut shifted = b.clone();
    shift_box_x_in_place(&mut shifted, delta);
    shifted
}

/// The in-place version of [`shift_box_x`] (for the same reason as [`shift_box_y_in_place`]).
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

/// The in-place version of [`shift_box_y`].
///
/// Cloning the subtree at every level of the recursion would rebuild the same data once per
/// level, inflating both the time and the peak memory for nothing. The move can be done on a
/// borrow, so the caller clones once, only when it has to.
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
                row.top += delta;
                row.bottom += delta;
                for item in row.items.iter_mut() {
                    shift_box_y_in_place(item, delta);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines.iter_mut() {
                shift_rect_y(&mut line.rect, delta);
                // The atomic boxes in a line move with the line.
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
        // Translating `b.layout.content` (at the top of this function) is enough. An image
        // has no child that carries a Rect of its own, unlike the lines of an `Inline`.
        LaidOutContent::Image(_) => {}
    }
}

fn shift_rect_y(rect: &mut Rect, delta: f32) {
    rect.y -= delta;
}

/// Shift only `b`'s contents (its child boxes, lines, or table rows and cells) vertically,
/// leaving `b`'s own position (`b.layout`) unchanged. It is deliberately distinct from
/// `shift_box_y`, which translates everything including the box itself: in the table cell
/// `vertical-align` implementation, the cell's own height and position are already settled by
/// the row height equalisation and must not change, while the content inside does need moving.
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
                row.top += delta;
                row.bottom += delta;
                for item in row.items.iter_mut() {
                    *item = shift_box_y(item, delta);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines.iter_mut() {
                shift_rect_y(&mut line.rect, delta);
                // The atomic boxes in a line move with the line.
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
        // An `Image` has no child carrying a Rect of its own, unlike the lines of an
        // `Inline`, so there is nothing to move (the `vertical-align` of an image nested in a
        // cell is left to moving the cell's whole content as one block).
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

        // html: no margin, padding or border -> content_width=800
        // body: the UA default margin of 8px -> content_width=784
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
        // Where width, margin-left and margin-right are all given explicitly and do not add
        // up to the containing width (over-constrained), CSS2.1 section 10.3.3 ignores the
        // given margin-right and recomputes it so the equation holds.
        let dom = html::parse(br#"<div class="box"></div>"#);
        let ua = user_agent_stylesheet();
        // containing width = 784 (html: 800, body margin: 8px each side).
        // width:300 + margin-left:50 + the given margin-right:50 = 400, so margin-right should
        // be recomputed to 434 to reach 784.
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
        // containing width = 784. The width alone fills it, so margin-left pushes it over and
        // the recomputed margin-right should come out negative, differing even in sign from
        // the given value (99px).
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
        // Both have 16px top and bottom margins. Collapsed, the gap between the border boxes
        // should be 16px, not 32px (their sum).
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
        // A float does not take part in the flow, so the block that follows ignores the
        // float's height (50px) and is placed straight from the top of the containing block.
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
        // Even with a float between a and b, margin collapsing with the previous non-float
        // child (a) still applies: max(20, 30) = 30. Including the float in the collapsing
        // would skew this value (a float has no margin and would count as 0).
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

        // The normal position is x=0, y=10 (below a), plus the top:5px/left:7px offset.
        assert_eq!(rel_box.layout.content.x, 7.0);
        assert_eq!(rel_box.layout.content.y, 15.0);
        // c is placed against the rel element's real (pre-offset) bottom edge (10+20=30) and
        // is unaffected by the visual offset.
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
        // With no border-top or padding-top on the parent, the first child's margin-top
        // escapes the parent and collapses with the parent's margin-top.
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

        // After collapsing, the parent's effective margin-top is collapse(0, 12) = 12.
        assert_eq!(outer.layout.margin.top, 12.0);
        // The top of the child's border coincides with the top of the parent's content (no gap between them).
        assert_eq!(inner.layout.content.y, outer.layout.content.y);
        // The parent's height covers the child's content (the child's margin has moved outside, so it is not included).
        assert_eq!(outer.layout.content.height, 20.0);
    }

    #[test]
    fn a_top_border_on_the_parent_prevents_the_collapse() {
        // With a border-top on the parent there is no collapsing, and the child's margin-top stays inside the parent.
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
        // The child sits margin-top below the top of the parent's content.
        assert_eq!(inner.layout.content.y, outer.layout.content.y + 12.0);
    }

    #[test]
    fn an_empty_block_collapses_its_own_top_and_bottom_margins() {
        // The top and bottom margins of an empty block (height 0, no border or padding)
        // collapse into one and do not apply twice.
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
        // The margin box height is 30 (the single collapsed value), not 60 (=30+30).
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
        // With enough width it is one line; when narrow it wraps onto several.
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

        // Under border-box the given 100px/60px are the outer dimensions including padding
        // and border, so the content box is smaller by that much (100 - 2*5 - 2*2 = 86).
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
        // CSS2.1 8.5.3: an edge whose border-style is none has a used value of 0 regardless
        // of its border-width (it is not merely undrawn; it does not affect the width
        // calculation in layout either).
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
        // An anonymous box (from wrapping mixed content, say) has no corresponding DOM
        // element, so its fragmentation hints should always be the initial values (auto/auto/auto/2/2).
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
        // The intrinsic size is 200x100 (2:1). With only width=50px given -> height=25px.
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
        // Even on a failed fetch, width/height attributes make it an empty box of that size
        // (reserving the space so the content that follows does not suddenly shift).

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
        // An ordinary block element with width:auto fills the containing block, but a
        // replaced element does not (it uses its intrinsic size as-is).
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
