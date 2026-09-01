//! Bridging Flexbox (`display: flex`) to taffy, as a subtree of the existing box tree.
//!
//! taffy is designed around its own node tree (`TaffyTree`), so we build taffy leaf nodes
//! for the flex container and each item on the fly and compute the layout once with
//! `compute_layout_with_measure`. Leaves needing an intrinsic size, such as text, get a
//! measure callback that calls the existing block/inline/table layout functions to measure
//! them for real. The result (each item's settled position and size) is then converted into
//! `LaidOutBox` by running the real layout once more through
//! `layout_box_with_forced_size` (the two-pass approach).
//!
//! taffy's types clash with our identically named CSS types (`crate::style::FlexDirection`
//! and friends), so they are referred to under the alias `tf`.

use std::collections::HashMap;
use std::rc::Rc;

use taffy as tf;

use crate::fonts::FontCollection;
use crate::html::NodeId;
use crate::style::{
    AlignContent, AlignItems, AlignSelf, BoxSizing, ComputedStyle, FlexBasis, FlexDirection,
    FlexWrap, JustifyContent, LengthPercentage, LengthPercentageOrAuto, MaxSize,
};

use super::block::{
    box_style, layout_box_with_forced_size, measure_box_with_forced_width, resolve_border,
    resolve_padding, LaidOutBox, PosCtx,
};
use super::box_tree::{FlexBox, LayoutBox};
use super::float_ctx::FloatContext;
use super::table::measure_natural_content_width;

/// Lay the flex items out inside the flex container's content box (starting at
/// `content_x`/`content_y`, with width `content_width`). Returns each laid-out item plus
/// the container's natural (content-based) content-box height, before the caller in
/// `block.rs` overrides it with an explicit `height` (the same division of labour as `layout_table`).
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_flex(
    flex: &FlexBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    container_style: &ComputedStyle,
    content_width: f32,
    content_x: f32,
    content_y: f32,
    pos: &mut PosCtx,
) -> (Vec<LaidOutBox>, f32) {
    let result = layout_taffy_subtree(
        &flex.items,
        styles,
        fonts,
        container_style,
        content_width,
        content_x,
        content_y,
        TaffyMode::Flex,
        pos,
    );
    (result.items, result.container_height)
}

/// Which kind of layout is delegated to taffy. Only the `Style` handed to the container and
/// the items differs; the measure bridge and the coordinate conversion are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TaffyMode {
    Flex,
    Grid,
}

/// The result of [`layout_taffy_subtree`].
pub(super) struct TaffySubtreeLayout {
    pub items: Vec<LaidOutBox>,
    /// The container's natural (content-based) content-box height.
    pub container_height: f32,
    /// For Grid only: the row tracks' used sizes and gutters (for pagination).
    pub row_tracks: Option<GridRowTracks>,
}

/// Row track information for a grid (from taffy's `DetailedGridInfo`).
/// `gutters` has `sizes.len() + 1` entries, one going before and after each track.
pub(super) struct GridRowTracks {
    pub sizes: Vec<f32>,
    pub gutters: Vec<f32>,
}

/// Lay flex/grid items out with taffy and convert them into the existing `LaidOutBox`
/// (the two-pass approach).
#[allow(clippy::too_many_arguments)]
pub(super) fn layout_taffy_subtree(
    flex_items: &[LayoutBox],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    container_style: &ComputedStyle,
    content_width: f32,
    content_x: f32,
    content_y: f32,
    mode: TaffyMode,
    pos: &mut PosCtx,
) -> TaffySubtreeLayout {
    if flex_items.is_empty() {
        return TaffySubtreeLayout {
            items: Vec::new(),
            container_height: 0.0,
            row_tracks: None,
        };
    }

    let mut tree: tf::TaffyTree<usize> = tf::TaffyTree::new();
    // taffy rounds the final layout to integers by default. That keeps gaps and overlaps out
    // of integer-pixel rasterisation, but it gains us nothing here, where the destination is
    // a PDF with floating-point coordinates: the natural width from measuring gets truncated,
    // items end up narrower than their content, and text that should fit wraps. So no rounding.
    tree.disable_rounding();

    // `box_style` clones a `ComputedStyle` (over 1KB), so it is built once per item and
    // reused (the measure callback is called many times per item).
    let item_styles: Vec<std::borrow::Cow<'_, ComputedStyle>> = flex_items
        .iter()
        .map(|item| box_style(item, styles))
        .collect();

    let leaves: Vec<tf::NodeId> = item_styles
        .iter()
        .enumerate()
        .map(|(index, item_style)| {
            let leaf_style = match mode {
                TaffyMode::Flex => item_taffy_style(item_style),
                TaffyMode::Grid => super::grid::item_taffy_style(item_style),
            };
            tree.new_leaf_with_context(leaf_style, index)
                .expect("adding a leaf node to taffy cannot fail")
        })
        .collect();

    let root_style = match mode {
        TaffyMode::Flex => container_taffy_style(container_style, content_width),
        TaffyMode::Grid => super::grid::container_taffy_style(container_style, content_width),
    };
    let root = tree
        .new_with_children(root_style, &leaves)
        .expect("adding the root node to taffy cannot fail");

    // The measure callback is called many times for the same item, from each of taffy's
    // passes. Its body is a pure computation determined entirely by `(item, width)`, so we
    // remember the result and skip the repeats. For one receipt, the full layout inside
    // measuring ran 188 times, of which only 52 fed into the final layout.
    //
    // The memo lives on the item's `LayoutBox`. Keeping it local here would rebuild it at
    // every ancestor level of a nested flex/grid, multiplying the measuring of the same
    // subtree by the nesting depth.
    tree.compute_layout_with_measure(
        root,
        tf::Size {
            width: tf::AvailableSpace::Definite(content_width),
            height: tf::AvailableSpace::MaxContent,
        },
        |known_dimensions, available_space, _node_id, node_context, _style| {
            let Some(&mut index) = node_context else {
                return tf::Size::ZERO;
            };
            let item = &flex_items[index];
            let item_style = &item_styles[index];

            // Padding and border always resolve against "the containing block's width"
            // (that is, the flex container's content_width), per the CSS spec, horizontally and vertically alike.
            let padding = resolve_padding(item_style, content_width);
            let border = resolve_border(item_style);
            let pb_x = padding.left + padding.right + border.left + border.right;
            let pb_y = padding.top + padding.bottom + border.top + border.bottom;

            // What measure returns is a content-box size (taffy adds padding/border itself;
            // a convention confirmed by measurement in `compute::leaf::compute_leaf_layout`).
            // `known_dimensions`, on the other hand, is border-box based (taffy computes
            // internally in border-box throughout), so rather than use it directly we
            // subtract padding/border to bring it to content-box. `available_space` needs no
            // conversion, taffy having already subtracted padding/border.
            let width = known_dimensions
                .width
                .map(|w| (w - pb_x).max(0.0))
                .unwrap_or_else(|| {
                    let natural = measure_natural_content_width(item, styles, fonts);
                    match available_space.width {
                        // Even with a definite "available width", return the content width
                        // when the content is narrower. Always returning `w` here would fill
                        // the whole track in cases that should shrink to the content width
                        // (Grid's `justify-items: start`, say).
                        tf::AvailableSpace::Definite(w) => natural.min(w),
                        // min-content and max-content are not distinguished (a known simplification).
                        tf::AvailableSpace::MinContent | tf::AvailableSpace::MaxContent => natural,
                    }
                });

            let height = known_dimensions
                .height
                .map(|h| (h - pb_y).max(0.0))
                .unwrap_or_else(|| {
                    let outer_width = width + pb_x;
                    if let Some(memo) = item.measured.height(width, outer_width) {
                        return memo;
                    }

                    let mut float_ctx = FloatContext::new();
                    let laid = measure_box_with_forced_width(
                        item,
                        styles,
                        fonts,
                        outer_width,
                        width,
                        &mut float_ctx,
                        0.0,
                        0.0,
                    );
                    let height = laid.layout.content.height;
                    item.measured.set_height(width, outer_width, height);
                    height
                });

            tf::Size { width, height }
        },
    )
    .expect("compute_layout_with_measure cannot fail");

    let mut result = Vec::with_capacity(flex_items.len());
    for (index, item) in flex_items.iter().enumerate() {
        let leaf = leaves[index];
        let item_layout = tree
            .layout(leaf)
            .expect("just computed by compute_layout_with_measure");
        let item_style = &item_styles[index];

        // taffy's Layout.size/padding/border assume border-box (confirmed by measurement in a
        // spike). Convert to content-box width and height.
        let content_w = (item_layout.size.width
            - item_layout.padding.left
            - item_layout.padding.right
            - item_layout.border.left
            - item_layout.border.right)
            .max(0.0);
        let content_h = (item_layout.size.height
            - item_layout.padding.top
            - item_layout.padding.bottom
            - item_layout.border.top
            - item_layout.border.bottom)
            .max(0.0);

        // taffy's location has the border-box origin (margins are already folded into the
        // position; confirmed by measurement in a spike: a leaf with margin-left: 10px has
        // location.x=10). `layout_box_with_forced_size` expects an x/y from before margins
        // are added, so the margins are subtracted back off here.
        let margin_left = super::block::resolve_lpa_or_zero(item_style.margin_left, content_width);
        let margin_top = super::block::resolve_lpa_or_zero(item_style.margin_top, content_width);

        let x = content_x + item_layout.location.x - margin_left;
        let y = content_y + item_layout.location.y - margin_top;

        // A flex item establishes a new formatting context (`float` has no effect on the item
        // itself, per the CSS spec), so each item uses its own independent `FloatContext`
        // (the same policy as cells in `table.rs`).
        //
        // This is the final layout pass, so any `absolute`/`fixed` among the item's
        // descendants is collected into the real `PosCtx` (unlike the measuring pass, this
        // runs only once per item).
        let mut item_float_ctx = FloatContext::new();
        let laid = layout_box_with_forced_size(
            item,
            styles,
            fonts,
            content_width,
            content_w,
            content_h,
            &mut item_float_ctx,
            x,
            y,
            pos,
        );
        result.push(laid);
    }

    let root_layout = tree
        .layout(root)
        .expect("just computed by compute_layout_with_measure");

    // Extract the row track information used for grid pagination.
    let row_tracks = match (mode, tree.detailed_layout_info(root)) {
        (TaffyMode::Grid, tf::DetailedLayoutInfo::Grid(info)) => Some(GridRowTracks {
            sizes: info.rows.sizes.clone(),
            gutters: info.rows.gutters.clone(),
        }),
        _ => None,
    };

    TaffySubtreeLayout {
        items: result,
        container_height: root_layout.size.height,
        row_tracks,
    }
}

fn container_taffy_style(style: &ComputedStyle, content_width: f32) -> tf::Style {
    tf::Style {
        display: tf::Display::Flex,
        flex_direction: map_flex_direction(style.flex_direction),
        flex_wrap: map_flex_wrap(style.flex_wrap),
        justify_content: map_justify_content(style.justify_content),
        align_items: Some(map_align_items(style.align_items)),
        align_content: Some(map_align_content(style.align_content)),
        gap: tf::Size {
            width: map_length_percentage(style.column_gap),
            height: map_length_percentage(style.row_gap),
        },
        // An explicit height (`height: 100px`, say) is passed on to taffy so that
        // `align-items`/`align-content` can align against the container's real height.
        // With `auto`, taffy computes the natural content-based height (the caller's
        // `resolve_height` in `block.rs` overrides it for an explicit `height`; the same
        // division of labour as `layout_table`).
        size: tf::Size {
            width: tf::Dimension::length(content_width),
            height: map_dimension(style.height),
        },
        // `min-*`/`max-*` are delegated to taffy as-is. In a flex context taffy can resolve
        // percentages against the container, so unlike the block side a percentage height
        // works too (the same asymmetry as the existing `height`).
        min_size: tf::Size {
            width: map_length_percentage_dimension(style.min_width),
            height: map_length_percentage_dimension(style.min_height),
        },
        max_size: tf::Size {
            width: map_max_size(style.max_width),
            height: map_max_size(style.max_height),
        },
        // `aspect-ratio` is delegated to taffy too.
        aspect_ratio: style.aspect_ratio.ratio,
        ..Default::default()
    }
}

pub(super) fn item_taffy_style(style: &ComputedStyle) -> tf::Style {
    let border = resolve_border(style);
    tf::Style {
        size: tf::Size {
            width: map_dimension(style.width),
            height: map_dimension(style.height),
        },
        margin: tf::Rect {
            left: map_margin(style.margin_left),
            right: map_margin(style.margin_right),
            top: map_margin(style.margin_top),
            bottom: map_margin(style.margin_bottom),
        },
        padding: tf::Rect {
            left: map_length_percentage(style.padding_left),
            right: map_length_percentage(style.padding_right),
            top: map_length_percentage(style.padding_top),
            bottom: map_length_percentage(style.padding_bottom),
        },
        border: tf::Rect {
            left: tf::LengthPercentage::length(border.left),
            right: tf::LengthPercentage::length(border.right),
            top: tf::LengthPercentage::length(border.top),
            bottom: tf::LengthPercentage::length(border.bottom),
        },
        min_size: tf::Size {
            width: map_length_percentage_dimension(style.min_width),
            height: map_length_percentage_dimension(style.min_height),
        },
        max_size: tf::Size {
            width: map_max_size(style.max_width),
            height: map_max_size(style.max_height),
        },
        aspect_ratio: style.aspect_ratio.ratio,
        align_self: map_align_self(style.align_self),
        flex_grow: style.flex_grow,
        flex_shrink: style.flex_shrink,
        flex_basis: map_flex_basis(style.flex_basis),
        box_sizing: map_box_sizing(style.box_sizing),
        ..Default::default()
    }
}

fn map_flex_direction(v: FlexDirection) -> tf::FlexDirection {
    match v {
        FlexDirection::Row => tf::FlexDirection::Row,
        FlexDirection::RowReverse => tf::FlexDirection::RowReverse,
        FlexDirection::Column => tf::FlexDirection::Column,
        FlexDirection::ColumnReverse => tf::FlexDirection::ColumnReverse,
    }
}

fn map_flex_wrap(v: FlexWrap) -> tf::FlexWrap {
    match v {
        FlexWrap::NoWrap => tf::FlexWrap::NoWrap,
        FlexWrap::Wrap => tf::FlexWrap::Wrap,
        FlexWrap::WrapReverse => tf::FlexWrap::WrapReverse,
    }
}

/// The initial value `normal` is passed to taffy as `None`. In flex that is the same as
/// `flex-start`, but in grid an `auto` track absorbs the leftover width and grows (writing
/// `flex-start` explicitly does not).
pub(super) fn map_justify_content(v: JustifyContent) -> Option<tf::JustifyContent> {
    match v {
        JustifyContent::Normal => None,
        JustifyContent::FlexStart => Some(tf::JustifyContent::FLEX_START),
        JustifyContent::FlexEnd => Some(tf::JustifyContent::FLEX_END),
        JustifyContent::Center => Some(tf::JustifyContent::CENTER),
        JustifyContent::SpaceBetween => Some(tf::JustifyContent::SPACE_BETWEEN),
        JustifyContent::SpaceAround => Some(tf::JustifyContent::SPACE_AROUND),
        JustifyContent::SpaceEvenly => Some(tf::JustifyContent::SPACE_EVENLY),
    }
}

pub(super) fn map_align_items(v: AlignItems) -> tf::AlignItems {
    match v {
        AlignItems::FlexStart => tf::AlignItems::FLEX_START,
        AlignItems::FlexEnd => tf::AlignItems::FLEX_END,
        AlignItems::Center => tf::AlignItems::CENTER,
        AlignItems::Baseline => tf::AlignItems::BASELINE,
        AlignItems::Stretch => tf::AlignItems::STRETCH,
    }
}

pub(super) fn map_align_content(v: AlignContent) -> tf::AlignContent {
    match v {
        AlignContent::FlexStart => tf::AlignContent::FLEX_START,
        AlignContent::FlexEnd => tf::AlignContent::FLEX_END,
        AlignContent::Center => tf::AlignContent::CENTER,
        AlignContent::Stretch => tf::AlignContent::STRETCH,
        AlignContent::SpaceBetween => tf::AlignContent::SPACE_BETWEEN,
        AlignContent::SpaceAround => tf::AlignContent::SPACE_AROUND,
        AlignContent::SpaceEvenly => tf::AlignContent::SPACE_EVENLY,
    }
}

/// `align-self: auto` (the initial value) becomes `None` (taffy then uses the parent's `align-items`).
pub(super) fn map_align_self(v: AlignSelf) -> Option<tf::AlignSelf> {
    match v {
        AlignSelf::Auto => None,
        AlignSelf::FlexStart => Some(tf::AlignSelf::FLEX_START),
        AlignSelf::FlexEnd => Some(tf::AlignSelf::FLEX_END),
        AlignSelf::Center => Some(tf::AlignSelf::CENTER),
        AlignSelf::Baseline => Some(tf::AlignSelf::BASELINE),
        AlignSelf::Stretch => Some(tf::AlignSelf::STRETCH),
    }
}

fn map_box_sizing(v: BoxSizing) -> tf::BoxSizing {
    match v {
        BoxSizing::ContentBox => tf::BoxSizing::ContentBox,
        BoxSizing::BorderBox => tf::BoxSizing::BorderBox,
    }
}

pub(super) fn map_length_percentage(v: LengthPercentage) -> tf::LengthPercentage {
    match v {
        LengthPercentage::Length(px) => tf::LengthPercentage::length(px),
        LengthPercentage::Percentage(p) => tf::LengthPercentage::percent(p),
        // taffy cannot express a px+% compound, so only the px component of a calc is passed
        // for gap and the like (calc is mainly used on width/margin outside flex; a known simplification).
        LengthPercentage::Calc { px, .. } => tf::LengthPercentage::length(px),
    }
}

pub(super) fn map_dimension(v: LengthPercentageOrAuto) -> tf::Dimension {
    match v {
        LengthPercentageOrAuto::Auto => tf::Dimension::auto(),
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(px)) => {
            tf::Dimension::length(px)
        }
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Percentage(p)) => {
            tf::Dimension::percent(p)
        }
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc { px, .. }) => {
            tf::Dimension::length(px)
        }
    }
}

/// Map `min-width`/`min-height` (initial value `0`) to taffy's `Dimension`.
pub(super) fn map_length_percentage_dimension(v: LengthPercentage) -> tf::Dimension {
    match v {
        LengthPercentage::Length(px) => tf::Dimension::length(px),
        LengthPercentage::Percentage(p) => tf::Dimension::percent(p),
        // taffy cannot express a px+% compound, so only the px component is passed
        // (the same simplification as `map_length_percentage`).
        LengthPercentage::Calc { px, .. } => tf::Dimension::length(px),
    }
}

/// Map `max-width`/`max-height` to taffy's `Dimension`. `none` becomes `auto` (no upper bound).
pub(super) fn map_max_size(v: MaxSize) -> tf::Dimension {
    match v {
        MaxSize::None => tf::Dimension::auto(),
        MaxSize::LengthPercentage(lp) => map_length_percentage_dimension(lp),
    }
}

fn map_margin(v: LengthPercentageOrAuto) -> tf::LengthPercentageAuto {
    match v {
        LengthPercentageOrAuto::Auto => tf::LengthPercentageAuto::auto(),
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(px)) => {
            tf::LengthPercentageAuto::length(px)
        }
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Percentage(p)) => {
            tf::LengthPercentageAuto::percent(p)
        }
        LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc { px, .. }) => {
            tf::LengthPercentageAuto::length(px)
        }
    }
}

fn map_flex_basis(v: FlexBasis) -> tf::Dimension {
    match v {
        FlexBasis::Auto => tf::Dimension::auto(),
        FlexBasis::LengthPercentage(LengthPercentage::Length(px)) => tf::Dimension::length(px),
        FlexBasis::LengthPercentage(LengthPercentage::Percentage(p)) => tf::Dimension::percent(p),
        FlexBasis::LengthPercentage(LengthPercentage::Calc { px, .. }) => tf::Dimension::length(px),
    }
}
