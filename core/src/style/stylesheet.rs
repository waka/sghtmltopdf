//! Parsing a whole stylesheet (a set of rules).

use std::cell::RefCell;
use std::rc::Rc;

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserInput,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser, Token,
};
use selectors::parser::{
    Combinator, Component, NthSelectorData, NthType, ParseRelative, Selector, SelectorList,
};

use super::font_face::{parse_font_face_block, FontFaceRule};
use super::page_rule::{parse_page_rule_block, parse_page_selector, PageRule};
use super::properties::{parse_declaration, PropertyDeclaration};
use super::rule_index::RuleIndex;
use super::selector_impl::{SelectorParser, SgSelectorImpl};

#[derive(Debug, Clone)]
pub struct StyleRule {
    pub selectors: SelectorList<SgSelectorImpl>,
    pub declarations: Vec<PropertyDeclaration>,
}

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<StyleRule>,
    pub font_faces: Vec<FontFaceRule>,
    pub page_rules: Vec<PageRule>,
    /// Memo of the index [`Self::index`] builds.
    index: RefCell<Option<Rc<RuleIndex>>>,
}

impl Stylesheet {
    /// The index that narrows the candidates for selector matching. Built on first use.
    ///
    /// `rules` is a public field and can be appended to after parsing (the path that adds
    /// user CSS onto the end of the UA stylesheet). Forgetting to discard the index on such
    /// an append would leave a stale one, so it is rebuilt whenever the rule count changed.
    pub fn index(&self) -> Rc<RuleIndex> {
        if let Some(index) = self.index.borrow().as_ref() {
            if index.rule_count() == self.rules.len() {
                return Rc::clone(index);
            }
        }
        let built = Rc::new(RuleIndex::build(&self.rules));
        *self.index.borrow_mut() = Some(Rc::clone(&built));
        built
    }
}

/// The intermediate representation of a top-level rule. Ordinary style rules, `@font-face`,
/// `@media` and `@page` all have to share the same `Prelude`/`Rule` types under
/// `StyleSheetParser`'s type system, so this enum bundles them
/// (and [`parse_stylesheet`] sorts them out).
enum TopLevelRule {
    /// One style rule plus the rules nested inside it, flattened into cascade order
    /// ([`parse_style_rule_body`]).
    Style(Vec<StyleRule>),
    FontFace(FontFaceRule),
    /// The contents of an `@media` (an empty Vec if it did not match).
    Media(Vec<TopLevelRule>),
    Page(PageRule),
}

pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut rule_parser = TopLevelRuleParser;

    let mut rules = Vec::new();
    let mut font_faces = Vec::new();
    let mut page_rules = Vec::new();
    for result in StyleSheetParser::new(&mut parser, &mut rule_parser).flatten() {
        flatten_top_level_rule(result, &mut rules, &mut font_faces, &mut page_rules);
    }

    Stylesheet {
        rules,
        font_faces,
        page_rules,
        index: RefCell::new(None),
    }
}

fn flatten_top_level_rule(
    rule: TopLevelRule,
    rules: &mut Vec<StyleRule>,
    font_faces: &mut Vec<FontFaceRule>,
    page_rules: &mut Vec<PageRule>,
) {
    match rule {
        // A rule with no declarations contributes to neither the cascade nor pseudo-element
        // resolution, leaving only the cost of indexing and matching. Drop it here.
        // The parent of a nested rule (the `.a` in `.a { &:hover { } }`) often has no
        // declarations, and keeping it would double the rules for every nesting.
        TopLevelRule::Style(r) => {
            rules.extend(r.into_iter().filter(|rule| !rule.declarations.is_empty()))
        }
        TopLevelRule::FontFace(r) => font_faces.push(r),
        TopLevelRule::Page(r) => page_rules.push(r),
        TopLevelRule::Media(inner) => {
            for r in inner {
                flatten_top_level_rule(r, rules, font_faces, page_rules);
            }
        }
    }
}

/// Parse a declaration list with no selector, such as the value of a `style="..."` attribute.
pub fn parse_inline_style(css: &str) -> Vec<PropertyDeclaration> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut declaration_parser = DeclarationBlockParser;

    RuleBodyParser::new(&mut parser, &mut declaration_parser)
        .filter_map(Result::ok)
        .flatten()
        .collect()
}

/// Parse a rule directly under the stylesheet (a selector plus a declaration block).
struct TopLevelRuleParser;

impl<'i> QualifiedRuleParser<'i> for TopLevelRuleParser {
    type Prelude = SelectorList<SgSelectorImpl>;
    type QualifiedRule = TopLevelRule;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        SelectorList::parse(&SelectorParser, input, ParseRelative::No)
            .map_err(|_| input.new_custom_error(()))
    }

    fn parse_block<'t>(
        &mut self,
        selectors: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, Self::Error>> {
        Ok(TopLevelRule::Style(parse_style_rule_body(selectors, input)))
    }
}

/// Parse the contents of a style rule's `{ }` (declarations and nested style rules) into a
/// flat list of rules in cascade order.
///
/// Under CSS Nesting, a nested rule is treated as though placed immediately after the
/// parent rule (in source order). Declarations written after a nested rule become a separate
/// rule with the same selector as the parent (CSSNestedDeclarations in the spec), placed
/// after that nested rule. Hoisting them to the front would make them win or lose against a
/// rule that overrides the parent via `&` in a way that contradicts source order, so the written order is preserved.
///
/// The first rule (the parent itself) and the trailing declaration rule come back empty if
/// they have no declarations. [`flatten_top_level_rule`] drops rules with no declarations.
fn parse_style_rule_body(
    selectors: SelectorList<SgSelectorImpl>,
    input: &mut Parser<'_, '_>,
) -> Vec<StyleRule> {
    let mut rules = vec![StyleRule {
        selectors: selectors.clone(),
        declarations: Vec::new(),
    }];
    // Where declarations are accepted. Reset to `None` whenever a nested rule intervenes, so
    // that the next declaration starts a new rule with the same selector as `&`.
    let mut open: Option<usize> = Some(0);
    // Declarations written after a nested rule (CSSNestedDeclarations in the spec) have the
    // same selector as `&`. When the parent is a selector list they collapse into `:is(parent)`,
    // whose specificity differs from the parent's own, so the resolved form is prepared separately.
    let mut nested_selectors: Option<SelectorList<SgSelectorImpl>> = None;

    let mut body_parser = StyleRuleBodyParser { parent: &selectors };
    for item in RuleBodyParser::new(input, &mut body_parser).filter_map(Result::ok) {
        match item {
            StyleRuleBodyItem::Declarations(declarations) => {
                let index = *open.get_or_insert_with(|| {
                    let selectors = nested_selectors
                        .get_or_insert_with(|| parent_selector_reference(&selectors))
                        .clone();
                    rules.push(StyleRule {
                        selectors,
                        declarations: Vec::new(),
                    });
                    rules.len() - 1
                });
                rules[index].declarations.extend(declarations);
            }
            StyleRuleBodyItem::Nested(nested) => {
                rules.extend(nested);
                open = None;
            }
        }
    }
    rules
}

/// The selector list with `&` (the parent selector) resolved against `parent`.
///
/// With only one parent, wrapping in `:is()` does not change the specificity, so it is
/// returned unchanged (this keeps the emitted selectors from changing needlessly).
fn parent_selector_reference(
    parent: &SelectorList<SgSelectorImpl>,
) -> SelectorList<SgSelectorImpl> {
    if parent.slice().len() == 1 {
        return parent.clone();
    }
    let mut input = ParserInput::new("&");
    let mut parser = Parser::new(&mut input);
    match SelectorList::parse(&SelectorParser, &mut parser, ParseRelative::ForNesting) {
        Ok(list) => list.replace_parent_selector(parent),
        Err(_) => parent.clone(),
    }
}

/// One item of a rule body, as returned by [`StyleRuleBodyParser`].
enum StyleRuleBodyItem {
    /// One declaration (or several, once a shorthand is expanded).
    Declarations(Vec<PropertyDeclaration>),
    /// A nested style rule (and its own nesting), flattened.
    Nested(Vec<StyleRule>),
}

/// Parse the contents of a style rule's `{ }`. In addition to declarations, it accepts
/// nested style rules (CSS Nesting).
///
/// A nested rule's selector is parsed as a relative selector that may contain `&` (the
/// parent selector), and `&` is resolved by substituting the parent's selector list
/// (equivalent to `:is(parent)`). A `.probe { }` written without `&` is treated as
/// `& .probe`, and a `> li { }` starting with a combinator as `& > li` (`ParseRelative::ForNesting`).
///
/// Nested at-rules (`@media` and friends) are not supported and are skipped block and all.
struct StyleRuleBodyParser<'a> {
    parent: &'a SelectorList<SgSelectorImpl>,
}

impl<'i> DeclarationParser<'i> for StyleRuleBodyParser<'_> {
    type Declaration = StyleRuleBodyItem;
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &cssparser::ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        parse_declaration(&name, input).map(StyleRuleBodyItem::Declarations)
    }
}

impl<'i> QualifiedRuleParser<'i> for StyleRuleBodyParser<'_> {
    type Prelude = SelectorList<SgSelectorImpl>;
    type QualifiedRule = StyleRuleBodyItem;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        let relative = SelectorList::parse(&SelectorParser, input, ParseRelative::ForNesting)
            .map_err(|_| input.new_custom_error(()))?;
        Ok(relative.replace_parent_selector(self.parent))
    }

    fn parse_block<'t>(
        &mut self,
        selectors: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::QualifiedRule, ParseError<'i, Self::Error>> {
        Ok(StyleRuleBodyItem::Nested(parse_style_rule_body(
            selectors, input,
        )))
    }
}

impl<'i> AtRuleParser<'i> for StyleRuleBodyParser<'_> {
    type Prelude = ();
    type AtRule = StyleRuleBodyItem;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, StyleRuleBodyItem, ()> for StyleRuleBodyParser<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        true
    }
}

/// Recognises `@font-face`, `@media` and `@page`.
enum TopLevelAtRulePrelude {
    FontFace,
    /// `applies` is what [`media_query_list_matches`] decided.
    Media {
        applies: bool,
    },
    Page(super::page_rule::PageSelector),
}

impl<'i> AtRuleParser<'i> for TopLevelRuleParser {
    type Prelude = TopLevelAtRulePrelude;
    type AtRule = TopLevelRule;
    type Error = ();

    fn parse_prelude<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::Prelude, ParseError<'i, Self::Error>> {
        if name.eq_ignore_ascii_case("font-face") {
            return Ok(TopLevelAtRulePrelude::FontFace);
        }
        if name.eq_ignore_ascii_case("media") {
            let applies = media_query_list_matches(input)?;
            return Ok(TopLevelAtRulePrelude::Media { applies });
        }
        if name.eq_ignore_ascii_case("page") {
            let selector = parse_page_selector(input)?;
            return Ok(TopLevelAtRulePrelude::Page(selector));
        }
        Err(input.new_custom_error(()))
    }

    fn parse_block<'t>(
        &mut self,
        prelude: Self::Prelude,
        _start: &cssparser::ParserState,
        input: &mut Parser<'i, 't>,
    ) -> Result<Self::AtRule, ParseError<'i, Self::Error>> {
        match prelude {
            TopLevelAtRulePrelude::FontFace => {
                Ok(TopLevelRule::FontFace(parse_font_face_block(input)?))
            }
            TopLevelAtRulePrelude::Page(selector) => {
                Ok(TopLevelRule::Page(parse_page_rule_block(input, selector)))
            }
            // The contents of an `@media` block that did not match need only be skipped
            // (`input` is already scoped to this block, so returning without consuming
            // anything still lets the caller advance correctly to the end of the block).
            TopLevelAtRulePrelude::Media { applies: false } => Ok(TopLevelRule::Media(Vec::new())),
            TopLevelAtRulePrelude::Media { applies: true } => {
                let mut rule_parser = TopLevelRuleParser;
                let rules = StyleSheetParser::new(input, &mut rule_parser)
                    .flatten()
                    .collect();
                Ok(TopLevelRule::Media(rules))
            }
        }
    }
}

/// Decide, from an `@media` prelude (a token sequence), a simplified media type check for
/// print/PDF output. The comma-separated query list (an OR) is decided per query, looking
/// only at the leading `not`/`only` modifier plus the media type identifier.
/// Feature queries (`(min-width: ...)` and the like) are skipped without being evaluated at all.
fn media_query_list_matches<'i, 't>(
    input: &mut Parser<'i, 't>,
) -> Result<bool, ParseError<'i, ()>> {
    let mut any_matches = false;
    loop {
        let (matches, has_more) = parse_one_media_query(input)?;
        any_matches = any_matches || matches;
        if !has_more {
            break;
        }
    }
    Ok(any_matches)
}

/// Decide one media query (up to the next comma, or the end of the prelude) and consume its
/// tokens. The feature query part is skipped without being evaluated. Returns
/// `(whether this query matched, whether more queries follow)`.
fn parse_one_media_query<'i, 't>(
    input: &mut Parser<'i, 't>,
) -> Result<(bool, bool), ParseError<'i, ()>> {
    let mut negate = false;
    let mut media_type: Option<CowRcStr<'i>> = None;

    if let Ok(ident) = input.try_parse(|input| input.expect_ident_cloned()) {
        if ident.eq_ignore_ascii_case("not") {
            negate = true;
            media_type = input.try_parse(|input| input.expect_ident_cloned()).ok();
        } else if ident.eq_ignore_ascii_case("only") {
            media_type = input.try_parse(|input| input.expect_ident_cloned()).ok();
        } else {
            media_type = Some(ident);
        }
    }

    // Skip the rest (`and (min-width: ...)` and so on) without evaluating it, up to the next
    // comma (consuming it to signal "more follow") or the end of the prelude.
    let has_more = loop {
        match input.next() {
            Ok(Token::Comma) => break true,
            Ok(_) => continue,
            Err(_) => break false,
        }
    };

    let is_screen = media_type
        .as_deref()
        .map(|ty| ty.eq_ignore_ascii_case("screen"))
        .unwrap_or(false);
    Ok((is_screen == negate, has_more))
}

/// Parse only the declarations inside `{ }` (nested rules are not handled).
/// Used by the `style` attribute and by `@page` margin boxes (`page_rule.rs`). A style
/// rule's body is parsed by [`StyleRuleBodyParser`], which also accepts nested rules.
pub(super) struct DeclarationBlockParser;

impl<'i> DeclarationParser<'i> for DeclarationBlockParser {
    type Declaration = Vec<PropertyDeclaration>;
    type Error = ();

    fn parse_value<'t>(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i, 't>,
        _declaration_start: &cssparser::ParserState,
    ) -> Result<Self::Declaration, ParseError<'i, Self::Error>> {
        parse_declaration(&name, input)
    }
}

impl<'i> QualifiedRuleParser<'i> for DeclarationBlockParser {
    type Prelude = ();
    type QualifiedRule = Vec<PropertyDeclaration>;
    type Error = ();
}

impl<'i> AtRuleParser<'i> for DeclarationBlockParser {
    type Prelude = ();
    type AtRule = Vec<PropertyDeclaration>;
    type Error = ();
}

impl<'i> RuleBodyItemParser<'i, Vec<PropertyDeclaration>, ()> for DeclarationBlockParser {
    fn parse_declarations(&self) -> bool {
        true
    }

    fn parse_qualified(&self) -> bool {
        false
    }
}

/// Selectors that need the preceding sibling to be decided.
///
/// Streaming frees top-level elements once processed, but in a document using these the
/// freeing has to be limited to the descendants, leaving the element itself visible as a
/// sibling ([`crate::html::Dom::release_descendants`]).
const NEEDS_PRECEDING: &[&str] = &[
    "+",
    "~",
    ":first-child",
    ":nth-child()",
    ":first-of-type",
    ":nth-of-type()",
    ":only-child",
    ":only-of-type",
];

/// Selectors that still cannot be decided even with the preceding sibling kept.
///
/// All of them need to know "whether more elements of the same type follow", but a top-level
/// element is only final once the next sibling appears, so what comes after that is unknown.
const STILL_UNSAFE: &[&str] = &[
    ":last-of-type",
    ":only-of-type",
    ":nth-last-child()",
    ":nth-last-of-type()",
    ":has(~ ...)",
];

/// Whether the stylesheet contains a selector that needs the preceding sibling.
///
/// If it does, streaming limits the freeing of a top-level element to its descendants.
/// If not, the whole subtree can be freed as before.
///
/// Keeping them only when they are used matters because kept nodes cannot be freed and
/// accumulate. Measured over 200,000 top-level elements, peak RSS went from 89.5MB to
/// 93.2MB, about 19 bytes per element. That accumulation can still hit
/// [`crate::html::MAX_NODES`], so in a document with more top-level elements than that,
/// using these selectors produces a node limit error (without them the limit is not hit, as before).
pub fn needs_preceding_siblings(sheet: &Stylesheet) -> bool {
    scan_sheet(sheet)
        .iter()
        .any(|name| NEEDS_PRECEDING.contains(&name.as_str()))
}

/// Return the names of any selectors used that make `Mode::Streaming` differ from batch even
/// with the preceding sibling kept.
///
/// A top-level element is only final once the next sibling appears, so whether more elements
/// of the same type follow is unknown. Anything needing that either matches too much or
/// misses matches. The caller uses this to warn rather than let the result change silently.
///
/// Conversely `:last-child`, `:empty` and the descendant or next-sibling forms of `:has()`
/// coincide with the condition for being final, give the same result as batch, and are not listed.
pub fn streaming_unsafe_selectors(sheet: &Stylesheet) -> Vec<String> {
    scan_sheet(sheet)
        .into_iter()
        .filter(|name| STILL_UNSAFE.contains(&name.as_str()))
        .collect()
}

/// Collect, without duplicates, the names of the sibling-dependent selectors in a stylesheet.
fn scan_sheet(sheet: &Stylesheet) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for rule in &sheet.rules {
        for selector in rule.selectors.slice() {
            scan_selector(selector, &mut found);
        }
    }
    found
}

fn scan_selector(selector: &Selector<SgSelectorImpl>, found: &mut Vec<String>) {
    for component in selector.iter_raw_match_order() {
        match component {
            Component::Combinator(Combinator::NextSibling) => push_once(found, "+"),
            Component::Combinator(Combinator::LaterSibling) => push_once(found, "~"),
            Component::Nth(data) => push_once(found, nth_name(data)),
            // The arguments follow the same rules, so descend into them.
            Component::Is(list) | Component::Where(list) | Component::Negation(list) => {
                for inner in list.slice() {
                    scan_selector(inner, found);
                }
            }
            // Inside `:has()`, only `~` (every following sibling) is a problem. Descendants,
            // `>` and `+` are all visible by the time an element is final.
            Component::Has(relatives) => {
                for relative in relatives.iter() {
                    if relative
                        .selector
                        .iter_raw_match_order()
                        .any(|c| matches!(c, Component::Combinator(Combinator::LaterSibling)))
                    {
                        push_once(found, ":has(~ ...)");
                    }
                }
            }
            _ => {}
        }
    }
}

/// Return by name only those that need a sibling before or after.
/// `:last-child` alone (a `LastChild` with `is_function` false) is safe, coinciding with the
/// condition for being final. Return an empty string to exclude it.
fn nth_name(data: &NthSelectorData) -> &'static str {
    match (data.ty, data.is_function) {
        (NthType::Child, false) => ":first-child",
        (NthType::Child, true) => ":nth-child()",
        (NthType::OfType, false) => ":first-of-type",
        (NthType::OfType, true) => ":nth-of-type()",
        (NthType::LastChild, false) => "",
        (NthType::LastChild, true) => ":nth-last-child()",
        (NthType::LastOfType, false) => ":last-of-type",
        (NthType::LastOfType, true) => ":nth-last-of-type()",
        (NthType::OnlyChild, _) => ":only-child",
        (NthType::OnlyOfType, _) => ":only-of-type",
    }
}

fn push_once(found: &mut Vec<String>, name: &str) {
    if name.is_empty() || found.iter().any(|f| f == name) {
        return;
    }
    found.push(name.to_string());
}

#[cfg(test)]
mod tests {

    /// Detect selectors needing the preceding sibling (which limits DOM freeing to descendants).
    #[test]
    fn selectors_that_need_preceding_siblings_are_detected() {
        for css in [
            "li:first-child { color: red }",
            "p:nth-child(2) { color: red }",
            "p:first-of-type { color: red }",
            "p:nth-of-type(2) { color: red }",
            "p:only-child { color: red }",
            "h1 + p { color: red }",
            "h1 ~ p { color: red }",
            ":is(p:first-child, span) { color: red }",
            ":not(p:first-child) { color: red }",
        ] {
            assert!(
                needs_preceding_siblings(&parse_stylesheet(css)),
                "not detected: {css}"
            );
        }
    }

    /// With selectors that do not need the preceding sibling, the whole subtree can be freed as before.
    #[test]
    fn ordinary_selectors_do_not_need_preceding_siblings() {
        for css in [
            "li:last-child { color: red }",
            "div:empty { color: red }",
            "a:hover { color: red }",
            "section:has(h1) { color: red }",
            "div > p { color: red }",
            "div p { color: red }",
        ] {
            assert!(
                !needs_preceding_siblings(&parse_stylesheet(css)),
                "over-detected: {css}"
            );
        }
    }

    /// Warn only about selectors that still cannot be decided with the preceding sibling kept.
    #[test]
    fn only_selectors_that_stay_broken_are_reported() {
        let sheet = parse_stylesheet(
            "p:last-of-type { color: red } p:only-of-type { color: blue } \
             p:nth-last-child(2) { color: green } p:nth-last-of-type(1) { color: teal } \
             div:has(~ h1) { color: navy }",
        );
        let found = streaming_unsafe_selectors(&sheet);
        for expected in [
            ":last-of-type",
            ":only-of-type",
            ":nth-last-child()",
            ":nth-last-of-type()",
            ":has(~ ...)",
        ] {
            assert!(
                found.contains(&expected.to_string()),
                "{expected} is missing: {found:?}"
            );
        }
    }

    /// Do not warn about ones that become correct with the preceding sibling kept. Warning
    /// too eagerly would make users give up on `--streaming` when they need not.
    #[test]
    fn selectors_that_stay_correct_are_not_reported() {
        let sheet = parse_stylesheet(
            "li:last-child { color: red } div:empty { color: green } \
             a:hover { color: blue } section:has(h1) { color: teal } \
             div:has(> p) { color: navy } h1:has(+ p) { color: olive } \
             h1 + p { color: gray } li:first-child { color: lime }",
        );
        assert!(
            streaming_unsafe_selectors(&sheet).is_empty(),
            "got: {:?}",
            streaming_unsafe_selectors(&sheet)
        );
    }

    /// Look inside the arguments of `:is()`/`:where()`/`:not()` too.
    #[test]
    fn nested_selector_lists_are_scanned() {
        let sheet = parse_stylesheet(":is(p:nth-last-of-type(2), span) { color: red }");
        assert_eq!(
            streaming_unsafe_selectors(&sheet),
            vec![":nth-last-of-type()"]
        );
    }

    use cssparser::ToCss;

    use super::*;

    #[test]
    fn media_print_and_all_rules_are_applied() {
        for query in ["print", "all", "print, screen", "not screen"] {
            let sheet = parse_stylesheet(&format!(
                "@media {query} {{ div {{ color: rgb(1, 2, 3); }} }}"
            ));
            assert_eq!(
                sheet.rules.len(),
                1,
                "@media {query} should apply its rules"
            );
        }
    }

    #[test]
    fn media_screen_only_rules_are_ignored() {
        let sheet = parse_stylesheet("@media screen { div { color: rgb(1, 2, 3); } }");
        assert!(
            sheet.rules.is_empty(),
            "@media screen should not apply its rules"
        );
    }

    #[test]
    fn media_with_only_a_feature_query_defaults_to_matching_all() {
        // A bare feature query with the type omitted (`(min-width: 600px)`) is defined to
        // mean `all and (min-width: 600px)`. Feature queries themselves are not evaluated,
        // so it always matches.
        let sheet = parse_stylesheet("@media (min-width: 600px) { div { color: rgb(1, 2, 3); } }");
        assert_eq!(sheet.rules.len(), 1);
    }

    #[test]
    fn media_rules_and_subsequent_rules_are_both_parsed() {
        let sheet = parse_stylesheet(
            "@media print { div { color: rgb(1, 2, 3); } } p { color: rgb(4, 5, 6); }",
        );
        assert_eq!(sheet.rules.len(), 2);
    }

    #[test]
    fn nested_media_rules_are_flattened() {
        let sheet =
            parse_stylesheet("@media print { @media all { div { color: rgb(1, 2, 3); } } }");
        assert_eq!(sheet.rules.len(), 1);
    }

    #[test]
    fn font_face_inside_a_matching_media_rule_is_still_recognized() {
        let sheet = parse_stylesheet(
            r#"@media print { @font-face { font-family: "Test"; src: url("test.ttf"); } }"#,
        );
        assert_eq!(sheet.font_faces.len(), 1);
    }

    #[test]
    fn parse_inline_style_parses_bare_declarations() {
        let decls = parse_inline_style("color: rgb(1, 2, 3); font-size: 14px");
        assert_eq!(decls.len(), 2);
        assert!(matches!(decls[0], PropertyDeclaration::Color(_)));
        assert!(matches!(decls[1], PropertyDeclaration::FontSize(_)));
    }

    #[test]
    fn parse_inline_style_ignores_unknown_properties() {
        let decls = parse_inline_style("not-a-real-property: 5px; color: rgb(1, 2, 3)");
        assert_eq!(decls.len(), 1);
        assert!(matches!(decls[0], PropertyDeclaration::Color(_)));
    }

    #[test]
    fn parse_stylesheet_ignores_at_import_and_keeps_subsequent_rules() {
        // `@import` is rejected by `TopLevelRuleParser::parse_prelude` as an at-rule other than
        // `@font-face`, and is expected to be skipped by cssparser's `StyleSheetParser` error
        // recovery. This checks that even when fetched external CSS contains an `@import`,
        // parsing of the ordinary rules after it continues.
        let sheet = parse_stylesheet(
            r#"@import url("other.css"); p { color: rgb(1, 2, 3); } div { color: rgb(4, 5, 6); }"#,
        );
        assert_eq!(
            sheet.rules.len(),
            2,
            "both rules after the ignored @import should still be parsed"
        );
    }

    #[test]
    fn parse_stylesheet_ignores_unrecognized_properties_with_url_values() {
        // Even with a property this implementation does not support whose value contains a
        // `url()` reference, only that declaration is ignored, and the other declarations in
        // the same rule and the rules that follow still parse.
        let sheet =
            parse_stylesheet(r#"div { border-image: url("border.png") 30; color: rgb(1, 2, 3); }"#);
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(
            sheet.rules[0].declarations.len(),
            1,
            "the unrecognized border-image declaration should be skipped, \
             leaving only the color declaration"
        );
        assert!(matches!(
            sheet.rules[0].declarations[0],
            PropertyDeclaration::Color(_)
        ));
    }

    #[test]
    fn parse_inline_style_handles_empty_string() {
        assert!(parse_inline_style("").is_empty());
    }

    #[test]
    fn deeply_nested_calc_is_rejected_instead_of_overflowing_the_stack() {
        // This is a recursive descent parser, so depth translates directly into stack use.
        // Up to the limit (32 levels) is accepted; anything deeper drops the declaration.
        let nested = |n: usize| {
            format!(
                "p {{ padding-left: {}1px{} }}",
                "calc(".repeat(n),
                ")".repeat(n)
            )
        };
        assert_eq!(
            parse_stylesheet(&nested(32)).rules.len(),
            1,
            "32 levels are accepted"
        );
        assert!(
            parse_stylesheet(&nested(33)).rules.is_empty(),
            "33 levels are dropped"
        );

        // Parentheses count towards the same depth as `calc()`.
        let parens = format!(
            "p {{ padding-left: calc({}1px{}) }}",
            "(".repeat(40),
            ")".repeat(40)
        );
        assert!(parse_stylesheet(&parens).rules.is_empty());
    }

    #[test]
    fn negative_padding_is_rejected() {
        // CSS does not allow a negative `padding`. Drop the declaration.
        for css in [
            "p { padding-left: -5px }",
            "p { padding: -5px }",
            "p { padding: 5px -5px }",
            "p { padding-inline-start: -5px }",
            "p { padding-block: -1em }",
            "p { padding-top: -10% }",
        ] {
            assert!(
                parse_stylesheet(css).rules.is_empty(),
                "negative padding should be dropped: {css}"
            );
        }
        // Zero, positive values and `calc()` (whose sign is unknown until resolved) are accepted.
        for css in [
            "p { padding-left: 0 }",
            "p { padding: 5px }",
            "p { padding-left: calc(10px - 20px) }",
        ] {
            assert_eq!(
                parse_stylesheet(css).rules.len(),
                1,
                "should be accepted: {css}"
            );
        }
    }

    // ===== CSS Nesting (#25) =====

    fn selector_texts(sheet: &Stylesheet) -> Vec<String> {
        sheet
            .rules
            .iter()
            .map(|r| r.selectors.to_css_string())
            .collect()
    }

    #[test]
    fn nested_rule_with_explicit_parent_selector_is_flattened() {
        let sheet = parse_stylesheet(".wrap { & .probe { color: rgb(1, 2, 3) } }");
        // A parent with no declarations (`.wrap`) does not survive as a rule.
        assert_eq!(selector_texts(&sheet), [":is(.wrap) .probe"]);
        assert_eq!(sheet.rules[0].declarations.len(), 1);
    }

    #[test]
    fn nested_rule_without_parent_selector_is_a_descendant_of_the_parent() {
        let sheet = parse_stylesheet(".wrap { .probe { color: rgb(1, 2, 3) } }");
        assert_eq!(selector_texts(&sheet), [":is(.wrap) .probe"]);
    }

    #[test]
    fn nested_compound_parent_selector_is_flattened() {
        let sheet = parse_stylesheet(".wrap { &.probe { color: rgb(1, 2, 3) } }");
        assert_eq!(selector_texts(&sheet), [":is(.wrap).probe"]);
    }

    #[test]
    fn nested_rule_with_leading_combinator_is_flattened() {
        let sheet = parse_stylesheet(".list { > li { color: rgb(1, 2, 3) } }");
        assert_eq!(selector_texts(&sheet), [":is(.list) > li"]);
    }

    #[test]
    fn nested_type_selector_is_parsed_as_a_rule() {
        let sheet = parse_stylesheet(".wrap { p { color: rgb(1, 2, 3) } }");
        assert_eq!(selector_texts(&sheet), [":is(.wrap) p"]);
    }

    #[test]
    fn nested_selector_list_parent_is_kept_as_a_list() {
        let sheet = parse_stylesheet(".a, .b { .c { color: rgb(1, 2, 3) } }");
        assert_eq!(selector_texts(&sheet), [":is(.a, .b) .c"]);
    }

    #[test]
    fn deeper_nesting_is_flattened_in_source_order() {
        let sheet = parse_stylesheet(".a { .b { .c { color: rgb(1, 2, 3) } } }");
        assert_eq!(selector_texts(&sheet), [":is(:is(.a) .b) .c"]);
    }

    #[test]
    fn declarations_around_nested_rules_keep_their_source_order() {
        // The earlier declarations become the parent rule, then the nested rule, then the later
        // declarations as a separate rule with the same selector, in that order (not hoisted).
        let sheet = parse_stylesheet(
            ".wrap { color: rgb(1, 2, 3); .probe { color: rgb(4, 5, 6) } margin-left: 5px }",
        );
        assert_eq!(
            selector_texts(&sheet),
            [".wrap", ":is(.wrap) .probe", ".wrap"]
        );
        assert!(matches!(
            sheet.rules[0].declarations[..],
            [PropertyDeclaration::Color(_)]
        ));
        assert!(matches!(
            sheet.rules[2].declarations[..],
            [PropertyDeclaration::MarginLeft(_)]
        ));
    }

    #[test]
    fn a_parent_with_only_nested_rules_has_no_trailing_rule() {
        let sheet = parse_stylesheet(".wrap { .probe { color: rgb(1, 2, 3) } }");
        assert_eq!(sheet.rules.len(), 1, "no empty trailing rule is created");
    }

    #[test]
    fn declarations_after_a_nested_rule_use_the_resolved_parent_selector() {
        // Declarations after a nested rule have the same selector as `&`.
        // With a selector list parent they collapse into `:is(.p, #q)`, and the specificity
        // levels up to the strongest selector (here `#q`'s (1,0,0)).
        let sheet = parse_stylesheet(".p, #q { .c { color: rgb(1, 2, 3) } margin-left: 5px }");
        assert_eq!(selector_texts(&sheet), [":is(.p, #q) .c", ":is(.p, #q)"]);
        let trailing = &sheet.rules[1].selectors;
        assert_eq!(trailing.slice().len(), 1);
        assert_eq!(
            trailing.slice()[0].specificity(),
            parse_stylesheet("#q { color: rgb(1, 2, 3) }").rules[0]
                .selectors
                .slice()[0]
                .specificity()
        );
    }

    #[test]
    fn rules_without_declarations_are_dropped() {
        // A rule with no declarations does not contribute to the cascade, so it is not indexed.
        let sheet = parse_stylesheet(".a { } .b { color: rgb(1, 2, 3) } .c { }");
        assert_eq!(selector_texts(&sheet), [".b"]);
    }

    #[test]
    fn nested_rules_inside_media_are_flattened() {
        let sheet = parse_stylesheet("@media print { .wrap { .probe { color: rgb(1, 2, 3) } } }");
        assert_eq!(selector_texts(&sheet), [":is(.wrap) .probe"]);
    }

    #[test]
    fn an_invalid_nested_rule_is_dropped_without_its_siblings() {
        let sheet = parse_stylesheet(
            ".wrap { .probe::first-line { color: rgb(1, 2, 3) } color: rgb(4, 5, 6); .ok { color: rgb(7, 8, 9) } }",
        );
        assert_eq!(selector_texts(&sheet), [".wrap", ":is(.wrap) .ok"]);
        assert_eq!(sheet.rules[0].declarations.len(), 1);
    }

    #[test]
    fn an_invalid_declaration_next_to_nested_rules_is_still_skipped() {
        let sheet = parse_stylesheet(
            ".wrap { border-image: url(\"b.png\") 30; color: rgb(1, 2, 3); .probe { color: rgb(4, 5, 6) } }",
        );
        assert_eq!(selector_texts(&sheet), [".wrap", ":is(.wrap) .probe"]);
        assert_eq!(sheet.rules[0].declarations.len(), 1);
    }
}
