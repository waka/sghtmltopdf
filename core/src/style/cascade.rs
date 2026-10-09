//! Selector matching, and ordering the applicable declarations by cascade order
//! (origin, then specificity, then source order).
//!
//! Which declaration wins when several specify the same property (inheritance included) is
//! the computed style's job (the style computation phase). This module goes as far as
//! returning a declaration list ordered so that later entries have higher priority.

use selectors::matching::{
    self, MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};
use selectors::parser::SelectorList;

use crate::html::{Dom, NodeId};

use super::custom_properties::{
    compute_custom_properties, resolve_unparsed, uses_variables, CustomProperties,
};
use super::element_ref::ElementRef;
use super::properties::PropertyDeclaration;
use super::selector_impl::{PseudoElement, SgSelectorImpl};
use super::stylesheet::Stylesheet;
use super::values::ContentPart;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    UserAgent,
    Author,
}

/// The same as [`matching_declarations`], but returning UA-stylesheet declarations and
/// author-CSS declarations separately (each in ascending cascade priority).
///
/// Legacy presentational attributes have to sit "stronger than the UA stylesheet, weaker
/// than author CSS", so this shape exists to let them be spliced in between the two.
pub fn matching_declarations_by_origin<'a>(
    dom: &Dom,
    element: NodeId,
    ua: &'a Stylesheet,
    author: &'a Stylesheet,
) -> (Vec<&'a PropertyDeclaration>, Vec<&'a PropertyDeclaration>) {
    let el = ElementRef::new(dom, element);
    let mut caches = SelectorCaches::default();
    let mut context = MatchingContext::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );

    let mut candidates = Vec::new();
    let mut collect = |sheet: &'a Stylesheet| {
        // Narrow the candidates with the index before matching (the index never drops a
        // match, so the result is the same as scanning every rule).
        sheet.index().candidates(dom, element, &mut candidates);
        let mut matched: Vec<(u32, usize, &'a Vec<PropertyDeclaration>)> = Vec::new();
        for &source_order in &candidates {
            let rule = &sheet.rules[source_order as usize];
            if let Some(specificity) = best_matching_specificity(&rule.selectors, &el, &mut context)
            {
                matched.push((specificity, source_order as usize, &rule.declarations));
            }
        }
        matched.sort_by_key(|(specificity, source_order, _)| (*specificity, *source_order));
        matched
            .into_iter()
            .flat_map(|(_, _, declarations)| declarations.iter())
            .collect::<Vec<_>>()
    };

    // Origin is the primary sort key, so sorting each origin separately and then
    // concatenating gives the same result as sorting everything at once.
    (collect(ua), collect(author))
}

/// Return the declarations that apply to `element` in `dom`, in ascending cascade priority
/// (lowest priority first, highest last).
pub fn matching_declarations<'a>(
    dom: &Dom,
    element: NodeId,
    ua: &'a Stylesheet,
    author: &'a Stylesheet,
) -> Vec<&'a PropertyDeclaration> {
    let (mut ua_declarations, author_declarations) =
        matching_declarations_by_origin(dom, element, ua, author);
    ua_declarations.extend(author_declarations);
    ua_declarations
}

/// Return the declarations matching `pseudo` (`::before`/`::after`/`::first-letter`) on
/// `element` in `dom`, in ascending cascade priority (lowest priority first, highest last).
/// The pseudo-element counterpart of `matching_declarations`.
///
/// Matching uses the `selectors` crate's `MatchingMode::ForStatelessPseudoElement`. That
/// assumes the selector ends in `pseudo`, consumes the pseudo-element part, and matches the
/// remaining compound selector against `element` (the real element) as usual, since no DOM
/// node corresponds to the pseudo-element itself.
pub(super) fn matching_pseudo_declarations<'a>(
    dom: &Dom,
    element: NodeId,
    pseudo: PseudoElement,
    ua: &'a Stylesheet,
    author: &'a Stylesheet,
) -> Vec<&'a PropertyDeclaration> {
    let el = ElementRef::new(dom, element);
    let mut caches = SelectorCaches::default();
    let mut context = MatchingContext::new(
        MatchingMode::ForStatelessPseudoElement,
        None,
        &mut caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    let matches_pseudo = |p: &PseudoElement| *p == pseudo;
    context.pseudo_element_matching_fn = Some(&matches_pseudo);

    let mut matched: Vec<(Origin, u32, usize, &'a Vec<PropertyDeclaration>)> = Vec::new();
    for (origin, sheet) in [(Origin::UserAgent, ua), (Origin::Author, author)] {
        // Only rules carrying a pseudo-element selector are relevant.
        let index = sheet.index();
        for &source_order in index.pseudo_candidates() {
            let source_order = source_order as usize;
            let rule = &sheet.rules[source_order];
            let specificity = rule
                .selectors
                .slice()
                .iter()
                .filter(|selector| selector.pseudo_element() == Some(&pseudo))
                .filter(|selector| matching::matches_selector(selector, 0, None, &el, &mut context))
                .map(|selector| selector.specificity())
                .max();
            if let Some(specificity) = specificity {
                matched.push((origin, specificity, source_order, &rule.declarations));
            }
        }
    }

    matched.sort_by_key(|(origin, specificity, source_order, _)| {
        (*origin, *specificity, *source_order)
    });

    matched
        .into_iter()
        .flat_map(|(_, _, _, declarations)| declarations.iter())
        .collect()
}

/// Resolve, according to the cascade, the generated-content parts of `pseudo`
/// (`::before`/`::after`) on `element` in `dom`. Returns `None` if the matched declarations
/// contain no valid `content` declaration at all (that is, no generated box, the same as
/// the CSS initial value `normal`).
pub fn matching_pseudo_content(
    dom: &Dom,
    element: NodeId,
    pseudo: PseudoElement,
    ua: &Stylesheet,
    author: &Stylesheet,
    element_properties: &CustomProperties,
) -> Option<Vec<ContentPart>> {
    let declarations = matching_pseudo_declarations(dom, element, pseudo, ua, author);
    // `content: var(--label)` resolves against the element's custom properties plus any
    // declared on the pseudo-element itself.
    let properties = if uses_variables(declarations.iter().copied()) {
        compute_custom_properties(element_properties, declarations.iter().copied())
    } else {
        element_properties.clone()
    };
    declarations
        .into_iter()
        .rev()
        .find_map(|decl| match decl {
            PropertyDeclaration::Content(content) => Some(content.clone()),
            // `content: initial | inherit | unset` all compute to `normal` on a
            // pseudo-element (the originating element's own `content` is `normal`).
            PropertyDeclaration::CssWide(wide)
                if wide
                    .longhands
                    .iter()
                    .any(|d| matches!(d, PropertyDeclaration::Content(_))) =>
            {
                Some(None)
            }
            PropertyDeclaration::Unparsed(unparsed)
                if unparsed.name.eq_ignore_ascii_case("content") =>
            {
                // Invalid at computed-value time: `content` becomes its initial value (`normal`).
                Some(
                    match resolve_unparsed(unparsed, &properties) {
                        (resolved, false) => resolved.into_iter().find_map(|d| match d {
                            PropertyDeclaration::Content(content) => Some(content),
                            _ => None,
                        }),
                        _ => None,
                    }
                    .flatten(),
                )
            }
            _ => None,
        })
        .flatten()
}

/// Return the highest specificity among the selectors in the list that actually matched the
/// element (for a selector group such as `h1, h2 { ... }`, the specificity of the one that matched).
fn best_matching_specificity(
    selectors: &SelectorList<SgSelectorImpl>,
    element: &ElementRef,
    context: &mut MatchingContext<SgSelectorImpl>,
) -> Option<u32> {
    selectors
        .slice()
        .iter()
        .filter(|selector| matching::matches_selector(selector, 0, None, element, context))
        .map(|selector| selector.specificity())
        .max()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::{self, NodeData};
    use crate::style::{parse_stylesheet, Color, Display};

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    fn last_color(decls: &[&PropertyDeclaration]) -> Option<Color> {
        decls.iter().rev().find_map(|d| match d {
            PropertyDeclaration::Color(c) => Some(*c),
            _ => None,
        })
    }

    fn last_display(decls: &[&PropertyDeclaration]) -> Option<Display> {
        decls.iter().rev().find_map(|d| match d {
            PropertyDeclaration::Display(display) => Some(*display),
            _ => None,
        })
    }

    fn rgb(v: u8) -> Color {
        Color::Rgba {
            red: v,
            green: v,
            blue: v,
            alpha: 1.0,
        }
    }

    #[test]
    fn specificity_beats_source_order() {
        let dom = html::parse(br#"<div id="x" class="c">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        // Even with the more specific #x rule written first in the source, specificity is
        // what wins, so it should end up last.
        let author = parse_stylesheet(
            "#x { color: rgb(2, 2, 2); } .c { color: rgb(1, 1, 1); } div { color: rgb(0, 0, 0); }",
        );
        let ua = Stylesheet::default();

        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), Some(rgb(2)));
    }

    #[test]
    fn later_source_order_wins_on_specificity_tie() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let author = parse_stylesheet("div { color: rgb(9, 9, 9); } div { color: rgb(8, 8, 8); }");
        let ua = Stylesheet::default();

        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), Some(rgb(8)));
    }

    #[test]
    fn author_origin_beats_user_agent_on_specificity_tie() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let ua = parse_stylesheet("div { display: block; }");
        let author = parse_stylesheet("div { display: inline; }");

        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_display(&decls), Some(Display::Inline));
    }

    #[test]
    fn descendant_combinator_matches_nested_element() {
        let dom = html::parse(br#"<div><p>inner</p></div><p>outer</p>"#);
        let ps: Vec<_> = {
            let mut out = Vec::new();
            fn collect(dom: &Dom, id: NodeId, out: &mut Vec<NodeId>) {
                if let NodeData::Element { name, .. } = &dom.node(id).data {
                    if &*name.local == "p" {
                        out.push(id);
                    }
                }
                for child in dom.children(id) {
                    collect(dom, child, out);
                }
            }
            collect(&dom, dom.document(), &mut out);
            out
        };
        assert_eq!(ps.len(), 2, "expected both <p> elements to be found");

        let author = parse_stylesheet("div p { color: rgb(3, 3, 3); }");
        let ua = Stylesheet::default();

        let inner_decls = matching_declarations(&dom, ps[0], &ua, &author);
        let outer_decls = matching_declarations(&dom, ps[1], &ua, &author);

        assert_eq!(last_color(&inner_decls), Some(rgb(3)));
        assert_eq!(last_color(&outer_decls), None);
    }

    #[test]
    fn hover_pseudo_class_never_matches_but_does_not_break_the_rest_of_the_selector_list() {
        let dom = html::parse(br#"<div class="foo">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        // If `.bar:hover` merely fails to match (rather than failing to parse), the
        // comma-separated `.foo` should survive and apply.
        let author = parse_stylesheet(".foo, .bar:hover { color: rgb(6, 6, 6); }");
        let ua = Stylesheet::default();

        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), Some(rgb(6)));
    }

    #[test]
    fn hover_alone_never_matches() {
        let dom = html::parse(br#"<div>t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        let author = parse_stylesheet("div:hover { color: rgb(6, 6, 6); }");
        let ua = Stylesheet::default();

        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), None);
    }

    /// Pseudo-elements that can never have a box in a static PDF parse but never match, so
    /// a selector list naming one keeps its other items (Tailwind v4's preflight reset).
    const NEVER_MATCHING: [&str; 5] = [
        "::backdrop",
        "::file-selector-button",
        "::placeholder",
        "::selection",
        "::marker",
    ];

    #[test]
    fn never_matching_pseudo_elements_do_not_drop_the_rest_of_the_selector_list() {
        let dom = html::parse(br#"<div class="foo">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let ua = Stylesheet::default();

        for pseudo in NEVER_MATCHING {
            let author = parse_stylesheet(&format!(".foo, {pseudo} {{ color: rgb(6, 6, 6); }}"));
            let decls = matching_declarations(&dom, div, &ua, &author);
            assert_eq!(last_color(&decls), Some(rgb(6)), "{pseudo}");
        }

        // The Tailwind v4 preflight selector.
        let author = parse_stylesheet(
            "*, ::after, ::before, ::backdrop, ::file-selector-button { color: rgb(6, 6, 6); }",
        );
        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), Some(rgb(6)));
    }

    #[test]
    fn never_matching_pseudo_elements_do_not_match_the_host_element() {
        let dom = html::parse(br#"<div class="foo">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let ua = Stylesheet::default();

        for pseudo in NEVER_MATCHING {
            let author = parse_stylesheet(&format!("div{pseudo}, .foo{pseudo} {{ color: red; }}"));
            let decls = matching_declarations(&dom, div, &ua, &author);
            assert_eq!(last_color(&decls), None, "{pseudo}");
            // ... and they are not mistaken for ::before/::after/::first-letter either.
            for p in [
                PseudoElement::Before,
                PseudoElement::After,
                PseudoElement::FirstLetter,
            ] {
                assert!(matching_pseudo_declarations(&dom, div, p, &ua, &author).is_empty());
            }
        }
    }

    #[test]
    fn never_matching_pseudo_element_inside_is_and_where_is_forgiven() {
        let dom = html::parse(br#"<div class="foo">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let ua = Stylesheet::default();

        // A pseudo-element is not valid inside :is()/:where(); the forgiving list drops
        // just that item, as it does for any other unsupported argument.
        let author = parse_stylesheet(":is(.foo, ::placeholder) { color: rgb(6, 6, 6); }");
        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), Some(rgb(6)));
        let author = parse_stylesheet(":where(::placeholder) { color: red; }");
        let decls = matching_declarations(&dom, div, &ua, &author);
        assert_eq!(last_color(&decls), None);
    }

    /// `:host` and `:host()` only match a shadow host from inside its shadow tree. There are
    /// no shadow trees here, so they parse but never match.
    const HOST_SELECTORS: [&str; 3] = [":host", ":host(.foo)", ":host(div)"];

    #[test]
    fn host_does_not_drop_the_rest_of_the_selector_list() {
        let dom = html::parse(br#"<div class="foo">t</div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let html_el = find(&dom, dom.document(), "html").expect("html not found");
        let ua = Stylesheet::default();

        for host in HOST_SELECTORS {
            let author = parse_stylesheet(&format!(".foo, {host} {{ color: rgb(6, 6, 6); }}"));
            let decls = matching_declarations(&dom, div, &ua, &author);
            assert_eq!(last_color(&decls), Some(rgb(6)), "{host}");
        }

        // Tailwind v4's theme and preflight selectors.
        for selector in [":root, :host", "html, :host"] {
            let author = parse_stylesheet(&format!("{selector} {{ color: rgb(6, 6, 6); }}"));
            let decls = matching_declarations(&dom, html_el, &ua, &author);
            assert_eq!(last_color(&decls), Some(rgb(6)), "{selector}");
        }
        let author = parse_stylesheet("@layer theme { :root, :host { color: rgb(6, 6, 6); } }");
        let decls = matching_declarations(&dom, html_el, &ua, &author);
        assert_eq!(last_color(&decls), Some(rgb(6)));
    }

    #[test]
    fn host_never_matches() {
        let dom = html::parse(br#"<div class="foo">t</div>"#);
        let ua = Stylesheet::default();

        for tag in ["html", "body", "div"] {
            let element = find(&dom, dom.document(), tag).expect("element not found");
            for host in HOST_SELECTORS {
                for selector in [
                    host.to_string(),
                    format!("{host} div"),
                    format!("{host} > *"),
                ] {
                    let author = parse_stylesheet(&format!("{selector} {{ color: red; }}"));
                    let decls = matching_declarations(&dom, element, &ua, &author);
                    assert_eq!(last_color(&decls), None, "{selector} on <{tag}>");
                }
            }
        }
    }
}
