//! A minimal arena-based DOM (`Vec<Node>`).
//!
//! Parent, child and sibling relationships are expressed as `NodeId`s (indices into the
//! arena). This avoids per-node reference counting and borrow checking (`Rc<RefCell<Node>>`)
//! so later phases (style computation, layout) can pass the DOM around freely.

use html5ever::{Attribute, QualName};

/// An ID pointing at a node in the arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub(crate) usize);

#[derive(Debug)]
pub struct Node {
    pub(crate) parent: Option<NodeId>,
    pub(crate) previous_sibling: Option<NodeId>,
    pub(crate) next_sibling: Option<NodeId>,
    pub(crate) first_child: Option<NodeId>,
    pub(crate) last_child: Option<NodeId>,
    /// Depth from the root ([`Dom::document`]). Fixed as soon as the node joins the tree,
    /// and recomputed for the whole subtree if it is moved to another parent
    /// ([`set_subtree_depth`]). Used to enforce the depth limit.
    pub(crate) depth: u32,
    pub data: NodeData,
}

impl Node {
    pub(crate) fn new(data: NodeData) -> Self {
        Self {
            parent: None,
            previous_sibling: None,
            next_sibling: None,
            first_child: None,
            last_child: None,
            depth: 0,
            data,
        }
    }
}

#[derive(Debug)]
pub enum NodeData {
    Document,
    Doctype {
        name: String,
    },
    Text {
        contents: String,
    },
    Comment {
        contents: String,
    },
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        /// A separate document node holding the contents of a `<template>` element.
        template_contents: Option<NodeId>,
    },
    ProcessingInstruction {
        target: String,
        contents: String,
    },
    /// A node already freed by [`Dom::release_subtree`].
    ///
    /// The heavy data (text content, attributes) is gone, but `Node`'s
    /// `parent`/`previous_sibling`/`next_sibling`/`first_child`/`last_child`
    /// (the tree links) are left in place. A `NodeId` is a fixed index into the arena
    /// (`Vec<Node>`), and actually removing an element would shift the indices and break
    /// what every other `NodeId` points at. So we "tombstone" instead: the node's slot
    /// stays and only its contents are emptied.
    ///
    /// None of the existing `NodeData` pattern matches (`if let NodeData::Element {..}`
    /// and friends) are exhaustive, and they all handle `Released` naturally on the
    /// wildcard arm, so it is silently ignored as "neither an element nor text". That
    /// fails safe (it never matches by mistake), but the opposite bug -- freeing a node
    /// that should have been kept -- can show up as a silent defect. Whether it is safe
    /// to free at a given point (the "never cross the range a sibling or descendant
    /// selector can see" constraint) is the caller's responsibility;
    /// this type does not enforce it.
    Released,
}

/// Whether a `<link>` element is `rel="stylesheet"`.
///
/// Per the HTML spec, `rel` is a whitespace-separated token list (`rel="stylesheet preload"`
/// is valid too), so a plain whole-string comparison would miss cases where other tokens
/// are mixed in. The same pitfall as token matching on the `class` attribute
/// (`has_class` in `style/element_ref.rs`).
pub fn is_stylesheet_link(attrs: &[Attribute]) -> bool {
    attrs
        .iter()
        .find(|attr| &*attr.name.local == "rel")
        .is_some_and(|attr| {
            attr.value
                .split_ascii_whitespace()
                .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        })
}

/// Text of the first `<title>` in the document (for `/Title` in the PDF Info dictionary).
/// Used as the fallback when `--title` is not given.
pub fn find_document_title(dom: &Dom) -> Option<String> {
    fn walk(dom: &Dom, node: NodeId) -> Option<String> {
        if let NodeData::Element { name, .. } = &dom.node(node).data {
            if &*name.local == "title" {
                let mut text = String::new();
                for child in dom.children(node) {
                    if let NodeData::Text { contents } = &dom.node(child).data {
                        text.push_str(contents);
                    }
                }
                let text = text.trim().to_string();
                if !text.is_empty() {
                    return Some(text);
                }
            }
        }
        for child in dom.children(node) {
            if let Some(found) = walk(dom, child) {
                return Some(found);
            }
        }
        None
    }
    walk(dom, dom.document())
}

/// Value of the first `<base href>` in the document. A `<base>` appearing after `<body>`
/// is ignored (`Mode::Streaming` cannot honour it in principle, so both modes behave
/// the same way).
pub fn find_base_href(dom: &Dom) -> Option<String> {
    fn walk(dom: &Dom, node: NodeId, seen_body: &mut bool) -> Option<String> {
        if let NodeData::Element { name, attrs, .. } = &dom.node(node).data {
            match &*name.local {
                "body" => *seen_body = true,
                "base" if !*seen_body => {
                    let href = attrs
                        .iter()
                        .find(|attr| &*attr.name.local == "href")
                        .map(|attr| attr.value.trim().to_string())
                        .filter(|href| !href.is_empty());
                    if href.is_some() {
                        return href;
                    }
                }
                _ => {}
            }
        }
        for child in dom.children(node) {
            if let Some(found) = walk(dom, child, seen_body) {
                return Some(found);
            }
        }
        None
    }
    let mut seen_body = false;
    walk(dom, dom.document(), &mut seen_body)
}

/// Collect the elements that can be anchor targets (elements with an `id` attribute, plus
/// `<a name>`) as `NodeId` -> name (the `id`/`name` value).
///
/// If the same name appears more than once, the first in document order wins
/// (as the HTML spec requires).
pub fn collect_anchor_targets(dom: &Dom) -> Vec<(NodeId, String)> {
    fn walk(dom: &Dom, node: NodeId, out: &mut Vec<(NodeId, String)>) {
        if let NodeData::Element { name, attrs, .. } = &dom.node(node).data {
            let is_anchor_element = &*name.local == "a";
            let target = attrs
                .iter()
                .find(|attr| {
                    &*attr.name.local == "id" || (is_anchor_element && &*attr.name.local == "name")
                })
                .map(|attr| attr.value.trim().to_string())
                .filter(|value| !value.is_empty());
            if let Some(target) = target {
                if !out.iter().any(|(_, existing)| *existing == target) {
                    out.push((node, target));
                }
            }
        }
        for child in dom.children(node) {
            walk(dom, child, out);
        }
    }
    let mut out = Vec::new();
    walk(dom, dom.document(), &mut out);
    out
}

/// A parsed DOM tree.
pub struct Dom {
    pub(crate) nodes: Vec<Node>,
    pub(crate) document: NodeId,
    /// The greatest depth seen in this tree. It is updated as the tree is built, so it can
    /// be read part-way through parsing (in streaming mode).
    pub(crate) max_depth: u32,
    /// Number of nodes still holding their contents.
    ///
    /// Not the length of `nodes`, but that length minus what [`Self::release_subtree`] has
    /// freed. Freed nodes are not removed from `nodes` (a NodeId is an index, so the list
    /// cannot be compacted), so the length cannot express how much is really held.
    pub(crate) live_nodes: usize,
}

impl Dom {
    pub fn document(&self) -> NodeId {
        self.document
    }

    /// The greatest [`Node::depth`] of anything attached to the tree so far.
    ///
    /// Everything that walks the DOM recursively (style computation, box tree construction,
    /// layout, PDF drawing, and the recursive Drop of `LayoutBox`) consumes stack in
    /// proportion to the depth, so this is compared against the limit
    /// ([`crate::html::MAX_ELEMENT_DEPTH`]) and rejected before any of them run.
    pub fn max_depth(&self) -> u32 {
        self.max_depth
    }

    /// Number of nodes still holding their contents ([`Self::live_nodes`]).
    ///
    /// Computed styles, the box tree and layout results all grow in proportion to this,
    /// so it is what the memory limit is checked against ([`crate::html::MAX_NODES`]).
    pub fn node_count(&self) -> usize {
        self.live_nodes
    }

    /// Add one node and return its `NodeId`.
    ///
    /// This is the single route for appending to `nodes`, which keeps [`Self::live_nodes`]
    /// from getting out of step.
    pub(crate) fn push_node(&mut self, data: NodeData) -> NodeId {
        self.nodes.push(Node::new(data));
        self.live_nodes += 1;
        NodeId(self.nodes.len() - 1)
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.0]
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).parent
    }

    pub fn children(&self, id: NodeId) -> Children<'_> {
        Children {
            dom: self,
            next: self.node(id).first_child,
        }
    }

    /// Recursively free the subtree rooted at `root`.
    ///
    /// Each node's `data` is replaced with [`NodeData::Released`], discarding the heavy
    /// data (text content, attributes and so on). `root` itself is freed too. The tree
    /// links (`parent`/`previous_sibling`/`next_sibling`/`first_child`/`last_child`) are
    /// left untouched, so navigating through this subtree (`children`/`parent`) still
    /// works afterwards.
    ///
    /// This is only safe to call once everything under `root` has finished both style
    /// computation and layout, and it is certain that no later element's selector matching
    /// will reference it (the "never cross the range a sibling or descendant selector can
    /// see" constraint). That judgement is the caller's; `Dom` does not enforce it.
    pub fn release_subtree(&mut self, root: NodeId) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            stack.extend(self.children(id));
            // Skip anything already freed, so a double free is not counted twice.
            if !matches!(self.nodes[id.0].data, NodeData::Released) {
                self.live_nodes -= 1;
                self.nodes[id.0].data = NodeData::Released;
            }
        }
    }

    /// Free only `root`'s descendants, leaving `root` itself as an element.
    ///
    /// The surviving `root` keeps its tag name, classes and id, so later siblings still
    /// see it as "the preceding sibling". In documents using selectors that need the
    /// preceding sibling, such as `+`/`~` or `:first-child`, use this instead of
    /// [`Self::release_subtree`] (`style::needs_preceding_siblings` decides which one
    /// streaming uses).
    ///
    /// Only one node per top-level element survives, so how much can be freed is barely
    /// affected (the descendants are the bulk of it).
    pub fn release_descendants(&mut self, root: NodeId) {
        let children: Vec<NodeId> = self.children(root).collect();
        for child in children {
            self.release_subtree(child);
        }
    }

    /// Whether `id` has already been freed by [`Dom::release_subtree`].
    pub fn is_released(&self, id: NodeId) -> bool {
        matches!(self.node(id).data, NodeData::Released)
    }
}

pub struct Children<'a> {
    dom: &'a Dom,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.dom.node(current).next_sibling;
        Some(current)
    }
}

/// Detach `id` from its parent and siblings.
pub(crate) fn detach(nodes: &mut [Node], id: NodeId) {
    let (parent, previous_sibling, next_sibling) = {
        let node = &mut nodes[id.0];
        (
            node.parent.take(),
            node.previous_sibling.take(),
            node.next_sibling.take(),
        )
    };

    if let Some(next) = next_sibling {
        nodes[next.0].previous_sibling = previous_sibling;
    } else if let Some(parent) = parent {
        nodes[parent.0].last_child = previous_sibling;
    }

    if let Some(previous) = previous_sibling {
        nodes[previous.0].next_sibling = next_sibling;
    } else if let Some(parent) = parent {
        nodes[parent.0].first_child = next_sibling;
    }
}

/// Append `child` as the last child of `parent` (detaching it from any existing parent).
/// Recompute [`Node::depth`] under `root` starting from `depth`, and return the greatest depth in the subtree.
///
/// Walked with an explicit stack. Written recursively, the very code enforcing the depth
/// limit would overflow the stack on a deep DOM, which rather defeats the point.
pub(crate) fn set_subtree_depth(nodes: &mut [Node], root: NodeId, depth: u32) -> u32 {
    let mut max = depth;
    let mut stack = vec![(root, depth)];
    while let Some((id, d)) = stack.pop() {
        nodes[id.0].depth = d;
        max = max.max(d);
        let mut child = nodes[id.0].first_child;
        while let Some(c) = child {
            stack.push((c, d + 1));
            child = nodes[c.0].next_sibling;
        }
    }
    max
}

/// Attach `child` at the end of `parent` and return the greatest depth of the attached subtree.
pub(crate) fn append(nodes: &mut [Node], parent: NodeId, child: NodeId) -> u32 {
    detach(nodes, child);

    nodes[child.0].parent = Some(parent);
    if let Some(last) = nodes[parent.0].last_child {
        nodes[child.0].previous_sibling = Some(last);
        nodes[last.0].next_sibling = Some(child);
    } else {
        nodes[parent.0].first_child = Some(child);
    }
    nodes[parent.0].last_child = Some(child);

    set_subtree_depth(nodes, child, nodes[parent.0].depth + 1)
}

/// Insert `new_node` immediately before `sibling` (detaching it from any existing parent).
pub(crate) fn insert_before(nodes: &mut [Node], sibling: NodeId, new_node: NodeId) -> u32 {
    detach(nodes, new_node);

    let parent = nodes[sibling.0].parent;
    nodes[new_node.0].parent = parent;
    nodes[new_node.0].next_sibling = Some(sibling);

    let previous = nodes[sibling.0].previous_sibling;
    nodes[new_node.0].previous_sibling = previous;
    if let Some(previous) = previous {
        nodes[previous.0].next_sibling = Some(new_node);
    } else if let Some(parent) = parent {
        nodes[parent.0].first_child = Some(new_node);
    }
    nodes[sibling.0].previous_sibling = Some(new_node);

    // They sit as siblings, so the depth is the same as `sibling`'s.
    set_subtree_depth(nodes, new_node, nodes[sibling.0].depth)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::parse;

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    #[test]
    fn release_subtree_marks_root_and_descendants_as_released() {
        let mut dom = parse(br#"<div><p>Hello <b>world</b></p></div>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");
        let b = find(&dom, p, "b").expect("b not found");

        dom.release_subtree(p);

        assert!(dom.is_released(p), "root of the released subtree");
        assert!(dom.is_released(b), "descendant of the released subtree");
    }

    #[test]
    fn release_subtree_does_not_affect_siblings_or_ancestors() {
        let mut dom = parse(br#"<div><p>first</p><p>second</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let first = dom.children(div).next().expect("first <p> not found");
        let second = dom.children(div).nth(1).expect("second <p> not found");

        dom.release_subtree(first);

        assert!(dom.is_released(first));
        assert!(
            !dom.is_released(second),
            "sibling outside the released subtree must be unaffected"
        );
        assert!(
            !dom.is_released(div),
            "ancestor outside the released subtree must be unaffected"
        );
    }

    #[test]
    fn tree_navigation_still_works_across_a_released_subtree() {
        // Even with the first sibling freed, the tree links themselves survive, so
        // navigating from the second one to its parent and ancestors still works.
        let mut dom = parse(br#"<div><p>first</p><p>second</p></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");
        let first = dom.children(div).next().expect("first <p> not found");
        let second = dom.children(div).nth(1).expect("second <p> not found");

        dom.release_subtree(first);

        assert_eq!(dom.parent(second), Some(div));
        assert_eq!(
            dom.children(div).count(),
            2,
            "child link count is preserved"
        );
    }

    #[test]
    fn find_base_href_returns_the_first_base_in_head() {
        let dom = crate::html::parse(
            br#"<html><head><base href="https://example.com/docs/"><base href="https://other.example/"></head><body>x</body></html>"#,
        );
        assert_eq!(
            find_base_href(&dom).as_deref(),
            Some("https://example.com/docs/")
        );
    }

    #[test]
    fn find_base_href_is_none_without_a_base_element() {
        let dom = crate::html::parse(br#"<html><head></head><body>x</body></html>"#);
        assert!(find_base_href(&dom).is_none());
    }

    #[test]
    fn find_base_href_ignores_a_base_without_href_and_an_empty_href() {
        let dom = crate::html::parse(
            br#"<html><head><base target="_blank"><base href="  "></head><body>x</body></html>"#,
        );
        assert!(find_base_href(&dom).is_none());
    }

    #[test]
    fn find_base_href_ignores_a_base_that_appears_after_body_starts() {
        // `Mode::Streaming` cannot honour it in principle, so both modes ignore it.
        let dom = crate::html::parse(
            br#"<html><body><base href="https://example.com/"><p>x</p></body></html>"#,
        );
        assert!(find_base_href(&dom).is_none());
    }
}
