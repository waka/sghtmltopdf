//! Types for CSS property values.
//!
//! `Length`/`LengthPercentage`/`LengthPercentageOrAuto` hold computed values after the
//! cascade (always already resolved to px). Specified values straight from the parser have
//! to keep relative units such as `em`/`rem` distinct, so they use
//! `SpecifiedLength`/`SpecifiedLengthPercentage`/`SpecifiedLengthPercentageOrAuto`
//! instead (`style::computed` resolves those to px using the element's own and the root
//! element's computed `font-size`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    /// Inline-level on the outside (it joins the parent's line) but laid out as a block
    /// inside: an unbreakable box.
    InlineBlock,
    /// For `table` elements only. Establishes a table formatting context
    /// (collects the `table-row`/`table-cell` descendants and lays them out with the column width algorithm).
    Table,
    /// For `tr` elements only. Meaningful only under a `Display::Table` ancestor.
    TableRow,
    /// For `td`/`th` elements only. Meaningful only under a `Display::TableRow` ancestor.
    TableCell,
    /// For `caption` elements only. Meaningful only under a `Display::Table` ancestor
    /// (`box_tree.rs::collect_table_rows` detects it alongside `table-row`).
    TableCaption,
    /// The default for `li` elements. Generates a marker box (the bullet or number) in
    /// addition to an ordinary block box.
    /// `box_tree.rs::child_kind` treats it the same as `Block`.
    ListItem,
    /// For `flex` elements only. Establishes a flexbox formatting context
    /// (generates one flex item per child and delegates layout to taffy).
    /// `inline-flex` is not supported.
    Flex,
    /// `display: grid`. `inline-grid` is not supported (same reason as `inline-flex`).
    Grid,
    None,
}

/// `font-weight`. A simplified implementation treating any numeric value (`700`, say) of 600 or more as `Bold`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontWeight {
    #[default]
    Normal,
    Bold,
}

/// `font-style`. `oblique` has no slant of its own, so it is treated as `Italic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontStyle {
    #[default]
    Normal,
    Italic,
}

/// `text-decoration-line`. `underline` and `line-through` can be given together
/// (as the spec allows). `overline` is not supported (it is rarely used).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextDecorationLine {
    pub underline: bool,
    pub line_through: bool,
}

/// `border-style`. `groove`/`ridge`/`inset`/`outset` (which derive a two-tone pseudo-3D
/// shading from border-color) are supported too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    #[default]
    None,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

/// Values of `break-before`/`break-after`. In the CSS spec `page` is a separate keyword
/// looking ahead to multiple page sizes and named pages, but within the current scope of a
/// single page size it has the same effect as `always`: force a break to a new page.
/// `left`/`right`/`recto`/`verso` (spread control) are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakBetween {
    #[default]
    Auto,
    Avoid,
    Always,
}

/// Values of `break-inside`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakInside {
    #[default]
    Auto,
    Avoid,
}

/// `float` (CSS2.1 9.5.1). The logical `inline-start`/`inline-end` values are outside
/// CSS2.1's scope and are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Float {
    #[default]
    None,
    Left,
    Right,
}

/// `clear` (CSS2.1 9.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Clear {
    #[default]
    None,
    Left,
    Right,
    Both,
}

/// `position`. `absolute`/`fixed` are supported too (ignored under `Mode::Streaming`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Position {
    #[default]
    Static,
    Relative,
    /// Positioned against the nearest positioned ancestor (or the initial containing block if there is none).
    Absolute,
    /// Repeated on every page, positioned against each page's content area.
    Fixed,
}

impl Position {
    /// Whether this positioning takes the element out of flow (occupying no space in normal flow).
    pub fn is_out_of_flow(self) -> bool {
        matches!(self, Position::Absolute | Position::Fixed)
    }
}

/// `text-align`. `start`/`end` (for bidi) are not supported: `direction` itself is not
/// supported, so they are out of scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Right,
    Center,
    Justify,
}

/// `white-space`. `pre-wrap`/`pre-line`/`break-spaces` are not supported (`pre` was judged
/// to be as far as business documents need).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WhiteSpace {
    #[default]
    Normal,
    Nowrap,
    Pre,
}

/// The computed value of `<track-breadth>`. Lengths are already resolved to px.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackBreadth {
    Length(f32),
    /// A percentage (50% = 0.5).
    Percentage(f32),
    /// `<flex>` (`1fr`). The track's growth factor.
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
}

/// The computed value of `<track-size>`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TrackSize {
    Breadth(TrackBreadth),
    /// `minmax(min, max)`. The CSS spec forbids `fr` on the min side (the parser rejects it).
    MinMax(TrackBreadth, TrackBreadth),
    /// `fit-content(<length-percentage>)`.
    FitContent(LengthPercentage),
}

/// The repeat count of `repeat()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatCount {
    Count(u16),
    AutoFill,
    AutoFit,
}

/// One element of a `<track-list>` (a single track, or a `repeat()`).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackComponent {
    Single(TrackSize),
    Repeat {
        count: RepeatCount,
        tracks: Vec<TrackSize>,
        /// Line names between the repeated tracks (`tracks.len() + 1` entries).
        line_names: Vec<Vec<String>>,
    },
}

/// The computed value of `grid-template-columns`/`grid-template-rows`. Empty means `none`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackList {
    pub components: Vec<TrackComponent>,
    /// The line names (`[name]`) placed before and after the tracks. `components.len() + 1` entries.
    pub line_names: Vec<Vec<String>>,
}

/// A placement value such as `grid-row-start`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum GridLine {
    #[default]
    Auto,
    /// A 1-indexed line number (a negative value counts from the end).
    Line(i16),
    /// `span <integer>`.
    Span(u16),
    /// A named line (including the implicit `foo-start` and friends created by `grid-template-areas`).
    Named(String),
    /// `span <custom-ident>`.
    NamedSpan(String, u16),
}

/// `grid-auto-flow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridAutoFlow {
    #[default]
    Row,
    Column,
    RowDense,
    ColumnDense,
}

/// One named area defined by `grid-template-areas`. Rows and columns are 1-indexed grid
/// line numbers (the same convention as taffy's `GridTemplateArea`, where
/// `row_end`/`column_end` point at the line after the final cell).
#[derive(Debug, Clone, PartialEq)]
pub struct GridArea {
    pub name: String,
    pub row_start: u16,
    pub row_end: u16,
    pub column_start: u16,
    pub column_end: u16,
}

/// `word-break`. Switches which break opportunities exist at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WordBreak {
    /// As before: breaks allowed only at a boundary adjoining a CJK character.
    #[default]
    Normal,
    /// Breaks allowed at every character boundary.
    BreakAll,
    /// No break even at a CJK boundary (whitespace is the only opportunity).
    KeepAll,
}

/// `overflow-wrap` (also known as `word-wrap`). Adds no break opportunities; it acts as a
/// fallback for when something "still does not fit even at the start of a line". `anywhere`
/// is treated as `break-word` (they differ only in their effect on min-content width, a
/// distinction this engine does not make).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverflowWrap {
    #[default]
    Normal,
    BreakWord,
}

/// `hyphens`. `auto` behaves as `manual` because we have no dictionary (breaking only at a
/// soft hyphen, U+00AD).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Hyphens {
    /// Never break, not even at a soft hyphen.
    None,
    /// Treat a soft hyphen as a break opportunity and show a hyphen when breaking there.
    #[default]
    Manual,
}

/// `text-overflow`. A `<string>` value is not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextOverflow {
    #[default]
    Clip,
    Ellipsis,
}

/// The mark shape of `text-emphasis-style`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmphasisShape {
    #[default]
    Dot,
    Circle,
    DoubleCircle,
    Triangle,
    Sesame,
}

/// `text-emphasis-style`. `None` means no mark (the initial value).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum EmphasisStyle {
    #[default]
    None,
    /// A keyword value: `filled` or `open`, paired with a shape.
    Shape { shape: EmphasisShape, filled: bool },
    /// A `<string>` value. Only the first character is used (as the spec says).
    String(char),
}

/// `text-emphasis-position`. In horizontal writing only `over`/`under` mean anything
/// (`right`/`left` are skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmphasisPosition {
    #[default]
    Over,
    Under,
}

/// One `text-shadow`. The specified value straight from the parser (lengths still have
/// unresolved `em`/`rem`). Unlike `box-shadow` it has no spread and no inset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecifiedTextShadow {
    pub offset_x: SpecifiedLength,
    pub offset_y: SpecifiedLength,
    pub blur_radius: SpecifiedLength,
    /// When omitted this means `currentcolor` (left as `None` and resolved by the computed style).
    pub color: Option<Color>,
}

impl SpecifiedTextShadow {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> TextShadow {
        TextShadow {
            offset_x: self.offset_x.resolve(font_size, root_font_size).0,
            offset_y: self.offset_y.resolve(font_size, root_font_size).0,
            blur_radius: self.blur_radius.resolve(font_size, root_font_size).0,
            color: self.color,
        }
    }
}

/// The computed value of one `text-shadow` (lengths resolved to px, colour still unresolved).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub color: Option<Color>,
}

/// `text-transform`. `full-width`/`full-size-kana` (special transforms for Japanese typesetting) are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextTransform {
    #[default]
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

/// The specified value of `line-height` straight from the parser. Under the CSS spec,
/// `<number>`/`<percentage>` follow a rule unlike other inherited properties: "the computed
/// value is the specified number itself", not an absolute value pre-multiplied by the parent's font-size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedLineHeight {
    Normal,
    /// `<number>`.
    Number(f32),
    Length(SpecifiedLength),
    /// `<percentage>`. The same meaning as `<number>` (50% means 0.5).
    Percentage(f32),
}

/// The specified value shared by `letter-spacing`/`word-spacing` (both `normal | <length>`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedSpacing {
    Normal,
    Length(SpecifiedLength),
}

impl SpecifiedSpacing {
    /// `normal` resolves to `0` (no extra space between words or characters).
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> f32 {
        match self {
            Self::Normal => 0.0,
            Self::Length(length) => length.resolve(font_size, root_font_size).0,
        }
    }
}

/// `border-collapse`. The collapse value only unifies how borders are drawn; layout
/// (column widths and cell placement) stays exactly as it is for separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderCollapse {
    #[default]
    Separate,
    Collapse,
}

/// `caption-side`. CSS2.1's `left`/`right` (logical values for vertical writing) are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptionSide {
    #[default]
    Top,
    Bottom,
}

/// `table-layout`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TableLayout {
    #[default]
    Auto,
    Fixed,
}

/// `empty-cells`. Meaningful only with `border-collapse: separate` (as the CSS spec says).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmptyCells {
    #[default]
    Show,
    Hide,
}

/// `vertical-align`. The initial value in CSS2.1 is `baseline`.
///
/// The set of values is shared between the inline context and the table-cell context. As
/// CSS2.1 requires, only `top`/`middle`/`bottom`/`baseline` apply to a table cell; any other
/// value is treated as `baseline`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum VerticalAlign {
    Top,
    Middle,
    Bottom,
    #[default]
    Baseline,
    /// Subscript. The font size is unchanged (the shrinking is done by the UA stylesheet's
    /// `sub` rule).
    Sub,
    /// Superscript (likewise).
    Super,
    /// Aligned with the top of the parent's text (the line's reference run).
    TextTop,
    /// Likewise with the bottom of the text.
    TextBottom,
    /// A length or percentage (positive is upwards). A percentage is relative to that run's
    /// `line-height`.
    LengthPercentage(LengthPercentage),
}

/// The specified value of `vertical-align` straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedVerticalAlign {
    Top,
    Middle,
    Bottom,
    Baseline,
    Sub,
    Super,
    TextTop,
    TextBottom,
    LengthPercentage(SpecifiedLengthPercentage),
}

impl SpecifiedVerticalAlign {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> VerticalAlign {
        match self {
            Self::Top => VerticalAlign::Top,
            Self::Middle => VerticalAlign::Middle,
            Self::Bottom => VerticalAlign::Bottom,
            Self::Baseline => VerticalAlign::Baseline,
            Self::Sub => VerticalAlign::Sub,
            Self::Super => VerticalAlign::Super,
            Self::TextTop => VerticalAlign::TextTop,
            Self::TextBottom => VerticalAlign::TextBottom,
            Self::LengthPercentage(lp) => {
                VerticalAlign::LengthPercentage(lp.resolve(font_size, root_font_size))
            }
        }
    }
}

/// `list-style-type`. `disc`/`circle`/`square` are fixed symbols; the rest are generated
/// from the counter value (numeric and alphabetic ones as "body plus `.`", a known simplification).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListStyleType {
    #[default]
    Disc,
    Circle,
    Square,
    Decimal,
    DecimalLeadingZero,
    LowerRoman,
    UpperRoman,
    LowerAlpha,
    UpperAlpha,
    None,
}

/// One part of `content` (at parse time, not yet resolved). Several parts can be
/// concatenated (`content: "Chapter " counter(chapter) ": "` and so on).
#[derive(Debug, Clone, PartialEq)]
pub enum ContentPart {
    String(String),
    /// `attr(name)`. An HTML attribute value.
    Attr(String),
    /// `counter(name [, style])`. `style` reuses [`ListStyleType`]
    /// (`disc`/`circle`/`square`/`none` generate an empty string, as the spec says).
    Counter(String, ListStyleType),
    /// `counters(name, separator [, style])`.
    Counters(String, String, ListStyleType),
    OpenQuote,
    CloseQuote,
    NoOpenQuote,
    NoCloseQuote,
}

/// One nesting level of `quotes` (the open quote and the close quote).
#[derive(Debug, Clone, PartialEq)]
pub struct QuotePair {
    pub open: String,
    pub close: String,
}

/// `list-style-position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListStylePosition {
    #[default]
    Outside,
    Inside,
}

/// `overflow`. `scroll`/`auto` are not distinguished from `hidden` and get the same
/// clipping (print has no concept of scrolling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Scroll,
    Auto,
}

impl Overflow {
    /// Everything but `visible` is subject to the same clipping.
    pub fn clips(self) -> bool {
        self != Overflow::Visible
    }
}

/// `visibility`. `collapse` is treated as `hidden`
/// (recomputing table row/column heights is not supported).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Visible,
    Hidden,
    Collapse,
}

impl Visibility {
    pub fn is_hidden(self) -> bool {
        self != Visibility::Visible
    }
}

/// `box-sizing`. `padding-box` (non-standard) is not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BoxSizing {
    #[default]
    ContentBox,
    BorderBox,
}

/// `z-index`. It has no effect on a `position: static` element
/// (as the spec says; the caller checks that).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ZIndex {
    #[default]
    Auto,
    Value(i32),
}

impl ZIndex {
    /// The effective value used as the sort key for drawing order (`auto` means `0`, as the spec says).
    pub fn sort_key(self) -> i32 {
        match self {
            ZIndex::Auto => 0,
            ZIndex::Value(v) => v,
        }
    }
}

/// A length (px) or a percentage. A computed value, after the cascade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthPercentage {
    Length(f32),
    Percentage(f32),
    /// A resolved compound `calc` value. The used value is
    /// `px + percent * basis` (`percent` is a ratio, 50% = 0.5).
    Calc {
        px: f32,
        percent: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LengthPercentageOrAuto {
    LengthPercentage(LengthPercentage),
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Length(pub f32);

/// The specified value of a length straight from the parser. `em` is relative to the
/// reference font size (the caller chooses whether that is the element's own or its
/// parent's), and `rem` is relative to the root element's font size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedLength {
    Px(f32),
    Em(f32),
    Rem(f32),
}

impl SpecifiedLength {
    /// Whether the length is negative.
    pub fn is_negative(self) -> bool {
        match self {
            Self::Px(v) | Self::Em(v) | Self::Rem(v) => v < 0.0,
        }
    }

    /// Resolve to a computed [`Length`] using `font_size` (the em basis, px) and
    /// `root_font_size` (the rem basis, px).
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> Length {
        match self {
            Self::Px(px) => Length(px),
            Self::Em(em) => Length(em * font_size),
            Self::Rem(rem) => Length(rem * root_font_size),
        }
    }
}

/// The computed value of one corner of `border-radius` (horizontal radius, vertical
/// radius). A true circle has horizontal = vertical.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CornerRadius {
    pub horizontal: Length,
    pub vertical: Length,
}

/// The specified value of one corner of `border-radius` straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecifiedCornerRadius {
    pub horizontal: SpecifiedLength,
    pub vertical: SpecifiedLength,
}

impl SpecifiedCornerRadius {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> CornerRadius {
        CornerRadius {
            horizontal: self.horizontal.resolve(font_size, root_font_size),
            vertical: self.vertical.resolve(font_size, root_font_size),
        }
    }
}

/// The specified value of a `calc`. `em`/`rem` are unresolved at parse time, so it is held
/// as four components and folded into px by `resolve`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpecifiedCalc {
    pub px: f32,
    pub em: f32,
    pub rem: f32,
    /// The percentage as a ratio (50% = 0.5).
    pub percent: f32,
}

impl SpecifiedCalc {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> LengthPercentage {
        LengthPercentage::Calc {
            px: self.px + self.em * font_size + self.rem * root_font_size,
            percent: self.percent,
        }
    }
}

/// The specified value of "a length or a percentage" straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedLengthPercentage {
    Length(SpecifiedLength),
    Percentage(f32),
    /// `calc`.
    Calc(SpecifiedCalc),
}

impl SpecifiedLengthPercentage {
    /// Whether the written value is negative. `calc()` is `false`, its sign being unknown until resolved.
    pub fn is_negative(self) -> bool {
        match self {
            Self::Length(length) => length.is_negative(),
            Self::Percentage(p) => p < 0.0,
            Self::Calc(_) => false,
        }
    }

    pub fn resolve(self, font_size: f32, root_font_size: f32) -> LengthPercentage {
        match self {
            Self::Length(length) => {
                LengthPercentage::Length(length.resolve(font_size, root_font_size).0)
            }
            Self::Percentage(fraction) => LengthPercentage::Percentage(fraction),
            Self::Calc(calc) => calc.resolve(font_size, root_font_size),
        }
    }
}

/// The specified value of "a length, a percentage, or auto" straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedLengthPercentageOrAuto {
    LengthPercentage(SpecifiedLengthPercentage),
    Auto,
}

impl SpecifiedLengthPercentageOrAuto {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> LengthPercentageOrAuto {
        match self {
            Self::Auto => LengthPercentageOrAuto::Auto,
            Self::LengthPercentage(lp) => {
                LengthPercentageOrAuto::LengthPercentage(lp.resolve(font_size, root_font_size))
            }
        }
    }
}

/// The computed value of `max-width`/`max-height`. It needs a type distinct from
/// `LengthPercentage` in order to express `none` (no upper bound). `min-width`/`min-height`
/// have an initial value of `0`, so they use `LengthPercentage` directly.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MaxSize {
    #[default]
    None,
    LengthPercentage(LengthPercentage),
}

/// A `<track-breadth>` straight from the parser. Lengths still have unresolved `em`/`rem`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedTrackBreadth {
    Length(SpecifiedLength),
    Percentage(f32),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
}

impl SpecifiedTrackBreadth {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> TrackBreadth {
        match self {
            Self::Length(length) => {
                TrackBreadth::Length(length.resolve(font_size, root_font_size).0)
            }
            Self::Percentage(v) => TrackBreadth::Percentage(v),
            Self::Fr(v) => TrackBreadth::Fr(v),
            Self::Auto => TrackBreadth::Auto,
            Self::MinContent => TrackBreadth::MinContent,
            Self::MaxContent => TrackBreadth::MaxContent,
        }
    }
}

/// A `<track-size>` straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedTrackSize {
    Breadth(SpecifiedTrackBreadth),
    MinMax(SpecifiedTrackBreadth, SpecifiedTrackBreadth),
    FitContent(SpecifiedLengthPercentage),
}

impl SpecifiedTrackSize {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> TrackSize {
        match self {
            Self::Breadth(b) => TrackSize::Breadth(b.resolve(font_size, root_font_size)),
            Self::MinMax(min, max) => TrackSize::MinMax(
                min.resolve(font_size, root_font_size),
                max.resolve(font_size, root_font_size),
            ),
            Self::FitContent(lp) => TrackSize::FitContent(lp.resolve(font_size, root_font_size)),
        }
    }
}

/// One element of a `<track-list>` straight from the parser.
#[derive(Debug, Clone, PartialEq)]
pub enum SpecifiedTrackComponent {
    Single(SpecifiedTrackSize),
    Repeat {
        count: RepeatCount,
        tracks: Vec<SpecifiedTrackSize>,
        line_names: Vec<Vec<String>>,
    },
}

/// `grid-template-columns`/`-rows` straight from the parser.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpecifiedTrackList {
    pub components: Vec<SpecifiedTrackComponent>,
    pub line_names: Vec<Vec<String>>,
}

impl SpecifiedTrackList {
    pub fn resolve(&self, font_size: f32, root_font_size: f32) -> TrackList {
        TrackList {
            components: self
                .components
                .iter()
                .map(|component| match component {
                    SpecifiedTrackComponent::Single(size) => {
                        TrackComponent::Single(size.resolve(font_size, root_font_size))
                    }
                    SpecifiedTrackComponent::Repeat {
                        count,
                        tracks,
                        line_names,
                    } => TrackComponent::Repeat {
                        count: *count,
                        tracks: tracks
                            .iter()
                            .map(|size| size.resolve(font_size, root_font_size))
                            .collect(),
                        line_names: line_names.clone(),
                    },
                })
                .collect(),
            line_names: self.line_names.clone(),
        }
    }
}

/// `aspect-ratio: auto || <ratio>`. It contains no length, so specified and computed values
/// need not be distinguished.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AspectRatio {
    /// Whether the `auto` keyword is present. For a replaced element (`<img>`) the intrinsic ratio wins.
    pub auto: bool,
    /// The specified ratio (`width / height`). `None` means no ratio was given.
    pub ratio: Option<f32>,
}

impl Default for AspectRatio {
    /// The initial value, `auto`.
    fn default() -> Self {
        Self {
            auto: true,
            ratio: None,
        }
    }
}

/// The specified value of `max-width`/`max-height` straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedMaxSize {
    None,
    LengthPercentage(SpecifiedLengthPercentage),
}

impl SpecifiedMaxSize {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> MaxSize {
        match self {
            Self::None => MaxSize::None,
            Self::LengthPercentage(lp) => {
                MaxSize::LengthPercentage(lp.resolve(font_size, root_font_size))
            }
        }
    }
}

/// The computed value of `background-position` (horizontal/vertical, a length or percentage
/// relative to the border box). The keywords (`left`/`center`/`right`/`top`/`bottom`) are
/// already resolved to the corresponding percentages at parse time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundPosition {
    pub horizontal: LengthPercentage,
    pub vertical: LengthPercentage,
}

impl Default for BackgroundPosition {
    /// The initial value, `0% 0%` (top left).
    fn default() -> Self {
        Self {
            horizontal: LengthPercentage::Percentage(0.0),
            vertical: LengthPercentage::Percentage(0.0),
        }
    }
}

/// The specified value of `background-position` straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecifiedBackgroundPosition {
    pub horizontal: SpecifiedLengthPercentage,
    pub vertical: SpecifiedLengthPercentage,
}

impl SpecifiedBackgroundPosition {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> BackgroundPosition {
        BackgroundPosition {
            horizontal: self.horizontal.resolve(font_size, root_font_size),
            vertical: self.vertical.resolve(font_size, root_font_size),
        }
    }
}

/// The computed value of `background-size`. `Cover`/`Contain` are converted to a rectangle
/// at drawing time, based on the intrinsic size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BackgroundSize {
    WidthHeight(LengthPercentageOrAuto, LengthPercentageOrAuto),
    Cover,
    Contain,
}

impl Default for BackgroundSize {
    /// The initial value, `auto auto`.
    fn default() -> Self {
        Self::WidthHeight(LengthPercentageOrAuto::Auto, LengthPercentageOrAuto::Auto)
    }
}

/// The specified value of `background-size` straight from the parser.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedBackgroundSize {
    WidthHeight(
        SpecifiedLengthPercentageOrAuto,
        SpecifiedLengthPercentageOrAuto,
    ),
    Cover,
    Contain,
}

impl SpecifiedBackgroundSize {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> BackgroundSize {
        match self {
            Self::WidthHeight(w, h) => BackgroundSize::WidthHeight(
                w.resolve(font_size, root_font_size),
                h.resolve(font_size, root_font_size),
            ),
            Self::Cover => BackgroundSize::Cover,
            Self::Contain => BackgroundSize::Contain,
        }
    }
}

/// `background-repeat`. Only the CSS2.1 value set (repeat/repeat-x/repeat-y/no-repeat) is
/// supported (comma-separated multiple backgrounds and `round`/`space` are outside CSS3 scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundRepeat {
    #[default]
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

/// `background-attachment`. `fixed` is drawn the same as `scroll`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackgroundAttachment {
    #[default]
    Scroll,
    Fixed,
}

/// A colour. Resolving `currentcolor` and inheritance is the computed style's job, so this
/// holds the parse result as-is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Color {
    CurrentColor,
    Rgba {
        red: u8,
        green: u8,
        blue: u8,
        alpha: f32,
    },
}

/// `object-fit`. For `<img>` (replaced elements) only; not inherited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectFit {
    /// The initial value. Stretches to fill the whole content box, ignoring the intrinsic
    /// aspect ratio (the same as the existing `<img>` drawing before `object-fit` support).
    #[default]
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// One `box-shadow`. The specified value straight from the parser (lengths still have
/// unresolved `em`/`rem`). Comma-separated multiples are held as `Vec<SpecifiedBoxShadow>`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpecifiedBoxShadow {
    pub offset_x: SpecifiedLength,
    pub offset_y: SpecifiedLength,
    pub blur_radius: SpecifiedLength,
    pub spread_radius: SpecifiedLength,
    /// When omitted this means `currentcolor` (left as `None` and resolved by the computed style).
    pub color: Option<Color>,
    /// The `inset` keyword. It parses, but drawing it is not supported.
    pub inset: bool,
}

impl SpecifiedBoxShadow {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> BoxShadow {
        BoxShadow {
            offset_x: self.offset_x.resolve(font_size, root_font_size).0,
            offset_y: self.offset_y.resolve(font_size, root_font_size).0,
            blur_radius: self.blur_radius.resolve(font_size, root_font_size).0,
            spread_radius: self.spread_radius.resolve(font_size, root_font_size).0,
            color: self.color,
            inset: self.inset,
        }
    }
}

/// One `box-shadow`. Lengths are resolved to px, but `color` may still be an unresolved
/// `currentcolor` (resolving to `RgbaColor` is the computed style's job).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub spread_radius: f32,
    pub color: Option<Color>,
    pub inset: bool,
}

/// `flex-direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexDirection {
    #[default]
    Row,
    RowReverse,
    Column,
    ColumnReverse,
}

/// `flex-wrap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FlexWrap {
    #[default]
    NoWrap,
    Wrap,
    WrapReverse,
}

/// `justify-content`. The `safe`/`unsafe` overflow keywords from CSS Box Alignment are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JustifyContent {
    /// The initial value. It behaves like `flex-start` in a flex container, but means
    /// something different in grid (`auto` tracks absorb the leftover width and grow), so it is distinct.
    #[default]
    Normal,
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// `align-items`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignItems {
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    #[default]
    Stretch,
}

/// `align-content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignContent {
    FlexStart,
    FlexEnd,
    Center,
    #[default]
    Stretch,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

/// `align-self`. `Auto` (the initial value) uses the parent's `align-items` as-is (as the spec says).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlignSelf {
    #[default]
    Auto,
    FlexStart,
    FlexEnd,
    Center,
    Baseline,
    Stretch,
}

/// The specified value of `flex-basis` straight from the parser (`em`/`rem` unresolved).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedFlexBasis {
    Auto,
    /// The `content` keyword. Treated the same as `Auto`.
    Content,
    LengthPercentage(SpecifiedLengthPercentage),
}

impl SpecifiedFlexBasis {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> FlexBasis {
        match self {
            Self::Auto | Self::Content => FlexBasis::Auto,
            Self::LengthPercentage(lp) => {
                FlexBasis::LengthPercentage(lp.resolve(font_size, root_font_size))
            }
        }
    }
}

/// The computed value of `flex-basis`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum FlexBasis {
    #[default]
    Auto,
    LengthPercentage(LengthPercentage),
}

/// One `transform` function, as the specified value straight from the parser (`em`/`rem`
/// unresolved). Angles are normalised to radians at parse time, including units other than
/// degrees (`rad`/`grad`/`turn`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpecifiedTransformFunction {
    Translate(SpecifiedLengthPercentage, SpecifiedLengthPercentage),
    TranslateX(SpecifiedLengthPercentage),
    TranslateY(SpecifiedLengthPercentage),
    Scale(f32, f32),
    ScaleX(f32),
    ScaleY(f32),
    /// In radians.
    Rotate(f32),
    /// In radians (horizontal, vertical).
    Skew(f32, f32),
    SkewX(f32),
    SkewY(f32),
    Matrix(f32, f32, f32, f32, f32, f32),
}

impl SpecifiedTransformFunction {
    pub fn resolve(self, font_size: f32, root_font_size: f32) -> TransformFunction {
        match self {
            Self::Translate(x, y) => TransformFunction::Translate(
                x.resolve(font_size, root_font_size),
                y.resolve(font_size, root_font_size),
            ),
            Self::TranslateX(x) => {
                TransformFunction::TranslateX(x.resolve(font_size, root_font_size))
            }
            Self::TranslateY(y) => {
                TransformFunction::TranslateY(y.resolve(font_size, root_font_size))
            }
            Self::Scale(x, y) => TransformFunction::Scale(x, y),
            Self::ScaleX(x) => TransformFunction::ScaleX(x),
            Self::ScaleY(y) => TransformFunction::ScaleY(y),
            Self::Rotate(r) => TransformFunction::Rotate(r),
            Self::Skew(x, y) => TransformFunction::Skew(x, y),
            Self::SkewX(x) => TransformFunction::SkewX(x),
            Self::SkewY(y) => TransformFunction::SkewY(y),
            Self::Matrix(a, b, c, d, e, f) => TransformFunction::Matrix(a, b, c, d, e, f),
        }
    }
}

/// The computed value of one `transform` function. Percentages on the `translate` family
/// stay as `LengthPercentage`, since they resolve only once the element's own border-box
/// width/height is final (the same idea as `background-position`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformFunction {
    Translate(LengthPercentage, LengthPercentage),
    TranslateX(LengthPercentage),
    TranslateY(LengthPercentage),
    Scale(f32, f32),
    ScaleX(f32),
    ScaleY(f32),
    Rotate(f32),
    Skew(f32, f32),
    SkewX(f32),
    SkewY(f32),
    Matrix(f32, f32, f32, f32, f32, f32),
}

fn resolve_lp_against(lp: LengthPercentage, basis: f32) -> f32 {
    match lp {
        LengthPercentage::Length(px) => px,
        LengthPercentage::Percentage(p) => p * basis,
        LengthPercentage::Calc { px, percent } => px + percent * basis,
    }
}

impl TransformFunction {
    /// The transform matrix for this one function. It follows the same convention as CSS's
    /// `matrix(a, b, c, d, e, f)` (`x' = a*x + c*y + e`, `y' = b*x + d*y + f`) and is
    /// returned in CSS coordinates (Y positive downwards); converting to PDF coordinates is
    /// the caller of [`compose_transform`]'s job. `box_width`/`box_height` are the element's
    /// own border-box size, used to resolve percentages on the `translate` family.
    pub fn to_matrix(self, box_width: f32, box_height: f32) -> [f32; 6] {
        match self {
            Self::Translate(x, y) => [
                1.0,
                0.0,
                0.0,
                1.0,
                resolve_lp_against(x, box_width),
                resolve_lp_against(y, box_height),
            ],
            Self::TranslateX(x) => [1.0, 0.0, 0.0, 1.0, resolve_lp_against(x, box_width), 0.0],
            Self::TranslateY(y) => [1.0, 0.0, 0.0, 1.0, 0.0, resolve_lp_against(y, box_height)],
            Self::Scale(sx, sy) => [sx, 0.0, 0.0, sy, 0.0, 0.0],
            Self::ScaleX(sx) => [sx, 0.0, 0.0, 1.0, 0.0, 0.0],
            Self::ScaleY(sy) => [1.0, 0.0, 0.0, sy, 0.0, 0.0],
            Self::Rotate(radians) => {
                let (s, c) = radians.sin_cos();
                [c, s, -s, c, 0.0, 0.0]
            }
            Self::Skew(ax, ay) => [1.0, ay.tan(), ax.tan(), 1.0, 0.0, 0.0],
            Self::SkewX(ax) => [1.0, 0.0, ax.tan(), 1.0, 0.0, 0.0],
            Self::SkewY(ay) => [1.0, ay.tan(), 0.0, 1.0, 0.0, 0.0],
            Self::Matrix(a, b, c, d, e, f) => [a, b, c, d, e, f],
        }
    }
}

/// The composition applying `a` first and then `b` to the result (`b` after `a`, the same
/// convention as `matrix(...)`). Used to compose several `transform` functions in written order.
pub fn compose_transform_matrices(a: [f32; 6], b: [f32; 6]) -> [f32; 6] {
    let [a1, b1, c1, d1, e1, f1] = b;
    let [a2, b2, c2, d2, e2, f2] = a;
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

/// A single transform matrix composing `functions` in written order (still in CSS coordinates).
pub fn compose_transform(
    functions: &[TransformFunction],
    box_width: f32,
    box_height: f32,
) -> [f32; 6] {
    functions
        .iter()
        .fold([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], |acc, f| {
            compose_transform_matrices(acc, f.to_matrix(box_width, box_height))
        })
}

#[cfg(test)]
mod transform_tests {
    use super::*;

    fn assert_matrix_eq(a: [f32; 6], b: [f32; 6]) {
        for i in 0..6 {
            assert!(
                (a[i] - b[i]).abs() < 1e-4,
                "matrices differ at index {i}: {a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn translate_uses_percentage_against_own_box_size() {
        let m = TransformFunction::Translate(
            LengthPercentage::Percentage(0.5),
            LengthPercentage::Length(10.0),
        )
        .to_matrix(200.0, 50.0);
        assert_matrix_eq(m, [1.0, 0.0, 0.0, 1.0, 100.0, 10.0]);
    }

    #[test]
    fn rotate_90_degrees_matches_the_standard_rotation_matrix() {
        let m = TransformFunction::Rotate(std::f32::consts::FRAC_PI_2).to_matrix(0.0, 0.0);
        assert_matrix_eq(m, [0.0, 1.0, -1.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn composing_translate_then_scale_applies_translate_first() {
        // translate(10px, 0) then scale(2) moves the origin from (10,0) to (20,0)
        // (translating first and scaling the result doubles the translation too).
        let translate = TransformFunction::TranslateX(LengthPercentage::Length(10.0));
        let scale = TransformFunction::Scale(2.0, 2.0);
        let total = compose_transform(&[translate, scale], 0.0, 0.0);
        assert_matrix_eq(total, [2.0, 0.0, 0.0, 2.0, 20.0, 0.0]);
    }

    #[test]
    fn identity_matrix_for_empty_function_list() {
        let total = compose_transform(&[], 0.0, 0.0);
        assert_matrix_eq(total, [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
    }
}
