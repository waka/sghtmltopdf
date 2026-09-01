//! Splitting the laid-out box tree according to the height left on the page.
//!
//! It honours `break-before`/`break-after`/`break-inside`/`orphans`/`widows`, and where a
//! box does not fit on the page it tries to split it in this order of preference:
//! 1. If `break-inside: avoid` and it is small enough to fit a whole page, move it whole to
//!    the top of the next page rather than splitting it
//! 2. If it is a block container, re-place it child box by child box (each child's
//!    `break-before`/`break-after: always` acts as a forced break at this granularity)
//! 3. If it is multi-line inline content, split it line box by line box
//!    ([`compute_orphans_widows_breaks`] adjusts the break points in advance so
//!    `orphans`/`widows` are satisfied)
//! 4. Anything still indivisible (an empty element, single-line content) is moved whole to
//!    the top of the next page (something too large for one page simply overflows)
//!
//! A descendant's `break-before`/`break-after: always` must not be missed even when the
//! ancestor's subtree fits in the height left on the page (a forced break is an explicit
//! setting, independent of overflow). [`subtree_requires_child_walk`] decides in advance
//! whether such a forced break exists inside the subtree, and if it does, falls back to
//! per-child placement rather than the fast path of "place it as a single leaf".
//!
//! Even where a container is itself split across pages, its background and borders are
//! reproduced on each page its children actually landed on (a simple box fragmentation; see
//! [`place_split`]). "Given break points already decided, how the container's decoration
//! carries over" follows these simple rules:
//! - The top margin, border and padding apply to the first fragment only
//! - The bottom margin, border and padding apply to the last fragment only
//! - The left and right borders and padding apply to every fragment
//! - The background colour is painted over each fragment's real content extent
//!
//! A subtree that fits on one page without crossing a boundary, and contains no forced
//! break, is placed with its original structure intact.

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

/// The state managing "the unflushed page buffer plus the decision of what may be flushed" during pagination.
///
/// The decoration fragments (background and borders) that [`place_split`] inserts for a
/// container are written after every one of its children has been placed, reaching back into
/// pages that were already `push`ed and look settled (see the module docs). So the simple
/// rule "once a new page starts, the previous one is settled" does not hold. The document
/// root itself also goes through `place_split`, so with no countermeasure "no page is
/// settled until the whole document is processed" (which is effectively batch processing
/// again).
///
/// Instead, every `place_split` currently in progress (that is, on the call stack) pushes
/// "the first absolute page index it touched" onto the `active_min_page` stack, and only
/// pages before that minimum are safely flushed ([`Self::try_flush`]). No container started
/// after a given page ever writes back to it (`place_split` never touches a page before the
/// range it spanned), which is what makes this rule safe.
///
/// The persistent part of [`PaginationState`]. Holding no `on_flush` callback (with its
/// lifetime), it can be a field of [`StreamingPaginator`] and survive across several
/// `push_item` calls.
#[derive(Default)]
struct PaginationBuffer {
    /// The buffer of pages not yet flushed (that is, pages we cannot yet guarantee will not
    /// be written to again). The absolute index of `buffer[0]` is `flushed`.
    buffer: Vec<Page>,
    /// How many pages have been flushed so far (that is, the absolute index of the next page
    /// to flush).
    flushed: usize,
    /// The stack of first-touched absolute page indices for the `place_split` calls currently
    /// active (pushed and popped by `enter_split`/`exit_split`).
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

/// [`PaginationBuffer`] (the persistent part) plus a per-call replaceable `on_flush`
/// callback: what `place_box` and friends actually operate on.
///
/// The decoration fragments (background and borders) that [`place_split`] inserts for a
/// container are written after every one of its children has been placed, reaching back into
/// pages that were already `push`ed and look settled (see the module docs). So the simple
/// rule "once a new page starts, the previous one is settled" does not hold. The document
/// root itself also goes through `place_split`, so with no countermeasure "no page is
/// settled until the whole document is processed" (which is effectively batch processing
/// again).
///
/// Instead, every `place_split` currently in progress (that is, on the call stack) pushes
/// "the first absolute page index it touched" onto the `active_min_page` stack, and only
/// pages before that minimum are safely flushed ([`Self::try_flush`]). No container started
/// after a given page ever writes back to it (`place_split` never touches a page before the
/// range it spanned), which is what makes this rule safe.
struct PaginationState<'a> {
    inner: &'a mut PaginationBuffer,
    on_flush: &'a mut dyn FnMut(Page),
}

impl<'a> PaginationState<'a> {
    fn new(inner: &'a mut PaginationBuffer, on_flush: &'a mut dyn FnMut(Page)) -> Self {
        Self { inner, on_flush }
    }

    /// The current page count, in absolute indices (0-based and running through the whole
    /// document).
    fn len(&self) -> usize {
        self.inner.flushed + self.inner.buffer.len()
    }

    /// The absolute index of the last (most recent) active page.
    fn current_index(&self) -> usize {
        self.len() - 1
    }

    fn last_mut(&mut self) -> &mut Page {
        self.inner
            .buffer
            .last_mut()
            .expect("the buffer always holds at least one page")
    }

    fn last(&self) -> &Page {
        self.inner
            .buffer
            .last()
            .expect("the buffer always holds at least one page")
    }

    /// A reference to the page at absolute index `absolute`. The caller guarantees it has
    /// not been flushed yet (that is, it is still in the buffer)
    /// (only the range protected by `active_min_page` is ever accessed).
    fn get(&self, absolute: usize) -> &Page {
        &self.inner.buffer[absolute - self.inner.flushed]
    }

    fn get_mut(&mut self, absolute: usize) -> &mut Page {
        &mut self.inner.buffer[absolute - self.inner.flushed]
    }

    /// Start a new page.
    fn push_new_page(&mut self) {
        self.inner.buffer.push(Page::default());
    }

    /// On entering [`place_split`], record the current page as "the first page this container
    /// touched".
    fn enter_split(&mut self) {
        let idx = self.current_index();
        self.inner.active_min_page.push(idx);
    }

    /// On leaving [`place_split`], remove the corresponding record and pass any flushable
    /// pages to `on_flush`.
    fn exit_split(&mut self) {
        self.inner.active_min_page.pop();
        self.try_flush();
    }

    /// With no active `place_split`, pass every page but the most recent to `on_flush`,
    /// oldest first; with one, pass every page before the minimum of "the first page each
    /// currently active container touched".
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

/// Split `root` (normally the return value of [`super::layout_document`]) into pages of
/// height `page_content_height` (the batch version). Internally a thin wrapper that has
/// [`paginate_streaming`] pile every page into a `Vec`.
pub fn paginate(root: &mut LaidOutBox, page_content_height: f32) -> Vec<Page> {
    let mut result = Vec::new();
    paginate_streaming(root, page_content_height, &mut |page| result.push(page));
    result
}

/// The streaming version of [`paginate`]. `on_page` is called as each page is settled.
///
/// See the documentation of [`PaginationState`] for what "settled" means. The intended use
/// is for the caller to free the DOM subtree corresponding to that page inside `on_page`,
/// via [`crate::html::Dom::release_subtree`].
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

/// A streaming paginator: several `LaidOutBox`es (normally one per top-level block element
/// of the document) are added in turn with [`Self::push_item`], and [`Self::finish`] flushes
/// the remaining pages.
///
/// [`paginate_streaming`] assumes "one complete `LaidOutBox` tree" processed at once, while
/// `StreamingPaginator` can continue ordinary top-to-bottom pagination (`cursor` and the
/// [`PaginationBuffer`] flush decision included) across several calls. The intended use, with
/// genuinely streaming input, is to lay out each top-level element directly under `<body>`
/// on its own with `layout::layout_document_from` as it becomes final, and `push_item` it.
///
/// The `on_page` callback is not held as a field: `push_item`/`finish` return the settled
/// pages as a `Vec<Page>` instead (giving the struct the callback's lifetime would cause
/// self-referential borrowing problems for a use case such as `Engine`, which wants to hold
/// the struct itself across several calls).
pub struct StreamingPaginator {
    buffer: PaginationBuffer,
    cursor: f32,
    page_height: f32,
    /// Whether the item added just before had `break-after: always`.
    /// It is consumed as a page break when the next item is added.
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

    /// Add one item. Returns the pages this call settled.
    ///
    /// The item's own `break-before`/`break-after` are handled here: `place_box` only looks
    /// at forced breaks within a child list, and the relationship between items (that is,
    /// siblings directly under `<body>`) exists only on the paginator's side. It matches the
    /// decision [`place_split`] makes for siblings in the batch version.
    pub fn push_item(&mut self, item: &mut LaidOutBox) -> Vec<Page> {
        let break_before =
            self.pending_break_after || item.fragmentation.break_before == BreakBetween::Always;
        // `break-after` means "break the page before the next sibling", so it is only recorded
        // here and consumed by the next `push_item`. The last item's `break-after` is ignored
        // by `finish`, so no empty page is left at the end.
        self.pending_break_after = item.fragmentation.break_after == BreakBetween::Always;

        let mut flushed = Vec::new();
        {
            let mut on_flush = |page: Page| flushed.push(page);
            let mut state = PaginationState::new(&mut self.buffer, &mut on_flush);
            // With nothing placed on the current page, breaking would only add an empty page,
            // so nothing is done (a `break-before` on the first element, say).
            if break_before && current_page_has_content(&state) {
                new_page(&mut state, &mut self.cursor);
            }
            place_box(item, self.page_height, &mut state, &mut self.cursor);
        }
        flushed
    }

    /// Signal that there are no more items and return every remaining page.
    pub fn finish(self) -> Vec<Page> {
        debug_assert!(
            self.buffer.active_min_page.is_empty(),
            "finish should be called after leaving every place_split call"
        );
        self.buffer.buffer
    }
}

/// Do everything from box tree construction through layout to pagination in one go, from the DOM plus the computed styles.
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
    // The box tree is finished with once layout is done. Pagination holds both the layout
    // result and the pages, so letting go of it here keeps the peak from spiking.
    drop(tree);
    let mut laid_out = laid_out;
    let mut pages = paginate(&mut laid_out, settings.content_height());
    apply_positioned_overlays(&mut pages, &positioned);
    pages
}

/// Add the absolutely positioned boxes ([`PositionedBox`]) to the pages they belong to as an
/// overlay. They are appended to `page.boxes`, so they are drawn above (in front of) the normal flow.
pub(crate) fn apply_positioned_overlays(pages: &mut [Page], positioned: &[PositionedBox]) {
    for pb in positioned {
        match pb.kind {
            // `fixed` goes in the content area of every page, at its layout coordinates.
            PositionedKind::Fixed => {
                for page in pages.iter_mut() {
                    page.boxes.push(pb.laid.clone());
                }
            }
            // An `absolute` with no positioned ancestor goes on the first page.
            PositionedKind::AbsoluteInitial => {
                if let Some(first) = pages.first_mut() {
                    first.boxes.push(pb.laid.clone());
                }
            }
            // An `absolute` with a positioned ancestor goes on the page where the ancestor
            // appeared, offset by the difference between the ancestor's padding box position on the page and at layout time.
            PositionedKind::AbsoluteAncestor {
                node,
                padding_box_origin,
            } => {
                if let Some((idx, (px, py))) = find_ancestor_padding_box_origin(pages, node) {
                    let dx = px - padding_box_origin.0;
                    let dy = py - padding_box_origin.1;
                    // `shift_box_y`'s delta is an amount to subtract, so moving down means `-dy`.
                    let shifted = shift_box_x(&shift_box_y(&pb.laid, -dy), dx);
                    pages[idx].boxes.push(shifted);
                }
            }
        }
    }
}

/// Find the page where `node` first appears, and the within-page coordinates of the top left of its padding box.
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

/// For `Mode::Batch`: settle every page, then overlay the absolute positioning and return.
/// Duplicating `fixed` onto every page and resolving an `absolute`'s ancestor page both
/// require every page to be present, so no streaming release is performed
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

/// The streaming version of [`paginate_document`]. As each page is settled, the DOM subtrees
/// that fitted entirely on it (and will not be split further) are freed with
/// [`Dom::release_subtree`] before `on_page` is called.
///
/// In the current pipeline (`compute_styles`, `build_box_tree` and `layout_document` all
/// read the whole DOM at once), style computation and layout are both already complete at
/// this point, and no page's processing reads `dom` again after this.
/// The "never cross the range a sibling or descendant selector can see" constraint is
/// therefore always satisfied here (no later, unparsed elements exist).
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

/// Free the DOM subtrees corresponding to the boxes in `page` that will not be split further
/// (`FragmentPosition::Whole` or `Last`), via
/// [`Dom::release_subtree`].
fn release_completed_subtrees(dom: &mut Dom, page: &Page) {
    for root in collect_completed_subtree_roots(page) {
        dom.release_subtree(root);
    }
}

/// Collect the root nodes of the DOM subtrees corresponding to the boxes in `page` that will
/// not be split further (`FragmentPosition::Whole` or `Last`).
///
/// The same "nodes no later page will reference" decision is needed not only by
/// [`release_completed_subtrees`] (freeing the DOM) but also when `Engine` removes entries
/// it no longer needs from the `ComputedStyle` map, so the logic for walking `page` is
/// factored out on its own.
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
            // The caller walks everything under this node recursively, so no recursion into the children is needed.
            roots.push(node);
            return;
        }
    }
    // A container that is not yet complete (whose decoration fragment is `First`/`Middle`)
    // cannot itself count as complete, but boxes where children were really placed (children
    // placed directly on that page, as distinct from the decoration fragments `place_split`
    // generates) may be complete independently, so we recurse.
    match &b.content {
        LaidOutContent::Blocks(children) => {
            for child in children {
                collect_completed_subtree_roots_in_box(child, roots);
            }
        }
        // A flex container is atomic with respect to pagination (handled like
        // `display: table`). A grid splits per row, but per-fragment completeness is expressed
        // by `place_grid` through `FragmentPosition`, so as with a table we do not recurse here.
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

    // `break-inside: avoid`: if it is small enough to fit a whole (empty) page and contains no
    // forced break, move it whole to the top of the next page rather than splitting it.
    // Something too large for one page is exempt and is split as usual on a best-effort
    // basis (an exception to avoid an infinite loop or an impossible output).
    // If the current page has no real content on it (including the case where only an
    // ancestor's margin has advanced `cursor`), moving it is pointless, so it is placed on
    // the current page.
    if break_inside_avoid
        && current_page_has_content(state)
        && height <= page_height
        && !has_forced_break_inside
    {
        new_page(state, cursor);
        place_leaf(b, state, cursor);
        return;
    }

    // While the children are handed out by `&mut`, the container's own scalar information is kept aside.
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
                margin_box_top,
                |child, ph, ps, c| {
                    place_box(child, ph, ps, c);
                },
            );
            return;
        }
        // A table splits per row. Without this, a row that does not fit the page is lost
        // undrawn.
        LaidOutContent::Table(table) if !table.rows.is_empty() => {
            place_table(&container, table, page_height, state, cursor);
            return;
        }
        // A grid splits per row band.
        LaidOutContent::Grid(grid) if grid.rows.len() > 1 => {
            place_grid(&container, grid, page_height, state, cursor);
            return;
        }
        LaidOutContent::Inline(lines) if lines.len() > 1 => {
            // To satisfy `orphans`/`widows`, compute the forced break positions per line in
            // advance (simulating the natural splitting caused by overflow). Unless the top
            // margin, border and padding `place_split` adds (`container_top_extra`) are
            // reflected in the simulation's initial cursor too, the break points diverge from
            // the real placement.
            let initial_cursor = *cursor + container.top_extra();
            let forced_breaks =
                compute_orphans_widows_breaks(lines, orphans, widows, page_height, initial_cursor);
            place_split(
                &mut container,
                lines,
                page_height,
                state,
                cursor,
                // A line box has no `break-after` (the next place is always the line
                // immediately after, and there is no cross-container sibling relationship).
                move |i, _line| (forced_breaks[i], false),
                // A line has no concept of a float.
                |_line: &LineBox| false,
                |_line: &LineBox| 0.0,
                |line, ph, ps, c| {
                    place_line(line, ph, ps, c);
                },
            );
            return;
        }
        _ => {}
    }

    // We did not take a splitting path, so the marker taken away is given back and it is placed as an indivisible unit.
    b.marker = container.marker.take();
    // An indivisible unit. If any of the page has been used, move it to the top of the next
    // page (on a blank page it is placed where it is).
    if *cursor > 0.0 {
        new_page(state, cursor);
    }
    place_leaf(b, state, cursor);
}

/// The absolute Y of the top of `b`'s margin box (its `content.y` minus the margin, border
/// and padding). Shared by `place_leaf`/`extent_of` and `place_split`'s float branch.
fn margin_box_top(b: &LaidOutBox) -> f32 {
    b.layout.content.y - b.layout.padding.top - b.layout.border.top - b.layout.margin.top
}

/// Whether any box inside `b`'s subtree (block descendants only; inline lines and table
/// internals are excluded) has `break-before`/`break-after: always`.
///
/// When this is `true`, the fast path of "place it as a single leaf" cannot be used even
/// where `b` itself fits the height left on the page (it would miss the forced break's
/// position). A table's internal rows and inline lines are not split here, so only `Blocks`
/// is recursed into.
fn subtree_requires_child_walk(b: &LaidOutBox) -> bool {
    match &b.content {
        LaidOutContent::Blocks(children) => children.iter().any(|child| {
            child.fragmentation.break_before == BreakBetween::Always
                || child.fragmentation.break_after == BreakBetween::Always
                || subtree_requires_child_walk(child)
        }),
        // A flex container is atomic. Splitting a grid by row is `place_grid`'s job, so we do
        // not recurse here.
        LaidOutContent::Inline(_)
        | LaidOutContent::Table(_)
        | LaidOutContent::Flex(_)
        | LaidOutContent::Grid(_)
        | LaidOutContent::Image(_) => false,
    }
}

/// When splitting multi-line inline content line by line, compute in advance whether a
/// forced break should be inserted before each line so that `orphans`/`widows` are
/// satisfied. In the return value `v`, `v[i] == true` means "break the page before
/// `lines[i]`" (passed straight to `place_split`'s `break_hints`).
///
/// It simulates the natural break points caused by overflow across all of `lines` (the same
/// decision `place_line` really makes: `cursor > 0.0 && cursor + line.height > page_height`)
/// and checks at each of them whether `orphans` (the lines left on this page) and `widows`
/// (the lines moved to the next) are sufficient:
/// - If both are satisfied, that natural break point is taken as-is (no extra marker is
///   needed, `place_line` itself breaking on the same decision)
/// - If `orphans` falls short, no more lines can physically be fitted onto this page, so all
///   the lines that were to go on it are moved to the next page (a forced break at the top
///   of this page)
/// - If `orphans` is sufficient but `widows` is not, the break point is brought forward to
///   `lines.len() - widows` (securing `widows` by sending lines not yet placed to the next page)
///
/// - If `orphans` still cannot be satisfied after bringing it forward, everything is moved
///   to the next page as in the `orphans`-shortfall case
/// - If the conditions cannot be met again at the break point immediately after such a whole
///   move (a run of lines so tall that one alone takes most of a page, say), the natural
///   break point is accepted on a best-effort basis to avoid an infinite loop (giving up on `orphans`/`widows`)
///
/// A paragraph too short to have `orphans + widows` lines cannot satisfy both at any break
/// point, so it ends up moved whole to the next page (if the current page has real content).
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
            // Take the natural break point as-is (no marker needed).
            page_start = i;
            cursor = 0.0;
            continue;
        }

        if force_break_before[page_start] {
            // To avoid an infinite loop, natural break points are accepted best-effort from here on.
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

        // orphans_ok == true, widows_ok == false: try bringing the break point forward to
        // secure widows. If orphans still cannot be satisfied afterwards, move everything to
        // the next page as in the orphans-shortfall case.
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

/// `b` does not fit on one page (or contains a forced break), so it is split and placed per
/// child (`items`, placed one at a time by `place_one`). After splitting, decoration
/// fragments are inserted to reproduce `b`'s own background and borders over each page's real
/// content extent.
///
/// `items` is either `LaidOutBox` (block children) or [`LineBox`] (inline lines).
/// `break_hints` is a callback returning, for each element (and its index),
/// `(is a forced break needed before it, is one needed after it)` (lines have no concept of
/// `break-before`/`break-after`, so the caller passes a callback indexing into an array
/// precomputed from `orphans`/`widows`).
///
/// `is_float`/`item_margin_box_top` are callbacks for identifying out-of-flow elements
/// (`float`) among `items` (the `LineBox` side passes dummy implementations always returning
/// `false`/`0.0`; a line has no concept of a float). A float item does not change the shared
/// `cursor`; instead `place_one` is recursed into with a temporary cursor seeded from
/// `shift_reference` (the absolute-Y to within-page-Y conversion offset)
/// (`place_leaf`/`place_line`/`new_page` are left completely unchanged for this branch).
#[allow(clippy::too_many_arguments)]
/// Just the information needed to generate the decoration fragments, extracted from the container being split.
///
/// Because the children (`items`) are handed out by `&mut` while other fields of the same
/// box are still being read, the scalars are copied out first so the borrows do not conflict.
struct SplitContainer {
    node: Option<NodeId>,
    layout: Layout,
    has_visible_decoration: bool,
    /// The marker moves to the first fragment. `place_split` takes it and uses it.
    marker: Option<Box<LineBox>>,
}

impl SplitContainer {
    /// Extract the scalar information from `b` (taking ownership of the marker).
    fn take_from(b: &mut LaidOutBox) -> Self {
        Self {
            node: b.node,
            layout: b.layout,
            has_visible_decoration: b.has_visible_decoration,
            marker: b.marker.take(),
        }
    }

    /// The sum of the container's own top margin, border and padding.
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
    item_margin_box_top: impl Fn(&T) -> f32,
    place_one: impl Fn(&mut T, f32, &mut PaginationState<'_>, &mut f32),
) {
    let top_extra = container.top_extra();
    let bottom_extra = container.layout.padding.bottom
        + container.layout.border.bottom
        + container.layout.margin.bottom;

    // Before the first fragment, reserve space for the container's own top margin, border and
    // padding (no adjustment is made for the extreme case where this exceeds what is left of
    // the page)
    *cursor += top_extra;

    // The offset converting an absolute Y (`b.layout.content.y`) to a within-page Y
    // (`*cursor`). The absolute Y of the container's first child (in normal flow) coincides
    // with the absolute Y of the container's own content area (`b.layout.content.y`), so that
    // is the initial value. It is updated whenever a non-float item is placed (so it keeps up
    // when a page break resets `*cursor`).
    let mut shift_reference = container.layout.content.y - *cursor;

    // If `b` really draws no background colour or borders, there is no need to generate a
    // decoration fragment at all. In that case tracking `segments` is unnecessary too, and
    // there is no longer a reason to make `PaginationState` hold onto pages
    // (`enter_split`/`exit_split` are not called). Containers with no decoration
    // (`<html>`/`<body>` and most wrapper `<div>`s) taking this fast path greatly improves
    // the flush frequency while streaming.
    let needs_decoration = container.has_visible_decoration;
    // An outside marker (`list-style-position: outside`) is drawn on the first fragment, so
    // fragment generation is still needed even with no decoration. Skipping it would lose the
    // marker of an `li` split across pages.
    let needs_fragments = needs_decoration || container.marker.is_some();
    if needs_fragments {
        // Record the first absolute page index this container touches
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

    // The shared handling for a forced break (`break-before`/`break-after: always`): start a
    // new page and add the corresponding segment. `current_page` is updated on the spot too,
    // to avoid double-counting against a natural page advance from overflow.
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
    for (i, item) in items.iter_mut().enumerate() {
        let (breaks_before, breaks_after) = break_hints(i, item);
        // If the current page has no real content on it (including the case where only an
        // ancestor's margin has advanced `cursor`), breaking would only create a pointless
        // empty page, so nothing is done.
        if breaks_before && current_page_has_content(state) {
            force_new_page(state, cursor, &mut current_page, &mut segments);
        }

        if is_float(item) {
            // A float does not take part in the flow and so does not change the shared
            // `cursor`. Using a temporary cursor seeded from `shift_reference` makes the
            // `shift = margin_box_top - *cursor` computation inside `place_one`
            // (that is, `place_box`) the same translation as the surrounding normal flow, so
            // it lands at the correct within-page position.
            let mut local_cursor = item_margin_box_top(item) - shift_reference;
            place_one(item, page_height, state, &mut local_cursor);
        } else {
            let cursor_before_item = *cursor;
            place_one(item, page_height, state, cursor);
            shift_reference = item_margin_box_top(item) - cursor_before_item;

            let now_page = state.current_index();
            if now_page != current_page {
                // We advanced to a new page. Nothing but `b`'s content can intervene on a page
                // created here, so it starts from the top (index 0).
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

        // Only break the page when there is a following element to place (so no empty page
        // is created after the last one).
        if breaks_after && i + 1 < item_count {
            force_new_page(state, cursor, &mut current_page, &mut segments);
        }
    }

    // For the caller's sake (the next sibling), add the bottom margin, border and padding to the cursor too.
    *cursor += bottom_extra;

    if !needs_fragments {
        return;
    }

    // Keep only the segments where content was really placed (for instance where the first
    // child caused a break at the top of a page and nothing landed on the page before it).
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
            // A container with no decoration needs only the first fragment, to carry the
            // marker; later fragments would be empty boxes drawing nothing.
            if !needs_decoration && !is_first {
                return None;
            }
            let end_index = state.get(seg.page_index).boxes.len();
            let (top, bottom) =
                extent_of(&state.get(seg.page_index).boxes[seg.start_index..end_index]);
            let layout = fragment_layout(&container.layout, top, bottom, is_first, is_last);
            // The marker is kept only on `b`'s first fragment (to avoid drawing it again on
            // later fragments when this box is split across pages). The marker's coordinates
            // are still the absolute ones from layout, so they are moved to the fragment's
            // within-page coordinates (preserving its position relative to the top of the
            // container's content).
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
                // A decoration-only fragment is never split further, so the fragmentation
                // hints mean nothing (they keep their initial values).
                fragmentation: FragmentationHints::default(),
                // This box itself is `Blocks(Vec::new())` with no children and is never passed
                // to `place_split` again. A fragment created solely for the marker draws no
                // background or borders.
                has_visible_decoration: needs_decoration,
                // A decoration fragment is not itself a float (even where `b` is one, this
                // fragment is mixed into the rest of `place_split`'s loop as part of the
                // normal flow, so it has to be `false`).
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

    // Every decoration fragment for this container has been inserted, so the record is removed.
    // If that made more pages flushable, they are passed to `on_flush` here.
    state.exit_split();
}

/// Find the vertical union extent, in within-page coordinates, of the descendants really
/// placed in `boxes` (the smallest top and largest bottom of their margin boxes).
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

/// Assemble the [`Layout`] for drawing one fragment's worth of decoration (background and
/// borders) for the container `original`. `content_y`/`content_bottom` are that fragment's
/// content area extent (with `is_first`, the top is already settled at `content_y`; with
/// `is_last`, the bottom does not yet include `padding-bottom`/`border-bottom`).
///
/// `fragment` (into [`FragmentPosition`]) records the fragment's position, derived from
/// `is_first`/`is_last`. `border-radius` is taken straight from the computed style
/// (`Layout` holds only thicknesses), so the renderer ([`crate::pdf::document`]) uses this
/// information to decide "do not round the corners on a continuing fragment".
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
    // The `display: inline-block` boxes in a line move with the line (moving only the line's
    // rect would strand the boxes at their original positions). `shift_box_y`'s `delta` is an
    // amount to subtract (`rect.y -= delta`), so the same movement as the line's
    // `rect.y -= shift` is achieved by passing `shift` through unchanged.
    for atomic in translated.atomics.iter_mut() {
        atomic.content = shift_box_y(&atomic.content, shift);
    }

    let fragment = LaidOutBox {
        node: None,
        layout: Layout {
            content: translated.rect,
            ..Layout::default()
        },
        // A synthesised wrapper box holding a single line, so it carries no fragmentation
        // hints (orphans/widows are decided per line by the caller, `place_split`).
        fragmentation: FragmentationHints::default(),
        has_visible_decoration: false,
        // A line box has no concept of a float.
        is_float: false,
        content: LaidOutContent::Inline(vec![translated]),
        // A single-line synthesised wrapper is never a list-item itself, so this is always
        // `None` (the original container's `marker` is carried by the decoration fragment above).
        marker: None,
    };
    *cursor += line.rect.height;
    state.last_mut().boxes.push(fragment);
}

/// Split a table across pages row by row.
///
/// Consecutive rows landing on the same page are gathered into one fragment (a `LaidOutBox`
/// holding `LaidOutContent::Table`). A fragment inherits the table's own node and geometry,
/// replacing only `content.y`/`content.height` and `FragmentPosition`.
/// Place a grid container across pages by row band. The same "assemble a fragment and settle
/// it" structure as `place_table`, with the row band as the unit.
///
/// No split happens at a boundary where an item spans the bottom of the band (a grid item
/// spanning several rows), the same as a table's `rowspan`.
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

    // Before the first fragment, reserve the container's own top margin, border and padding
    // (handled the same way as in `place_split`/`place_table`).
    *cursor += top_extra;

    for (index, row) in grid.rows.iter().enumerate() {
        if pending.is_empty() {
            fragment_top = *cursor;
            shift = row.top - *cursor;
        }

        // With an item carried over from the previous band, no cut is possible at that boundary.
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

/// Translate a row band and the items within it vertically together.
fn shift_grid_row_y(row: &LaidOutGridRow, delta: f32) -> LaidOutGridRow {
    LaidOutGridRow {
        items: row
            .items
            .iter()
            .map(|item| shift_box_y(item, -delta))
            .collect(),
        top: row.top - delta,
        bottom: row.bottom - delta,
        spans_bottom: row.spans_bottom,
    }
}

/// Push the row bands `place_grid` assembled onto the page as one fragment.
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

    // The rows to push into the fragment, and the translation from absolute to within-page coordinates.
    let mut pending: Vec<LaidOutTableRow> = Vec::new();
    let mut shift = 0.0f32;
    let mut fragment_top = 0.0f32;
    let mut is_first_fragment = true;

    // The `<thead>` rows to repeat at the top of the second page onwards. They are not
    // repeated when the headings alone fill the page (that is, when not a single row advances).
    // A heading row is copied and placed on the second page onwards, so this is the one place
    // that holds a copy (there are few of them). Body rows are moved rather than copied.
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
    // A `caption-side: top` caption goes on the first fragment.
    let caption_is_top = table.caption_side == CaptionSide::Top;
    let mut pending_caption = table.caption.as_deref().filter(|_| caption_is_top).cloned();

    // Before the first fragment, reserve the container's own top margin, border and padding
    // (handled the same way as in `place_split`).
    *cursor += top_extra;

    // The caption's height counts towards the first fragment too.
    let caption_height = pending_caption
        .as_ref()
        .map(|c| c.layout.margin_box_height())
        .unwrap_or(0.0);

    let start_new_fragment = |cursor: &f32, first_row_top: f32, extra_above: f32| {
        // The top of the fragment (in within-page coordinates), and the translation to get it there.
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
        }

        // If this row would overflow the current page, settle the fragment so far and break.
        // The check is "this fragment already has at least one row" (not
        // `current_page_has_content`): a table has to split per row even when it is the only
        // content at the top of a page, and never creating an empty fragment guarantees progress.
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
            // Copy the heading rows to the top of the new page (the original rows are placed
            // on the first page as-is, so copies only appear from the second page on).
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

    // A `caption-side: bottom` caption goes on the last fragment.
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

/// Push the rows `place_table` assembled onto the page as one fragment.
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

/// The absolute Y of the top of a table row's margin box (the topmost among its cells).
fn table_row_top(row: &LaidOutTableRow) -> f32 {
    row.cells
        .iter()
        .map(margin_box_top)
        .fold(f32::MAX, f32::min)
}

/// The absolute Y of the bottom of a table row's margin box (the lowest among its cells).
fn table_row_bottom(row: &LaidOutTableRow) -> f32 {
    row.cells
        .iter()
        .map(|cell| margin_box_top(cell) + cell.layout.margin_box_height())
        .fold(f32::MIN, f32::max)
}

/// Translate a row (all of its cells) vertically in place.
fn shift_table_row_y_in_place(row: &mut LaidOutTableRow, shift: f32) {
    for cell in &mut row.cells {
        shift_box_y_in_place(cell, shift);
    }
}

/// Translate a row (all of its cells) vertically. `shift` is an amount to subtract, as in `shift_box_y`.
fn shift_table_row_y(row: &LaidOutTableRow, shift: f32) -> LaidOutTableRow {
    LaidOutTableRow {
        node: row.node,
        cells: row.cells.iter().map(|c| shift_box_y(c, shift)).collect(),
        section: row.section,
    }
}

/// Move a box that will not be split further onto the page as-is.
///
/// The contents (`content`/`marker`) are taken by ownership. Cloning here would mean the
/// layout result and the pages existing at once, doubling the peak memory on a large
/// document. After the take, `b` becomes an empty `Blocks` and the caller never reads its
/// contents again (every path returns immediately afterwards).
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
    // When the call stack holds only containers with no decoration (which never call
    // `enter_split`/`exit_split`), no flush ever happens via `exit_split`, so the flush check
    // also runs here whenever a new page starts. That gives the finest streaming granularity:
    // in a structure with no decoration, a page is flushed the moment it is settled.

    state.try_flush();
    *cursor = 0.0;
}

/// Whether even one box has actually been placed on the current page. `cursor` may already
/// have advanced by an ancestor's margin, border or padding (so `cursor > 0.0` is possible
/// with nothing drawn yet), so this, rather than `cursor`, is what decides "is a forced break
/// really a meaningful move (that is, does it avoid discarding the current page while empty)".
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

    /// Recursively check that the boxes on a page stay within the page height (allowing a
    /// small tolerance).
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

        // No split occurs, so the original structure including the anonymous root (node: None) should be intact.
        let mut htmls = Vec::new();
        find_all(&dom, dom.document(), "html", &mut htmls);
        assert_eq!(pages[0].boxes.len(), 1);
        assert_eq!(pages[0].boxes[0].node, None);
        assert!(box_contains_node(&pages[0].boxes[0], htmls[0]));
        // An unsplit box is `Whole` (border-radius may apply to every corner).
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
             (a float is allowed to cross a page boundary)"
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

        // On the block.rs side a float is placed at the `cursor_y`=50 the previous sibling `a`
        // (height:50px) advanced the normal flow to (that is, right after `a`), a float starting
        // from the cursor_y where it was found in DOM order. No page break has happened, so
        // this absolute Y should carry over as the within-page Y
        // (if shift_reference works, the float's position does not shift).
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

    /// Find, among `page.boxes`, one whose node is `target` and whose content is
    /// `LaidOutContent::Blocks(vec![])` (that is, a decoration-only fragment).
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

        // Every page should carry a decoration fragment for the wrapper
        // (checking that the background and borders survive across pages).
        let decorations: Vec<&LaidOutBox> = pages
            .iter()
            .map(|page| {
                find_decoration_fragment(page, wrapper)
                    .expect("every page the wrapper spans should carry a decoration fragment")
            })
            .collect();

        // Only the first fragment has the top border and padding.
        assert_eq!(decorations[0].layout.border.top, 2.0);
        assert_eq!(decorations[0].layout.padding.top, 5.0);
        assert_eq!(decorations[0].layout.fragment, FragmentPosition::First);
        // Only the last fragment has the bottom border and padding.
        let last = decorations.last().unwrap();
        assert_eq!(last.layout.border.bottom, 2.0);
        assert_eq!(last.layout.padding.bottom, 5.0);
        assert_eq!(last.layout.fragment, FragmentPosition::Last);
        // A middle fragment is `Middle` (used to suppress border-radius rounding).
        for decoration in &decorations[1..decorations.len() - 1] {
            assert_eq!(decoration.layout.fragment, FragmentPosition::Middle);
        }

        // The left and right borders and padding apply to every fragment.
        for decoration in &decorations {
            assert_eq!(decoration.layout.border.left, 2.0);
            assert_eq!(decoration.layout.padding.left, 5.0);
            assert!(decoration.layout.content.height > 0.0);
        }

        // A middle fragment (neither first nor last) has no top or bottom border or padding.
        for decoration in &decorations[1..decorations.len() - 1] {
            assert_eq!(decoration.layout.border.top, 0.0);
            assert_eq!(decoration.layout.border.bottom, 0.0);
            assert_eq!(decoration.layout.padding.top, 0.0);
            assert_eq!(decoration.layout.padding.bottom, 0.0);
        }

        // Every one of the <p>s inside should still be found (a regression check that adding
        // decoration fragments has no side effect on the existing child placement logic).
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
        // A container with neither a background colour nor borders generates no decoration fragment at all
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

    /// Whether the page holds a box whose node is `target` and that carries real content
    /// (rather than being a decoration-only fragment).
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
        // The wrapper div's contents add up to very little (against the default page height)
        // and could take the "place it as a single leaf" fast path. Even so, the b inside has
        // `break-before: always` and it must not be missed.
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
        // Use filler to make the height left on the page smaller than the wrapper's total
        // height (400px), while the wrapper alone still fits a blank page whole.
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
        // avoid means "do not split if possible", so something too large for one page has to
        // be split as usual on a best-effort basis.
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

    /// Measure how a paragraph of `word_count` words breaks into lines at an explicit `width`
    /// (px): the line count, and that the line heights are uniform. The orphans/widows tests
    /// work back from that uniform line height to the `filler` height, to aim at a specific
    /// natural break point within the page. The tests themselves give the paragraph under test
    /// the same `width: {width}px; margin: 0;`, so the value measured here carries over
    /// (the paragraph's `width` being explicit, the containing width itself does not affect
    /// the wrapping).
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

    /// A line fragment after pagination (the anonymous wrapper `place_line` creates) does not
    /// carry the original paragraph's NodeId, so the lines on a page are simply totalled
    /// rather than filtered by node (the DOM in this test has no element with inline content
    /// other than the paragraph under test, so the total matches its line count).
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
        // Use filler to leave exactly one line plus half a line of page height
        // (so naturally only one line fits, which cannot satisfy orphans=3).
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
        // At the natural break point (n - 1) lines fit on this page and only one is left for
        // the next (which cannot satisfy widows=3).
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
        // Make the width extremely narrow so each word is its own line (giving three lines).
        let width = 10.0;
        let (n, line_height) = measure_paragraph_lines(word_count, width);
        assert_eq!(n, 3, "expected each word to wrap onto its own line");

        let settings = PageSettings::default();
        // orphans+widows (4) > n (3), so no break point can satisfy both.
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

    /// A test helper that calls [`PaginationState`] and [`place_box`] directly and reports how
    /// many pages are left in the buffer before `finish()` is called.
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
        // A structure of nothing but containers with no decoration (background colour or borders).
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

        // A container with no decoration never calls `place_split`'s enter_split/exit_split,
        // so `PaginationState::try_flush` fires on the spot at every `new_page` and the
        // previous page is flushed immediately (see the module docs and
        // `has_visible_decoration`). So even once the top-level `place_box` call is complete
        // (with `finish()` not yet called), the buffer should hold only "the last page, still
        // being written". An implementation that flushed every page at the end would give
        // `total_pages` here.

        assert_eq!(
            unflushed_buffer_len_after_place_box(&laid_out, page_height),
            1,
            "pages should already be flushed incrementally before finish() is even called"
        );
    }

    #[test]
    fn streaming_still_flushes_down_to_one_page_when_a_decorated_wrapper_spans_many_pages() {
        // Even where a wrapper with a background and borders crosses pages, once the top-level
        // `place_box` call is complete that wrapper's `place_split` must have run through to
        // its own exit_split (`place_split` finishes its work, then calls `exit_split` and
        // returns). So decoration or not, the invariant holds that only the last page remains
        // in the buffer once `place_box` is complete.

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
        // A correctness check on whether `PaginationState`'s flush decision (see the module
        // docs) is safe: in the most delicate case involving decoration fragments being
        // (`split_container_gets_a_decoration_fragment_on_every_page_it_spans`
        // written back), confirm that the streaming and batch versions return exactly the same
        // result. Flushing too early would either panic (the page a decoration fragment goes
        // on having already been flushed) or lose the insertion altogether.
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
        // Twenty independent <p> elements with no decoration. Check that each time a page is
        // flushed, the <p> elements placed on it (and their text descendants) have been freed.

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

        // After every page is processed, all twenty <p> elements should be freed
        // (the undecorated wrapper div too, on the last page's flush).
        for &p in &ps {
            assert!(
                dom.is_released(p),
                "paragraph {p:?} should be released once its page has flushed"
            );
        }
    }

    #[test]
    fn paginate_document_streaming_eventually_releases_a_spanning_wrapper() {
        // Even where a wrapper with a background and borders spans several pages, once
        // everything has run through `paginate_document_streaming` (the public API) the
        // wrapper's own node should be freed too (it is freed once its decoration fragment
        // becomes `Last` on the final page). Directly checking the intermediate state, that
        // "it is not freed before the last page", is done in the test
        // `wrapper_node_is_not_released_before_its_last_fragment_flushes`
        // below.
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
        // Observe directly from inside the `on_page` callback that the wrapper node is not yet
        // freed at the point the first page is flushed.
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
        // Check there is no premature freeing (a bug wrongly freeing a node that has not
        // appeared yet) by precomputing with the batch version which page each <p> really
        // lands on, and confirming at every `on_page` that it is not yet freed before that page.

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
        // Check that "processing all twenty <p>s as one tree at once" and "push_item one at a
        // time" end up with the same page count and contents (the groundwork for those two
        // paths agreeing under top-level-element streaming input).

        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        // Batch version: lay all twenty <p>s out inside a single div.
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

        // push_item version: from the same `combined_dom`/`combined_styles`, cut out each <p>
        // element's LayoutBox individually with `build_box_for_element` and push_item each in
        // turn. Calling `html::parse` twenty times would supply an independent <html>/<body>
        // each time, accumulating twenty lots of the UA stylesheet's `body { margin: 8px; }`
        // and testing something other than the push_item logic, so the elements are cut from
        // the same DOM.
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

    /// Paginate the same element sequence both with the batch version ([`paginate`]) and with
    /// [`StreamingPaginator`]'s `push_item`, and return each page count.
    ///
    /// Only one DOM is built and shared by both, because parsing each element with
    /// `html::parse` would accumulate one supplied `<body>`'s UA margin per element
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
        // `place_box` only looks at forced breaks within a child list, so a `break-after`
        // between top-level elements (that is, across `push_item` calls) would be ignored
        // unless the paginator handled it.
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
        // A leading `break-before` and a trailing `break-after` have nothing to move to, so
        // no empty page should be created (the same handling as the batch version).
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

    // ===== Splitting a table across pages row by row =====

    /// Paginate `html_src` and return the table row count per page.
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
            // Within each page the rows run top to bottom and stay within the page height.
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
        // With the headings alone filling the page not a single row could advance, so they are not repeated.
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
