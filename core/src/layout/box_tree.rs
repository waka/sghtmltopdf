//! Building the layout box tree from the DOM plus the computed styles.
//!
//! Elements with `display: none` (and their subtrees) are excluded. Where a block
//! container's children mix block-level with inline-level content and text, runs of
//! consecutive inline-level content are wrapped in anonymous block boxes, following the CSS
//! anonymous box generation rules (CSS2.1 9.2.1.1). An anonymous box has no corresponding DOM node, so its `node` is `None`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::html::{Dom, NodeData, NodeId};
use crate::pdf::{ImageAssetCache, PreparedImage};
use crate::style::{
    CaptionSide, ComputedStyle, Display, LengthPercentage, LengthPercentageOrAuto,
    ListStylePosition, ListStyleType, RgbaColor, WhiteSpace,
};

use super::white_space;

#[derive(Debug, Clone)]
pub struct LayoutBox {
    /// The corresponding DOM element. `None` for an anonymous box.
    pub node: Option<NodeId>,
    pub content: BoxContent,
    /// The marker text (bullet or number) for `display: list-item`.
    /// With `list-style-position: inside` and `BoxContent::Inline` content, it is instead
    /// embedded directly into the first `InlineSpan` of `content`, so this stays `None`
    /// (to avoid drawing it twice). Otherwise (`outside`, or an `inside` with block
    /// children) the layout layer (`block.rs`) reads this field and places it separately.
    pub marker: Option<String>,
    /// Memoised measurements. See [`MeasureMemo`].
    pub measured: MeasureMemo,
}

impl LayoutBox {
    /// A box holding only content (an anonymous box, or the container for a replaced element).
    pub fn anonymous(content: BoxContent) -> Self {
        Self {
            node: None,
            content,
            marker: None,
            measured: MeasureMemo::default(),
        }
    }

    /// The box corresponding to `node`.
    pub fn for_node(node: NodeId, content: BoxContent) -> Self {
        Self {
            node: Some(node),
            content,
            marker: None,
            measured: MeasureMemo::default(),
        }
    }
}

/// The measurement memo for one box.
///
/// The same subtree is measured over and over: a flex/grid item is measured at every
/// ancestor level, and a throwaway layout runs for each candidate width just to learn the
/// height, so without the memo the cost grows exponentially in the nesting depth.
///
/// A memoised value is determined solely by the box's content, computed style and font. The
/// tree is rebuilt per document, and content rewriting such as `resolve_images` finishes
/// before layout begins, so it is immutable during layout.
#[derive(Debug, Clone, Default)]
pub struct MeasureMemo {
    /// The natural (max-content) width. Filled in by [`super::table::measure_natural_content_width`].
    natural_width: Cell<Option<f32>>,
    /// The content height when built with a fixed content width. Filled in by the flex/grid
    /// measure bridge.
    ///
    /// The key is the pair of content width and containing width (`(content, containing)`).
    /// Percentages inside resolve against the containing width, so the same content width
    /// can give a different height under a different containing width. Only a handful of
    /// pairs are ever asked of one box, so a linear search suffices (and beats hashing).
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
    /// The content of an inline formatting context.
    Inline(Vec<InlineSpan>),
    /// The content of a `display: table` element (its rows and cells).
    Table(TableBox),
    /// The content of a `display: flex` element (its sequence of flex items).
    Flex(FlexBox),
    Grid(GridBox),
    /// An `<img>` element (treated as a replaced element; see [`resolve_images`]).
    Image(ImageBoxContent),
}

/// The sequence of flex items collected from a `display: flex` element. Each item is an
/// ordinary `LayoutBox`, just like a block child (one per child element; the anonymous box
/// generation rules of `build_children_boxes` do not apply).
#[derive(Debug, Clone)]
pub struct FlexBox {
    pub items: Vec<LayoutBox>,
}

/// A `display: grid` container. Structurally identical to [`FlexBox`]; only the taffy
/// `Style` handed over at layout time differs.
#[derive(Debug, Clone)]
pub struct GridBox {
    pub items: Vec<LayoutBox>,
}

/// The content of an `<img>` element. Built by `resolve_images`.
#[derive(Debug, Clone)]
pub struct ImageBoxContent {
    /// The image data when the fetch and decode succeeded. On failure (a network error, an
    /// SSRF block, an undecodable image; all treated alike) it is `None`, and layout treats
    /// this as an empty replaced element.
    pub image: Option<std::rc::Rc<crate::pdf::PreparedImage>>,
    /// The values of the `width`/`height` attributes (px, from the HTML attributes)
    pub attr_width: Option<u32>,
    pub attr_height: Option<u32>,
}

/// The sequence of rows collected from a `display: table` element, plus an optional `caption`.
#[derive(Debug, Clone)]
pub struct TableBox {
    /// The `display: table-caption` child (`<caption>`). With several, only the first is
    /// used (a known simplification). The `Box` is needed to break the
    /// `LayoutBox` -> `BoxContent::Table` -> `TableBox` recursion by indirection (avoiding
    /// the infinite-size compile error).
    pub caption: Option<Box<LayoutBox>>,
    /// The `caption-side` read from the caption's computed style (the initial value `Top` when there is no caption).
    pub caption_side: CaptionSide,
    pub rows: Vec<TableRow>,
    /// Column width hints from `<colgroup>`/`<col>` (in column index order, `None` meaning
    /// unspecified). It holds the `width` from the `<col>` element's computed style as-is.
    /// Any beyond the real column count are discarded by `layout::table`, and any shortfall counts as unspecified.
    pub column_widths: Vec<Option<LengthPercentage>>,
}

/// The section a table row belongs to. `<thead>`/`<tbody>`/`<tfoot>` have no `display`
/// value of their own, being "transparent containers", so this is decided from the
/// container's element name and kept here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableSection {
    Head,
    #[default]
    Body,
    Foot,
}

/// One `display: table-row` element (`<tr>`).
#[derive(Debug, Clone)]
pub struct TableRow {
    /// The original `display: table-row` element. A row created by the CSS anonymous box
    /// generation rules has no corresponding DOM node, so it is `None`.
    pub node: Option<NodeId>,
    pub cells: Vec<TableCell>,
    /// The section this row belongs to. The pagination layer uses it to repeat the
    /// `<thead>` rows at the top of every page.
    pub section: TableSection,
}

/// One `display: table-cell` element (`<td>`/`<th>`).
#[derive(Debug, Clone)]
pub struct TableCell {
    /// The original `display: table-cell` element. `None` for an anonymous cell.
    pub node: Option<NodeId>,
    /// The value of the `colspan` attribute (1 when absent or invalid).
    pub colspan: usize,
    /// The value of the `rowspan` attribute (1 when absent or invalid). `rowspan="0"`
    /// (HTML5's "extend to the end of the section" special value) is not supported and is treated as 1.
    pub rowspan: usize,
    /// The cell's own content (the same structure as an ordinary block/inline box).
    pub content: LayoutBox,
}

/// A text run with a single computed style, coming from one DOM text node.
#[derive(Debug, Clone)]
pub struct InlineSpan {
    /// The DOM text node this text came from. The computed style is looked up from `styles`
    /// (declarations on ancestors such as `<b>` or `<span style="...">` are already
    /// inherited and cascaded into the text node's own computed style, so that is all we need).
    pub node: NodeId,
    pub text: String,
    /// Whether this is the first character split off for `::first-letter`. When `true`,
    /// some properties are overridden by `node`'s computed `first_letter_style` (if any),
    /// which `layout::inline::flatten_spans` applies.
    pub is_first_letter: bool,
    /// Whether this is a forced break from a `<br>`. When `true`, `text` is `"\n"` and
    /// `node` is the `<br>` element itself (so the empty line's height comes from its computed style).
    pub is_forced_break: bool,
    /// An atomic box for `display: inline-block`. When `Some`, `text` is empty and this span
    /// represents "one box, not text".
    pub atomic: Option<Box<LayoutBox>>,
    /// The href of the `<a href>` enclosing this text. Many runs are generated under the
    /// same link, so it is shared through an `Rc`.
    pub link: Option<Rc<str>>,
    /// The `background-color` of the inline element enclosing this text (`<mark>`,
    /// `<span>` and so on). Transparent when there is none.
    ///
    /// A text node's computed style clones even the parent's non-inherited properties
    /// (background colour included) in `style::computed::compute_recursive`, so using
    /// `styles[&span.node].background_color` would paint the block's background as an
    /// inline background. So at span construction we extract just "the background specified
    /// by the nearest inline element within the IFC" and carry it here.
    pub background_color: RgbaColor,
}

impl InlineSpan {
    /// An ordinary text run (with no decoration from an enclosing inline element).
    fn text(node: NodeId, text: String) -> Self {
        Self::text_in_inline_context(node, text, &InlineContext::default())
    }

    /// An ordinary text run (carrying information inherited from an enclosing inline element).
    fn text_in_inline_context(node: NodeId, text: String, context: &InlineContext) -> Self {
        Self {
            node,
            text,
            is_first_letter: false,
            is_forced_break: false,
            atomic: None,
            link: context.link.clone(),
            background_color: context.background_color,
        }
    }

    /// An atomic box for `display: inline-block`.
    fn atomic(node: NodeId, atomic: LayoutBox) -> Self {
        Self {
            node,
            text: String::new(),
            is_first_letter: false,
            is_forced_break: false,
            atomic: Some(Box::new(atomic)),
            link: None,
            background_color: RgbaColor::TRANSPARENT,
        }
    }

    /// A forced break from a `<br>`. Setting `text` to `"\n"` lets the
    /// `white-space: pre` path handle the forced break unchanged
    /// (`layout::inline::layout_pre_content` splits lines on `'\n'`).
    fn forced_break(node: NodeId) -> Self {
        Self {
            node,
            text: "\n".to_string(),
            is_first_letter: false,
            is_forced_break: true,
            atomic: None,
            link: None,
            background_color: RgbaColor::TRANSPARENT,
        }
    }
}

/// Information carried down while descending an inline formatting context
/// (coming from the enclosing inline elements, and not recoverable from a text node's
/// computed style).
#[derive(Debug, Clone)]
struct InlineContext {
    /// The href of the nearest `<a href>`.
    link: Option<Rc<str>>,
    /// The background colour specified by the nearest inline element.
    background_color: RgbaColor,
}

impl Default for InlineContext {
    fn default() -> Self {
        Self {
            link: None,
            background_color: RgbaColor::TRANSPARENT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChildKind {
    /// Generates no box, as with `display: none`.
    None,
    Block,
    Inline,
    /// A whitespace-only text node. It only means something when sandwiched between inline
    /// content (where it collapses into an inter-word space). Between blocks, or before or
    /// after inline content, it is discarded (CSS2.1 9.2.2.1).
    Whitespace,
}

pub fn build_box_tree(dom: &Dom, styles: &HashMap<NodeId, Rc<ComputedStyle>>) -> LayoutBox {
    let child_ids: Vec<NodeId> = dom.children(dom.document()).collect();
    LayoutBox::anonymous(BoxContent::Blocks(build_children_boxes(
        dom, styles, &child_ids, 1,
    )))
}

/// Build a [`LayoutBox`] from `node` alone (and its descendants). This is internal to
/// `build_box_tree`'s walk of the whole document, but streaming calls it directly to build
/// a `LayoutBox` for "just the one top-level element that was cut out"
/// (targeting that specific `node` rather than walking every child of `dom.document()` as
/// `build_box_tree` does).
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
        // `::before`/`::after` are unsupported on an element with block children (a
        // simplification), the combination with the anonymous box rules being complicated.
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
        // A `Vec` reserves room for at least 4 elements on its first push. In a document with
        // many boxes holding a single text (table cells, say) that slack simply accumulates.
        spans.shrink_to_fit();
        BoxContent::Inline(spans)
    };

    Some(LayoutBox::for_node(node, content))
}

/// Called after the box tree is built, this replaces the boxes corresponding to `<img>`
/// elements (treated as blocks by `child_kind`, and holding an empty
/// `BoxContent::Inline(vec![])` at this point) with a real [`BoxContent::Image`].
///
/// `image_cache` does the fetching and decoding (which involves I/O). The same `src` is
/// memoised inside `image_cache`, so even a repeatedly referenced image is fetched and
/// decoded only the first time.
pub fn resolve_images(tree: &mut LayoutBox, dom: &Dom, image_cache: &ImageAssetCache) {
    if let Some(node) = tree.node {
        if let NodeData::Element { name, .. } = &dom.node(node).data {
            if &*name.local == "img" {
                tree.content = BoxContent::Image(build_image_box_content(dom, node, image_cache));
                return; // <img> is a void element (it has no children), so no recursion is needed.
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
        // Descend into the atomic boxes that take part in a line (an inline `<img>` and
        // `display: inline-block`). Without this, an inline image would always count as a failed fetch.
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

/// Build a side map letting the decoded image of an element with a `background-image` be
/// looked up by `NodeId`. Unlike [`resolve_images`] for `<img>`, it changes nothing inside
/// the box tree (`LayoutBox`), a background image being draw-time-only information that does
/// not affect layout sizing. It needs no second walk of the DOM tree either: filtering the
/// already-cascaded `styles` by `background_image.is_some()` is enough.
///
/// An element whose fetch or decode failed is simply left out of the map and treated as
/// having no background image (the same fallback policy as 0014, never stopping the whole document).
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

/// `list_item_start` is the starting value when counting `display: list-item` children
/// within this list of child boxes (the HTML `<ol start="N">` attribute; 1 when absent).
/// The unit of this call (that is, one container's direct children) is exactly the counter's
/// scope (a nested `<ol>`/`<ul>` becomes its own call, so as a side effect it starts
/// counting from 1 again).
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

/// Add a whitespace-only text node (`ChildKind::Whitespace`) to the span list.
///
/// In `<span>one</span> <span>two</span>` the whitespace between the inline elements means
/// something as an inter-word space (collapsed to one during line layout), so it must be
/// kept as a span rather than discarded. Where there is no preceding inline content, on the
/// other hand (right after a block, or at the start of the parent), the whitespace would
/// only end up at the start of a line and change nothing once collapsed, so it is not added.
/// [`flush_pending_spans`] guarantees that a run of nothing but whitespace creates no anonymous box.
///
/// Under `white-space: pre`, though, leading whitespace survives as written (it is
/// meaningful as indentation), so this thinning must not happen. That matters for content
/// starting with a whitespace-only text node, as in `<pre>   <b>x</b>y</pre>`.
fn push_collapsible_whitespace(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    // `white-space` is inherited, so the parent's value is already in the text node's own
    // computed style.
    let preserves_leading_whitespace =
        styles.get(&node).map(|s| s.white_space) == Some(WhiteSpace::Pre);
    if out.is_empty() && !preserves_leading_whitespace {
        return;
    }
    // Pass the original text through unchanged (the `white-space: pre` path uses the run of
    // whitespace as written, so it must not be squashed to one here).
    collect_spans(dom, styles, node, out);
}

/// If `node`'s computed style matched `::first-letter` (`first_letter_style` is `Some`),
/// split the first character off the first span in `spans` containing a non-whitespace
/// character and insert it before that span, as a span with `is_first_letter: true`.
///
/// A known simplification: leading whitespace and punctuation are not skipped (simply the
/// first character of the text is used). `spans` covers only the host's direct text content,
/// so it does not apply to content starting inside a nested inline element.
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
        },
    );
}

/// If `node` (the element corresponding to `b`) is `display: list-item`, advance the counter
/// by one and put the marker text on `b`. With `list-style-position: inside` and
/// `BoxContent::Inline` content on `b`, it is embedded at the front as an `InlineSpan`, the
/// same way `::before` is (and `b.marker` stays `None`). Otherwise the text goes on
/// `b.marker` and the actual placement is left to the layout layer (`block.rs`).
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

/// Generate the marker text from `list-style-type`. `None` means no marker
/// (`list-style-type: none`).
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

/// Read the `start` attribute (`<ol start="N">`), treating absent, non-positive and
/// non-numeric values as 1 (the same policy as `read_colspan`/`read_rowspan`).
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

/// Collect the `table-row` elements and the `caption` from `table_node`'s
/// (`display: table`) descendants and assemble a [`TableBox`].
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

/// Collect the column width hints from `<colgroup>`/`<col>` in column index order.
///
/// A `<colgroup>` with `<col>` children expands to those `<col>`s; without them it expands
/// to itself repeated as many times as its `span` attribute says. A `<col>` directly under
/// the table (written without a `<colgroup>`; html5ever inserts an implicit `<colgroup>`,
/// but we look directly under the table defensively) is handled the same way.
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

/// Generate the display text for a form control.
///
/// `<input>` is a void element with no text node, so the text has to be generated from its
/// `value`/`placeholder` attributes. A `<select>` displays the text of the selected
/// `<option>` (the `<option>` itself stays `display: none` under the UA stylesheet).
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
                // A checkbox or radio is drawn as just a frame and a fill.
                "checkbox" | "radio" | "hidden" | "file" | "color" | "range" => None,
                "submit" => Some(attr("value").unwrap_or_else(|| "Submit".to_string())),
                "reset" => Some(attr("value").unwrap_or_else(|| "Reset".to_string())),
                _ => attr("value").or_else(|| attr("placeholder")),
            }
        }
        // For `<select>`, the `<option>` carrying `selected`, or the first `<option>` if there is none.
        "select" => selected_option_text(dom, node),
        _ => None,
    };

    if let Some(text) = text.filter(|t| !t.is_empty()) {
        let mut span = InlineSpan::text(node, text);
        // The generated text is drawn with the element's own computed style (handled like `::before`).
        span.background_color = styles
            .get(&node)
            .map(|s| s.background_color)
            .filter(|c| c.alpha > 0.0)
            .unwrap_or(RgbaColor::TRANSPARENT);
        out.push(span);
    }
}

/// The display text of a `<select>` (the selected `<option>`, or the first one if there is none).
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
            // `<option>`s inside an `<optgroup>` count too.
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

/// Concatenate the text nodes under `node` (dropping leading and trailing whitespace).
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

/// Build the contents of a `display: inline-block` element by the same rules as an ordinary
/// block.
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
        // A `Vec` reserves room for at least 4 elements on its first push. In a document with
        // many boxes holding a single text (table cells, say) that slack simply accumulates.
        spans.shrink_to_fit();
        BoxContent::Inline(spans)
    };

    Some(LayoutBox::for_node(node, content))
}

/// If `node` is an `<a>` element with an `href`, its value.
/// The `javascript:` scheme is not treated as a link.
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

/// Read `<col span>`/`<colgroup span>` (absent, non-positive and non-numeric values are 1,
/// the same leniency as `colspan`).
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

/// Build one flex item per child of the flex container (`node`). Per the CSS spec a flex
/// item is generated independently for each child element, and the rule that wraps adjacent
/// inline-level elements into one anonymous box (`build_children_boxes`) does not apply to
/// a flex container's children, so `build_box_for_element` is called directly per child.
/// A child's own `display` value (`block`, `table`, a nested `flex` and so on) is respected
/// as-is and used for laying out that item's contents (nesting without limit).
///
/// Bare text not wrapped in an element has its consecutive runs gathered into a single
/// anonymous flex item (CSS Flexbox section 4). A run of nothing but whitespace produces no
/// item. A `display: none` child generates no box, so text either side of one counts as
/// contiguous and gathers into a single item.
fn build_flex_box(dom: &Dom, styles: &HashMap<NodeId, Rc<ComputedStyle>>, node: NodeId) -> FlexBox {
    let mut items = Vec::new();
    let mut pending_spans: Vec<InlineSpan> = Vec::new();

    for child in dom.children(node) {
        match &dom.node(child).data {
            NodeData::Element { .. } => {
                if styles.get(&child).map(|s| s.display) == Some(Display::None) {
                    continue;
                }
                // An element always becomes its own item, so the text accumulated so far is
                // settled as an anonymous item first.
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

/// Walk `node`'s children, collecting each `table-row` as a row and recording the first
/// `table-caption` found in `out_caption`. A transparent container such as
/// `thead`/`tbody`/`tfoot` (an element that is neither `table-row`/`table-caption` nor
/// `table`) is passed through and recursed into. A nested `table` is a separate table in
/// itself (its rows belong to the inner table), so it is not recursed into here.
fn collect_table_rows(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<TableRow>,
    out_caption: &mut Option<NodeId>,
) {
    collect_table_rows_in_section(dom, styles, node, TableSection::Body, out, out_caption);
    // HTML4 required `<tfoot>` to be written before `<tbody>`. Now that the section is
    // known, it is moved to the end regardless of source order. The sort is stable, so the
    // order within a section is preserved.
    out.sort_by_key(|row| match row.section {
        TableSection::Head => 0,
        TableSection::Body => 1,
        TableSection::Foot => 2,
    });
}

/// What a child of a table (or of a container such as `<thead>` inside it) counts as within
/// the table structure.
enum TableChild {
    Row,
    Caption,
    /// `<thead>`/`<tbody>`/`<tfoot>`. Transparent containers with no `display` value of their
    /// own, so the rows inside are collected as that section.
    Section(TableSection),
    /// A child that is neither a row nor a section. Something to wrap in an anonymous row or
    /// cell (CSS2.1 17.2.1 rule 2.1).
    Content,
    /// Generates no box (`display: none`, a column specification, a comment and so on).
    Ignored,
}

fn table_child_kind(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
) -> TableChild {
    if !matches!(dom.node(node).data, NodeData::Element { .. }) {
        // A text node counts as content (a whitespace-only one is discarded by
        // `flush_anonymous_row` if its whole run is whitespace). Comments and the like are ignored.
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
            // A box representing a column is never drawn and generates no anonymous box
            // (the width hints are read separately by `collect_column_widths`).
            Some("colgroup") | Some("col") => TableChild::Ignored,
            _ => TableChild::Content,
        },
    }
}

/// The body of [`collect_table_rows`]. `section` is the section indicated by "the container we are currently in".
fn collect_table_rows_in_section(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    section: TableSection,
    out: &mut Vec<TableRow>,
    out_caption: &mut Option<NodeId>,
) {
    // The run of consecutive children that are not rows. On reaching a boundary they are gathered into an anonymous row.
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

/// Gather the accumulated "children that are not rows" into one anonymous `table-row` and
/// push it onto `out` (CSS2.1 17.2.1 rule 2.1). A run of nothing but whitespace is discarded
/// without creating a row (the equivalent of rule 1's "remove irrelevant boxes").
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

/// Whether this is a whitespace-only text node. Whitespace in the gaps of a table structure
/// (between rows or cells) generates no box.
fn is_ignorable_whitespace(dom: &Dom, node: NodeId) -> bool {
    match &dom.node(node).data {
        NodeData::Text { contents } => white_space::is_collapsible_only(contents),
        _ => false,
    }
}

/// Build the cell list from a row's children (`children`). A `display: table-cell` becomes a
/// cell as-is, and any other child is wrapped, run by consecutive run, in an anonymous cell
/// (CSS2.1 17.2.1 rule 2.2).
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

/// Anything that is not an element (a comment and so on) and any column specification generates no box.
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

/// Gather the accumulated "children that are not cells" into one anonymous `table-cell`.
/// A run of nothing but whitespace is discarded without creating a cell.
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

/// Read the `colspan` attribute (absent, non-positive and non-numeric values are treated as 1).
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

/// Read the `rowspan` attribute (absent, non-positive and non-numeric values are treated as 1;
/// the special value `rowspan="0"` is also unsupported and treated as 1, the same policy as `read_colspan`).
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
    // Whitespace-only text creates no anonymous block (CSS2.1 9.2.2.1).
    // An atomic box (an inline `<img>` or `display: inline-block`) is meaningful content even
    // with an empty `text`, though, so one alone is enough to create an anonymous block
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
                // An `inline-block` takes part in the parent's line (its contents are laid
                // out as a block).
                Some(Display::InlineBlock) => ChildKind::Inline,
                Some(Display::Block)
                | Some(Display::Table)
                | Some(Display::ListItem)
                | Some(Display::Flex)
                | Some(Display::Grid) => ChildKind::Block,
                Some(Display::Inline) => ChildKind::Inline,
                // table-row/table-cell/table-caption are searched for specially by
                // `build_table_box`, so they do not appear in the ordinary block/inline walk
                // (unless invalid markup puts them outside a table context).
                // They are ignored defensively.
                Some(Display::TableRow)
                | Some(Display::TableCell)
                | Some(Display::TableCaption) => ChildKind::None,
                Some(Display::None) | None => ChildKind::None,
            }
        }
        NodeData::Text { contents } => {
            // A text node of nothing but `&nbsp;` is not "whitespace only" (it has
            // non-collapsing content), so the CSS classification decides, not `str::trim`.
            if white_space::is_collapsible_only(contents) {
                ChildKind::Whitespace
            } else {
                ChildKind::Inline
            }
        }
        _ => ChildKind::None,
    }
}

/// Walk an inline element's descendants recursively and push an [`InlineSpan`] per text node.
/// The cascade and inheritance from ancestor inline elements (`<b>`, `<span>` and so on) are
/// already reflected in the text node's own computed style, so only the node ID needs keeping here.
/// Each inline element's `::before`/`::after` generated content is also inserted as spans
/// before and after the corresponding descendants.
fn collect_spans(
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    node: NodeId,
    out: &mut Vec<InlineSpan>,
) {
    collect_spans_in_context(dom, styles, node, &InlineContext::default(), out)
}

/// The body of [`collect_spans`]. `context` is "what is inherited from the inline elements
/// enclosing this node" (settings from outside the IFC, on the block side, are not included).
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
            // Make `display: none` take effect on inline-context descendants too.
            // `child_kind` is only called when sorting into block and inline, so without
            // checking here the descendant text of a hidden element inside an inline element
            // (`<p>a <select><option>x</option></select> b</p>`, say) would leak into the
            // body (the premise of the UA stylesheet's hiding).
            if styles.get(&node).map(|s| s.display) == Some(Display::None) {
                return;
            }
            // `<br>` is a forced-break marker with no children.
            if &*name.local == "br" {
                out.push(InlineSpan::forced_break(node));
                return;
            }
            // `<wbr>` is a childless "a break is allowed here" marker (a line break
            // opportunity, in HTML spec terms). Placing one ZWSP gives exactly that meaning:
            // a zero-width break opportunity (`layout::white_space` treats it as one).
            // Browsers implement it the same way.
            if &*name.local == "wbr" {
                out.push(InlineSpan::text_in_inline_context(
                    node,
                    white_space::ZERO_WIDTH_SPACE.to_string(),
                    context,
                ));
                return;
            }
            // An inline `<img>` (a replaced element) also takes part in the line as one box.
            // Its contents are swapped for `BoxContent::Image` later by `resolve_images`.
            if &*name.local == "img" {
                out.push(InlineSpan::atomic(
                    node,
                    LayoutBox::for_node(node, BoxContent::Inline(Vec::new())),
                ));
                return;
            }
            // `display: inline-block` takes part in the line as one box. Its contents are
            // built by the same rules as an ordinary block.
            if styles.get(&node).map(|s| s.display) == Some(Display::InlineBlock) {
                if let Some(mut atomic) = build_inline_block_box(dom, styles, node) {
                    atomic.marker = None;
                    out.push(InlineSpan::atomic(node, atomic));
                }
                return;
            }
            // If this inline element has a background colour of its own, the descendants that
            // follow are painted with it (when nested, the inner one wins; a simplification
            // of CSS background layering). An `<a href>` link is likewise carried down to the descendants.
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
            push_before_content(styles, node, out);
            for child in dom.children(node) {
                collect_spans_in_context(dom, styles, child, &context, out);
            }
            push_after_content(styles, node, out);
        }
        _ => {}
    }
}

/// If `node` has `::before` generated content, push its spans along with the node ID used to
/// look up the computed style (`node` itself).
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

/// If `node` has `::after` generated content, push its spans along with the node ID used to
/// look up the computed style (`node` itself).
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
        // The default display of `<img>` is inline, so it sits in the same inline box as the
        // text, as an atomic box.
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
        // Regression test: `flush_pending_spans` used to discard the anonymous block when
        // "the text is whitespace only", losing a bare `<img>` between `<p>` siblings.
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
        // An `<img>` with an explicit `display: block` is still a block replaced element
        // (it does not become an atomic span).
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
        // The bold text's span comes from the child text node of <b> and has a different
        // NodeId from the text directly under <p> (so it looks up a different computed style).
        assert_ne!(spans[0].node, spans[1].node);
        assert_eq!(dom.children(b).next(), Some(spans[1].node));
    }

    /// Concatenate and return the text of the (first) `<p>`'s span list.
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
        // Regression test (issue #3): whitespace-only text nodes were discarded across the
        // board, so the inter-word space between inline elements vanished, giving `onetwo`.
        assert_eq!(
            first_p_text(br#"<p><span>one</span> <span>two</span></p>"#),
            "one two"
        );
    }

    #[test]
    fn whitespace_between_inline_elements_is_kept_across_a_whole_run() {
        // With three or more in a row, every space between them survives.
        assert_eq!(
            first_p_text(br#"<p><b>one</b> <i>two</i> <span>three</span></p>"#),
            "one two three"
        );
    }

    #[test]
    fn a_newline_between_two_inline_elements_is_kept() {
        // The newlines common in formatted markup also survive as inter-word spaces
        // (collapsing them to one is the line layout's job, in `layout::inline`).
        assert_eq!(
            first_p_text(b"<p><span>one</span>\n  <span>two</span></p>"),
            "one\n  two"
        );
    }

    #[test]
    fn a_non_breaking_space_between_two_inline_elements_is_kept() {
        // `&nbsp;` makes `char::is_whitespace` true, so it used to be discarded along with
        // the "whitespace-only text nodes".
        assert_eq!(
            first_p_text("<p><span>one</span>\u{a0}<span>two</span></p>".as_bytes()),
            "one\u{a0}two"
        );
    }

    #[test]
    fn whitespace_before_the_first_inline_child_creates_no_span() {
        // Whitespace that only ends up at the start of a line, changing nothing, creates no
        // span (so formatted markup does not pile up pointless spans). At the end it may
        // remain, line layout ignoring it (and it means something under `white-space: pre`).
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
        // Under `white-space: pre`, leading whitespace means something as indentation, so it
        // must not be discarded even when the content starts with a whitespace-only text node
        // (when it was discarded, `<pre>   <b>x</b>y</pre>` came out as `xy`).
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
        // `<wbr>` expresses only "a break is allowed here". Placing one ZWSP puts it on the
        // break opportunity rules of `layout::white_space`.
        let dom = html::parse(br#"<p>aaa<wbr>bbb</p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        let tree = build_box_tree(&dom, &styles);

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_box(&tree, p).expect("p box not found");
        let spans = find_inline_spans(p_box).expect("expected inline content");

        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_eq!(text, "aaa\u{200b}bbb");
        // Unlike `<br>` it is not a forced break (it only adds a break *opportunity*).
        assert!(spans.iter().all(|span| !span.is_forced_break));
    }

    #[test]
    fn whitespace_between_block_siblings_creates_no_anonymous_box() {
        // Whitespace between blocks still generates no box, as before (CSS2.1 9.2.2.1).
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
        // <span> is an inline element, so on its own it has no LayoutBox and is woven into
        // the flattened span list of an ancestor block container (here <body>).
        // ::before/::after should still be inserted correctly either side of it.
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
        // A generated-content span carries the host element's own node ID
        // (that is, it reuses the host's computed style).
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
        // CSS2.1 17.2.1 rule 2.1: consecutive `table-cell`s directly under a `table` gather
        // into one anonymous `table-row`.
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
        // CSS2.1 17.2.1 rule 2.2: children that are not cells are wrapped, run by consecutive
        // run, in one anonymous `table-cell`.
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
        // A nested table's <tr> belongs to the inner table and should not be collected as a
        // row of the outer table.
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
        // The outer table's single cell holds a block container (containing the inner table),
        // and the inner table's rows should not have slipped in.
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
        // `resolve_background_images` should build the side map without walking the DOM tree
        // again, just by filtering the already-cascaded `styles` on
        // `background_image.is_some()`.
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
        // The third `li` has a block child (the nested `ol`), so it carries only the marker
        // itself (its content is `BoxContent::Blocks`).
        assert_eq!(
            find_box(&tree, lis[2]).unwrap().marker.as_deref(),
            Some("3.")
        );
        // The nested `ol` has its own counter scope and counts from 1 again.
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
        // A `none` item still consumes one counter step (matching what browsers really do).
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
        // `inside` embeds into the spans, so the `marker` field itself stays `None`.
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
        // The split first-character span carries the host element's (p's) own node ID, so it can look up the ::first-letter style.
        assert_eq!(spans[0].node, p);
        assert_eq!(spans[1].text, "ello world");
        assert!(!spans[1].is_first_letter);
        // The remainder keeps the original text node's ID (unchanged by the split).
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
        // Regression test: text not wrapped in an element used to be discarded, losing the
        // contents of something like `<div class="seal">sample</div>`.
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
        // An element always becomes its own item, so the text either side of it splits into
        // separate anonymous items.
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
        // A `display: none` child creates no box, so text either side of one counts as
        // contiguous and gathers into a single item.
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
        // grid collects its items with `build_flex_box` too, so the same rules apply.
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

    /// The height memo is keyed on the pair of content width and containing width.
    /// Percentages inside resolve against the containing width, so the same content width
    /// must not be confused across different containing widths (in a nested flex/grid the
    /// same item is measured many times at different containing widths).
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
