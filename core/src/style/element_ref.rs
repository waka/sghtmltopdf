//! The `selectors::Element` implementation for [`Dom`] nodes.

use html5ever::Namespace;
use selectors::attr::{AttrSelectorOperation, CaseSensitivity, NamespaceConstraint};
use selectors::bloom::BloomFilter;
use selectors::matching::{ElementSelectorFlags, MatchingContext};
use selectors::{Element, OpaqueElement};

use crate::html::{Dom, Node, NodeData, NodeId};

use super::selector_impl::{CssLocalName, NonTSPseudoClass, PseudoElement, SgSelectorImpl};

#[derive(Clone, Copy)]
pub struct ElementRef<'a> {
    dom: &'a Dom,
    id: NodeId,
}

impl<'a> ElementRef<'a> {
    pub fn new(dom: &'a Dom, id: NodeId) -> Self {
        Self { dom, id }
    }

    pub fn id(&self) -> NodeId {
        self.id
    }

    fn node(&self) -> &'a Node {
        self.dom.node(self.id)
    }

    fn is_element(id: NodeId, dom: &Dom) -> bool {
        matches!(dom.node(id).data, NodeData::Element { .. })
    }
}

impl std::fmt::Debug for ElementRef<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ElementRef({:?})", self.id)
    }
}

/// Iterator over the element nodes immediately before or after an element.
fn sibling_elements(dom: &Dom, mut next: Option<NodeId>, forward: bool) -> Option<NodeId> {
    while let Some(id) = next {
        if ElementRef::is_element(id, dom) {
            return Some(id);
        }
        let node = dom.node(id);
        next = if forward {
            node.next_sibling
        } else {
            node.previous_sibling
        };
    }
    None
}

impl<'a> Element for ElementRef<'a> {
    type Impl = SgSelectorImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.node())
    }

    fn parent_element(&self) -> Option<Self> {
        self.dom
            .parent(self.id)
            .map(|id| ElementRef::new(self.dom, id))
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<Self> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<Self> {
        sibling_elements(self.dom, self.node().previous_sibling, false)
            .map(|id| ElementRef::new(self.dom, id))
    }

    fn next_sibling_element(&self) -> Option<Self> {
        sibling_elements(self.dom, self.node().next_sibling, true)
            .map(|id| ElementRef::new(self.dom, id))
    }

    fn first_element_child(&self) -> Option<Self> {
        sibling_elements(self.dom, self.node().first_child, true)
            .map(|id| ElementRef::new(self.dom, id))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        matches!(&self.node().data, NodeData::Element { name, .. } if name.ns == html5ever::ns!(html))
    }

    fn has_local_name(&self, local_name: &CssLocalName) -> bool {
        matches!(&self.node().data, NodeData::Element { name, .. } if name.local == local_name.0)
    }

    fn has_namespace(&self, ns: &Namespace) -> bool {
        matches!(&self.node().data, NodeData::Element { name, .. } if &name.ns == ns)
    }

    fn is_same_type(&self, other: &Self) -> bool {
        match (&self.node().data, &other.node().data) {
            (NodeData::Element { name: a, .. }, NodeData::Element { name: b, .. }) => a == b,
            _ => false,
        }
    }

    fn attr_matches(
        &self,
        ns: &NamespaceConstraint<&Namespace>,
        local_name: &CssLocalName,
        operation: &AttrSelectorOperation<&super::selector_impl::CssString>,
    ) -> bool {
        let NodeData::Element { attrs, .. } = &self.node().data else {
            return false;
        };
        attrs.iter().any(|attr| {
            !matches!(*ns, NamespaceConstraint::Specific(url) if *url != attr.name.ns)
                && local_name.0 == attr.name.local
                && operation.eval_str(&attr.value)
        })
    }

    /// Interaction states (`:hover`/`:focus` and friends), visit history (`:visited`) and
    /// form states (`:checked` and friends) are meaningless in print output with no JS, so
    /// they never match. Only `:link`/`:any-link` match, since they can be decided
    /// statically as "an `<a>` with an `href`" ([`Self::is_link`]). `:visited` is the
    /// complement of `:link`, but with no visit history every link is unvisited.
    fn match_non_ts_pseudo_class(
        &self,
        pc: &NonTSPseudoClass,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        match pc {
            NonTSPseudoClass::Link | NonTSPseudoClass::AnyLink => self.is_link(),
            _ => false,
        }
    }

    fn match_pseudo_element(
        &self,
        _pe: &PseudoElement,
        _context: &mut MatchingContext<Self::Impl>,
    ) -> bool {
        false
    }

    fn apply_selector_flags(&self, _flags: ElementSelectorFlags) {}

    fn is_link(&self) -> bool {
        self.has_local_name(&CssLocalName::from("a"))
            && self.has_attr_in_no_namespace(&"href".into())
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(&self, id: &CssLocalName, case_sensitivity: CaseSensitivity) -> bool {
        let NodeData::Element { attrs, .. } = &self.node().data else {
            return false;
        };
        attrs
            .iter()
            .find(|attr| &*attr.name.local == "id")
            .is_some_and(|attr| case_sensitivity.eq(id.0.as_bytes(), attr.value.as_bytes()))
    }

    fn has_class(&self, name: &CssLocalName, case_sensitivity: CaseSensitivity) -> bool {
        let NodeData::Element { attrs, .. } = &self.node().data else {
            return false;
        };
        attrs
            .iter()
            .find(|attr| &*attr.name.local == "class")
            .is_some_and(|attr| {
                attr.value
                    .split_ascii_whitespace()
                    .any(|class| case_sensitivity.eq(name.0.as_bytes(), class.as_bytes()))
            })
    }

    fn has_custom_state(&self, _name: &CssLocalName) -> bool {
        false
    }

    fn imported_part(&self, _name: &CssLocalName) -> Option<CssLocalName> {
        None
    }

    fn is_part(&self, _name: &CssLocalName) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.dom.children(self.id).all(|child| {
            !matches!(
                &self.dom.node(child).data,
                NodeData::Element { .. } | NodeData::Text { .. }
            )
        })
    }

    fn is_root(&self) -> bool {
        self.dom
            .parent(self.id)
            .is_some_and(|parent| matches!(self.dom.node(parent).data, NodeData::Document))
    }

    fn add_element_unique_hashes(&self, _filter: &mut BloomFilter) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse;
    use selectors::attr::AttrSelectorOperation;

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    /// Check that a node freed by [`Dom::release_subtree`](crate::html::Dom::release_subtree)
    /// stops behaving as an element: `NodeData::Released` matches none of the existing
    /// `match` patterns positively, so every query about its tag name, attributes, classes
    /// and so on fails to match. This is the assumption streaming relies on: a freed node
    /// is safely ignored by all later selector matching.
    #[test]
    fn released_node_no_longer_behaves_like_an_element() {
        let mut dom = parse(br#"<div id="x" class="c"><p>text</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        dom.release_subtree(div);

        let el = ElementRef::new(&dom, div);
        assert!(!el.has_local_name(&"div".into()));
        assert!(!el.has_id(&"x".into(), selectors::attr::CaseSensitivity::CaseSensitive));
        assert!(!el.has_class(&"c".into(), selectors::attr::CaseSensitivity::CaseSensitive));
        assert!(!el.attr_matches(
            &selectors::attr::NamespaceConstraint::Any,
            &"id".into(),
            &AttrSelectorOperation::Exists,
        ));
    }

    /// `:link`/`:any-link` match only an `<a>` with an `href` (the UA stylesheet's link
    /// colour and underline depend on it). The other state pseudo-classes are meaningless
    /// in print output, so they never match.
    #[test]
    fn link_pseudo_classes_match_only_anchors_with_an_href() {
        use crate::style::selector_impl::NonTSPseudoClass;
        use selectors::context::{QuirksMode, SelectorCaches};
        use selectors::matching::{
            MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags,
        };

        let dom = parse(br#"<div><a href="x">link</a><a id="plain">anchor</a></div>"#);
        let mut anchors = Vec::new();
        fn collect(dom: &Dom, id: NodeId, out: &mut Vec<NodeId>) {
            if let NodeData::Element { name, .. } = &dom.node(id).data {
                if &*name.local == "a" {
                    out.push(id);
                }
            }
            for child in dom.children(id) {
                collect(dom, child, out);
            }
        }
        collect(&dom, dom.document(), &mut anchors);

        let mut caches = SelectorCaches::default();
        let mut context = MatchingContext::new(
            MatchingMode::Normal,
            None,
            &mut caches,
            QuirksMode::NoQuirks,
            NeedsSelectorFlags::No,
            MatchingForInvalidation::No,
        );

        let with_href = ElementRef::new(&dom, anchors[0]);
        let without_href = ElementRef::new(&dom, anchors[1]);
        for pc in [NonTSPseudoClass::Link, NonTSPseudoClass::AnyLink] {
            assert!(with_href.match_non_ts_pseudo_class(&pc, &mut context));
            assert!(!without_href.match_non_ts_pseudo_class(&pc, &mut context));
        }
        assert!(!with_href.match_non_ts_pseudo_class(&NonTSPseudoClass::Hover, &mut context));
        assert!(!with_href.match_non_ts_pseudo_class(&NonTSPseudoClass::Visited, &mut context));
    }
}
