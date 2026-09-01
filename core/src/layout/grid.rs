//! Bridging CSS Grid (`display: grid`) to taffy, as a subtree of the existing box tree.
//!
//! The bridge to taffy (the measure callback and the two-pass coordinate conversion) is
//! shared entirely with Flexbox and lives in [`super::flex::layout_taffy_subtree`]. What
//! this module holds is the part that maps CSS's grid-specific properties onto taffy's
//! `Style`, and the part that groups the layout result into row bands (the unit of pagination).

use std::collections::HashMap;
use std::rc::Rc;

use taffy as tf;

use crate::fonts::FontCollection;
use crate::html::NodeId;
use crate::style::{
    AlignItems, AlignSelf, ComputedStyle, GridArea, GridAutoFlow, GridLine, LengthPercentage,
    TrackBreadth, TrackComponent, TrackList, TrackSize,
};

use super::block::{LaidOutBox, PosCtx};
use super::box_tree::GridBox;
use super::flex::{layout_taffy_subtree, TaffyMode};

/// A laid-out grid. It holds the list of row bands, which are the unit of pagination.
#[derive(Debug, Clone)]
pub struct LaidOutGrid {
    pub rows: Vec<LaidOutGridRow>,
}

/// One row's band. `top`/`bottom` are absolute y coordinates (the same coordinate space as
/// the other layout results, so `shift_box_y` translates them along with everything else).
#[derive(Debug, Clone)]
pub struct LaidOutGridRow {
    pub items: Vec<LaidOutBox>,
    pub top: f32,
    pub bottom: f32,
    /// Whether any item spans the bottom of this band. When `true` the page cannot break
    /// here (handled like a table's `rowspan`).
    pub spans_bottom: bool,
}

/// Lay the items out inside the grid container's content box.
/// Returns the laid-out grid and the height of the container's content box.
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_grid(
    grid: &GridBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    container_style: &ComputedStyle,
    content_width: f32,
    content_x: f32,
    content_y: f32,
    pos: &mut PosCtx,
) -> (LaidOutGrid, f32) {
    let result = layout_taffy_subtree(
        &grid.items,
        styles,
        fonts,
        container_style,
        content_width,
        content_x,
        content_y,
        TaffyMode::Grid,
        pos,
    );

    let rows = group_into_rows(result.items, result.row_tracks.as_ref(), content_y);
    (LaidOutGrid { rows }, result.container_height)
}

/// Group the laid-out items into row bands.
///
/// taffy's `DetailedGridInfo::items` is ordered by the internals of the grid placement
/// algorithm, with no guarantee that it corresponds to the order of the leaves. So we decide
/// geometrically instead, matching each item's actual y coordinate against the band extents
/// derived from the row tracks' used sizes (an item belongs to the band its top falls in,
/// and `spans_bottom` is set if its bottom crosses the band).
fn group_into_rows(
    items: Vec<LaidOutBox>,
    row_tracks: Option<&super::flex::GridRowTracks>,
    content_y: f32,
) -> Vec<LaidOutGridRow> {
    let Some(tracks) = row_tracks.filter(|tracks| !tracks.sizes.is_empty()) else {
        // With no row track information, treat the whole thing as one band
        // (that is, do not split; the same behaviour as a flex container).
        let (top, bottom) = items_vertical_extent(&items, content_y);
        return vec![LaidOutGridRow {
            items,
            top,
            bottom,
            spans_bottom: false,
        }];
    };

    // The band extents. taffy's track array is laid out as
    // [gutter, track, gutter, ..., gutter], so track i starts at the sum of the first i+1
    // gutters plus i tracks.
    let mut bands: Vec<(f32, f32)> = Vec::with_capacity(tracks.sizes.len());
    // Start from the top of the content box (in absolute coordinates).
    let mut offset = content_y;
    for (i, size) in tracks.sizes.iter().enumerate() {
        offset += tracks.gutters.get(i).copied().unwrap_or(0.0);
        bands.push((offset, offset + size));
        offset += size;
    }

    let mut rows: Vec<LaidOutGridRow> = bands
        .iter()
        .map(|(top, bottom)| LaidOutGridRow {
            items: Vec::new(),
            top: *top,
            bottom: *bottom,
            spans_bottom: false,
        })
        .collect();

    for item in items {
        let margin_box_top = item.layout.content.y
            - item.layout.padding.top
            - item.layout.border.top
            - item.layout.margin.top;
        let margin_box_bottom = margin_box_top + item.layout.margin_box_height();

        // The band the top falls in (the last band if none is found).
        let index = bands
            .iter()
            .position(|(top, bottom)| margin_box_top < *bottom || margin_box_top <= *top)
            .unwrap_or(bands.len() - 1);
        // If the bottom crosses out of its own band, no break is possible at that band's boundary.
        if margin_box_bottom > bands[index].1 + BAND_EPSILON {
            rows[index].spans_bottom = true;
        }
        rows[index].items.push(item);
    }

    // Bands with no items at all (empty rows) still have a height, so they are kept.
    rows
}

/// The tolerance (px) used when deciding band boundaries. taffy returns floating-point
/// coordinates, so this keeps an item exactly touching a boundary from counting as crossing it.
const BAND_EPSILON: f32 = 0.01;

/// The fallback when there is no row track information: the top and bottom of all the items (in absolute coordinates).
fn items_vertical_extent(items: &[LaidOutBox], content_y: f32) -> (f32, f32) {
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for item in items {
        let item_top = item.layout.content.y
            - item.layout.padding.top
            - item.layout.border.top
            - item.layout.margin.top;
        top = top.min(item_top);
        bottom = bottom.max(item_top + item.layout.margin_box_height());
    }
    if items.is_empty() {
        (content_y, content_y)
    } else {
        (top, bottom)
    }
}

/// The taffy `Style` for a grid container.
pub(super) fn container_taffy_style(style: &ComputedStyle, content_width: f32) -> tf::Style {
    tf::Style {
        display: tf::Display::Grid,
        grid_template_columns: map_track_list(&style.grid_template_columns),
        grid_template_rows: map_track_list(&style.grid_template_rows),
        grid_auto_columns: map_auto_tracks(&style.grid_auto_columns),
        grid_auto_rows: map_auto_tracks(&style.grid_auto_rows),
        grid_auto_flow: map_auto_flow(style.grid_auto_flow),
        grid_template_areas: map_template_areas(&style.grid_template_areas),
        grid_template_column_names: map_line_names(&style.grid_template_columns),
        grid_template_row_names: map_line_names(&style.grid_template_rows),
        justify_content: super::flex::map_justify_content(style.justify_content),
        align_content: Some(super::flex::map_align_content(style.align_content)),
        // In Grid both `justify-items` and `align-items` mean something.
        justify_items: Some(map_align_items(style.justify_items)),
        align_items: Some(super::flex::map_align_items(style.align_items)),
        gap: tf::Size {
            width: super::flex::map_length_percentage(style.column_gap),
            height: super::flex::map_length_percentage(style.row_gap),
        },
        size: tf::Size {
            width: tf::Dimension::length(content_width),
            height: super::flex::map_dimension(style.height),
        },
        min_size: tf::Size {
            width: super::flex::map_length_percentage_dimension(style.min_width),
            height: super::flex::map_length_percentage_dimension(style.min_height),
        },
        max_size: tf::Size {
            width: super::flex::map_max_size(style.max_width),
            height: super::flex::map_max_size(style.max_height),
        },
        aspect_ratio: style.aspect_ratio.ratio,
        ..Default::default()
    }
}

/// The taffy `Style` for a grid item.
pub(super) fn item_taffy_style(style: &ComputedStyle) -> tf::Style {
    let mut base = super::flex::item_taffy_style(style);
    base.grid_row = tf::Line {
        start: map_grid_line(&style.grid_row_start),
        end: map_grid_line(&style.grid_row_end),
    };
    base.grid_column = tf::Line {
        start: map_grid_line(&style.grid_column_start),
        end: map_grid_line(&style.grid_column_end),
    };
    base.justify_self = map_align_self(style.justify_self);
    base
}

fn map_track_list(list: &TrackList) -> Vec<tf::GridTemplateComponent<String>> {
    list.components
        .iter()
        .map(|component| match component {
            TrackComponent::Single(size) => {
                tf::GridTemplateComponent::Single(map_track_size(*size))
            }
            TrackComponent::Repeat {
                count,
                tracks,
                line_names,
            } => tf::GridTemplateComponent::Repeat(tf::GridTemplateRepetition {
                count: match count {
                    crate::style::RepeatCount::Count(n) => tf::RepetitionCount::Count(*n),
                    crate::style::RepeatCount::AutoFill => tf::RepetitionCount::AutoFill,
                    crate::style::RepeatCount::AutoFit => tf::RepetitionCount::AutoFit,
                },
                tracks: tracks.iter().map(|size| map_track_size(*size)).collect(),
                line_names: line_names.clone(),
            }),
        })
        .collect()
}

/// The line names written as `[name]`. taffy holds them as one list per track boundary.
fn map_line_names(list: &TrackList) -> Vec<Vec<String>> {
    list.line_names.clone()
}

fn map_auto_tracks(sizes: &[TrackSize]) -> Vec<tf::TrackSizingFunction> {
    sizes.iter().map(|size| map_track_size(*size)).collect()
}

fn map_track_size(size: TrackSize) -> tf::TrackSizingFunction {
    match size {
        TrackSize::Breadth(breadth) => tf::TrackSizingFunction {
            min: map_min_breadth(breadth),
            max: map_max_breadth(breadth),
        },
        TrackSize::MinMax(min, max) => tf::TrackSizingFunction {
            min: map_min_breadth(min),
            max: map_max_breadth(max),
        },
        TrackSize::FitContent(lp) => tf::TrackSizingFunction {
            min: tf::MinTrackSizingFunction::auto(),
            max: match lp {
                LengthPercentage::Length(px) => tf::MaxTrackSizingFunction::fit_content_px(px),
                LengthPercentage::Percentage(v) => {
                    tf::MaxTrackSizingFunction::fit_content_percent(v)
                }
                // A calc track size is rejected by the parser, so it never reaches here.
                LengthPercentage::Calc { px, .. } => tf::MaxTrackSizingFunction::fit_content_px(px),
            },
        },
    }
}

/// Map a `<track-breadth>` to taffy's minimum track size. On the minimum side `fr` counts
/// as `auto` (per the CSS spec, `1fr` is equivalent to `minmax(auto, 1fr)`).
fn map_min_breadth(breadth: TrackBreadth) -> tf::MinTrackSizingFunction {
    match breadth {
        TrackBreadth::Length(px) => tf::MinTrackSizingFunction::length(px),
        TrackBreadth::Percentage(v) => tf::MinTrackSizingFunction::percent(v),
        TrackBreadth::Fr(_) | TrackBreadth::Auto => tf::MinTrackSizingFunction::auto(),
        TrackBreadth::MinContent => tf::MinTrackSizingFunction::min_content(),
        TrackBreadth::MaxContent => tf::MinTrackSizingFunction::max_content(),
    }
}

fn map_max_breadth(breadth: TrackBreadth) -> tf::MaxTrackSizingFunction {
    match breadth {
        TrackBreadth::Length(px) => tf::MaxTrackSizingFunction::length(px),
        TrackBreadth::Percentage(v) => tf::MaxTrackSizingFunction::percent(v),
        TrackBreadth::Fr(v) => tf::MaxTrackSizingFunction::fr(v),
        TrackBreadth::Auto => tf::MaxTrackSizingFunction::auto(),
        TrackBreadth::MinContent => tf::MaxTrackSizingFunction::min_content(),
        TrackBreadth::MaxContent => tf::MaxTrackSizingFunction::max_content(),
    }
}

fn map_auto_flow(flow: GridAutoFlow) -> tf::GridAutoFlow {
    match flow {
        GridAutoFlow::Row => tf::GridAutoFlow::Row,
        GridAutoFlow::Column => tf::GridAutoFlow::Column,
        GridAutoFlow::RowDense => tf::GridAutoFlow::RowDense,
        GridAutoFlow::ColumnDense => tf::GridAutoFlow::ColumnDense,
    }
}

fn map_template_areas(areas: &[GridArea]) -> Vec<tf::GridTemplateArea<String>> {
    areas
        .iter()
        .map(|area| tf::GridTemplateArea {
            name: area.name.clone(),
            row_start: area.row_start,
            row_end: area.row_end,
            column_start: area.column_start,
            column_end: area.column_end,
        })
        .collect()
}

fn map_grid_line(line: &GridLine) -> tf::GridPlacement<String> {
    match line {
        GridLine::Auto => tf::GridPlacement::Auto,
        GridLine::Line(n) => tf::GridPlacement::Line((*n).into()),
        GridLine::Span(n) => tf::GridPlacement::Span(*n),
        GridLine::Named(name) => tf::GridPlacement::NamedLine(name.clone(), 1),
        GridLine::NamedSpan(name, n) => tf::GridPlacement::NamedSpan(name.clone(), *n),
    }
}

/// `justify-items` is item placement along Grid's inline axis. Its value set is shared with
/// `align-items`.
fn map_align_items(items: AlignItems) -> tf::AlignItems {
    super::flex::map_align_items(items)
}

fn map_align_self(align: AlignSelf) -> Option<tf::AlignSelf> {
    super::flex::map_align_self(align)
}
