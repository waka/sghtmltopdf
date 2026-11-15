//! Splitting a flex container taller than a page by band: a group of items that overlap
//! vertically.
use super::super::block::LaidOutBox;
use super::{margin_box_top, new_page, place_box, place_leaf, PaginationState};

/// The unit a flex container taller than a page is split into: a band grouping
/// items that overlap vertically. In a column flex each item is a band; in a row
/// flex each flex line is. `top`/`bottom` are absolute y coordinates of the
/// margin box.
pub(super) struct FlexBand {
    items: Vec<LaidOutBox>,
    pub(super) top: f32,
    pub(super) bottom: f32,
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
pub(super) fn group_flex_items_into_bands(mut items: Vec<LaidOutBox>) -> Vec<FlexBand> {
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

/// Places one flex band on a page. Called from [`place_split`](super::place_split).
///
/// A band holding a single item (each item of a column flex) goes to
/// [`place_box`] as an ordinary box, so an item taller than a page is split
/// inside it like a block. A band with several items side by side is treated as
/// one leaf, to keep the items in the same position relative to each other: if
/// it does not fit in what is left it is moved to the top of the next page, and
/// if the band itself is taller than a page it overflows, as before.
pub(super) fn place_flex_band(
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
