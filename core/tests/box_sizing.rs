//! E2E tests for `box-sizing`.
//!
//! The same approach as `box_model.rs`/`typography.rs`: catch regressions by going through
//! the real pipeline (HTML parse, style cascade, pagination, PDF encode).

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

fn build_pdf(html_src: &str, css: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
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
fn border_box_and_content_box_siblings_end_up_with_the_same_border_box_width() {
    let (dom, laid) = layout(
        r#"<div class="content">content-box</div><div class="border">border-box</div>"#,
        "body { margin: 0; } \
         div { width: 100px; margin: 0; padding: 10px; border: 5px solid black; } \
         .border { box-sizing: border-box; }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let content_box = find_laid_out(&laid, divs[0]).unwrap();
    let border_box = find_laid_out(&laid, divs[1]).unwrap();

    // On a content-box div the given 100px is the content width, so the border box swells to
    // 100+2*10+2*5=130px. On a border-box div the 100px is the border box itself, so the
    // content width shrinks to 100-2*10-2*5=70px.
    assert_eq!(content_box.layout.content.width, 100.0);
    assert_eq!(content_box.layout.border_box().width, 130.0);

    assert_eq!(border_box.layout.content.width, 70.0);
    assert_eq!(border_box.layout.border_box().width, 100.0);
}

#[test]
fn box_sizing_border_box_is_not_inherited_by_children() {
    let (dom, laid) = layout(
        r#"<div class="outer"><div class="inner"></div></div>"#,
        "body { margin: 0; } \
         .outer { box-sizing: border-box; width: 200px; margin: 0; padding: 10px; border: 5px solid black; } \
         .inner { width: 100px; margin: 0; padding: 10px; border: 5px solid black; }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let outer = find_laid_out(&laid, divs[0]).unwrap();
    let inner = find_laid_out(&laid, divs[1]).unwrap();

    // outer(border-box): content = 200 - 2*10 - 2*5 = 170
    assert_eq!(outer.layout.content.width, 170.0);
    // inner (no box-sizing set, so content-box; it is not inherited from the parent): the
    // given 100px is the content width outright.
    assert_eq!(inner.layout.content.width, 100.0);
}

#[test]
fn box_sizing_border_box_combined_with_percentage_width_renders_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">box-sizing test</div>"#,
        "body { margin: 0; } \
         .box { box-sizing: border-box; width: 50%; padding: 20px; border: 3px solid black; }",
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
