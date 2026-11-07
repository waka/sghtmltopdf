//! Streaming pagination: [`StreamingPaginator`] continues pagination across top-level items
//! added one at a time, and overlays their positioned boxes onto the pages they belong to.
use super::super::block::{shift_box_x, shift_box_y, LaidOutBox, PositionedBox, PositionedKind};
use super::{
    current_page_has_content, find_ancestor_padding_box_origin, margin_box_top, new_page,
    place_box, Page, PaginationBuffer, PaginationState,
};
use crate::style::BreakBetween;

/// A streaming paginator: several `LaidOutBox`es (normally one per top-level block element
/// of the document) are added in turn with [`Self::push_item`], and [`Self::finish`] flushes
/// the remaining pages.
///
/// [`paginate_streaming`](super::paginate_streaming) assumes "one complete `LaidOutBox` tree" processed at once, while
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
    /// How much to subtract from an item's absolute y (the coordinates layout produced) to get
    /// an in-page one, as [`place_split`](super::place_split) keeps it for the children of `<body>` in batch. It
    /// is derived again from where each item landed, so it follows along across page breaks.
    shift_reference: f32,
    page_height: f32,
    /// Whether the item added just before had `break-after: always`.
    /// It is consumed as a page break when the next item is added.
    pending_break_after: bool,
    /// The positioned boxes ([`Self::place_positioned`]) for pages still held: an `absolute`
    /// one for its page, and a `fixed` one for every page. They are added when the page is
    /// flushed, so they are drawn above the normal flow later items place on it too.
    overlays: Vec<(OverlayPages, LaidOutBox)>,
}

impl StreamingPaginator {
    pub fn new(page_height: f32) -> Self {
        Self {
            buffer: PaginationBuffer::new(),
            cursor: 0.0,
            shift_reference: 0.0,
            page_height,
            pending_break_after: false,
            overlays: Vec::new(),
        }
    }

    /// Add one item. Returns the pages this call settled.
    ///
    /// The item's own `break-before`/`break-after` are handled here: `place_box` only looks
    /// at forced breaks within a child list, and the relationship between items (that is,
    /// siblings directly under `<body>`) exists only on the paginator's side. It matches the
    /// decision [`place_split`](super::place_split) makes for siblings in the batch version.
    pub fn push_item(&mut self, item: &mut LaidOutBox) -> Vec<Page> {
        let break_before =
            self.pending_break_after || item.fragmentation.break_before == BreakBetween::Always;
        // `break-after` means "break the page before the next sibling", so it is only recorded
        // here and consumed by the next `push_item`. The last item's `break-after` is ignored
        // by `finish`, so no empty page is left at the end.
        self.pending_break_after = item.fragmentation.break_after == BreakBetween::Always;
        let top = margin_box_top(item);
        let bottom = top + item.layout.margin_box_height();

        let mut flushed = Vec::new();
        {
            let mut on_flush = |page: Page| flushed.push(page);
            let mut state = PaginationState::new(&mut self.buffer, &mut on_flush);
            // With nothing placed on the current page, breaking would only add an empty page,
            // so nothing is done (a `break-before` on the first element, say).
            if break_before && current_page_has_content(&state) {
                new_page(&mut state, &mut self.cursor);
                self.shift_reference = top - self.cursor;
            }
            // Map the absolute coordinates straight into the page, as `place_split` does.
            // Stacking margin box heights instead would drop `<body>`'s own top margin and
            // count two collapsed sibling margins twice. Nothing above the top of the page is
            // painted, so stop at 0.
            self.cursor = (top - self.shift_reference).max(0.0);
            place_box(item, self.page_height, &mut state, &mut self.cursor);
            // `place_box` may have broken the page, so derive the factor again from the
            // in-page coordinate of the item's bottom.
            self.shift_reference = bottom - self.cursor;
        }
        let first = self.buffer.flushed - flushed.len();
        for (i, page) in flushed.iter_mut().enumerate() {
            attach_overlays(&mut self.overlays, first + i, page);
        }
        flushed
    }

    /// Overlay the absolutely positioned boxes of the item just added onto the pages they
    /// belong to, by the same rules as [`apply_positioned_overlays`](super::apply_positioned_overlays). `flushed` is what
    /// [`Self::push_item`] returned for that item (empty for an item that is itself out of
    /// the flow).
    ///
    /// A `fixed` box is repeated on `flushed` and on every page after it. Batch also puts it on
    /// the pages handed out before, which streaming cannot, and an `absolute` with no
    /// positioned ancestor belongs on the first page, which may be among them: both are
    /// reported in the result.
    pub(crate) fn place_positioned(
        &mut self,
        flushed: &mut [Page],
        positioned: Vec<PositionedBox>,
    ) -> PositionedLeftovers {
        let first_flushed = self.buffer.flushed - flushed.len();
        let mut leftovers = PositionedLeftovers::default();
        for pb in positioned {
            // The page's absolute index, and how far to move the box from where it was laid out.
            let (index, (dx, dy)) = match pb.kind {
                PositionedKind::Fixed => {
                    for page in flushed.iter_mut() {
                        page.boxes.push(pb.laid.clone());
                    }
                    leftovers.fixed_missed_pages |= first_flushed > 0;
                    self.overlays.push((OverlayPages::Every, pb.laid));
                    continue;
                }
                PositionedKind::AbsoluteInitial => (0, (0.0, 0.0)),
                PositionedKind::AbsoluteAncestor {
                    node,
                    padding_box_origin,
                } => {
                    // The ancestor is part of this item, so it is on `flushed` or a held page.
                    let pages = flushed.iter().chain(&self.buffer.buffer);
                    match find_ancestor_padding_box_origin(pages, node) {
                        Some((i, (px, py))) => (
                            first_flushed + i,
                            (px - padding_box_origin.0, py - padding_box_origin.1),
                        ),
                        // Batch leaves out a box whose ancestor is on no page as well.
                        None => continue,
                    }
                }
            };
            if index < first_flushed {
                leftovers.dropped.push(pb);
                continue;
            }
            // `shift_box_y`'s delta is an amount to subtract, so moving down means `-dy`.
            let laid = shift_box_x(&shift_box_y(&pb.laid, -dy), dx);
            match flushed.get_mut(index - first_flushed) {
                Some(page) => page.boxes.push(laid),
                None => self.overlays.push((OverlayPages::One(index), laid)),
            }
        }
        leftovers
    }

    /// The absolute index of the page the next item starts on (the last one still held).
    pub(crate) fn current_page_index(&self) -> usize {
        self.buffer.flushed + self.buffer.buffer.len() - 1
    }

    /// Signal that there are no more items and return every remaining page.
    pub fn finish(mut self) -> Vec<Page> {
        debug_assert!(
            self.buffer.active_min_page.is_empty(),
            "finish should be called after leaving every place_split call"
        );
        let first = self.buffer.flushed;
        for (i, page) in self.buffer.buffer.iter_mut().enumerate() {
            attach_overlays(&mut self.overlays, first + i, page);
        }
        self.buffer.buffer
    }
}

/// Which pages a held overlay ([`StreamingPaginator::place_positioned`]) goes on.
enum OverlayPages {
    /// The page at this absolute index (an `absolute` box).
    One(usize),
    /// Every page flushed from now on (a `fixed` box).
    Every,
}

/// What [`StreamingPaginator::place_positioned`] could not draw as batch draws it.
#[derive(Default)]
pub(crate) struct PositionedLeftovers {
    /// The boxes not drawn at all: an `absolute` with no positioned ancestor, once the first
    /// page has been handed out.
    pub(crate) dropped: Vec<PositionedBox>,
    /// Whether a `fixed` box arrived after some pages had been handed out, so they lack it.
    pub(crate) fixed_missed_pages: bool,
}

/// Add the overlays held for the page at absolute index `index` to `page`, in the order they
/// were placed. One for that page alone is used up; a `fixed` one stays for the next page.
fn attach_overlays(overlays: &mut Vec<(OverlayPages, LaidOutBox)>, index: usize, page: &mut Page) {
    let mut i = 0;
    while i < overlays.len() {
        match overlays[i].0 {
            OverlayPages::One(target) if target == index => {
                page.boxes.push(overlays.remove(i).1);
                continue;
            }
            OverlayPages::Every => page.boxes.push(overlays[i].1.clone()),
            OverlayPages::One(_) => {}
        }
        i += 1;
    }
}
