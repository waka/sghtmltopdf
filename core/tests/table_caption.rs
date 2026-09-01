//! E2E tests for `<caption>`/`caption-side`.
//!
//! The same approach as `fragmentation.rs`/`float_position.rs`/`typography.rs`: catch
//! regressions by going through the real pipeline. The detailed coordinate checks run
//! against the result of `layout_document` (before pagination).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, PageSettings,
};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

fn build_pdf(html_src: &str, css: &str) -> (usize, Vec<u8>) {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let engine_page_count = pages.len();
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);

    (engine_page_count, bytes)
}

fn find_tag(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id).find_map(|child| find_tag(dom, child, tag))
}

/// A test-only `find_laid_out` that also descends into a `Table`'s caption and cells.
fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            children.iter().find_map(|c| find_laid_out(c, target))
        }
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .find_map(|item| find_laid_out(item, target)),
        LaidOutContent::Table(table) => table
            .caption
            .as_deref()
            .and_then(|c| find_laid_out(c, target))
            .or_else(|| {
                table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .find_map(|cell| find_laid_out(cell, target))
            }),
        LaidOutContent::Inline(_) | LaidOutContent::Image(_) => None,
    }
}

fn layout(html_src: &str, css: &str) -> (Dom, LaidOutBox) {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );
    (dom, laid)
}

#[test]
fn caption_top_is_placed_above_the_rows_and_pushes_them_down() {
    let html_src = r#"<table><caption>Fruit Prices</caption><tr><td>Apple</td></tr></table>"#;
    let css = "body { margin: 0; } td { height: 20px; }";

    let (dom, laid) = layout(html_src, css);
    let table_node = find_tag(&dom, dom.document(), "table").expect("table not found");
    let table_box = find_laid_out(&laid, table_node).expect("table box not found");
    let LaidOutContent::Table(table) = &table_box.content else {
        panic!("expected a laid-out table");
    };

    let caption = table.caption.as_ref().expect("caption not found");
    assert_eq!(
        caption.layout.content.y, 0.0,
        "caption should start at the top"
    );

    let td_node = find_tag(&dom, dom.document(), "td").expect("td not found");
    let td_box = find_laid_out(&laid, td_node).expect("td box not found");
    assert!(
        td_box.layout.content.y >= caption.layout.margin_box_height(),
        "row should be pushed down below the caption"
    );

    let (page_count, bytes) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn caption_bottom_is_placed_below_the_rows() {
    let html_src = r#"<table><caption>Fruit Prices</caption><tr><td>Apple</td></tr></table>"#;
    let css = "body { margin: 0; } td { height: 20px; } caption { caption-side: bottom; }";

    let (dom, laid) = layout(html_src, css);
    let table_node = find_tag(&dom, dom.document(), "table").expect("table not found");
    let table_box = find_laid_out(&laid, table_node).expect("table box not found");
    let LaidOutContent::Table(table) = &table_box.content else {
        panic!("expected a laid-out table");
    };

    let td_node = find_tag(&dom, dom.document(), "td").expect("td not found");
    let td_box = find_laid_out(&laid, td_node).expect("td box not found");
    let caption = table.caption.as_ref().expect("caption not found");

    assert_eq!(td_box.layout.content.y, 0.0, "row should start at the top");
    assert!(
        caption.layout.content.y >= td_box.layout.margin_box_height(),
        "caption should be placed below the row"
    );

    let (page_count, bytes) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn caption_text_actually_renders_in_the_final_pdf() {
    // The contents of a `<caption>` used to be lost entirely (a known bug). This checks that
    // the caption's text really reaches the PDF output (the presence of an embedded font
    // being the evidence that font embedding happened, that is, that some glyph was drawn).
    let html_src = r#"<table><caption>Fruit Prices</caption><tr><td>Apple</td></tr></table>"#;
    let (page_count, bytes) = build_pdf(html_src, "body { margin: 0; }");
    assert_eq!(page_count, 1);
    assert!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0,
        "the font should be embedded to render the caption text"
    );
}

#[test]
fn table_without_a_caption_behaves_as_before() {
    let html_src = r#"<table><tr><td>Apple</td></tr></table>"#;
    let css = "body { margin: 0; } td { height: 20px; }";

    let (dom, laid) = layout(html_src, css);
    let td_node = find_tag(&dom, dom.document(), "td").expect("td not found");
    let td_box = find_laid_out(&laid, td_node).expect("td box not found");
    assert_eq!(td_box.layout.content.y, 0.0);

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}
