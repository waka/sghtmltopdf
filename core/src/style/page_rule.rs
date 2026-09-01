//! Parsing and resolving `@page` rules (the page's `size`/`margin` and its margin boxes).
//!
//! An `@page` block mixes ordinary property declarations (`size` and the `margin` family)
//! with the 16 margin box at-rules (`@top-left` and friends), so it uses its own
//! [`PageRuleParser`] rather than the existing parser in `stylesheet.rs`.
//!
//! The `style` crate is designed not to depend on the `layout` crate (the dependency runs
//! one way, [`crate::layout`] on [`crate::style`]), so the actual pixel values of the page
//! sizes (the conversion table for [`NamedPageSize`]) are kept independently in this file
//! (and must be kept in sync with the identically named constants in
//! `layout::page::PageSize`).

use std::collections::BTreeMap;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, QualifiedRuleParser,
    RuleBodyItemParser, RuleBodyParser,
};

use super::properties::{parse_declaration, parse_length, PropertyDeclaration};
use super::stylesheet::DeclarationBlockParser;
use super::values::{ContentPart, LengthPercentageOrAuto, SpecifiedLength};

/// The page selector (prelude) of `@page`. Named pages (`@page intro`) and `:blank` are
/// not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSelector {
    All,
    First,
    Left,
    Right,
}

/// A margin box area (there are 16).
///
/// `Ord` follows the declaration order of the variants (the order in the CSS spec). Holding
/// the margin boxes in a `BTreeMap` fixes the drawing order so it does not vary between runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MarginBoxArea {
    TopLeftCorner,
    TopLeft,
    TopCenter,
    TopRight,
    TopRightCorner,
    LeftTop,
    LeftMiddle,
    LeftBottom,
    RightTop,
    RightMiddle,
    RightBottom,
    BottomLeftCorner,
    BottomLeft,
    BottomCenter,
    BottomRight,
    BottomRightCorner,
}

impl MarginBoxArea {
    /// The mapping from the 16 margin box at-rule names.
    fn from_at_rule_name(name: &str) -> Option<Self> {
        use MarginBoxArea::*;
        let table: &[(&str, MarginBoxArea)] = &[
            ("top-left-corner", TopLeftCorner),
            ("top-left", TopLeft),
            ("top-center", TopCenter),
            ("top-right", TopRight),
            ("top-right-corner", TopRightCorner),
            ("left-top", LeftTop),
            ("left-middle", LeftMiddle),
            ("left-bottom", LeftBottom),
            ("right-top", RightTop),
            ("right-middle", RightMiddle),
            ("right-bottom", RightBottom),
            ("bottom-left-corner", BottomLeftCorner),
            ("bottom-left", BottomLeft),
            ("bottom-center", BottomCenter),
            ("bottom-right", BottomRight),
            ("bottom-right-corner", BottomRightCorner),
        ];
        table
            .iter()
            .find(|(candidate, _)| name.eq_ignore_ascii_case(candidate))
            .map(|(_, area)| *area)
    }
}

/// A named page size for the `size` property. `b4`/`b5`/`ledger` are not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedPageSize {
    A4,
    A3,
    A5,
    Letter,
    Legal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageOrientation {
    #[default]
    Portrait,
    Landscape,
}

/// The specified value of the `size` property.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PageSizeValue {
    Auto,
    Named(NamedPageSize, PageOrientation),
    Explicit(SpecifiedLength, SpecifiedLength),
}

/// One `@page` rule, exactly as parsed.
#[derive(Debug, Clone, Default)]
pub struct PageRule {
    pub selector_is_all: bool,
    pub selector: Option<PageSelector>,
    pub size: Option<PageSizeValue>,
    /// `margin`, `margin-top` and so on (every declaration directly under `@page` other
    /// than `size`). Only the margin family actually means anything, but parsing reuses
    /// `parse_declaration` as-is, so other properties are syntactically accepted
    pub margin: Vec<PropertyDeclaration>,
    pub margin_boxes: BTreeMap<MarginBoxArea, Vec<PropertyDeclaration>>,
}

/// One item inside an `@page` block. This groups them into the single output type
/// `RuleBodyItemParser` requires.
enum PageBodyItem {
    Size(PageSizeValue),
    Declarations(Vec<PropertyDeclaration>),
    MarginBox(MarginBoxArea, Vec<PropertyDeclaration>),
}

struct PageRuleParser;

impl<'i> DeclarationParser<'i> for PageRuleParser {
    type Declaration = PageBodyItem;
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &cssparser::ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        if name.eq_ignore_ascii_case("size") {
            return Ok(PageBodyItem::Size(parse_page_size(input)?));
        }
        Ok(PageBodyItem::Declarations(parse_declaration(&name, input)?))
    }
}

impl<'i> QualifiedRuleParser<'i> for PageRuleParser {
    type Prelude = ();
    type QualifiedRule = PageBodyItem;
    type Error = ();
}

impl<'i> AtRuleParser<'i> for PageRuleParser {
    type Prelude = MarginBoxArea;
    type AtRule = PageBodyItem;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        MarginBoxArea::from_at_rule_name(&name).ok_or_else(|| input.new_custom_error(()))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, Self::Error>> {
        let mut declaration_parser = DeclarationBlockParser;
        let declarations = RuleBodyParser::new(input, &mut declaration_parser)
            .filter_map(Result::ok)
            .flatten()
            .collect();
        Ok(PageBodyItem::MarginBox(prelude, declarations))
    }
}

impl<'i> RuleBodyItemParser<'i, PageBodyItem, ()> for PageRuleParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// Parse the prelude (page selector) of `@page`. Only a bare `:first`/`:left`/`:right` is
/// recognised (compound selectors and named pages are not supported).
pub(super) fn parse_page_selector<'i, 't>(
    input: &mut Parser<'i, 't>,
) -> Result<PageSelector, ParseError<'i, ()>> {
    if input.is_exhausted() {
        return Ok(PageSelector::All);
    }
    input.expect_colon()?;
    let ident = input.expect_ident()?.clone();
    if ident.eq_ignore_ascii_case("first") {
        Ok(PageSelector::First)
    } else if ident.eq_ignore_ascii_case("left") {
        Ok(PageSelector::Left)
    } else if ident.eq_ignore_ascii_case("right") {
        Ok(PageSelector::Right)
    } else {
        Err(input.new_custom_error(()))
    }
}

/// Parse the block body of `@page { ... }`.
pub(super) fn parse_page_rule_block<'i, 't>(
    input: &mut Parser<'i, 't>,
    selector: PageSelector,
) -> PageRule {
    let mut rule_parser = PageRuleParser;
    let mut rule = PageRule {
        selector_is_all: selector == PageSelector::All,
        selector: Some(selector),
        ..PageRule::default()
    };
    for item in RuleBodyParser::new(input, &mut rule_parser).filter_map(Result::ok) {
        match item {
            PageBodyItem::Size(size) => rule.size = Some(size),
            PageBodyItem::Declarations(decls) => rule.margin.extend(decls),
            PageBodyItem::MarginBox(area, decls) => {
                rule.margin_boxes.entry(area).or_default().extend(decls)
            }
        }
    }
    rule
}

/// `size`. `auto` | `<page-size> [portrait | landscape]?` | `<length>{1,2}`.
fn parse_page_size<'i>(input: &mut Parser<'i, '_>) -> Result<PageSizeValue, ParseError<'i, ()>> {
    if input
        .try_parse(|input| input.expect_ident_matching("auto"))
        .is_ok()
    {
        return Ok(PageSizeValue::Auto);
    }

    let mut named: Option<NamedPageSize> = None;
    let mut orientation: Option<PageOrientation> = None;
    while let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        if named.is_none() {
            let candidate = if ident.eq_ignore_ascii_case("a4") {
                Some(NamedPageSize::A4)
            } else if ident.eq_ignore_ascii_case("a3") {
                Some(NamedPageSize::A3)
            } else if ident.eq_ignore_ascii_case("a5") {
                Some(NamedPageSize::A5)
            } else if ident.eq_ignore_ascii_case("letter") {
                Some(NamedPageSize::Letter)
            } else if ident.eq_ignore_ascii_case("legal") {
                Some(NamedPageSize::Legal)
            } else {
                None
            };
            if let Some(candidate) = candidate {
                named = Some(candidate);
                continue;
            }
        }
        if orientation.is_none() {
            if ident.eq_ignore_ascii_case("portrait") {
                orientation = Some(PageOrientation::Portrait);
                continue;
            }
            if ident.eq_ignore_ascii_case("landscape") {
                orientation = Some(PageOrientation::Landscape);
                continue;
            }
        }
        return Err(input.new_custom_error(()));
    }
    if let Some(named) = named {
        return Ok(PageSizeValue::Named(named, orientation.unwrap_or_default()));
    }
    if orientation.is_some() {
        // A bare `portrait`/`landscape` is specified to require a `<page-size>` alongside it
        // (on its own it is invalid).
        return Err(input.new_custom_error(()));
    }

    let width = parse_length(input)?;
    let height = input.try_parse(parse_length).unwrap_or(width);
    Ok(PageSizeValue::Explicit(width, height))
}

/// The final result of merging several `@page` rules.
#[derive(Debug, Clone, Default)]
pub struct ResolvedPageRule {
    /// Width and height (px). Resolved once for the whole document (a size declaration
    /// under `:first`/`:left`/`:right` is not honoured).
    pub size_px: Option<(f32, f32)>,
    pub margin_top: Option<LengthPercentageOrAuto>,
    pub margin_right: Option<LengthPercentageOrAuto>,
    pub margin_bottom: Option<LengthPercentageOrAuto>,
    pub margin_left: Option<LengthPercentageOrAuto>,
    pub margin_boxes: BTreeMap<MarginBoxArea, Vec<PropertyDeclaration>>,
}

/// A simple cascade. Unconditional `@page{}` rules are folded in stylesheet order, then the
/// pseudo-class rules matching `is_first`/`is_left` are folded in for the margin boxes only
/// (`size`/`margin` are honoured only from unconditional rules).
pub fn resolve_page_rules(rules: &[PageRule], is_first: bool, is_left: bool) -> ResolvedPageRule {
    let mut result = ResolvedPageRule::default();

    for rule in rules.iter().filter(|r| r.selector_is_all) {
        if let Some(size) = rule.size {
            result.size_px = Some(resolve_page_size_px(size));
        }
        apply_margin_declarations(&mut result, &rule.margin);
        merge_margin_boxes(&mut result, &rule.margin_boxes);
    }

    for rule in rules.iter().filter(|r| !r.selector_is_all) {
        let applies = match rule.selector {
            Some(PageSelector::First) => is_first,
            Some(PageSelector::Left) => is_left,
            Some(PageSelector::Right) => !is_left,
            Some(PageSelector::All) | None => false,
        };
        if applies {
            merge_margin_boxes(&mut result, &rule.margin_boxes);
        }
    }

    result
}

/// Whether any margin box's `content` uses `counter(pages)` (including `counters(pages, ...)`).
/// The total page count cannot be known in principle under `Mode::Streaming`, so this
/// decides whether to return `EngineError::UnsupportedInStreamingMode`.
pub fn rules_use_page_count(rules: &[PageRule]) -> bool {
    rules.iter().any(|rule| {
        rule.margin_boxes.values().any(|decls| {
            decls.iter().any(|decl| match decl {
                PropertyDeclaration::Content(Some(parts)) => parts.iter().any(|part| {
                    matches!(
                        part,
                        ContentPart::Counter(name, _) | ContentPart::Counters(name, _, _)
                            if name == "pages"
                    )
                }),
                _ => false,
            })
        })
    })
}

fn apply_margin_declarations(result: &mut ResolvedPageRule, decls: &[PropertyDeclaration]) {
    // `em`/`rem` in an `@page` margin declaration is rare, but with no element to speak of
    // the reference font size is fixed to the initial value (16px).
    const NOMINAL_FONT_SIZE: f32 = 16.0;
    for decl in decls {
        match decl {
            PropertyDeclaration::MarginTop(v) => {
                result.margin_top = Some(v.resolve(NOMINAL_FONT_SIZE, NOMINAL_FONT_SIZE))
            }
            PropertyDeclaration::MarginRight(v) => {
                result.margin_right = Some(v.resolve(NOMINAL_FONT_SIZE, NOMINAL_FONT_SIZE))
            }
            PropertyDeclaration::MarginBottom(v) => {
                result.margin_bottom = Some(v.resolve(NOMINAL_FONT_SIZE, NOMINAL_FONT_SIZE))
            }
            PropertyDeclaration::MarginLeft(v) => {
                result.margin_left = Some(v.resolve(NOMINAL_FONT_SIZE, NOMINAL_FONT_SIZE))
            }
            _ => {}
        }
    }
}

fn merge_margin_boxes(
    result: &mut ResolvedPageRule,
    margin_boxes: &BTreeMap<MarginBoxArea, Vec<PropertyDeclaration>>,
) {
    for (area, decls) in margin_boxes {
        result
            .margin_boxes
            .entry(*area)
            .or_default()
            .extend(decls.iter().cloned());
    }
}

/// The same values as the identically named constants in `layout::page::PageSize` (at
/// 96dpi). This function keeps a copy of them here so the `style` crate stays free of any
/// dependency on `layout`.
fn resolve_page_size_px(size: PageSizeValue) -> (f32, f32) {
    const NOMINAL_FONT_SIZE: f32 = 16.0;
    let (w, h) = match size {
        PageSizeValue::Auto => return (793.7, 1122.5), // auto defaults to the A4 equivalent
        PageSizeValue::Named(named, _) => match named {
            NamedPageSize::A4 => (793.7, 1122.5),
            NamedPageSize::A3 => (1122.5, 1587.4),
            NamedPageSize::A5 => (559.4, 793.7),
            NamedPageSize::Letter => (816.0, 1056.0),
            NamedPageSize::Legal => (816.0, 1344.0),
        },
        PageSizeValue::Explicit(w, h) => (
            w.resolve(NOMINAL_FONT_SIZE, NOMINAL_FONT_SIZE).0,
            h.resolve(NOMINAL_FONT_SIZE, NOMINAL_FONT_SIZE).0,
        ),
    };
    if let PageSizeValue::Named(_, PageOrientation::Landscape) = size {
        (h, w)
    } else {
        (w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{parse_stylesheet, LengthPercentage};

    #[test]
    fn page_rule_parses_size_and_margin() {
        let sheet = parse_stylesheet("@page { size: a4 landscape; margin: 48px; }");
        assert_eq!(sheet.page_rules.len(), 1);
        let rule = &sheet.page_rules[0];
        assert!(rule.selector_is_all);
        assert_eq!(
            rule.size,
            Some(PageSizeValue::Named(
                NamedPageSize::A4,
                PageOrientation::Landscape
            ))
        );
        assert_eq!(
            rule.margin.len(),
            4,
            "margin shorthand should expand to 4 longhands"
        );
    }

    #[test]
    fn page_rule_parses_explicit_two_value_size() {
        let sheet = parse_stylesheet("@page { size: 300px 400px; }");
        let rule = &sheet.page_rules[0];
        assert!(matches!(rule.size, Some(PageSizeValue::Explicit(_, _))));
    }

    #[test]
    fn page_rule_accepts_physical_units_for_size_and_margin() {
        // The form usually written in print CSS. Giving A4 at real size matches the named `a4`.
        let sheet = parse_stylesheet("@page { size: 210mm 297mm; margin: 0.5in; }");
        let resolved = resolve_page_rules(&sheet.page_rules, false, false);
        let named = resolve_page_rules(
            &parse_stylesheet("@page { size: a4; }").page_rules,
            false,
            false,
        );

        let (width, height) = resolved.size_px.expect("size was not resolved");
        let (named_width, named_height) = named.size_px.expect("size was not resolved");
        assert!(
            (width - named_width).abs() < 0.5,
            "{width} vs {named_width}"
        );
        assert!(
            (height - named_height).abs() < 0.5,
            "{height} vs {named_height}"
        );
        // 0.5in = 48px.
        assert_eq!(
            resolved.margin_top,
            Some(LengthPercentageOrAuto::LengthPercentage(
                LengthPercentage::Length(48.0)
            ))
        );
    }

    #[test]
    fn page_rule_recognizes_pseudo_class_selectors() {
        for (css, expected) in [
            ("@page :first { margin: 0; }", PageSelector::First),
            ("@page :left { margin: 0; }", PageSelector::Left),
            ("@page :right { margin: 0; }", PageSelector::Right),
        ] {
            let sheet = parse_stylesheet(css);
            assert_eq!(sheet.page_rules.len(), 1, "css={css}");
            assert_eq!(sheet.page_rules[0].selector, Some(expected), "css={css}");
            assert!(!sheet.page_rules[0].selector_is_all, "css={css}");
        }
    }

    #[test]
    fn page_rule_rejects_named_pages_and_combined_pseudo_classes() {
        // Named pages and compound pseudo-classes are not supported. An @page rule that
        // fails to parse is ignored and does not affect the rules that follow.
        for css in [
            "@page intro { margin: 0; }",
            "@page :first:left { margin: 0; }",
        ] {
            let sheet = parse_stylesheet(&format!("{css} div {{ color: rgb(1, 2, 3); }}"));
            assert!(sheet.page_rules.is_empty(), "css={css}");
        }
    }

    #[test]
    fn page_rule_parses_margin_box_content() {
        let sheet = parse_stylesheet(
            r#"@page { @top-center { content: "Hello"; } @bottom-right { content: counter(page); } }"#,
        );
        let rule = &sheet.page_rules[0];
        assert!(rule.margin_boxes.contains_key(&MarginBoxArea::TopCenter));
        assert!(rule.margin_boxes.contains_key(&MarginBoxArea::BottomRight));
        assert_eq!(rule.margin_boxes.len(), 2);
    }

    #[test]
    fn page_rule_parses_all_sixteen_margin_box_names() {
        let names = [
            "top-left-corner",
            "top-left",
            "top-center",
            "top-right",
            "top-right-corner",
            "left-top",
            "left-middle",
            "left-bottom",
            "right-top",
            "right-middle",
            "right-bottom",
            "bottom-left-corner",
            "bottom-left",
            "bottom-center",
            "bottom-right",
            "bottom-right-corner",
        ];
        let css = names
            .iter()
            .map(|name| format!("@{name} {{ content: \"x\"; }}"))
            .collect::<String>();
        let sheet = parse_stylesheet(&format!("@page {{ {css} }}"));
        assert_eq!(sheet.page_rules[0].margin_boxes.len(), 16);
    }

    #[test]
    fn resolve_page_rules_uses_only_unconditional_rules_for_size_and_margin() {
        let sheet = parse_stylesheet(
            "@page { size: 300px 400px; margin: 10px; } \
             @page :first { size: 999px 999px; margin: 999px; }",
        );
        let resolved = resolve_page_rules(&sheet.page_rules, true, false);
        // The size/margin under :first are not honoured.
        assert_eq!(resolved.size_px, Some((300.0, 400.0)));
        assert_eq!(
            resolved.margin_top,
            Some(LengthPercentageOrAuto::LengthPercentage(
                crate::style::LengthPercentage::Length(10.0)
            ))
        );
    }

    #[test]
    fn resolve_page_rules_merges_margin_boxes_by_page_context() {
        let sheet = parse_stylesheet(
            r#"@page { @bottom-center { content: "default"; } }
               @page :first { @bottom-center { content: none; } @top-center { content: "cover"; } }"#,
        );
        let first_page = resolve_page_rules(&sheet.page_rules, true, false);
        let other_page = resolve_page_rules(&sheet.page_rules, false, false);

        // On the :first page @bottom-center is overridden (later wins), so both the
        // unconditional rule's content declaration and the :first one are present, in that
        // order (which of them actually applies is the content resolver's job).
        let first_bottom_center = &first_page.margin_boxes[&MarginBoxArea::BottomCenter];
        assert_eq!(first_bottom_center.len(), 2);
        assert!(first_page
            .margin_boxes
            .contains_key(&MarginBoxArea::TopCenter));

        // Other pages have no :first-only @top-center.
        assert!(!other_page
            .margin_boxes
            .contains_key(&MarginBoxArea::TopCenter));
        assert_eq!(
            other_page.margin_boxes[&MarginBoxArea::BottomCenter].len(),
            1
        );
    }

    #[test]
    fn resolve_page_rules_left_and_right_are_mutually_exclusive_based_on_parity() {
        let sheet = parse_stylesheet(
            r#"@page :left { @top-left { content: "L"; } }
               @page :right { @top-right { content: "R"; } }"#,
        );
        let left_page = resolve_page_rules(&sheet.page_rules, false, true);
        let right_page = resolve_page_rules(&sheet.page_rules, false, false);

        assert!(left_page.margin_boxes.contains_key(&MarginBoxArea::TopLeft));
        assert!(!left_page
            .margin_boxes
            .contains_key(&MarginBoxArea::TopRight));
        assert!(right_page
            .margin_boxes
            .contains_key(&MarginBoxArea::TopRight));
        assert!(!right_page
            .margin_boxes
            .contains_key(&MarginBoxArea::TopLeft));
    }

    #[test]
    fn resolve_page_rules_with_no_page_rules_leaves_everything_unset() {
        let resolved = resolve_page_rules(&[], true, false);
        assert_eq!(resolved.size_px, None);
        assert_eq!(resolved.margin_top, None);
        assert!(resolved.margin_boxes.is_empty());
    }
}
