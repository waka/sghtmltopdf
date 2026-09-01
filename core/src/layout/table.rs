//! Layout of `display: table` elements (the content-based automatic column width algorithm).
//!
//! A simplified version of the automatic table layout in CSS2.1 section 17.5.2. Each cell's
//! "natural width" is measured (in practice, the width of its text laid out on one line with
//! no wrapping); the maximum natural width per column is taken; and the columns are scaled
//! proportionally to fit the containing width (scaling up when the containing width is
//! larger, so the table always fills it, matching the ordinary CSS behaviour of a
//! `width: auto` table filling its containing block).
//!
//! - `rowspan="0"` (HTML5's "extend to the end of the section" special value) is not supported and is treated as 1
//! - `border-collapse: collapse` only unifies how the borders are drawn; the layout
//!   calculation is identical to the separate model
//! - The natural width of a table, flex or grid nested inside a cell is approximated
//!   according to the meaning of its own axes (broken down in [`measure_natural_content_width`])
//! - Cell content that cannot provide a baseline for `vertical-align: baseline`
//!   (a nested table, a replaced element) falls back to the equivalent of `bottom`

use std::collections::HashMap;
use std::rc::Rc;

use crate::fonts::FontCollection;
use crate::html::NodeId;
use crate::style::{
    CaptionSide, ComputedStyle, FlexDirection, LengthPercentageOrAuto, RepeatCount, TableLayout,
    TrackComponent, TrackList, VerticalAlign,
};

use super::block::{
    box_style, clamp_used_width, layout_box, layout_box_with_forced_width, resolve_border,
    resolve_lp, resolve_padding, shift_box_y, shift_box_y_in_place, shift_content_vertical,
    LaidOutBox, LaidOutContent, LaidOutTable, LaidOutTableRow, PosCtx,
};
use super::box_tree::{BoxContent, LayoutBox, TableBox, TableCell, TableRow};
use super::float_ctx::FloatContext;
use super::inline::layout_inline_content;

/// A width that can be treated as effectively infinite, used to disable wrapping.
const UNCONSTRAINED_WIDTH: f32 = f32::MAX / 4.0;

/// Lay the table out and return the laid-out table (caption plus rows and columns) and its
/// overall height. `table_layout` is the computed `table-layout` of the `display: table`
/// element itself (a non-inherited property, read from that element's own style by the
/// caller). `h_spacing`/`v_spacing` are the resolved `border-spacing` values (the caller
/// collapses them to 0 under `border-collapse: collapse`).
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_table(
    table: &TableBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    containing_width: f32,
    table_layout: TableLayout,
    h_spacing: f32,
    v_spacing: f32,
    x: f32,
    y: f32,
    pos: &mut PosCtx,
) -> (LaidOutTable, f32) {
    // The caption is laid out independently even when there are no rows (so the empty-table-
    // plus-caption case works too, this happens before the column_count == 0 early return).
    // The caption is also taken to establish a new Block Formatting Context, keeping it
    // independent of the outer floats (the same policy as the table body's cells).
    let laid_caption = table.caption.as_deref().map(|caption| {
        let mut caption_float_ctx = FloatContext::new();
        layout_box(
            caption,
            styles,
            fonts,
            containing_width,
            &mut caption_float_ctx,
            x,
            y,
            pos,
        )
    });
    let caption_height = laid_caption
        .as_ref()
        .map(|c| c.layout.margin_box_height())
        .unwrap_or(0.0);
    let caption_is_top = table.caption_side == CaptionSide::Top;
    // With `caption-side: top`, the rows start lower by the height of the caption.
    // With `bottom`, the rows start at `y` as usual and the caption is shifted below them
    // after the rows are laid out (see below). `rows_block_start` is the start of the whole
    // row group (including the `v_spacing` before and after) and `rows_start_y` is where the first row actually goes.
    let rows_block_start = if caption_is_top {
        y + caption_height
    } else {
        y
    };
    let rows_start_y = rows_block_start + v_spacing;

    // Grid placement accounting for rowspan/colspan occupancy. Every later calculation of
    // column widths, row heights and cell placement goes through this grid. The column count
    // falls out of that placement (the maximum colspan sum per row would miss a cell placed
    // past a column filled by a rowspan).
    let (grid, column_count) = build_table_grid(&table.rows);

    if column_count == 0 {
        let (caption, total_height) = match laid_caption {
            Some(c) => (Some(Box::new(c)), caption_height),
            None => (None, 0.0),
        };
        return (
            LaidOutTable {
                caption,
                caption_side: table.caption_side,
                rows: Vec::new(),
            },
            total_height,
        );
    }

    // In the separate model, `border-spacing` goes not only between columns but also between
    // the table's outer edge and the outermost columns (CSS2.1 17.6.1), so the width
    // available for the columns is reduced by `(column count + 1)` lots of `h_spacing`.
    let available_column_width =
        (containing_width - h_spacing * (column_count + 1) as f32).max(0.0);

    // Resolve the column width hints from `<colgroup>`/`<col>` into used widths (px) at this
    // point. Any beyond the column count are discarded, and any shortfall counts as unspecified.
    let column_hints: Vec<Option<f32>> = (0..column_count)
        .map(|i| {
            table
                .column_widths
                .get(i)
                .copied()
                .flatten()
                .map(|lp| resolve_lp(lp, available_column_width))
        })
        .collect();

    // `table-layout: fixed` is a fast path that looks only at `<col>` and the explicit
    // `width` settings on the first row, skipping content measurement
    // (the cell natural widths in `compute_column_widths`) entirely.
    let col_widths = if table_layout == TableLayout::Fixed {
        compute_fixed_column_widths(
            &grid,
            styles,
            &column_hints,
            column_count,
            available_column_width,
        )
    } else {
        compute_column_widths(
            &grid,
            styles,
            fonts,
            &column_hints,
            column_count,
            available_column_width,
        )
    };
    let mut col_x = vec![0.0f32; column_count + 1];
    col_x[0] = h_spacing;
    for i in 0..column_count {
        col_x[i + 1] = col_x[i] + col_widths[i] + h_spacing;
    }

    // Pass 1: lay each cell out at a provisional position of y=0 to get its "natural
    // height", before the row heights are settled (cells spanning several rows via rowspan
    // are moved to their real positions later, all together via `shift_box_y`, once every row height is known).
    let laid_grid: Vec<Vec<LaidOutBox>> = grid
        .iter()
        .map(|row_cells| {
            row_cells
                .iter()
                .map(|gc| {
                    let outer_width: f32 = if gc.col_end > gc.col_start {
                        col_x[gc.col_end] - col_x[gc.col_start] - h_spacing
                    } else {
                        0.0
                    };
                    let cell_x = x + col_x[gc.col_start];

                    let cell_style = box_style(&gc.cell.content, styles);
                    let cell_padding = resolve_padding(&cell_style, outer_width);
                    let cell_border = resolve_border(&cell_style);
                    let content_width = (outer_width
                        - cell_padding.left
                        - cell_padding.right
                        - cell_border.left
                        - cell_border.right)
                        .max(0.0);

                    // A `display: table` cell establishes a new Block Formatting Context
                    // (CSS2.1 9.4.1), so it gets an empty context independent of the outer floats.
                    //
                    // A cell is laid out at the provisional y=0 and moved down once the row
                    // height is known, but the `absolute`s collected need no moving:
                    // an absolute position is determined solely by the containing block
                    // (static position is unsupported), and where the containing block is
                    // inside the cell, page composition corrects it by the ancestor's real offset.
                    let mut cell_float_ctx = FloatContext::new();
                    layout_box_with_forced_width(
                        &gc.cell.content,
                        styles,
                        fonts,
                        outer_width,
                        content_width,
                        &mut cell_float_ctx,
                        cell_x,
                        0.0,
                        pos,
                    )
                })
                .collect()
        })
        .collect();

    let row_count = table.rows.len();
    // Pass 2: find each row's maximum natural height using only the rowspan=1 cells
    // (the same two-pass approach as colspan column widths).
    let mut row_natural = vec![0.0f32; row_count];
    for (row_cells, laid_row) in grid.iter().zip(laid_grid.iter()) {
        for (gc, laid_cell) in row_cells.iter().zip(laid_row.iter()) {
            if gc.cell.rowspan == 1 {
                row_natural[gc.row_index] =
                    row_natural[gc.row_index].max(laid_cell.layout.margin_box_height());
            }
        }
    }
    // Pass 3: for cells spanning several rows via rowspan, if the sum of the spanned rows'
    // natural heights (plus the `v_spacing` between them, the same reasoning as colspan's
    // h_spacing) falls short of the cell's own natural height, distribute the shortfall evenly over the spanned rows.
    for (row_cells, laid_row) in grid.iter().zip(laid_grid.iter()) {
        for (gc, laid_cell) in row_cells.iter().zip(laid_row.iter()) {
            if gc.cell.rowspan > 1 {
                let end = (gc.row_index + gc.cell.rowspan).min(row_count);
                if end > gc.row_index {
                    let span_count = end - gc.row_index;
                    let span_natural_sum: f32 = row_natural[gc.row_index..end].iter().sum::<f32>()
                        + v_spacing * (span_count - 1) as f32;
                    let cell_natural = laid_cell.layout.margin_box_height();
                    if cell_natural > span_natural_sum {
                        let deficit = cell_natural - span_natural_sum;
                        let share = deficit / span_count as f32;
                        for h in &mut row_natural[gc.row_index..end] {
                            *h += share;
                        }
                    }
                }
            }
        }
    }

    // Pass 4: find each row's absolute Y position (cumulative from the start of the row group, `v_spacing` before and after included).
    let mut row_y = vec![0.0f32; row_count + 1];
    row_y[0] = rows_start_y;
    for r in 0..row_count {
        row_y[r + 1] = row_y[r] + row_natural[r] + v_spacing;
    }

    // Pass 5: translate each cell from its provisional position (y=0) to its real row
    // position and stretch it to the final height settled by rowspan. How the extra space is
    // distributed is decided by the computed `vertical-align` (four cases,
    // top/middle/bottom/baseline, CSS2.1 17.5.3).
    let mut laid_rows = Vec::with_capacity(table.rows.len());
    for (row, (row_cells, laid_row)) in table.rows.iter().zip(grid.iter().zip(laid_grid)) {
        let mut laid_cells: Vec<LaidOutBox> = row_cells
            .iter()
            .zip(laid_row)
            .map(|(gc, mut laid_cell)| {
                shift_box_y_in_place(&mut laid_cell, -row_y[gc.row_index]);
                laid_cell
            })
            .collect();

        // For baseline-aligned cells, find how far the baseline of the cell's own first line
        // sits below the top of the cell, and use the largest across the row as the row's
        // baseline position to align to (only cells starting in that row count, as CSS2.1 defines).
        let row_baseline_offset = laid_cells
            .iter()
            .zip(row_cells.iter())
            .filter(|(cell, _)| cell_vertical_align(cell, styles) == VerticalAlign::Baseline)
            .filter_map(|(cell, _)| own_baseline_offset(cell, fonts))
            .fold(0.0f32, f32::max);

        for (cell, gc) in laid_cells.iter_mut().zip(row_cells.iter()) {
            let span_end = (gc.row_index + gc.cell.rowspan).min(row_count);
            let final_height = if span_end > gc.row_index {
                row_y[span_end] - row_y[gc.row_index] - v_spacing
            } else {
                0.0
            };
            let deficit = final_height - cell.layout.margin_box_height();
            if deficit <= 0.0 {
                continue;
            }
            cell.layout.content.height += deficit;

            let shift_down = match cell_vertical_align(cell, styles) {
                VerticalAlign::Top => 0.0,
                VerticalAlign::Middle => deficit / 2.0,
                VerticalAlign::Bottom => deficit,
                // `sub`/`super`/`text-top`/`text-bottom` and lengths are inline-context-only
                // values and do not apply to a table cell in CSS2.1. They are treated as
                // `baseline`.
                VerticalAlign::Baseline
                | VerticalAlign::Sub
                | VerticalAlign::Super
                | VerticalAlign::TextTop
                | VerticalAlign::TextBottom
                | VerticalAlign::LengthPercentage(_) => match own_baseline_offset(cell, fonts) {
                    Some(own_offset) => row_baseline_offset - own_offset,
                    // Cell content that cannot provide a baseline falls back to the
                    // equivalent of `bottom` (a known simplification; see the comment at the top of this file).
                    None => deficit,
                },
            };
            if shift_down > 0.0 {
                *cell = shift_content_vertical(cell, -shift_down);
            }
        }

        laid_rows.push(LaidOutTableRow {
            node: row.node,
            cells: laid_cells,
            section: row.section,
        });
    }

    let rows_height = row_y[row_count] - rows_block_start;
    let total_height = caption_height + rows_height;

    let final_caption = match (laid_caption, caption_is_top) {
        (Some(c), true) => Some(Box::new(c)),
        (Some(c), false) => {
            // The caption is already laid out at `y`, so it is shifted below the rows (down
            // by `rows_height`). `shift_box_y`'s delta subtracts, so a negative value is passed.
            Some(Box::new(shift_box_y(&c, -rows_height)))
        }
        (None, _) => None,
    };

    (
        LaidOutTable {
            caption: final_caption,
            caption_side: table.caption_side,
            rows: laid_rows,
        },
        total_height,
    )
}

/// The computed `vertical-align` of the cell (`display: table-cell`) itself.
fn cell_vertical_align(
    cell: &LaidOutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
) -> VerticalAlign {
    cell.node
        .and_then(|n| styles.get(&n))
        .map(|s| s.vertical_align)
        .unwrap_or_default()
}

/// Find how far the baseline of the first line of the cell's content sits below the top of
/// the cell itself (`cell.layout.content.y`). Content with no text (a nested table, a
/// replaced element and so on) cannot provide one, so `None` is returned (a known
/// simplification; the caller falls back to the equivalent of `bottom`).
fn own_baseline_offset(cell: &LaidOutBox, fonts: &FontCollection) -> Option<f32> {
    let absolute = first_baseline_absolute_y(cell, fonts)?;
    Some(absolute - cell.layout.content.y)
}

/// Walk `b`'s content depth-first in document order and return the absolute Y coordinate
/// (in the same coordinate space as `b`) of the first text line's baseline.
fn first_baseline_absolute_y(b: &LaidOutBox, fonts: &FontCollection) -> Option<f32> {
    match &b.content {
        LaidOutContent::Inline(lines) => lines.iter().find_map(|line| {
            let first_run = line.runs.first()?;
            let font = fonts.get(first_run.font_index)?;
            let offset = font.baseline_offset(first_run.font_size, line.rect.height);
            Some(line.rect.y + offset)
        }),
        LaidOutContent::Blocks(children) => children
            .iter()
            .find_map(|child| first_baseline_absolute_y(child, fonts)),
        // A nested table, flex, grid or replaced element provides no baseline
        // (a known simplification).
        LaidOutContent::Table(_)
        | LaidOutContent::Flex(_)
        | LaidOutContent::Grid(_)
        | LaidOutContent::Image(_) => None,
    }
}

/// A reference to one cell in the grid, carrying its row number plus its real start and end
/// (exclusive) columns. A simplified version of "table grid construction", CSS2.1 section 17.2.
struct GridCell<'a> {
    cell: &'a TableCell,
    row_index: usize,
    col_start: usize,
    col_end: usize,
}

/// From `rows`, work out the grid placement accounting for rowspan/colspan occupancy (later
/// rows skipping columns filled by a rowspan), and the resulting column count. The grid
/// returned is a list of `GridCell` per row (the outer `Vec` is the rows, the inner one the
/// cells belonging to that row). Where every rowspan is 1, it returns exactly the same result
/// as a simple walk advancing the column cursor by `col += cell.colspan`, so the existing
/// tests (which use no rowspan) stay backward-compatible.
///
/// The column count is worked out here rather than taken from the caller because a mismatch
/// between the placement that skips rowspan-filled columns and the way the count is derived
/// silently loses cells with nowhere to go ("row 1 is a single rowspan cell, row 2 fills the rest").
fn build_table_grid(rows: &[TableRow]) -> (Vec<Vec<GridCell<'_>>>, usize) {
    // occupied[col]: how many more rows a rowspan occupies that column for (0 means free).
    // It grows whenever a placement goes past the known column count, and its final length is the column count.
    let mut occupied: Vec<usize> = Vec::new();
    let mut grid = Vec::with_capacity(rows.len());

    for (row_index, row) in rows.iter().enumerate() {
        let mut row_cells = Vec::with_capacity(row.cells.len());
        let mut col = 0usize;
        for cell in &row.cells {
            while col < occupied.len() && occupied[col] > 0 {
                col += 1;
            }
            let col_start = col;
            let col_end = col_start + cell.colspan;
            if occupied.len() < col_end {
                occupied.resize(col_end, 0);
            }
            for slot in &mut occupied[col_start..col_end] {
                *slot = cell.rowspan;
            }
            row_cells.push(GridCell {
                cell,
                row_index,
                col_start,
                col_end,
            });
            col = col_end;
        }
        grid.push(row_cells);
        // This row is done, so decrement the remaining rowspan counts
        // (consuming this row; a column reaching 0 is free again from the next row).
        for slot in &mut occupied {
            *slot = slot.saturating_sub(1);
        }
    }

    let column_count = occupied.len();
    (grid, column_count)
}

/// Column width determination for `table-layout: fixed` (a simplified version of CSS2.1
/// section 17.5.2.1). The `<col>` hints (`column_hints`) win outright, then the explicit
/// `width` (px or %) on a first-row cell becomes the total width of the columns it occupies
/// (divided evenly across them when a colspan covers several), and columns with neither
/// share the remaining width evenly. Columns absent from the first row (when the first row's
/// colspan sum falls short of `column_count`) are included in that even share. No content is measured at all.
fn compute_fixed_column_widths(
    grid: &[Vec<GridCell<'_>>],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    column_hints: &[Option<f32>],
    column_count: usize,
    containing_width: f32,
) -> Vec<f32> {
    let mut widths: Vec<Option<f32>> = vec![None; column_count];

    if let Some(first_row) = grid.first() {
        for gc in first_row {
            if gc.col_end > gc.col_start {
                let cell_style = box_style(&gc.cell.content, styles);
                if let Some(resolved) = fixed_cell_width(&cell_style, containing_width) {
                    let share = resolved / (gc.col_end - gc.col_start) as f32;
                    for w in &mut widths[gc.col_start..gc.col_end] {
                        *w = Some(share);
                    }
                }
            }
        }
    }

    // A `<col>` setting wins over a first-row cell.
    for (i, hint) in column_hints.iter().enumerate().take(column_count) {
        if let Some(hint) = hint {
            widths[i] = Some(*hint);
        }
    }

    let specified_sum: f32 = widths.iter().filter_map(|w| *w).sum();
    let auto_count = widths.iter().filter(|w| w.is_none()).count();
    let remaining = (containing_width - specified_sum).max(0.0);
    let auto_share = if auto_count > 0 {
        remaining / auto_count as f32
    } else {
        0.0
    };

    widths.iter().map(|w| w.unwrap_or(auto_share)).collect()
}

/// Find each column's used width. The maximum per column of the "natural width" derived
/// from the cells' content is scaled proportionally to fit the containing width exactly.
///
/// A column with a `<col>` hint (`column_hints`) is fixed at that width, and the remaining
/// width is distributed over the unhinted columns in proportion to their natural widths.
fn compute_column_widths(
    grid: &[Vec<GridCell<'_>>],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    column_hints: &[Option<f32>],
    column_count: usize,
    containing_width: f32,
) -> Vec<f32> {
    let mut natural = vec![0.0f32; column_count];

    // Pass 1: find each column's maximum natural width using only the colspan=1 cells.
    for row_cells in grid {
        for gc in row_cells {
            if gc.col_end - gc.col_start == 1 {
                natural[gc.col_start] =
                    natural[gc.col_start].max(natural_cell_width(gc.cell, styles, fonts));
            }
        }
    }

    // Pass 2: for cells spanning several columns via colspan, if the sum of the spanned
    // columns' natural widths falls short of the cell's own natural width, distribute the shortfall evenly over them.
    for row_cells in grid {
        for gc in row_cells {
            if gc.col_end - gc.col_start > 1 {
                let span_natural_sum: f32 = natural[gc.col_start..gc.col_end].iter().sum();
                let cell_natural = natural_cell_width(gc.cell, styles, fonts);
                if cell_natural > span_natural_sum {
                    let deficit = cell_natural - span_natural_sum;
                    let share = deficit / (gc.col_end - gc.col_start) as f32;
                    for w in &mut natural[gc.col_start..gc.col_end] {
                        *w += share;
                    }
                }
            }
        }
    }

    let has_hint = column_hints
        .iter()
        .take(column_count)
        .any(|hint| hint.is_some());
    if has_hint {
        return distribute_with_column_hints(
            &natural,
            column_hints,
            column_count,
            containing_width,
        );
    }

    let natural_sum: f32 = natural.iter().sum();
    if natural_sum > 0.0 {
        let scale = containing_width / natural_sum;
        natural.iter().map(|w| w * scale).collect()
    } else {
        vec![containing_width / column_count as f32; column_count]
    }
}

/// Under `table-layout: fixed`, the width a first-row cell contributes to its column.
///
/// * `width` given -> the value clamped by `min-width`/`max-width`
/// * `width: auto` with only `min-width` given -> `min-width` becomes the column's width
/// * neither (including a `max-width`-only setting) -> `None` (left to the even share of the remaining width)
fn fixed_cell_width(cell_style: &ComputedStyle, containing_width: f32) -> Option<f32> {
    match cell_style.width {
        LengthPercentageOrAuto::LengthPercentage(lp) => Some(clamp_used_width(
            cell_style,
            containing_width,
            0.0,
            0.0,
            resolve_lp(lp, containing_width),
        )),
        // The initial value of `min-width` is `0`. A 0 counts as "unspecified".
        LengthPercentageOrAuto::Auto => {
            let min = resolve_lp(cell_style.min_width, containing_width);
            (min > 0.0).then_some(min)
        }
    }
}

/// Fix the columns that have a `<col>` hint and distribute the rest over the unhinted
/// columns in proportion to their natural widths. When the hints add up to more than
/// `containing_width`, only the hinted columns are scaled down proportionally to fit.
fn distribute_with_column_hints(
    natural: &[f32],
    column_hints: &[Option<f32>],
    column_count: usize,
    containing_width: f32,
) -> Vec<f32> {
    let hint_of = |i: usize| column_hints.get(i).copied().flatten();
    let hint_sum: f32 = (0..column_count).filter_map(hint_of).sum();

    if hint_sum > containing_width && hint_sum > 0.0 {
        let scale = containing_width / hint_sum;
        return (0..column_count)
            .map(|i| hint_of(i).map(|w| w * scale).unwrap_or(0.0))
            .collect();
    }

    let auto_natural_sum: f32 = (0..column_count)
        .filter(|&i| hint_of(i).is_none())
        .map(|i| natural[i])
        .sum();
    let remaining = (containing_width - hint_sum).max(0.0);

    (0..column_count)
        .map(|i| match hint_of(i) {
            Some(w) => w,
            None if auto_natural_sum > 0.0 => remaining * natural[i] / auto_natural_sum,
            None => {
                let auto_count = (0..column_count).filter(|&i| hint_of(i).is_none()).count();
                remaining / auto_count as f32
            }
        })
        .collect()
}

/// One cell's "natural width" (its content laid out without wrapping, plus padding and border).
///
/// The cell's own `min-width`/`max-width` are applied here as a clamp. A column's natural
/// width is the maximum of the clamped values, but the proportional scaling that follows
/// (fitting the table to the paper width) still happens, so the final column width does not guarantee `min-width`.
fn natural_cell_width(
    cell: &TableCell,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
) -> f32 {
    let style = box_style(&cell.content, styles);
    // A percentage padding cannot resolve here, before layout is settled, because the basis
    // width is unknown, so it resolves against 0 (a simplification).
    let padding = resolve_padding(&style, 0.0);
    let border = resolve_border(&style);
    // The clamp is applied to the content width (min/max are content-box based) and
    // padding/border are added afterwards. The percentage basis for min/max is also unknown
    // at this point, so it too resolves against 0 (the same simplification as padding).
    let content_natural = measure_natural_content_width(&cell.content, styles, fonts);
    let clamped = clamp_used_width(
        &style,
        0.0,
        padding.left + padding.right,
        border.left + border.right,
        content_natural,
    );
    clamped + padding.left + padding.right + border.left + border.right
}

/// One child box's natural width plus that child's own padding and border.
/// A percentage resolves against 0, the basis width being unknown at this point
/// (the same simplification as `natural_cell_width`). Margins are not included.
fn outer_natural_width(
    child: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
) -> f32 {
    let style = box_style(child, styles);
    let padding = resolve_padding(&style, 0.0);
    let border = resolve_border(&style);
    measure_natural_content_width(child, styles, fonts)
        + padding.left
        + padding.right
        + border.left
        + border.right
}

/// The number of tracks `grid-template-columns` declares.
/// `repeat(auto-fill|auto-fit, ...)` counts as a single repetition under a min/max-content
/// constraint (as the CSS Grid spec prescribes).
fn declared_column_count(list: &TrackList) -> usize {
    list.components
        .iter()
        .map(|component| match component {
            TrackComponent::Single(_) => 1,
            TrackComponent::Repeat { count, tracks, .. } => match count {
                RepeatCount::Count(n) => *n as usize * tracks.len(),
                RepeatCount::AutoFill | RepeatCount::AutoFit => tracks.len(),
            },
        })
        .sum()
}

/// Measure a box's natural width (its max-content width): the width its content would take
/// laid out with no wrapping. Shared by the table's automatic column width algorithm, the
/// taffy measure bridge in `layout::flex`, and shrink-to-fit width (`width: auto` on a float, inline-block or absolutely positioned box).
///
/// A nested table, flex or grid is measured recursively according to the meaning of its own
/// axes. The breakdown is not the CSS specification itself but the following approximation,
/// all of it staying within "what can be found without actually solving item placement".
///
/// * flex: the sum of the items plus `column-gap` when the main axis is horizontal, the maximum when it is vertical
/// * grid: rows are cut at the number of tracks `grid-template-columns` declares, and the
///   maximum row sum is taken. `repeat(auto-fill|auto-fit, ...)` counts as one repetition, per the CSS spec.
///   Explicit placement (`grid-column`) and items spanning several tracks are not considered
/// * table: the maximum per row of the sum of the cell widths (`border-spacing` excluded)
///
/// The result is memoised on [`LayoutBox::natural_width`]. The same subtree is re-measured
/// from every ancestor level, and again from the throwaway layouts for each candidate width,
/// so without the memo the cost grows exponentially in the nesting depth.
pub(super) fn measure_natural_content_width(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
) -> f32 {
    if let Some(memo) = b.measured.natural_width() {
        return memo;
    }
    let width = compute_natural_content_width(b, styles, fonts);
    b.measured.set_natural_width(width);
    width
}

fn compute_natural_content_width(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
) -> f32 {
    match &b.content {
        BoxContent::Inline(spans) => {
            // This is a measuring pass, so any `absolute` among an `inline-block`'s
            // descendants is discarded (the final layout pass walks the same descendants and collects them).
            let mut discarded = Vec::new();
            let mut pos = PosCtx::new(&mut discarded, (0.0, 0.0));
            let lines = layout_inline_content(
                spans.as_slice(),
                styles,
                fonts,
                UNCONSTRAINED_WIDTH,
                0.0,
                0.0,
                None,
                // Not needed in a measuring pass: the width is unbounded, so `text-align` has no slack to distribute.
                None,
                &mut pos,
            );
            lines.iter().map(|l| l.rect.width).fold(0.0f32, f32::max)
        }
        BoxContent::Blocks(children) => children
            .iter()
            .map(|child| outer_natural_width(child, styles, fonts))
            .fold(0.0f32, f32::max),
        BoxContent::Flex(flex) => {
            let style = box_style(b, styles);
            let items: Vec<f32> = flex
                .items
                .iter()
                .map(|item| outer_natural_width(item, styles, fonts))
                .collect();
            match style.flex_direction {
                FlexDirection::Row | FlexDirection::RowReverse => {
                    let gaps =
                        resolve_lp(style.column_gap, 0.0) * (items.len().saturating_sub(1)) as f32;
                    items.iter().sum::<f32>() + gaps
                }
                // They stack vertically, so the widest item is the natural width outright.
                FlexDirection::Column | FlexDirection::ColumnReverse => {
                    items.into_iter().fold(0.0f32, f32::max)
                }
            }
        }
        BoxContent::Grid(grid) => {
            let style = box_style(b, styles);
            // With no declared tracks (`grid-template-columns: none`) there is one column.
            // Items flow along the row axis, so with one column each item takes a row.
            let columns = declared_column_count(&style.grid_template_columns).max(1);
            let gaps = resolve_lp(style.column_gap, 0.0) * (columns.saturating_sub(1)) as f32;
            grid.items
                .chunks(columns)
                .map(|row| {
                    row.iter()
                        .map(|item| outer_natural_width(item, styles, fonts))
                        .sum::<f32>()
                        + gaps
                })
                .fold(0.0f32, f32::max)
        }
        BoxContent::Table(table) => table
            .rows
            .iter()
            .map(|row| {
                row.cells
                    .iter()
                    .map(|cell| natural_cell_width(cell, styles, fonts))
                    .sum::<f32>()
            })
            .fold(0.0f32, f32::max),
        BoxContent::Image(image_content) => image_content
            .attr_width
            .map(|w| w as f32)
            .or_else(|| image_content.image.as_ref().map(|img| img.width))
            .unwrap_or(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::super::block::{layout_document, LaidOutBox};
    use super::super::box_tree::{build_box_tree, LayoutBox};
    use super::*;
    use crate::fonts::Font;
    use crate::html::{self, Dom, NodeData};
    use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

    const TEST_FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    fn test_fonts() -> FontCollection {
        FontCollection::new(vec![
            Font::load(TEST_FONT_PATH).expect("should load bundled test font")
        ])
    }

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    /// A test-only `find_laid_out` that also descends into a `Table` (the caption included).
    fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
        if b.node == Some(target) {
            return Some(b);
        }
        match &b.content {
            super::super::block::LaidOutContent::Blocks(children) => children
                .iter()
                .find_map(|child| find_laid_out(child, target)),
            super::super::block::LaidOutContent::Grid(grid) => grid
                .rows
                .iter()
                .flat_map(|row| &row.items)
                .find_map(|item| find_laid_out(item, target)),
            super::super::block::LaidOutContent::Table(table) => table
                .caption
                .as_deref()
                .and_then(|caption| find_laid_out(caption, target))
                .or_else(|| {
                    table
                        .rows
                        .iter()
                        .flat_map(|row| &row.cells)
                        .find_map(|cell| find_laid_out(cell, target))
                }),
            super::super::block::LaidOutContent::Flex(children) => children
                .iter()
                .find_map(|child| find_laid_out(child, target)),
            super::super::block::LaidOutContent::Inline(_)
            | super::super::block::LaidOutContent::Image(_) => None,
        }
    }

    fn layout_table_html(html_src: &str, css: &str, containing_width: f32) -> LaidOutBox {
        let dom = html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(css);
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let fonts = test_fonts();
        let laid = layout_document(&tree, &styles, &fonts, containing_width);
        let table_node = find(&dom, dom.document(), "table").expect("table not found");
        find_laid_out(&laid, table_node)
            .expect("table box not found")
            .clone()
    }

    fn cell_widths(table: &LaidOutBox, row: usize) -> Vec<f32> {
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        laid_table.rows[row]
            .cells
            .iter()
            .map(|c| c.layout.border_box().width)
            .collect()
    }

    fn cell_lefts(table: &LaidOutBox, row: usize) -> Vec<f32> {
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        laid_table.rows[row]
            .cells
            .iter()
            .map(|c| c.layout.border_box().x)
            .collect()
    }

    fn row_tops(table: &LaidOutBox) -> Vec<f32> {
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        laid_table
            .rows
            .iter()
            .map(|row| row.cells[0].layout.border_box().y)
            .collect()
    }

    fn row_cells(table: &LaidOutBox, row: usize) -> &[LaidOutBox] {
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        &laid_table.rows[row].cells
    }

    fn first_line_y(cell: &LaidOutBox) -> f32 {
        let super::super::block::LaidOutContent::Inline(lines) = &cell.content else {
            panic!("expected inline content");
        };
        lines[0].rect.y
    }

    /// Walk `b`'s content and return the `LaidOutTable` of the first nested table found.
    fn find_nested_table(b: &LaidOutBox) -> Option<&LaidOutTable> {
        match &b.content {
            super::super::block::LaidOutContent::Table(table) => Some(table),
            super::super::block::LaidOutContent::Blocks(children)
            | super::super::block::LaidOutContent::Flex(children) => {
                children.iter().find_map(find_nested_table)
            }
            super::super::block::LaidOutContent::Grid(grid) => grid
                .rows
                .iter()
                .flat_map(|row| &row.items)
                .find_map(find_nested_table),
            super::super::block::LaidOutContent::Inline(_)
            | super::super::block::LaidOutContent::Image(_) => None,
        }
    }

    #[test]
    fn table_stretches_to_fill_the_containing_width() {
        // Cancel out body's default margin (from the UA stylesheet) so the containing width
        // reaches the table unchanged.
        let table = layout_table_html(
            "<table><tr><td>a</td><td>bb</td></tr></table>",
            "body { margin: 0; }",
            700.0,
        );
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        let total_width: f32 = laid_table.rows[0]
            .cells
            .iter()
            .map(|c| c.layout.border_box().width)
            .sum();
        assert!(
            (total_width - 700.0).abs() < 0.5,
            "table should stretch to fill the containing width, got {total_width}"
        );
    }

    #[test]
    fn wider_content_gets_a_proportionally_wider_column() {
        let table = layout_table_html(
            "<table><tr><td>x</td><td>a much much much longer piece of text</td></tr></table>",
            "",
            700.0,
        );
        let widths = cell_widths(&table, 0);
        assert!(
            widths[1] > widths[0] * 3.0,
            "the column with much longer content should be proportionally wider: {widths:?}"
        );
    }

    #[test]
    fn equal_content_produces_roughly_equal_columns() {
        // The same number of characters can still give different glyph widths (the advance of
        // 'a' and 'b' need not be equal), so verifying that the natural widths really are
        // identical requires using the same text.
        let table = layout_table_html(
            "<table><tr><td>identical</td><td>identical</td></tr></table>",
            "",
            700.0,
        );
        let widths = cell_widths(&table, 0);
        assert!(
            (widths[0] - widths[1]).abs() < 0.5,
            "identical content should produce identical column widths: {widths:?}"
        );
    }

    #[test]
    fn colspan_cell_widens_the_columns_it_spans() {
        // A three-column table: row 1 is a wide heading spanning the first two columns plus a
        // narrow cell in column 3; row 2 has the same narrow content in all three ("x"/"y"/"w").
        // On their own content (x/y), columns 0 and 1 would be the same width as column 2 (w),
        // but they are widened to accommodate row 1's wide colspan cell and should end up
        // clearly wider than column 2.
        let table = layout_table_html(
            r#"<table>
                <tr><td colspan="2">a much much much longer heading spanning both columns nicely</td><td>z</td></tr>
                <tr><td>x</td><td>y</td><td>w</td></tr>
            </table>"#,
            "",
            700.0,
        );
        let row1_widths = cell_widths(&table, 1);
        assert!(
            row1_widths[0] + row1_widths[1] > row1_widths[2] * 3.0,
            "columns spanned by the wide header should be widened relative to the untouched column: {row1_widths:?}"
        );
    }

    #[test]
    fn row_height_is_the_tallest_cell_in_that_row() {
        let table = layout_table_html(
            r#"<table>
                <tr><td style="height: 10px;">a</td><td style="height: 80px;">b</td></tr>
            </table>"#,
            "",
            700.0,
        );
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        for cell in &laid_table.rows[0].cells {
            assert_eq!(
                cell.layout.margin_box_height(),
                80.0,
                "every cell in the row should occupy the tallest cell's height"
            );
        }
    }

    #[test]
    fn cells_in_the_same_row_are_placed_side_by_side() {
        let table = layout_table_html(
            "<table><tr><td>a</td><td>b</td><td>c</td></tr></table>",
            "",
            700.0,
        );
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        let cells = &laid_table.rows[0].cells;
        for pair in cells.windows(2) {
            assert_eq!(
                pair[1].layout.border_box().x,
                pair[0].layout.border_box().x + pair[0].layout.border_box().width,
                "adjacent cells should touch with no gap"
            );
        }
    }

    #[test]
    fn empty_table_has_no_rows_and_zero_height() {
        let table = layout_table_html("<table></table>", "", 700.0);
        let super::super::block::LaidOutContent::Table(laid_table) = &table.content else {
            panic!("expected a laid-out table");
        };
        assert!(laid_table.rows.is_empty());
        assert_eq!(table.layout.content.height, 0.0);
    }

    #[test]
    fn table_layout_fixed_uses_first_row_widths_and_ignores_content() {
        // The first row's width settings (200px, 500px) become the column widths outright,
        // with the content length ignored entirely (short content such as row 2's "x" must
        // not change the column widths).
        let table = layout_table_html(
            r#"<table style="table-layout: fixed;">
                <tr><td style="width: 200px;">a</td><td style="width: 500px;">a much much much longer piece of text</td></tr>
                <tr><td>x</td><td>x</td></tr>
            </table>"#,
            "",
            700.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 200.0).abs() < 0.5, "widths: {widths:?}");
        assert!((widths[1] - 500.0).abs() < 0.5, "widths: {widths:?}");
    }

    #[test]
    fn table_layout_fixed_distributes_remaining_width_to_auto_columns() {
        let table = layout_table_html(
            r#"<table style="table-layout: fixed;">
                <tr><td style="width: 100px;">a</td><td>b</td><td>c</td></tr>
            </table>"#,
            "body { margin: 0; }",
            700.0,
        );
        let widths = cell_widths(&table, 0);
        // The remaining 600px is split evenly between the two auto columns = 300px each.
        assert!((widths[0] - 100.0).abs() < 0.5, "widths: {widths:?}");
        assert!((widths[1] - 300.0).abs() < 0.5, "widths: {widths:?}");
        assert!((widths[2] - 300.0).abs() < 0.5, "widths: {widths:?}");
    }

    #[test]
    fn table_layout_fixed_splits_a_colspan_width_evenly_across_spanned_columns() {
        let table = layout_table_html(
            r#"<table style="table-layout: fixed;">
                <tr><td colspan="2" style="width: 400px;">a</td></tr>
            </table>"#,
            "",
            700.0,
        );
        let widths = cell_widths(&table, 0);
        // 400px split evenly over 2 columns = 200px each; the colspan cell itself is 400px wide.
        assert!((widths[0] - 400.0).abs() < 0.5, "widths: {widths:?}");
    }

    #[test]
    fn border_spacing_adds_horizontal_gaps_between_and_around_columns() {
        // Containing width 700px, horizontal border-spacing 20px, 2 columns (equal content).
        // Width available to the columns = 700 - 20*3 (column count + 1 gaps) = 640 -> 320px each.
        let table = layout_table_html(
            "<table><tr><td>identical</td><td>identical</td></tr></table>",
            "body { margin: 0; } table { border-spacing: 20px 0; }",
            700.0,
        );
        let widths = cell_widths(&table, 0);
        let lefts = cell_lefts(&table, 0);
        assert!((widths[0] - 320.0).abs() < 0.5, "widths: {widths:?}");
        assert!((widths[1] - 320.0).abs() < 0.5, "widths: {widths:?}");
        assert!(
            (lefts[0] - 20.0).abs() < 0.5,
            "the first column should be inset by one spacing unit: {lefts:?}"
        );
        assert!(
            (lefts[1] - 360.0).abs() < 0.5,
            "the second column should start after column0 + 2 spacing units: {lefts:?}"
        );
    }

    #[test]
    fn border_spacing_adds_vertical_gaps_between_and_around_rows() {
        let table = layout_table_html(
            r#"<table>
                <tr><td style="height: 30px;">a</td></tr>
                <tr><td style="height: 30px;">b</td></tr>
            </table>"#,
            "body { margin: 0; } table { border-spacing: 0 15px; }",
            700.0,
        );
        let tops = row_tops(&table);
        assert!(
            (tops[0] - 15.0).abs() < 0.5,
            "the first row should be inset by one spacing unit: {tops:?}"
        );
        assert!(
            (tops[1] - 60.0).abs() < 0.5,
            "the second row should start after row0(30px) + 2 spacing units: {tops:?}"
        );
        // The overall height should include the `v_spacing` before and after: 15+30+15+30+15=105.
        assert!(
            (table.layout.content.height - 105.0).abs() < 0.5,
            "table height should include leading/trailing spacing: {}",
            table.layout.content.height
        );
    }

    #[test]
    fn border_collapse_forces_border_spacing_to_zero() {
        // Even with `border-spacing` given explicitly, `border-collapse: collapse` ignores it
        // and the cells should abut with no gap (the two are mutually exclusive).
        let table = layout_table_html(
            "<table><tr><td>a</td><td>b</td></tr></table>",
            "body { margin: 0; } table { border-spacing: 20px; border-collapse: collapse; }",
            700.0,
        );
        let lefts = cell_lefts(&table, 0);
        let widths = cell_widths(&table, 0);
        assert!(
            lefts[0].abs() < 0.5,
            "with collapse the first column should touch the table's edge: {lefts:?}"
        );
        assert!(
            (lefts[1] - (lefts[0] + widths[0])).abs() < 0.5,
            "with collapse adjacent cells should touch with no gap: lefts={lefts:?} widths={widths:?}"
        );
    }

    #[test]
    fn vertical_align_top_keeps_content_flush_with_the_row_top() {
        let table = layout_table_html(
            r#"<table>
                <tr><td style="height: 10px;">a</td><td style="height: 80px;">b</td></tr>
            </table>"#,
            "body { margin: 0; } td { vertical-align: top; }",
            700.0,
        );
        for cell in row_cells(&table, 0) {
            assert_eq!(
                first_line_y(cell),
                0.0,
                "top-aligned content should stay flush with the row top"
            );
        }
    }

    #[test]
    fn vertical_align_bottom_pushes_the_shorter_cells_content_to_the_bottom() {
        let table = layout_table_html(
            r#"<table>
                <tr><td style="height: 10px;">a</td><td style="height: 80px;">b</td></tr>
            </table>"#,
            "body { margin: 0; } td { vertical-align: bottom; }",
            700.0,
        );
        let cells = row_cells(&table, 0);
        assert!(
            first_line_y(&cells[1]).abs() < 0.5,
            "the tallest cell defines the row height so its own content shouldn't shift: {}",
            first_line_y(&cells[1])
        );
        assert!(
            (first_line_y(&cells[0]) - 70.0).abs() < 0.5,
            "the shorter cell's content should be pushed down by the full deficit(80-10=70px): {}",
            first_line_y(&cells[0])
        );
    }

    #[test]
    fn vertical_align_middle_centers_the_shorter_cells_content() {
        let table = layout_table_html(
            r#"<table>
                <tr><td style="height: 10px;">a</td><td style="height: 80px;">b</td></tr>
            </table>"#,
            "body { margin: 0; } td { vertical-align: middle; }",
            700.0,
        );
        let cells = row_cells(&table, 0);
        assert!(
            first_line_y(&cells[1]).abs() < 0.5,
            "the tallest cell defines the row height so its own content shouldn't shift: {}",
            first_line_y(&cells[1])
        );
        assert!(
            (first_line_y(&cells[0]) - 35.0).abs() < 0.5,
            "the shorter cell's content should be pushed down by half the deficit((80-10)/2=35px): {}",
            first_line_y(&cells[0])
        );
    }

    #[test]
    fn vertical_align_baseline_aligns_first_lines_of_cells_with_different_font_sizes() {
        // Even between cells with different font sizes (and so different line heights), the
        // text baselines themselves should line up at the same Y (CSS2.1 17.5.3 baseline alignment).
        let table = layout_table_html(
            r#"<table>
                <tr><td style="font-size: 12px;">Ay</td><td style="font-size: 36px;">Ay</td></tr>
            </table>"#,
            "body { margin: 0; } td { vertical-align: baseline; }",
            700.0,
        );
        let fonts = test_fonts();
        let cells = row_cells(&table, 0);
        let baseline_y = |cell: &LaidOutBox| {
            let super::super::block::LaidOutContent::Inline(lines) = &cell.content else {
                panic!("expected inline content");
            };
            let run = lines[0].runs.first().expect("cell should have text");
            let font = fonts.get(run.font_index).expect("font should be loaded");
            lines[0].rect.y + font.baseline_offset(run.font_size, lines[0].rect.height)
        };

        let small_baseline = baseline_y(&cells[0]);
        let large_baseline = baseline_y(&cells[1]);
        assert!(
            (small_baseline - large_baseline).abs() < 0.5,
            "baseline-aligned cells should share the same baseline Y: small={small_baseline} large={large_baseline}"
        );
    }

    #[test]
    fn vertical_align_baseline_falls_back_to_bottom_for_content_without_a_baseline() {
        // A nested table has no baseline (a known simplification), so it should fall back to
        // the equivalent of `bottom`.
        let table = layout_table_html(
            r#"<table>
                <tr>
                    <td style="height: 80px;">a</td>
                    <td style="height: 10px;"><table><tr><td>nested</td></tr></table></td>
                </tr>
            </table>"#,
            "body { margin: 0; } td { vertical-align: baseline; }",
            700.0,
        );
        let cells = row_cells(&table, 0);
        let nested_top_y = find_nested_table(&cells[1])
            .expect("expected the outer cell to contain a nested table")
            .rows[0]
            .cells[0]
            .layout
            .border_box()
            .y;
        assert!(
            (nested_top_y - 70.0).abs() < 0.5,
            "content without a baseline should fall back to bottom alignment (deficit=80-10=70px): {nested_top_y}"
        );
    }

    fn grid_cell(colspan: usize, rowspan: usize) -> TableCell {
        TableCell {
            node: Some(NodeId(0)),
            colspan,
            rowspan,
            content: LayoutBox::anonymous(BoxContent::Inline(Vec::new())),
        }
    }

    fn grid_row(cells: Vec<TableCell>) -> TableRow {
        TableRow {
            node: Some(NodeId(0)),
            cells,
            section: super::super::box_tree::TableSection::Body,
        }
    }

    /// Convert to a list of `(col_start, col_end)` to make the grid construction easy to check.
    fn grid_spans(grid: &[Vec<GridCell<'_>>]) -> Vec<Vec<(usize, usize)>> {
        grid.iter()
            .map(|row| row.iter().map(|gc| (gc.col_start, gc.col_end)).collect())
            .collect()
    }

    #[test]
    fn build_table_grid_matches_the_naive_colspan_walk_when_rowspan_is_always_one() {
        let rows = vec![
            grid_row(vec![grid_cell(2, 1), grid_cell(1, 1)]),
            grid_row(vec![grid_cell(1, 1), grid_cell(1, 1), grid_cell(1, 1)]),
        ];
        let (grid, column_count) = build_table_grid(&rows);
        assert_eq!(column_count, 3);
        assert_eq!(
            grid_spans(&grid),
            vec![vec![(0, 2), (2, 3)], vec![(0, 1), (1, 2), (2, 3)]]
        );
    }

    #[test]
    fn build_table_grid_skips_columns_occupied_by_a_rowspan_cell() {
        // Row 0: col0 is rowspan=2 and occupies two rows; col1 is an ordinary cell.
        // Row 1: only one cell, but col0 is still filled by the rowspan, so it should be
        // placed from col1.
        // Row 2: the rowspan has expired, so placement should start from col0 again.
        let rows = vec![
            grid_row(vec![grid_cell(1, 2), grid_cell(1, 1)]),
            grid_row(vec![grid_cell(1, 1)]),
            grid_row(vec![grid_cell(1, 1), grid_cell(1, 1)]),
        ];
        let (grid, column_count) = build_table_grid(&rows);
        assert_eq!(column_count, 2);
        assert_eq!(
            grid_spans(&grid),
            vec![vec![(0, 1), (1, 2)], vec![(1, 2)], vec![(0, 1), (1, 2)]]
        );
    }

    #[test]
    fn build_table_grid_handles_rowspan_and_colspan_combined() {
        // Row 0: a cell spanning col0..2 (colspan=2) with rowspan=2, occupying two rows.
        // Row 1: both col0 and col1 are filled, so the next cell is placed from col2.
        let rows = vec![
            grid_row(vec![grid_cell(2, 2), grid_cell(1, 1)]),
            grid_row(vec![grid_cell(1, 1)]),
        ];
        let (grid, column_count) = build_table_grid(&rows);
        assert_eq!(column_count, 3);
        assert_eq!(grid_spans(&grid), vec![vec![(0, 2), (2, 3)], vec![(2, 3)]]);
    }

    #[test]
    fn build_table_grid_counts_a_column_that_only_exists_after_a_rowspan_is_skipped() {
        // Row 0 is a single rowspan=2 cell. Its colspan sum is 1, but row 1's cell goes to
        // col1 because col0 is still filled, so the table has 2 columns.
        let rows = vec![
            grid_row(vec![grid_cell(1, 2)]),
            grid_row(vec![grid_cell(1, 1)]),
        ];
        let (grid, column_count) = build_table_grid(&rows);
        assert_eq!(
            column_count, 2,
            "the column opened up next to the rowspan cell must be counted"
        );
        assert_eq!(grid_spans(&grid), vec![vec![(0, 1)], vec![(1, 2)]]);
    }

    #[test]
    fn rowspan_cell_spans_the_full_height_of_the_rows_it_covers() {
        // "tall" (rowspan=2, an explicit 80px) exceeds the natural heights of rows 0 and 1
        // (which have only 10px cells), so both rows should expand to 40px each and "tall"
        // itself should end up exactly their sum (= 80px).
        let table = layout_table_html(
            r#"<table>
                <tr><td rowspan="2" style="height: 80px;">tall</td><td style="height: 10px;">a</td></tr>
                <tr><td style="height: 10px;">b</td></tr>
            </table>"#,
            "body { margin: 0; }",
            700.0,
        );
        let row0 = row_cells(&table, 0);
        let row1 = row_cells(&table, 1);
        assert_eq!(row1.len(), 1, "row1 should only have its own single cell");

        assert!(
            (row0[0].layout.margin_box_height() - 80.0).abs() < 0.5,
            "the rowspan cell should span exactly the combined height of both rows: {}",
            row0[0].layout.margin_box_height()
        );
        assert!(
            (row0[1].layout.margin_box_height() - 40.0).abs() < 0.5,
            "row0's non-spanning cell should be stretched to row0's height(40px): {}",
            row0[1].layout.margin_box_height()
        );
        assert!(
            (row1[0].layout.border_box().y - 40.0).abs() < 0.5,
            "row1 should start after row0's height(40px): {}",
            row1[0].layout.border_box().y
        );
    }

    #[test]
    fn rowspan_cell_makes_the_following_row_skip_its_occupied_column() {
        // Row 1 has only one cell, but col0 is occupied by row 0's rowspan cell, so it
        // should be placed in col1 (the same column as row 0's second cell).
        let table = layout_table_html(
            r#"<table>
                <tr><td rowspan="2">tall</td><td>a</td></tr>
                <tr><td>b</td></tr>
            </table>"#,
            "body { margin: 0; }",
            700.0,
        );
        let row0_lefts = cell_lefts(&table, 0);
        let row1 = row_cells(&table, 1);
        assert!(
            (row1[0].layout.border_box().x - row0_lefts[1]).abs() < 0.5,
            "row1's single cell should land in column1 (same x as row0's second cell), not column0: row1_x={} row0_col1_x={}",
            row1[0].layout.border_box().x,
            row0_lefts[1]
        );
    }

    // ===== <colgroup>/<col> (column width settings) =====

    #[test]
    fn col_width_fixes_the_column_width_in_auto_layout() {
        // Set border-spacing to 0 so the column width and the cell's border-box width compare directly.
        let table = layout_table_html(
            r#"<table>
                 <colgroup><col style="width: 100px;"><col></colgroup>
                 <tr><td>a</td><td>bbbbbbbbbbbbbbbb</td></tr>
               </table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 100.0).abs() < 0.5, "got {widths:?}");
        // The unspecified columns get all of the remaining width.
        assert!((widths[1] - 400.0).abs() < 0.5, "got {widths:?}");
    }

    #[test]
    fn col_percentage_width_resolves_against_the_table_width() {
        let table = layout_table_html(
            r#"<table>
                 <colgroup><col style="width: 20%;"><col></colgroup>
                 <tr><td>a</td><td>b</td></tr>
               </table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 100.0).abs() < 0.5, "got {widths:?}");
    }

    #[test]
    fn col_span_applies_the_same_width_to_several_columns() {
        let table = layout_table_html(
            r#"<table>
                 <colgroup><col span="2" style="width: 50px;"><col></colgroup>
                 <tr><td>a</td><td>b</td><td>c</td></tr>
               </table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 50.0).abs() < 0.5, "got {widths:?}");
        assert!((widths[1] - 50.0).abs() < 0.5, "got {widths:?}");
        assert!((widths[2] - 400.0).abs() < 0.5, "got {widths:?}");
    }

    #[test]
    fn colgroup_span_without_col_children_defines_the_columns_itself() {
        let table = layout_table_html(
            r#"<table>
                 <colgroup span="2" style="width: 60px;"></colgroup>
                 <tr><td>a</td><td>b</td><td>c</td></tr>
               </table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 60.0).abs() < 0.5, "got {widths:?}");
        assert!((widths[1] - 60.0).abs() < 0.5, "got {widths:?}");
        assert!((widths[2] - 380.0).abs() < 0.5, "got {widths:?}");
    }

    #[test]
    fn columns_without_a_col_hint_share_the_rest_proportionally_to_their_content() {
        let table = layout_table_html(
            r#"<table>
                 <colgroup><col style="width: 100px;"><col><col></colgroup>
                 <tr><td>a</td><td>short</td><td>a much much much longer cell</td></tr>
               </table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 100.0).abs() < 0.5, "got {widths:?}");
        assert!(
            widths[2] > widths[1],
            "the column with more content should get more of the remaining width: {widths:?}"
        );
        let total: f32 = widths.iter().sum();
        assert!((total - 500.0).abs() < 1.0, "got {widths:?}");
    }

    #[test]
    fn col_hints_wider_than_the_table_are_scaled_down_to_fit() {
        // When the settings add up to more than the available width, only the specified columns are scaled down.
        let table = layout_table_html(
            r#"<table>
                 <colgroup><col style="width: 600px;"><col style="width: 200px;"></colgroup>
                 <tr><td>a</td><td>b</td></tr>
               </table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            400.0,
        );
        let widths = cell_widths(&table, 0);
        assert!((widths[0] - 300.0).abs() < 0.5, "got {widths:?}");
        assert!((widths[1] - 100.0).abs() < 0.5, "got {widths:?}");
    }

    #[test]
    fn col_width_takes_precedence_over_the_first_row_cell_in_fixed_layout() {
        let table = layout_table_html(
            r#"<table>
                 <colgroup><col style="width: 300px;"><col></colgroup>
                 <tr><td style="width: 100px;">a</td><td>b</td></tr>
               </table>"#,
            "body { margin: 0; } table { table-layout: fixed; border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        assert!(
            (widths[0] - 300.0).abs() < 0.5,
            "the <col> width must win over the first row cell: {widths:?}"
        );
        assert!((widths[1] - 200.0).abs() < 0.5, "got {widths:?}");
    }

    #[test]
    fn a_table_without_colgroup_keeps_the_previous_behaviour() {
        // Regression check: with no hints at all, every column is scaled proportionally as before.
        let table = layout_table_html(
            r#"<table><tr><td>a</td><td>a much much much longer cell</td></tr></table>"#,
            "body { margin: 0; } table { border-spacing: 0; }",
            500.0,
        );
        let widths = cell_widths(&table, 0);
        let total: f32 = widths.iter().sum();
        assert!((total - 500.0).abs() < 1.0, "got {widths:?}");
        assert!(widths[1] > widths[0], "got {widths:?}");
    }
}
