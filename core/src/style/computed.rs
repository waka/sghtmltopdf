//! Computing each element's style from the cascaded declarations (T3).
//!
//! Per property: "take the declaration if there is one (whichever won in cascade order);
//! otherwise inherit from the parent for inherited properties, else use the initial value" - the CSS computed-value procedure

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::html::{Dom, NodeData, NodeId};

use super::cascade::{
    matching_declarations_by_origin, matching_pseudo_content, matching_pseudo_declarations,
};
use super::presentational::presentational_hint_declarations;
use super::properties::PropertyDeclaration;
use super::selector_impl::PseudoElement;
use super::stylesheet::{parse_inline_style, Stylesheet};
use super::values::{
    AlignContent, AlignItems, AlignSelf, AspectRatio, BackgroundAttachment, BackgroundPosition,
    BackgroundRepeat, BackgroundSize, BorderCollapse, BorderStyle, BoxSizing, BreakBetween,
    BreakInside, CaptionSide, Clear, Color, ContentPart, CornerRadius, Display, EmphasisPosition,
    EmphasisStyle, EmptyCells, FlexBasis, FlexDirection, FlexWrap, Float, FontStyle, FontWeight,
    GridArea, GridAutoFlow, GridLine, Hyphens, JustifyContent, Length, LengthPercentage,
    LengthPercentageOrAuto, ListStylePosition, ListStyleType, MaxSize, ObjectFit, Overflow,
    OverflowWrap, Position, QuotePair, SpecifiedCornerRadius, SpecifiedLength,
    SpecifiedLengthPercentage, SpecifiedLengthPercentageOrAuto, SpecifiedLineHeight,
    SpecifiedMaxSize, SpecifiedTrackSize, TableLayout, TextAlign, TextDecorationLine, TextOverflow,
    TextTransform, TrackList, TrackSize, TransformFunction, VerticalAlign, Visibility, WhiteSpace,
    WordBreak, ZIndex,
};

/// The computed value of `color`/`background-color`. Unlike at parse time, `currentcolor` is already resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbaColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: f32,
}

impl RgbaColor {
    /// Fully transparent (the initial value of `background-color`, `transparent`).
    pub const TRANSPARENT: Self = Self {
        red: 0,
        green: 0,
        blue: 0,
        alpha: 0.0,
    };
}

/// The computed value of `line-height`. CSS2.1 section 10.8.1: the computed value of a
/// `<number>`/`<percentage>` is "the specified number itself", not an absolute value
/// pre-multiplied by the parent's font-size. It is inherited as-is, and the consumer
/// (`layout::inline`) multiplies it by that text run's font-size.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum LineHeight {
    #[default]
    Normal,
    /// `<number>`. A `<percentage>` is normalised to `p/100.0` and stored here.
    Number(f32),
    /// `<length>`. Absolute px with em/rem resolved, inherited unchanged.
    Length(f32),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ComputedStyle {
    pub display: Display,
    pub width: LengthPercentageOrAuto,
    pub height: LengthPercentageOrAuto,
    /// `min-width`/`min-height`. Not inherited; initial value `0`.
    pub min_width: LengthPercentage,
    pub min_height: LengthPercentage,
    /// `max-width`/`max-height`. Not inherited; initial value `none` (no upper bound).
    pub max_width: MaxSize,
    pub max_height: MaxSize,
    /// `aspect-ratio`. Not inherited; initial value `auto`. For a replaced element (`<img>`)
    /// the intrinsic ratio is baked into `ratio` at the entry to size resolution.
    pub aspect_ratio: AspectRatio,
    pub margin_top: LengthPercentageOrAuto,
    pub margin_right: LengthPercentageOrAuto,
    pub margin_bottom: LengthPercentageOrAuto,
    pub margin_left: LengthPercentageOrAuto,
    pub padding_top: LengthPercentage,
    pub padding_right: LengthPercentage,
    pub padding_bottom: LengthPercentage,
    pub padding_left: LengthPercentage,
    pub border_top_width: Length,
    pub border_right_width: Length,
    pub border_bottom_width: Length,
    pub border_left_width: Length,
    /// The initial value is `currentcolor` (as the spec says). With no declaration, the
    /// element's own computed `color` is used (resolved by `resolve_color`).
    pub border_top_color: RgbaColor,
    pub border_right_color: RgbaColor,
    pub border_bottom_color: RgbaColor,
    pub border_left_color: RgbaColor,
    pub border_top_style: BorderStyle,
    pub border_right_style: BorderStyle,
    pub border_bottom_style: BorderStyle,
    pub border_left_style: BorderStyle,
    /// Holds a horizontal and a vertical radius (a true circle has the two equal).
    pub border_top_left_radius: CornerRadius,
    pub border_top_right_radius: CornerRadius,
    pub border_bottom_right_radius: CornerRadius,
    pub border_bottom_left_radius: CornerRadius,
    /// An inherited property.
    pub font_size: Length,
    /// An inherited property.
    pub font_family: Vec<String>,
    /// An inherited property.
    pub font_weight: FontWeight,
    /// An inherited property.
    pub font_style: FontStyle,
    /// An inherited property.
    pub color: RgbaColor,
    pub background_color: RgbaColor,
    /// `url(...)` (the raw value; resolving it is left to the caller). Not inherited; initial value `None`.
    pub background_image: Option<String>,
    /// Not inherited.
    pub background_position: BackgroundPosition,
    /// Not inherited.
    pub background_size: BackgroundSize,
    /// Not inherited.
    pub background_repeat: BackgroundRepeat,
    /// Not inherited. `fixed` is drawn the same as `scroll`.
    pub background_attachment: BackgroundAttachment,
    /// `text-decoration-line`. The spec makes it non-inherited but gives it a special rule
    /// whereby an ancestor's decoration line "propagates" onto descendant boxes. Rather
    /// than implement that propagation separately, we treat it as an inherited property, a
    /// simplification that matches the appearance in the common nesting cases (such as
    /// `<u>bold <b>text</b></u>`). An explicit override on a descendant still wins, as with ordinary inheritance.
    pub text_decoration_line: TextDecorationLine,
    /// The generated content of `::before { content: "..." }`. It is drawn reusing the
    /// element's own style (a simplification: pseudo-elements have no computed style of their own).
    pub pseudo_before_content: Option<String>,
    /// The generated content of `::after { content: "..." }`.
    pub pseudo_after_content: Option<String>,
    /// CSS Fragmentation. Not inherited (per the spec).
    pub break_before: BreakBetween,
    pub break_after: BreakBetween,
    pub break_inside: BreakInside,
    /// Minimum number of lines that may be left at the end of a page. Not inherited; initial value 2 (per the spec).
    pub orphans: u32,
    /// Minimum number of lines that may be carried to the start of a page. Not inherited; initial value 2 (per the spec).
    pub widows: u32,
    /// `float`. Not inherited. Anything but `none` makes `display` compute to block-level
    /// (CSS2.1 9.7, applied in `compute_element_style` below).
    pub float: Float,
    /// `clear`. Not inherited.
    pub clear: Clear,
    /// `position`. Not inherited.
    pub position: Position,
    pub top: LengthPercentageOrAuto,
    pub right: LengthPercentageOrAuto,
    pub bottom: LengthPercentageOrAuto,
    pub left: LengthPercentageOrAuto,
    /// An inherited property. Inside an IFC the first `InlineSpan`'s computed value represents it.
    pub text_align: TextAlign,
    /// An inherited property. `Number`/`Percentage` are inherited unmultiplied, and the
    /// consumer (`layout::inline`) multiplies by the text run's font-size.
    pub line_height: LineHeight,
    /// An inherited property. Percentages are kept as fractions, the containing block width
    /// being unresolved (the same "the used value is resolved by the consumer" pattern as
    /// `width`/`margin`). Inside an IFC the first `InlineSpan`'s computed value represents it.
    pub text_indent: LengthPercentage,
    /// An inherited property. Inside an IFC the first `InlineSpan`'s computed value represents it.
    pub white_space: WhiteSpace,
    /// An inherited property. Resolved px; `normal` is `0.0`.
    pub letter_spacing: f32,
    /// An inherited property. Resolved px; `normal` is `0.0`.
    pub word_spacing: f32,
    /// An inherited property.
    pub text_transform: TextTransform,
    /// `text-shadow`. Inherited; an empty Vec means `none`. Colours are resolved.
    pub text_shadow: Vec<ComputedTextShadow>,
    /// `text-overflow`. Not inherited (per the spec). It applies only when `overflow` is
    /// something other than `visible`.
    pub text_overflow: TextOverflow,
    /// `word-break`. An inherited property.
    pub word_break: WordBreak,
    /// `overflow-wrap` (also known as `word-wrap`). An inherited property.
    pub overflow_wrap: OverflowWrap,
    /// `hyphens`. An inherited property.
    pub hyphens: Hyphens,
    /// `text-emphasis-style`. An inherited property.
    pub text_emphasis_style: EmphasisStyle,
    /// `text-emphasis-color`. Inherited; the initial value is `currentcolor`.
    pub text_emphasis_color: RgbaColor,
    /// `text-emphasis-position`. Inherited; initial value `over`.
    pub text_emphasis_position: EmphasisPosition,
    /// `grid-template-columns`/`grid-template-rows`. Not inherited;
    /// empty means `none`.
    pub grid_template_columns: TrackList,
    pub grid_template_rows: TrackList,
    /// `grid-auto-columns`/`grid-auto-rows`. Not inherited; empty means the initial value `auto`.
    pub grid_auto_columns: Vec<TrackSize>,
    pub grid_auto_rows: Vec<TrackSize>,
    /// `grid-auto-flow`. Not inherited; initial value `row`.
    pub grid_auto_flow: GridAutoFlow,
    /// `grid-template-areas`. Not inherited; empty means `none`.
    pub grid_template_areas: Vec<GridArea>,
    /// `grid-row-start` and friends. Not inherited; initial value `auto`.
    pub grid_row_start: GridLine,
    pub grid_row_end: GridLine,
    pub grid_column_start: GridLine,
    pub grid_column_end: GridLine,
    /// `justify-items`/`justify-self`. Not inherited. Meaningful only in Grid
    /// (it does not apply to flex items).
    pub justify_items: AlignItems,
    pub justify_self: AlignSelf,
    /// `border-collapse`. Inherited. It only unifies how borders are drawn.
    pub border_collapse: BorderCollapse,
    /// The horizontal component of `border-spacing`. Inherited; ignored and treated as 0
    /// under `border-collapse: collapse` (per the spec, resolved in `layout::table`).
    pub border_spacing_horizontal: Length,
    /// The vertical component of `border-spacing`. An inherited property.
    pub border_spacing_vertical: Length,
    /// `caption-side`. An inherited property.
    pub caption_side: CaptionSide,
    /// `table-layout`. Not inherited; the table element's own value is used.
    pub table_layout: TableLayout,
    /// `empty-cells`. Inherited; meaningful only with `border-collapse: separate`.
    pub empty_cells: EmptyCells,
    /// `vertical-align` (in the table-cell context). Not inherited.
    pub vertical_align: VerticalAlign,
    /// `list-style-type`. An inherited property.
    pub list_style_type: ListStyleType,
    /// `list-style-position`. An inherited property.
    pub list_style_position: ListStylePosition,
    /// `list-style-image` (the raw `url(...)` value). Inherited, but in practice it always
    /// falls back to the `list_style_type` text marker and is never drawn.
    pub list_style_image: Option<String>,
    /// `overflow`. Not inherited. `hidden`/`scroll`/`auto` are not distinguished and all
    /// clip.
    pub overflow: Overflow,
    /// `box-sizing`. Not inherited.
    pub box_sizing: BoxSizing,
    /// `z-index`. Not inherited. It has no effect on a `position: static` element
    /// (per the spec; decided in `layout`/`pdf`).
    pub z_index: ZIndex,
    /// `visibility`. Inherited. `collapse` is treated as `hidden`.
    pub visibility: Visibility,
    /// `outline-width`. Not inherited.
    pub outline_width: Length,
    /// `outline-style`. Not inherited; initial value `none`.
    pub outline_style: BorderStyle,
    /// `outline-color`. Not inherited; the initial value amounts to `currentcolor`
    /// (the same resolution rule as `border-color`).
    pub outline_color: RgbaColor,
    /// `quotes`. Inherited. `None` means `none` (always generating an empty string).
    pub quotes: Option<Vec<QuotePair>>,
    /// The limited override style for `::first-letter`.
    /// `None` if no declaration matched.
    pub first_letter_style: Option<FirstLetterStyle>,
    /// `object-fit`. Not inherited; meaningful only on `<img>`.
    pub object_fit: ObjectFit,
    /// `object-position`. Not inherited; initial value `50% 50%`
    /// (unlike `background-position`, whose initial value is `0% 0%`).
    pub object_position: BackgroundPosition,
    /// `box-shadow`. Not inherited; the initial value is empty (no shadow). Comma-separated
    /// multiples are supported, the first being frontmost.
    pub box_shadow: Vec<ComputedBoxShadow>,
    /// `flex-direction`. Not inherited; meaningful only on the flex container itself.
    pub flex_direction: FlexDirection,
    /// `flex-wrap`. Not inherited; meaningful only on the flex container itself.
    pub flex_wrap: FlexWrap,
    /// `justify-content`. Not inherited; meaningful only on the flex container itself.
    pub justify_content: JustifyContent,
    /// `align-items`. Not inherited; meaningful only on the flex container itself.
    pub align_items: AlignItems,
    /// `align-content`. Not inherited; meaningful only on the flex container itself.
    pub align_content: AlignContent,
    /// `align-self`. Not inherited; meaningful only on a flex item.
    pub align_self: AlignSelf,
    /// `flex-grow`. Not inherited; meaningful only on a flex item.
    pub flex_grow: f32,
    /// `flex-shrink`. Not inherited; meaningful only on a flex item.
    pub flex_shrink: f32,
    /// `flex-basis`. Not inherited; meaningful only on a flex item.
    pub flex_basis: FlexBasis,
    /// `row-gap`. Not inherited; meaningful only on the flex container itself.
    pub row_gap: LengthPercentage,
    /// `column-gap`. Not inherited; meaningful only on the flex container itself.
    pub column_gap: LengthPercentage,
    /// `transform`. Not inherited. Percentages (on the `translate` family) are kept
    /// unresolved, since they resolve only once the element's own border-box size is final.
    /// An empty Vec means `none`.
    pub transform: Vec<TransformFunction>,
    /// `transform-origin`. Not inherited; initial value
    /// `50% 50%` (unlike `background-position`, whose initial value is `0% 0%`).
    pub transform_origin: BackgroundPosition,
    /// `opacity`. Not inherited; already clamped to 0-1; initial value 1.0.
    pub opacity: f32,
}

/// The computed value of one `box-shadow`. Lengths are resolved to px and `color` has
/// `currentcolor` resolved (`resolve_color`, against the element's own computed `color`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputedBoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: RgbaColor,
    /// The `inset` keyword. It parses, but drawing it is not supported (a known simplification).
    pub inset: bool,
}

/// `em`/`rem` resolution for `grid-auto-columns`/`grid-auto-rows`.
fn resolve_track_sizes(
    sizes: &[SpecifiedTrackSize],
    font_size: f32,
    root_font_size: f32,
) -> Vec<TrackSize> {
    sizes
        .iter()
        .map(|size| size.resolve(font_size, root_font_size))
        .collect()
}

/// The computed value of one `text-shadow`. Lengths are resolved to px and `color` has
/// `currentcolor` resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComputedTextShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub color: RgbaColor,
}

/// The limited override style for `::first-letter`. Balancing implementation cost against
/// demand, only the font properties, color, text-decoration-line and text-transform are
/// supported (`float` and the box model properties are not). A field that is `None` uses
/// the host element's own computed value as-is.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FirstLetterStyle {
    pub font_size: Option<Length>,
    pub font_family: Option<Vec<String>>,
    pub font_weight: Option<FontWeight>,
    pub font_style: Option<FontStyle>,
    pub color: Option<RgbaColor>,
    pub text_decoration_line: Option<TextDecorationLine>,
    pub text_transform: Option<TextTransform>,
}

impl Default for ComputedStyle {
    /// The CSS initial value. The spec makes the initial `border-width` `medium` (an
    /// implementation-defined thickness, roughly 3px), but we use `0` here to avoid drawing
    /// an unintended default border (nothing is drawn anyway, the initial `border-style` being `none`).
    fn default() -> Self {
        let zero_lp = LengthPercentage::Length(0.0);
        Self {
            display: Display::Inline,
            width: LengthPercentageOrAuto::Auto,
            height: LengthPercentageOrAuto::Auto,
            min_width: zero_lp,
            min_height: zero_lp,
            max_width: MaxSize::None,
            max_height: MaxSize::None,
            aspect_ratio: AspectRatio::default(),
            margin_top: LengthPercentageOrAuto::LengthPercentage(zero_lp),
            margin_right: LengthPercentageOrAuto::LengthPercentage(zero_lp),
            margin_bottom: LengthPercentageOrAuto::LengthPercentage(zero_lp),
            margin_left: LengthPercentageOrAuto::LengthPercentage(zero_lp),
            padding_top: zero_lp,
            padding_right: zero_lp,
            padding_bottom: zero_lp,
            padding_left: zero_lp,
            border_top_width: Length(0.0),
            border_right_width: Length(0.0),
            border_bottom_width: Length(0.0),
            border_left_width: Length(0.0),
            // Where currentcolor initially resolves to (the basis when this default value
            // itself has no parent). The actual resolution is done by `resolve_color`.
            border_top_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            border_right_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            border_bottom_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            border_left_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            border_top_style: BorderStyle::None,
            border_right_style: BorderStyle::None,
            border_bottom_style: BorderStyle::None,
            border_left_style: BorderStyle::None,
            border_top_left_radius: CornerRadius::default(),
            border_top_right_radius: CornerRadius::default(),
            border_bottom_right_radius: CornerRadius::default(),
            border_bottom_left_radius: CornerRadius::default(),
            font_size: Length(16.0),
            // The default is an empty Vec, meaning "unspecified". When empty,
            // `select_for_char` falls back to the caller's font (`--font`/`@font-face`)
            // (`sans-serif` resolves to a gothic face only when written explicitly).
            font_family: Vec::new(),
            font_weight: FontWeight::Normal,
            font_style: FontStyle::Normal,
            color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            background_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 0.0,
            },
            background_image: None,
            background_position: BackgroundPosition::default(),
            background_size: BackgroundSize::default(),
            background_repeat: BackgroundRepeat::default(),
            background_attachment: BackgroundAttachment::default(),
            text_decoration_line: TextDecorationLine::default(),
            pseudo_before_content: None,
            pseudo_after_content: None,
            break_before: BreakBetween::Auto,
            break_after: BreakBetween::Auto,
            break_inside: BreakInside::Auto,
            orphans: 2,
            widows: 2,
            float: Float::None,
            clear: Clear::None,
            position: Position::Static,
            top: LengthPercentageOrAuto::Auto,
            right: LengthPercentageOrAuto::Auto,
            bottom: LengthPercentageOrAuto::Auto,
            left: LengthPercentageOrAuto::Auto,
            text_align: TextAlign::Left,
            line_height: LineHeight::Normal,
            text_indent: zero_lp,
            white_space: WhiteSpace::Normal,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            text_transform: TextTransform::None,
            text_shadow: Vec::new(),
            text_overflow: TextOverflow::Clip,
            word_break: WordBreak::Normal,
            overflow_wrap: OverflowWrap::Normal,
            hyphens: Hyphens::Manual,
            text_emphasis_style: EmphasisStyle::None,
            text_emphasis_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            text_emphasis_position: EmphasisPosition::Over,
            grid_template_columns: TrackList::default(),
            grid_template_rows: TrackList::default(),
            grid_auto_columns: Vec::new(),
            grid_auto_rows: Vec::new(),
            grid_auto_flow: GridAutoFlow::Row,
            grid_template_areas: Vec::new(),
            grid_row_start: GridLine::Auto,
            grid_row_end: GridLine::Auto,
            grid_column_start: GridLine::Auto,
            grid_column_end: GridLine::Auto,
            // The initial value of `justify-items` is `legacy` (effectively `stretch`),
            // and of `justify-self` is `auto` (following the parent's `justify-items`).
            justify_items: AlignItems::Stretch,
            justify_self: AlignSelf::Auto,
            border_collapse: BorderCollapse::Separate,
            border_spacing_horizontal: Length(0.0),
            border_spacing_vertical: Length(0.0),
            caption_side: CaptionSide::Top,
            table_layout: TableLayout::Auto,
            empty_cells: EmptyCells::Show,
            vertical_align: VerticalAlign::Baseline,
            list_style_type: ListStyleType::Disc,
            list_style_position: ListStylePosition::Outside,
            list_style_image: None,
            overflow: Overflow::Visible,
            box_sizing: BoxSizing::ContentBox,
            z_index: ZIndex::Auto,
            visibility: Visibility::Visible,
            outline_width: Length(0.0),
            outline_style: BorderStyle::None,
            outline_color: RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 1.0,
            },
            // The same curly quotes as the common browser defaults.
            quotes: Some(vec![
                QuotePair {
                    open: "\u{201C}".to_string(),
                    close: "\u{201D}".to_string(),
                },
                QuotePair {
                    open: "\u{2018}".to_string(),
                    close: "\u{2019}".to_string(),
                },
            ]),
            first_letter_style: None,
            object_fit: ObjectFit::Fill,
            object_position: BackgroundPosition {
                horizontal: LengthPercentage::Percentage(0.5),
                vertical: LengthPercentage::Percentage(0.5),
            },
            box_shadow: Vec::new(),
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: JustifyContent::Normal,
            align_items: AlignItems::Stretch,
            align_content: AlignContent::Stretch,
            align_self: AlignSelf::Auto,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: FlexBasis::Auto,
            row_gap: LengthPercentage::Length(0.0),
            column_gap: LengthPercentage::Length(0.0),
            transform: Vec::new(),
            transform_origin: BackgroundPosition {
                horizontal: LengthPercentage::Percentage(0.5),
                vertical: LengthPercentage::Percentage(0.5),
            },
            opacity: 1.0,
        }
    }
}

/// Compute the styles for the whole DOM. Box-related properties mean nothing on non-element
/// nodes (text and so on), so those simply take the parent's computed style
/// (that is, unchanged, inherited properties included).
///
/// The font size of the root element (`<html>`), which `rem` is relative to, is fixed at
/// the first element found while walking the tree. Before that point (while it is still
/// undecided) the initial value `16px` stands in, which is fine in practice: barring the
/// unheard-of case of the root element specifying its own font-size in `rem`, the root's
/// descendants are always processed after the root has been fixed.
/// A small cache that shares identical computed styles behind a single `Rc`.
///
/// In a document such as a business form, where rows share one style, computing per element
/// still converges on a handful of distinct results. Each is a little over 1KB, so without
/// sharing the memory grows with the node count (hundreds of MB of styles on a 100,000-cell table).
///
/// Comparison is linear from the most recently used, and a hit is moved to the front (MRU).
/// To bound the scanning cost the number kept is capped, and the last entry is dropped on
/// overflow. Dropping one only misses a chance to share; it never changes the result.
#[derive(Default)]
struct StyleInterner {
    recent: Vec<Rc<ComputedStyle>>,
}

/// How many styles the interner remembers. Comfortably more than a table's column count or
/// the variety of headings, and small enough that linear comparison is not a problem.
const STYLE_CACHE_LIMIT: usize = 64;

impl StyleInterner {
    fn intern(&mut self, style: ComputedStyle) -> Rc<ComputedStyle> {
        if let Some(index) = self.recent.iter().position(|known| **known == style) {
            let found = self.recent.remove(index);
            self.recent.insert(0, Rc::clone(&found));
            return found;
        }
        let shared = Rc::new(style);
        self.recent.insert(0, Rc::clone(&shared));
        self.recent.truncate(STYLE_CACHE_LIMIT);
        shared
    }
}

pub fn compute_styles(
    dom: &Dom,
    ua: &Stylesheet,
    author: &Stylesheet,
) -> HashMap<NodeId, Rc<ComputedStyle>> {
    let mut styles = HashMap::new();
    let ctx = StyleContext {
        ua,
        author,
        root_font_size: Cell::new(ComputedStyle::default().font_size.0),
    };
    // Counters and quote depth are each one per document (batch processing handles the whole
    // document in a single walk, so creating them here is enough).
    let mut counters = HashMap::new();
    let mut quote_depth = 0;
    compute_recursive(
        dom,
        dom.document(),
        None,
        false,
        &ctx,
        &mut counters,
        &mut quote_depth,
        &mut styles,
        &mut StyleInterner::default(),
    );
    styles
}

/// A variant of [`compute_styles`]: rather than walking from `dom`'s root (`document()`),
/// it computes an arbitrary `root` (and its descendants) starting from a known parent style
/// `parent_style` and an already-fixed `root_font_size` (the basis for `rem`).
///
/// Streaming uses it to compute styles for just one node, each time a top-level element
/// directly under `<body>` becomes final, carrying over the already-computed style of
/// `<body>`. `dom` itself may be passed as the whole document (only `root` and its
/// descendants are walked). `root` is not a root candidate such as `<html>`, so it does not
/// override the `rem` basis (it is called with `is_root_candidate: false`).
#[allow(clippy::too_many_arguments)]
pub fn compute_styles_with_parent(
    dom: &Dom,
    root: NodeId,
    parent_style: &ComputedStyle,
    root_font_size: f32,
    ua: &Stylesheet,
    author: &Stylesheet,
    counters: &mut HashMap<String, Vec<i32>>,
    quote_depth: &mut i32,
) -> HashMap<NodeId, Rc<ComputedStyle>> {
    let mut styles = HashMap::new();
    let ctx = StyleContext {
        ua,
        author,
        root_font_size: Cell::new(root_font_size),
    };
    compute_recursive(
        dom,
        root,
        Some(parent_style),
        false,
        &ctx,
        counters,
        quote_depth,
        &mut styles,
        &mut StyleInterner::default(),
    );
    styles
}

/// Compute the style of `element` alone, starting from a known parent style.
///
/// Streaming uses it to fix the styles of the `<html>`/`<body>` elements themselves
/// individually, without recursing through all of their descendants. It simply exposes
/// [`compute_element_style`] (the list of counter names this element pushed is not tracked
/// for popping: a `counter-reset` at the `<html>`/`<body>` level may safely persist for the
/// whole document).
#[allow(clippy::too_many_arguments)]
pub fn compute_single_element_style(
    dom: &Dom,
    element: NodeId,
    parent_style: Option<&ComputedStyle>,
    root_font_size: f32,
    ua: &Stylesheet,
    author: &Stylesheet,
    counters: &mut HashMap<String, Vec<i32>>,
    quote_depth: &mut i32,
) -> ComputedStyle {
    compute_element_style(
        dom,
        element,
        parent_style,
        root_font_size,
        ua,
        author,
        counters,
        quote_depth,
    )
    .0
}

/// Values shared across the whole `compute_recursive`/`compute_element_style` recursion
/// that do not change while walking the tree (or are updated one way only, through a
/// `Cell`). A simple grouping to keep the argument count down.
struct StyleContext<'a> {
    ua: &'a Stylesheet,
    author: &'a Stylesheet,
    /// The computed font size of the root element (`<html>`), which `rem` is relative to.
    /// Fixed at the first element found while walking the tree.
    root_font_size: Cell<f32>,
}

/// The return value is the list of counter names `node` itself pushed via `counter-reset`
/// (or implicit creation). Under the CSS spec the scope that push creates extends to
/// "`node` itself and the siblings that follow it" (the rest of `node`'s parent's child
/// list), so the one that can pop is `node`'s parent, not `node` itself. So `node` does not
/// pop here and returns them to the caller (the parent's `compute_recursive`). Counters
/// pushed the same way by `node`'s direct children, on the other hand, may be popped at the
/// end of this function, where the walk of `node`'s child list (their sibling scope) ends.
#[allow(clippy::too_many_arguments)]
fn compute_recursive(
    dom: &Dom,
    node: NodeId,
    parent_style: Option<&ComputedStyle>,
    is_root_candidate: bool,
    ctx: &StyleContext<'_>,
    counters: &mut HashMap<String, Vec<i32>>,
    quote_depth: &mut i32,
    out: &mut HashMap<NodeId, Rc<ComputedStyle>>,
    interner: &mut StyleInterner,
) -> Vec<String> {
    let (mut style, own_pushed_counter_names, after_parts) = match &dom.node(node).data {
        NodeData::Element { .. } => {
            let (style, pushed_counter_names, after_parts) = compute_element_style(
                dom,
                node,
                parent_style,
                ctx.root_font_size.get(),
                ctx.ua,
                ctx.author,
                counters,
                quote_depth,
            );
            // The first element directly under the document (usually <html>) is the root element.
            if is_root_candidate {
                ctx.root_font_size.set(style.font_size.0);
            }
            (style, pushed_counter_names, after_parts)
        }
        _ => (parent_style.cloned().unwrap_or_default(), Vec::new(), None),
    };

    // If `node` is the document node, its direct child (usually <html>) is the root element candidate.
    let children_are_root_candidates = node == dom.document();
    let mut children_pushed_counter_names = Vec::new();
    for child in dom.children(node) {
        let pushed_by_child = compute_recursive(
            dom,
            child,
            Some(&style),
            children_are_root_candidates,
            ctx,
            counters,
            quote_depth,
            out,
            interner,
        );
        children_pushed_counter_names.extend(pushed_by_child);
    }

    // End the scope of the counters pushed by the direct children (and their whole sibling scope).
    // What `node` itself pushed is not popped here (the caller, `node`'s parent, pops it;
    // see the comment above).
    for name in &children_pushed_counter_names {
        if let Some(stack) = counters.get_mut(name) {
            stack.pop();
        }
    }

    // Resolve the `::after` content against the state as it now stands (with counter()/quotes
    // reflecting any changes made by the descendants).
    let quotes = style.quotes.clone();
    style.pseudo_after_content =
        resolve_content_parts(after_parts, dom, node, counters, quote_depth, &quotes);

    out.insert(node, interner.intern(style));
    own_pushed_counter_names
}

/// Compute the style of `element`. Returns the triple (style, list of pushed counter names,
/// unresolved ::after content).
///
/// The `Vec<String>` is the list of counter names this element pushed onto `counters` via
/// `counter-reset` (or implicit creation from a `counter-increment` on an undefined
/// counter), which the caller (`compute_recursive`) uses to pop the same number after
/// processing the descendants.
///
/// The `content` of `::after` is not resolved at this point and comes back as an
/// `Option<Vec<ContentPart>>`. `::after` appears after the descendants in DOM order, so
/// `counter()`/`quotes` (which should reflect changes made by the descendants) can only be
/// resolved once those are processed (the caller's `compute_recursive` calls
/// `resolve_content_parts` after the child loop and fills in `ComputedStyle::pseudo_after_content`).
#[allow(clippy::too_many_arguments)]
fn compute_element_style(
    dom: &Dom,
    element: NodeId,
    parent: Option<&ComputedStyle>,
    root_font_size: f32,
    ua: &Stylesheet,
    author: &Stylesheet,
    counters: &mut HashMap<String, Vec<i32>>,
    quote_depth: &mut i32,
) -> (ComputedStyle, Vec<String>, Option<Vec<ContentPart>>) {
    let (ua_declarations, author_declarations) =
        matching_declarations_by_origin(dom, element, ua, author);
    let inline_declarations = inline_style_declarations(dom, element);
    // Legacy presentational attributes (`bgcolor`/`align` and friends) and the
    // `data-page-break` sugar sit stronger than the UA stylesheet and weaker than author CSS.
    let mut attribute_declarations = presentational_hint_declarations(dom, element);
    attribute_declarations.extend(data_page_break_declarations(dom, element));

    let mut display = None;
    let mut width = None;
    let mut height = None;
    let mut margin_top = None;
    let mut margin_right = None;
    let mut margin_bottom = None;
    let mut margin_left = None;
    let mut padding_top = None;
    let mut padding_right = None;
    let mut padding_bottom = None;
    let mut padding_left = None;
    let mut border_top_width = None;
    let mut border_right_width = None;
    let mut border_bottom_width = None;
    let mut border_left_width = None;
    let mut border_top_color = None;
    let mut border_right_color = None;
    let mut border_bottom_color = None;
    let mut border_left_color = None;
    let mut border_top_style = None;
    let mut border_right_style = None;
    let mut border_bottom_style = None;
    let mut border_left_style = None;
    let mut border_top_left_radius = None;
    let mut border_top_right_radius = None;
    let mut border_bottom_right_radius = None;
    let mut border_bottom_left_radius = None;
    let mut font_size = None;
    let mut font_family = None;
    let mut font_weight = None;
    let mut font_style = None;
    let mut color = None;
    let mut background_color = None;
    let mut background_image = None;
    let mut background_position = None;
    let mut background_size = None;
    let mut background_repeat = None;
    let mut background_attachment = None;
    let mut text_decoration_line = None;
    let mut break_before = None;
    let mut break_after = None;
    let mut break_inside = None;
    let mut orphans = None;
    let mut widows = None;
    let mut float = None;
    let mut clear = None;
    let mut position = None;
    let mut top = None;
    let mut right = None;
    let mut bottom = None;
    let mut left = None;
    let mut text_align = None;
    let mut line_height = None;
    let mut text_indent = None;
    let mut white_space = None;
    let mut letter_spacing = None;
    let mut word_spacing = None;
    let mut text_transform = None;
    let mut text_shadow = None;
    let mut text_overflow = None;
    let mut word_break = None;
    let mut overflow_wrap = None;
    let mut hyphens = None;
    let mut text_emphasis_style = None;
    let mut text_emphasis_color = None;
    let mut text_emphasis_position = None;
    let mut grid_template_columns = None;
    let mut grid_template_rows = None;
    let mut grid_auto_columns = None;
    let mut grid_auto_rows = None;
    let mut grid_auto_flow = None;
    let mut grid_template_areas = None;
    let mut grid_row_start = None;
    let mut grid_row_end = None;
    let mut grid_column_start = None;
    let mut grid_column_end = None;
    let mut justify_items = None;
    let mut justify_self = None;
    let mut border_collapse = None;
    let mut border_spacing = None;
    let mut caption_side = None;
    let mut table_layout = None;
    let mut empty_cells = None;
    let mut vertical_align = None;
    let mut list_style_type = None;
    let mut list_style_position = None;
    let mut list_style_image = None;
    let mut overflow = None;
    let mut box_sizing = None;
    let mut z_index = None;
    let mut visibility = None;
    let mut outline_width = None;
    let mut outline_style = None;
    let mut outline_color = None;
    let mut counter_reset = None;
    let mut min_width = None;
    let mut min_height = None;
    let mut max_width = None;
    let mut max_height = None;
    let mut aspect_ratio = None;
    let mut counter_increment = None;
    let mut quotes = None;
    let mut object_fit = None;
    let mut object_position = None;
    let mut box_shadow = None;
    let mut flex_direction = None;
    let mut flex_wrap = None;
    let mut justify_content = None;
    let mut align_items = None;
    let mut align_content = None;
    let mut align_self = None;
    let mut flex_grow = None;
    let mut flex_shrink = None;
    let mut flex_basis = None;
    let mut row_gap = None;
    let mut column_gap = None;
    let mut transform = None;
    let mut transform_origin = None;
    let mut opacity = None;

    // We walk in cascade order (ascending priority), so a later find naturally wins.
    // Declarations from HTML attributes (legacy presentational attributes and the
    // `data-page-break` sugar) count as "default hints, stronger than the UA stylesheet but
    // overridable by author CSS", so they sit between the two. An inline style attribute
    // outranks every selector-based declaration, so it goes last.
    for decl in ua_declarations
        .into_iter()
        .chain(attribute_declarations.iter())
        .chain(author_declarations)
        .chain(inline_declarations.iter())
    {
        match decl {
            PropertyDeclaration::Display(v) => display = Some(*v),
            PropertyDeclaration::Width(v) => width = Some(*v),
            PropertyDeclaration::Height(v) => height = Some(*v),
            PropertyDeclaration::MinWidth(v) => min_width = Some(*v),
            PropertyDeclaration::MinHeight(v) => min_height = Some(*v),
            PropertyDeclaration::MaxWidth(v) => max_width = Some(*v),
            PropertyDeclaration::MaxHeight(v) => max_height = Some(*v),
            PropertyDeclaration::AspectRatio(v) => aspect_ratio = Some(*v),
            PropertyDeclaration::MarginTop(v) => margin_top = Some(*v),
            PropertyDeclaration::MarginRight(v) => margin_right = Some(*v),
            PropertyDeclaration::MarginBottom(v) => margin_bottom = Some(*v),
            PropertyDeclaration::MarginLeft(v) => margin_left = Some(*v),
            PropertyDeclaration::PaddingTop(v) => padding_top = Some(*v),
            PropertyDeclaration::PaddingRight(v) => padding_right = Some(*v),
            PropertyDeclaration::PaddingBottom(v) => padding_bottom = Some(*v),
            PropertyDeclaration::PaddingLeft(v) => padding_left = Some(*v),
            PropertyDeclaration::BorderTopWidth(v) => border_top_width = Some(*v),
            PropertyDeclaration::BorderRightWidth(v) => border_right_width = Some(*v),
            PropertyDeclaration::BorderBottomWidth(v) => border_bottom_width = Some(*v),
            PropertyDeclaration::BorderLeftWidth(v) => border_left_width = Some(*v),
            PropertyDeclaration::BorderTopColor(v) => border_top_color = Some(*v),
            PropertyDeclaration::BorderRightColor(v) => border_right_color = Some(*v),
            PropertyDeclaration::BorderBottomColor(v) => border_bottom_color = Some(*v),
            PropertyDeclaration::BorderLeftColor(v) => border_left_color = Some(*v),
            PropertyDeclaration::BorderTopStyle(v) => border_top_style = Some(*v),
            PropertyDeclaration::BorderRightStyle(v) => border_right_style = Some(*v),
            PropertyDeclaration::BorderBottomStyle(v) => border_bottom_style = Some(*v),
            PropertyDeclaration::BorderLeftStyle(v) => border_left_style = Some(*v),
            PropertyDeclaration::BorderTopLeftRadius(v) => border_top_left_radius = Some(*v),
            PropertyDeclaration::BorderTopRightRadius(v) => border_top_right_radius = Some(*v),
            PropertyDeclaration::BorderBottomRightRadius(v) => {
                border_bottom_right_radius = Some(*v)
            }
            PropertyDeclaration::BorderBottomLeftRadius(v) => border_bottom_left_radius = Some(*v),
            PropertyDeclaration::FontSize(v) => font_size = Some(*v),
            PropertyDeclaration::FontFamily(v) => font_family = Some(v.clone()),
            PropertyDeclaration::FontWeight(v) => font_weight = Some(*v),
            PropertyDeclaration::FontStyle(v) => font_style = Some(*v),
            PropertyDeclaration::Color(v) => color = Some(*v),
            PropertyDeclaration::BackgroundColor(v) => background_color = Some(*v),
            PropertyDeclaration::BackgroundImage(v) => background_image = v.clone(),
            PropertyDeclaration::BackgroundPosition(v) => background_position = Some(*v),
            PropertyDeclaration::BackgroundSize(v) => background_size = Some(*v),
            PropertyDeclaration::BackgroundRepeat(v) => background_repeat = Some(*v),
            PropertyDeclaration::BackgroundAttachment(v) => background_attachment = Some(*v),
            PropertyDeclaration::TextDecorationLine(v) => text_decoration_line = Some(*v),
            // `content` is for `::before`/`::after` only and has no effect on an ordinary
            // element (`matching_pseudo_content` does the pseudo-element matching separately).
            PropertyDeclaration::Content(_) => {}
            PropertyDeclaration::BreakBefore(v) => break_before = Some(*v),
            PropertyDeclaration::BreakAfter(v) => break_after = Some(*v),
            PropertyDeclaration::BreakInside(v) => break_inside = Some(*v),
            PropertyDeclaration::Orphans(v) => orphans = Some(*v),
            PropertyDeclaration::Widows(v) => widows = Some(*v),
            PropertyDeclaration::Float(v) => float = Some(*v),
            PropertyDeclaration::Clear(v) => clear = Some(*v),
            PropertyDeclaration::Position(v) => position = Some(*v),
            PropertyDeclaration::Top(v) => top = Some(*v),
            PropertyDeclaration::Right(v) => right = Some(*v),
            PropertyDeclaration::Bottom(v) => bottom = Some(*v),
            PropertyDeclaration::Left(v) => left = Some(*v),
            PropertyDeclaration::TextAlign(v) => text_align = Some(*v),
            PropertyDeclaration::LineHeight(v) => line_height = Some(*v),
            PropertyDeclaration::TextIndent(v) => text_indent = Some(*v),
            PropertyDeclaration::WhiteSpace(v) => white_space = Some(*v),
            PropertyDeclaration::LetterSpacing(v) => letter_spacing = Some(*v),
            PropertyDeclaration::WordSpacing(v) => word_spacing = Some(*v),
            PropertyDeclaration::TextTransform(v) => text_transform = Some(*v),
            PropertyDeclaration::TextShadow(v) => text_shadow = Some(v.clone()),
            PropertyDeclaration::TextOverflow(v) => text_overflow = Some(*v),
            PropertyDeclaration::WordBreak(v) => word_break = Some(*v),
            PropertyDeclaration::OverflowWrap(v) => overflow_wrap = Some(*v),
            PropertyDeclaration::Hyphens(v) => hyphens = Some(*v),
            PropertyDeclaration::TextEmphasisStyle(v) => text_emphasis_style = Some(v.clone()),
            PropertyDeclaration::TextEmphasisColor(v) => text_emphasis_color = Some(*v),
            PropertyDeclaration::TextEmphasisPosition(v) => text_emphasis_position = Some(*v),
            PropertyDeclaration::GridTemplateColumns(v) => grid_template_columns = Some(v.clone()),
            PropertyDeclaration::GridTemplateRows(v) => grid_template_rows = Some(v.clone()),
            PropertyDeclaration::GridAutoColumns(v) => grid_auto_columns = Some(v.clone()),
            PropertyDeclaration::GridAutoRows(v) => grid_auto_rows = Some(v.clone()),
            PropertyDeclaration::GridAutoFlow(v) => grid_auto_flow = Some(*v),
            PropertyDeclaration::GridTemplateAreas(v) => grid_template_areas = Some(v.clone()),
            PropertyDeclaration::GridRowStart(v) => grid_row_start = Some(v.clone()),
            PropertyDeclaration::GridRowEnd(v) => grid_row_end = Some(v.clone()),
            PropertyDeclaration::GridColumnStart(v) => grid_column_start = Some(v.clone()),
            PropertyDeclaration::GridColumnEnd(v) => grid_column_end = Some(v.clone()),
            PropertyDeclaration::JustifyItems(v) => justify_items = Some(*v),
            PropertyDeclaration::JustifySelf(v) => justify_self = Some(*v),
            PropertyDeclaration::BorderCollapse(v) => border_collapse = Some(*v),
            PropertyDeclaration::BorderSpacing(h, v) => border_spacing = Some((*h, *v)),
            PropertyDeclaration::CaptionSide(v) => caption_side = Some(*v),
            PropertyDeclaration::TableLayout(v) => table_layout = Some(*v),
            PropertyDeclaration::EmptyCells(v) => empty_cells = Some(*v),
            PropertyDeclaration::VerticalAlign(v) => vertical_align = Some(*v),
            PropertyDeclaration::ListStyleType(v) => list_style_type = Some(*v),
            PropertyDeclaration::ListStylePosition(v) => list_style_position = Some(*v),
            PropertyDeclaration::ListStyleImage(v) => list_style_image = Some(v.clone()),
            PropertyDeclaration::Overflow(v) => overflow = Some(*v),
            PropertyDeclaration::BoxSizing(v) => box_sizing = Some(*v),
            PropertyDeclaration::ZIndex(v) => z_index = Some(*v),
            PropertyDeclaration::Visibility(v) => visibility = Some(*v),
            PropertyDeclaration::OutlineWidth(v) => outline_width = Some(*v),
            PropertyDeclaration::OutlineStyle(v) => outline_style = Some(*v),
            PropertyDeclaration::OutlineColor(v) => outline_color = Some(*v),
            PropertyDeclaration::CounterReset(v) => counter_reset = Some(v.clone()),
            PropertyDeclaration::CounterIncrement(v) => counter_increment = Some(v.clone()),
            PropertyDeclaration::Quotes(v) => quotes = Some(v.clone()),
            PropertyDeclaration::ObjectFit(v) => object_fit = Some(*v),
            PropertyDeclaration::ObjectPosition(v) => object_position = Some(*v),
            PropertyDeclaration::BoxShadow(v) => box_shadow = Some(v.clone()),
            PropertyDeclaration::FlexDirection(v) => flex_direction = Some(*v),
            PropertyDeclaration::FlexWrap(v) => flex_wrap = Some(*v),
            PropertyDeclaration::JustifyContent(v) => justify_content = Some(*v),
            PropertyDeclaration::AlignItems(v) => align_items = Some(*v),
            PropertyDeclaration::AlignContent(v) => align_content = Some(*v),
            PropertyDeclaration::AlignSelf(v) => align_self = Some(*v),
            PropertyDeclaration::FlexGrow(v) => flex_grow = Some(*v),
            PropertyDeclaration::FlexShrink(v) => flex_shrink = Some(*v),
            PropertyDeclaration::FlexBasis(v) => flex_basis = Some(*v),
            PropertyDeclaration::RowGap(v) => row_gap = Some(*v),
            PropertyDeclaration::ColumnGap(v) => column_gap = Some(*v),
            PropertyDeclaration::Transform(v) => transform = Some(v.clone()),
            PropertyDeclaration::TransformOrigin(v) => transform_origin = Some(*v),
            PropertyDeclaration::Opacity(v) => opacity = Some(*v),
        }
    }

    let initial = ComputedStyle::default();
    let inherited_font_size = parent.map_or(initial.font_size, |p| p.font_size);
    let inherited_font_family =
        parent.map_or_else(|| initial.font_family.clone(), |p| p.font_family.clone());
    let inherited_font_weight = parent.map_or(initial.font_weight, |p| p.font_weight);
    let inherited_font_style = parent.map_or(initial.font_style, |p| p.font_style);
    let inherited_color = parent.map_or(initial.color, |p| p.color);
    let inherited_text_decoration_line =
        parent.map_or(initial.text_decoration_line, |p| p.text_decoration_line);
    let inherited_text_align = parent.map_or(initial.text_align, |p| p.text_align);
    let inherited_line_height = parent.map_or(initial.line_height, |p| p.line_height);
    let inherited_text_indent = parent.map_or(initial.text_indent, |p| p.text_indent);
    let inherited_white_space = parent.map_or(initial.white_space, |p| p.white_space);
    let inherited_letter_spacing = parent.map_or(initial.letter_spacing, |p| p.letter_spacing);
    let inherited_word_spacing = parent.map_or(initial.word_spacing, |p| p.word_spacing);
    let inherited_text_transform = parent.map_or(initial.text_transform, |p| p.text_transform);
    let inherited_word_break = parent.map_or(initial.word_break, |p| p.word_break);
    let inherited_overflow_wrap = parent.map_or(initial.overflow_wrap, |p| p.overflow_wrap);
    let inherited_hyphens = parent.map_or(initial.hyphens, |p| p.hyphens);
    let inherited_emphasis_position =
        parent.map_or(initial.text_emphasis_position, |p| p.text_emphasis_position);
    let inherited_emphasis_style = parent
        .map(|p| p.text_emphasis_style.clone())
        .unwrap_or_else(|| initial.text_emphasis_style.clone());
    let inherited_border_collapse = parent.map_or(initial.border_collapse, |p| p.border_collapse);
    let inherited_border_spacing_horizontal = parent
        .map_or(initial.border_spacing_horizontal, |p| {
            p.border_spacing_horizontal
        });
    let inherited_border_spacing_vertical = parent.map_or(initial.border_spacing_vertical, |p| {
        p.border_spacing_vertical
    });
    let inherited_caption_side = parent.map_or(initial.caption_side, |p| p.caption_side);
    let inherited_empty_cells = parent.map_or(initial.empty_cells, |p| p.empty_cells);
    let inherited_list_style_type = parent.map_or(initial.list_style_type, |p| p.list_style_type);
    let inherited_list_style_position =
        parent.map_or(initial.list_style_position, |p| p.list_style_position);
    let inherited_list_style_image = parent.map_or_else(
        || initial.list_style_image.clone(),
        |p| p.list_style_image.clone(),
    );
    let inherited_visibility = parent.map_or(initial.visibility, |p| p.visibility);
    let inherited_quotes = parent.map_or_else(|| initial.quotes.clone(), |p| p.quotes.clone());

    // font-size is resolved before the other length properties. Per the spec, `em` is
    // relative to "the parent element's computed font-size" (not its own value, to avoid a cycle).
    let resolved_font_size = font_size
        .map(|specified| specified.resolve(inherited_font_size.0, root_font_size))
        .unwrap_or(inherited_font_size);
    // For length properties other than font-size, `em` is relative to this element's own (just-resolved) font-size.
    let own_font_size = resolved_font_size.0;
    let resolve_lp_or_auto = |v: Option<SpecifiedLengthPercentageOrAuto>,
                              initial: LengthPercentageOrAuto| {
        v.map(|specified| specified.resolve(own_font_size, root_font_size))
            .unwrap_or(initial)
    };
    let resolve_lp = |v: Option<SpecifiedLengthPercentage>, initial: LengthPercentage| {
        v.map(|specified| specified.resolve(own_font_size, root_font_size))
            .unwrap_or(initial)
    };
    let resolve_max_size = |v: Option<SpecifiedMaxSize>| {
        v.map(|specified| specified.resolve(own_font_size, root_font_size))
            .unwrap_or(MaxSize::None)
    };
    let resolve_len = |v: Option<SpecifiedLength>, initial: Length| {
        v.map(|specified| specified.resolve(own_font_size, root_font_size))
            .unwrap_or(initial)
    };
    let resolve_corner_radius = |v: Option<SpecifiedCornerRadius>, initial: CornerRadius| {
        v.map(|specified| specified.resolve(own_font_size, root_font_size))
            .unwrap_or(initial)
    };
    let resolved_background_position = background_position
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.background_position);
    let resolved_background_size = background_size
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.background_size);

    // The `<number>`/`<percentage>` of `line-height` is inherited unmultiplied.
    // A `<percentage>` is already parsed as a fraction (50% -> 0.5), so it can be handled
    // exactly like a `<number>`.
    let resolved_line_height = match line_height {
        Some(SpecifiedLineHeight::Normal) => LineHeight::Normal,
        Some(SpecifiedLineHeight::Number(n) | SpecifiedLineHeight::Percentage(n)) => {
            LineHeight::Number(n)
        }
        Some(SpecifiedLineHeight::Length(l)) => {
            LineHeight::Length(l.resolve(own_font_size, root_font_size).0)
        }
        None => inherited_line_height,
    };
    let resolved_letter_spacing = letter_spacing
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(inherited_letter_spacing);
    let resolved_word_spacing = word_spacing
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(inherited_word_spacing);
    let resolved_border_spacing_horizontal = border_spacing
        .map(|(h, _)| resolve_len(Some(h), inherited_border_spacing_horizontal))
        .unwrap_or(inherited_border_spacing_horizontal);
    let resolved_border_spacing_vertical = border_spacing
        .map(|(_, v)| resolve_len(Some(v), inherited_border_spacing_vertical))
        .unwrap_or(inherited_border_spacing_vertical);

    let resolved_color = resolve_color(color, inherited_color);
    let resolved_background_color = match background_color {
        Some(Color::Rgba {
            red,
            green,
            blue,
            alpha,
        }) => RgbaColor {
            red,
            green,
            blue,
            alpha,
        },
        // `background-color: currentcolor` uses this element's own computed color.
        Some(Color::CurrentColor) => resolved_color,
        None => initial.background_color,
    };
    // The initial value of `border-color` is `currentcolor` per the spec, so even when
    // unspecified it resolves to this element's own computed color (as it would if written explicitly).
    let resolved_border_top_color = resolve_color(border_top_color, resolved_color);
    let resolved_border_right_color = resolve_color(border_right_color, resolved_color);
    let resolved_border_bottom_color = resolve_color(border_bottom_color, resolved_color);
    let resolved_border_left_color = resolve_color(border_left_color, resolved_color);
    // The initial value of `outline-color` is `currentcolor` too (per the spec).
    let resolved_outline_color = resolve_color(outline_color, resolved_color);

    let resolved_object_position = object_position
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.object_position);
    // `text-shadow` is inherited, so with no declaration the parent's already-resolved value
    // carries over unchanged (its colour stays resolved against the parent's `color`; the
    // CSS spec also fixes `currentcolor` at the value it had when inherited).
    let resolved_text_shadow: Vec<ComputedTextShadow> = match text_shadow {
        Some(shadows) => shadows
            .into_iter()
            .map(|specified| {
                let resolved = specified.resolve(own_font_size, root_font_size);
                ComputedTextShadow {
                    offset_x: resolved.offset_x,
                    offset_y: resolved.offset_y,
                    blur_radius: resolved.blur_radius.max(0.0),
                    color: resolve_color(resolved.color, resolved_color),
                }
            })
            .collect(),
        None => parent.map(|p| p.text_shadow.clone()).unwrap_or_default(),
    };

    // `text-emphasis-color` is inherited too. Its initial value is `currentcolor`
    // (that is, this element's own `color`).
    let resolved_text_emphasis_color = match text_emphasis_color {
        Some(color) => resolve_color(Some(color), resolved_color),
        None => parent.map_or(resolved_color, |p| p.text_emphasis_color),
    };

    // em/rem and `currentcolor` resolution for each comma-separated `box-shadow` entry.
    // The spec makes a negative `blur-radius` invalid, but rather than reject it at parse
    // time we clamp anything below 0 here (simple robustness, following the existing pattern).
    let resolved_box_shadow: Vec<ComputedBoxShadow> = box_shadow
        .unwrap_or_default()
        .into_iter()
        .map(|specified| {
            let resolved = specified.resolve(own_font_size, root_font_size);
            ComputedBoxShadow {
                offset_x: resolved.offset_x,
                offset_y: resolved.offset_y,
                blur_radius: resolved.blur_radius.max(0.0),
                spread_radius: resolved.spread_radius,
                color: resolve_color(resolved.color, resolved_color),
                inset: resolved.inset,
            }
        })
        .collect();

    // Flexbox-related. All are non-inherited properties, so no `inherited_*` is needed to
    // compare against the initial value.
    let resolved_flex_basis = flex_basis
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.flex_basis);
    let resolved_row_gap = row_gap
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.row_gap);
    let resolved_column_gap = column_gap
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.column_gap);

    // `transform`/`transform-origin`/`opacity`. All are non-inherited properties.
    let resolved_transform = transform
        .map(|specified| {
            specified
                .into_iter()
                .map(|f| f.resolve(own_font_size, root_font_size))
                .collect()
        })
        .unwrap_or_else(|| initial.transform.clone());
    let resolved_transform_origin = transform_origin
        .map(|specified| specified.resolve(own_font_size, root_font_size))
        .unwrap_or(initial.transform_origin);
    let resolved_opacity = opacity.unwrap_or(initial.opacity);

    let resolved_float = float.unwrap_or(initial.float);
    let resolved_position = position.unwrap_or(initial.position);
    // CSS2.1 9.7: with a float other than `none`, or `position: absolute`/`fixed`, the
    // element automatically computes to block-level (even with `display: inline`).
    // That lets the `Block` arm of `box_tree.rs::child_kind` work unchanged, so an inline
    // element (`<span style="position: absolute">`) is also picked up by `box_tree`'s
    // Blocks loop.
    let resolved_display = match display.unwrap_or(initial.display) {
        Display::Inline if resolved_float != Float::None || resolved_position.is_out_of_flow() => {
            Display::Block
        }
        other => other,
    };

    let resolved_quotes = quotes.unwrap_or(inherited_quotes);

    // Applying `counter-reset`/`counter-increment`. It has to happen before the
    // `counter`/`counters` in `content` are resolved, so that the values this element itself
    // reset or incremented are visible to this element's
    // `content`.
    let mut pushed_counter_names = Vec::new();
    if let Some(resets) = &counter_reset {
        for (name, value) in resets {
            counters.entry(name.clone()).or_default().push(*value);
            pushed_counter_names.push(name.clone());
        }
    }
    if let Some(increments) = &counter_increment {
        for (name, value) in increments {
            let stack = counters.entry(name.clone()).or_default();
            if stack.is_empty() {
                // Implicit creation when no counter of that name exists in scope
                // (a simplification: it should really persist for the whole document, but
                // it is popped on leaving this element's subtree; a known simplification).
                stack.push(0);
                pushed_counter_names.push(name.clone());
            }
            *stack.last_mut().expect("just ensured non-empty") += value;
        }
    }

    // Resolving `content` (::before). It needs the "current" state of the counters and quote
    // depth, so it comes after the reset/increment above. `::before` appears before the
    // descendants in DOM order, so resolving it here (before walking them) is correct.
    //
    // `::after`, conversely, appears after the descendants in DOM order, so it is not
    // resolved here (the `counter()`/`quotes` state should reflect changes made by the
    // descendants). The list of parts is returned unresolved to the caller
    // (`compute_recursive`), which resolves it once the descendants are processed.
    let before_parts = matching_pseudo_content(dom, element, PseudoElement::Before, ua, author);
    let after_parts = matching_pseudo_content(dom, element, PseudoElement::After, ua, author);
    let pseudo_before_content = resolve_content_parts(
        before_parts,
        dom,
        element,
        counters,
        quote_depth,
        &resolved_quotes,
    );

    // `::first-letter`. A limited override style covering only the supported properties.
    let first_letter_declarations =
        matching_pseudo_declarations(dom, element, PseudoElement::FirstLetter, ua, author);
    let first_letter_style = compute_first_letter_style(
        &first_letter_declarations,
        resolved_font_size.0,
        root_font_size,
    );

    let style = ComputedStyle {
        display: resolved_display,
        width: resolve_lp_or_auto(width, initial.width),
        height: resolve_lp_or_auto(height, initial.height),
        min_width: resolve_lp(min_width, initial.min_width),
        min_height: resolve_lp(min_height, initial.min_height),
        max_width: resolve_max_size(max_width),
        max_height: resolve_max_size(max_height),
        aspect_ratio: aspect_ratio.unwrap_or_default(),
        margin_top: resolve_lp_or_auto(margin_top, initial.margin_top),
        margin_right: resolve_lp_or_auto(margin_right, initial.margin_right),
        margin_bottom: resolve_lp_or_auto(margin_bottom, initial.margin_bottom),
        margin_left: resolve_lp_or_auto(margin_left, initial.margin_left),
        padding_top: resolve_lp(padding_top, initial.padding_top),
        padding_right: resolve_lp(padding_right, initial.padding_right),
        padding_bottom: resolve_lp(padding_bottom, initial.padding_bottom),
        padding_left: resolve_lp(padding_left, initial.padding_left),
        border_top_width: resolve_len(border_top_width, initial.border_top_width),
        border_right_width: resolve_len(border_right_width, initial.border_right_width),
        border_bottom_width: resolve_len(border_bottom_width, initial.border_bottom_width),
        border_left_width: resolve_len(border_left_width, initial.border_left_width),
        border_top_color: resolved_border_top_color,
        border_right_color: resolved_border_right_color,
        border_bottom_color: resolved_border_bottom_color,
        border_left_color: resolved_border_left_color,
        border_top_style: border_top_style.unwrap_or(initial.border_top_style),
        border_right_style: border_right_style.unwrap_or(initial.border_right_style),
        border_bottom_style: border_bottom_style.unwrap_or(initial.border_bottom_style),
        border_left_style: border_left_style.unwrap_or(initial.border_left_style),
        border_top_left_radius: resolve_corner_radius(
            border_top_left_radius,
            initial.border_top_left_radius,
        ),
        border_top_right_radius: resolve_corner_radius(
            border_top_right_radius,
            initial.border_top_right_radius,
        ),
        border_bottom_right_radius: resolve_corner_radius(
            border_bottom_right_radius,
            initial.border_bottom_right_radius,
        ),
        border_bottom_left_radius: resolve_corner_radius(
            border_bottom_left_radius,
            initial.border_bottom_left_radius,
        ),
        font_size: resolved_font_size,
        font_family: font_family.unwrap_or(inherited_font_family),
        font_weight: font_weight.unwrap_or(inherited_font_weight),
        font_style: font_style.unwrap_or(inherited_font_style),
        color: resolved_color,
        background_color: resolved_background_color,
        background_image: background_image.or(initial.background_image),
        background_position: resolved_background_position,
        background_size: resolved_background_size,
        background_repeat: background_repeat.unwrap_or(initial.background_repeat),
        background_attachment: background_attachment.unwrap_or(initial.background_attachment),
        text_decoration_line: text_decoration_line.unwrap_or(inherited_text_decoration_line),
        pseudo_before_content,
        // `compute_recursive` resolves it after processing the descendants and fills in this
        // field (the unresolved `after_parts` are returned as the third element).
        pseudo_after_content: None,
        break_before: break_before.unwrap_or(initial.break_before),
        break_after: break_after.unwrap_or(initial.break_after),
        break_inside: break_inside.unwrap_or(initial.break_inside),
        orphans: orphans.unwrap_or(initial.orphans),
        widows: widows.unwrap_or(initial.widows),
        float: resolved_float,
        clear: clear.unwrap_or(initial.clear),
        position: resolved_position,
        top: resolve_lp_or_auto(top, initial.top),
        right: resolve_lp_or_auto(right, initial.right),
        bottom: resolve_lp_or_auto(bottom, initial.bottom),
        left: resolve_lp_or_auto(left, initial.left),
        text_align: text_align.unwrap_or(inherited_text_align),
        line_height: resolved_line_height,
        text_indent: resolve_lp(text_indent, inherited_text_indent),
        white_space: white_space.unwrap_or(inherited_white_space),
        letter_spacing: resolved_letter_spacing,
        word_spacing: resolved_word_spacing,
        text_transform: text_transform.unwrap_or(inherited_text_transform),
        text_shadow: resolved_text_shadow,
        text_overflow: text_overflow.unwrap_or_default(),
        word_break: word_break.unwrap_or(inherited_word_break),
        overflow_wrap: overflow_wrap.unwrap_or(inherited_overflow_wrap),
        hyphens: hyphens.unwrap_or(inherited_hyphens),
        text_emphasis_style: text_emphasis_style.unwrap_or(inherited_emphasis_style),
        text_emphasis_color: resolved_text_emphasis_color,
        text_emphasis_position: text_emphasis_position.unwrap_or(inherited_emphasis_position),
        grid_template_columns: grid_template_columns
            .map(|list| list.resolve(own_font_size, root_font_size))
            .unwrap_or_default(),
        grid_template_rows: grid_template_rows
            .map(|list| list.resolve(own_font_size, root_font_size))
            .unwrap_or_default(),
        grid_auto_columns: grid_auto_columns
            .map(|sizes| resolve_track_sizes(&sizes, own_font_size, root_font_size))
            .unwrap_or_default(),
        grid_auto_rows: grid_auto_rows
            .map(|sizes| resolve_track_sizes(&sizes, own_font_size, root_font_size))
            .unwrap_or_default(),
        grid_auto_flow: grid_auto_flow.unwrap_or(initial.grid_auto_flow),
        grid_template_areas: grid_template_areas.unwrap_or_default(),
        grid_row_start: grid_row_start.unwrap_or(GridLine::Auto),
        grid_row_end: grid_row_end.unwrap_or(GridLine::Auto),
        grid_column_start: grid_column_start.unwrap_or(GridLine::Auto),
        grid_column_end: grid_column_end.unwrap_or(GridLine::Auto),
        justify_items: justify_items.unwrap_or(initial.justify_items),
        justify_self: justify_self.unwrap_or(initial.justify_self),
        border_collapse: border_collapse.unwrap_or(inherited_border_collapse),
        border_spacing_horizontal: resolved_border_spacing_horizontal,
        border_spacing_vertical: resolved_border_spacing_vertical,
        caption_side: caption_side.unwrap_or(inherited_caption_side),
        table_layout: table_layout.unwrap_or(initial.table_layout),
        empty_cells: empty_cells.unwrap_or(inherited_empty_cells),
        vertical_align: vertical_align
            .map(|v| v.resolve(own_font_size, root_font_size))
            .unwrap_or(initial.vertical_align),
        list_style_type: list_style_type.unwrap_or(inherited_list_style_type),
        list_style_position: list_style_position.unwrap_or(inherited_list_style_position),
        list_style_image: list_style_image.unwrap_or(inherited_list_style_image),
        overflow: overflow.unwrap_or(initial.overflow),
        box_sizing: box_sizing.unwrap_or(initial.box_sizing),
        z_index: z_index.unwrap_or(initial.z_index),
        visibility: visibility.unwrap_or(inherited_visibility),
        outline_width: resolve_len(outline_width, initial.outline_width),
        outline_style: outline_style.unwrap_or(initial.outline_style),
        outline_color: resolved_outline_color,
        quotes: resolved_quotes,
        first_letter_style,
        object_fit: object_fit.unwrap_or(initial.object_fit),
        object_position: resolved_object_position,
        box_shadow: resolved_box_shadow,
        flex_direction: flex_direction.unwrap_or(initial.flex_direction),
        flex_wrap: flex_wrap.unwrap_or(initial.flex_wrap),
        justify_content: justify_content.unwrap_or(initial.justify_content),
        align_items: align_items.unwrap_or(initial.align_items),
        align_content: align_content.unwrap_or(initial.align_content),
        align_self: align_self.unwrap_or(initial.align_self),
        flex_grow: flex_grow.unwrap_or(initial.flex_grow),
        flex_shrink: flex_shrink.unwrap_or(initial.flex_shrink),
        flex_basis: resolved_flex_basis,
        row_gap: resolved_row_gap,
        column_gap: resolved_column_gap,
        transform: resolved_transform,
        transform_origin: resolved_transform_origin,
        opacity: resolved_opacity,
    };

    (style, pushed_counter_names, after_parts)
}

/// Resolve a list of `content` parts into an actual string. `counters`/`quote_depth` must be
/// passed in the state they are in after this element's own `counter-reset`/
/// `counter-increment` have been applied.
fn resolve_content_parts(
    parts: Option<Vec<ContentPart>>,
    dom: &Dom,
    element: NodeId,
    counters: &HashMap<String, Vec<i32>>,
    quote_depth: &mut i32,
    quotes: &Option<Vec<QuotePair>>,
) -> Option<String> {
    let parts = parts?;
    let mut result = String::new();
    for part in parts {
        match part {
            ContentPart::String(s) => result.push_str(&s),
            ContentPart::Attr(name) => {
                if let Some(value) = read_element_attr(dom, element, &name) {
                    result.push_str(&value);
                }
            }
            ContentPart::Counter(name, style) => {
                let value = counters
                    .get(&name)
                    .and_then(|s| s.last())
                    .copied()
                    .unwrap_or(0);
                result.push_str(&format_counter_value(style, value));
            }
            ContentPart::Counters(name, separator, style) => {
                if let Some(stack) = counters.get(&name) {
                    let formatted: Vec<String> = stack
                        .iter()
                        .map(|&v| format_counter_value(style, v))
                        .collect();
                    result.push_str(&formatted.join(&separator));
                }
            }
            ContentPart::OpenQuote => {
                result.push_str(&quote_text(quotes, *quote_depth, true));
                *quote_depth += 1;
            }
            ContentPart::CloseQuote => {
                *quote_depth = (*quote_depth - 1).max(0);
                result.push_str(&quote_text(quotes, *quote_depth, false));
            }
            ContentPart::NoOpenQuote => *quote_depth += 1,
            ContentPart::NoCloseQuote => *quote_depth = (*quote_depth - 1).max(0),
        }
    }
    Some(result)
}

/// The open and close quotes at nesting level `depth` of `quotes`. `quotes: none` or
/// unspecified always gives an empty string. Past the number of pairs given, the last pair repeats.
fn quote_text(quotes: &Option<Vec<QuotePair>>, depth: i32, is_open: bool) -> String {
    let Some(pairs) = quotes else {
        return String::new();
    };
    let Some(last_index) = pairs.len().checked_sub(1) else {
        return String::new();
    };
    let index = (depth.max(0) as usize).min(last_index);
    let pair = &pairs[index];
    if is_open {
        pair.open.clone()
    } else {
        pair.close.clone()
    }
}

/// Generate the counter representation (for `content: counter()`) from a `list-style-type`
/// value. Unlike the identically named marker generation for `list-style-type`
/// ([`crate::layout::box_tree`]), no trailing `.` is added. `disc`/`circle`/`square`/`none`
/// mean nothing as a counter representation and return an empty string (as the spec says).
pub(crate) fn format_counter_value(style: ListStyleType, n: i32) -> String {
    let n = n.max(0) as usize;
    match style {
        ListStyleType::None
        | ListStyleType::Disc
        | ListStyleType::Circle
        | ListStyleType::Square => String::new(),
        ListStyleType::Decimal => n.to_string(),
        ListStyleType::DecimalLeadingZero => format!("{n:02}"),
        ListStyleType::LowerRoman => crate::numbering::to_roman(n).to_lowercase(),
        ListStyleType::UpperRoman => crate::numbering::to_roman(n),
        ListStyleType::LowerAlpha => crate::numbering::to_alpha(n).to_lowercase(),
        ListStyleType::UpperAlpha => crate::numbering::to_alpha(n),
    }
}

/// Resolve the `content` of a margin box (`@top-left` and friends). The timing differs
/// fundamentally from `content: counter` in the body (a DOM-order counter scope, `compute_recursive`), being after pagination, so this is a separate path.
/// Only `counter(page)`/`counter(pages)` have a value (the `counters` form included, where the separator means nothing);
/// any other named counter, `attr` and quotes are always empty
pub fn resolve_margin_box_content(
    parts: &[ContentPart],
    page_number: usize,
    total_pages: Option<usize>,
) -> String {
    let mut out = String::new();
    for part in parts {
        match part {
            ContentPart::String(s) => out.push_str(s),
            ContentPart::Counter(name, style) | ContentPart::Counters(name, _, style) => {
                if let Some(n) = page_counter_value(name, page_number, total_pages) {
                    out.push_str(&format_counter_value(*style, n));
                }
            }
            ContentPart::Attr(_)
            | ContentPart::OpenQuote
            | ContentPart::CloseQuote
            | ContentPart::NoOpenQuote
            | ContentPart::NoCloseQuote => {}
        }
    }
    out
}

fn page_counter_value(name: &str, page_number: usize, total_pages: Option<usize>) -> Option<i32> {
    if name == "page" {
        Some(page_number as i32)
    } else if name == "pages" {
        total_pages.map(|n| n as i32)
    } else {
        None
    }
}

/// Read an HTML attribute value from `element` (for `content: attr(name)`).
fn read_element_attr(dom: &Dom, element: NodeId, name: &str) -> Option<String> {
    let NodeData::Element { attrs, .. } = &dom.node(element).data else {
        return None;
    };
    attrs
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.to_string())
}

/// Build a [`FirstLetterStyle`] by picking out only the supported properties from the
/// declarations that matched `::first-letter` (a lightweight implementation that does no
/// full `ComputedStyle` resolution). `own_font_size` is the basis for resolving `em`
/// (the host element's own computed font-size).
fn compute_first_letter_style(
    declarations: &[&PropertyDeclaration],
    own_font_size: f32,
    root_font_size: f32,
) -> Option<FirstLetterStyle> {
    if declarations.is_empty() {
        return None;
    }
    let mut style = FirstLetterStyle::default();
    let mut any = false;
    for decl in declarations {
        match decl {
            PropertyDeclaration::FontSize(v) => {
                style.font_size = Some(v.resolve(own_font_size, root_font_size));
                any = true;
            }
            PropertyDeclaration::FontFamily(v) => {
                style.font_family = Some(v.clone());
                any = true;
            }
            PropertyDeclaration::FontWeight(v) => {
                style.font_weight = Some(*v);
                any = true;
            }
            PropertyDeclaration::FontStyle(v) => {
                style.font_style = Some(*v);
                any = true;
            }
            // `currentcolor` gives effectively the same result as using the host element's
            // own colour, so it is not resolved explicitly and is treated as "unspecified"
            // (a known simplification).
            PropertyDeclaration::Color(Color::Rgba {
                red,
                green,
                blue,
                alpha,
            }) => {
                style.color = Some(RgbaColor {
                    red: *red,
                    green: *green,
                    blue: *blue,
                    alpha: *alpha,
                });
                any = true;
            }
            PropertyDeclaration::TextDecorationLine(v) => {
                style.text_decoration_line = Some(*v);
                any = true;
            }
            PropertyDeclaration::TextTransform(v) => {
                style.text_transform = Some(*v);
                any = true;
            }
            _ => {}
        }
    }
    any.then_some(style)
}

/// `color` is an inherited property, so the parent's computed value is used both when it is
/// unspecified and when `currentcolor` is given (which the spec makes circular, so the inherited value is used).
fn resolve_color(declared: Option<Color>, inherited: RgbaColor) -> RgbaColor {
    match declared {
        Some(Color::Rgba {
            red,
            green,
            blue,
            alpha,
        }) => RgbaColor {
            red,
            green,
            blue,
            alpha,
        },
        Some(Color::CurrentColor) | None => inherited,
    }
}

/// Parse an element's `style="..."` attribute (empty if the attribute is absent).
fn inline_style_declarations(dom: &Dom, element: NodeId) -> Vec<PropertyDeclaration> {
    let NodeData::Element { attrs, .. } = &dom.node(element).data else {
        return Vec::new();
    };
    attrs
        .iter()
        .find(|attr| &*attr.name.local == "style")
        .map(|attr| parse_inline_style(&attr.value))
        .unwrap_or_default()
}

/// Sugar for the `data-page-break="before|after|avoid"` attribute. Converted into the
/// corresponding `break-before`/`break-after`/`break-inside: avoid` declarations (the value
/// is case-insensitive). An unrecognised value is ignored (treated as no declaration, like any invalid CSS).
fn data_page_break_declarations(dom: &Dom, element: NodeId) -> Vec<PropertyDeclaration> {
    let NodeData::Element { attrs, .. } = &dom.node(element).data else {
        return Vec::new();
    };
    let Some(attr) = attrs
        .iter()
        .find(|attr| &*attr.name.local == "data-page-break")
    else {
        return Vec::new();
    };
    match attr.value.trim().to_ascii_lowercase().as_str() {
        "before" => vec![PropertyDeclaration::BreakBefore(BreakBetween::Always)],
        "after" => vec![PropertyDeclaration::BreakAfter(BreakBetween::Always)],
        "avoid" => vec![PropertyDeclaration::BreakInside(BreakInside::Avoid)],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;
    use crate::style::parse_stylesheet;

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
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
    fn inherits_color_and_font_family_through_multiple_levels() {
        let dom = html::parse(br#"<div><section><p>text</p></section></div>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { color: rgb(9, 8, 7); font-family: Georgia; }");

        let styles = compute_styles(&dom, &ua, &author);
        let p_style = &styles[&p];

        assert_eq!(
            p_style.color,
            RgbaColor {
                red: 9,
                green: 8,
                blue: 7,
                alpha: 1.0
            }
        );
        assert_eq!(p_style.font_family, vec!["Georgia".to_string()]);
    }

    #[test]
    fn reassigning_inherited_property_stops_old_value_propagation() {
        let dom = html::parse(br#"<div><section><p>text</p></section></div>"#);
        let section = find(&dom, dom.document(), "section").expect("section not found");
        let p = find(&dom, section, "p").expect("p not found");

        let ua = Stylesheet::default();
        let author =
            parse_stylesheet("div { color: rgb(9, 8, 7); } section { color: rgb(1, 2, 3); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&section].color,
            RgbaColor {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 1.0
            }
        );
        assert_eq!(
            styles[&p].color,
            RgbaColor {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn background_color_is_not_inherited() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { background-color: rgb(5, 5, 5); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].background_color,
            RgbaColor {
                red: 5,
                green: 5,
                blue: 5,
                alpha: 1.0
            }
        );
        assert_eq!(
            styles[&p].background_color,
            ComputedStyle::default().background_color
        );
    }

    #[test]
    fn background_image_is_parsed_and_not_inherited() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(r#"div { background-image: url("bg.png"); }"#);

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].background_image.as_deref(),
            Some("bg.png"),
            "background-image should be parsed and reach ComputedStyle"
        );
        assert_eq!(
            styles[&p].background_image, None,
            "background-image should not be inherited"
        );
    }

    #[test]
    fn background_image_none_overrides_an_earlier_url_in_the_cascade() {
        let dom = html::parse(br#"<div class="a b"></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(
            r#".a { background-image: url("bg.png"); } .b { background-image: none; }"#,
        );

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].background_image, None,
            "a later `background-image: none` should win the cascade over an earlier url()"
        );
    }

    #[test]
    fn background_position_keyword_pairs_are_order_independent() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { background-position: bottom right; }");
        let styles = compute_styles(&dom, &ua, &author);
        let position = styles[&div].background_position;
        assert_eq!(position.horizontal, LengthPercentage::Percentage(1.0));
        assert_eq!(position.vertical, LengthPercentage::Percentage(1.0));

        let author = parse_stylesheet("div { background-position: right bottom; }");
        let styles = compute_styles(&dom, &ua, &author);
        let position = styles[&div].background_position;
        assert_eq!(position.horizontal, LengthPercentage::Percentage(1.0));
        assert_eq!(position.vertical, LengthPercentage::Percentage(1.0));
    }

    #[test]
    fn background_position_single_keyword_centers_the_other_axis() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { background-position: top; }");
        let styles = compute_styles(&dom, &ua, &author);
        let position = styles[&div].background_position;
        assert_eq!(position.horizontal, LengthPercentage::Percentage(0.5));
        assert_eq!(position.vertical, LengthPercentage::Percentage(0.0));
    }

    #[test]
    fn background_position_mixes_keyword_and_length() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { background-position: 20px top; }");
        let styles = compute_styles(&dom, &ua, &author);
        let position = styles[&div].background_position;
        assert_eq!(position.horizontal, LengthPercentage::Length(20.0));
        assert_eq!(position.vertical, LengthPercentage::Percentage(0.0));
    }

    #[test]
    fn background_position_rejects_same_axis_keyword_pairs() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // `left right` is invalid, both being horizontal keywords -> the declaration is ignored and the initial value stands.
        let author = parse_stylesheet("div { background-position: left right; }");
        let styles = compute_styles(&dom, &ua, &author);
        let position = styles[&div].background_position;
        assert_eq!(position, BackgroundPosition::default());
    }

    #[test]
    fn background_position_default_is_top_left() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let position = styles[&div].background_position;
        assert_eq!(position.horizontal, LengthPercentage::Percentage(0.0));
        assert_eq!(position.vertical, LengthPercentage::Percentage(0.0));
    }

    #[test]
    fn background_size_keywords_and_single_value() {
        let dom =
            html::parse(br#"<div class="a"></div><div class="b"></div><div class="c"></div>"#);
        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let [a, b, c] = divs[..] else {
            panic!("expected exactly 3 divs")
        };

        let ua = Stylesheet::default();
        let author = parse_stylesheet(
            r#".a { background-size: cover; }
               .b { background-size: contain; }
               .c { background-size: 50%; }"#,
        );
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&a].background_size, BackgroundSize::Cover);
        assert_eq!(styles[&b].background_size, BackgroundSize::Contain);
        assert_eq!(
            styles[&c].background_size,
            BackgroundSize::WidthHeight(
                LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Percentage(0.5)),
                LengthPercentageOrAuto::Auto
            )
        );
    }

    #[test]
    fn object_fit_keywords_are_parsed_and_default_is_fill() {
        let dom = html::parse(
            br#"<img class="a"><img class="b"><img class="c"><img class="d"><img class="e"><img class="f">"#,
        );
        let mut imgs = Vec::new();
        find_all(&dom, dom.document(), "img", &mut imgs);
        let [a, b, c, d, e, f] = imgs[..] else {
            panic!("expected exactly 6 imgs")
        };

        let ua = Stylesheet::default();
        let author = parse_stylesheet(
            r#".a { object-fit: fill; }
               .b { object-fit: contain; }
               .c { object-fit: cover; }
               .d { object-fit: none; }
               .e { object-fit: scale-down; }"#,
        );
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&a].object_fit, ObjectFit::Fill);
        assert_eq!(styles[&b].object_fit, ObjectFit::Contain);
        assert_eq!(styles[&c].object_fit, ObjectFit::Cover);
        assert_eq!(styles[&d].object_fit, ObjectFit::None);
        assert_eq!(styles[&e].object_fit, ObjectFit::ScaleDown);
        // The initial value when unspecified is `fill`.
        assert_eq!(styles[&f].object_fit, ObjectFit::Fill);
    }

    #[test]
    fn object_position_default_is_50_percent_and_can_be_overridden() {
        let dom = html::parse(br#"<img class="a"><img class="b">"#);
        let mut imgs = Vec::new();
        find_all(&dom, dom.document(), "img", &mut imgs);
        let [a, b] = imgs[..] else {
            panic!("expected exactly 2 imgs")
        };

        let ua = Stylesheet::default();
        let author = parse_stylesheet(".b { object-position: right bottom; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&a].object_position,
            BackgroundPosition {
                horizontal: LengthPercentage::Percentage(0.5),
                vertical: LengthPercentage::Percentage(0.5),
            }
        );
        assert_eq!(
            styles[&b].object_position,
            BackgroundPosition {
                horizontal: LengthPercentage::Percentage(1.0),
                vertical: LengthPercentage::Percentage(1.0),
            }
        );
    }

    #[test]
    fn box_shadow_defaults_to_empty_and_parses_offsets_blur_spread() {
        let dom = html::parse(br#"<div class="a"></div><div class="b"></div>"#);
        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let [a, b] = divs[..] else {
            panic!("expected exactly 2 divs")
        };

        let ua = Stylesheet::default();
        let author = parse_stylesheet(".b { box-shadow: 2px 3px 4px 5px rgb(10, 20, 30); }");
        let styles = compute_styles(&dom, &ua, &author);
        assert!(styles[&a].box_shadow.is_empty());

        let shadows = &styles[&b].box_shadow;
        assert_eq!(shadows.len(), 1);
        let shadow = shadows[0];
        assert_eq!(shadow.offset_x, 2.0);
        assert_eq!(shadow.offset_y, 3.0);
        assert_eq!(shadow.blur_radius, 4.0);
        assert_eq!(shadow.spread_radius, 5.0);
        assert_eq!(
            shadow.color,
            RgbaColor {
                red: 10,
                green: 20,
                blue: 30,
                alpha: 1.0
            }
        );
        assert!(!shadow.inset);
    }

    #[test]
    fn box_shadow_supports_comma_separated_list_inset_and_currentcolor() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(
            "div { color: rgb(9, 8, 7); \
             box-shadow: 1px 1px, inset 2px 2px 3px rgb(1,1,1); }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let shadows = &styles[&div].box_shadow;
        assert_eq!(shadows.len(), 2);
        // First: with the colour omitted it resolves to `currentcolor` (this element's computed `color`).
        assert_eq!(
            shadows[0].color,
            RgbaColor {
                red: 9,
                green: 8,
                blue: 7,
                alpha: 1.0
            }
        );
        assert_eq!(shadows[0].blur_radius, 0.0);
        assert!(!shadows[0].inset);
        // Second: `inset` parses (drawing it is not supported).
        assert!(shadows[1].inset);
    }

    #[test]
    fn box_shadow_none_clears_the_shorthand() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { box-shadow: none; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert!(styles[&div].box_shadow.is_empty());
    }

    #[test]
    fn background_repeat_and_attachment_are_parsed_and_not_inherited() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let ua = Stylesheet::default();
        let author =
            parse_stylesheet("div { background-repeat: repeat-x; background-attachment: fixed; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].background_repeat, BackgroundRepeat::RepeatX);
        assert_eq!(
            styles[&div].background_attachment,
            BackgroundAttachment::Fixed
        );
        assert_eq!(styles[&p].background_repeat, BackgroundRepeat::Repeat);
        assert_eq!(
            styles[&p].background_attachment,
            BackgroundAttachment::Scroll
        );
    }

    #[test]
    fn background_shorthand_resets_unspecified_longhands_to_initial_values() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // After setting the background image and repeat via the individual properties, the
        // `background` shorthand gives only a colour -> per the spec the rest reset to their initial values.
        let author = parse_stylesheet(
            r#"div { background-image: url("bg.png"); background-repeat: no-repeat; }
               div { background: red; }"#,
        );
        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(
            style.background_color,
            RgbaColor {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 1.0
            }
        );
        assert_eq!(
            style.background_image, None,
            "background shorthand should reset background-image to none"
        );
        assert_eq!(
            style.background_repeat,
            BackgroundRepeat::Repeat,
            "background shorthand should reset background-repeat to its initial value"
        );
    }

    #[test]
    fn background_shorthand_parses_position_and_size_with_slash() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author =
            parse_stylesheet(r#"div { background: url("bg.png") no-repeat center / cover; }"#);
        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(style.background_image.as_deref(), Some("bg.png"));
        assert_eq!(style.background_repeat, BackgroundRepeat::NoRepeat);
        assert_eq!(
            style.background_position.horizontal,
            LengthPercentage::Percentage(0.5)
        );
        assert_eq!(style.background_size, BackgroundSize::Cover);
    }

    #[test]
    fn hsl_color_function_resolves_to_expected_rgb() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // Pure red (hue=0, saturation=100%, lightness=50%) = rgb(255, 0, 0).
        let author = parse_stylesheet("div { color: hsl(0deg 100% 50%); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].color,
            RgbaColor {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn hwb_color_function_resolves_to_expected_rgb() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // 100% white -> pure white, rgb(255, 255, 255).
        let author = parse_stylesheet("div { color: hwb(0deg 100% 0%); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].color,
            RgbaColor {
                red: 255,
                green: 255,
                blue: 255,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn hsl_color_function_with_alpha_is_preserved() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { background-color: hsl(0deg 0% 0% / 50%); }");

        let styles = compute_styles(&dom, &ua, &author);
        let bg = styles[&div].background_color;
        assert_eq!((bg.red, bg.green, bg.blue), (0, 0, 0));
        assert!((bg.alpha - 0.5).abs() < 0.01);
    }

    #[test]
    fn lab_color_function_resolves_to_expected_rgb() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // lab(53.2408% 80.0925 67.2032) is pure red, rgb(255, 0, 0).
        let author = parse_stylesheet("div { color: lab(53.2408% 80.0925 67.2032); }");

        let styles = compute_styles(&dom, &ua, &author);
        let color = styles[&div].color;
        assert_eq!(color.red, 255);
        assert!(color.green <= 1);
        assert_eq!(color.blue, 0);
    }

    #[test]
    fn lch_color_function_resolves_to_expected_rgb() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // lch(53.2408% 104.5518 39.999deg) is pure red, rgb(255, 0, 0).
        let author = parse_stylesheet("div { color: lch(53.2408% 104.5518 39.999deg); }");

        let styles = compute_styles(&dom, &ua, &author);
        let color = styles[&div].color;
        assert_eq!(color.red, 255);
        assert!(color.green <= 1);
        assert_eq!(color.blue, 0);
    }

    #[test]
    fn oklab_color_function_resolves_to_expected_rgb() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // oklab(62.8% 0.2249 0.1258) is pure red, rgb(255, 0, 0).
        let author = parse_stylesheet("div { color: oklab(62.8% 0.2249 0.1258); }");

        let styles = compute_styles(&dom, &ua, &author);
        let color = styles[&div].color;
        assert_eq!(color.red, 255);
        assert!(color.green <= 1);
        assert_eq!(color.blue, 0);
    }

    #[test]
    fn oklch_color_function_with_alpha_is_preserved() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // oklch(59.686% 0.15619 49.7694deg) is #ba5d06, that is rgb(198, 93, 6).
        let author =
            parse_stylesheet("div { background-color: oklch(59.686% 0.15619 49.7694deg / 50%); }");

        let styles = compute_styles(&dom, &ua, &author);
        let bg = styles[&div].background_color;
        assert_eq!((bg.red, bg.green, bg.blue), (198, 93, 6));
        assert!((bg.alpha - 0.5).abs() < 0.01);
    }

    #[test]
    fn current_color_background_resolves_to_own_computed_color() {
        let dom = html::parse(br#"<div>text</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author =
            parse_stylesheet("div { color: rgb(4, 5, 6); background-color: currentcolor; }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].background_color,
            RgbaColor {
                red: 4,
                green: 5,
                blue: 6,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn root_without_declarations_gets_initial_values() {
        let dom = html::parse(br#"<div>text</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = Stylesheet::default();

        let styles = compute_styles(&dom, &ua, &author);
        let default = ComputedStyle::default();
        assert_eq!(styles[&div].color, default.color);
        assert_eq!(styles[&div].font_size, default.font_size);
        assert_eq!(styles[&div].font_family, default.font_family);
    }

    #[test]
    fn non_element_nodes_inherit_parent_style_directly() {
        let dom = html::parse(br#"<p>hello</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");
        let text = dom.children(p).next().expect("text node not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("p { color: rgb(7, 7, 7); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&text], styles[&p]);
    }

    #[test]
    fn inline_style_overrides_stylesheet_rules_regardless_of_specificity() {
        // An #id selector normally outranks any class or type selector in specificity, but
        // an inline style should outrank even that.
        let dom = html::parse(br#"<div id="x" style="color: rgb(9, 9, 9);">t</div>"#);
        let p = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("#x { color: rgb(1, 1, 1); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&p].color,
            RgbaColor {
                red: 9,
                green: 9,
                blue: 9,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn inline_style_applies_when_there_is_no_matching_rule() {
        let dom = html::parse(br#"<div style="background-color: rgb(4, 5, 6);">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = Stylesheet::default();

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].background_color,
            RgbaColor {
                red: 4,
                green: 5,
                blue: 6,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn font_weight_and_style_are_inherited_but_overridable() {
        let dom = html::parse(br#"<p><b>bold <i>bold-italic</i></b></p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");
        let b = find(&dom, p, "b").expect("b not found");
        let i = find(&dom, b, "i").expect("i not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("b { font-weight: bold; } i { font-style: italic; }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&p].font_weight, super::FontWeight::Normal);
        assert_eq!(styles[&b].font_weight, super::FontWeight::Bold);
        assert_eq!(styles[&b].font_style, super::FontStyle::Normal);
        // <i> inherits font-weight: bold from <b> and adds its own font-style: italic.
        assert_eq!(styles[&i].font_weight, super::FontWeight::Bold);
        assert_eq!(styles[&i].font_style, super::FontStyle::Italic);
    }

    #[test]
    fn text_decoration_line_parses_underline_and_line_through() {
        let dom = html::parse(br#"<p>a</p>"#);

        let underline = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { text-decoration: underline; }"),
        );
        let p = find(&dom, dom.document(), "p").expect("p not found");
        assert!(underline[&p].text_decoration_line.underline);
        assert!(!underline[&p].text_decoration_line.line_through);

        let line_through = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { text-decoration-line: line-through; }"),
        );
        assert!(line_through[&p].text_decoration_line.line_through);
        assert!(!line_through[&p].text_decoration_line.underline);

        let both = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { text-decoration: underline line-through; }"),
        );
        assert!(both[&p].text_decoration_line.underline);
        assert!(both[&p].text_decoration_line.line_through);
    }

    #[test]
    fn text_decoration_line_propagates_to_descendants_like_font_weight() {
        // The spec makes it non-inherited, but instead of the special rule propagating an
        // ancestor's decoration to descendants, this repository treats it as inherited (see the comment in computed.rs).
        let dom = html::parse(br#"<u>bold <b>text</b></u>"#);
        let u = find(&dom, dom.document(), "u").expect("u not found");
        let b = find(&dom, u, "b").expect("b not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("u { text-decoration: underline; }"),
        );
        assert!(styles[&u].text_decoration_line.underline);
        assert!(styles[&b].text_decoration_line.underline);
    }

    #[test]
    fn ua_stylesheet_gives_u_and_s_their_default_text_decoration() {
        use super::super::ua::user_agent_stylesheet;

        let dom = html::parse(br#"<p><u>underlined</u> <s>struck</s></p>"#);
        let u = find(&dom, dom.document(), "u").expect("u not found");
        let s = find(&dom, dom.document(), "s").expect("s not found");

        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        assert!(styles[&u].text_decoration_line.underline);
        assert!(styles[&s].text_decoration_line.line_through);
    }

    #[test]
    fn ua_stylesheet_gives_pre_its_default_white_space() {
        use super::super::ua::user_agent_stylesheet;

        let dom = html::parse(br#"<pre>  a   b  </pre>"#);
        let pre = find(&dom, dom.document(), "pre").expect("pre not found");

        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        assert_eq!(styles[&pre].white_space, super::WhiteSpace::Pre);
    }

    #[test]
    fn text_decoration_none_overrides_inherited_underline() {
        let dom = html::parse(br#"<u><span class="plain">text</span></u>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let ua = Stylesheet::default();
        let author =
            parse_stylesheet("u { text-decoration: underline; } .plain { text-decoration: none; }");

        let styles = compute_styles(&dom, &ua, &author);
        assert!(!styles[&span].text_decoration_line.underline);
    }

    #[test]
    fn numeric_font_weight_is_thresholded_to_bold_or_normal() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let light = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { font-weight: 400; }"),
        );
        assert_eq!(light[&p].font_weight, super::FontWeight::Normal);

        let heavy = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { font-weight: 700; }"),
        );
        assert_eq!(heavy[&p].font_weight, super::FontWeight::Bold);
    }

    #[test]
    fn elements_without_style_attribute_are_unaffected() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = Stylesheet::default();

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(*styles[&div], ComputedStyle::default());
    }

    #[test]
    fn border_shorthand_sets_width_style_and_color_on_all_sides() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { border: 2px dashed rgb(10, 20, 30); }");

        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(style.border_top_width.0, 2.0);
        assert_eq!(style.border_right_width.0, 2.0);
        assert_eq!(style.border_bottom_width.0, 2.0);
        assert_eq!(style.border_left_width.0, 2.0);
        assert_eq!(style.border_top_style, super::BorderStyle::Dashed);
        assert_eq!(
            style.border_top_color,
            RgbaColor {
                red: 10,
                green: 20,
                blue: 30,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn border_top_width_longhand_sets_only_the_top_edge() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { border-top-width: 5px; }");

        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(style.border_top_width.0, 5.0);
        assert_eq!(style.border_right_width.0, 0.0);
        assert_eq!(style.border_bottom_width.0, 0.0);
        assert_eq!(style.border_left_width.0, 0.0);
    }

    #[test]
    fn border_edge_longhands_set_width_style_and_color_independently() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(
            "div { border-bottom-width: 3px; border-bottom-style: dotted; \
             border-bottom-color: rgb(1, 2, 3); }",
        );

        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(style.border_bottom_width.0, 3.0);
        assert_eq!(style.border_bottom_style, super::BorderStyle::Dotted);
        assert_eq!(
            style.border_bottom_color,
            RgbaColor {
                red: 1,
                green: 2,
                blue: 3,
                alpha: 1.0
            }
        );
        // The other edges are unaffected.
        assert_eq!(style.border_top_width.0, 0.0);
        assert_eq!(style.border_top_style, super::BorderStyle::None);
    }

    #[test]
    fn border_edge_shorthand_sets_width_style_and_color_on_one_side() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { border-left: 4px solid rgb(5, 6, 7); }");

        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(style.border_left_width.0, 4.0);
        assert_eq!(style.border_left_style, super::BorderStyle::Solid);
        assert_eq!(
            style.border_left_color,
            RgbaColor {
                red: 5,
                green: 6,
                blue: 7,
                alpha: 1.0
            }
        );
        // The other edges are unaffected (they keep their initial values).
        assert_eq!(style.border_right_width.0, 0.0);
        assert_eq!(style.border_right_style, super::BorderStyle::None);
    }

    #[test]
    fn opacity_parses_and_clamps_out_of_range_values() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { opacity: 0.5; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].opacity, 0.5);

        let author = parse_stylesheet("div { opacity: 2; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].opacity, 1.0);

        let author = parse_stylesheet("div { opacity: -1; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].opacity, 0.0);
    }

    #[test]
    fn opacity_defaults_to_one_and_is_not_inherited() {
        let dom = html::parse(br#"<div><p>t</p></div>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { opacity: 0.3; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&p].opacity, 1.0);
    }

    #[test]
    fn transform_parses_multiple_functions_and_none_resets_it() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { transform: translateX(10px) scale(2); }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].transform.len(), 2);

        let author = parse_stylesheet("div { transform: none; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert!(styles[&div].transform.is_empty());
    }

    #[test]
    fn transform_origin_defaults_to_50_percent_and_can_be_overridden() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].transform_origin.horizontal,
            LengthPercentage::Percentage(0.5)
        );
        assert_eq!(
            styles[&div].transform_origin.vertical,
            LengthPercentage::Percentage(0.5)
        );

        let author = parse_stylesheet("div { transform-origin: left top; }");
        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&div].transform_origin.horizontal,
            LengthPercentage::Percentage(0.0)
        );
        assert_eq!(
            styles[&div].transform_origin.vertical,
            LengthPercentage::Percentage(0.0)
        );
    }

    #[test]
    fn border_color_defaults_to_currentcolor_when_unspecified() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { color: rgb(9, 9, 9); border: 1px solid; }");

        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(
            style.border_top_color,
            RgbaColor {
                red: 9,
                green: 9,
                blue: 9,
                alpha: 1.0
            },
            "border-color should follow currentcolor when not explicitly set"
        );
    }

    #[test]
    fn border_color_and_border_style_shorthands_expand_per_side() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(
            "div { border-style: solid dotted; border-color: rgb(1,1,1) rgb(2,2,2); }",
        );

        let styles = compute_styles(&dom, &ua, &author);
        let style = &styles[&div];
        assert_eq!(style.border_top_style, super::BorderStyle::Solid);
        assert_eq!(style.border_right_style, super::BorderStyle::Dotted);
        assert_eq!(style.border_bottom_style, super::BorderStyle::Solid);
        assert_eq!(style.border_left_style, super::BorderStyle::Dotted);
        assert_eq!(
            style.border_top_color,
            RgbaColor {
                red: 1,
                green: 1,
                blue: 1,
                alpha: 1.0
            }
        );
        assert_eq!(
            style.border_right_color,
            RgbaColor {
                red: 2,
                green: 2,
                blue: 2,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn em_font_size_resolves_against_parent_font_size() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let ua = Stylesheet::default();
        // div: 20px; p: 1.5x div = 30px.
        let author = parse_stylesheet("div { font-size: 20px; } p { font-size: 1.5em; }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].font_size.0, 20.0);
        assert_eq!(styles[&p].font_size.0, 30.0);
    }

    #[test]
    fn em_length_on_non_font_size_property_uses_own_font_size() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = Stylesheet::default();
        // font-size resolves to 20px first, and border-width's 2em is relative to that = 40px.
        let author = parse_stylesheet("div { font-size: 20px; border: 2em solid black; }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&div].border_top_width.0, 40.0);
    }

    #[test]
    fn rem_length_resolves_against_root_element_font_size_regardless_of_nesting() {
        let dom = html::parse(br#"<html><body><div><p>text</p></div></body></html>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let ua = Stylesheet::default();
        // Set the root (<html>) font-size to 10px and check that a nested p's margin: 2rem
        // is always 20px, unaffected by the parent's (div/body) font-size.
        let author = parse_stylesheet(
            "html { font-size: 10px; } div { font-size: 30px; } p { margin: 2rem; }",
        );

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(
            styles[&p].margin_top,
            LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(20.0))
        );
    }

    #[test]
    fn border_is_not_inherited() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet("div { border: 3px solid rgb(1, 2, 3); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&p].border_top_style, super::BorderStyle::None);
        assert_eq!(styles[&p].border_top_width.0, 0.0);
    }

    #[test]
    fn break_before_and_break_after_default_to_auto() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&p].break_before, BreakBetween::Auto);
        assert_eq!(styles[&p].break_after, BreakBetween::Auto);
        assert_eq!(styles[&p].break_inside, BreakInside::Auto);
    }

    #[test]
    fn break_before_and_break_after_parse_avoid_and_always() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { break-before: avoid; break-after: always; }"),
        );
        assert_eq!(styles[&p].break_before, BreakBetween::Avoid);
        assert_eq!(styles[&p].break_after, BreakBetween::Always);
    }

    #[test]
    fn break_before_page_keyword_is_treated_as_always() {
        // Only a single page size is handled, so `page` is treated as having the same effect as `always`.
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { break-before: page; }"),
        );
        assert_eq!(styles[&p].break_before, BreakBetween::Always);
    }

    #[test]
    fn break_inside_parses_avoid() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { break-inside: avoid; }"),
        );
        assert_eq!(styles[&p].break_inside, BreakInside::Avoid);
    }

    #[test]
    fn legacy_page_break_properties_are_aliases_for_break_properties() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "p { page-break-before: always; page-break-after: avoid; \
                 page-break-inside: avoid; }",
            ),
        );
        assert_eq!(styles[&p].break_before, BreakBetween::Always);
        assert_eq!(styles[&p].break_after, BreakBetween::Avoid);
        assert_eq!(styles[&p].break_inside, BreakInside::Avoid);
    }

    #[test]
    fn orphans_and_widows_default_to_two_and_can_be_overridden() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&p].orphans, 2);
        assert_eq!(defaults[&p].widows, 2);

        let overridden = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { orphans: 3; widows: 4; }"),
        );
        assert_eq!(overridden[&p].orphans, 3);
        assert_eq!(overridden[&p].widows, 4);
    }

    #[test]
    fn orphans_rejects_non_positive_values() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        // An invalid value makes the whole declaration ignored, leaving the initial value.
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { orphans: 0; }"),
        );
        assert_eq!(styles[&p].orphans, 2);
    }

    #[test]
    fn break_properties_are_not_inherited() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { break-before: always; orphans: 5; }"),
        );
        assert_eq!(styles[&div].break_before, BreakBetween::Always);
        assert_eq!(styles[&div].orphans, 5);
        assert_eq!(styles[&p].break_before, BreakBetween::Auto);
        assert_eq!(styles[&p].orphans, 2);
    }

    #[test]
    fn data_page_break_attribute_maps_to_break_properties() {
        let dom = html::parse(
            br#"<div><p id="a" data-page-break="before">a</p>
                <p id="b" data-page-break="after">b</p>
                <p id="c" data-page-break="avoid">c</p></div>"#,
        );
        let a = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&a].break_before, BreakBetween::Always);

        let mut ps = Vec::new();
        fn find_all(dom: &Dom, id: NodeId, out: &mut Vec<NodeId>) {
            if let NodeData::Element { name, .. } = &dom.node(id).data {
                if &*name.local == "p" {
                    out.push(id);
                }
            }
            for child in dom.children(id) {
                find_all(dom, child, out);
            }
        }
        find_all(&dom, dom.document(), &mut ps);
        assert_eq!(styles[&ps[1]].break_after, BreakBetween::Always);
        assert_eq!(styles[&ps[2]].break_inside, BreakInside::Avoid);
    }

    #[test]
    fn data_page_break_ignores_unrecognized_values() {
        let dom = html::parse(br#"<p data-page-break="sideways">a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&p].break_before, BreakBetween::Auto);
        assert_eq!(styles[&p].break_after, BreakBetween::Auto);
        assert_eq!(styles[&p].break_inside, BreakInside::Auto);
    }

    #[test]
    fn stylesheet_rule_overrides_data_page_break_attribute() {
        // The attribute sugar counts as "a default hint that a stylesheet can override
        // individually", so an ordinary CSS rule takes priority.
        let dom = html::parse(br#"<p data-page-break="before">a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { break-before: auto; }"),
        );
        assert_eq!(styles[&p].break_before, BreakBetween::Auto);
    }

    #[test]
    fn inline_style_overrides_data_page_break_attribute() {
        let dom = html::parse(br#"<p data-page-break="before" style="break-before: auto;">a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&p].break_before, BreakBetween::Auto);
    }

    #[test]
    fn before_and_after_pseudo_content_resolve_from_matching_rules() {
        let dom = html::parse(br#"<span class="badge">Text</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let ua = Stylesheet::default();
        let author =
            parse_stylesheet(r#".badge::before { content: "["; } .badge::after { content: "]"; }"#);

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&span].pseudo_before_content.as_deref(), Some("["));
        assert_eq!(styles[&span].pseudo_after_content.as_deref(), Some("]"));
    }

    #[test]
    fn pseudo_content_is_none_without_a_matching_before_after_rule() {
        let dom = html::parse(br#"<span class="badge">Text</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(".badge { color: rgb(1, 2, 3); }");

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&span].pseudo_before_content, None);
        assert_eq!(styles[&span].pseudo_after_content, None);
    }

    #[test]
    fn explicit_content_none_wins_over_an_earlier_lower_specificity_rule() {
        let dom = html::parse(br#"<span id="x" class="badge">Text</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let ua = Stylesheet::default();
        // Even with a string given by a class selector, a more specific #id selector coming
        // later with `content: none` should remove the generated box.
        let author =
            parse_stylesheet(r#".badge::before { content: "x"; } #x::before { content: none; }"#);

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&span].pseudo_before_content, None);
    }

    #[test]
    fn float_left_and_right_parse_and_are_not_inherited() {
        let dom = html::parse(br#"<div><img></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let img = find(&dom, div, "img").expect("img not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { float: left; } img { float: right; }"),
        );
        assert_eq!(styles[&div].float, super::Float::Left);
        assert_eq!(styles[&img].float, super::Float::Right);
    }

    #[test]
    fn float_forces_inline_display_to_block() {
        let dom = html::parse(br#"<span>text</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("span { float: left; }"),
        );
        assert_eq!(
            styles[&span].display,
            Display::Block,
            "CSS2.1 9.7: an element with a float automatically becomes block-level"
        );
    }

    #[test]
    fn float_none_does_not_affect_display() {
        let dom = html::parse(br#"<span>text</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&span].display, Display::Inline);
    }

    #[test]
    fn clear_parses_all_keywords() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for (value, expected) in [
            ("left", super::Clear::Left),
            ("right", super::Clear::Right),
            ("both", super::Clear::Both),
            ("none", super::Clear::None),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ clear: {value}; }}")),
            );
            assert_eq!(styles[&div].clear, expected, "clear: {value}");
        }
    }

    #[test]
    fn position_relative_parses_with_offsets() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { position: relative; top: 5px; left: 10px; }"),
        );
        assert_eq!(styles[&div].position, super::Position::Relative);
        assert_eq!(
            styles[&div].top,
            LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(5.0))
        );
        assert_eq!(
            styles[&div].left,
            LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(10.0))
        );
    }

    #[test]
    fn calc_mixes_percentage_and_pixels() {
        use crate::style::LengthPercentage;
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { width: calc(100% - 40px); }"),
        );
        match styles[&div].width {
            super::LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc {
                px,
                percent,
            }) => {
                assert_eq!(px, -40.0);
                assert_eq!(percent, 1.0);
            }
            other => panic!("expected a calc value, got {other:?}"),
        }
    }

    #[test]
    fn calc_resolves_em_using_the_element_font_size() {
        use crate::style::LengthPercentage;
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { font-size: 20px; margin-left: calc(2em + 5px); }"),
        );
        // 2em (= 40px) + 5px = 45px, with no percentage component.
        match styles[&div].margin_left {
            super::LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc {
                px,
                percent,
            }) => {
                assert_eq!(px, 45.0);
                assert_eq!(percent, 0.0);
            }
            other => panic!("expected a calc value, got {other:?}"),
        }
    }

    #[test]
    fn calc_supports_multiplication_and_division() {
        use crate::style::LengthPercentage;
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { width: calc((100% - 20px) / 2 + 3px * 2); }"),
        );
        // (100% - 20px)/2 = 50% - 10px, plus 6px = 50% - 4px.
        match styles[&div].width {
            super::LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc {
                px,
                percent,
            }) => {
                assert!((px - (-4.0)).abs() < 0.001, "px={px}");
                assert!((percent - 0.5).abs() < 0.001, "percent={percent}");
            }
            other => panic!("expected a calc value, got {other:?}"),
        }
    }

    #[test]
    fn calc_with_a_bare_number_or_dimension_product_is_rejected() {
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        // `calc(2)` leaves a bare number and `calc(2px * 3px)` is dimension times dimension; both invalid.
        for css in ["div { width: calc(2); }", "div { width: calc(2px * 3px); }"] {
            let styles = compute_styles(&dom, &Stylesheet::default(), &parse_stylesheet(css));
            assert_eq!(
                styles[&div].width,
                super::LengthPercentageOrAuto::Auto,
                "invalid calc should be dropped, leaving the initial value: {css}"
            );
        }
    }

    #[test]
    fn calc_accepts_a_nested_calc_as_a_term() {
        // CSS Values 4: a `calc()` may be a term of a `calc()` (equivalent to parentheses).
        // Tailwind v4's `space-y-*`/`divide-*` emit this form.
        use crate::style::LengthPercentage;
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        for css in [
            // Both terms
            "div { margin-left: calc(calc(45px * 2) * calc(1 - 0)); }",
            // Left term only
            "div { margin-left: calc(calc(45px * 2) * 1); }",
            // Right term only
            "div { margin-left: calc(90px * calc(1 - 0)); }",
            // Addition
            "div { margin-left: calc(calc(45px) + calc(45px)); }",
            // A redundant single level of nesting
            "div { margin-left: calc(calc(90px)); }",
            // Two levels of nesting
            "div { margin-left: calc(calc(calc(30px) * 3)); }",
            // Mixed with parentheses
            "div { margin-left: calc((calc(45px) + 45px) * calc(2 / 2)); }",
        ] {
            let styles = compute_styles(&dom, &Stylesheet::default(), &parse_stylesheet(css));
            match styles[&div].margin_left {
                super::LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc {
                    px,
                    percent,
                }) => {
                    assert!((px - 90.0).abs() < 0.001, "px={px} for {css}");
                    assert_eq!(percent, 0.0, "{css}");
                }
                other => panic!("expected a 90px calc value for {css}, got {other:?}"),
            }
        }
    }

    #[test]
    fn nested_calc_overrides_an_earlier_declaration_in_the_cascade() {
        // If it were dropped as an invalid declaration, the preceding `margin-left: 20px`
        // would survive (case 4 of issue #17). It is valid, so later-wins gives 90px.
        use crate::style::LengthPercentage;
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { margin-left: 20px; margin-left: calc(calc(45px * 2) * 1); }"),
        );
        match styles[&div].margin_left {
            super::LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc {
                px, ..
            }) => assert!((px - 90.0).abs() < 0.001, "px={px}"),
            other => panic!("expected a 90px calc value, got {other:?}"),
        }
    }

    #[test]
    fn nested_calc_still_rejects_invalid_expressions() {
        // Type checking survives nesting: a bare number, dimension times dimension, and an unknown function are all invalid.
        let dom = html::parse(br#"<div>x</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        for css in [
            "div { width: calc(calc(2)); }",
            "div { width: calc(calc(2px) * calc(3px)); }",
            "div { width: calc(calc(2px * 3px)); }",
            "div { width: calc(foo(2px)); }",
        ] {
            let styles = compute_styles(&dom, &Stylesheet::default(), &parse_stylesheet(css));
            assert_eq!(
                styles[&div].width,
                super::LengthPercentageOrAuto::Auto,
                "invalid calc should be dropped, leaving the initial value: {css}"
            );
        }
    }

    #[test]
    fn absolute_and_fixed_are_block_level() {
        // CSS2.1 9.7: absolute/fixed make display block-level, which brings inline elements
        // (span) into absolute positioning too.
        let dom = html::parse(br#"<span>x</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");
        for value in ["absolute", "fixed"] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("span {{ position: {value}; }}")),
            );
            assert_eq!(
                styles[&span].display,
                super::Display::Block,
                "position: {value} should block-ify an inline element"
            );
        }
    }

    #[test]
    fn position_absolute_and_fixed_parse() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for (value, expected) in [
            ("absolute", super::Position::Absolute),
            ("fixed", super::Position::Fixed),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ position: {value}; }}")),
            );
            assert_eq!(styles[&div].position, expected);
        }
    }

    #[test]
    fn top_right_bottom_left_default_to_auto() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&div].top, LengthPercentageOrAuto::Auto);
        assert_eq!(styles[&div].right, LengthPercentageOrAuto::Auto);
        assert_eq!(styles[&div].bottom, LengthPercentageOrAuto::Auto);
        assert_eq!(styles[&div].left, LengthPercentageOrAuto::Auto);
    }

    #[test]
    fn typography_properties_parse_and_are_inherited() {
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { text-align: center; white-space: nowrap; \
                 letter-spacing: 2px; word-spacing: 3px; text-transform: uppercase; }",
            ),
        );
        for id in [div, p] {
            assert_eq!(styles[&id].text_align, super::TextAlign::Center);
            assert_eq!(styles[&id].white_space, super::WhiteSpace::Nowrap);
            assert_eq!(styles[&id].letter_spacing, 2.0);
            assert_eq!(styles[&id].word_spacing, 3.0);
            assert_eq!(styles[&id].text_transform, super::TextTransform::Uppercase);
        }
    }

    #[test]
    fn text_align_parses_all_keywords() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for (value, expected) in [
            ("left", super::TextAlign::Left),
            ("right", super::TextAlign::Right),
            ("center", super::TextAlign::Center),
            ("justify", super::TextAlign::Justify),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ text-align: {value}; }}")),
            );
            assert_eq!(styles[&div].text_align, expected, "text-align: {value}");
        }
    }

    #[test]
    fn line_height_number_and_percentage_are_inherited_unmultiplied() {
        // CSS2.1 10.8.1: the computed value of a <number>/<percentage> is the specified value
        // itself (not an absolute value pre-multiplied by the parent's font-size). Even with a
        // child at a different font-size, the inherited `LineHeight::Number` value should not change.
        let dom = html::parse(br#"<div><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { line-height: 1.5; } p { font-size: 30px; }"),
        );
        assert_eq!(styles[&div].line_height, super::LineHeight::Number(1.5));
        assert_eq!(
            styles[&p].line_height,
            super::LineHeight::Number(1.5),
            "line-height: <number> should be inherited unmultiplied"
        );

        let percentage_styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { line-height: 150%; }"),
        );
        assert_eq!(
            percentage_styles[&div].line_height,
            super::LineHeight::Number(1.5),
            "150% should normalize to the same representation as <number> 1.5"
        );
    }

    #[test]
    fn line_height_length_resolves_to_absolute_px() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { line-height: 24px; }"),
        );
        assert_eq!(styles[&div].line_height, super::LineHeight::Length(24.0));
    }

    #[test]
    fn line_height_defaults_to_normal() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&p].line_height, super::LineHeight::Normal);
    }

    #[test]
    fn text_indent_percentage_stays_as_a_fraction_until_used() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { text-indent: 10%; }"),
        );
        assert_eq!(
            styles[&p].text_indent,
            LengthPercentage::Percentage(0.1),
            "text-indent percentage should remain unresolved (fraction) at computed-value time"
        );
    }

    #[test]
    fn text_indent_length_and_inheritance() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { text-indent: 20px; }"),
        );
        assert_eq!(styles[&div].text_indent, LengthPercentage::Length(20.0));
        assert_eq!(
            styles[&p].text_indent,
            LengthPercentage::Length(20.0),
            "text-indent should be inherited"
        );
    }

    #[test]
    fn white_space_parses_all_keywords() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for (value, expected) in [
            ("normal", super::WhiteSpace::Normal),
            ("nowrap", super::WhiteSpace::Nowrap),
            ("pre", super::WhiteSpace::Pre),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ white-space: {value}; }}")),
            );
            assert_eq!(styles[&div].white_space, expected, "white-space: {value}");
        }
    }

    #[test]
    fn letter_spacing_and_word_spacing_default_to_zero_and_resolve_em() {
        let dom = html::parse(br#"<p>a</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&p].letter_spacing, 0.0);
        assert_eq!(defaults[&p].word_spacing, 0.0);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("p { font-size: 20px; letter-spacing: 0.5em; }"),
        );
        assert_eq!(styles[&p].letter_spacing, 10.0);
    }

    #[test]
    fn text_transform_parses_all_keywords() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for (value, expected) in [
            ("none", super::TextTransform::None),
            ("uppercase", super::TextTransform::Uppercase),
            ("lowercase", super::TextTransform::Lowercase),
            ("capitalize", super::TextTransform::Capitalize),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ text-transform: {value}; }}")),
            );
            assert_eq!(
                styles[&div].text_transform, expected,
                "text-transform: {value}"
            );
        }
    }

    #[test]
    fn table_layout_properties_parse_and_have_correct_inheritance() {
        let dom = html::parse(br#"<table><tr><td>a</td></tr></table>"#);
        let table = find(&dom, dom.document(), "table").expect("table not found");
        let td = find(&dom, table, "td").expect("td not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "table { border-collapse: collapse; border-spacing: 3px 5px; \
                 caption-side: bottom; empty-cells: hide; table-layout: fixed; \
                 vertical-align: middle; }",
            ),
        );
        let table_style = &styles[&table];
        assert_eq!(table_style.border_collapse, super::BorderCollapse::Collapse);
        assert_eq!(table_style.border_spacing_horizontal.0, 3.0);
        assert_eq!(table_style.border_spacing_vertical.0, 5.0);
        assert_eq!(table_style.caption_side, super::CaptionSide::Bottom);
        assert_eq!(table_style.empty_cells, super::EmptyCells::Hide);
        assert_eq!(table_style.table_layout, super::TableLayout::Fixed);
        assert_eq!(table_style.vertical_align, super::VerticalAlign::Middle);

        let td_style = &styles[&td];
        // Inherited properties: border-collapse/border-spacing/caption-side/empty-cells.
        assert_eq!(td_style.border_collapse, super::BorderCollapse::Collapse);
        assert_eq!(td_style.border_spacing_horizontal.0, 3.0);
        assert_eq!(td_style.caption_side, super::CaptionSide::Bottom);
        assert_eq!(td_style.empty_cells, super::EmptyCells::Hide);
        // Non-inherited properties: table-layout/vertical-align (the td keeps the initial values).
        assert_eq!(td_style.table_layout, super::TableLayout::Auto);
        assert_eq!(td_style.vertical_align, super::VerticalAlign::Baseline);
    }

    #[test]
    fn border_spacing_single_value_applies_to_both_axes() {
        let dom = html::parse(br#"<table></table>"#);
        let table = find(&dom, dom.document(), "table").expect("table not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("table { border-spacing: 4px; }"),
        );
        assert_eq!(styles[&table].border_spacing_horizontal.0, 4.0);
        assert_eq!(styles[&table].border_spacing_vertical.0, 4.0);
    }

    #[test]
    fn table_layout_properties_default_correctly() {
        let dom = html::parse(br#"<table><tr><td>a</td></tr></table>"#);
        let table = find(&dom, dom.document(), "table").expect("table not found");

        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        let style = &styles[&table];
        assert_eq!(style.border_collapse, super::BorderCollapse::Separate);
        assert_eq!(style.border_spacing_horizontal.0, 0.0);
        assert_eq!(style.border_spacing_vertical.0, 0.0);
        assert_eq!(style.caption_side, super::CaptionSide::Top);
        assert_eq!(style.table_layout, super::TableLayout::Auto);
        assert_eq!(style.empty_cells, super::EmptyCells::Show);
        assert_eq!(style.vertical_align, super::VerticalAlign::Baseline);
    }

    #[test]
    fn caption_element_gets_table_caption_display_from_ua_stylesheet() {
        use super::super::ua::user_agent_stylesheet;

        let dom = html::parse(br#"<table><caption>Title</caption></table>"#);
        let caption = find(&dom, dom.document(), "caption").expect("caption not found");

        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        assert_eq!(styles[&caption].display, Display::TableCaption);
    }

    #[test]
    fn pseudo_content_ignores_declarations_on_the_real_element() {
        // A `content` declaration on an ordinary selector, without `::before`/`::after`, is invalid.
        let dom = html::parse(br#"<span class="badge">Text</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let ua = Stylesheet::default();
        let author = parse_stylesheet(r#".badge { content: "x"; }"#);

        let styles = compute_styles(&dom, &ua, &author);
        assert_eq!(styles[&span].pseudo_before_content, None);
        assert_eq!(styles[&span].pseudo_after_content, None);
    }

    #[test]
    fn list_style_properties_default_to_disc_outside_and_no_image() {
        let dom = html::parse(br#"<li>a</li>"#);
        let li = find(&dom, dom.document(), "li").expect("li not found");
        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&li].list_style_type, super::ListStyleType::Disc);
        assert_eq!(
            styles[&li].list_style_position,
            super::ListStylePosition::Outside
        );
        assert_eq!(styles[&li].list_style_image, None);
    }

    #[test]
    fn list_style_type_parses_all_keywords() {
        let dom = html::parse(br#"<li>a</li>"#);
        let li = find(&dom, dom.document(), "li").expect("li not found");

        for (value, expected) in [
            ("disc", super::ListStyleType::Disc),
            ("circle", super::ListStyleType::Circle),
            ("square", super::ListStyleType::Square),
            ("decimal", super::ListStyleType::Decimal),
            (
                "decimal-leading-zero",
                super::ListStyleType::DecimalLeadingZero,
            ),
            ("lower-roman", super::ListStyleType::LowerRoman),
            ("upper-roman", super::ListStyleType::UpperRoman),
            ("lower-alpha", super::ListStyleType::LowerAlpha),
            ("upper-alpha", super::ListStyleType::UpperAlpha),
            ("none", super::ListStyleType::None),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("li {{ list-style-type: {value}; }}")),
            );
            assert_eq!(
                styles[&li].list_style_type, expected,
                "list-style-type: {value}"
            );
        }
    }

    #[test]
    fn list_style_properties_are_inherited() {
        let dom = html::parse(br#"<ul><li>a</li></ul>"#);
        let ul = find(&dom, dom.document(), "ul").expect("ul not found");
        let li = find(&dom, ul, "li").expect("li not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "ul { list-style-type: square; list-style-position: inside; \
                 list-style-image: url(marker.png); }",
            ),
        );
        assert_eq!(styles[&li].list_style_type, super::ListStyleType::Square);
        assert_eq!(
            styles[&li].list_style_position,
            super::ListStylePosition::Inside
        );
        assert_eq!(styles[&li].list_style_image.as_deref(), Some("marker.png"));
    }

    #[test]
    fn list_style_shorthand_expands_to_all_three_longhands() {
        let dom = html::parse(br#"<li>a</li>"#);
        let li = find(&dom, dom.document(), "li").expect("li not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("li { list-style: square inside url(marker.png); }"),
        );
        assert_eq!(styles[&li].list_style_type, super::ListStyleType::Square);
        assert_eq!(
            styles[&li].list_style_position,
            super::ListStylePosition::Inside
        );
        assert_eq!(styles[&li].list_style_image.as_deref(), Some("marker.png"));
    }

    #[test]
    fn list_style_shorthand_none_clears_type_and_image() {
        let dom = html::parse(br#"<li>a</li>"#);
        let li = find(&dom, dom.document(), "li").expect("li not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("li { list-style: none; }"),
        );
        assert_eq!(styles[&li].list_style_type, super::ListStyleType::None);
        assert_eq!(styles[&li].list_style_image, None);
    }

    #[test]
    fn list_style_shorthand_type_then_bare_none_means_image_none() {
        // A `none` appearing after `type` is already decided should be read as
        // `list-style-image: none` (it must not override `list-style-type`).
        let dom = html::parse(br#"<li>a</li>"#);
        let li = find(&dom, dom.document(), "li").expect("li not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("li { list-style: square none; }"),
        );
        assert_eq!(styles[&li].list_style_type, super::ListStyleType::Square);
        assert_eq!(styles[&li].list_style_image, None);
    }

    #[test]
    fn li_gets_list_item_display_from_ua_stylesheet() {
        use super::super::ua::user_agent_stylesheet;

        let dom = html::parse(br#"<ul><li>a</li></ul>"#);
        let li = find(&dom, dom.document(), "li").expect("li not found");

        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
        assert_eq!(styles[&li].display, Display::ListItem);
    }

    #[test]
    fn padding_left_and_margin_left_longhands_parse_directly() {
        // Parsing a standalone longhand without going through a shorthand (`padding`/`margin`)
        // (a regression test for a gap found and fixed while implementing `list-style`).
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { padding-left: 12px; padding-top: 3px; \
                 margin-left: 5px; margin-top: 7px; }",
            ),
        );
        assert_eq!(
            styles[&div].padding_left,
            super::LengthPercentage::Length(12.0)
        );
        assert_eq!(
            styles[&div].padding_top,
            super::LengthPercentage::Length(3.0)
        );
        assert_eq!(
            styles[&div].margin_left,
            super::LengthPercentageOrAuto::LengthPercentage(super::LengthPercentage::Length(5.0))
        );
        assert_eq!(
            styles[&div].margin_top,
            super::LengthPercentageOrAuto::LengthPercentage(super::LengthPercentage::Length(7.0))
        );
    }

    #[test]
    fn overflow_parses_all_keywords_and_defaults_to_visible() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&div].overflow, super::Overflow::Visible);

        for (value, expected) in [
            ("visible", super::Overflow::Visible),
            ("hidden", super::Overflow::Hidden),
            ("scroll", super::Overflow::Scroll),
            ("auto", super::Overflow::Auto),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ overflow: {value}; }}")),
            );
            assert_eq!(styles[&div].overflow, expected, "overflow: {value}");
        }
    }

    #[test]
    fn overflow_is_not_inherited() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { overflow: hidden; }"),
        );
        assert_eq!(styles[&div].overflow, super::Overflow::Hidden);
        assert_eq!(styles[&p].overflow, super::Overflow::Visible);
    }

    #[test]
    fn box_sizing_parses_both_keywords_and_defaults_to_content_box() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&div].box_sizing, super::BoxSizing::ContentBox);

        for (value, expected) in [
            ("content-box", super::BoxSizing::ContentBox),
            ("border-box", super::BoxSizing::BorderBox),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ box-sizing: {value}; }}")),
            );
            assert_eq!(styles[&div].box_sizing, expected, "box-sizing: {value}");
        }
    }

    #[test]
    fn box_sizing_is_not_inherited() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { box-sizing: border-box; }"),
        );
        assert_eq!(styles[&div].box_sizing, super::BoxSizing::BorderBox);
        assert_eq!(styles[&p].box_sizing, super::BoxSizing::ContentBox);
    }

    #[test]
    fn z_index_parses_auto_and_integers_and_is_not_inherited() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&div].z_index, super::ZIndex::Auto);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { z-index: -3; }"),
        );
        assert_eq!(styles[&div].z_index, super::ZIndex::Value(-3));
        assert_eq!(
            styles[&p].z_index,
            super::ZIndex::Auto,
            "z-index should not be inherited"
        );
    }

    #[test]
    fn visibility_parses_all_keywords_and_is_inherited() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        for (value, expected) in [
            ("visible", super::Visibility::Visible),
            ("hidden", super::Visibility::Hidden),
            ("collapse", super::Visibility::Collapse),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ visibility: {value}; }}")),
            );
            assert_eq!(styles[&div].visibility, expected, "visibility: {value}");
            assert_eq!(
                styles[&p].visibility, expected,
                "visibility should be inherited: {value}"
            );
        }
    }

    #[test]
    fn outline_shorthand_expands_to_width_style_color_and_defaults_to_currentcolor() {
        let dom = html::parse(br#"<div style="color: rgb(9, 9, 9);">a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { outline: 3px dashed; }"),
        );
        assert_eq!(styles[&div].outline_width.0, 3.0);
        assert_eq!(styles[&div].outline_style, super::BorderStyle::Dashed);
        // With the colour omitted it resolves to `currentcolor` (this element's own color).
        assert_eq!(
            styles[&div].outline_color,
            RgbaColor {
                red: 9,
                green: 9,
                blue: 9,
                alpha: 1.0
            }
        );
    }

    #[test]
    fn border_style_keyword_parses_groove_ridge_inset_outset() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for (value, expected) in [
            ("groove", super::BorderStyle::Groove),
            ("ridge", super::BorderStyle::Ridge),
            ("inset", super::BorderStyle::Inset),
            ("outset", super::BorderStyle::Outset),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ border-style: {value}; }}")),
            );
            assert_eq!(
                styles[&div].border_top_style, expected,
                "border-style: {value}"
            );
        }
    }

    #[test]
    fn border_radius_shorthand_with_slash_sets_independent_horizontal_and_vertical_radii() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { border-radius: 10px 20px / 30px 40px; }"),
        );
        let style = &styles[&div];
        assert_eq!(style.border_top_left_radius.horizontal.0, 10.0);
        assert_eq!(style.border_top_left_radius.vertical.0, 30.0);
        assert_eq!(style.border_top_right_radius.horizontal.0, 20.0);
        assert_eq!(style.border_top_right_radius.vertical.0, 40.0);
        // Two values are in the order (top-left/bottom-right, top-right/bottom-left).
        assert_eq!(style.border_bottom_right_radius.horizontal.0, 10.0);
        assert_eq!(style.border_bottom_right_radius.vertical.0, 30.0);
    }

    #[test]
    fn border_radius_shorthand_without_slash_makes_a_circle() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { border-radius: 15px; }"),
        );
        let corner = styles[&div].border_top_left_radius;
        assert_eq!(corner.horizontal.0, 15.0);
        assert_eq!(corner.vertical.0, 15.0);
    }

    #[test]
    fn border_corner_radius_longhand_accepts_one_or_two_lengths() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { border-top-left-radius: 5px 8px; border-top-right-radius: 6px; }",
            ),
        );
        let style = &styles[&div];
        assert_eq!(style.border_top_left_radius.horizontal.0, 5.0);
        assert_eq!(style.border_top_left_radius.vertical.0, 8.0);
        assert_eq!(style.border_top_right_radius.horizontal.0, 6.0);
        assert_eq!(style.border_top_right_radius.vertical.0, 6.0);
    }

    #[test]
    fn content_attr_reads_the_element_own_html_attribute() {
        let dom = html::parse(br#"<span data-label="hello">x</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(r#"span::before { content: attr(data-label) ": "; }"#),
        );
        assert_eq!(
            styles[&span].pseudo_before_content.as_deref(),
            Some("hello: ")
        );
    }

    #[test]
    fn content_attr_is_empty_when_the_attribute_is_missing() {
        let dom = html::parse(br#"<span>x</span>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(r#"span::before { content: "[" attr(data-missing) "]"; }"#),
        );
        assert_eq!(styles[&span].pseudo_before_content.as_deref(), Some("[]"));
    }

    #[test]
    fn counter_increments_across_siblings_and_resets_are_scoped_to_the_parent() {
        // Counters carry over between siblings (counter-increment accumulates), and a
        // different parent gives an independent scope through counter-reset.
        let dom = html::parse(
            br#"<div>
                <section>
                    <h2 class="a">a</h2>
                    <h2 class="b">b</h2>
                </section>
                <section>
                    <h2 class="c">c</h2>
                </section>
            </div>"#,
        );
        let mut h2s = Vec::new();
        find_all(&dom, dom.document(), "h2", &mut h2s);
        assert_eq!(h2s.len(), 3);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "section { counter-reset: h2count; } \
                 h2 { counter-increment: h2count; } \
                 h2::before { content: counter(h2count) \". \"; }",
            ),
        );
        assert_eq!(
            styles[&h2s[0]].pseudo_before_content.as_deref(),
            Some("1. ")
        );
        assert_eq!(
            styles[&h2s[1]].pseudo_before_content.as_deref(),
            Some("2. ")
        );
        // The second `section` is an independent scope, so it counts from 1 again.
        assert_eq!(
            styles[&h2s[2]].pseudo_before_content.as_deref(),
            Some("1. ")
        );
    }

    #[test]
    fn counter_reset_on_an_element_stays_visible_to_its_following_siblings() {
        // Regression test: originally there was a bug where the counter pushed by
        // `counter-reset` was popped as soon as that element itself finished, making it
        // invisible to the following siblings ("the scope extends to the element itself
        // and the siblings that follow it").
        let dom = html::parse(
            br#"<div>
                <h2 class="reset">Intro</h2>
                <h3 class="a">A</h3>
                <h3 class="b">B</h3>
            </div>"#,
        );
        let h3_a = find(&dom, dom.document(), "h3").expect("h3 not found");
        let mut h3s = Vec::new();
        find_all(&dom, dom.document(), "h3", &mut h3s);
        assert_eq!(h3s[0], h3_a);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "h2 { counter-reset: section; } \
                 h3 { counter-increment: section; } \
                 h3::before { content: counter(section) \". \"; }",
            ),
        );
        assert_eq!(
            styles[&h3s[0]].pseudo_before_content.as_deref(),
            Some("1. ")
        );
        assert_eq!(
            styles[&h3s[1]].pseudo_before_content.as_deref(),
            Some("2. ")
        );
    }

    #[test]
    fn counters_function_joins_nested_scope_values_with_the_separator() {
        let dom = html::parse(
            br#"<ol class="outer">
                <li class="a">a
                    <ol class="inner"><li class="b">b</li></ol>
                </li>
            </ol>"#,
        );
        let li_a = find(&dom, dom.document(), "li").expect("li not found");
        let mut lis = Vec::new();
        find_all(&dom, dom.document(), "li", &mut lis);
        assert_eq!(lis[0], li_a);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "ol { counter-reset: item; } \
                 li { counter-increment: item; } \
                 li::before { content: counters(item, \".\"); }",
            ),
        );
        assert_eq!(styles[&lis[0]].pseudo_before_content.as_deref(), Some("1"));
        assert_eq!(
            styles[&lis[1]].pseudo_before_content.as_deref(),
            Some("1.1")
        );
    }

    #[test]
    fn counter_increment_on_an_unknown_counter_implicitly_creates_it_at_zero() {
        let dom = html::parse(br#"<div><span>x</span></div>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "span { counter-increment: undeclared; } \
                 span::before { content: counter(undeclared); }",
            ),
        );
        assert_eq!(styles[&span].pseudo_before_content.as_deref(), Some("1"));
    }

    #[test]
    fn counter_styles_cover_roman_alpha_and_non_numeric_fallback() {
        let dom = html::parse(br#"<div><span>x</span></div>"#);
        let span = find(&dom, dom.document(), "span").expect("span not found");

        for (style, expected) in [
            ("upper-roman", "IV"),
            ("lower-roman", "iv"),
            ("upper-alpha", "D"),
            ("lower-alpha", "d"),
            ("decimal-leading-zero", "04"),
            ("disc", ""),
            ("none", ""),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!(
                    "span {{ counter-increment: c 4; }} \
                     span::before {{ content: counter(c, {style}); }}"
                )),
            );
            assert_eq!(
                styles[&span].pseudo_before_content.as_deref(),
                Some(expected),
                "counter(c, {style})"
            );
        }
    }

    #[test]
    fn after_content_is_resolved_after_descendants_so_it_reflects_their_counter_changes() {
        // Regression test: resolving `::after` before the descendants in DOM order (during
        // this element's own processing) meant changes made by the descendants via
        // `counter-increment`/`quotes` were not reflected.
        let dom = html::parse(br#"<div><span>x</span></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let span = find(&dom, dom.document(), "span").expect("span not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { counter-reset: c; } \
                 span { counter-increment: c; } \
                 div::after { content: \"total=\" counter(c); }",
            ),
        );
        let _ = span;
        assert_eq!(
            styles[&div].pseudo_after_content.as_deref(),
            Some("total=1")
        );
    }

    #[test]
    fn nested_quotes_use_the_pair_matching_their_nesting_depth() {
        // The depth update for `::after` (close-quote) must not happen before the descendants
        // are processed, or a nested `<q>` would always use the depth-0 pair.
        let dom = html::parse(br#"<div><q class="outer">a<q class="inner">b</q>c</q></div>"#);
        let outer = find(&dom, dom.document(), "q").expect("outer q not found");
        let mut qs = Vec::new();
        find_all(&dom, dom.document(), "q", &mut qs);
        assert_eq!(qs[0], outer);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                r#"q { quotes: "\201C" "\201D" "\2018" "\2019"; }
                   q::before { content: open-quote; }
                   q::after { content: close-quote; }"#,
            ),
        );
        assert_eq!(
            styles[&qs[0]].pseudo_before_content.as_deref(),
            Some("\u{201C}")
        );
        assert_eq!(
            styles[&qs[1]].pseudo_before_content.as_deref(),
            Some("\u{2018}")
        );
        assert_eq!(
            styles[&qs[1]].pseudo_after_content.as_deref(),
            Some("\u{2019}")
        );
        assert_eq!(
            styles[&qs[0]].pseudo_after_content.as_deref(),
            Some("\u{201D}")
        );
    }

    #[test]
    fn quotes_none_produces_empty_strings_but_still_tracks_depth() {
        let dom = html::parse(br#"<q>a</q>"#);
        let q = find(&dom, dom.document(), "q").expect("q not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "q { quotes: none; } q::before { content: open-quote; } \
                 q::after { content: close-quote; }",
            ),
        );
        assert_eq!(styles[&q].pseudo_before_content.as_deref(), Some(""));
        assert_eq!(styles[&q].pseudo_after_content.as_deref(), Some(""));
    }

    #[test]
    fn first_letter_style_only_captures_the_supported_property_subset() {
        let dom = html::parse(br#"<p>text</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");
        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "p::first-letter { font-size: 2em; color: rgb(200, 0, 0); \
                 float: left; }",
            ),
        );
        let fl = styles[&p]
            .first_letter_style
            .as_ref()
            .expect("first_letter_style should be Some");
        assert_eq!(fl.font_size, Some(super::Length(32.0)));
        assert_eq!(
            fl.color,
            Some(RgbaColor {
                red: 200,
                green: 0,
                blue: 0,
                alpha: 1.0
            })
        );
        // `float` is an unsupported property here, so it is ignored.
        assert_eq!(fl.font_weight, None);
    }

    #[test]
    fn first_letter_style_is_none_without_a_matching_rule() {
        let dom = html::parse(br#"<p>text</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");
        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(styles[&p].first_letter_style, None);
    }

    #[test]
    fn resolve_margin_box_content_formats_page_and_pages_counters() {
        let parts = vec![
            ContentPart::String("Page ".to_string()),
            ContentPart::Counter("page".to_string(), ListStyleType::Decimal),
            ContentPart::String(" of ".to_string()),
            ContentPart::Counter("pages".to_string(), ListStyleType::Decimal),
        ];
        assert_eq!(
            resolve_margin_box_content(&parts, 3, Some(10)),
            "Page 3 of 10"
        );
    }

    #[test]
    fn resolve_margin_box_content_leaves_pages_empty_when_total_is_unknown() {
        // In streaming mode `counter(pages)` is expected to be an error in itself, but this
        // function is written to behave safely and simply return an empty string when it is
        // handed `total_pages: None`.
        let parts = vec![ContentPart::Counter(
            "pages".to_string(),
            ListStyleType::Decimal,
        )];
        assert_eq!(resolve_margin_box_content(&parts, 1, None), "");
    }

    #[test]
    fn resolve_margin_box_content_respects_the_counter_style() {
        let parts = vec![ContentPart::Counter(
            "page".to_string(),
            ListStyleType::UpperRoman,
        )];
        assert_eq!(resolve_margin_box_content(&parts, 4, None), "IV");
    }

    #[test]
    fn resolve_margin_box_content_ignores_attr_and_unrelated_counters_and_quotes() {
        let parts = vec![
            ContentPart::Attr("href".to_string()),
            ContentPart::Counter("chapter".to_string(), ListStyleType::Decimal),
            ContentPart::OpenQuote,
            ContentPart::String("x".to_string()),
            ContentPart::CloseQuote,
        ];
        assert_eq!(resolve_margin_box_content(&parts, 1, None), "x");
    }

    #[test]
    fn min_and_max_size_parse_lengths_percentages_and_none() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&div].min_width, LengthPercentage::Length(0.0));
        assert_eq!(defaults[&div].min_height, LengthPercentage::Length(0.0));
        assert_eq!(defaults[&div].max_width, MaxSize::None);
        assert_eq!(defaults[&div].max_height, MaxSize::None);

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { min-width: 10px; min-height: 50%; max-width: 20em; max-height: none; }",
            ),
        );
        assert_eq!(styles[&div].min_width, LengthPercentage::Length(10.0));
        assert_eq!(styles[&div].min_height, LengthPercentage::Percentage(0.5));
        // `em` is folded into px during the cascade against the default font-size (16px).
        assert_eq!(
            styles[&div].max_width,
            MaxSize::LengthPercentage(LengthPercentage::Length(320.0))
        );
        assert_eq!(styles[&div].max_height, MaxSize::None);
    }

    /// Keyword values (`auto`/`min-content` and so on) are not supported and the declaration
    /// is ignored, leaving the other declarations in the same rule unaffected.
    #[test]
    fn min_and_max_size_reject_intrinsic_sizing_keywords() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { min-width: auto; max-width: max-content; min-height: min-content; \
                 max-height: fit-content; width: 30px; }",
            ),
        );
        assert_eq!(styles[&div].min_width, LengthPercentage::Length(0.0));
        assert_eq!(styles[&div].min_height, LengthPercentage::Length(0.0));
        assert_eq!(styles[&div].max_width, MaxSize::None);
        assert_eq!(styles[&div].max_height, MaxSize::None);
        assert_eq!(
            styles[&div].width,
            LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(30.0)),
            "unsupported keywords must not swallow the other declarations"
        );
    }

    #[test]
    fn aspect_ratio_parses_auto_ratios_and_their_combination() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert_eq!(defaults[&div].aspect_ratio, AspectRatio::default());
        assert!(defaults[&div].aspect_ratio.auto);
        assert_eq!(defaults[&div].aspect_ratio.ratio, None);

        for (value, expected) in [
            (
                "auto",
                AspectRatio {
                    auto: true,
                    ratio: None,
                },
            ),
            (
                "16 / 9",
                AspectRatio {
                    auto: false,
                    ratio: Some(16.0 / 9.0),
                },
            ),
            // An omitted denominator means `/ 1`.
            (
                "2",
                AspectRatio {
                    auto: false,
                    ratio: Some(2.0),
                },
            ),
            (
                "auto 16 / 9",
                AspectRatio {
                    auto: true,
                    ratio: Some(16.0 / 9.0),
                },
            ),
            // `auto` and `<ratio>` may come in either order.
            (
                "16 / 9 auto",
                AspectRatio {
                    auto: true,
                    ratio: Some(16.0 / 9.0),
                },
            ),
        ] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ aspect-ratio: {value}; }}")),
            );
            assert_eq!(styles[&div].aspect_ratio, expected, "aspect-ratio: {value}");
        }
    }

    /// A degenerate ratio containing zero or a negative number is an invalid declaration and is ignored.
    #[test]
    fn aspect_ratio_rejects_degenerate_ratios() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        for value in ["0 / 1", "1 / 0", "-16 / 9", "0"] {
            let styles = compute_styles(
                &dom,
                &Stylesheet::default(),
                &parse_stylesheet(&format!("div {{ aspect-ratio: {value}; width: 30px; }}")),
            );
            assert_eq!(
                styles[&div].aspect_ratio,
                AspectRatio::default(),
                "aspect-ratio: {value} should be ignored"
            );
            assert_eq!(
                styles[&div].width,
                LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(30.0)),
                "an invalid ratio must not swallow the other declarations"
            );
        }
    }

    #[test]
    fn text_detail_properties_parse_and_inherit() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let defaults = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());
        assert!(defaults[&div].text_shadow.is_empty());
        assert_eq!(defaults[&div].text_overflow, TextOverflow::Clip);
        assert_eq!(defaults[&div].word_break, WordBreak::Normal);
        assert_eq!(defaults[&div].overflow_wrap, OverflowWrap::Normal);
        assert_eq!(defaults[&div].hyphens, Hyphens::Manual);
        assert_eq!(defaults[&div].text_emphasis_style, EmphasisStyle::None);
        assert_eq!(
            defaults[&div].text_emphasis_position,
            EmphasisPosition::Over
        );

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                "div { text-shadow: 1px 2px 3px rgb(1, 2, 3); word-break: break-all; \
                 overflow-wrap: break-word; hyphens: none; text-overflow: ellipsis; \
                 text-emphasis: open sesame rgb(4, 5, 6); text-emphasis-position: under; }",
            ),
        );
        assert_eq!(styles[&div].text_shadow.len(), 1);
        assert_eq!(styles[&div].text_shadow[0].offset_x, 1.0);
        assert_eq!(styles[&div].text_shadow[0].offset_y, 2.0);
        assert_eq!(styles[&div].text_shadow[0].blur_radius, 3.0);
        assert_eq!(styles[&div].text_shadow[0].color.red, 1);
        assert_eq!(styles[&div].word_break, WordBreak::BreakAll);
        assert_eq!(styles[&div].overflow_wrap, OverflowWrap::BreakWord);
        assert_eq!(styles[&div].hyphens, Hyphens::None);
        assert_eq!(styles[&div].text_overflow, TextOverflow::Ellipsis);
        assert_eq!(
            styles[&div].text_emphasis_style,
            EmphasisStyle::Shape {
                shape: crate::style::EmphasisShape::Sesame,
                filled: false,
            }
        );
        assert_eq!(styles[&div].text_emphasis_color.red, 4);
        assert_eq!(styles[&div].text_emphasis_position, EmphasisPosition::Under);

        // Which are inherited and which are not (`text-overflow` alone is non-inherited).
        assert_eq!(styles[&p].text_shadow.len(), 1);
        assert_eq!(styles[&p].word_break, WordBreak::BreakAll);
        assert_eq!(styles[&p].overflow_wrap, OverflowWrap::BreakWord);
        assert_eq!(styles[&p].hyphens, Hyphens::None);
        assert_eq!(styles[&p].text_emphasis_position, EmphasisPosition::Under);
        assert_eq!(styles[&p].text_emphasis_color.red, 4);
        assert_eq!(
            styles[&p].text_overflow,
            TextOverflow::Clip,
            "text-overflow must not be inherited"
        );
    }

    /// `word-wrap` is the legacy alias of `overflow-wrap`, and `hyphens: auto` behaves the
    /// same as `manual`.
    #[test]
    fn text_detail_property_aliases() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { word-wrap: anywhere; hyphens: auto; }"),
        );
        assert_eq!(styles[&div].overflow_wrap, OverflowWrap::BreakWord);
        assert_eq!(styles[&div].hyphens, Hyphens::Manual);
    }

    /// `text-emphasis-style: <string>` uses only the first character.
    #[test]
    fn text_emphasis_style_accepts_a_string() {
        let dom = html::parse(br#"<div>a</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(r#"div { text-emphasis-style: "×か"; }"#),
        );
        assert_eq!(styles[&div].text_emphasis_style, EmphasisStyle::String('×'));
    }

    #[test]
    fn min_and_max_size_are_not_inherited() {
        let dom = html::parse(br#"<div><p>a</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let p = find(&dom, div, "p").expect("p not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { min-width: 100px; max-height: 40px; }"),
        );
        assert_eq!(styles[&div].min_width, LengthPercentage::Length(100.0));
        assert_eq!(styles[&p].min_width, LengthPercentage::Length(0.0));
        assert_eq!(styles[&p].max_height, MaxSize::None);
    }

    #[test]
    fn absolute_length_units_resolve_to_px() {
        // Business-document CSS often writes dimensions in mm/pt. Check they fold at 1in = 96px.
        let dom = html::parse(
            br#"<div class="mm"></div><div class="cm"></div><div class="in"></div>
                <div class="pt"></div><div class="pc"></div><div class="q"></div>"#,
        );
        let mut divs = Vec::new();
        find_all(&dom, dom.document(), "div", &mut divs);
        let [mm, cm, inch, pt, pc, q] = divs[..] else {
            panic!("expected exactly 6 divs")
        };

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet(
                ".mm { width: 25.4mm; } .cm { width: 2.54cm; } .in { width: 1in; }
                 .pt { width: 72pt; } .pc { width: 6pc; } .q { width: 101.6q; }",
            ),
        );
        // Each of these is one inch = 96px.
        for node in [mm, cm, inch, pt, pc, q] {
            assert_eq!(
                styles[&node].width,
                LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(96.0))
            );
        }
    }

    #[test]
    fn absolute_length_units_work_inside_calc() {
        let dom = html::parse(br#"<div></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { width: calc(1in - 24pt); }"),
        );
        // 96px - 32px. The calc stays a `Calc` (only the unit conversion is folded at parse time).
        assert_eq!(
            styles[&div].width,
            LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Calc {
                px: 64.0,
                percent: 0.0
            })
        );
    }

    #[test]
    fn viewport_units_are_still_rejected() {
        // Viewport units stay unsupported, print having no concept of one (the declaration is ignored).
        let dom = html::parse(br#"<div></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let styles = compute_styles(
            &dom,
            &Stylesheet::default(),
            &parse_stylesheet("div { width: 50vh; }"),
        );
        assert_eq!(styles[&div].width, LengthPercentageOrAuto::Auto);
    }
}
