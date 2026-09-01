//! Parsing of `Declaration: value;`, and the property declaration types.

use cssparser::{match_ignore_ascii_case, CowRcStr, ParseError, Parser, Token};
use palette::{FromColor, Lab, Lch, Oklab, Oklch, Srgb};

use super::color_mix::{self, HueMethod, Space as ColorSpace};

use super::values::{
    AlignContent, AlignItems, AlignSelf, AspectRatio, BackgroundAttachment, BackgroundRepeat,
    BorderCollapse, BorderStyle, BoxSizing, BreakBetween, BreakInside, CaptionSide, Clear, Color,
    ContentPart, Display, EmphasisPosition, EmphasisShape, EmphasisStyle, EmptyCells,
    FlexDirection, FlexWrap, Float, FontStyle, FontWeight, GridArea, GridAutoFlow, GridLine,
    Hyphens, JustifyContent, ListStylePosition, ListStyleType, ObjectFit, Overflow, OverflowWrap,
    Position, QuotePair, RepeatCount, SpecifiedBackgroundPosition, SpecifiedBackgroundSize,
    SpecifiedBoxShadow, SpecifiedCalc, SpecifiedCornerRadius, SpecifiedFlexBasis, SpecifiedLength,
    SpecifiedLengthPercentage, SpecifiedLengthPercentageOrAuto, SpecifiedLineHeight,
    SpecifiedMaxSize, SpecifiedSpacing, SpecifiedTextShadow, SpecifiedTrackBreadth,
    SpecifiedTrackComponent, SpecifiedTrackList, SpecifiedTrackSize, SpecifiedTransformFunction,
    SpecifiedVerticalAlign, TableLayout, TextAlign, TextDecorationLine, TextOverflow,
    TextTransform, Visibility, WhiteSpace, WordBreak, ZIndex,
};

#[derive(Debug, Clone, PartialEq)]
pub enum PropertyDeclaration {
    Display(Display),
    Width(SpecifiedLengthPercentageOrAuto),
    Height(SpecifiedLengthPercentageOrAuto),
    /// `min-width`/`min-height`. Initial value `0`.
    /// `auto`/`min-content`/`max-content`/`fit-content` are not supported.
    MinWidth(SpecifiedLengthPercentage),
    MinHeight(SpecifiedLengthPercentage),
    /// `max-width`/`max-height`. Initial value `none` (no upper bound).
    MaxWidth(SpecifiedMaxSize),
    MaxHeight(SpecifiedMaxSize),
    /// `aspect-ratio: auto || <ratio>`.
    AspectRatio(AspectRatio),
    MarginTop(SpecifiedLengthPercentageOrAuto),
    MarginRight(SpecifiedLengthPercentageOrAuto),
    MarginBottom(SpecifiedLengthPercentageOrAuto),
    MarginLeft(SpecifiedLengthPercentageOrAuto),
    PaddingTop(SpecifiedLengthPercentage),
    PaddingRight(SpecifiedLengthPercentage),
    PaddingBottom(SpecifiedLengthPercentage),
    PaddingLeft(SpecifiedLengthPercentage),
    BorderTopWidth(SpecifiedLength),
    BorderRightWidth(SpecifiedLength),
    BorderBottomWidth(SpecifiedLength),
    BorderLeftWidth(SpecifiedLength),
    BorderTopColor(Color),
    BorderRightColor(Color),
    BorderBottomColor(Color),
    BorderLeftColor(Color),
    BorderTopStyle(BorderStyle),
    BorderRightStyle(BorderStyle),
    BorderBottomStyle(BorderStyle),
    BorderLeftStyle(BorderStyle),
    BorderTopLeftRadius(SpecifiedCornerRadius),
    BorderTopRightRadius(SpecifiedCornerRadius),
    BorderBottomRightRadius(SpecifiedCornerRadius),
    BorderBottomLeftRadius(SpecifiedCornerRadius),
    FontSize(SpecifiedLength),
    FontFamily(Vec<String>),
    FontWeight(FontWeight),
    FontStyle(FontStyle),
    Color(Color),
    BackgroundColor(Color),
    /// `url(...)` (the raw value; resolving it is left to the caller, the same policy as
    /// `FontFaceSource::Url`). `None` means `none` (no background image).
    BackgroundImage(Option<String>),
    BackgroundPosition(SpecifiedBackgroundPosition),
    BackgroundSize(SpecifiedBackgroundSize),
    BackgroundRepeat(BackgroundRepeat),
    /// `fixed` is drawn the same as `scroll`.
    BackgroundAttachment(BackgroundAttachment),
    TextDecorationLine(TextDecorationLine),
    /// `content` for `::before`/`::after`/`::first-letter`. `None` means
    /// `none`/`normal` (no generated box). Concatenation of string literals, `attr`,
    /// `counter`/`counters` and quote keywords is supported.
    Content(Option<Vec<ContentPart>>),
    BreakBefore(BreakBetween),
    BreakAfter(BreakBetween),
    BreakInside(BreakInside),
    Orphans(u32),
    Widows(u32),
    Float(Float),
    Clear(Clear),
    /// `static`/`relative`/`absolute`/`fixed`.
    Position(Position),
    Top(SpecifiedLengthPercentageOrAuto),
    Right(SpecifiedLengthPercentageOrAuto),
    Bottom(SpecifiedLengthPercentageOrAuto),
    Left(SpecifiedLengthPercentageOrAuto),
    TextAlign(TextAlign),
    LineHeight(SpecifiedLineHeight),
    TextIndent(SpecifiedLengthPercentage),
    WhiteSpace(WhiteSpace),
    LetterSpacing(SpecifiedSpacing),
    WordSpacing(SpecifiedSpacing),
    TextTransform(TextTransform),
    BorderCollapse(BorderCollapse),
    /// `border-spacing` (horizontal, vertical). A single value applies to both (as the spec says).
    BorderSpacing(SpecifiedLength, SpecifiedLength),
    CaptionSide(CaptionSide),
    TableLayout(TableLayout),
    EmptyCells(EmptyCells),
    VerticalAlign(SpecifiedVerticalAlign),
    ListStyleType(ListStyleType),
    ListStylePosition(ListStylePosition),
    /// `url(...)` (the raw value; resolving it is left to the caller). `None` means `none`.
    /// In practice it always falls back to the `list-style-type` text marker, and an image
    /// marker itself is never drawn.
    ListStyleImage(Option<String>),
    /// `hidden`/`scroll`/`auto` all get the same clipping.
    Overflow(Overflow),
    /// `padding-box` (non-standard) is not supported.
    BoxSizing(BoxSizing),
    /// It has no effect on a `position: static` element (as the spec says).
    ZIndex(ZIndex),
    /// `collapse` is treated as `hidden`.
    Visibility(Visibility),
    OutlineWidth(SpecifiedLength),
    /// `outline-style`. `auto` (the UA-dependent default focus ring) is not supported; only
    /// the same value set as `border-style` plus `none` is accepted.
    OutlineStyle(BorderStyle),
    OutlineColor(Color),
    /// `counter-reset: name [value]` (several may be listed). An empty Vec means `none`.
    CounterReset(Vec<(String, i32)>),
    /// `counter-increment: name [value]` (several may be listed; the value defaults to 1).
    CounterIncrement(Vec<(String, i32)>),
    /// `quotes`. `None` means `none` (always generating an empty string).
    Quotes(Option<Vec<QuotePair>>),
    /// `object-fit`. Meaningful only on `<img>`.
    ObjectFit(ObjectFit),
    /// `object-position`. The value grammar is the same as `background-position`, so
    /// `SpecifiedBackgroundPosition` is reused.
    ObjectPosition(SpecifiedBackgroundPosition),
    /// `box-shadow`. Comma-separated multiples.
    BoxShadow(Vec<SpecifiedBoxShadow>),
    /// `flex-direction`. Flex containers only.
    FlexDirection(FlexDirection),
    FlexWrap(FlexWrap),
    JustifyContent(JustifyContent),
    AlignItems(AlignItems),
    AlignContent(AlignContent),
    /// `align-self`. Flex items only.
    AlignSelf(AlignSelf),
    /// `flex-grow`. A negative value is invalid (rejected at parse time).
    FlexGrow(f32),
    /// `flex-shrink`. A negative value is invalid (rejected at parse time).
    FlexShrink(f32),
    FlexBasis(SpecifiedFlexBasis),
    RowGap(SpecifiedLengthPercentage),
    ColumnGap(SpecifiedLengthPercentage),
    /// `transform`. An empty Vec means `none`.
    Transform(Vec<SpecifiedTransformFunction>),
    /// `transform-origin`. The value grammar is the same as `background-position`, so
    /// `SpecifiedBackgroundPosition` is reused (the initial value is `50% 50%`, set
    /// separately from `background-position`'s `0% 0%`).
    TransformOrigin(SpecifiedBackgroundPosition),
    /// `opacity`. Already clamped to 0-1.
    Opacity(f32),
    /// `text-shadow`. An empty Vec means `none`.
    TextShadow(Vec<SpecifiedTextShadow>),
    /// `text-overflow`. A `<string>` value is not supported.
    TextOverflow(TextOverflow),
    /// `word-break`.
    WordBreak(WordBreak),
    /// `overflow-wrap` (also known as `word-wrap`). `anywhere` is treated as `break-word`.
    OverflowWrap(OverflowWrap),
    /// `hyphens`. `auto` behaves the same as `manual`.
    Hyphens(Hyphens),
    /// `text-emphasis-style`.
    TextEmphasisStyle(EmphasisStyle),
    TextEmphasisColor(Color),
    TextEmphasisPosition(EmphasisPosition),
    /// `grid-template-columns`/`grid-template-rows`.
    /// An empty `TrackList` means `none`.
    GridTemplateColumns(SpecifiedTrackList),
    GridTemplateRows(SpecifiedTrackList),
    /// `grid-auto-columns`/`grid-auto-rows`. An empty Vec means the initial value `auto`.
    GridAutoColumns(Vec<SpecifiedTrackSize>),
    GridAutoRows(Vec<SpecifiedTrackSize>),
    GridAutoFlow(GridAutoFlow),
    /// `grid-template-areas`. An empty Vec means `none`.
    GridTemplateAreas(Vec<GridArea>),
    /// Placement such as `grid-row-start`.
    GridRowStart(GridLine),
    GridRowEnd(GridLine),
    GridColumnStart(GridLine),
    GridColumnEnd(GridLine),
    /// `justify-items`/`justify-self` (Grid only). The value set is shared with
    /// `align-items`/`align-self`.
    JustifyItems(AlignItems),
    JustifySelf(AlignSelf),
}

/// Parse a value given a property name. Shorthands (`margin`/`padding`/`border`) are
/// expanded into the corresponding longhand declarations.
pub fn parse_declaration<'i>(
    name: &CowRcStr<'i>,
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;

    match_ignore_ascii_case! { name,
        "display" => Ok(vec![D::Display(parse_display(input)?)]),
        "width" => Ok(vec![D::Width(parse_length_percentage_or_auto(input)?)]),
        "height" => Ok(vec![D::Height(parse_length_percentage_or_auto(input)?)]),
        "min-width" => Ok(vec![D::MinWidth(parse_length_percentage(input)?)]),
        "min-height" => Ok(vec![D::MinHeight(parse_length_percentage(input)?)]),
        "max-width" => Ok(vec![D::MaxWidth(parse_max_size(input)?)]),
        "max-height" => Ok(vec![D::MaxHeight(parse_max_size(input)?)]),
        "aspect-ratio" => Ok(vec![D::AspectRatio(parse_aspect_ratio(input)?)]),
        "margin" => parse_margin_shorthand(input),
        "margin-top" => Ok(vec![D::MarginTop(parse_length_percentage_or_auto(input)?)]),
        "margin-right" => Ok(vec![D::MarginRight(parse_length_percentage_or_auto(input)?)]),
        "margin-bottom" => Ok(vec![D::MarginBottom(parse_length_percentage_or_auto(input)?)]),
        "margin-left" => Ok(vec![D::MarginLeft(parse_length_percentage_or_auto(input)?)]),
        "padding" => parse_padding_shorthand(input),
        "padding-top" => Ok(vec![D::PaddingTop(parse_non_negative_length_percentage(input)?)]),
        "padding-right" => Ok(vec![D::PaddingRight(parse_non_negative_length_percentage(input)?)]),
        "padding-bottom" => Ok(vec![D::PaddingBottom(parse_non_negative_length_percentage(input)?)]),
        "padding-left" => Ok(vec![D::PaddingLeft(parse_non_negative_length_percentage(input)?)]),
        "border" => parse_border_shorthand(input),
        "border-width" => parse_border_width_shorthand(input),
        "border-color" => parse_border_color_shorthand(input),
        "border-style" => parse_border_style_shorthand(input),
        "border-top" => parse_border_top_shorthand(input),
        "border-right" => parse_border_right_shorthand(input),
        "border-bottom" => parse_border_bottom_shorthand(input),
        "border-left" => parse_border_left_shorthand(input),
        "border-top-width" => Ok(vec![D::BorderTopWidth(parse_length(input)?)]),
        "border-right-width" => Ok(vec![D::BorderRightWidth(parse_length(input)?)]),
        "border-bottom-width" => Ok(vec![D::BorderBottomWidth(parse_length(input)?)]),
        "border-left-width" => Ok(vec![D::BorderLeftWidth(parse_length(input)?)]),
        "border-top-color" => Ok(vec![D::BorderTopColor(parse_color(input)?)]),
        "border-right-color" => Ok(vec![D::BorderRightColor(parse_color(input)?)]),
        "border-bottom-color" => Ok(vec![D::BorderBottomColor(parse_color(input)?)]),
        "border-left-color" => Ok(vec![D::BorderLeftColor(parse_color(input)?)]),
        "border-top-style" => Ok(vec![D::BorderTopStyle(parse_border_style_keyword(input)?)]),
        "border-right-style" => Ok(vec![D::BorderRightStyle(parse_border_style_keyword(input)?)]),
        "border-bottom-style" => {
            Ok(vec![D::BorderBottomStyle(parse_border_style_keyword(input)?)])
        },
        "border-left-style" => Ok(vec![D::BorderLeftStyle(parse_border_style_keyword(input)?)]),
        "border-radius" => parse_border_radius_shorthand(input),
        "border-top-left-radius" => Ok(vec![D::BorderTopLeftRadius(parse_corner_radius(input)?)]),
        "border-top-right-radius" => Ok(vec![D::BorderTopRightRadius(parse_corner_radius(input)?)]),
        "border-bottom-right-radius" => {
            Ok(vec![D::BorderBottomRightRadius(parse_corner_radius(input)?)])
        },
        "border-bottom-left-radius" => {
            Ok(vec![D::BorderBottomLeftRadius(parse_corner_radius(input)?)])
        },
        "font-size" => Ok(vec![D::FontSize(parse_length(input)?)]),
        "font-family" => Ok(vec![D::FontFamily(parse_font_family(input)?)]),
        "font-weight" => Ok(vec![D::FontWeight(parse_font_weight(input)?)]),
        "font-style" => Ok(vec![D::FontStyle(parse_font_style(input)?)]),
        "color" => Ok(vec![D::Color(parse_color(input)?)]),
        "background-color" => Ok(vec![D::BackgroundColor(parse_color(input)?)]),
        "background-image" => Ok(vec![D::BackgroundImage(parse_background_image(input)?)]),
        "background-position" => {
            Ok(vec![D::BackgroundPosition(parse_background_position(input)?)])
        },
        "background-size" => Ok(vec![D::BackgroundSize(parse_background_size(input)?)]),
        "background-repeat" => Ok(vec![D::BackgroundRepeat(parse_background_repeat(input)?)]),
        "background-attachment" => {
            Ok(vec![D::BackgroundAttachment(parse_background_attachment(input)?)])
        },
        "background" => parse_background_shorthand(input),
        "text-decoration" | "text-decoration-line" => {
            Ok(vec![D::TextDecorationLine(parse_text_decoration_line(input)?)])
        },
        "content" => Ok(vec![D::Content(parse_content(input)?)]),
        // `page-break-*` are the previous generation of property names (accepted as aliases
        // for `break-*`, to lower the cost of migrating from wkhtmltopdf/wicked_pdf).
        "break-before" | "page-break-before" => {
            Ok(vec![D::BreakBefore(parse_break_between(input)?)])
        },
        "break-after" | "page-break-after" => {
            Ok(vec![D::BreakAfter(parse_break_between(input)?)])
        },
        "break-inside" | "page-break-inside" => {
            Ok(vec![D::BreakInside(parse_break_inside(input)?)])
        },
        "orphans" => Ok(vec![D::Orphans(parse_positive_integer(input)?)]),
        "widows" => Ok(vec![D::Widows(parse_positive_integer(input)?)]),
        "float" => Ok(vec![D::Float(parse_float(input)?)]),
        "clear" => Ok(vec![D::Clear(parse_clear(input)?)]),
        "position" => Ok(vec![D::Position(parse_position(input)?)]),
        "top" => Ok(vec![D::Top(parse_length_percentage_or_auto(input)?)]),
        "right" => Ok(vec![D::Right(parse_length_percentage_or_auto(input)?)]),
        "bottom" => Ok(vec![D::Bottom(parse_length_percentage_or_auto(input)?)]),
        "left" => Ok(vec![D::Left(parse_length_percentage_or_auto(input)?)]),
        "text-align" => Ok(vec![D::TextAlign(parse_text_align(input)?)]),
        "line-height" => Ok(vec![D::LineHeight(parse_line_height(input)?)]),
        "text-indent" => Ok(vec![D::TextIndent(parse_length_percentage(input)?)]),
        "white-space" => Ok(vec![D::WhiteSpace(parse_white_space(input)?)]),
        "letter-spacing" => Ok(vec![D::LetterSpacing(parse_spacing(input)?)]),
        "word-spacing" => Ok(vec![D::WordSpacing(parse_spacing(input)?)]),
        "text-transform" => Ok(vec![D::TextTransform(parse_text_transform(input)?)]),
        "border-collapse" => Ok(vec![D::BorderCollapse(parse_border_collapse(input)?)]),
        "border-spacing" => {
            let (h, v) = parse_border_spacing(input)?;
            Ok(vec![D::BorderSpacing(h, v)])
        },
        "caption-side" => Ok(vec![D::CaptionSide(parse_caption_side(input)?)]),
        "table-layout" => Ok(vec![D::TableLayout(parse_table_layout(input)?)]),
        "empty-cells" => Ok(vec![D::EmptyCells(parse_empty_cells(input)?)]),
        "vertical-align" => Ok(vec![D::VerticalAlign(parse_vertical_align(input)?)]),
        "list-style-type" => Ok(vec![D::ListStyleType(parse_list_style_type(input)?)]),
        "list-style-position" => {
            Ok(vec![D::ListStylePosition(parse_list_style_position(input)?)])
        },
        "list-style-image" => Ok(vec![D::ListStyleImage(parse_list_style_image(input)?)]),
        "list-style" => parse_list_style_shorthand(input),
        "overflow" => Ok(vec![D::Overflow(parse_overflow(input)?)]),
        "box-sizing" => Ok(vec![D::BoxSizing(parse_box_sizing(input)?)]),
        "z-index" => Ok(vec![D::ZIndex(parse_z_index(input)?)]),
        "visibility" => Ok(vec![D::Visibility(parse_visibility(input)?)]),
        "outline-width" => Ok(vec![D::OutlineWidth(parse_length(input)?)]),
        "outline-style" => Ok(vec![D::OutlineStyle(parse_border_style_keyword(input)?)]),
        "outline-color" => Ok(vec![D::OutlineColor(parse_color(input)?)]),
        "outline" => parse_outline_shorthand(input),
        "counter-reset" => Ok(vec![D::CounterReset(parse_counter_list(input, 0)?)]),
        "counter-increment" => Ok(vec![D::CounterIncrement(parse_counter_list(input, 1)?)]),
        "quotes" => Ok(vec![D::Quotes(parse_quotes(input)?)]),
        "object-fit" => Ok(vec![D::ObjectFit(parse_object_fit(input)?)]),
        "object-position" => Ok(vec![D::ObjectPosition(parse_background_position(input)?)]),
        "box-shadow" => Ok(vec![D::BoxShadow(parse_box_shadow(input)?)]),
        "flex-direction" => Ok(vec![D::FlexDirection(parse_flex_direction(input)?)]),
        "flex-wrap" => Ok(vec![D::FlexWrap(parse_flex_wrap(input)?)]),
        "justify-content" => Ok(vec![D::JustifyContent(parse_justify_content(input)?)]),
        "align-items" => Ok(vec![D::AlignItems(parse_align_items(input)?)]),
        "align-content" => Ok(vec![D::AlignContent(parse_align_content(input)?)]),
        "align-self" => Ok(vec![D::AlignSelf(parse_align_self(input)?)]),
        "flex-grow" => Ok(vec![D::FlexGrow(parse_non_negative_number(input)?)]),
        "flex-shrink" => Ok(vec![D::FlexShrink(parse_non_negative_number(input)?)]),
        "flex-basis" => Ok(vec![D::FlexBasis(parse_flex_basis(input)?)]),
        "flex" => parse_flex_shorthand(input),
        "row-gap" => Ok(vec![D::RowGap(parse_length_percentage(input)?)]),
        "column-gap" => Ok(vec![D::ColumnGap(parse_length_percentage(input)?)]),
        "gap" => parse_gap_shorthand(input),
        "transform" => Ok(vec![D::Transform(parse_transform(input)?)]),
        "transform-origin" => Ok(vec![D::TransformOrigin(parse_background_position(input)?)]),
        "opacity" => Ok(vec![D::Opacity(parse_opacity(input)?)]),
        "text-shadow" => Ok(vec![D::TextShadow(parse_text_shadow(input)?)]),
        "text-overflow" => Ok(vec![D::TextOverflow(parse_text_overflow(input)?)]),
        "word-break" => Ok(vec![D::WordBreak(parse_word_break(input)?)]),
        // `word-wrap` is the legacy name for `overflow-wrap` (handled like `page-break-*`).
        "overflow-wrap" | "word-wrap" => {
            Ok(vec![D::OverflowWrap(parse_overflow_wrap(input)?)])
        },
        "hyphens" => Ok(vec![D::Hyphens(parse_hyphens(input)?)]),
        "text-emphasis-style" => {
            Ok(vec![D::TextEmphasisStyle(parse_text_emphasis_style(input)?)])
        },
        "text-emphasis-color" => Ok(vec![D::TextEmphasisColor(parse_color(input)?)]),
        "text-emphasis-position" => {
            Ok(vec![D::TextEmphasisPosition(parse_text_emphasis_position(input)?)])
        },
        "text-emphasis" => parse_text_emphasis_shorthand(input),
        "grid-template-columns" => {
            Ok(vec![D::GridTemplateColumns(parse_track_list(input)?)])
        },
        "grid-template-rows" => Ok(vec![D::GridTemplateRows(parse_track_list(input)?)]),
        "grid-auto-columns" => Ok(vec![D::GridAutoColumns(parse_auto_track_list(input)?)]),
        "grid-auto-rows" => Ok(vec![D::GridAutoRows(parse_auto_track_list(input)?)]),
        "grid-auto-flow" => Ok(vec![D::GridAutoFlow(parse_grid_auto_flow(input)?)]),
        "grid-template-areas" => {
            Ok(vec![D::GridTemplateAreas(parse_grid_template_areas(input)?)])
        },
        "grid-row-start" => Ok(vec![D::GridRowStart(parse_grid_line(input)?)]),
        "grid-row-end" => Ok(vec![D::GridRowEnd(parse_grid_line(input)?)]),
        "grid-column-start" => Ok(vec![D::GridColumnStart(parse_grid_line(input)?)]),
        "grid-column-end" => Ok(vec![D::GridColumnEnd(parse_grid_line(input)?)]),
        "grid-row" => parse_grid_row_shorthand(input),
        "grid-column" => parse_grid_column_shorthand(input),
        "grid-area" => parse_grid_area_shorthand(input),
        "justify-items" => Ok(vec![D::JustifyItems(parse_align_items(input)?)]),
        "justify-self" => Ok(vec![D::JustifySelf(parse_align_self(input)?)]),
        // Logical properties. The only writing mode supported is `horizontal-tb` plus LTR,
        // so they are expanded through a fixed mapping to the physical properties
        // (`inline-start` = left, `inline-end` = right, `block-start` = top,
        // `block-end` = bottom). `writing-mode`/`direction` are not supported, so it never changes.
        "margin-inline-start" => Ok(vec![D::MarginLeft(parse_length_percentage_or_auto(input)?)]),
        "margin-inline-end" => Ok(vec![D::MarginRight(parse_length_percentage_or_auto(input)?)]),
        "margin-block-start" => Ok(vec![D::MarginTop(parse_length_percentage_or_auto(input)?)]),
        "margin-block-end" => Ok(vec![D::MarginBottom(parse_length_percentage_or_auto(input)?)]),
        "margin-inline" => {
            parse_start_end(input, parse_length_percentage_or_auto, D::MarginLeft, D::MarginRight)
        },
        "margin-block" => {
            parse_start_end(input, parse_length_percentage_or_auto, D::MarginTop, D::MarginBottom)
        },
        "padding-inline-start" => Ok(vec![D::PaddingLeft(parse_non_negative_length_percentage(input)?)]),
        "padding-inline-end" => Ok(vec![D::PaddingRight(parse_non_negative_length_percentage(input)?)]),
        "padding-block-start" => Ok(vec![D::PaddingTop(parse_non_negative_length_percentage(input)?)]),
        "padding-block-end" => Ok(vec![D::PaddingBottom(parse_non_negative_length_percentage(input)?)]),
        "padding-inline" => {
            parse_start_end(
                input,
                parse_non_negative_length_percentage,
                D::PaddingLeft,
                D::PaddingRight,
            )
        },
        "padding-block" => {
            parse_start_end(
                input,
                parse_non_negative_length_percentage,
                D::PaddingTop,
                D::PaddingBottom,
            )
        },
        "inset" => parse_inset_shorthand(input),
        "inset-inline-start" => Ok(vec![D::Left(parse_length_percentage_or_auto(input)?)]),
        "inset-inline-end" => Ok(vec![D::Right(parse_length_percentage_or_auto(input)?)]),
        "inset-block-start" => Ok(vec![D::Top(parse_length_percentage_or_auto(input)?)]),
        "inset-block-end" => Ok(vec![D::Bottom(parse_length_percentage_or_auto(input)?)]),
        "inset-inline" => {
            parse_start_end(input, parse_length_percentage_or_auto, D::Left, D::Right)
        },
        "inset-block" => {
            parse_start_end(input, parse_length_percentage_or_auto, D::Top, D::Bottom)
        },
        "border-inline-start" => parse_border_left_shorthand(input),
        "border-inline-end" => parse_border_right_shorthand(input),
        "border-block-start" => parse_border_top_shorthand(input),
        "border-block-end" => parse_border_bottom_shorthand(input),
        "border-inline" => {
            let mut decls = parse_border_left_shorthand(input)?;
            decls.extend(mirror_border_side(&decls, Side::Right));
            Ok(decls)
        },
        "border-block" => {
            let mut decls = parse_border_top_shorthand(input)?;
            decls.extend(mirror_border_side(&decls, Side::Bottom));
            Ok(decls)
        },
        "border-inline-start-width" => Ok(vec![D::BorderLeftWidth(parse_length(input)?)]),
        "border-inline-end-width" => Ok(vec![D::BorderRightWidth(parse_length(input)?)]),
        "border-block-start-width" => Ok(vec![D::BorderTopWidth(parse_length(input)?)]),
        "border-block-end-width" => Ok(vec![D::BorderBottomWidth(parse_length(input)?)]),
        "border-inline-width" => {
            parse_start_end(input, parse_length, D::BorderLeftWidth, D::BorderRightWidth)
        },
        "border-block-width" => {
            parse_start_end(input, parse_length, D::BorderTopWidth, D::BorderBottomWidth)
        },
        "border-inline-start-style" => {
            Ok(vec![D::BorderLeftStyle(parse_border_style_keyword(input)?)])
        },
        "border-inline-end-style" => {
            Ok(vec![D::BorderRightStyle(parse_border_style_keyword(input)?)])
        },
        "border-block-start-style" => {
            Ok(vec![D::BorderTopStyle(parse_border_style_keyword(input)?)])
        },
        "border-block-end-style" => {
            Ok(vec![D::BorderBottomStyle(parse_border_style_keyword(input)?)])
        },
        "border-inline-style" => {
            parse_start_end(input, parse_border_style_keyword, D::BorderLeftStyle, D::BorderRightStyle)
        },
        "border-block-style" => {
            parse_start_end(input, parse_border_style_keyword, D::BorderTopStyle, D::BorderBottomStyle)
        },
        "border-inline-start-color" => Ok(vec![D::BorderLeftColor(parse_color(input)?)]),
        "border-inline-end-color" => Ok(vec![D::BorderRightColor(parse_color(input)?)]),
        "border-block-start-color" => Ok(vec![D::BorderTopColor(parse_color(input)?)]),
        "border-block-end-color" => Ok(vec![D::BorderBottomColor(parse_color(input)?)]),
        "border-inline-color" => {
            parse_start_end(input, parse_color, D::BorderLeftColor, D::BorderRightColor)
        },
        "border-block-color" => {
            parse_start_end(input, parse_color, D::BorderTopColor, D::BorderBottomColor)
        },
        // The logical corner radii. The first names the block direction and the second the
        // inline direction (`border-start-end-radius` is the inline end of the top edge: top right).
        "border-start-start-radius" => {
            Ok(vec![D::BorderTopLeftRadius(parse_corner_radius(input)?)])
        },
        "border-start-end-radius" => {
            Ok(vec![D::BorderTopRightRadius(parse_corner_radius(input)?)])
        },
        "border-end-start-radius" => {
            Ok(vec![D::BorderBottomLeftRadius(parse_corner_radius(input)?)])
        },
        "border-end-end-radius" => {
            Ok(vec![D::BorderBottomRightRadius(parse_corner_radius(input)?)])
        },
        _ => Err(input.new_custom_error(())),
    }
}

fn parse_margin_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (top, right, bottom, left) = parse_four_sides(input, parse_length_percentage_or_auto)?;
    Ok(vec![
        D::MarginTop(top),
        D::MarginRight(right),
        D::MarginBottom(bottom),
        D::MarginLeft(left),
    ])
}

fn parse_padding_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (top, right, bottom, left) = parse_four_sides(input, parse_non_negative_length_percentage)?;
    Ok(vec![
        D::PaddingTop(top),
        D::PaddingRight(right),
        D::PaddingBottom(bottom),
        D::PaddingLeft(left),
    ])
}

/// The `inset` shorthand. The same 1-to-4-value expansion rule as `margin` (top/right/bottom/left).
fn parse_inset_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (top, right, bottom, left) = parse_four_sides(input, parse_length_percentage_or_auto)?;
    Ok(vec![
        D::Top(top),
        D::Right(right),
        D::Bottom(bottom),
        D::Left(left),
    ])
}

/// The two-edge shorthands such as `margin-inline`/`padding-block`/`inset-inline`/
/// `border-block-width`. One value applies to both edges; two values are in `start end`
/// order (as the CSS Logical Properties spec requires). It takes the constructors of the
/// physical longhands corresponding to `start`/`end` and expands through them.
fn parse_start_end<'i, T: Copy>(
    input: &mut Parser<'i, '_>,
    mut parse_one: impl FnMut(&mut Parser<'i, '_>) -> Result<T, ParseError<'i, ()>>,
    start: fn(T) -> PropertyDeclaration,
    end: fn(T) -> PropertyDeclaration,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    let first = parse_one(input)?;
    let second = input.try_parse(&mut parse_one).unwrap_or(first);
    Ok(vec![start(first), end(second)])
}

/// The edge [`mirror_border_side`] copies onto.
#[derive(Clone, Copy)]
enum Side {
    Right,
    Bottom,
}

/// For `border-inline`/`border-block`. Copies the per-edge shorthand expansion of one side
/// (left/top) onto the other (right/bottom).
///
/// Declarations that cannot be copied are dropped. Per-edge shorthands only return width,
/// style and colour, so that does not currently happen, but passing them through unchanged
/// would emit the left (top) declaration twice, so nothing is passed through blindly.
fn mirror_border_side(decls: &[PropertyDeclaration], to: Side) -> Vec<PropertyDeclaration> {
    use PropertyDeclaration as D;
    decls
        .iter()
        .filter_map(|d| {
            Some(match (d, to) {
                (D::BorderLeftWidth(w), Side::Right) => D::BorderRightWidth(*w),
                (D::BorderLeftStyle(s), Side::Right) => D::BorderRightStyle(*s),
                (D::BorderLeftColor(c), Side::Right) => D::BorderRightColor(*c),
                (D::BorderTopWidth(w), Side::Bottom) => D::BorderBottomWidth(*w),
                (D::BorderTopStyle(s), Side::Bottom) => D::BorderBottomStyle(*s),
                (D::BorderTopColor(c), Side::Bottom) => D::BorderBottomColor(*c),
                _ => return None,
            })
        })
        .collect()
}

/// The CSS 1-to-4-value shorthand expansion rule (top/right/bottom/left).
fn parse_four_sides<'i, T: Copy>(
    input: &mut Parser<'i, '_>,
    mut parse_one: impl FnMut(&mut Parser<'i, '_>) -> Result<T, ParseError<'i, ()>>,
) -> Result<(T, T, T, T), ParseError<'i, ()>> {
    let top = parse_one(input)?;
    let Ok(right) = input.try_parse(&mut parse_one) else {
        return Ok((top, top, top, top));
    };
    let Ok(bottom) = input.try_parse(&mut parse_one) else {
        return Ok((top, right, top, right));
    };
    let Ok(left) = input.try_parse(&mut parse_one) else {
        return Ok((top, right, bottom, right));
    };
    Ok((top, right, bottom, left))
}

/// The parse shared by `border`/`border-top`/`border-right`/`border-bottom`/`border-left`:
/// `<width>`/`<style>`/`<color>` in any order, any of them omitted (as the CSS spec says).
fn parse_border_edge_values<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<(SpecifiedLength, BorderStyle, Option<Color>), ParseError<'i, ()>> {
    let mut width = SpecifiedLength::Px(0.0);
    let mut style = BorderStyle::None;
    let mut color = None;

    loop {
        if let Ok(w) = input.try_parse(parse_length) {
            width = w;
            continue;
        }
        if let Ok(s) = input.try_parse(parse_border_style_keyword) {
            style = s;
            continue;
        }
        if let Ok(c) = input.try_parse(parse_color) {
            color = Some(c);
            continue;
        }
        break;
    }
    Ok((width, style, color))
}

/// A simple implementation of the `border` shorthand. `border-width`/`border-style`/
/// `border-color` may be given together in any order (as the CSS `border` shorthand spec
/// says). All of them apply the same value to all four edges (to vary them per edge, use a
/// per-edge shorthand such as `border-top`, or a longhand such as `border-top-width`).
/// When `border-color` is omitted no declaration is generated (the computed style treats it
/// as the initial value `currentcolor`).
fn parse_border_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (width, style, color) = parse_border_edge_values(input)?;

    let mut decls = vec![
        D::BorderTopWidth(width),
        D::BorderRightWidth(width),
        D::BorderBottomWidth(width),
        D::BorderLeftWidth(width),
        D::BorderTopStyle(style),
        D::BorderRightStyle(style),
        D::BorderBottomStyle(style),
        D::BorderLeftStyle(style),
    ];
    if let Some(c) = color {
        decls.extend([
            D::BorderTopColor(c),
            D::BorderRightColor(c),
            D::BorderBottomColor(c),
            D::BorderLeftColor(c),
        ]);
    }
    Ok(decls)
}

/// The per-edge shorthands `border-top`/`border-right`/`border-bottom`/`border-left`.
/// The same value grammar as the `border` shorthand (`<width>`/`<style>`/`<color>`, any
/// order, any omitted), but applying only to the one edge named.
fn parse_border_top_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (width, style, color) = parse_border_edge_values(input)?;
    let mut decls = vec![D::BorderTopWidth(width), D::BorderTopStyle(style)];
    if let Some(c) = color {
        decls.push(D::BorderTopColor(c));
    }
    Ok(decls)
}

fn parse_border_right_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (width, style, color) = parse_border_edge_values(input)?;
    let mut decls = vec![D::BorderRightWidth(width), D::BorderRightStyle(style)];
    if let Some(c) = color {
        decls.push(D::BorderRightColor(c));
    }
    Ok(decls)
}

fn parse_border_bottom_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (width, style, color) = parse_border_edge_values(input)?;
    let mut decls = vec![D::BorderBottomWidth(width), D::BorderBottomStyle(style)];
    if let Some(c) = color {
        decls.push(D::BorderBottomColor(c));
    }
    Ok(decls)
}

fn parse_border_left_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (width, style, color) = parse_border_edge_values(input)?;
    let mut decls = vec![D::BorderLeftWidth(width), D::BorderLeftStyle(style)];
    if let Some(c) = color {
        decls.push(D::BorderLeftColor(c));
    }
    Ok(decls)
}

fn parse_border_width_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (top, right, bottom, left) = parse_four_sides(input, parse_length)?;
    Ok(vec![
        D::BorderTopWidth(top),
        D::BorderRightWidth(right),
        D::BorderBottomWidth(bottom),
        D::BorderLeftWidth(left),
    ])
}

fn parse_border_color_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (top, right, bottom, left) = parse_four_sides(input, parse_color)?;
    Ok(vec![
        D::BorderTopColor(top),
        D::BorderRightColor(right),
        D::BorderBottomColor(bottom),
        D::BorderLeftColor(left),
    ])
}

fn parse_border_style_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (top, right, bottom, left) = parse_four_sides(input, parse_border_style_keyword)?;
    Ok(vec![
        D::BorderTopStyle(top),
        D::BorderRightStyle(right),
        D::BorderBottomStyle(bottom),
        D::BorderLeftStyle(left),
    ])
}

/// A simple implementation of the `border-radius` shorthand. CSS corner radii go
/// top-left, top-right, bottom-right, bottom-left rather than top, right, bottom, left even
/// in the four-value expansion, but `parse_four_sides` is a generic helper concerned only
/// with the expansion rule for the number of values given (1 to 4) and takes no part in
/// what each slot means, so it can be reused unchanged.
///
/// The elliptical syntax, which gives separate horizontal and vertical radii either side of
/// a `/`, is supported.
///
/// Each corner ends up as a (horizontal, vertical) pair; a true circle has the two equal.
fn parse_border_radius_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (h_tl, h_tr, h_br, h_bl) = parse_four_sides(input, parse_length)?;
    let vertical = if input.try_parse(|input| input.expect_delim('/')).is_ok() {
        Some(parse_four_sides(input, parse_length)?)
    } else {
        None
    };
    let (v_tl, v_tr, v_br, v_bl) = vertical.unwrap_or((h_tl, h_tr, h_br, h_bl));

    Ok(vec![
        D::BorderTopLeftRadius(SpecifiedCornerRadius {
            horizontal: h_tl,
            vertical: v_tl,
        }),
        D::BorderTopRightRadius(SpecifiedCornerRadius {
            horizontal: h_tr,
            vertical: v_tr,
        }),
        D::BorderBottomRightRadius(SpecifiedCornerRadius {
            horizontal: h_br,
            vertical: v_br,
        }),
        D::BorderBottomLeftRadius(SpecifiedCornerRadius {
            horizontal: h_bl,
            vertical: v_bl,
        }),
    ])
}

/// Longhands such as `border-top-left-radius`. Accepts `<length>{1,2}` (horizontal then
/// vertical; when omitted the vertical equals the horizontal, giving a true circle), the same pattern as `border-spacing`.
fn parse_corner_radius<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedCornerRadius, ParseError<'i, ()>> {
    let horizontal = parse_length(input)?;
    let vertical = input.try_parse(parse_length).unwrap_or(horizontal);
    Ok(SpecifiedCornerRadius {
        horizontal,
        vertical,
    })
}

/// The keywords shared by `border-style`/`outline-style`. `groove`/`ridge`/`inset`/
/// `outset` (deriving a two-tone pseudo-3D shading from border-color) are supported too.
fn parse_border_style_keyword<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BorderStyle, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "none" | "hidden" => BorderStyle::None,
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        "groove" => BorderStyle::Groove,
        "ridge" => BorderStyle::Ridge,
        "inset" => BorderStyle::Inset,
        "outset" => BorderStyle::Outset,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `overflow`. `hidden`/`scroll`/`auto` all get the same clipping.
fn parse_overflow<'i>(input: &mut Parser<'i, '_>) -> Result<Overflow, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "visible" => Overflow::Visible,
        "hidden" => Overflow::Hidden,
        "scroll" => Overflow::Scroll,
        "auto" => Overflow::Auto,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `box-sizing`. `padding-box` (non-standard) is not supported.
fn parse_box_sizing<'i>(input: &mut Parser<'i, '_>) -> Result<BoxSizing, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "content-box" => BoxSizing::ContentBox,
        "border-box" => BoxSizing::BorderBox,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `z-index`. `auto | <integer>`.
fn parse_z_index<'i>(input: &mut Parser<'i, '_>) -> Result<ZIndex, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(ZIndex::Auto);
    }
    Ok(ZIndex::Value(input.expect_integer()?))
}

/// `visibility`. `collapse` is treated as `hidden`.
fn parse_visibility<'i>(input: &mut Parser<'i, '_>) -> Result<Visibility, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "visible" => Visibility::Visible,
        "hidden" => Visibility::Hidden,
        "collapse" => Visibility::Collapse,
        _ => return Err(input.new_custom_error(())),
    })
}

/// A simple implementation of the `outline` shorthand. `outline-width`/`outline-style`/
/// `outline-color` are accepted in any order with any of them omitted (the same pattern as
/// the `border` shorthand). `outline-offset` is not supported and is always 0.
fn parse_outline_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let mut width = SpecifiedLength::Px(0.0);
    let mut style = BorderStyle::None;
    let mut color = None;

    loop {
        if let Ok(w) = input.try_parse(parse_length) {
            width = w;
            continue;
        }
        if let Ok(s) = input.try_parse(parse_border_style_keyword) {
            style = s;
            continue;
        }
        if let Ok(c) = input.try_parse(parse_color) {
            color = Some(c);
            continue;
        }
        break;
    }

    let mut decls = vec![D::OutlineWidth(width), D::OutlineStyle(style)];
    if let Some(c) = color {
        decls.push(D::OutlineColor(c));
    }
    Ok(decls)
}

fn parse_text_align<'i>(input: &mut Parser<'i, '_>) -> Result<TextAlign, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "left" => TextAlign::Left,
        "right" => TextAlign::Right,
        "center" => TextAlign::Center,
        "justify" => TextAlign::Justify,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_white_space<'i>(input: &mut Parser<'i, '_>) -> Result<WhiteSpace, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "normal" => WhiteSpace::Normal,
        "nowrap" => WhiteSpace::Nowrap,
        "pre" => WhiteSpace::Pre,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_text_transform<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<TextTransform, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "none" => TextTransform::None,
        "uppercase" => TextTransform::Uppercase,
        "lowercase" => TextTransform::Lowercase,
        "capitalize" => TextTransform::Capitalize,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `line-height`. `normal | <number> | <length> | <percentage>`.
fn parse_line_height<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedLineHeight, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("normal"))
        .is_ok()
    {
        return Ok(SpecifiedLineHeight::Normal);
    }
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(SpecifiedLineHeight::Number(value)),
        Token::Percentage { unit_value, .. } => Ok(SpecifiedLineHeight::Percentage(unit_value)),
        Token::Dimension {
            value, ref unit, ..
        } => Ok(SpecifiedLineHeight::Length(parse_length_unit(
            input, value, unit,
        )?)),
        _ => Err(input.new_custom_error(())),
    }
}

/// Shared by `letter-spacing`/`word-spacing`. `normal | <length>`.
fn parse_spacing<'i>(input: &mut Parser<'i, '_>) -> Result<SpecifiedSpacing, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("normal"))
        .is_ok()
    {
        return Ok(SpecifiedSpacing::Normal);
    }
    Ok(SpecifiedSpacing::Length(parse_length(input)?))
}

fn parse_border_collapse<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BorderCollapse, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "separate" => BorderCollapse::Separate,
        "collapse" => BorderCollapse::Collapse,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `border-spacing`. `<length>` (the same value horizontally and vertically) or
/// `<length> <length>` (horizontal, vertical).
fn parse_border_spacing<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<(SpecifiedLength, SpecifiedLength), ParseError<'i, ()>> {
    let horizontal = parse_length(input)?;
    let vertical = input.try_parse(parse_length).unwrap_or(horizontal);
    Ok((horizontal, vertical))
}

fn parse_caption_side<'i>(input: &mut Parser<'i, '_>) -> Result<CaptionSide, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "top" => CaptionSide::Top,
        "bottom" => CaptionSide::Bottom,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_table_layout<'i>(input: &mut Parser<'i, '_>) -> Result<TableLayout, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "auto" => TableLayout::Auto,
        "fixed" => TableLayout::Fixed,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_empty_cells<'i>(input: &mut Parser<'i, '_>) -> Result<EmptyCells, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "show" => EmptyCells::Show,
        "hide" => EmptyCells::Hide,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `vertical-align`. Accepts either a keyword or a length/percentage.
///
/// The inline-context values (`sub`/`super`/`text-top`/`text-bottom`/`<length>`/
/// `<percentage>`) are handled here too; a table cell keeps only the CSS2.1 subset.
fn parse_vertical_align<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedVerticalAlign, ParseError<'i, ()>> {
    if let Ok(ident) = input.try_parse(|i| i.expect_ident_cloned()) {
        return Ok(match_ignore_ascii_case! { &ident,
            "top" => SpecifiedVerticalAlign::Top,
            "middle" => SpecifiedVerticalAlign::Middle,
            "bottom" => SpecifiedVerticalAlign::Bottom,
            "baseline" => SpecifiedVerticalAlign::Baseline,
            "sub" => SpecifiedVerticalAlign::Sub,
            "super" => SpecifiedVerticalAlign::Super,
            "text-top" => SpecifiedVerticalAlign::TextTop,
            "text-bottom" => SpecifiedVerticalAlign::TextBottom,
            _ => return Err(input.new_custom_error(())),
        });
    }
    Ok(SpecifiedVerticalAlign::LengthPercentage(
        parse_length_percentage(input)?,
    ))
}

fn parse_display<'i>(input: &mut Parser<'i, '_>) -> Result<Display, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "block" => Display::Block,
        "inline" => Display::Inline,
        "inline-block" => Display::InlineBlock,
        "table" => Display::Table,
        "table-row" => Display::TableRow,
        "table-cell" => Display::TableCell,
        "table-caption" => Display::TableCaption,
        "list-item" => Display::ListItem,
        "flex" => Display::Flex,
        // `inline-grid` is not supported (the same known simplification as `inline-flex`).
        "grid" => Display::Grid,
        "none" => Display::None,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_list_style_type<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ListStyleType, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "disc" => ListStyleType::Disc,
        "circle" => ListStyleType::Circle,
        "square" => ListStyleType::Square,
        "decimal" => ListStyleType::Decimal,
        "decimal-leading-zero" => ListStyleType::DecimalLeadingZero,
        "lower-roman" => ListStyleType::LowerRoman,
        "upper-roman" => ListStyleType::UpperRoman,
        "lower-alpha" | "lower-latin" => ListStyleType::LowerAlpha,
        "upper-alpha" | "upper-latin" => ListStyleType::UpperAlpha,
        "none" => ListStyleType::None,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_list_style_position<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ListStylePosition, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "outside" => ListStylePosition::Outside,
        "inside" => ListStylePosition::Inside,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `list-style-image`. The same `url(...) | none` form as `background-image`.
/// In practice it always falls back to `list-style-type` and is never used for drawing.
fn parse_list_style_image<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Option<String>, ParseError<'i, ()>> {
    parse_background_image(input)
}

/// A simple implementation of the `list-style` shorthand.
/// `list-style-type`/`list-style-position`/`list-style-image` are accepted in any order
/// with any of them omitted (the same pattern as the `border` shorthand). `none` is a valid
/// value for both `list-style-type` and `list-style-image`, so it fills whichever slot is
/// not yet decided, in order (`type: none` if `type` is undecided, `image: none` if it is
/// already decided). That resolves both
/// `list-style: square none` (type=square, image=none) and
/// `list-style: none` (both none) correctly.
fn parse_list_style_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let mut ty = None;
    let mut position = None;
    let mut image = None;

    loop {
        if position.is_none() {
            if let Ok(p) = input.try_parse(parse_list_style_position) {
                position = Some(p);
                continue;
            }
        }
        if ty.is_none() {
            if let Ok(t) = input.try_parse(parse_list_style_type) {
                ty = Some(t);
                continue;
            }
        }
        if image.is_none() {
            if let Ok(img) = input.try_parse(parse_list_style_image) {
                image = Some(img);
                continue;
            }
        }
        break;
    }

    let mut decls = Vec::new();
    if let Some(p) = position {
        decls.push(D::ListStylePosition(p));
    }
    decls.push(D::ListStyleType(ty.unwrap_or_default()));
    decls.push(D::ListStyleImage(image.unwrap_or(None)));
    Ok(decls)
}

fn parse_float<'i>(input: &mut Parser<'i, '_>) -> Result<Float, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "none" => Float::None,
        "left" => Float::Left,
        "right" => Float::Right,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_clear<'i>(input: &mut Parser<'i, '_>) -> Result<Clear, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "none" => Clear::None,
        "left" => Clear::Left,
        "right" => Clear::Right,
        "both" => Clear::Both,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `position`. `absolute`/`fixed` are made a parse error as known unsupported keywords
/// (the same pattern as groove/ridge in `border-style`: the declaration is ignored without
/// affecting any other).
fn parse_position<'i>(input: &mut Parser<'i, '_>) -> Result<Position, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "static" => Position::Static,
        "relative" => Position::Relative,
        "absolute" => Position::Absolute,
        "fixed" => Position::Fixed,
        _ => return Err(input.new_custom_error(())),
    })
}

/// Values of `break-before`/`break-after` (and the `page-break-before`/`-after` aliases).
/// `left`/`right`/`recto`/`verso` (spread control) are unsupported, a single page size being assumed.
fn parse_break_between<'i>(input: &mut Parser<'i, '_>) -> Result<BreakBetween, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "auto" => BreakBetween::Auto,
        "avoid" | "avoid-page" | "avoid-column" => BreakBetween::Avoid,
        "always" | "page" => BreakBetween::Always,
        _ => return Err(input.new_custom_error(())),
    })
}

/// Values of `break-inside` (and the `page-break-inside` alias).
fn parse_break_inside<'i>(input: &mut Parser<'i, '_>) -> Result<BreakInside, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "auto" => BreakInside::Auto,
        "avoid" | "avoid-page" | "avoid-column" => BreakInside::Avoid,
        _ => return Err(input.new_custom_error(())),
    })
}

/// Values of `orphans`/`widows`. Anything below 1 is invalid (the spec also allows only integers of 1 or more).
fn parse_positive_integer<'i>(input: &mut Parser<'i, '_>) -> Result<u32, ParseError<'i, ()>> {
    let value = input.expect_integer()?;
    if value < 1 {
        return Err(input.new_custom_error(()));
    }
    Ok(value as u32)
}

/// Accepts both keywords (`normal`/`bold`) and numbers (`100` to `900`).
/// A simplified implementation treats any number of 600 or more as `Bold` (we hold no real
/// bold font and render it as faux bold, so finer weight steps are not distinguished).
pub(crate) fn parse_font_weight<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<FontWeight, ParseError<'i, ()>> {
    if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        return Ok(match_ignore_ascii_case! { &ident,
            "normal" => FontWeight::Normal,
            "bold" => FontWeight::Bold,
            _ => return Err(input.new_custom_error(())),
        });
    }
    let value = input.expect_number()?;
    Ok(if value >= 600.0 {
        FontWeight::Bold
    } else {
        FontWeight::Normal
    })
}

pub(crate) fn parse_font_style<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<FontStyle, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "normal" => FontStyle::Normal,
        // `oblique` has no slant angle of its own, so it is treated as `italic`.
        "italic" | "oblique" => FontStyle::Italic,
        _ => return Err(input.new_custom_error(())),
    })
}

/// A simple implementation of `text-decoration`/`text-decoration-line`.
/// `underline`/`line-through` may be given together (`underline line-through`).
/// `overline`/`blink`, and the `text-decoration-style`/`text-decoration-color` parts of the `text-decoration` shorthand, are not supported.
fn parse_text_decoration_line<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<TextDecorationLine, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(TextDecorationLine::default());
    }

    let mut line = TextDecorationLine::default();
    loop {
        let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) else {
            break;
        };
        match_ignore_ascii_case! { &ident,
            "underline" => line.underline = true,
            "line-through" => line.line_through = true,
            _ => return Err(input.new_custom_error(())),
        }
    }
    Ok(line)
}

/// For properties that accept no negative value, such as `padding`. A negative value is
/// invalid in CSS, so it is made a parse error and the whole declaration is dropped.
/// `calc()` passes through here, its sign being unknown until resolved
/// (CSS also specifies clamping a negative result to 0).
fn parse_non_negative_length_percentage<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedLengthPercentage, ParseError<'i, ()>> {
    let value = parse_length_percentage(input)?;
    if value.is_negative() {
        return Err(input.new_custom_error(()));
    }
    Ok(value)
}

fn parse_length_percentage<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedLengthPercentage, ParseError<'i, ()>> {
    // `calc(...)`.
    if let Ok(calc) = input.try_parse(parse_calc) {
        return Ok(SpecifiedLengthPercentage::Calc(calc));
    }
    let token = input.next()?.clone();
    match token {
        Token::Percentage { unit_value, .. } => {
            Ok(SpecifiedLengthPercentage::Percentage(unit_value))
        }
        Token::Number { value: 0.0, .. } => {
            Ok(SpecifiedLengthPercentage::Length(SpecifiedLength::Px(0.0)))
        }
        Token::Dimension {
            value, ref unit, ..
        } => Ok(SpecifiedLengthPercentage::Length(parse_length_unit(
            input, value, unit,
        )?)),
        _ => Err(input.new_custom_error(())),
    }
}

/// An intermediate `calc` value. It holds a linear combination of the length dimensions
/// (px/em/rem), a percentage and a pure number.
#[derive(Debug, Clone, Copy, Default)]
struct CalcValue {
    px: f32,
    em: f32,
    rem: f32,
    /// The percentage as a ratio (50% = 0.5).
    percent: f32,
    /// A unitless number (the coefficient of a `* 2` or `/ 3`, or an invalid bare number).
    number: f32,
}

impl CalcValue {
    fn number(n: f32) -> Self {
        Self {
            number: n,
            ..Default::default()
        }
    }
    /// Whether it has no dimension or percentage component (that is, it is a pure number).
    fn is_pure_number(&self) -> bool {
        self.px == 0.0 && self.em == 0.0 && self.rem == 0.0 && self.percent == 0.0
    }
    fn add(self, other: Self) -> Self {
        Self {
            px: self.px + other.px,
            em: self.em + other.em,
            rem: self.rem + other.rem,
            percent: self.percent + other.percent,
            number: self.number + other.number,
        }
    }
    fn scale(self, factor: f32) -> Self {
        Self {
            px: self.px * factor,
            em: self.em * factor,
            rem: self.rem * factor,
            percent: self.percent * factor,
            number: self.number * factor,
        }
    }
}

/// The depth limit for accepting nested `calc()` and parentheses.
///
/// The parser is recursive descent, so depth translates directly into stack use (untrusted
/// CSS could otherwise overflow the stack). Real CSS such as `calc(calc(...) * calc(...))`
/// fits in a few levels, so anything past this deliberately generous value is treated as
/// invalid and the declaration is dropped.
/// The same idea as [`crate::html::MAX_ELEMENT_DEPTH`] for DOM depth.
const MAX_CALC_DEPTH: u32 = 32;

/// Parse `calc(...)` into [`SpecifiedCalc`]. An expression that leaves a bare number (which
/// is invalid as a length) is an error. `min`/`max`/`clamp` are not supported.
fn parse_calc<'i>(input: &mut Parser<'i, '_>) -> Result<SpecifiedCalc, ParseError<'i, ()>> {
    input.expect_function_matching("calc")?;
    let value = input.parse_nested_block(|input| parse_calc_sum(input, 1))?;
    if value.number != 0.0 {
        // A bare number left over, as in `calc(2)`, is invalid in a length context.
        return Err(input.new_custom_error(()));
    }
    Ok(SpecifiedCalc {
        px: value.px,
        em: value.em,
        rem: value.rem,
        percent: value.percent,
    })
}

fn parse_calc_sum<'i>(
    input: &mut Parser<'i, '_>,
    depth: u32,
) -> Result<CalcValue, ParseError<'i, ()>> {
    let mut acc = parse_calc_product(input, depth)?;
    loop {
        // Whitespace is required either side of `+`/`-` (per the CSS spec). cssparser makes
        // a signed number such as `+5` a single token, so we leave the loop when it is not a Delim.
        let sign = input.try_parse(|input| {
            let token = input.next()?.clone();
            match token {
                Token::Delim('+') => Ok(1.0),
                Token::Delim('-') => Ok(-1.0),
                _ => Err(input.new_custom_error::<(), ()>(())),
            }
        });
        match sign {
            Ok(sign) => {
                let rhs = parse_calc_product(input, depth)?;
                acc = acc.add(rhs.scale(sign));
            }
            Err(_) => return Ok(acc),
        }
    }
}

fn parse_calc_product<'i>(
    input: &mut Parser<'i, '_>,
    depth: u32,
) -> Result<CalcValue, ParseError<'i, ()>> {
    let mut acc = parse_calc_value(input, depth)?;
    loop {
        enum Op {
            Mul,
            Div,
        }
        let op = input.try_parse(|input| {
            let token = input.next()?.clone();
            match token {
                Token::Delim('*') => Ok(Op::Mul),
                Token::Delim('/') => Ok(Op::Div),
                _ => Err(input.new_custom_error::<(), ()>(())),
            }
        });
        match op {
            Ok(Op::Mul) => {
                let rhs = parse_calc_value(input, depth)?;
                // Dimension times dimension is not allowed (at least one side must be a pure number, per the CSS spec).
                if acc.is_pure_number() {
                    acc = rhs.scale(acc.number);
                } else if rhs.is_pure_number() {
                    acc = acc.scale(rhs.number);
                } else {
                    return Err(input.new_custom_error(()));
                }
            }
            Ok(Op::Div) => {
                let rhs = parse_calc_value(input, depth)?;
                if !rhs.is_pure_number() || rhs.number == 0.0 {
                    return Err(input.new_custom_error(()));
                }
                acc = acc.scale(1.0 / rhs.number);
            }
            Err(_) => return Ok(acc),
        }
    }
}

fn parse_calc_value<'i>(
    input: &mut Parser<'i, '_>,
    depth: u32,
) -> Result<CalcValue, ParseError<'i, ()>> {
    // Parentheses, or a nested `calc()` (CSS Values 4 treats both the same).
    // Tailwind v4's `space-y-*`/`divide-*` emit a nested `calc()` like
    // `calc(calc(var(--spacing) * N) * calc(1 - var(--tw-space-y-reverse)))`
    // (issue #17).
    if input
        .try_parse(|input| input.expect_parenthesis_block())
        .is_ok()
        || input
            .try_parse(|input| input.expect_function_matching("calc"))
            .is_ok()
    {
        if depth >= MAX_CALC_DEPTH {
            return Err(input.new_custom_error(()));
        }
        return input.parse_nested_block(|input| parse_calc_sum(input, depth + 1));
    }
    let token = input.next()?.clone();
    match token {
        Token::Number { value, .. } => Ok(CalcValue::number(value)),
        Token::Percentage { unit_value, .. } => Ok(CalcValue {
            percent: unit_value,
            ..Default::default()
        }),
        Token::Dimension {
            value, ref unit, ..
        } => {
            let mut v = CalcValue::default();
            if let Some(px_per_unit) = absolute_length_px(unit) {
                v.px = value * px_per_unit;
            } else if unit.eq_ignore_ascii_case("em") {
                v.em = value;
            } else if unit.eq_ignore_ascii_case("rem") {
                v.rem = value;
            } else {
                return Err(input.new_custom_error(()));
            }
            Ok(v)
        }
        _ => Err(input.new_custom_error(())),
    }
}

/// `aspect-ratio: auto || <ratio>`. `auto` and `<ratio>` may be given together in either
/// order. `<ratio>` is `<number> [ / <number> ]?` (the denominator defaults to 1), and a
/// degenerate ratio containing zero or a negative number is invalid, so the whole declaration is ignored (as the spec says).
fn parse_aspect_ratio<'i>(input: &mut Parser<'i, '_>) -> Result<AspectRatio, ParseError<'i, ()>> {
    let mut auto = false;
    let mut ratio = None;

    loop {
        if !auto
            && input
                .try_parse(|input| input.expect_ident_matching("auto"))
                .is_ok()
        {
            auto = true;
            continue;
        }
        if ratio.is_none() {
            if let Ok(r) = input.try_parse(parse_ratio) {
                ratio = Some(r);
                continue;
            }
        }
        break;
    }

    if !auto && ratio.is_none() {
        return Err(input.new_custom_error(()));
    }
    Ok(AspectRatio { auto, ratio })
}

/// `<ratio> = <number> [ / <number> ]?`. Returns the `width / height` ratio.
fn parse_ratio<'i>(input: &mut Parser<'i, '_>) -> Result<f32, ParseError<'i, ()>> {
    let width = input.expect_number()?;
    let height = input
        .try_parse(|input| {
            input.expect_delim('/')?;
            input.expect_number()
        })
        .unwrap_or(1.0);
    if width <= 0.0 || height <= 0.0 {
        return Err(input.new_custom_error(()));
    }
    Ok(width / height)
}

/// `max-width`/`max-height`. `none | <length-percentage>`.
/// `min-content`/`max-content`/`fit-content` are not supported.
fn parse_max_size<'i>(input: &mut Parser<'i, '_>) -> Result<SpecifiedMaxSize, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(SpecifiedMaxSize::None);
    }
    Ok(SpecifiedMaxSize::LengthPercentage(parse_length_percentage(
        input,
    )?))
}

fn parse_length_percentage_or_auto<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedLengthPercentageOrAuto, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(SpecifiedLengthPercentageOrAuto::Auto);
    }
    parse_length_percentage(input).map(SpecifiedLengthPercentageOrAuto::LengthPercentage)
}

pub(crate) fn parse_length<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedLength, ParseError<'i, ()>> {
    let token = input.next()?.clone();
    match token {
        Token::Number { value: 0.0, .. } => Ok(SpecifiedLength::Px(0.0)),
        Token::Dimension {
            value, ref unit, ..
        } => parse_length_unit(input, value, unit),
        _ => Err(input.new_custom_error(())),
    }
}

/// How many CSS px one absolute unit is. `None` for anything that is not an absolute unit other than `px`.
///
/// Every CSS absolute unit has a fixed ratio to px (1in = 96px), so they are folded into px
/// at parse time. That keeps units out of [`SpecifiedLength`] entirely and changes nothing
/// about computed value resolution or layout. Viewport units (`vh` and friends) are out of
/// scope, print having no concept of a viewport.
fn absolute_length_px(unit: &str) -> Option<f32> {
    const PX_PER_IN: f32 = 96.0;
    if unit.eq_ignore_ascii_case("px") {
        Some(1.0)
    } else if unit.eq_ignore_ascii_case("in") {
        Some(PX_PER_IN)
    } else if unit.eq_ignore_ascii_case("cm") {
        Some(PX_PER_IN / 2.54)
    } else if unit.eq_ignore_ascii_case("mm") {
        Some(PX_PER_IN / 25.4)
    } else if unit.eq_ignore_ascii_case("q") {
        // 1Q = 1/4mm.
        Some(PX_PER_IN / 25.4 / 4.0)
    } else if unit.eq_ignore_ascii_case("pt") {
        Some(PX_PER_IN / 72.0)
    } else if unit.eq_ignore_ascii_case("pc") {
        // 1pc = 12pt.
        Some(PX_PER_IN / 6.0)
    } else {
        None
    }
}

/// Read the unit part of `<number><unit>` as either an absolute unit
/// (`px`/`mm`/`cm`/`in`/`pt`/`pc`/`Q`) or a relative unit (`em`/`rem`).
/// Viewport units (`vh` and friends) are not supported.
fn parse_length_unit<'i>(
    input: &Parser<'i, '_>,
    value: f32,
    unit: &str,
) -> Result<SpecifiedLength, ParseError<'i, ()>> {
    if let Some(px_per_unit) = absolute_length_px(unit) {
        Ok(SpecifiedLength::Px(value * px_per_unit))
    } else if unit.eq_ignore_ascii_case("em") {
        Ok(SpecifiedLength::Em(value))
    } else if unit.eq_ignore_ascii_case("rem") {
        Ok(SpecifiedLength::Rem(value))
    } else {
        Err(input.new_custom_error(()))
    }
}

/// The nesting limit for `color-mix()`. A brake so an invalid depth cannot eat the stack
/// (the same idea as `MAX_IMPORT_DEPTH`).
const MAX_COLOR_MIX_DEPTH: u32 = 16;

/// `lab`/`lch`/`oklab`/`oklch` are converted with the `palette` crate, because
/// `cssparser-color` does not expose sRGB conversion functions for them.
fn parse_color<'i>(input: &mut Parser<'i, '_>) -> Result<Color, ParseError<'i, ()>> {
    parse_color_at_depth(input, 0)
}

fn parse_color_at_depth<'i>(
    input: &mut Parser<'i, '_>,
    depth: u32,
) -> Result<Color, ParseError<'i, ()>> {
    // `cssparser-color` does not handle `color-mix()` (it is left out pending better
    // `calc()` support), so we try our own first.
    if let Ok(mixed) = input.try_parse(|input| parse_color_mix(input, depth)) {
        return Ok(mixed);
    }
    let color = cssparser_color::Color::parse(input).map_err(|_| input.new_custom_error(()))?;
    match color {
        cssparser_color::Color::CurrentColor => Ok(Color::CurrentColor),
        cssparser_color::Color::Rgba(rgba) => Ok(Color::Rgba {
            red: rgba.red,
            green: rgba.green,
            blue: rgba.blue,
            alpha: rgba.alpha,
        }),
        cssparser_color::Color::Hsl(hsl) => {
            let (r, g, b) = cssparser_color::hsl_to_rgb(
                hsl.hue.unwrap_or(0.0) / 360.0,
                hsl.saturation.unwrap_or(0.0),
                hsl.lightness.unwrap_or(0.0),
            );
            Ok(rgba_from_unit_floats(r, g, b, hsl.alpha.unwrap_or(1.0)))
        }
        cssparser_color::Color::Hwb(hwb) => {
            let (r, g, b) = cssparser_color::hwb_to_rgb(
                hwb.hue.unwrap_or(0.0) / 360.0,
                hwb.whiteness.unwrap_or(0.0),
                hwb.blackness.unwrap_or(0.0),
            );
            Ok(rgba_from_unit_floats(r, g, b, hwb.alpha.unwrap_or(1.0)))
        }
        cssparser_color::Color::Lab(lab) => {
            let srgb = Srgb::from_color(Lab::new(
                lab.lightness.unwrap_or(0.0),
                lab.a.unwrap_or(0.0),
                lab.b.unwrap_or(0.0),
            ));
            Ok(rgba_from_unit_floats(
                srgb.red,
                srgb.green,
                srgb.blue,
                lab.alpha.unwrap_or(1.0),
            ))
        }
        cssparser_color::Color::Lch(lch) => {
            let srgb = Srgb::from_color(Lch::new(
                lch.lightness.unwrap_or(0.0),
                lch.chroma.unwrap_or(0.0),
                lch.hue.unwrap_or(0.0),
            ));
            Ok(rgba_from_unit_floats(
                srgb.red,
                srgb.green,
                srgb.blue,
                lch.alpha.unwrap_or(1.0),
            ))
        }
        cssparser_color::Color::Oklab(oklab) => {
            let srgb = Srgb::from_color(Oklab::new(
                oklab.lightness.unwrap_or(0.0),
                oklab.a.unwrap_or(0.0),
                oklab.b.unwrap_or(0.0),
            ));
            Ok(rgba_from_unit_floats(
                srgb.red,
                srgb.green,
                srgb.blue,
                oklab.alpha.unwrap_or(1.0),
            ))
        }
        cssparser_color::Color::Oklch(oklch) => {
            let srgb = Srgb::from_color(Oklch::new(
                oklch.lightness.unwrap_or(0.0),
                oklch.chroma.unwrap_or(0.0),
                oklch.hue.unwrap_or(0.0),
            ));
            Ok(rgba_from_unit_floats(
                srgb.red,
                srgb.green,
                srgb.blue,
                oklch.alpha.unwrap_or(1.0),
            ))
        }
        _ => Err(input.new_custom_error(())),
    }
}

/// `color-mix(in <color-space> [<hue-interpolation-method>]?, <color> <percentage>?, <color> <percentage>?)`.
///
/// A form with `currentcolor` as an operand is not supported. `currentcolor` is resolved
/// after the cascade (once the element's `color` is known), whereas the mixing happens
/// here, so the value is not known yet. As the spec says, it is treated as an invalid
/// colour and the declaration is dropped.
fn parse_color_mix<'i>(
    input: &mut Parser<'i, '_>,
    depth: u32,
) -> Result<Color, ParseError<'i, ()>> {
    if depth >= MAX_COLOR_MIX_DEPTH {
        return Err(input.new_custom_error(()));
    }
    if input.expect_function_matching("color-mix").is_err() {
        return Err(input.new_custom_error(()));
    }
    input.parse_nested_block(|input| {
        if input.expect_ident_matching("in").is_err() {
            return Err(input.new_custom_error(()));
        }
        let space = match input.expect_ident() {
            Ok(ident) => ColorSpace::parse(ident).ok_or(()),
            Err(_) => Err(()),
        }
        .map_err(|_| input.new_custom_error(()))?;

        // A `<hue-interpolation-method>` is meaningful only in a polar colour space.
        // Syntactically it is two idents in a row, as in `shorter hue`.
        let hue_method = input
            .try_parse(|input| {
                let method = match input.expect_ident() {
                    Ok(ident) => HueMethod::parse(ident).ok_or(()),
                    Err(_) => Err(()),
                }?;
                input.expect_ident_matching("hue").map_err(|_| ())?;
                Ok::<_, ()>(method)
            })
            .unwrap_or_default();

        input
            .expect_comma()
            .map_err(|_| input.new_custom_error(()))?;
        let (first, first_percentage) = parse_color_mix_operand(input, depth)?;
        input
            .expect_comma()
            .map_err(|_| input.new_custom_error(()))?;
        let (second, second_percentage) = parse_color_mix_operand(input, depth)?;

        let (w1, w2, alpha_multiplier) = normalize_mix_weights(first_percentage, second_percentage)
            .ok_or_else(|| input.new_custom_error(()))?;

        let (red, green, blue, alpha) = color_mix::mix(space, hue_method, first, w1, second, w2);
        Ok(rgba_from_unit_floats(
            red,
            green,
            blue,
            alpha * alpha_multiplier,
        ))
    })
}

/// `<color> <percentage>?` (in either order).
#[allow(clippy::type_complexity)]
fn parse_color_mix_operand<'i>(
    input: &mut Parser<'i, '_>,
    depth: u32,
) -> Result<(color_mix::UnitRgba, Option<f32>), ParseError<'i, ()>> {
    let leading = input.try_parse(parse_mix_percentage).ok();
    let color = parse_color_at_depth(input, depth + 1)?;
    let percentage = match leading {
        Some(p) => Some(p),
        None => input.try_parse(parse_mix_percentage).ok(),
    };

    let Color::Rgba {
        red,
        green,
        blue,
        alpha,
    } = color
    else {
        // `currentcolor`. As explained above, it cannot be resolved here.
        return Err(input.new_custom_error(()));
    };
    Ok((
        (
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
            alpha,
        ),
        percentage,
    ))
}

fn parse_mix_percentage<'i>(input: &mut Parser<'i, '_>) -> Result<f32, ParseError<'i, ()>> {
    match input.expect_percentage() {
        // A negative percentage is invalid.
        Ok(p) if p >= 0.0 => Ok(p),
        _ => Err(input.new_custom_error(())),
    }
}

/// Normalise the two weights (CSS Color 5 section 3.2). Returns
/// `(first weight, second weight, the factor to apply to the result's alpha)`. Both being 0% is invalid.
fn normalize_mix_weights(first: Option<f32>, second: Option<f32>) -> Option<(f32, f32, f32)> {
    match (first, second) {
        (None, None) => Some((0.5, 0.5, 1.0)),
        (Some(p), None) => Some((p, 1.0 - p, 1.0)),
        (None, Some(p)) => Some((1.0 - p, p, 1.0)),
        (Some(a), Some(b)) => {
            let sum = a + b;
            if sum <= 0.0 {
                return None;
            }
            // When they add up to less than 100%, the result is made transparent by the shortfall.
            let alpha_multiplier = if sum < 1.0 { sum } else { 1.0 };
            Some((a / sum, b / sum, alpha_multiplier))
        }
    }
}

/// Build a [`Color::Rgba`] from RGB components and an alpha in the range 0.0 to 1.0.
fn rgba_from_unit_floats(red: f32, green: f32, blue: f32, alpha: f32) -> Color {
    let to_u8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::Rgba {
        red: to_u8(red),
        green: to_u8(green),
        blue: to_u8(blue),
        alpha: alpha.clamp(0.0, 1.0),
    }
}

/// `object-fit`.
fn parse_object_fit<'i>(input: &mut Parser<'i, '_>) -> Result<ObjectFit, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "fill" => ObjectFit::Fill,
        "contain" => ObjectFit::Contain,
        "cover" => ObjectFit::Cover,
        "none" => ObjectFit::None,
        "scale-down" => ObjectFit::ScaleDown,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `box-shadow: none | <shadow>#`.
fn parse_box_shadow<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<SpecifiedBoxShadow>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(Vec::new());
    }
    input.parse_comma_separated(parse_single_box_shadow)
}

/// One `<shadow>`. `inset` and `<color>` may be written either before or after, but the run
/// of lengths (`<length>{2,4}`, in the order offset-x/offset-y/blur-radius/spread-radius)
/// is parsed as one block, as the CSS spec requires.
fn parse_single_box_shadow<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedBoxShadow, ParseError<'i, ()>> {
    let mut inset = false;
    let mut color = None;
    let mut lengths = None;

    loop {
        if !inset
            && input
                .try_parse(|input| input.expect_ident_matching("inset"))
                .is_ok()
        {
            inset = true;
            continue;
        }
        if color.is_none() {
            if let Ok(c) = input.try_parse(parse_color) {
                color = Some(c);
                continue;
            }
        }
        if lengths.is_none() {
            if let Ok(l) = input.try_parse(parse_box_shadow_lengths) {
                lengths = Some(l);
                continue;
            }
        }
        break;
    }

    let Some((offset_x, offset_y, blur_radius, spread_radius)) = lengths else {
        return Err(input.new_custom_error(()));
    };
    Ok(SpecifiedBoxShadow {
        offset_x,
        offset_y,
        blur_radius,
        spread_radius,
        color,
        inset,
    })
}

/// `<length>{2,4}` (offset-x offset-y [blur-radius [spread-radius]]).
/// offset-x/offset-y are required; blur-radius/spread-radius default to `0`.
#[allow(clippy::type_complexity)]
fn parse_box_shadow_lengths<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<
    (
        SpecifiedLength,
        SpecifiedLength,
        SpecifiedLength,
        SpecifiedLength,
    ),
    ParseError<'i, ()>,
> {
    let offset_x = parse_length(input)?;
    let offset_y = parse_length(input)?;
    let blur_radius = input
        .try_parse(parse_length)
        .unwrap_or(SpecifiedLength::Px(0.0));
    let spread_radius = input
        .try_parse(parse_length)
        .unwrap_or(SpecifiedLength::Px(0.0));
    Ok((offset_x, offset_y, blur_radius, spread_radius))
}

/// `content`. Accepts a sequence of string literals, `attr`, `counter`/`counters` and quote
/// keywords, concatenating any number of them. `none`/`normal` are treated as `None`,
/// meaning "no generated box".
fn parse_content<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Option<Vec<ContentPart>>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| -> Result<(), ParseError<'i, ()>> {
            let ident = input.expect_ident()?.clone();
            match_ignore_ascii_case! { &ident,
                "none" | "normal" => Ok(()),
                _ => Err(input.new_custom_error(())),
            }
        })
        .is_ok()
    {
        return Ok(None);
    }

    let mut parts = Vec::new();
    loop {
        if let Ok(s) = input.try_parse(|input| input.expect_string_cloned()) {
            parts.push(ContentPart::String(s.as_ref().to_string()));
            continue;
        }
        if let Ok(part) = input.try_parse(parse_content_quote_keyword) {
            parts.push(part);
            continue;
        }
        if let Ok(part) = input.try_parse(parse_content_function) {
            parts.push(part);
            continue;
        }
        break;
    }
    if parts.is_empty() {
        return Err(input.new_custom_error(()));
    }
    Ok(Some(parts))
}

fn parse_content_quote_keyword<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ContentPart, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "open-quote" => ContentPart::OpenQuote,
        "close-quote" => ContentPart::CloseQuote,
        "no-open-quote" => ContentPart::NoOpenQuote,
        "no-close-quote" => ContentPart::NoCloseQuote,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `attr(name)`/`counter(name [, style])`/`counters(name, separator [, style])`.
fn parse_content_function<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<ContentPart, ParseError<'i, ()>> {
    let name = input.expect_function()?.clone();
    if name.eq_ignore_ascii_case("attr") {
        return input.parse_nested_block(|input| {
            let ident = input.expect_ident()?.clone();
            Ok(ContentPart::Attr(ident.as_ref().to_string()))
        });
    }
    if name.eq_ignore_ascii_case("counter") {
        return input.parse_nested_block(|input| {
            let counter_name = input.expect_ident()?.as_ref().to_string();
            let style = if input.try_parse(|input| input.expect_comma()).is_ok() {
                parse_list_style_type(input)?
            } else {
                ListStyleType::Decimal
            };
            Ok(ContentPart::Counter(counter_name, style))
        });
    }
    if name.eq_ignore_ascii_case("counters") {
        return input.parse_nested_block(|input| {
            let counter_name = input.expect_ident()?.as_ref().to_string();
            input.expect_comma()?;
            let separator = input.expect_string()?.as_ref().to_string();
            let style = if input.try_parse(|input| input.expect_comma()).is_ok() {
                parse_list_style_type(input)?
            } else {
                ListStyleType::Decimal
            };
            Ok(ContentPart::Counters(counter_name, separator, style))
        });
    }
    Err(input.new_custom_error(()))
}

/// Shared by `counter-reset`/`counter-increment`. `none` is an empty list; otherwise it is
/// a repetition of `name [<integer>]` (the value defaults to `default_value`).
fn parse_counter_list<'i>(
    input: &mut Parser<'i, '_>,
    default_value: i32,
) -> Result<Vec<(String, i32)>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(Vec::new());
    }
    let mut result = Vec::new();
    loop {
        let Ok(name) = input.try_parse(|input| input.expect_ident_cloned()) else {
            break;
        };
        let value = input
            .try_parse(|input| input.expect_integer())
            .unwrap_or(default_value);
        result.push((name.as_ref().to_string(), value));
    }
    if result.is_empty() {
        return Err(input.new_custom_error(()));
    }
    Ok(result)
}

/// `quotes`. `none` becomes `None` (always generating an empty string); otherwise it is a
/// repetition of `"open" "close"` pairs, shallowest nesting depth first.
fn parse_quotes<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Option<Vec<QuotePair>>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(None);
    }
    let mut pairs = Vec::new();
    loop {
        let Ok(open) = input.try_parse(|input| input.expect_string_cloned()) else {
            break;
        };
        let close = input.expect_string()?.as_ref().to_string();
        pairs.push(QuotePair {
            open: open.as_ref().to_string(),
            close,
        });
    }
    if pairs.is_empty() {
        return Err(input.new_custom_error(()));
    }
    Ok(Some(pairs))
}

/// A simple implementation of `background-image`. Only a single `url(...)` is accepted
/// (non-`url()` values such as `linear-gradient()`, and comma-separated multiple
/// backgrounds, are not supported). `none` is treated as `None`, meaning no background image.
fn parse_background_image<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Option<String>, ParseError<'i, ()>> {
    if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        return match_ignore_ascii_case! { &ident,
            "none" => Ok(None),
            _ => Err(input.new_custom_error(())),
        };
    }
    let url = input
        .expect_url_or_string()
        .map_err(|_| input.new_custom_error(()))?;
    Ok(Some(url.as_ref().to_string()))
}

/// One component of `background-position`. `left`/`right` fix the horizontal axis and
/// `top`/`bottom` the vertical one. `center`, a length and a percentage can each be either
/// axis.
enum BackgroundPositionComponent {
    Horizontal(SpecifiedLengthPercentage),
    Vertical(SpecifiedLengthPercentage),
    Either(SpecifiedLengthPercentage),
}

fn parse_background_position_component<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BackgroundPositionComponent, ParseError<'i, ()>> {
    use BackgroundPositionComponent as C;
    if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        return match_ignore_ascii_case! { &ident,
            "left" => Ok(C::Horizontal(SpecifiedLengthPercentage::Percentage(0.0))),
            "right" => Ok(C::Horizontal(SpecifiedLengthPercentage::Percentage(1.0))),
            "top" => Ok(C::Vertical(SpecifiedLengthPercentage::Percentage(0.0))),
            "bottom" => Ok(C::Vertical(SpecifiedLengthPercentage::Percentage(1.0))),
            "center" => Ok(C::Either(SpecifiedLengthPercentage::Percentage(0.5))),
            _ => Err(input.new_custom_error(())),
        };
    }
    Ok(C::Either(parse_length_percentage(input)?))
}

/// `background-position`. Accepts one or two values, combining keywords
/// (`left`/`center`/`right`/`top`/`bottom`) with lengths and percentages.
fn parse_background_position<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedBackgroundPosition, ParseError<'i, ()>> {
    use BackgroundPositionComponent as C;

    let half = SpecifiedLengthPercentage::Percentage(0.5);
    let first = parse_background_position_component(input)?;
    let second = input.try_parse(parse_background_position_component).ok();

    let (horizontal, vertical) = match second {
        None => match first {
            C::Vertical(v) => (half, v),
            C::Horizontal(h) | C::Either(h) => (h, half),
        },
        Some(second) => {
            let first_is_vertical = matches!(first, C::Vertical(_));
            let second_is_horizontal = matches!(second, C::Horizontal(_));
            if matches!((&first, &second), (C::Horizontal(_), C::Horizontal(_)))
                || matches!((&first, &second), (C::Vertical(_), C::Vertical(_)))
            {
                return Err(input.new_custom_error(()));
            }
            let value_of = |c: C| match c {
                C::Horizontal(v) | C::Vertical(v) | C::Either(v) => v,
            };
            if first_is_vertical || second_is_horizontal {
                (value_of(second), value_of(first))
            } else {
                (value_of(first), value_of(second))
            }
        }
    };

    Ok(SpecifiedBackgroundPosition {
        horizontal,
        vertical,
    })
}

/// `background-size`. `cover`/`contain`, or `[<length-percentage> | auto]{1,2}`
/// (with only one value, the height is `auto`).
fn parse_background_size<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedBackgroundSize, ParseError<'i, ()>> {
    if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        return match_ignore_ascii_case! { &ident,
            "cover" => Ok(SpecifiedBackgroundSize::Cover),
            "contain" => Ok(SpecifiedBackgroundSize::Contain),
            "auto" => Ok(SpecifiedBackgroundSize::WidthHeight(
                SpecifiedLengthPercentageOrAuto::Auto,
                input
                    .try_parse(parse_length_percentage_or_auto)
                    .unwrap_or(SpecifiedLengthPercentageOrAuto::Auto),
            )),
            _ => Err(input.new_custom_error(())),
        };
    }
    let width = SpecifiedLengthPercentageOrAuto::LengthPercentage(parse_length_percentage(input)?);
    let height = input
        .try_parse(parse_length_percentage_or_auto)
        .unwrap_or(SpecifiedLengthPercentageOrAuto::Auto);
    Ok(SpecifiedBackgroundSize::WidthHeight(width, height))
}

/// `background-repeat`. Only the CSS2.1 value set (CSS3 values such as `round`/`space`,
/// and comma-separated multiple backgrounds, are not supported).
fn parse_background_repeat<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BackgroundRepeat, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "repeat" => BackgroundRepeat::Repeat,
        "repeat-x" => BackgroundRepeat::RepeatX,
        "repeat-y" => BackgroundRepeat::RepeatY,
        "no-repeat" => BackgroundRepeat::NoRepeat,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `background-attachment`. `fixed` is drawn the same as `scroll`.
fn parse_background_attachment<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<BackgroundAttachment, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "scroll" => BackgroundAttachment::Scroll,
        "fixed" => BackgroundAttachment::Fixed,
        _ => return Err(input.new_custom_error(())),
    })
}

/// A simple implementation of the `background` shorthand.
/// `color`/`image`/`repeat`/`attachment`/`position` (with `size` immediately after a `/`)
/// are accepted in any order (the same "loop and decide which kind of value it is with
/// `try_parse`" approach as the `border` shorthand). As the spec requires, every longhand
/// not given is reset to its initial value (unlike the `border`/`list-style` shorthands,
/// it does not carry over earlier declarations).
fn parse_background_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let mut color = None;
    let mut image = None;
    let mut repeat = None;
    let mut attachment = None;
    let mut position = None;
    let mut size = None;

    loop {
        if position.is_none() {
            if let Ok(p) = input.try_parse(parse_background_position) {
                position = Some(p);
                if input.try_parse(|input| input.expect_delim('/')).is_ok() {
                    size = Some(parse_background_size(input)?);
                }
                continue;
            }
        }
        if repeat.is_none() {
            if let Ok(r) = input.try_parse(parse_background_repeat) {
                repeat = Some(r);
                continue;
            }
        }
        if attachment.is_none() {
            if let Ok(a) = input.try_parse(parse_background_attachment) {
                attachment = Some(a);
                continue;
            }
        }
        if image.is_none() {
            if let Ok(img) = input.try_parse(parse_background_image) {
                image = Some(img);
                continue;
            }
        }
        if color.is_none() {
            if let Ok(c) = input.try_parse(parse_color) {
                color = Some(c);
                continue;
            }
        }
        break;
    }

    Ok(vec![
        D::BackgroundColor(color.unwrap_or(Color::Rgba {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0.0,
        })),
        D::BackgroundImage(image.unwrap_or(None)),
        D::BackgroundPosition(position.unwrap_or(SpecifiedBackgroundPosition {
            horizontal: SpecifiedLengthPercentage::Percentage(0.0),
            vertical: SpecifiedLengthPercentage::Percentage(0.0),
        })),
        D::BackgroundSize(size.unwrap_or(SpecifiedBackgroundSize::WidthHeight(
            SpecifiedLengthPercentageOrAuto::Auto,
            SpecifiedLengthPercentageOrAuto::Auto,
        ))),
        D::BackgroundRepeat(repeat.unwrap_or(BackgroundRepeat::Repeat)),
        D::BackgroundAttachment(attachment.unwrap_or(BackgroundAttachment::Scroll)),
    ])
}

fn parse_font_family<'i>(input: &mut Parser<'i, '_>) -> Result<Vec<String>, ParseError<'i, ()>> {
    input.parse_comma_separated(parse_family_name)
}

/// Parse a single `<family-name>` (a quoted string, or a run of whitespace-separated
/// identifiers). Called both from the `font-family` property (a comma-separated list) and
/// from the `font-family` descriptor of `@font-face` (a single value).
pub(crate) fn parse_family_name<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<String, ParseError<'i, ()>> {
    if let Ok(name) = input.try_parse(|input| input.expect_string_cloned()) {
        return Ok(name.as_ref().to_string());
    }
    let mut name = input.expect_ident()?.as_ref().to_string();
    while let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        name.push(' ');
        name.push_str(&ident);
    }
    Ok(name)
}

fn parse_flex_direction<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<FlexDirection, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "row" => FlexDirection::Row,
        "row-reverse" => FlexDirection::RowReverse,
        "column" => FlexDirection::Column,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_flex_wrap<'i>(input: &mut Parser<'i, '_>) -> Result<FlexWrap, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "nowrap" => FlexWrap::NoWrap,
        "wrap" => FlexWrap::Wrap,
        "wrap-reverse" => FlexWrap::WrapReverse,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `justify-content`. The `safe`/`unsafe` overflow keywords from CSS Box Alignment are not supported.
fn parse_justify_content<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<JustifyContent, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "normal" => JustifyContent::Normal,
        "flex-start" | "start" => JustifyContent::FlexStart,
        "flex-end" | "end" => JustifyContent::FlexEnd,
        "center" => JustifyContent::Center,
        "space-between" => JustifyContent::SpaceBetween,
        "space-around" => JustifyContent::SpaceAround,
        "space-evenly" => JustifyContent::SpaceEvenly,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_align_items<'i>(input: &mut Parser<'i, '_>) -> Result<AlignItems, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "flex-start" | "start" => AlignItems::FlexStart,
        "flex-end" | "end" => AlignItems::FlexEnd,
        "center" => AlignItems::Center,
        "baseline" => AlignItems::Baseline,
        "stretch" => AlignItems::Stretch,
        _ => return Err(input.new_custom_error(())),
    })
}

fn parse_align_content<'i>(input: &mut Parser<'i, '_>) -> Result<AlignContent, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "flex-start" | "start" => AlignContent::FlexStart,
        "flex-end" | "end" => AlignContent::FlexEnd,
        "center" => AlignContent::Center,
        "stretch" => AlignContent::Stretch,
        "space-between" => AlignContent::SpaceBetween,
        "space-around" => AlignContent::SpaceAround,
        "space-evenly" => AlignContent::SpaceEvenly,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `align-self`, including `auto` (the initial value, using the parent's `align-items`).
fn parse_align_self<'i>(input: &mut Parser<'i, '_>) -> Result<AlignSelf, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "auto" => AlignSelf::Auto,
        "flex-start" | "start" => AlignSelf::FlexStart,
        "flex-end" | "end" => AlignSelf::FlexEnd,
        "center" => AlignSelf::Center,
        "baseline" => AlignSelf::Baseline,
        "stretch" => AlignSelf::Stretch,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `flex-grow`/`flex-shrink`. A negative value is invalid per the spec (rejected at parse
/// time, riding on the existing behaviour of ignoring the whole declaration).
fn parse_non_negative_number<'i>(input: &mut Parser<'i, '_>) -> Result<f32, ParseError<'i, ()>> {
    let value = input.expect_number()?;
    if value < 0.0 {
        return Err(input.new_custom_error(()));
    }
    Ok(value)
}

/// `flex-basis: auto | content | <length-percentage>`. `content` is treated as `auto`.
fn parse_flex_basis<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedFlexBasis, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(SpecifiedFlexBasis::Auto);
    }
    if input
        .try_parse(|input| input.expect_ident_matching("content"))
        .is_ok()
    {
        return Ok(SpecifiedFlexBasis::Content);
    }
    parse_length_percentage(input).map(SpecifiedFlexBasis::LengthPercentage)
}

/// A simple implementation of the `flex` shorthand. It reproduces the CSS spec's default
/// rules (a lone `flex: <number>`, or `<number> <number>`, gives an omitted basis of 0%,
/// while a lone `flex: <width>` gives both grow and shrink a value of 1).
fn parse_flex_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;

    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(vec![
            D::FlexGrow(0.0),
            D::FlexShrink(0.0),
            D::FlexBasis(SpecifiedFlexBasis::Auto),
        ]);
    }

    let mut grow = None;
    let mut shrink = None;
    let mut basis = None;

    loop {
        if grow.is_none() {
            if let Ok(g) = input.try_parse(parse_non_negative_number) {
                grow = Some(g);
                if let Ok(s) = input.try_parse(parse_non_negative_number) {
                    shrink = Some(s);
                }
                continue;
            }
        }
        if basis.is_none() {
            if let Ok(b) = input.try_parse(parse_flex_basis) {
                basis = Some(b);
                continue;
            }
        }
        break;
    }

    if grow.is_none() && basis.is_none() {
        return Err(input.new_custom_error(()));
    }

    // With basis omitted it is 0% (a numbers-only setting such as `flex: 1` flexes from a
    // 0% basis, per the spec). With grow omitted (a basis-only setting) it is 1 (per the
    // spec, unlike flex-grow's ordinary initial value of 0).
    Ok(vec![
        D::FlexGrow(grow.unwrap_or(1.0)),
        D::FlexShrink(shrink.unwrap_or(1.0)),
        D::FlexBasis(basis.unwrap_or(SpecifiedFlexBasis::LengthPercentage(
            SpecifiedLengthPercentage::Percentage(0.0),
        ))),
    ])
}

/// The `gap` shorthand. `<row-gap> <column-gap>?` (the same 1-to-2-value pattern as
/// `border-spacing`).
fn parse_gap_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let row = parse_length_percentage(input)?;
    let column = input.try_parse(parse_length_percentage).unwrap_or(row);
    Ok(vec![D::RowGap(row), D::ColumnGap(column)])
}

/// `transform: none | <transform-function>+`.
fn parse_transform<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<SpecifiedTransformFunction>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(Vec::new());
    }
    let mut functions = Vec::new();
    while let Ok(f) = input.try_parse(parse_transform_function) {
        functions.push(f);
    }
    if functions.is_empty() {
        return Err(input.new_custom_error(()));
    }
    Ok(functions)
}

/// One `<transform-function>` (`translate(...)` and the like).
fn parse_transform_function<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedTransformFunction, ParseError<'i, ()>> {
    use SpecifiedTransformFunction as F;
    let name = match input.next()?.clone() {
        Token::Function(name) => name,
        _ => return Err(input.new_custom_error(())),
    };
    input.parse_nested_block(|input| {
        Ok(match_ignore_ascii_case! { &name,
            "translate" => {
                let x = parse_length_percentage(input)?;
                let y = input
                    .try_parse(|input| {
                        input.expect_comma()?;
                        parse_length_percentage(input)
                    })
                    .unwrap_or(SpecifiedLengthPercentage::Length(SpecifiedLength::Px(0.0)));
                F::Translate(x, y)
            },
            "translatex" => F::TranslateX(parse_length_percentage(input)?),
            "translatey" => F::TranslateY(parse_length_percentage(input)?),
            "scale" => {
                let x = input.expect_number()?;
                let y = input
                    .try_parse(|input| {
                        input.expect_comma()?;
                        input.expect_number()
                    })
                    .unwrap_or(x);
                F::Scale(x, y)
            },
            "scalex" => F::ScaleX(input.expect_number()?),
            "scaley" => F::ScaleY(input.expect_number()?),
            "rotate" => F::Rotate(parse_angle_radians(input)?),
            "skew" => {
                let x = parse_angle_radians(input)?;
                let y = input
                    .try_parse(|input| {
                        input.expect_comma()?;
                        parse_angle_radians(input)
                    })
                    .unwrap_or(0.0);
                F::Skew(x, y)
            },
            "skewx" => F::SkewX(parse_angle_radians(input)?),
            "skewy" => F::SkewY(parse_angle_radians(input)?),
            "matrix" => {
                let a = input.expect_number()?;
                input.expect_comma()?;
                let b = input.expect_number()?;
                input.expect_comma()?;
                let c = input.expect_number()?;
                input.expect_comma()?;
                let d = input.expect_number()?;
                input.expect_comma()?;
                let e = input.expect_number()?;
                input.expect_comma()?;
                let f = input.expect_number()?;
                F::Matrix(a, b, c, d, e, f)
            },
            _ => return Err(input.new_custom_error(())),
        })
    })
}

/// Normalise an angle value (`deg`/`rad`/`grad`/`turn`) to radians. A unitless `0` is valid
/// too (as the CSS spec says).
fn parse_angle_radians<'i>(input: &mut Parser<'i, '_>) -> Result<f32, ParseError<'i, ()>> {
    let token = input.next()?.clone();
    match token {
        Token::Number { value: 0.0, .. } => Ok(0.0),
        Token::Dimension {
            value, ref unit, ..
        } => {
            if unit.eq_ignore_ascii_case("deg") {
                Ok(value.to_radians())
            } else if unit.eq_ignore_ascii_case("rad") {
                Ok(value)
            } else if unit.eq_ignore_ascii_case("grad") {
                Ok(value * std::f32::consts::PI / 200.0)
            } else if unit.eq_ignore_ascii_case("turn") {
                Ok(value * std::f32::consts::TAU)
            } else {
                Err(input.new_custom_error(()))
            }
        }
        _ => Err(input.new_custom_error(())),
    }
}

/// `grid-template-columns`/`grid-template-rows`.
/// `none | [ <line-names>? <track-size> | <repeat> ]+ <line-names>?`
fn parse_track_list<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedTrackList, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(SpecifiedTrackList::default());
    }

    let mut components = Vec::new();
    // Line names can sit before and after the tracks. Keep `components.len() + 1` entries.
    let mut line_names: Vec<Vec<String>> = vec![parse_line_names(input)];

    loop {
        if let Ok(repeat) = input.try_parse(parse_track_repeat) {
            components.push(repeat);
        } else if let Ok(size) = input.try_parse(parse_track_size) {
            components.push(SpecifiedTrackComponent::Single(size));
        } else {
            break;
        }
        line_names.push(parse_line_names(input));
    }

    if components.is_empty() {
        return Err(input.new_custom_error(()));
    }
    Ok(SpecifiedTrackList {
        components,
        line_names,
    })
}

/// Line names in `[a b]` form. An empty `Vec` when absent (there is always one entry per track boundary).
fn parse_line_names(input: &mut Parser<'_, '_>) -> Vec<String> {
    input
        .try_parse(|input| {
            input.expect_square_bracket_block()?;
            input.parse_nested_block(|input| -> Result<Vec<String>, ParseError<'_, ()>> {
                let mut names = Vec::new();
                while let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
                    names.push(ident.as_ref().to_string());
                }
                Ok(names)
            })
        })
        .unwrap_or_default()
}

/// `repeat( [ <integer> | auto-fill | auto-fit ] , <track-list> )`.
fn parse_track_repeat<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedTrackComponent, ParseError<'i, ()>> {
    input.expect_function_matching("repeat")?;
    input.parse_nested_block(|input| {
        let count = if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
            match_ignore_ascii_case! { &ident,
                "auto-fill" => RepeatCount::AutoFill,
                "auto-fit" => RepeatCount::AutoFit,
                _ => return Err(input.new_custom_error(())),
            }
        } else {
            let count = input.expect_integer()?;
            if count < 1 {
                return Err(input.new_custom_error(()));
            }
            RepeatCount::Count(count as u16)
        };
        input.expect_comma()?;

        let mut tracks = Vec::new();
        let mut line_names: Vec<Vec<String>> = vec![parse_line_names(input)];
        while let Ok(size) = input.try_parse(parse_track_size) {
            tracks.push(size);
            line_names.push(parse_line_names(input));
        }
        if tracks.is_empty() {
            return Err(input.new_custom_error(()));
        }
        Ok(SpecifiedTrackComponent::Repeat {
            count,
            tracks,
            line_names,
        })
    })
}

/// `<track-size> = <track-breadth> | minmax(<inflexible>, <track-breadth>) |
/// `fit-content(<length-percentage>)`.
fn parse_track_size<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedTrackSize, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_function_matching("minmax"))
        .is_ok()
    {
        return input.parse_nested_block(|input| {
            let min = parse_track_breadth(input)?;
            // Per the CSS spec, `fr` cannot be the first argument of minmax().
            if matches!(min, SpecifiedTrackBreadth::Fr(_)) {
                return Err(input.new_custom_error(()));
            }
            input.expect_comma()?;
            let max = parse_track_breadth(input)?;
            Ok(SpecifiedTrackSize::MinMax(min, max))
        });
    }
    if input
        .try_parse(|input| input.expect_function_matching("fit-content"))
        .is_ok()
    {
        return input.parse_nested_block(|input| {
            Ok(SpecifiedTrackSize::FitContent(parse_length_percentage(
                input,
            )?))
        });
    }
    Ok(SpecifiedTrackSize::Breadth(parse_track_breadth(input)?))
}

/// `<track-breadth> = <length-percentage> | <flex> | auto | min-content | max-content`.
fn parse_track_breadth<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedTrackBreadth, ParseError<'i, ()>> {
    if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        return match_ignore_ascii_case! { &ident,
            "auto" => Ok(SpecifiedTrackBreadth::Auto),
            "min-content" => Ok(SpecifiedTrackBreadth::MinContent),
            "max-content" => Ok(SpecifiedTrackBreadth::MaxContent),
            _ => Err(input.new_custom_error(())),
        };
    }
    // `<flex>` (`1fr`). cssparser returns a number with `fr` as a Dimension token.
    if let Ok(fr) = input.try_parse(|input| -> Result<f32, ParseError<'i, ()>> {
        let token = input.next()?.clone();
        match token {
            Token::Dimension {
                value, ref unit, ..
            } if unit.eq_ignore_ascii_case("fr") => {
                if value < 0.0 {
                    return Err(input.new_custom_error(()));
                }
                Ok(value)
            }
            _ => Err(input.new_custom_error(())),
        }
    }) {
        return Ok(SpecifiedTrackBreadth::Fr(fr));
    }

    match parse_length_percentage(input)? {
        SpecifiedLengthPercentage::Length(length) => Ok(SpecifiedTrackBreadth::Length(length)),
        SpecifiedLengthPercentage::Percentage(v) => Ok(SpecifiedTrackBreadth::Percentage(v)),
        // A `calc()` track size is not supported (the type handed to taffy has no compound
        // value; a known simplification).
        SpecifiedLengthPercentage::Calc(_) => Err(input.new_custom_error(())),
    }
}

/// `grid-auto-columns`/`grid-auto-rows`. `<track-size>+`.
fn parse_auto_track_list<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<SpecifiedTrackSize>, ParseError<'i, ()>> {
    let mut sizes = Vec::new();
    while let Ok(size) = input.try_parse(parse_track_size) {
        sizes.push(size);
    }
    if sizes.is_empty() {
        return Err(input.new_custom_error(()));
    }
    Ok(sizes)
}

/// `grid-auto-flow: [ row | column ] || dense`.
fn parse_grid_auto_flow<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<GridAutoFlow, ParseError<'i, ()>> {
    let mut column = None;
    let mut dense = false;
    loop {
        let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) else {
            break;
        };
        match_ignore_ascii_case! { &ident,
            "row" if column.is_none() => column = Some(false),
            "column" if column.is_none() => column = Some(true),
            "dense" if !dense => dense = true,
            _ => return Err(input.new_custom_error(())),
        }
    }
    if column.is_none() && !dense {
        return Err(input.new_custom_error(()));
    }
    Ok(match (column.unwrap_or(false), dense) {
        (false, false) => GridAutoFlow::Row,
        (false, true) => GridAutoFlow::RowDense,
        (true, false) => GridAutoFlow::Column,
        (true, true) => GridAutoFlow::ColumnDense,
    })
}

/// `grid-row-start` and friends.
/// `auto | <integer> | span <integer> | <custom-ident> | span <custom-ident>`
fn parse_grid_line<'i>(input: &mut Parser<'i, '_>) -> Result<GridLine, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(GridLine::Auto);
    }
    if input
        .try_parse(|input| input.expect_ident_matching("span"))
        .is_ok()
    {
        // Accepts both `span <integer>` and `span <custom-ident> <integer>?`.
        if let Ok(count) = input.try_parse(|input| input.expect_integer()) {
            if count < 1 {
                return Err(input.new_custom_error(()));
            }
            return Ok(GridLine::Span(count as u16));
        }
        let name = input.expect_ident()?.as_ref().to_string();
        let count = input.try_parse(|input| input.expect_integer()).unwrap_or(1);
        if count < 1 {
            return Err(input.new_custom_error(()));
        }
        return Ok(GridLine::NamedSpan(name, count as u16));
    }
    if let Ok(line) = input.try_parse(|input| input.expect_integer()) {
        // `0` is invalid (per the CSS spec, line numbers start at 1 and negatives count from the end).
        if line == 0 {
            return Err(input.new_custom_error(()));
        }
        return Ok(GridLine::Line(line as i16));
    }
    Ok(GridLine::Named(input.expect_ident()?.as_ref().to_string()))
}

/// `grid-row: <start> [/ <end>]?`. An omitted end is `auto` (as the spec says).
fn parse_grid_row_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (start, end) = parse_grid_line_pair(input)?;
    Ok(vec![D::GridRowStart(start), D::GridRowEnd(end)])
}

fn parse_grid_column_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let (start, end) = parse_grid_line_pair(input)?;
    Ok(vec![D::GridColumnStart(start), D::GridColumnEnd(end)])
}

fn parse_grid_line_pair<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<(GridLine, GridLine), ParseError<'i, ()>> {
    let start = parse_grid_line(input)?;
    let end = if input.try_parse(|input| input.expect_delim('/')).is_ok() {
        parse_grid_line(input)?
    } else {
        // When a single named line is written, as in `grid-row: foo`, the end takes the same
        // name (per the CSS spec). Otherwise it is `auto`.
        match &start {
            GridLine::Named(name) => GridLine::Named(name.clone()),
            _ => GridLine::Auto,
        }
    };
    Ok((start, end))
}

/// `grid-area: <row-start> [/ <col-start> [/ <row-end> [/ <col-end>]]]`.
fn parse_grid_area_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let row_start = parse_grid_line(input)?;

    let mut slots = Vec::new();
    while input.try_parse(|input| input.expect_delim('/')).is_ok() {
        slots.push(parse_grid_line(input)?);
    }

    // An omitted slot takes the name of the corresponding start if that is a named line, and
    // `auto` otherwise (per the CSS spec).
    let fallback = |line: &GridLine| match line {
        GridLine::Named(name) => GridLine::Named(name.clone()),
        _ => GridLine::Auto,
    };
    let column_start = slots
        .first()
        .cloned()
        .unwrap_or_else(|| fallback(&row_start));
    let row_end = slots
        .get(1)
        .cloned()
        .unwrap_or_else(|| fallback(&row_start));
    let column_end = slots
        .get(2)
        .cloned()
        .unwrap_or_else(|| fallback(&column_start));

    Ok(vec![
        D::GridRowStart(row_start),
        D::GridColumnStart(column_start),
        D::GridRowEnd(row_end),
        D::GridColumnEnd(column_end),
    ])
}

/// `grid-template-areas: none | <string>+`. Checks that every row has the same number of
/// columns and that cells with the same name form a rectangle, ignoring the whole declaration (`Err`) on a violation.
fn parse_grid_template_areas<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<GridArea>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(Vec::new());
    }

    let mut rows: Vec<Vec<String>> = Vec::new();
    while let Ok(s) = input.try_parse(|input| input.expect_string_cloned()) {
        rows.push(s.split_whitespace().map(|cell| cell.to_string()).collect());
    }
    if rows.is_empty() {
        return Err(input.new_custom_error(()));
    }

    let column_count = rows[0].len();
    if column_count == 0 || rows.iter().any(|row| row.len() != column_count) {
        return Err(input.new_custom_error(()));
    }

    // Collect the extent of each name (its minimum and maximum row and column) and check
    // that the resulting rectangle is filled with no gaps.
    let mut bounds: Vec<(String, usize, usize, usize, usize)> = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            // A `.` (one or more in a row) is an unnamed cell.
            if cell.chars().all(|ch| ch == '.') {
                continue;
            }
            match bounds.iter_mut().find(|(name, ..)| name == cell) {
                Some((_, row_start, row_end, column_start, column_end)) => {
                    *row_start = (*row_start).min(r);
                    *row_end = (*row_end).max(r);
                    *column_start = (*column_start).min(c);
                    *column_end = (*column_end).max(c);
                }
                None => bounds.push((cell.clone(), r, r, c, c)),
            }
        }
    }

    for (name, row_start, row_end, column_start, column_end) in &bounds {
        for row in rows.iter().take(row_end + 1).skip(*row_start) {
            for cell in row.iter().take(column_end + 1).skip(*column_start) {
                if cell != name {
                    // Disjoint or L-shaped (non-rectangular) areas are invalid.
                    return Err(input.new_custom_error(()));
                }
            }
        }
    }

    Ok(bounds
        .into_iter()
        .map(
            |(name, row_start, row_end, column_start, column_end)| GridArea {
                name,
                // taffy's `GridTemplateArea` holds 1-indexed grid line numbers (the area's
                // implicit `-start`/`-end` line names are registered with those numbers).
                // From 0-indexed cell coordinates, the start converts with +1 and the end
                // with +2 (the line after the final cell).
                row_start: row_start as u16 + 1,
                row_end: row_end as u16 + 2,
                column_start: column_start as u16 + 1,
                column_end: column_end as u16 + 2,
            },
        )
        .collect())
}

/// `text-shadow: none | <shadow>#`.
/// Unlike `box-shadow` it has no spread and no inset.
fn parse_text_shadow<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<SpecifiedTextShadow>, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("none"))
        .is_ok()
    {
        return Ok(Vec::new());
    }
    input.parse_comma_separated(parse_single_text_shadow)
}

/// One `<shadow>`. The `<color>` may be written before or after the lengths (per the CSS spec).
fn parse_single_text_shadow<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<SpecifiedTextShadow, ParseError<'i, ()>> {
    let mut color = None;
    let mut lengths = None;

    loop {
        if color.is_none() {
            if let Ok(c) = input.try_parse(parse_color) {
                color = Some(c);
                continue;
            }
        }
        if lengths.is_none() {
            if let Ok(l) = input.try_parse(parse_text_shadow_lengths) {
                lengths = Some(l);
                continue;
            }
        }
        break;
    }

    let Some((offset_x, offset_y, blur_radius)) = lengths else {
        return Err(input.new_custom_error(()));
    };
    Ok(SpecifiedTextShadow {
        offset_x,
        offset_y,
        blur_radius,
        color,
    })
}

/// `<length>{2,3}` (offset-x offset-y [blur-radius]). An omitted blur is `0`.
fn parse_text_shadow_lengths<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<(SpecifiedLength, SpecifiedLength, SpecifiedLength), ParseError<'i, ()>> {
    let offset_x = parse_length(input)?;
    let offset_y = parse_length(input)?;
    let blur_radius = input
        .try_parse(parse_length)
        .unwrap_or(SpecifiedLength::Px(0.0));
    Ok((offset_x, offset_y, blur_radius))
}

/// `text-overflow: clip | ellipsis`.
fn parse_text_overflow<'i>(input: &mut Parser<'i, '_>) -> Result<TextOverflow, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "clip" => TextOverflow::Clip,
        "ellipsis" => TextOverflow::Ellipsis,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `word-break: normal | break-all | keep-all`. `break-word` (a deprecated value) is
/// equivalent to `overflow-wrap: break-word`, but that would be a conversion across
/// properties, so it is not accepted (a known simplification).
fn parse_word_break<'i>(input: &mut Parser<'i, '_>) -> Result<WordBreak, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "normal" => WordBreak::Normal,
        "break-all" => WordBreak::BreakAll,
        "keep-all" => WordBreak::KeepAll,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `overflow-wrap: normal | break-word | anywhere`.
fn parse_overflow_wrap<'i>(input: &mut Parser<'i, '_>) -> Result<OverflowWrap, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "normal" => OverflowWrap::Normal,
        // It differs from `anywhere` only in its effect on min-content width, so they are treated alike.
        "break-word" | "anywhere" => OverflowWrap::BreakWord,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `hyphens: none | manual | auto`. `auto` behaves the same as `manual`, since we have no
/// dictionary.
fn parse_hyphens<'i>(input: &mut Parser<'i, '_>) -> Result<Hyphens, ParseError<'i, ()>> {
    let ident = input.expect_ident()?.clone();
    Ok(match_ignore_ascii_case! { &ident,
        "none" => Hyphens::None,
        "manual" | "auto" => Hyphens::Manual,
        _ => return Err(input.new_custom_error(())),
    })
}

/// `text-emphasis-style`.
/// `none | [ filled | open ] || [ dot | circle | double-circle | triangle | sesame ] | <string>`.
fn parse_text_emphasis_style<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<EmphasisStyle, ParseError<'i, ()>> {
    if let Ok(s) = input.try_parse(|input| input.expect_string_cloned()) {
        // Only the first character of a `<string>` is used (as the spec says). An empty string is invalid.
        let Some(ch) = s.chars().next() else {
            return Err(input.new_custom_error(()));
        };
        return Ok(EmphasisStyle::String(ch));
    }

    /// The keywords this property could interpret.
    enum Keyword {
        None,
        Filled(bool),
        Shape(EmphasisShape),
    }

    let mut filled = None;
    let mut shape = None;
    loop {
        // A keyword we cannot interpret (the `<color>` of the `text-emphasis` shorthand, say)
        // rewinds the whole `try_parse` and leaves the loop. Returning `Err` here would drop
        // the entire style for a legitimate setting such as
        // `text-emphasis: filled dot red`.
        let Ok(keyword) = input.try_parse(|input| -> Result<Keyword, ParseError<'i, ()>> {
            let ident = input.expect_ident()?.clone();
            Ok(match_ignore_ascii_case! { &ident,
                "none" => Keyword::None,
                "filled" => Keyword::Filled(true),
                "open" => Keyword::Filled(false),
                "dot" => Keyword::Shape(EmphasisShape::Dot),
                "circle" => Keyword::Shape(EmphasisShape::Circle),
                "double-circle" => Keyword::Shape(EmphasisShape::DoubleCircle),
                "triangle" => Keyword::Shape(EmphasisShape::Triangle),
                "sesame" => Keyword::Shape(EmphasisShape::Sesame),
                _ => return Err(input.new_custom_error(())),
            })
        }) else {
            break;
        };
        match keyword {
            Keyword::None => return Ok(EmphasisStyle::None),
            // Writing the same kind of keyword twice is invalid (`filled open`, say).
            Keyword::Filled(_) if filled.is_some() => return Err(input.new_custom_error(())),
            Keyword::Shape(_) if shape.is_some() => return Err(input.new_custom_error(())),
            Keyword::Filled(v) => filled = Some(v),
            Keyword::Shape(v) => shape = Some(v),
        }
    }

    if filled.is_none() && shape.is_none() {
        return Err(input.new_custom_error(()));
    }
    Ok(EmphasisStyle::Shape {
        // With the shape omitted the initial value is `dot`, and with the fill omitted it is `filled` (as the spec says).
        shape: shape.unwrap_or(EmphasisShape::Dot),
        filled: filled.unwrap_or(true),
    })
}

/// `text-emphasis-position`. In horizontal writing only `over`/`under` mean anything, so
/// `right`/`left` are accepted and then skipped.
fn parse_text_emphasis_position<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<EmphasisPosition, ParseError<'i, ()>> {
    let mut position = None;
    loop {
        let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) else {
            break;
        };
        match_ignore_ascii_case! { &ident,
            "over" if position.is_none() => position = Some(EmphasisPosition::Over),
            "under" if position.is_none() => position = Some(EmphasisPosition::Under),
            "right" | "left" => {},
            _ => return Err(input.new_custom_error(())),
        }
    }
    position.ok_or_else(|| input.new_custom_error(()))
}

/// The `text-emphasis` shorthand (`<style> || <color>`). Whichever side is not given is
/// reset to its initial value (the same policy as the `background` shorthand).
fn parse_text_emphasis_shorthand<'i>(
    input: &mut Parser<'i, '_>,
) -> Result<Vec<PropertyDeclaration>, ParseError<'i, ()>> {
    use PropertyDeclaration as D;
    let mut style = None;
    let mut color = None;

    loop {
        if style.is_none() {
            if let Ok(s) = input.try_parse(parse_text_emphasis_style) {
                style = Some(s);
                continue;
            }
        }
        if color.is_none() {
            if let Ok(c) = input.try_parse(parse_color) {
                color = Some(c);
                continue;
            }
        }
        break;
    }

    if style.is_none() && color.is_none() {
        return Err(input.new_custom_error(()));
    }
    Ok(vec![
        D::TextEmphasisStyle(style.unwrap_or_default()),
        D::TextEmphasisColor(color.unwrap_or(Color::CurrentColor)),
    ])
}

/// `opacity: <number> | <percentage>`. Clamped to 0-1.
fn parse_opacity<'i>(input: &mut Parser<'i, '_>) -> Result<f32, ParseError<'i, ()>> {
    let token = input.next()?.clone();
    let value = match token {
        Token::Number { value, .. } => value,
        Token::Percentage { unit_value, .. } => unit_value,
        _ => return Err(input.new_custom_error(())),
    };
    Ok(value.clamp(0.0, 1.0))
}
