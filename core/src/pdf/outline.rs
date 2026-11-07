//! The document outline (bookmarks) built from `bookmark-level`, `bookmark-label` and
//! `bookmark-state`.
//!
//! The engine resolves the label of each element that has a level into a [`Bookmark`]
//! (`bookmarks` of [`super::LinkSettings`]). When a page is written, the boxes on it are
//! walked and every bookmarked element found gets an [`OutlineEntry`] with its page and
//! position. The outline objects are written last, when every entry is known, so the same
//! code serves the batch and the streaming writer.

use pdf_writer::{Chunk, Ref, TextStr};

use super::document::{to_pdf_y, RefAllocator};
use crate::html::NodeId;
use crate::layout::{LaidOutBox, LaidOutContent, PageSettings};

use std::collections::HashMap;

/// The outline entry of one element, as resolved from its style.
#[derive(Debug, Clone, PartialEq)]
pub struct Bookmark {
    /// `bookmark-level`, 1 or more.
    pub level: u32,
    /// The label, whitespace-collapsed.
    pub title: String,
    /// Whether the entry is shown expanded (`bookmark-state: open`).
    pub open: bool,
    /// The element's place among the bookmarked elements in document order. The pages list
    /// them in box order, where a positioned box can come last, so the outline sorts by this.
    pub order: usize,
}

/// A bookmark placed on a page.
#[derive(Debug, Clone)]
pub(super) struct OutlineEntry {
    pub bookmark: Bookmark,
    pub page: Ref,
    /// The top left of the element's border box, in pt.
    pub x: f32,
    pub y: f32,
}

/// Walk the boxes on a page and collect where each bookmarked element (a `NodeId` present in
/// `bookmarks`) appears, in box order (the PDF coordinates of the top left of its
/// border box, in px). An element split across pages appears on each of them; the caller
/// keeps the first.
pub(super) fn collect_bookmark_positions(
    b: &LaidOutBox,
    bookmarks: &HashMap<NodeId, Bookmark>,
    settings: &PageSettings,
    out: &mut Vec<(NodeId, f32, f32)>,
) {
    if let Some(node) = b.node.filter(|n| bookmarks.contains_key(n)) {
        let border_box = b.layout.border_box();
        out.push((
            node,
            settings.margin.left + border_box.x,
            to_pdf_y(settings, border_box.y),
        ));
    }

    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_bookmark_positions(child, bookmarks, settings, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                collect_bookmark_positions(child, bookmarks, settings, out);
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                collect_bookmark_positions(caption, bookmarks, settings, out);
            }
            for row in &table.rows {
                // A row (and its row group, at its first row) has no box of its own: it
                // starts where its cells do.
                if let Some(first) = row.cells.first() {
                    let x = settings.margin.left + first.layout.border_box().x;
                    let top = row
                        .cells
                        .iter()
                        .map(|cell| cell.layout.border_box().y)
                        .fold(f32::INFINITY, f32::min);
                    for node in [row.group, row.node].into_iter().flatten() {
                        if bookmarks.contains_key(&node) {
                            out.push((node, x, to_pdf_y(settings, top)));
                        }
                    }
                }
                for cell in &row.cells {
                    collect_bookmark_positions(cell, bookmarks, settings, out);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines {
                for atomic in &line.atomics {
                    collect_bookmark_positions(&atomic.content, bookmarks, settings, out);
                }
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

/// Write the outline objects for `entries` (sorted into document order first) and return the
/// outline root together with every object, ready to be written. `None` when there are no
/// entries.
///
/// An entry nests under the nearest preceding entry with a smaller level, so a level that
/// skips ahead (a level 3 right after a level 1) simply nests one step down, and one that
/// returns to a shallower level than the entry before it closes the deeper ones.
pub(super) fn write_outline(
    mut entries: Vec<OutlineEntry>,
    alloc: &mut RefAllocator,
) -> Option<(Ref, Vec<(Ref, Chunk)>)> {
    if entries.is_empty() {
        return None;
    }
    entries.sort_by_key(|entry| entry.bookmark.order);
    let root = alloc.next();
    let ids: Vec<Ref> = entries.iter().map(|_| alloc.next()).collect();

    // `parent[i]` is the index of the entry `i` nests under; `children[i]` lists its children.
    // The extra last slot of `children` holds the top-level entries.
    let top = entries.len();
    let mut parent: Vec<usize> = Vec::with_capacity(entries.len());
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); entries.len() + 1];
    let mut open_chain: Vec<usize> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        while open_chain
            .last()
            .is_some_and(|&p| entries[p].bookmark.level >= entry.bookmark.level)
        {
            open_chain.pop();
        }
        let p = open_chain.last().copied().unwrap_or(top);
        parent.push(p);
        children[p].push(i);
        open_chain.push(i);
    }

    // The number of entries a viewer shows below each entry: its children, plus the
    // children of those that are open. A child always comes after its parent, so one pass
    // from the end sees every child before its parent, and the top level comes last.
    let mut visible = vec![0i32; entries.len() + 1];
    for i in (0..top).rev().chain([top]) {
        visible[i] = children[i]
            .iter()
            .map(|&c| {
                1 + if entries[c].bookmark.open {
                    visible[c]
                } else {
                    0
                }
            })
            .sum();
    }

    let mut chunks = Vec::with_capacity(entries.len() + 1);
    let mut chunk = Chunk::new();
    {
        let mut outline = chunk.outline(root);
        outline.first(ids[children[top][0]]);
        outline.last(ids[*children[top].last().expect("a top-level entry")]);
        outline.count(visible[top]);
    }
    chunks.push((root, chunk));

    for (i, entry) in entries.iter().enumerate() {
        let siblings = &children[parent[i]];
        let at = siblings.iter().position(|&s| s == i).expect("a sibling");
        let mut chunk = Chunk::new();
        {
            let mut item = chunk.outline_item(ids[i]);
            item.title(TextStr(&entry.bookmark.title));
            item.parent(if parent[i] == top {
                root
            } else {
                ids[parent[i]]
            });
            if let Some(&prev) = at.checked_sub(1).and_then(|p| siblings.get(p)) {
                item.prev(ids[prev]);
            }
            if let Some(&next) = siblings.get(at + 1) {
                item.next(ids[next]);
            }
            if let (Some(&first), Some(&last)) = (children[i].first(), children[i].last()) {
                item.first(ids[first]);
                item.last(ids[last]);
                // Negative when closed: the number of entries that opening it would show.
                item.count(if entry.bookmark.open {
                    visible[i]
                } else {
                    -visible[i]
                });
            }
            item.dest().page(entry.page).xyz(entry.x, entry.y, None);
        }
        chunks.push((ids[i], chunk));
    }
    Some((root, chunks))
}
