//! Tagged PDF: the structure tree and the marked content it points at.
//!
//! Tagging is opt-in (`--tagged`). While a page is drawn, [`Tagger`] follows the boxes being
//! painted: a box whose element has a [`TagRole`] opens a structure element, text and images
//! are written as marked-content sequences carrying an `MCID`, and everything that is only
//! decoration (backgrounds, borders, rules, header and footer) is marked as an `Artifact`.
//! At the end the elements are written as the `/StructTreeRoot` with its `/ParentTree`.
//!
//! Inline elements (`<strong>`, `<span>`) have no box of their own and are not represented; the
//! text of a paragraph is a sequence of marked-content items of the paragraph, split only where
//! a link starts or ends (each link becomes a `Link` element, which also holds the link
//! annotation drawn over it).

use std::collections::HashMap;

use pdf_writer::types::{StructRole, TableHeaderScope};
use pdf_writer::{Chunk, Name, Ref, TextStr};

use crate::html::{Dom, NodeData, NodeId};

/// Which rows or columns a table header cell labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellScope {
    Row,
    Column,
    Both,
}

/// What kind of structure element an HTML element becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagRole {
    /// `<h1>` to `<h6>`: the level, 1 to 6.
    Heading(u8),
    /// `<p>`, `<pre>`.
    Paragraph,
    /// `<div>`.
    Division,
    /// `<section>`.
    Section,
    /// `<article>`.
    Article,
    /// `<blockquote>`.
    BlockQuote,
    /// `<ul>`, `<ol>`.
    List,
    /// `<li>`.
    ListItem,
    /// `<img>` with a non-empty `alt`. An `<img>` without one is decoration and an `Artifact`.
    Figure { alt: String },
    /// `<table>`.
    Table,
    /// `<caption>`.
    Caption,
    /// `<thead>`.
    TableHead,
    /// `<tbody>`.
    TableBody,
    /// `<tfoot>`.
    TableFoot,
    /// `<tr>`.
    TableRow,
    /// `<th>` (`header`) or `<td>`.
    TableCell {
        header: bool,
        scope: Option<CellScope>,
        col_span: i32,
        row_span: i32,
    },
}

impl TagRole {
    fn struct_role(&self) -> StructRole {
        match self {
            Self::Heading(1) => StructRole::H1,
            Self::Heading(2) => StructRole::H2,
            Self::Heading(3) => StructRole::H3,
            Self::Heading(4) => StructRole::H4,
            Self::Heading(5) => StructRole::H5,
            Self::Heading(_) => StructRole::H6,
            Self::Paragraph => StructRole::P,
            Self::Division => StructRole::Div,
            Self::Section => StructRole::Sect,
            Self::Article => StructRole::Art,
            Self::BlockQuote => StructRole::BlockQuote,
            Self::List => StructRole::L,
            Self::ListItem => StructRole::LI,
            Self::Figure { .. } => StructRole::Figure,
            Self::Table => StructRole::Table,
            Self::Caption => StructRole::Caption,
            Self::TableHead => StructRole::THead,
            Self::TableBody => StructRole::TBody,
            Self::TableFoot => StructRole::TFoot,
            Self::TableRow => StructRole::TR,
            Self::TableCell { header: true, .. } => StructRole::TH,
            Self::TableCell { header: false, .. } => StructRole::TD,
        }
    }

    fn table_attributes(&self) -> Option<TableAttributes> {
        match self {
            Self::TableCell {
                header,
                scope,
                col_span,
                row_span,
            } => Some(TableAttributes {
                scope: header.then_some(*scope).flatten(),
                col_span: *col_span,
                row_span: *row_span,
            }),
            _ => None,
        }
    }
}

/// The `Table` attributes of a cell (`/Scope`, `/ColSpan`, `/RowSpan`).
#[derive(Debug, Clone, Copy)]
struct TableAttributes {
    scope: Option<CellScope>,
    col_span: i32,
    row_span: i32,
}

/// Map the elements of `dom` that become structure elements to their [`TagRole`].
pub fn collect_tag_roles(dom: &Dom) -> HashMap<NodeId, TagRole> {
    fn span(attrs: &[html5ever::Attribute], name: &str) -> i32 {
        attrs
            .iter()
            .find(|a| &*a.name.local == name)
            .and_then(|a| a.value.trim().parse::<i32>().ok())
            .filter(|&n| n > 1)
            .unwrap_or(1)
    }

    fn role_of(name: &str, attrs: &[html5ever::Attribute], in_head: bool) -> Option<TagRole> {
        Some(match name {
            "h1" => TagRole::Heading(1),
            "h2" => TagRole::Heading(2),
            "h3" => TagRole::Heading(3),
            "h4" => TagRole::Heading(4),
            "h5" => TagRole::Heading(5),
            "h6" => TagRole::Heading(6),
            "p" | "pre" => TagRole::Paragraph,
            "div" => TagRole::Division,
            "section" => TagRole::Section,
            "article" => TagRole::Article,
            "blockquote" => TagRole::BlockQuote,
            "ul" | "ol" => TagRole::List,
            "li" => TagRole::ListItem,
            "table" => TagRole::Table,
            "caption" => TagRole::Caption,
            "thead" => TagRole::TableHead,
            "tbody" => TagRole::TableBody,
            "tfoot" => TagRole::TableFoot,
            "tr" => TagRole::TableRow,
            "td" | "th" => {
                let header = name == "th";
                let scope = attrs
                    .iter()
                    .find(|a| &*a.name.local == "scope")
                    .and_then(|a| match a.value.trim().to_ascii_lowercase().as_str() {
                        "col" | "colgroup" => Some(CellScope::Column),
                        "row" | "rowgroup" => Some(CellScope::Row),
                        _ => None,
                    })
                    // A header in `<thead>` labels its column unless it says otherwise.
                    .or(in_head.then_some(CellScope::Column));
                TagRole::TableCell {
                    header,
                    scope,
                    col_span: span(attrs, "colspan"),
                    row_span: span(attrs, "rowspan"),
                }
            }
            "img" => {
                let alt = attrs.iter().find(|a| &*a.name.local == "alt")?;
                let alt = alt.value.trim();
                if alt.is_empty() {
                    return None;
                }
                TagRole::Figure {
                    alt: alt.to_string(),
                }
            }
            _ => return None,
        })
    }

    fn walk(dom: &Dom, node: NodeId, in_head: bool, out: &mut HashMap<NodeId, TagRole>) {
        let mut in_head = in_head;
        if let NodeData::Element { name, attrs, .. } = &dom.node(node).data {
            if let Some(role) = role_of(&name.local, attrs, in_head) {
                out.insert(node, role);
            }
            if &*name.local == "thead" {
                in_head = true;
            } else if matches!(&*name.local, "tbody" | "tfoot" | "table") {
                in_head = false;
            }
        }
        for child in dom.children(node) {
            walk(dom, child, in_head, out);
        }
    }
    let mut out = HashMap::new();
    walk(dom, dom.document(), false, &mut out);
    out
}

enum Kid {
    Elem(usize),
    /// A marked-content sequence of a page or, with `stm`, of a Form XObject.
    Content {
        page: Ref,
        stm: Option<Ref>,
        mcid: i32,
    },
    /// An annotation.
    Object {
        page: Ref,
        obj: Ref,
    },
}

struct Elem {
    role: StructRole,
    alt: Option<String>,
    table: Option<TableAttributes>,
    kids: Vec<Kid>,
    /// The stream (page) the element was first used on.
    first_stream: usize,
    /// For a `Link`: where its text is, for finding its annotation.
    link: Option<Bounds>,
}

/// A rectangle in the PDF coordinates of the page (CSS px, origin bottom left).
#[derive(Debug, Clone, Copy)]
pub(super) struct Bounds {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Bounds {
    fn overlap_area(&self, other: &Bounds) -> f32 {
        let w = self.x1.min(other.x1) - self.x0.max(other.x0);
        let h = self.y1.min(other.y1) - self.y0.max(other.y0);
        if w > 0.0 && h > 0.0 {
            w * h
        } else {
            // Touching or degenerate rectangles still count as a (tiny) overlap.
            let touching = w >= 0.0 && h >= 0.0;
            if touching {
                f32::MIN_POSITIVE
            } else {
                0.0
            }
        }
    }
}

/// A content stream with marked content: a page, or a Form XObject drawn on one.
struct Stream {
    page: Ref,
    stm: Option<Ref>,
    /// The `/StructParents` key.
    key: i32,
    /// The element owning each `MCID`, by index.
    owners: Vec<usize>,
}

/// What a `/ParentTree` key stands for.
enum ParentEntry {
    /// A content stream: the owner of each of its `MCID`s.
    Stream(usize),
    /// An annotation: its `Link` element.
    Annotation(usize),
}

/// What [`Tagger::begin_row`] did, to be handed back to [`Tagger::end_row`].
#[derive(Debug, Clone, Copy)]
pub(super) struct RowTag {
    pub pushed: bool,
    /// A repeated header row: an artifact, with nothing inside tagged.
    pub repeated: bool,
}

/// Follows the drawing of the pages and collects the structure tree.
pub(super) struct Tagger {
    roles: HashMap<NodeId, TagRole>,
    /// Element 0 is the `Document`.
    elems: Vec<Elem>,
    parents: Vec<usize>,
    by_node: HashMap<NodeId, usize>,
    /// The open elements, innermost last. Never empty.
    stack: Vec<usize>,
    streams: Vec<Stream>,
    /// The stream of the page being drawn, and the one being drawn into (a Form XObject).
    page_stream: usize,
    current_stream: usize,
    entries: Vec<(i32, ParentEntry)>,
    /// The `Link` elements of the page being drawn, by what they link to, so an annotation finds
    /// its element without scanning every element. The annotations of a page are attached as
    /// soon as it is drawn, so the index only ever holds that page.
    links: HashMap<String, Vec<usize>>,
    /// While above zero nothing is tagged (header, footer and other page furniture).
    suspended: u32,
}

impl Tagger {
    pub(super) fn new(roles: HashMap<NodeId, TagRole>) -> Self {
        Self {
            roles,
            elems: vec![Elem {
                role: StructRole::Document,
                alt: None,
                table: None,
                kids: Vec::new(),
                first_stream: 0,
                link: None,
            }],
            parents: vec![0],
            by_node: HashMap::new(),
            stack: vec![0],
            streams: Vec::new(),
            page_stream: 0,
            current_stream: 0,
            entries: Vec::new(),
            links: HashMap::new(),
            suspended: 0,
        }
    }

    fn new_stream(&mut self, page: Ref, stm: Option<Ref>) -> usize {
        let key = self.entries.len() as i32;
        let index = self.streams.len();
        self.streams.push(Stream {
            page,
            stm,
            key,
            owners: Vec::new(),
        });
        self.entries.push((key, ParentEntry::Stream(index)));
        index
    }

    pub(super) fn begin_page(&mut self, page: Ref) {
        let index = self.new_stream(page, None);
        self.page_stream = index;
        self.current_stream = index;
        self.links.clear();
    }

    /// The `/StructParents` key of the page being drawn, if anything on it was tagged.
    pub(super) fn current_page_key(&self) -> Option<i32> {
        let stream = self.streams.get(self.page_stream)?;
        (!stream.owners.is_empty()).then_some(stream.key)
    }

    /// Draw into the Form XObject `form` from now on. Returns what to hand to [`Self::leave_form`].
    pub(super) fn enter_form(&mut self, form: Ref) -> usize {
        let previous = self.current_stream;
        let page = self.streams[self.page_stream].page;
        self.current_stream = self.new_stream(page, Some(form));
        previous
    }

    pub(super) fn leave_form(&mut self, previous: usize) {
        self.current_stream = previous;
    }

    /// The `/StructParents` key of a Form XObject, if anything in it was tagged.
    pub(super) fn form_key(&self, form: Ref) -> Option<i32> {
        self.streams
            .iter()
            .find(|s| s.stm == Some(form) && !s.owners.is_empty())
            .map(|s| s.key)
    }

    pub(super) fn suspend(&mut self) {
        self.suspended += 1;
    }

    pub(super) fn resume(&mut self) {
        self.suspended -= 1;
    }

    pub(super) fn is_active(&self) -> bool {
        self.suspended == 0
    }

    fn open(&mut self, role: StructRole, alt: Option<String>) -> usize {
        let parent = *self.stack.last().expect("the Document is always open");
        let index = self.elems.len();
        self.elems.push(Elem {
            role,
            alt,
            table: None,
            kids: Vec::new(),
            first_stream: self.page_stream,
            link: None,
        });
        self.parents.push(parent);
        self.elems[parent].kids.push(Kid::Elem(index));
        index
    }

    /// The element of `node`, created on first use. A box split over several pages reuses the
    /// element it started, so the fragments stay one element.
    fn node_elem(&mut self, node: NodeId) -> Option<usize> {
        let role = self.roles.get(&node)?;
        if matches!(role, TagRole::Figure { .. }) {
            return None;
        }
        if let Some(&index) = self.by_node.get(&node) {
            return Some(index);
        }
        let table = role.table_attributes();
        let index = self.open(role.struct_role(), None);
        self.elems[index].table = table;
        self.by_node.insert(node, index);
        Some(index)
    }

    /// Open the element for `node` if it has a role. Returns whether an element was pushed.
    pub(super) fn begin_node(&mut self, node: Option<NodeId>) -> bool {
        if !self.is_active() {
            return false;
        }
        let Some(index) = node.and_then(|n| self.node_elem(n)) else {
            return false;
        };
        self.stack.push(index);
        true
    }

    /// Open a table row. A `<thead>` row seen again on a later page is a repeat of the header
    /// and becomes an artifact. A row with no `<tr>` of its own gets a `TR` anyway, since a
    /// table's cells have to sit in rows.
    pub(super) fn begin_row(&mut self, node: Option<NodeId>, header: bool) -> RowTag {
        if !self.is_active() {
            return RowTag {
                pushed: false,
                repeated: false,
            };
        }
        let existing = node.and_then(|n| self.by_node.get(&n).copied());
        if header && existing.is_some_and(|i| self.elems[i].first_stream != self.page_stream) {
            return RowTag {
                pushed: false,
                repeated: true,
            };
        }
        let pushed = match node {
            Some(_) => self.begin_node(node),
            None => self.begin_synthetic(StructRole::TR),
        };
        RowTag {
            pushed,
            repeated: false,
        }
    }

    pub(super) fn is_list_item(&self, node: Option<NodeId>) -> bool {
        node.and_then(|n| self.roles.get(&n)) == Some(&TagRole::ListItem)
    }

    /// Open an element that has no HTML element of its own (`Lbl`, `LBody`, `Link`).
    pub(super) fn begin_synthetic(&mut self, role: StructRole) -> bool {
        if !self.is_active() {
            return false;
        }
        let index = self.open(role, None);
        self.stack.push(index);
        true
    }

    /// Open the `Link` element of a run group linking to `href`, whose text is in `bounds` of
    /// the page being drawn.
    pub(super) fn begin_link(&mut self, href: &str, bounds: Bounds) -> bool {
        if !self.begin_synthetic(StructRole::Link) {
            return false;
        }
        let index = *self.stack.last().expect("just pushed");
        self.elems[index].link = Some(bounds);
        self.links.entry(href.to_string()).or_default().push(index);
        true
    }

    pub(super) fn end(&mut self, pushed: bool) {
        if pushed {
            self.stack.pop();
        }
    }

    /// Allocate the next `MCID` of the current stream for `owner`.
    fn next_mcid(&mut self, owner: usize) -> i32 {
        let stream = &mut self.streams[self.current_stream];
        let mcid = stream.owners.len() as i32;
        stream.owners.push(owner);
        let (page, stm) = (stream.page, stream.stm);
        self.elems[owner]
            .kids
            .push(Kid::Content { page, stm, mcid });
        mcid
    }

    /// The `MCID` for content belonging to the innermost open element.
    pub(super) fn content_mcid(&mut self) -> Option<i32> {
        if !self.is_active() {
            return None;
        }
        let owner = *self.stack.last().expect("the Document is always open");
        Some(self.next_mcid(owner))
    }

    /// The `MCID` for an `<img>`: a `Figure` with its alternative text, or `None` for decoration.
    pub(super) fn figure_mcid(&mut self, node: Option<NodeId>) -> Option<i32> {
        if !self.is_active() {
            return None;
        }
        let Some(TagRole::Figure { alt }) = node.and_then(|n| self.roles.get(&n)) else {
            return None;
        };
        let alt = alt.clone();
        let index = self.open(StructRole::Figure, Some(alt));
        Some(self.next_mcid(index))
    }

    /// Put the link annotation `annotation` (of `href`, over `area`, on `page`) in the `Link`
    /// element whose text it covers, or in a `Link` of its own if there is none. Returns the
    /// annotation's `/StructParent` key.
    pub(super) fn attach_annotation(
        &mut self,
        page: Ref,
        href: &str,
        area: Bounds,
        annotation: Ref,
    ) -> i32 {
        let best = self
            .links
            .get(href)
            .into_iter()
            .flatten()
            .filter_map(|&index| {
                let bounds = self.elems[index].link.as_ref()?;
                let overlap = bounds.overlap_area(&area);
                (overlap > 0.0).then_some((index, overlap))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index);
        let owner = best.unwrap_or_else(|| {
            let saved = std::mem::replace(&mut self.stack, vec![0]);
            let index = self.open(StructRole::Link, None);
            self.stack = saved;
            index
        });
        self.elems[owner].kids.push(Kid::Object {
            page,
            obj: annotation,
        });
        let key = self.entries.len() as i32;
        self.entries.push((key, ParentEntry::Annotation(owner)));
        key
    }

    /// Write the structure tree. Returns the `StructTreeRoot` ref and the objects to write, or
    /// `None` if nothing was tagged. `alloc` hands out the object numbers.
    pub(super) fn write(&self, mut alloc: impl FnMut() -> Ref) -> Option<(Ref, Vec<(Ref, Chunk)>)> {
        let mut objects: Vec<(Ref, Chunk)> = Vec::new();
        // Children are created after their parents, so one reverse pass tells which elements
        // end up holding any content; the empty ones are left out.
        let mut alive = vec![false; self.elems.len()];
        for index in (0..self.elems.len()).rev() {
            alive[index] = self.elems[index].kids.iter().any(|kid| match kid {
                Kid::Content { .. } | Kid::Object { .. } => true,
                Kid::Elem(child) => alive[*child],
            });
        }
        if !alive[0] {
            return None;
        }

        let root_id = alloc();
        let ids: Vec<Ref> = self.elems.iter().map(|_| alloc()).collect();

        for (index, elem) in self.elems.iter().enumerate() {
            if !alive[index] {
                continue;
            }
            let parent = if index == 0 {
                root_id
            } else {
                ids[self.parents[index]]
            };
            let mut chunk = Chunk::new();
            {
                let mut e = chunk.struct_element(ids[index]);
                e.kind(elem.role);
                e.parent(parent);
                if let Some(alt) = &elem.alt {
                    e.alt(TextStr(alt));
                }
                if let Some(table) = &elem.table {
                    let mut attributes = e.attributes();
                    let mut cell = attributes.push().table();
                    if let Some(scope) = table.scope {
                        cell.scope(match scope {
                            CellScope::Row => TableHeaderScope::Row,
                            CellScope::Column => TableHeaderScope::Column,
                            CellScope::Both => TableHeaderScope::Both,
                        });
                    }
                    if table.col_span > 1 {
                        cell.col_span(table.col_span);
                    }
                    if table.row_span > 1 {
                        cell.row_span(table.row_span);
                    }
                }
                let mut kids = e.children();
                for kid in &elem.kids {
                    match kid {
                        Kid::Elem(child) if alive[*child] => {
                            kids.struct_element(ids[*child]);
                        }
                        Kid::Elem(_) => {}
                        Kid::Content { page, stm, mcid } => {
                            let mut mcr = kids.marked_content_ref();
                            mcr.page(*page);
                            if let Some(stm) = stm {
                                mcr.stream(*stm);
                            }
                            mcr.marked_content_id(*mcid);
                        }
                        Kid::Object { page, obj } => {
                            let mut objr = kids.object_ref();
                            objr.page(*page);
                            objr.object(*obj);
                        }
                    }
                }
            }
            objects.push((ids[index], chunk));
        }

        // The ParentTree maps each `/StructParents` key to the owner of each `MCID` (a stream)
        // or to the element holding the annotation.
        enum Value {
            Array(Ref),
            Elem(Ref),
        }
        let mut nums: Vec<(i32, Value)> = Vec::new();
        for (key, entry) in &self.entries {
            match entry {
                ParentEntry::Stream(stream) => {
                    let owners = &self.streams[*stream].owners;
                    if owners.is_empty() {
                        continue;
                    }
                    let array_id = alloc();
                    let mut chunk = Chunk::new();
                    chunk
                        .indirect(array_id)
                        .array()
                        .typed::<Ref>()
                        .items(owners.iter().map(|&owner| ids[owner]));
                    objects.push((array_id, chunk));
                    nums.push((*key, Value::Array(array_id)));
                }
                ParentEntry::Annotation(owner) => nums.push((*key, Value::Elem(ids[*owner]))),
            }
        }

        let mut chunk = Chunk::new();
        {
            let mut root = chunk
                .indirect(root_id)
                .start::<pdf_writer::writers::StructTreeRoot>();
            root.child(ids[0]);
            {
                let mut tree = root.parent_tree();
                let mut entries = tree.nums();
                for (key, value) in &nums {
                    match value {
                        Value::Array(id) | Value::Elem(id) => entries.insert(*key, *id),
                    };
                }
            }
            root.parent_tree_next_key(self.entries.len() as i32);
        }
        objects.push((root_id, chunk));
        Some((root_id, objects))
    }
}

/// The tag of a marked-content sequence for text and other tagged content.
pub(super) const CONTENT_TAG: Name<'static> = Name(b"Span");

/// Remove the artifact sequences that enclose nothing (a box without a background or border
/// still opens its decoration sequence), so they do not bloat the content stream.
pub(super) fn strip_empty_artifacts(content: Vec<u8>) -> Vec<u8> {
    // The last operator of a stream has no newline after it.
    const EMPTY: &[u8] = b"/Artifact BMC\nEMC";
    let mut content = content;
    // An empty pair inside another leaves the outer one empty in turn.
    loop {
        let mut stripped = Vec::with_capacity(content.len());
        let mut rest = content.as_slice();
        while let Some(at) = rest.windows(EMPTY.len()).position(|w| w == EMPTY) {
            stripped.extend_from_slice(&rest[..at]);
            rest = &rest[at + EMPTY.len()..];
            rest = rest.strip_prefix(b"\n").unwrap_or(rest);
        }
        stripped.extend_from_slice(rest);
        if stripped.len() == content.len() {
            return stripped;
        }
        content = stripped;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_artifact_sequences_are_removed_and_filled_ones_kept() {
        let content = b"q\n/Artifact BMC\n/Artifact BMC\nEMC\nEMC\n/Artifact BMC\n0 0 m\nEMC\nQ\n";
        assert_eq!(
            strip_empty_artifacts(content.to_vec()),
            b"q\n/Artifact BMC\n0 0 m\nEMC\nQ\n".to_vec()
        );
    }
}
