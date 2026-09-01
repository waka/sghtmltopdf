//! CSS parsing, selector matching and the cascade (cssparser/selectors).

mod cascade;
mod color_mix;
mod computed;
mod custom_properties;
mod element_ref;
mod extract;
mod font_face;
mod import;
mod page_rule;
mod presentational;
mod properties;
mod rule_index;
mod selector_impl;
mod stylesheet;
mod ua;
mod values;

pub use cascade::matching_declarations;
pub use computed::{
    compute_single_element_style, compute_styles, compute_styles_with_parent,
    resolve_margin_box_content, ComputedBoxShadow, ComputedStyle, ComputedTextShadow,
    FirstLetterStyle, LineHeight, RgbaColor,
};
pub use element_ref::ElementRef;
pub use extract::extract_author_stylesheet;
pub use font_face::{FontFaceRule, FontFaceSource};
pub use page_rule::{
    resolve_page_rules, rules_use_page_count, MarginBoxArea, NamedPageSize, PageOrientation,
    PageRule, PageSelector, PageSizeValue, ResolvedPageRule,
};
pub use properties::PropertyDeclaration;
pub use selector_impl::SgSelectorImpl;
pub use stylesheet::{
    needs_preceding_siblings, parse_stylesheet, streaming_unsafe_selectors, StyleRule, Stylesheet,
};
pub use ua::user_agent_stylesheet;
pub use values::{
    compose_transform, AlignContent, AlignItems, AlignSelf, AspectRatio, BackgroundAttachment,
    BackgroundPosition, BackgroundRepeat, BackgroundSize, BorderCollapse, BorderStyle, BoxSizing,
    BreakBetween, BreakInside, CaptionSide, Clear, Color, ContentPart, CornerRadius, Display,
    EmphasisPosition, EmphasisShape, EmphasisStyle, EmptyCells, FlexBasis, FlexDirection, FlexWrap,
    Float, FontStyle, FontWeight, GridArea, GridAutoFlow, GridLine, Hyphens, JustifyContent,
    Length, LengthPercentage, LengthPercentageOrAuto, ListStylePosition, ListStyleType, MaxSize,
    ObjectFit, Overflow, OverflowWrap, Position, QuotePair, RepeatCount, TableLayout, TextAlign,
    TextOverflow, TextTransform, TrackBreadth, TrackComponent, TrackList, TrackSize,
    TransformFunction, VerticalAlign, Visibility, WhiteSpace, WordBreak, ZIndex,
};
