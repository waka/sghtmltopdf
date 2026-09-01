//! E2E tests for `float`/`clear`/`position:relative`.
//!
//! The same approach as `fragmentation.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode). The detailed coordinate
//! checks (text flow-around, placing several floats, crossing a page break) run against the
//! result of `layout_document` (before pagination), and `build_pdf` separately confirms that
//! the whole pipeline through to PDF encoding does not crash and produces valid output.

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

fn page_count_in_pdf(bytes: &[u8]) -> usize {
    count_occurrences(bytes, b"/MediaBox")
}

/// Run the whole real pipeline (parse, cascade, pagination, PDF encode) from HTML plus CSS
/// (the same as `fragmentation.rs::build_pdf`).
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

fn box_contains_node(b: &LaidOutBox, target: NodeId) -> bool {
    if b.node == Some(target) {
        return true;
    }
    if let LaidOutContent::Blocks(children) = &b.content {
        return children
            .iter()
            .any(|child| box_contains_node(child, target));
    }
    false
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
fn left_float_with_text_wrap_narrows_the_first_line_and_renders_a_valid_pdf() {
    let html_src = r#"<div><div class="f"></div>
        <p class="text">hello world foo bar baz qux quux corge grault garply</p></div>"#;
    let css = "body { margin: 0; } \
               .f { float: left; width: 100px; height: 15px; } \
               .text { margin: 0; width: 300px; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };

    assert!(
        lines.len() >= 2,
        "narrowing the first line beside the float should force wrapping"
    );
    assert_eq!(
        lines[0].rect.x, 100.0,
        "first line should be pushed to the right of the 100px-wide float"
    );
    // A line past the float's height (15px, below the 19.2px line height) should return to
    // the original left edge (the p's content.x).
    let below_float_line = lines
        .iter()
        .find(|l| l.rect.y >= 15.0)
        .expect("expected at least one line below the float");
    assert_eq!(below_float_line.rect.x, p_box.layout.content.x);

    // The p's box itself (being block-level) does not flow around the float
    // (CSS2.1: a float affects only inline content).
    assert_eq!(p_box.layout.content.x, 0.0);

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

#[test]
fn right_float_with_text_wrap_narrows_the_first_line() {
    // Put the float and the text on the same containing width (the parent div's width:300px).
    // Setting a width only on `.text` would place the float against the parent div's
    // content_width (the whole page width), putting it outside `.text`'s narrower width
    // (a right float's inner edge depends on the containing width where it is placed, so
    // unlike a left float's fixed 0 this asymmetry arises).
    let html_src = r#"<div class="outer"><div class="f"></div>
        <p class="text">hello world foo bar baz qux quux corge grault garply</p></div>"#;
    let css = "body { margin: 0; } \
               .outer { width: 300px; } \
               .f { float: right; width: 250px; height: 15px; } \
               .text { margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };

    assert!(lines.len() >= 2);
    assert_eq!(lines[0].rect.x, 0.0, "first line starts at the left edge");
    assert!(
        lines[0].rect.width <= 50.0 + 0.01,
        "first line should be narrowed to fit beside the 250px right float \
         (containing width 300 - 250 = 50), got {}",
        lines[0].rect.width
    );
}

#[test]
fn two_left_floats_pack_side_by_side_and_text_flows_around_both() {
    let html_src = r#"<div><div class="a"></div><div class="b"></div>
        <p class="text">hello world foo bar baz qux quux</p></div>"#;
    let css = "body { margin: 0; } \
               .a { float: left; width: 100px; height: 30px; } \
               .b { float: left; width: 80px; height: 30px; } \
               .text { margin: 0; width: 300px; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");

    let a = find_tag(&dom, dom.document(), "div").expect("div not found");
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let a_box = find_laid_out(&laid, divs[1]).expect("a not found");
    let b_box = find_laid_out(&laid, divs[2]).expect("b not found");
    let _ = a;

    // The two left floats should sit side by side (a: 0-100, b: 100-180).
    assert_eq!(a_box.layout.content.x, 0.0);
    assert_eq!(b_box.layout.content.x, 100.0);

    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert_eq!(
        lines[0].rect.x, 180.0,
        "first line should start after both floats (0 + 100 + 80)"
    );
}

#[test]
fn clear_pushes_content_below_the_float_end_to_end() {
    let html_src = r#"<div><div class="f"></div><p class="c">after</p></div>"#;
    let css = "body { margin: 0; } \
               .f { float: left; width: 100px; height: 50px; } \
               .c { clear: left; height: 20px; margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");

    assert_eq!(p_box.layout.content.y, 50.0);

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

#[test]
fn float_taller_than_a_page_spans_multiple_pages_end_to_end() {
    let mut inner = String::new();
    for i in 0..20 {
        inner.push_str(&format!(r#"<p class="item">item {i}</p>"#));
    }
    let html_src = format!(r#"<div><div class="f">{inner}</div></div>"#);
    let css = "body { margin: 0; } \
               .f { float: left; width: 100px; } \
               .item { height: 100px; margin: 0; }";

    let (page_count, bytes) = build_pdf(&html_src, css);
    assert!(
        page_count > 1,
        "a float containing 20 items of 100px should overflow a single page \
         (a float is allowed to cross a page boundary)"
    );
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn position_relative_offset_does_not_shift_subsequent_siblings_end_to_end() {
    let html_src =
        r#"<div><div class="a">a</div><div class="rel">b</div><div class="c">c</div></div>"#;
    let css = "body { margin: 0; } \
               .a { height: 10px; margin: 0; } \
               .rel { position: relative; top: 5px; left: 7px; height: 20px; margin: 0; } \
               .c { height: 10px; margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let rel_box = find_laid_out(&laid, divs[2]).expect("rel not found");
    let c_box = find_laid_out(&laid, divs[3]).expect("c not found");

    assert_eq!(rel_box.layout.content.x, 7.0);
    assert_eq!(rel_box.layout.content.y, 15.0);
    // c is placed against the rel element's ordinary (pre-offset) bottom edge (10+20=30).
    assert_eq!(c_box.layout.content.y, 30.0);

    let (page_count, bytes) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn float_none_regression_keeps_ordinary_block_flow_unaffected() {
    let html_src = r#"<div><p class="a">A</p><p class="b">B</p></div>"#;
    let css = ".a, .b { height: 50px; margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let mut ps = Vec::new();
    find_all_tags(&dom, dom.document(), "p", &mut ps);
    let a = find_laid_out(&laid, ps[0]).expect("a not found");
    let b = find_laid_out(&laid, ps[1]).expect("b not found");

    assert!(!a.is_float);
    assert!(!b.is_float);
    assert_eq!(
        b.layout.content.y,
        a.layout.content.y + a.layout.content.height
    );

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

#[test]
fn float_content_is_reachable_on_the_page_it_spans() {
    // Reachability after pagination (`box_contains_node`) rather than wrapping is checked the
    // same way as in `fragmentation.rs`.
    let html_src = r#"<div><div class="f"><p class="item">inside float</p></div></div>"#;
    let css = "body { margin: 0; } .f { float: left; width: 100px; height: 30px; }";

    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    let item = find_tag(&dom, dom.document(), "p").expect("p not found");
    let found = pages
        .iter()
        .any(|page| page.boxes.iter().any(|b| box_contains_node(b, item)));
    assert!(found, "content inside a float should still be reachable");
}

#[test]
fn a_float_without_width_shrinks_to_its_content() {
    // A `width: auto` float shrinks to its content (the short text). It does not stretch to
    // the containing width.
    let (dom, laid) = layout(
        r#"<div class="f">hi</div><p>body text that wraps beside the float</p>"#,
        "body { margin: 0; } .f { float: left; }",
    );
    let mut floats = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut floats);
    let float_box = find_laid_out(&laid, floats[0]).expect("float box");

    let content_width = PageSettings::default().content_width();
    assert!(
        float_box.layout.content.width < content_width * 0.5,
        "an auto-width float must shrink to its content, got {} of {}",
        float_box.layout.content.width,
        content_width
    );
    assert!(float_box.layout.content.width > 0.0);
}

#[test]
fn a_wider_content_float_is_wider_than_a_narrow_one() {
    let narrow = {
        let (dom, laid) = layout(
            r#"<div class="f">x</div>"#,
            "body { margin: 0; } .f { float: left; }",
        );
        let mut ds = Vec::new();
        find_all_tags(&dom, dom.document(), "div", &mut ds);
        find_laid_out(&laid, ds[0]).unwrap().layout.content.width
    };
    let wide = {
        let (dom, laid) = layout(
            r#"<div class="f">a much longer caption here</div>"#,
            "body { margin: 0; } .f { float: left; }",
        );
        let mut ds = Vec::new();
        find_all_tags(&dom, dom.document(), "div", &mut ds);
        find_laid_out(&laid, ds[0]).unwrap().layout.content.width
    };
    assert!(wide > narrow, "wide={wide} narrow={narrow}");
}

#[test]
fn an_auto_width_float_is_clamped_to_the_available_width() {
    // Content exceeding the available width is clamped (it does not overflow).
    let (dom, laid) = layout(
        r#"<div class="f">wordwordwordwordwordwordwordwordwordwordwordwordword</div>"#,
        "body { margin: 0; } .f { float: left; }",
    );
    let mut ds = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut ds);
    let width = find_laid_out(&laid, ds[0]).unwrap().layout.content.width;
    assert!(width <= PageSettings::default().content_width() + 0.5);
}
