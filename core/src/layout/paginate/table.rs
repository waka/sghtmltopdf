//! Splitting a table across pages row by row, and a row taller than a page through its
//! cells.
use std::collections::VecDeque;

use super::super::block::{
    shift_box_y, shift_box_y_in_place, CollapsedBoundary, FragmentationHints, LaidOutBox,
    LaidOutContent, LaidOutTable, LaidOutTableRow, TableBreak,
};
use super::super::box_tree::TableSection;
use super::super::geometry::Layout;
use super::{
    can_be_split, current_page_has_content, extent_of, fragment_layout, margin_box_top, new_page,
    place_box, rows_avoid_break_between, table_first_piece_bottom, Page, PaginationBuffer,
    PaginationState, SplitContainer,
};
use crate::style::{BreakBetween, CaptionSide};

/// Split a table across pages row by row.
///
/// Consecutive rows landing on the same page are gathered into one fragment (a `LaidOutBox`
/// holding `LaidOutContent::Table`). A fragment inherits the table's own node and geometry,
/// replacing only `content.y`/`content.height` and `FragmentPosition`.
pub(super) fn place_table(
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
    let head_top = head_rows
        .iter()
        .map(|row| table_row_top(row) - row.boundary_above.gap())
        .fold(f32::MAX, f32::min);
    let head_bottom = head_rows
        .iter()
        .map(table_row_bottom)
        .fold(f32::MIN, f32::max);
    let head_height = if head_rows.is_empty() {
        0.0
    } else {
        head_bottom - head_top
    };
    // The boundary below the head's last row, whose upper side a repeated head brings along.
    let head_boundary_below = table
        .rows
        .iter()
        .position(|row| row.section != TableSection::Head)
        .map_or(CollapsedBoundary::default(), |i| {
            table.rows[i].boundary_above
        });
    let row_count = table.rows.len();
    // The row placed last (its cells left out), to hand to the fragment that follows a break.
    let mut previous_row: Option<LaidOutTableRow> = None;

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

    // The first piece of the table (see `table_first_piece_bottom`). When it does not fit in
    // what is left of the page, the whole table starts on the next one rather than leaving its
    // head alone at the bottom.
    let first_piece_bottom = table_first_piece_bottom(&table.rows).unwrap_or(f32::MIN);

    // A queue rather than a plain iteration: at a break, the rows the page should not break
    // between go back to the front and are placed again on the next page. Each row keeps its
    // index in the table.
    let mut queue: VecDeque<(usize, LaidOutTableRow, RowKind)> = std::mem::take(&mut table.rows)
        .into_iter()
        .enumerate()
        .map(|(index, row)| (index, row, RowKind::Whole))
        .collect();
    while let Some((index, mut row, mut kind)) = queue.pop_front() {
        let mut row_top = table_row_top(&row);
        let mut cells_bottom = table_row_bottom(&row);
        // The last row needs room for the table's closing collapsed border too. Other rows
        // need none: at a break after them the border can move to the next page whole. A row
        // split across pages needs it on its last piece only.
        let closes_table = index + 1 == row_count && !matches!(kind, RowKind::Piece);
        let row_bottom = cells_bottom
            + if closes_table {
                table.trailing_gap
            } else {
                0.0
            };
        let extra_above = if index == 0 && is_first_fragment {
            caption_height
        } else {
            0.0
        };
        // Whether this row, still whole, is to be split through its cells when it does not fit
        // on a page by itself.
        let may_split = kind == RowKind::Whole && row_splits_through_cells(&row, page_height);

        if pending.is_empty() {
            let gap_above = row.boundary_above.gap();
            let (top, s) = start_new_fragment(cursor, row_top - gap_above, extra_above);
            fragment_top = top;
            shift = s;

            // As with the grid, start on the next page when the first row, with the first
            // piece of the table, does not fit in what is left (the caption height counts
            // towards it).
            let whole_first = index == 0 && kind == RowKind::Whole;
            let first_bottom = if whole_first {
                first_piece_bottom.max(row_bottom)
            } else {
                row_bottom
            };
            // ...unless the row would not fit an empty page either: a row cannot be kept
            // whole anywhere then, so it starts here and splits.
            let on_empty_page = row_bottom - (row_top - gap_above - extra_above)
                + if is_first_fragment { top_extra } else { 0.0 };
            let too_tall_for_a_page = may_split && on_empty_page > page_height;
            if first_bottom - shift > page_height
                && current_page_has_content(state)
                && !too_tall_for_a_page
            {
                new_page(state, cursor);
                // Still the first fragment: reserve its top margin, border and padding again
                // (as in `place_grid`).
                if is_first_fragment {
                    *cursor += top_extra;
                }
                let (top, s) = start_new_fragment(cursor, row_top - gap_above, extra_above);
                fragment_top = top;
                shift = s;
            }
        }

        // If this row would overflow the current page, settle the fragment so far and break.
        // The check is "this fragment already has at least one row" (not
        // `current_page_has_content`): a table has to split per row even when it is the only
        // content at the top of a page, and never creating an empty fragment guarantees progress.
        // A fragment holding only the head rows repeated on this page takes its first body row
        // whatever its height, for the same reason.
        //
        // A row that is to be split (see `split_tall_row`) takes the place of the first body
        // row in the same way, also on the first page, where breaking before it would leave
        // the head alone.
        let row_bottom_on_page = row_bottom - shift;
        let only_head = pending.iter().all(|r| r.section == TableSection::Head);
        let only_repeated_head = !is_first_fragment && only_head;
        let mut carried_back = false;
        if row_bottom_on_page > page_height
            && !pending.is_empty()
            && !only_repeated_head
            && !(may_split && only_head)
        {
            // The rows the page should not break between go to the next page together.
            let keep = rows_to_keep_at_break(&pending, &row);
            if keep < pending.len() {
                carried_back = true;
                let mut carried = pending.split_off(keep);
                *cursor = pending.last().map_or(*cursor, table_row_bottom);
                // Back to their coordinates before this fragment, to be placed again in
                // order: the first carried row now starts the next fragment, and the others
                // and `row` follow it. The carried rows are the body rows just before `row`.
                let first_index = index - carried.len();
                for carried_row in &mut carried {
                    shift_table_row_y_in_place(carried_row, -shift);
                }
                queue.push_front((index, row, kind));
                let mut rest = carried.split_off(1);
                for (offset, carried_row) in rest.drain(..).enumerate().rev() {
                    queue.push_front((first_index + 1 + offset, carried_row, RowKind::Whole));
                }
                row = carried.pop().expect("one row was carried");
                kind = RowKind::Whole;
                row_top = table_row_top(&row);
                cells_bottom = table_row_bottom(&row);
            }

            let boundary = &row.boundary_above;
            let share_above = if *cursor + boundary.split_gaps(0.5).0 <= page_height {
                0.5
            } else {
                0.0
            };
            let (end_gap, start_gap) = boundary.split_gaps(share_above);
            *cursor += end_gap;
            if let Some(last) = pending.last_mut() {
                last.break_below = Some(TableBreak {
                    row: Box::new(without_cells(&row)),
                    share: share_above,
                });
            }
            // The row the page breaks after, for the fragment below to know.
            if let Some(last) = pending.last() {
                previous_row = Some(without_cells(last));
            }
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
            // Below a repeated head the row meets the head rather than the break, as in Chrome.
            let gap_above = if repeat_head {
                let head_shift = head_top - *cursor;
                for head_row in &head_rows {
                    pending.push(shift_table_row_y(head_row, head_shift));
                }
                *cursor += head_height;
                head_boundary_below.joined_with(&row.boundary_above).gap()
            } else {
                row.break_above = previous_row.take().map(|previous| TableBreak {
                    row: Box::new(previous),
                    share: 1.0 - share_above,
                });
                start_gap
            };
            shift = row_top - gap_above - *cursor;
        }

        // A row that does not fit on a page by itself (below the head repeated on it, if any)
        // is split through its cells. Its pieces go back to the front of the queue, each laid
        // out to fill one page, and the loop places them like any other row: the first one
        // here, and the others each below a break.
        if may_split
            && !carried_back
            && row_bottom - shift > page_height
            && pending.iter().all(|r| r.section == TableSection::Head)
        {
            let head_offset = if repeat_head { head_height } else { 0.0 };
            let pieces = split_tall_row(&mut row, shift, head_offset, page_height);
            let last = pieces.len() - 1;
            for (i, piece) in pieces.into_iter().enumerate().rev() {
                // The rows below the split row go on from where it ended.
                let kind = if i == last {
                    RowKind::LastPiece {
                        resume_at: cells_bottom,
                    }
                } else {
                    RowKind::Piece
                };
                queue.push_front((index, piece, kind));
            }
            continue;
        }

        previous_row = Some(without_cells(&row));
        shift_table_row_y_in_place(&mut row, shift);
        pending.push(row);
        *cursor = cells_bottom - shift;
        // The rows after a split row sit below it by the same distance as in the layout.
        if let RowKind::LastPiece { resume_at } = kind {
            shift = resume_at - *cursor;
        }
    }
    *cursor += table.trailing_gap;

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

/// How many of the rows of a fragment stay on its page when `next` does not fit below them.
/// The rows the page should not break between (see [`rows_avoid_break_between`]) move to the
/// next page together. The head rows and one more row always stay, which guarantees progress;
/// a run of rows that cannot be broken anywhere is broken where it overflows.
fn rows_to_keep_at_break(pending: &[LaidOutTableRow], next: &LaidOutTableRow) -> usize {
    let min_keep = pending
        .iter()
        .take_while(|row| row.section == TableSection::Head)
        .count()
        + 1;
    // Whether the page should not break after the first `keep` rows.
    let avoided_at = |keep: usize| {
        rows_avoid_break_between(&pending[keep - 1], pending.get(keep).unwrap_or(next))
    };
    let mut keep = pending.len();
    while keep > min_keep && avoided_at(keep) {
        keep -= 1;
    }
    if keep < pending.len() && avoided_at(keep) {
        pending.len()
    } else {
        keep
    }
}

/// What `place_table` holds in its queue of rows.
#[derive(Debug, Clone, Copy, PartialEq)]
enum RowKind {
    /// A row as the layout made it.
    Whole,
    /// One of the pieces a row taller than a page was split into (see [`split_tall_row`]),
    /// except the last. Its coordinates are made so that it lands on a page of its own.
    Piece,
    /// The last piece. The rows below the split row go on from where it ends; `resume_at` is
    /// the absolute Y the split row ended at in the layout.
    LastPiece { resume_at: f32 },
}

/// Whether a row too tall for a page can be split through its cells: some cell has content
/// that splits, and the cells all end together (a cell spanning rows below does not, and the
/// rows below it could not follow the split).
fn row_splits_through_cells(row: &LaidOutTableRow, page_height: f32) -> bool {
    let bottom_of = |cell: &LaidOutBox| margin_box_top(cell) + cell.layout.margin_box_height();
    let Some(first) = row.cells.first() else {
        return false;
    };
    let bottom = bottom_of(first);
    row.cells
        .iter()
        .all(|cell| (bottom_of(cell) - bottom).abs() < 0.5)
        && row.cells.iter().any(|cell| can_be_split(cell, page_height))
}

/// Split a table row that is taller than a page through its cells, as a block is, so that the
/// row starts where it is and the content of its cells goes on from page to page.
///
/// Every cell is paginated on its own, as if it stood alone on the pages, from where it starts
/// (`shift`, the translation from layout coordinates to the page the row starts on, says where
/// that is). `head_offset` is the height of the head repeated at the top of the pages after
/// the first, which the row's pieces there are placed below.
///
/// A cell's content can land on later pages than the row starts on (a short cell, centred in
/// the row's height), and the cell is as tall as the row whatever its content: so a cell has
/// a fragment on every page the row reaches, whose geometry is worked out here rather than
/// from where its content is. The cell's top padding and border belong to the first page,
/// its bottom ones to the last, and a page without content of its own holds an empty
/// fragment, which paints the cell's background and side borders. A cell whose content cannot
/// be split (a flex container, say) stays on the first page, cut at its foot.
///
/// The result has a piece per page the row reaches. They are in the layout's coordinate space,
/// placed one page height apart below one another so that `place_table`, placing them in
/// turn, breaks the page before each but the first.
fn split_tall_row(
    row: &mut LaidOutTableRow,
    shift: f32,
    head_offset: f32,
    page_height: f32,
) -> Vec<LaidOutTableRow> {
    let height = page_height - head_offset;

    /// What a cell placed on each page, with the bottom of its content box there when
    /// `place_split` made a decoration fragment for it.
    struct CellPages {
        shell: LaidOutBox,
        start: f32,
        pages: Vec<(Vec<LaidOutBox>, Option<f32>)>,
        /// A cell that cannot be split, placed whole on the first page (`whole`, which the
        /// first piece takes).
        unsplittable: bool,
        whole: Option<LaidOutBox>,
    }
    let mut cells: Vec<CellPages> = Vec::with_capacity(row.cells.len());
    for cell in &mut row.cells {
        // The cell's own scalar information, which every fragment of it starts from.
        let shell = LaidOutBox {
            node: cell.node,
            layout: cell.layout,
            fragmentation: cell.fragmentation,
            has_visible_decoration: cell.has_visible_decoration,
            is_float: false,
            content: LaidOutContent::Blocks(Vec::new()),
            marker: None,
            link: cell.link.clone(),
        };
        let start = (margin_box_top(cell) - shift - head_offset).max(0.0);
        // A cell holding one line has nothing to split, but its line may sit anywhere in the
        // height of the row (`vertical-align`). Made a block child, it is placed where it
        // lands, like the children of any other cell.
        if let LaidOutContent::Inline(lines) = &mut cell.content {
            if let [line] = lines.as_slice() {
                let child = LaidOutBox {
                    node: None,
                    layout: Layout {
                        content: line.rect,
                        ..Layout::default()
                    },
                    fragmentation: FragmentationHints::default(),
                    has_visible_decoration: false,
                    is_float: false,
                    content: LaidOutContent::Inline(std::mem::take(lines)),
                    marker: None,
                    link: None,
                };
                cell.content = LaidOutContent::Blocks(vec![child]);
            }
        }
        let mut pages: Vec<(Vec<LaidOutBox>, Option<f32>)> = Vec::new();
        let mut placed_whole = None;
        if can_be_split(cell, height) {
            pages = paginate_cell(cell, start, height)
                .into_iter()
                .map(|page| take_decoration(&shell, page.boxes))
                .collect();
        } else {
            let mut whole = LaidOutBox {
                content: std::mem::replace(&mut cell.content, LaidOutContent::Blocks(Vec::new())),
                marker: cell.marker.take(),
                ..shell.clone()
            };
            let delta = margin_box_top(&whole) - start;
            shift_box_y_in_place(&mut whole, delta);
            let bottom = margin_box_top(&whole) + whole.layout.margin_box_height();
            if bottom > height {
                whole.layout =
                    fragment_layout(&whole.layout, whole.layout.content.y, height, true, false);
            }
            placed_whole = Some(whole);
        }
        cells.push(CellPages {
            shell,
            start,
            pages,
            unsplittable: placed_whole.is_some(),
            whole: placed_whole,
        });
    }

    // Where the row ends, counted down the pages laid one below another (`p * height` plus the
    // in-page Y on page `p`). Every cell is as tall as the row, so the row ends where the
    // lowest cell's margin box ends: at least where the layout put it, and lower when breaks
    // pushed the cell's content down. Taking the cells' margin boxes, not their content, lines
    // up the bottom borders of cells with different bottom padding, and an explicit height
    // that runs past the content still counts.
    let row_end = cells
        .iter()
        .map(|c| {
            let layout = &c.shell.layout;
            let below = layout.padding.bottom + layout.border.bottom + layout.margin.bottom;
            let laid_out = c.start + layout.margin_box_height();
            let paginated =
                c.pages
                    .iter()
                    .enumerate()
                    .rev()
                    .find_map(|(p, (boxes, decoration_bottom))| {
                        let content_bottom = (!boxes.is_empty()).then(|| extent_of(boxes).1);
                        content_bottom
                            .into_iter()
                            .chain(*decoration_bottom)
                            .reduce(f32::max)
                            .map(|bottom| p as f32 * height + bottom + below)
                    });
            paginated.map_or(laid_out, |bottom| bottom.max(laid_out))
        })
        .fold(0.0, f32::max);
    // A row ending exactly at a page foot does not reach the next page.
    let page_count = (((row_end - 0.01) / height).ceil() as usize).max(1);
    // The bottom of every cell's margin box on the last page.
    let last_page_end = row_end - (page_count - 1) as f32 * height;

    let mut pieces: Vec<LaidOutTableRow> = Vec::new();
    for page in 0..page_count {
        let origin = shift + head_offset + page as f32 * page_height;
        let (is_first, is_last) = (page == 0, page + 1 == page_count);
        let fragments: Vec<LaidOutBox> = cells
            .iter_mut()
            .filter_map(|c| {
                if c.unsplittable {
                    return if is_first { c.whole.take() } else { None };
                }
                let children = c.pages.get_mut(page).map(|p| std::mem::take(&mut p.0));
                let layout = &c.shell.layout;
                let content_y = if is_first {
                    c.start + layout.margin.top + layout.border.top + layout.padding.top
                } else {
                    0.0
                };
                let content_bottom = if is_last {
                    last_page_end
                        - layout.padding.bottom
                        - layout.border.bottom
                        - layout.margin.bottom
                } else {
                    height
                };
                Some(LaidOutBox {
                    layout: fragment_layout(layout, content_y, content_bottom, is_first, is_last),
                    content: LaidOutContent::Blocks(children.unwrap_or_default()),
                    ..c.shell.clone()
                })
            })
            .map(|mut fragment| {
                shift_box_y_in_place(&mut fragment, -origin);
                fragment
            })
            .collect();
        pieces.push(LaidOutTableRow {
            node: row.node,
            cells: fragments,
            section: row.section,
            group: row.group,
            fragmentation: row.fragmentation,
            first_in_group: row.first_in_group,
            last_in_group: row.last_in_group,
            boundary_above: row.boundary_above,
            break_above: None,
            break_below: None,
        });
    }

    // What belongs to the row as a whole belongs to its ends: where a break is wanted around
    // it, and the borders of the row and of its group.
    let last = pieces.len() - 1;
    for (i, piece) in pieces.iter_mut().enumerate() {
        if i > 0 {
            piece.fragmentation.break_before = BreakBetween::Auto;
            piece.first_in_group = false;
            piece.boundary_above = CollapsedBoundary::default();
        }
        if i < last {
            piece.fragmentation.break_after = BreakBetween::Auto;
            piece.last_in_group = false;
        }
    }
    pieces
}

/// Paginate one table cell from `start`, as if it were laid down alone on pages `height` tall.
fn paginate_cell(cell: &mut LaidOutBox, start: f32, height: f32) -> Vec<Page> {
    let mut buffer = PaginationBuffer::new();
    let mut pages: Vec<Page> = Vec::new();
    let mut cursor = start;
    {
        let mut on_flush = |page: Page| pages.push(page);
        let mut state = PaginationState::new(&mut buffer, &mut on_flush);
        place_box(cell, height, &mut state, &mut cursor);
    }
    pages.extend(buffer.buffer);
    pages
}

/// What a table cell placed on one page: `boxes` is what [`place_split`](super::place_split)
/// put there, with the decoration fragment it made for the cell (if the cell has any) first.
/// Returns the rest, and the bottom of the cell's content box on the page according to that
/// decoration fragment.
fn take_decoration(
    shell: &LaidOutBox,
    mut boxes: Vec<LaidOutBox>,
) -> (Vec<LaidOutBox>, Option<f32>) {
    let is_decoration = |b: &LaidOutBox| {
        shell.node.is_some()
            && b.node == shell.node
            && matches!(&b.content, LaidOutContent::Blocks(children) if children.is_empty())
    };
    if boxes.first().is_some_and(is_decoration) {
        let decoration = boxes.remove(0);
        let bottom = decoration.layout.content.y + decoration.layout.content.height;
        return (boxes, Some(bottom));
    }
    (boxes, None)
}

/// A copy of `row` without its cells, for a fragment to know the row across a break.
fn without_cells(row: &LaidOutTableRow) -> LaidOutTableRow {
    LaidOutTableRow {
        node: row.node,
        cells: Vec::new(),
        section: row.section,
        group: row.group,
        fragmentation: row.fragmentation,
        first_in_group: row.first_in_group,
        last_in_group: row.last_in_group,
        boundary_above: row.boundary_above,
        break_above: None,
        break_below: None,
    }
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
        has_visible_decoration: container.has_visible_decoration,
        is_float: false,
        content: LaidOutContent::Table(LaidOutTable {
            caption: caption.take().map(Box::new),
            caption_side: CaptionSide::Top,
            rows: std::mem::take(rows),
            trailing_gap: 0.0,
        }),
        marker: None,
        link: container.link.clone(),
    };
    state.active_mut().boxes.push(fragment);
}

/// The absolute Y of the top of a table row's margin box (the topmost among its cells).
fn table_row_top(row: &LaidOutTableRow) -> f32 {
    row.cells
        .iter()
        .map(margin_box_top)
        .fold(f32::MAX, f32::min)
}

/// The absolute Y of the bottom of a table row's margin box (the lowest among its cells).
pub(super) fn table_row_bottom(row: &LaidOutTableRow) -> f32 {
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
        group: row.group,
        fragmentation: row.fragmentation,
        first_in_group: row.first_in_group,
        last_in_group: row.last_in_group,
        boundary_above: row.boundary_above,
        break_above: None,
        break_below: None,
    }
}
