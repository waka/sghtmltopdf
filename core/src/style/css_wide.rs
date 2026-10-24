//! The CSS-wide keywords `inherit`, `initial` and `unset` on ordinary properties.
//!
//! A declaration whose whole value is one of these keywords is kept as a
//! [`CssWideDeclaration`]: the keyword plus one placeholder declaration per longhand the
//! property expands to (the placeholders only name the longhands; their values are unused).
//!
//! The style computation treats an absent declaration as `unset` already (the inherited value
//! for an inherited property, the initial value otherwise), so every keyword first clears the
//! longhands it names. That settles `unset`, `inherit` on an inherited property and `initial`
//! on a non-inherited one. The two remaining cases need the tables in this module:
//!
//! - `inherit` on a non-inherited property copies the parent's computed value
//!   ([`copy_computed`] from the parent into the finished style);
//! - `initial` on an inherited property makes the element inherit from a parent whose value
//!   has been put back to the initial one ([`copy_computed`] from the initial style into a
//!   copy of the parent).
//!
//! `revert` and `revert-layer` are not supported (the declaration is ignored).

use std::rc::Rc;

use cssparser::Parser;

use super::computed::ComputedStyle;
use super::properties::PropertyDeclaration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CssWideKeyword {
    Initial,
    Inherit,
    Unset,
}

impl CssWideKeyword {
    /// The keyword `text` spells, ignoring ASCII case and surrounding whitespace.
    pub fn from_text(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.eq_ignore_ascii_case("inherit") {
            Some(Self::Inherit)
        } else if text.eq_ignore_ascii_case("initial") {
            Some(Self::Initial)
        } else if text.eq_ignore_ascii_case("unset") {
            Some(Self::Unset)
        } else {
            None
        }
    }

    /// Parse a value that is exactly one keyword. `input` is left untouched otherwise.
    pub fn parse(input: &mut Parser<'_, '_>) -> Option<Self> {
        input
            .try_parse(|input| -> Result<Self, ()> {
                let keyword = input
                    .expect_ident()
                    .ok()
                    .and_then(|ident| Self::from_text(ident))
                    .ok_or(())?;
                input.expect_exhausted().map_err(|_| ())?;
                Ok(keyword)
            })
            .ok()
    }
}

/// A declaration such as `box-sizing: inherit` or `margin: unset`.
#[derive(Debug, Clone, PartialEq)]
pub struct CssWideDeclaration {
    pub keyword: CssWideKeyword,
    /// One placeholder per longhand the property expands to.
    pub longhands: Rc<[PropertyDeclaration]>,
}

/// Whether the style computation inherits `property` when it is not declared. This follows
/// what `compute_element_style` does, so the two always agree.
pub(super) fn is_inherited(property: &PropertyDeclaration) -> bool {
    use PropertyDeclaration as D;
    matches!(
        property,
        D::FontSize(_)
            | D::FontFamily(_)
            | D::FontWeight(_)
            | D::FontStyle(_)
            | D::Color(_)
            | D::TextDecorationLine(_)
            | D::TextAlign(_)
            | D::LineHeight(_)
            | D::TextIndent(_)
            | D::WhiteSpace(_)
            | D::LetterSpacing(_)
            | D::WordSpacing(_)
            | D::TextTransform(_)
            | D::TextShadow(_)
            | D::WordBreak(_)
            | D::OverflowWrap(_)
            | D::Hyphens(_)
            | D::TextEmphasisStyle(_)
            | D::TextEmphasisColor(_)
            | D::TextEmphasisPosition(_)
            | D::BorderCollapse(_)
            | D::BorderSpacing(..)
            | D::CaptionSide(_)
            | D::EmptyCells(_)
            | D::ListStyleType(_)
            | D::ListStylePosition(_)
            | D::ListStyleImage(_)
            | D::Visibility(_)
            | D::Quotes(_)
    )
}

/// Copy the computed value of the longhand `property` names from `src` to `dst`.
///
/// The match has no wildcard arm on purpose: a new property has to be given a line here.
pub(super) fn copy_computed(
    dst: &mut ComputedStyle,
    src: &ComputedStyle,
    property: &PropertyDeclaration,
) {
    use PropertyDeclaration as D;
    match property {
        D::Display(_) => dst.display = src.display,
        D::Width(_) => dst.width = src.width,
        D::Height(_) => dst.height = src.height,
        D::MinWidth(_) => dst.min_width = src.min_width,
        D::MinHeight(_) => dst.min_height = src.min_height,
        D::MaxWidth(_) => dst.max_width = src.max_width,
        D::MaxHeight(_) => dst.max_height = src.max_height,
        D::AspectRatio(_) => dst.aspect_ratio = src.aspect_ratio,
        D::MarginTop(_) => dst.margin_top = src.margin_top,
        D::MarginRight(_) => dst.margin_right = src.margin_right,
        D::MarginBottom(_) => dst.margin_bottom = src.margin_bottom,
        D::MarginLeft(_) => dst.margin_left = src.margin_left,
        D::PaddingTop(_) => dst.padding_top = src.padding_top,
        D::PaddingRight(_) => dst.padding_right = src.padding_right,
        D::PaddingBottom(_) => dst.padding_bottom = src.padding_bottom,
        D::PaddingLeft(_) => dst.padding_left = src.padding_left,
        D::BorderTopWidth(_) => dst.border_top_width = src.border_top_width,
        D::BorderRightWidth(_) => dst.border_right_width = src.border_right_width,
        D::BorderBottomWidth(_) => dst.border_bottom_width = src.border_bottom_width,
        D::BorderLeftWidth(_) => dst.border_left_width = src.border_left_width,
        D::BorderTopColor(_) => dst.border_top_color = src.border_top_color,
        D::BorderRightColor(_) => dst.border_right_color = src.border_right_color,
        D::BorderBottomColor(_) => dst.border_bottom_color = src.border_bottom_color,
        D::BorderLeftColor(_) => dst.border_left_color = src.border_left_color,
        D::BorderTopStyle(_) => dst.border_top_style = src.border_top_style,
        D::BorderRightStyle(_) => dst.border_right_style = src.border_right_style,
        D::BorderBottomStyle(_) => dst.border_bottom_style = src.border_bottom_style,
        D::BorderLeftStyle(_) => dst.border_left_style = src.border_left_style,
        D::BorderTopLeftRadius(_) => dst.border_top_left_radius = src.border_top_left_radius,
        D::BorderTopRightRadius(_) => dst.border_top_right_radius = src.border_top_right_radius,
        D::BorderBottomRightRadius(_) => {
            dst.border_bottom_right_radius = src.border_bottom_right_radius
        }
        D::BorderBottomLeftRadius(_) => {
            dst.border_bottom_left_radius = src.border_bottom_left_radius
        }
        D::FontSize(_) => dst.font_size = src.font_size,
        D::FontFamily(_) => dst.font_family = src.font_family.clone(),
        D::FontWeight(_) => dst.font_weight = src.font_weight,
        D::FontStyle(_) => dst.font_style = src.font_style,
        D::Color(_) => dst.color = src.color,
        D::BackgroundColor(_) => dst.background_color = src.background_color,
        D::BackgroundImage(_) => dst.background_image = src.background_image.clone(),
        D::BackgroundPosition(_) => dst.background_position = src.background_position,
        D::BackgroundSize(_) => dst.background_size = src.background_size,
        D::BackgroundRepeat(_) => dst.background_repeat = src.background_repeat,
        D::BackgroundAttachment(_) => dst.background_attachment = src.background_attachment,
        D::TextDecorationLine(_) => dst.text_decoration_line = src.text_decoration_line,
        D::BreakBefore(_) => dst.break_before = src.break_before,
        D::BreakAfter(_) => dst.break_after = src.break_after,
        D::BreakInside(_) => dst.break_inside = src.break_inside,
        D::Orphans(_) => dst.orphans = src.orphans,
        D::Widows(_) => dst.widows = src.widows,
        D::BookmarkLevel(_) => dst.bookmark_level = src.bookmark_level,
        D::BookmarkLabel(_) => dst.bookmark_label = src.bookmark_label.clone(),
        D::BookmarkState(_) => dst.bookmark_state = src.bookmark_state,
        D::Float(_) => dst.float = src.float,
        D::Clear(_) => dst.clear = src.clear,
        D::Position(_) => dst.position = src.position,
        D::Top(_) => dst.top = src.top,
        D::Right(_) => dst.right = src.right,
        D::Bottom(_) => dst.bottom = src.bottom,
        D::Left(_) => dst.left = src.left,
        D::TextAlign(_) => dst.text_align = src.text_align,
        D::LineHeight(_) => dst.line_height = src.line_height,
        D::TextIndent(_) => dst.text_indent = src.text_indent,
        D::WhiteSpace(_) => dst.white_space = src.white_space,
        D::LetterSpacing(_) => dst.letter_spacing = src.letter_spacing,
        D::WordSpacing(_) => dst.word_spacing = src.word_spacing,
        D::TextTransform(_) => dst.text_transform = src.text_transform,
        D::BorderCollapse(_) => dst.border_collapse = src.border_collapse,
        D::BorderSpacing(..) => {
            dst.border_spacing_horizontal = src.border_spacing_horizontal;
            dst.border_spacing_vertical = src.border_spacing_vertical;
        }
        D::CaptionSide(_) => dst.caption_side = src.caption_side,
        D::TableLayout(_) => dst.table_layout = src.table_layout,
        D::EmptyCells(_) => dst.empty_cells = src.empty_cells,
        D::VerticalAlign(_) => dst.vertical_align = src.vertical_align,
        D::ListStyleType(_) => dst.list_style_type = src.list_style_type,
        D::ListStylePosition(_) => dst.list_style_position = src.list_style_position,
        D::ListStyleImage(_) => dst.list_style_image = src.list_style_image.clone(),
        D::Overflow(_) => dst.overflow = src.overflow,
        D::BoxSizing(_) => dst.box_sizing = src.box_sizing,
        D::ZIndex(_) => dst.z_index = src.z_index,
        D::Visibility(_) => dst.visibility = src.visibility,
        D::OutlineWidth(_) => dst.outline_width = src.outline_width,
        D::OutlineStyle(_) => dst.outline_style = src.outline_style,
        D::OutlineColor(_) => dst.outline_color = src.outline_color,
        D::Quotes(_) => dst.quotes = src.quotes.clone(),
        D::ObjectFit(_) => dst.object_fit = src.object_fit,
        D::ObjectPosition(_) => dst.object_position = src.object_position,
        D::BoxShadow(_) => dst.box_shadow = src.box_shadow.clone(),
        D::FlexDirection(_) => dst.flex_direction = src.flex_direction,
        D::FlexWrap(_) => dst.flex_wrap = src.flex_wrap,
        D::JustifyContent(_) => dst.justify_content = src.justify_content,
        D::AlignItems(_) => dst.align_items = src.align_items,
        D::AlignContent(_) => dst.align_content = src.align_content,
        D::AlignSelf(_) => dst.align_self = src.align_self,
        D::FlexGrow(_) => dst.flex_grow = src.flex_grow,
        D::FlexShrink(_) => dst.flex_shrink = src.flex_shrink,
        D::FlexBasis(_) => dst.flex_basis = src.flex_basis,
        D::RowGap(_) => dst.row_gap = src.row_gap,
        D::ColumnGap(_) => dst.column_gap = src.column_gap,
        D::Transform(_) => dst.transform = src.transform.clone(),
        D::TransformOrigin(_) => dst.transform_origin = src.transform_origin,
        D::Opacity(_) => dst.opacity = src.opacity,
        D::TextShadow(_) => dst.text_shadow = src.text_shadow.clone(),
        D::TextOverflow(_) => dst.text_overflow = src.text_overflow,
        D::WordBreak(_) => dst.word_break = src.word_break,
        D::OverflowWrap(_) => dst.overflow_wrap = src.overflow_wrap,
        D::Hyphens(_) => dst.hyphens = src.hyphens,
        D::TextEmphasisStyle(_) => dst.text_emphasis_style = src.text_emphasis_style.clone(),
        D::TextEmphasisColor(_) => dst.text_emphasis_color = src.text_emphasis_color,
        D::TextEmphasisPosition(_) => dst.text_emphasis_position = src.text_emphasis_position,
        D::GridTemplateColumns(_) => dst.grid_template_columns = src.grid_template_columns.clone(),
        D::GridTemplateRows(_) => dst.grid_template_rows = src.grid_template_rows.clone(),
        D::GridAutoColumns(_) => dst.grid_auto_columns = src.grid_auto_columns.clone(),
        D::GridAutoRows(_) => dst.grid_auto_rows = src.grid_auto_rows.clone(),
        D::GridAutoFlow(_) => dst.grid_auto_flow = src.grid_auto_flow,
        D::GridTemplateAreas(_) => dst.grid_template_areas = src.grid_template_areas.clone(),
        D::GridRowStart(_) => dst.grid_row_start = src.grid_row_start.clone(),
        D::GridRowEnd(_) => dst.grid_row_end = src.grid_row_end.clone(),
        D::GridColumnStart(_) => dst.grid_column_start = src.grid_column_start.clone(),
        D::GridColumnEnd(_) => dst.grid_column_end = src.grid_column_end.clone(),
        D::JustifyItems(_) => dst.justify_items = src.justify_items,
        D::JustifySelf(_) => dst.justify_self = src.justify_self,
        // These have no computed value of their own in `ComputedStyle`. A keyword on a
        // counter property only clears the declaration, and `content` is settled where the
        // pseudo-element's content is resolved (`cascade::matching_pseudo_content`).
        D::Content(_) | D::CounterReset(_) | D::CounterIncrement(_) => {}
        D::Custom(_) | D::Unparsed(_) | D::CssWide(_) => {}
    }
}
