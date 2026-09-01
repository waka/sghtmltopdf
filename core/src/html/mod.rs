//! HTML parsing and DOM construction (html5ever).

mod dom;
mod parse;

mod encoding;

pub use dom::{
    collect_anchor_targets, find_base_href, find_document_title, is_stylesheet_link, Children, Dom,
    Node, NodeData, NodeId,
};

/// Maximum DOM depth accepted. Deeper input is rejected with an error.
///
/// Measured stack use per level is about 4.6KiB in an optimised build and about 11KiB
/// in a debug build (limits of roughly depth 450 and 195 on a 2MiB stack). This cap
/// works out to about 2.8MiB in debug-build terms, so the rendering thread needs a
/// stack of roughly [`STACK_SIZE`](crate::cli::STACK_SIZE). The CLI, the HTTP server
/// and the Ruby extension all run on a thread whose stack they allocate themselves.
pub const MAX_ELEMENT_DEPTH: u32 = 256;

/// Maximum number of nodes held at once. Larger input is rejected with an error.
///
/// Not just the DOM: computed styles, the box tree, layout results and pages all grow
/// in proportion to the node count. Measurements (optimised build) came out at 472B per
/// node (tables) to 1210B (a run of inline elements), and stayed roughly linear in node
/// count regardless of shape. 500,000 nodes therefore stay under about 600MiB.
///
/// Memory proportional to the amount of text is not bounded by this cap (three nodes
/// holding 10MiB of text still use 1.7GiB). That is what the HTTP server's
/// `--max-body-size` is for.
pub const MAX_NODES: usize = 500_000;
pub use encoding::{decode_html, StreamingDecoder};
pub use parse::{parse, StreamingParser};
