//! An index over the style rules.
//!
//! Selector matching runs per element, so a naive implementation performs
//! "element count x rule count" comparisons. In practice almost every rule requires a tag
//! name, class or id in its rightmost compound selector (the `p` part of `div p`), and an
//! element without it can be ruled out without any comparison at all. This module buckets
//! rules by that requirement and returns only the buckets matching an element's tag name,
//! classes and id as candidates, cutting the comparisons per element down to the number of
//! candidates.
//!
//! What the index returns is only candidates; whether a rule actually matches is decided as
//! before by `selectors` (the index never drops a match).

use std::collections::HashMap;

use selectors::parser::Component;

use crate::html::{Dom, NodeData, NodeId};

use super::selector_impl::SgSelectorImpl;
use super::stylesheet::StyleRule;

/// The narrowing key a rule requires of its rightmost compound selector.
enum Bucket {
    Id(String),
    Class(String),
    LocalName(String),
    /// Requires no tag name, class or id (`*`, an attribute selector, a multi-selector
    /// `:is()` and so on). A candidate for every element.
    Any,
}

#[derive(Debug, Default)]
pub struct RuleIndex {
    by_id: HashMap<String, Vec<u32>>,
    by_class: HashMap<String, Vec<u32>>,
    by_local_name: HashMap<String, Vec<u32>>,
    /// Rules that cannot be narrowed (always candidates).
    any: Vec<u32>,
    /// Rules carrying a pseudo-element selector. They never match during ordinary matching,
    /// so they stay out of the buckets above and are used only by pseudo-element matching.
    pseudo: Vec<u32>,
    /// The rule count when the index was built (used by
    /// [`super::stylesheet::Stylesheet::index`] to decide whether to rebuild).
    rule_count: usize,
}

impl RuleIndex {
    pub fn build(rules: &[StyleRule]) -> Self {
        let mut index = Self {
            rule_count: rules.len(),
            ..Self::default()
        };
        for (rule_index, rule) in rules.iter().enumerate() {
            let rule_index = rule_index as u32;
            for selector in rule.selectors.slice() {
                if selector.has_pseudo_element() {
                    push_unique(&mut index.pseudo, rule_index);
                    continue;
                }
                match bucket_of(selector) {
                    Bucket::Id(name) => {
                        push_unique(index.by_id.entry(name).or_default(), rule_index)
                    }
                    Bucket::Class(name) => {
                        push_unique(index.by_class.entry(name).or_default(), rule_index)
                    }
                    Bucket::LocalName(name) => {
                        push_unique(index.by_local_name.entry(name).or_default(), rule_index)
                    }
                    Bucket::Any => push_unique(&mut index.any, rule_index),
                }
            }
        }
        index
    }

    /// The rule count when the index was built.
    pub fn rule_count(&self) -> usize {
        self.rule_count
    }

    /// Put the numbers of the rules that could match `element` into `out`, in ascending source order.
    pub fn candidates(&self, dom: &Dom, element: NodeId, out: &mut Vec<u32>) {
        out.clear();
        let NodeData::Element { name, attrs, .. } = &dom.node(element).data else {
            return;
        };
        out.extend_from_slice(&self.any);
        if let Some(rules) = self.by_local_name.get(&*name.local) {
            out.extend_from_slice(rules);
        }
        for attr in attrs {
            match &*attr.name.local {
                "id" => {
                    if let Some(rules) = self.by_id.get(&*attr.value) {
                        out.extend_from_slice(rules);
                    }
                }
                "class" => {
                    for class in attr.value.split_ascii_whitespace() {
                        if let Some(rules) = self.by_class.get(class) {
                            out.extend_from_slice(rules);
                        }
                    }
                }
                _ => {}
            }
        }
        // The same rule can land in several buckets (a selector list such as `h1, .lead`),
        // so sort into source order and then drop duplicates.
        out.sort_unstable();
        out.dedup();
    }

    /// The numbers of the rules carrying a pseudo-element selector (in ascending source order).
    pub fn pseudo_candidates(&self) -> &[u32] {
        &self.pseudo
    }
}

/// A rule is added to the candidates only once even when several of its selectors end up in
/// the same bucket (numbers are pushed in ascending order).
fn push_unique(rules: &mut Vec<u32>, rule_index: u32) {
    if rules.last() != Some(&rule_index) {
        rules.push(rule_index);
    }
}

/// The narrowing key required by `selector`'s rightmost compound selector.
/// Preference goes id, then class, then tag name (strongest narrowing first).
fn bucket_of(selector: &selectors::parser::Selector<SgSelectorImpl>) -> Bucket {
    let mut bucket = Bucket::Any;
    // `iter` returns only the rightmost compound selector (it stops before the combinator).
    for component in selector.iter() {
        match component {
            Component::ID(name) => return Bucket::Id(name.0.to_string()),
            Component::Class(name) => bucket = Bucket::Class(name.0.to_string()),
            // A type selector containing uppercase is only case-sensitive outside the HTML
            // namespace, so it cannot be narrowed straightforwardly. To avoid dropping a
            // match, it is left out of the narrowing (that is, kept as `Bucket::Any`).
            Component::LocalName(name)
                if matches!(bucket, Bucket::Any) && name.name == name.lower_name =>
            {
                bucket = Bucket::LocalName(name.lower_name.0.to_string());
            }
            // CSS Nesting's `&` expands to `:is(parent selector)`, so a form where `&`
            // carries the narrowing key, such as `&:hover`, would fall all the way to
            // `Bucket::Any` and be compared against every element unless we look inside.
            // When the inside is a single selector, its narrowing key is carried over.
            // (A multi-selector such as `:is(.a, .b)` stays `Bucket::Any`, since picking
            // just one of them would drop matches.)
            Component::Is(list) if list.slice().len() == 1 => match bucket_of(&list.slice()[0]) {
                Bucket::Id(name) => return Bucket::Id(name),
                Bucket::Class(name) => bucket = Bucket::Class(name),
                Bucket::LocalName(name) if matches!(bucket, Bucket::Any) => {
                    bucket = Bucket::LocalName(name)
                }
                _ => {}
            },
            _ => {}
        }
    }
    bucket
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::{self, NodeData};
    use crate::style::parse_stylesheet;

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    fn candidates_for(css: &str, source: &str, tag: &str) -> Vec<u32> {
        let sheet = parse_stylesheet(css);
        let index = RuleIndex::build(&sheet.rules);
        let dom = html::parse(source.as_bytes());
        let element = find(&dom, dom.document(), tag).expect("element not found");
        let mut out = Vec::new();
        index.candidates(&dom, element, &mut out);
        out
    }

    #[test]
    fn only_rules_requiring_the_elements_tag_are_candidates() {
        let out = candidates_for(
            "p { color: red; } div { color: blue; } span { color: green; }",
            "<p>text</p>",
            "p",
        );
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn class_and_id_rules_are_picked_up_from_the_attributes() {
        let out = candidates_for(
            ".lead { color: red; } #main { color: blue; } .other { color: green; }",
            r#"<p id="main" class="lead">text</p>"#,
            "p",
        );
        assert_eq!(out, vec![0, 1]);
    }

    #[test]
    fn the_rightmost_compound_decides_the_bucket() {
        // `div p` ends in `p`, so it is a candidate for elements that are `p`.
        let out = candidates_for("div p { color: red; }", "<div><p>text</p></div>", "p");
        assert_eq!(out, vec![0]);

        // The same rule is not a candidate for the `div` itself.
        let out = candidates_for("div p { color: red; }", "<div><p>text</p></div>", "div");
        assert!(out.is_empty());
    }

    #[test]
    fn rules_that_cannot_be_narrowed_are_always_candidates() {
        let out = candidates_for(
            "* { color: red; } [data-x] { color: blue; }",
            "<p>text</p>",
            "p",
        );
        assert_eq!(out, vec![0, 1]);
    }

    #[test]
    fn a_selector_list_puts_the_rule_in_every_matching_bucket_but_only_once() {
        let css = "h1, .lead, p { color: red; }";
        assert_eq!(candidates_for(css, "<h1>t</h1>", "h1"), vec![0]);
        assert_eq!(
            candidates_for(css, r#"<div class="lead">t</div>"#, "div"),
            vec![0]
        );
        // Matching both a selector ending in `p` and a class selector still counts once.
        assert_eq!(
            candidates_for(css, r#"<p class="lead">t</p>"#, "p"),
            vec![0]
        );
    }

    #[test]
    fn a_nested_rule_is_narrowed_by_the_parent_inside_is() {
        // `&:hover` expands to `:is(.lead):hover`. Without looking inside `:is()` no
        // narrowing key can be taken and it becomes a candidate for every element.
        let css = ".lead { &:hover { color: red; } } .other { &:hover { color: blue; } }";
        assert_eq!(
            candidates_for(css, r#"<p class="lead">t</p>"#, "p"),
            vec![0]
        );
        // It is not a candidate for an element with a different class.
        assert!(candidates_for(css, "<p>t</p>", "p").is_empty());
    }

    #[test]
    fn a_multi_selector_is_stays_a_candidate_for_every_element() {
        // Picking one bucket for `:is(.a, .b)` would drop matches, so it is always a
        // candidate, unnarrowed.
        let out = candidates_for(":is(.a, .b):hover { color: red; }", "<p>t</p>", "p");
        assert_eq!(out, vec![0]);
    }

    #[test]
    fn pseudo_element_rules_are_kept_out_of_the_normal_buckets() {
        let sheet = parse_stylesheet("p::before { content: 'x'; } p { color: red; }");
        let index = RuleIndex::build(&sheet.rules);
        let dom = html::parse(b"<p>text</p>");
        let element = find(&dom, dom.document(), "p").expect("p not found");

        let mut out = Vec::new();
        index.candidates(&dom, element, &mut out);
        assert_eq!(out, vec![1]);
        assert_eq!(index.pseudo_candidates(), &[0]);
    }
}
