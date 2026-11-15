//! Splitting a grid container across pages by row band.
use super::super::block::{shift_box_y, FragmentationHints, LaidOutBox, LaidOutContent};
use super::super::grid::{LaidOutGrid, LaidOutGridRow};
use super::{current_page_has_content, fragment_layout, new_page, PaginationState, SplitContainer};

/// Place a grid container across pages by row band. The same "assemble a fragment and settle
/// it" structure as `place_table`, with the row band as the unit.
///
/// No split happens at a boundary where an item spans the bottom of the band (a grid item
/// spanning several rows), the same as a table's `rowspan`.
pub(super) fn place_grid(
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

            // When the first row of a fragment does not fit in what is left of
            // the page, start at the top of the next one rather than overflow
            // (without this, a row laid down at the bottom of the page loses its
            // lower half). A row too tall for an empty page is placed as it is,
            // so that pagination keeps moving forward.
            if row.bottom - shift > page_height && current_page_has_content(state) {
                new_page(state, cursor);
                // Still the first fragment: its top margin, border and padding were reserved
                // on the page just left, so reserve them again on this one.
                if is_first_fragment {
                    *cursor += top_extra;
                }
                fragment_top = *cursor;
                shift = row.top - *cursor;
            }
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

    // `fragment_top`/`fragment_bottom` already bound the content: the caller reserves the top
    // margin, border and padding before the first fragment and the bottom ones after the
    // last, so the decoration is sliced the same way as in `place_split`.
    let layout = fragment_layout(
        &container.layout,
        fragment_top,
        fragment_bottom,
        is_first,
        is_last,
    );

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
        link: container.link.clone(),
    };
    state.active_mut().boxes.push(fragment);
}
