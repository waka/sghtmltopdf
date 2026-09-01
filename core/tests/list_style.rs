//! `list-style-type`/`list-style-position`/`list-style-image`/`list-style`
//! E2E tests for the shorthand.
//!
//! The same approach as `typography.rs`/`table_caption.rs`: catch regressions by going
//! through the real pipeline (HTML parse, style cascade, pagination, PDF encode).
//! The detailed checks on marker coordinates and counter behaviour run against the result of
//! `layout_document` (before pagination), and `build_pdf` separately confirms that the whole
//! pipeline through to PDF encoding does not crash and produces valid output.

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, Page,
    PageSettings,
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

fn page_count_in_pdf(bytes: &[u8]) -> usize {
    count_occurrences(bytes, b"/MediaBox")
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
    assert_eq!(
        page_count_in_pdf(&bytes),
        engine_page_count,
        "PDF page count should match the layout engine's own page count"
    );

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

fn find_all_tags(dom: &Dom, id: NodeId, tag: &str, out: &mut Vec<NodeId>) {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            out.push(id);
        }
    }
    for child in dom.children(id) {
        find_all_tags(dom, child, tag, out);
    }
}

fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    if let LaidOutContent::Blocks(children) = &b.content {
        for child in children {
            if let Some(found) = find_laid_out(child, target) {
                return Some(found);
            }
        }
    }
    None
}

/// The shared helper running as far as `layout_document` (before pagination).
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
fn unordered_list_renders_a_valid_pdf_with_disc_markers() {
    let html_src = r#"<ul><li>one</li><li>two</li></ul>"#;
    let (page_count, bytes) = build_pdf(html_src, "body { margin: 0; }");
    assert_eq!(page_count, 1);
    assert!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0,
        "the font should be embedded to render the list text and markers"
    );
}

#[test]
fn ordered_list_numbers_items_in_document_order() {
    let (dom, laid) = layout(
        r#"<ol><li>a</li><li>b</li><li>c</li></ol>"#,
        "body { margin: 0; }",
    );
    let mut lis = Vec::new();
    find_all_tags(&dom, dom.document(), "li", &mut lis);
    assert_eq!(lis.len(), 3);

    let expected = ["1.", "2.", "3."];
    for (li, expected_marker) in lis.iter().zip(expected) {
        let li_box = find_laid_out(&laid, *li).expect("li box not found");
        assert_eq!(
            li_box.marker.as_ref().map(|m| m.runs[0].text.as_str()),
            Some(expected_marker)
        );
    }
}

#[test]
fn ol_start_attribute_offsets_the_numbering_end_to_end() {
    let (dom, laid) = layout(
        r#"<ol start="10"><li>a</li><li>b</li></ol>"#,
        "body { margin: 0; }",
    );
    let mut lis = Vec::new();
    find_all_tags(&dom, dom.document(), "li", &mut lis);

    let first = find_laid_out(&laid, lis[0]).expect("li box not found");
    assert_eq!(first.marker.as_ref().unwrap().runs[0].text, "10.");
    let second = find_laid_out(&laid, lis[1]).expect("li box not found");
    assert_eq!(second.marker.as_ref().unwrap().runs[0].text, "11.");
}

#[test]
fn nested_ordered_list_restarts_numbering_and_indents_further_than_its_parent() {
    let (dom, laid) = layout(
        r#"<ol><li>outer</li><li><ol><li>inner</li></ol></li></ol>"#,
        "body { margin: 0; }",
    );
    let mut lis = Vec::new();
    find_all_tags(&dom, dom.document(), "li", &mut lis);
    assert_eq!(lis.len(), 3, "2 top-level li + 1 nested li");

    let outer_first = find_laid_out(&laid, lis[0]).expect("outer li not found");
    let outer_second = find_laid_out(&laid, lis[1]).expect("outer li (with nested ol) not found");
    let inner = find_laid_out(&laid, lis[2]).expect("inner li not found");

    assert_eq!(outer_first.marker.as_ref().unwrap().runs[0].text, "1.");
    assert_eq!(outer_second.marker.as_ref().unwrap().runs[0].text, "2.");
    // A nested `<ol>` has its own counter scope and counts from 1 again.
    assert_eq!(inner.marker.as_ref().unwrap().runs[0].text, "1.");

    // Through the nested `<ol>`'s own `padding-left: 40px` (from the UA stylesheet), the
    // inner marker's content edge should sit further right than the outer marker's.
    let outer_marker = outer_first.marker.as_ref().unwrap();
    let inner_marker = inner.marker.as_ref().unwrap();
    assert!(
        inner_marker.rect.x > outer_marker.rect.x,
        "nested list marker (x={}) should sit further right than the outer one (x={})",
        inner_marker.rect.x,
        outer_marker.rect.x
    );
}

#[test]
fn list_style_type_none_still_advances_the_counter_but_has_no_visible_marker() {
    let (dom, laid) = layout(
        r#"<ol><li style="list-style-type: none;">a</li><li>b</li></ol>"#,
        "body { margin: 0; }",
    );
    let mut lis = Vec::new();
    find_all_tags(&dom, dom.document(), "li", &mut lis);

    let first = find_laid_out(&laid, lis[0]).expect("li box not found");
    assert!(first.marker.is_none());
    let second = find_laid_out(&laid, lis[1]).expect("li box not found");
    assert_eq!(second.marker.as_ref().unwrap().runs[0].text, "2.");
}

#[test]
fn list_style_position_inside_wraps_the_marker_with_the_text_instead_of_a_gutter_box() {
    let (dom, laid) = layout(
        r#"<ul style="list-style-position: inside;"><li>hello</li></ul>"#,
        "body { margin: 0; }",
    );
    let li = find_tag(&dom, dom.document(), "li").expect("li not found");
    let li_box = find_laid_out(&laid, li).expect("li box not found");

    // `inside` weaves the marker into the first line as part of the text, so it has no
    // separate marker box.
    assert!(li_box.marker.is_none());
    let LaidOutContent::Inline(lines) = &li_box.content else {
        panic!("expected inline content");
    };
    // Inter-word whitespace is not literally part of any run's `text` (it is expressed only
    // as a positional gap; an existing simplification), so the run sequence itself is checked.
    let run_texts: Vec<&str> = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(run_texts, vec!["•", "hello"]);
}

#[test]
fn various_list_style_types_render_a_valid_pdf_end_to_end() {
    let html_src = r#"
        <ul><li style="list-style-type: circle;">a</li></ul>
        <ul><li style="list-style-type: square;">b</li></ul>
        <ol><li style="list-style-type: decimal-leading-zero;">c</li></ol>
        <ol><li style="list-style-type: lower-roman;">d</li></ol>
        <ol><li style="list-style-type: upper-roman;">e</li></ol>
        <ol><li style="list-style-type: lower-alpha;">f</li></ol>
        <ol><li style="list-style-type: upper-alpha;">g</li></ol>
    "#;
    let (page_count, _bytes) = build_pdf(html_src, "body { margin: 0; }");
    assert_eq!(page_count, 1);
}

#[test]
fn list_style_shorthand_applies_type_position_and_falls_back_from_image() {
    let (dom, laid) = layout(
        r#"<ul style="list-style: square inside url(does-not-exist.png);"><li>x</li></ul>"#,
        "body { margin: 0; }",
    );
    let li = find_tag(&dom, dom.document(), "li").expect("li not found");
    let li_box = find_laid_out(&laid, li).expect("li box not found");

    // `list-style-image` always falls back to the `list-style-type` text marker. Being
    // `inside`, it is embedded in the spans.
    assert!(li_box.marker.is_none());
    let LaidOutContent::Inline(lines) = &li_box.content else {
        panic!("expected inline content");
    };
    let run_texts: Vec<&str> = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(run_texts, vec!["▪", "x"]);
}

/// The shared helper running as far as pagination.
fn paginate(html_src: &str, css: &str) -> Vec<Page> {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    paginate_document(&dom, &styles, &fonts, &PageSettings::default())
}

/// Walk the boxes placed in `pages` recursively and collect the outside markers' text and
/// their within-page Y coordinates, along with the page number (0-based).
fn collect_markers(pages: &[Page]) -> Vec<(usize, String, f32)> {
    fn walk(b: &LaidOutBox, page_index: usize, out: &mut Vec<(usize, String, f32)>) {
        if let Some(marker) = &b.marker {
            let text: String = marker.runs.iter().map(|r| r.text.as_str()).collect();
            out.push((page_index, text, marker.rect.y));
        }
        match &b.content {
            LaidOutContent::Blocks(children) => {
                for child in children {
                    walk(child, page_index, out);
                }
            }
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    for atomic in &line.atomics {
                        walk(&atomic.content, page_index, out);
                    }
                }
            }
            _ => {}
        }
    }

    let mut out = Vec::new();
    for (page_index, page) in pages.iter().enumerate() {
        for b in &page.boxes {
            walk(b, page_index, &mut out);
        }
    }
    out
}

/// Paginate, with `css`, a list of 24 `li`s too long to fit one page, and confirm that
/// "every `li` keeps exactly one marker, in order" and that "each marker stays within the
/// page it sits on".
fn assert_split_list_keeps_every_marker(css: &str) {
    let words = "Word ".repeat(80);
    let items: String = (0..24).map(|_| format!("<li>{words}</li>")).collect();
    let pages = paginate(&format!("<ol>{items}</ol>"), css);
    assert!(
        pages.len() > 1,
        "the fixture should span multiple pages, got {}",
        pages.len()
    );

    let markers = collect_markers(&pages);
    let texts: Vec<&str> = markers.iter().map(|(_, text, _)| text.as_str()).collect();
    let expected: Vec<String> = (1..=24).map(|n| format!("{n}.")).collect();
    assert_eq!(
        texts,
        expected.iter().map(String::as_str).collect::<Vec<_>>(),
        "every list item should keep its marker exactly once, in order"
    );

    // A split `li`'s marker must be on the page where that `li` begins
    // (its coordinates must not have shifted to another page's after the split).
    let content_height = PageSettings::default().content_height();
    for (page_index, text, y) in &markers {
        assert!(
            *y >= 0.0 && *y <= content_height,
            "marker {text} on page {page_index} sits outside the page (y={y})"
        );
    }

    // At least one marker should be on the second page or later (with them all on the first
    // page, the splitting path was never taken).
    assert!(
        markers.iter().any(|(page_index, _, _)| *page_index > 0),
        "the fixture should place some markers on later pages"
    );
}

#[test]
fn outside_marker_survives_when_a_list_item_is_split_across_pages() {
    // An `li` split across pages goes through the `place_split` path. This confirms the
    // marker stays on the first fragment even for an `li` with no decoration (background or
    // borders).
    assert_split_list_keeps_every_marker("body { margin: 0; } ol, li { margin: 0; }");
}

#[test]
fn outside_marker_of_a_split_list_item_with_a_background_stays_on_its_own_page() {
    // An `li` with decoration gets a decoration fragment on the split. If the marker's
    // coordinates stayed the absolute Y from layout, they would shift to another page's position.
    assert_split_list_keeps_every_marker(
        "body { margin: 0; } ol, li { margin: 0; } li { background: #eee; }",
    );
}
