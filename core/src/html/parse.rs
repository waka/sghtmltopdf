//! The html5ever `TreeSink` implementation. Assembles the parse result into a [`Dom`].

use std::cell::{Cell, Ref, RefCell, RefMut};

use html5ever::interface::tree_builder::{
    ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink,
};
use html5ever::tendril::stream::Utf8LossyDecoder;
use html5ever::tendril::{ByteTendril, StrTendril, TendrilSink};
use html5ever::{parse_document, Attribute, LocalName, Namespace, Parser, QualName};

use super::dom::{append, detach, insert_before, Dom, Node, NodeData, NodeId};

/// Parse HTML bytes into a [`Dom`] (whole-document conversion).
///
/// Internally a thin wrapper that `feed`s all the bytes to [`StreamingParser`] in one go,
/// so the batch API and the chunked API share the same logic.
pub fn parse(html: &[u8]) -> Dom {
    let mut parser = StreamingParser::new();
    parser.feed(html);
    parser.finish()
}

/// A parser that accepts HTML chunk by chunk.
///
/// html5ever's tokenizer is itself designed for streaming, and
/// [`Utf8LossyDecoder`](html5ever::driver::Utf8LossyDecoder) joins a `feed` boundary that
/// falls in the middle of a multi-byte UTF-8 character with the following bytes before
/// decoding (the `tendril` crate's incremental decoding). Callers therefore do not need
/// to align chunk boundaries to UTF-8 character boundaries.
pub struct StreamingParser {
    inner: Utf8LossyDecoder<Parser<Sink>>,
    /// The last child directly under `<body>` that
    /// [`Self::take_completed_top_level_children`] has already returned. The next call resumes from there.
    last_yielded_top_level_child: Option<NodeId>,
}

impl StreamingParser {
    pub fn new() -> Self {
        let sink = Sink {
            dom: RefCell::new(Dom {
                nodes: vec![Node::new(NodeData::Document)],
                document: NodeId(0),
                max_depth: 0,
                // For the document node itself.
                live_nodes: 1,
            }),
            quirks_mode: Cell::new(QuirksMode::NoQuirks),
            seen_body: Cell::new(false),
            late_css_source_detected: Cell::new(false),
            body_id: Cell::new(None),
        };
        Self {
            inner: parse_document(sink, Default::default()).from_utf8(),
            last_yielded_top_level_child: None,
        }
    }

    /// Feed one chunk of HTML bytes. May be called any number of times.
    pub fn feed(&mut self, chunk: &[u8]) {
        self.inner.process(ByteTendril::from_slice(chunk));
    }

    /// Whether a CSS source (a `<style>`, or a `<link>` with `rel="stylesheet"`) appeared
    /// after `<body>`.
    ///
    /// Used to decide whether `Engine`'s `Mode::Streaming` returns an error
    /// (`Mode::Batch` may ignore it). It becomes `true` if even one
    /// `<style>`/`<link rel=stylesheet>` element was created at or after the point the
    /// `<body>` start tag was seen.
    pub fn has_late_css_source(&self) -> bool {
        self.sink().late_css_source_detected.get()
    }

    /// The `NodeId` of the `<body>` element (`None` if it has not been parsed yet).
    pub fn body_node(&self) -> Option<NodeId> {
        self.sink().body_id.get()
    }

    /// Read-only access to the [`Dom`] while it is still being parsed (before `finish`).
    ///
    /// Used by `Engine`'s true streaming path, which computes styles and builds the box
    /// tree for each top-level element directly under `<body>` as soon as it is final,
    /// without waiting for `finish`.
    pub fn dom(&self) -> Ref<'_, Dom> {
        self.sink().dom.borrow()
    }

    /// The writable version of [`Self::dom`]. `Engine` uses it to free a top-level
    /// element's subtree via [`crate::html::Dom::release_subtree`].
    pub fn dom_mut(&self) -> RefMut<'_, Dom> {
        self.sink().dom.borrow_mut()
    }

    /// Return, in document order, the `NodeId`s of the children directly under `<body>`
    /// that can be considered final (that is, a later sibling has already been added).
    /// Each call advances the position already returned, so the same element is never
    /// returned twice. Returns an empty vector if `<body>` does not exist yet, or if
    /// there is nothing to return.
    ///
    /// The last element is excluded, since children may still be being added to it
    /// (it waits for a later call, or for [`Self::finish`]).
    pub fn take_completed_top_level_children(&mut self) -> Vec<NodeId> {
        let Some(body) = self.body_node() else {
            return Vec::new();
        };

        let dom = self.dom();
        let mut children: Vec<NodeId> = dom.children(body).collect();
        drop(dom);

        if children.len() < 2 {
            // Nothing but the last one is final (there are only 0 or 1 elements).
            return Vec::new();
        }

        let start = match self.last_yielded_top_level_child {
            Some(last) => match children.iter().position(|&id| id == last) {
                Some(i) => i + 1,
                None => 0,
            },
            None => 0,
        };
        // Exclude the last element, since children may still be being added to it.
        let end = children.len() - 1;
        if start >= end {
            return Vec::new();
        }

        children.truncate(end);
        let result = children.split_off(start);
        if let Some(&last) = result.last() {
            self.last_yielded_top_level_child = Some(last);
        }
        result
    }

    /// Like [`Self::take_completed_top_level_children`], but returns everything including
    /// the last element. Used once it is certain that no more children will be added to
    /// `<body>` (immediately before `Engine::finish` is called).
    pub fn take_all_remaining_top_level_children(&mut self) -> Vec<NodeId> {
        let Some(body) = self.body_node() else {
            return Vec::new();
        };
        let dom = self.dom();
        let children: Vec<NodeId> = dom.children(body).collect();
        drop(dom);

        let start = match self.last_yielded_top_level_child {
            Some(last) => children
                .iter()
                .position(|&id| id == last)
                .map(|i| i + 1)
                .unwrap_or(0),
            None => 0,
        };
        let result = children[start..].to_vec();
        self.last_yielded_top_level_child = children.last().copied();
        result
    }

    fn sink(&self) -> &Sink {
        &self.inner.inner_sink.tokenizer.sink.sink
    }

    /// Signal that there are no more chunks and take the parsed [`Dom`].
    pub fn finish(self) -> Dom {
        self.inner.finish()
    }
}

impl Default for StreamingParser {
    fn default() -> Self {
        Self::new()
    }
}

struct Sink {
    dom: RefCell<Dom>,
    quirks_mode: Cell<QuirksMode>,
    /// Whether the `<body>` start tag has been seen
    seen_body: Cell<bool>,
    /// Whether a CSS source (a `<style>` or a `<link rel=stylesheet>`) appeared after
    /// `<body>`.
    late_css_source_detected: Cell<bool>,
    /// The `NodeId` of the `<body>` element
    body_id: Cell<Option<NodeId>>,
}

/// Whether `name`/`attrs` describe an element treated as a CSS source (a `<style>`, or a
/// `<link>` with `rel="stylesheet"`), so that an appearance after `<body>` is an error
/// just as it is for `<style>`.
fn is_late_css_source(local_name: &str, attrs: &[Attribute]) -> bool {
    local_name == "style" || (local_name == "link" && super::dom::is_stylesheet_link(attrs))
}

/// The element name returned by [`TreeSink::elem_name`], independent of what lent it.
///
/// The arena lives behind a single `RefCell`, so a borrow cannot be returned directly as
/// `&'a QualName` (the borrow guard's lifetime does not line up).
#[derive(Debug)]
struct OwnedElemName(QualName);

impl ElemName for OwnedElemName {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

impl Sink {
    fn alloc(&self, data: NodeData) -> NodeId {
        self.dom.borrow_mut().push_node(data)
    }

    /// Text is concatenated onto the preceding sibling when that is a Text node (as html5ever specifies).
    ///
    /// `do_append` returns the greatest depth of the attached subtree. Attaching to the
    /// tree is funnelled through here, so [`Dom::max_depth`] only needs updating here too.
    fn append_common(
        &self,
        child: NodeOrText<NodeId>,
        previous_sibling: impl FnOnce(&[Node]) -> Option<NodeId>,
        do_append: impl FnOnce(&mut [Node], NodeId) -> u32,
    ) {
        let mut dom = self.dom.borrow_mut();

        let new_node = match child {
            NodeOrText::AppendText(text) => {
                if let Some(prev) = previous_sibling(&dom.nodes) {
                    if let NodeData::Text { contents } = &mut dom.nodes[prev.0].data {
                        contents.push_str(&text);
                        return;
                    }
                }
                let id = dom.push_node(NodeData::Text {
                    contents: text.to_string(),
                });
                let depth = do_append(&mut dom.nodes, id);
                dom.max_depth = dom.max_depth.max(depth);
                return;
            }
            NodeOrText::AppendNode(id) => id,
        };

        let depth = do_append(&mut dom.nodes, new_node);
        dom.max_depth = dom.max_depth.max(depth);
    }
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a> = OwnedElemName;

    fn finish(self) -> Dom {
        self.dom.into_inner()
    }

    fn parse_error(&self, _msg: std::borrow::Cow<'static, str>) {}

    fn get_document(&self) -> NodeId {
        self.dom.borrow().document
    }

    fn elem_name<'a>(&'a self, target: &'a NodeId) -> OwnedElemName {
        let dom = self.dom.borrow();
        match &dom.nodes[target.0].data {
            NodeData::Element { name, .. } => OwnedElemName(name.clone()),
            _ => panic!("not an element!"),
        }
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> NodeId {
        let is_body = &*name.local == "body";
        if is_body {
            self.seen_body.set(true);
        } else if self.seen_body.get() && is_late_css_source(&name.local, &attrs) {
            self.late_css_source_detected.set(true);
        }

        let template_contents = if flags.template {
            Some(self.alloc(NodeData::Document))
        } else {
            None
        };
        let id = self.alloc(NodeData::Element {
            name,
            attrs,
            template_contents,
        });
        if is_body {
            self.body_id.set(Some(id));
        }
        id
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.alloc(NodeData::Comment {
            contents: text.to_string(),
        })
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.alloc(NodeData::ProcessingInstruction {
            target: target.to_string(),
            contents: data.to_string(),
        })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let parent = *parent;
        self.append_common(
            child,
            |nodes| nodes[parent.0].last_child,
            |nodes, new_node| append(nodes, parent, new_node),
        );
    }

    fn append_before_sibling(&self, sibling: &NodeId, child: NodeOrText<NodeId>) {
        let sibling = *sibling;
        self.append_common(
            child,
            |nodes| nodes[sibling.0].previous_sibling,
            |nodes, new_node| insert_before(nodes, sibling, new_node),
        );
    }

    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self.dom.borrow().nodes[element.0].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
        let doctype = self.alloc(NodeData::Doctype {
            name: name.to_string(),
        });
        let mut dom = self.dom.borrow_mut();
        let document = dom.document;
        let depth = append(&mut dom.nodes, document, doctype);
        dom.max_depth = dom.max_depth.max(depth);
    }

    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        match &self.dom.borrow().nodes[target.0].data {
            NodeData::Element {
                template_contents: Some(contents),
                ..
            } => *contents,
            _ => panic!("not a template element!"),
        }
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.quirks_mode.set(mode);
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut dom = self.dom.borrow_mut();
        let NodeData::Element {
            attrs: existing, ..
        } = &mut dom.nodes[target.0].data
        else {
            panic!("not an element");
        };
        for attr in attrs {
            if !existing.iter().any(|a| a.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        detach(&mut self.dom.borrow_mut().nodes, *target);
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        let mut dom = self.dom.borrow_mut();
        let mut next_child = dom.nodes[node.0].first_child;
        while let Some(child) = next_child {
            next_child = dom.nodes[child.0].next_sibling;
            // The whole subtree moves to a different parent, so `append` recomputes the depths.
            let depth = append(&mut dom.nodes, *new_parent, child);
            dom.max_depth = dom.max_depth.max(depth);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk the tree in pre-order and return the first element with the given tag name.
    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    fn text_of(dom: &Dom, id: NodeId) -> String {
        let mut out = String::new();
        collect_text(dom, id, &mut out);
        out
    }

    fn collect_text(dom: &Dom, id: NodeId, out: &mut String) {
        if let NodeData::Text { contents } = &dom.node(id).data {
            out.push_str(contents);
        }
        for child in dom.children(id) {
            collect_text(dom, child, out);
        }
    }

    #[test]
    fn parses_element_tree_with_attrs_and_text() {
        let dom = parse(br#"<div class="a"><p>Hello <b>world</b></p></div>"#);

        let div = find(&dom, dom.document(), "div").expect("div not found");
        let NodeData::Element { attrs, .. } = &dom.node(div).data else {
            panic!("expected element")
        };
        assert_eq!(attrs.len(), 1);
        assert_eq!(&*attrs[0].name.local, "class");
        assert_eq!(&*attrs[0].value, "a");

        let p = find(&dom, div, "p").expect("p not found");
        assert_eq!(text_of(&dom, p), "Hello world");

        let b = find(&dom, p, "b").expect("b not found");
        assert_eq!(text_of(&dom, b), "world");
    }

    #[test]
    fn merges_adjacent_text_into_a_single_node() {
        // "&amp;" is handled separately inside the tokenizer as a character reference, so
        // this is the classic case where a naive implementation splits adjacent text nodes.
        let dom = parse(br#"<p>AT&amp;T</p>"#);
        let p = find(&dom, dom.document(), "p").expect("p not found");

        let children: Vec<_> = dom.children(p).collect();
        assert_eq!(
            children.len(),
            1,
            "adjacent text nodes should be merged into one"
        );
        assert_eq!(text_of(&dom, p), "AT&T");
    }

    #[test]
    fn parses_sibling_elements_in_order() {
        let dom = parse(br#"<ul><li>one</li><li>two</li><li>three</li></ul>"#);
        let ul = find(&dom, dom.document(), "ul").expect("ul not found");

        let lis: Vec<_> = dom.children(ul).collect();
        assert_eq!(lis.len(), 3);
        assert_eq!(text_of(&dom, lis[0]), "one");
        assert_eq!(text_of(&dom, lis[1]), "two");
        assert_eq!(text_of(&dom, lis[2]), "three");
    }

    /// Feeding the bytes one at a time must produce the same DOM as a whole-input `parse`.
    #[test]
    fn streaming_parser_byte_by_byte_matches_one_shot_parse() {
        let html = br#"<div class="a"><p>Hello <b>world</b></p></div>"#;

        let mut parser = StreamingParser::new();
        for byte in html {
            parser.feed(std::slice::from_ref(byte));
        }
        let streamed = parser.finish();
        let batched = parse(html);

        let streamed_p = find(&streamed, streamed.document(), "p").expect("p not found");
        let batched_p = find(&batched, batched.document(), "p").expect("p not found");
        assert_eq!(text_of(&streamed, streamed_p), text_of(&batched, batched_p));

        let streamed_div = find(&streamed, streamed.document(), "div").expect("div not found");
        let NodeData::Element { attrs, .. } = &streamed.node(streamed_div).data else {
            panic!("expected element")
        };
        assert_eq!(&*attrs[0].value, "a");
    }

    /// A multi-byte UTF-8 character split across a chunk boundary (each character of
    /// "nihongo" is three bytes) must still be joined correctly by `Utf8LossyDecoder`'s
    /// incremental decoding, rather than turning into mojibake.
    #[test]
    fn streaming_parser_handles_utf8_multibyte_char_split_across_chunks() {
        let html = "<p>日本語のテスト</p>".as_bytes();

        let mut parser = StreamingParser::new();
        for byte in html {
            parser.feed(std::slice::from_ref(byte));
        }
        let dom = parser.finish();

        let p = find(&dom, dom.document(), "p").expect("p not found");
        assert_eq!(text_of(&dom, p), "日本語のテスト");
    }

    /// Text fed over several calls must be merged into a single adjacent text node just as
    /// a whole-input `parse` does (confirming the merging logic in `html::dom` is not
    /// affected by how the input is chunked).
    #[test]
    fn streaming_parser_merges_text_fed_across_multiple_chunks() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<p>Hello");
        parser.feed(b", ");
        parser.feed(b"world!</p>");
        let dom = parser.finish();

        let p = find(&dom, dom.document(), "p").expect("p not found");
        let children: Vec<_> = dom.children(p).collect();
        assert_eq!(
            children.len(),
            1,
            "text fed across multiple chunks should still merge into one node"
        );
        assert_eq!(text_of(&dom, p), "Hello, world!");
    }

    #[test]
    fn has_late_css_source_is_false_when_style_is_in_head() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<html><head><style>p{color:red}</style></head><body><p>x</p></body></html>");
        assert!(!parser.has_late_css_source());
    }

    #[test]
    fn has_late_css_source_is_true_when_style_appears_after_body_starts() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<body><p>x</p><style>p{color:red}</style></body>");
        assert!(parser.has_late_css_source());
    }

    #[test]
    fn has_late_css_source_updates_incrementally_across_feed_calls() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<body><p>x</p>");
        assert!(
            !parser.has_late_css_source(),
            "no <style> tag has appeared yet"
        );
        parser.feed(b"<style>p{color:red}</style>");
        assert!(
            parser.has_late_css_source(),
            "should detect the <style> tag fed in a later chunk"
        );
    }

    #[test]
    fn has_late_css_source_is_false_when_stylesheet_link_is_in_head() {
        let mut parser = StreamingParser::new();
        parser.feed(
            br#"<html><head><link rel="stylesheet" href="a.css"></head><body><p>x</p></body></html>"#,
        );
        assert!(!parser.has_late_css_source());
    }

    #[test]
    fn has_late_css_source_is_true_when_stylesheet_link_appears_after_body_starts() {
        let mut parser = StreamingParser::new();
        parser.feed(br#"<body><p>x</p><link rel="stylesheet" href="a.css"></body>"#);
        assert!(parser.has_late_css_source());
    }

    #[test]
    fn has_late_css_source_ignores_a_late_link_that_is_not_a_stylesheet() {
        let mut parser = StreamingParser::new();
        parser.feed(br#"<body><p>x</p><link rel="icon" href="favicon.ico"></body>"#);
        assert!(!parser.has_late_css_source());
    }

    #[test]
    fn has_late_css_source_detects_stylesheet_among_multiple_rel_tokens() {
        // rel is a whitespace-separated token list (a form such as
        // rel="preload stylesheet" is valid too).
        let mut parser = StreamingParser::new();
        parser.feed(br#"<body><p>x</p><link rel="preload stylesheet" href="a.css"></body>"#);
        assert!(parser.has_late_css_source());
    }

    fn tag_of(parser: &StreamingParser, id: NodeId) -> String {
        let dom = parser.dom();
        match &dom.node(id).data {
            NodeData::Element { name, .. } => name.local.to_string(),
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn take_completed_top_level_children_is_empty_before_body_exists() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<html><head><title>t</title></head>");
        assert!(parser.take_completed_top_level_children().is_empty());
    }

    #[test]
    fn take_completed_top_level_children_holds_back_the_last_child() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<body><div>a</div>");
        assert!(
            parser.take_completed_top_level_children().is_empty(),
            "only one top-level child exists so far; it might still grow"
        );
    }

    #[test]
    fn take_completed_top_level_children_yields_once_a_sibling_follows() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<body><div>a</div><p>b</p>");
        let completed = parser.take_completed_top_level_children();
        assert_eq!(completed.len(), 1);
        assert_eq!(tag_of(&parser, completed[0]), "div");

        // The second one (p) is still held back.
        assert!(parser.take_completed_top_level_children().is_empty());
    }

    #[test]
    fn take_completed_top_level_children_does_not_repeat_already_yielded_nodes() {
        let mut parser = StreamingParser::new();
        parser.feed(b"<body><div>a</div><p>b</p><span>c</span>");
        let first_batch = parser.take_completed_top_level_children();
        assert_eq!(
            first_batch
                .iter()
                .map(|&id| tag_of(&parser, id))
                .collect::<Vec<_>>(),
            vec!["div", "p"]
        );

        parser.feed(b"<footer>d</footer>");
        let second_batch = parser.take_completed_top_level_children();
        assert_eq!(
            second_batch
                .iter()
                .map(|&id| tag_of(&parser, id))
                .collect::<Vec<_>>(),
            vec!["span"],
            "should yield only newly-completed nodes, not repeat earlier ones"
        );
    }
}
