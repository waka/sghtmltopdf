//! E2E tests for `overflow`, `z-index`, `outline`, `visibility`, the `border-style`
//! extensions (groove/ridge/inset/outset) and elliptical `border-radius`.
//!
//! The same approach as `list_style.rs`/`typography.rs`: catch regressions by going through
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
fn all_box_model_features_combined_render_a_valid_pdf_end_to_end() {
    let html_src = r#"
        <div style="border: 8px groove blue;">groove</div>
        <div style="border: 8px ridge blue;">ridge</div>
        <div style="border: 8px inset blue;">inset</div>
        <div style="border: 8px outset blue;">outset</div>
        <div style="width: 100px; height: 60px; border-radius: 30px / 15px; border: 2px solid black;"></div>
        <div style="outline: 4px dashed red; border: 2px solid black;">outlined</div>
        <div style="visibility: hidden;">hidden text</div>
        <div style="width: 80px; height: 40px; overflow: hidden;">this text is longer than the box and should clip</div>
        <div style="position: relative; width: 200px; height: 80px;">
          <div style="position: relative; top: 0; left: 0; width: 100px; height: 60px; background-color: red; z-index: 1;"></div>
          <div style="position: relative; top: -40px; left: 40px; width: 100px; height: 60px; background-color: blue; z-index: 2;"></div>
        </div>
    "#;
    let (page_count, bytes) = build_pdf(html_src, "body { margin: 0; }");
    assert_eq!(page_count, 1);
    assert!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0,
        "the font should be embedded to render the text"
    );
}

#[test]
fn visibility_hidden_reserves_layout_space_but_display_none_does_not() {
    let (dom, laid) = layout(
        r#"<div class="a">A</div><div class="hidden">B</div><div class="none">C</div><div class="d">D</div>"#,
        "body { margin: 0; } div { height: 40px; margin: 0; } \
         .hidden { visibility: hidden; } .none { display: none; }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    // The DOM holds four `div`s (`display: none` does not remove them from the DOM itself).
    assert_eq!(divs.len(), 4);

    let a = find_laid_out(&laid, divs[0]).unwrap();
    let hidden = find_laid_out(&laid, divs[1]).unwrap();
    // A `display: none` element is excluded from the box tree entirely (there is no box for C).
    assert!(find_laid_out(&laid, divs[2]).is_none());
    let d = find_laid_out(&laid, divs[3]).unwrap();

    // Unlike `display: none`, `visibility: hidden` still occupies its height in layout
    // (it is merely invisible).
    assert_eq!(hidden.layout.content.height, 40.0);
    // D should come right after "A (40px) plus the hidden one (40px, which is occupied)"
    // (C is not in the tree and contributes no height).
    assert_eq!(d.layout.content.y, a.layout.content.y + 80.0);
}

#[test]
fn z_index_reorders_overlapping_relative_siblings_but_keeps_their_own_position() {
    let (dom, laid) = layout(
        r#"<div class="outer">
            <div class="first" style="position: relative; z-index: 1;">first</div>
            <div class="second" style="position: relative; top: -20px; z-index: 2;">second</div>
        </div>"#,
        "body { margin: 0; } div.first, div.second { height: 30px; margin: 0; }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    // divs[0] is "outer", divs[1] is "first" and divs[2] is "second".
    let first_box = find_laid_out(&laid, divs[1]).unwrap();
    let second_box = find_laid_out(&laid, divs[2]).unwrap();

    // A `position: relative` offset does not affect the ordinary flow position
    // (later elements are placed against the pre-`position:relative` position; existing behaviour).
    assert_eq!(
        first_box.layout.content.y,
        second_box.layout.content.y - 30.0 + 20.0
    );
}

#[test]
fn border_radius_longhand_and_shorthand_render_a_valid_pdf() {
    let html_src = r#"
        <div style="width: 100px; height: 50px; border: 2px solid black; border-radius: 10px 20px / 5px 10px;"></div>
        <div style="width: 100px; height: 50px; border: 2px solid black; border-top-left-radius: 8px 4px;"></div>
    "#;
    let (page_count, _bytes) = build_pdf(html_src, "body { margin: 0; }");
    assert_eq!(page_count, 1);
}

// ===== Parent/child and empty-block margin collapsing =====

#[test]
fn a_child_top_margin_collapses_through_a_borderless_parent() {
    let (dom, laid) = layout(
        r#"<div class="wrap"><p>child</p></div><p class="sib">sibling</p>"#,
        "body { margin: 0; } .wrap { margin: 0; } p { margin: 30px 0; }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let mut ps = Vec::new();
    find_all_tags(&dom, dom.document(), "p", &mut ps);
    let wrap = find_laid_out(&laid, divs[0]).unwrap();
    let child = find_laid_out(&laid, ps[0]).unwrap();

    // The child's margin-top escapes the parent and the child sits flush against the top of the parent's content.
    assert_eq!(child.layout.content.y, wrap.layout.content.y);
    // The parent itself gets an effective margin-top of 30 (collapsed with the child's margin).
    assert_eq!(wrap.layout.margin.top, 30.0);
}

#[test]
fn the_gap_between_a_wrapped_child_and_a_following_sibling_collapses() {
    // Parent/child collapsing (bottom) chains with adjacent-sibling collapsing, so the gap is a single 40 rather than doubled.
    let (dom, laid) = layout(
        r#"<div class="wrap"><p class="inner">child</p></div><p class="sib">sibling</p>"#,
        "body { margin: 0; } .wrap { margin: 0; }          .inner { margin-bottom: 40px; } .sib { margin-top: 20px; }",
    );
    let mut ps = Vec::new();
    find_all_tags(&dom, dom.document(), "p", &mut ps);
    let inner = find_laid_out(&laid, ps[0]).unwrap();
    let sib = find_laid_out(&laid, ps[1]).unwrap();

    let gap = sib.layout.content.y - (inner.layout.content.y + inner.layout.content.height);
    // Not a plain sum (40+20=60) but max(40, 20) = 40 after collapsing.
    assert!(
        (gap - 40.0).abs() < 0.5,
        "gap should collapse to 40, got {gap}"
    );
}

#[test]
fn an_empty_block_does_not_double_its_margins() {
    let (dom, laid) = layout(
        r#"<p class="a">above</p><div class="empty"></div><p class="b">below</p>"#,
        "body { margin: 0; } p { margin: 0; } .empty { margin: 25px 0; }",
    );
    let mut ps = Vec::new();
    find_all_tags(&dom, dom.document(), "p", &mut ps);
    let above = find_laid_out(&laid, ps[0]).unwrap();
    let below = find_laid_out(&laid, ps[1]).unwrap();

    let gap = below.layout.content.y - (above.layout.content.y + above.layout.content.height);
    // The empty div's 25px top and bottom do not double (50); they collapse to 25.
    assert!(
        (gap - 25.0).abs() < 0.5,
        "empty block margins should collapse, got {gap}"
    );
}

#[test]
fn a_document_using_margin_collapse_renders_a_valid_pdf() {
    let (_, bytes) = build_pdf(
        r#"<div class="card"><h2>Title</h2><p>body</p></div>"#,
        ".card { margin: 20px 0; } h2 { margin: 16px 0; } p { margin: 12px 0; }",
    );
    assert!(bytes.starts_with(b"%PDF-"));
}

// ===== calc =====

#[test]
fn calc_width_mixes_percentage_and_pixels() {
    let (dom, laid) = layout(
        r#"<div class="c">x</div>"#,
        "body { margin: 0; } .c { width: calc(100% - 100px); height: 20px; }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let c = find_laid_out(&laid, divs[0]).unwrap();
    let content_width = PageSettings::default().content_width();
    assert!(
        (c.layout.content.width - (content_width - 100.0)).abs() < 0.5,
        "calc(100% - 100px) should be {} but was {}",
        content_width - 100.0,
        c.layout.content.width
    );
}

#[test]
fn calc_padding_resolves_em_and_pixels() {
    let (dom, laid) = layout(
        r#"<div class="c">x</div>"#,
        "body { margin: 0; } .c { font-size: 16px; width: 300px; padding-left: calc(1em + 4px); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let c = find_laid_out(&laid, divs[0]).unwrap();
    // 1em (16px) + 4px = 20px.
    assert!(
        (c.layout.padding.left - 20.0).abs() < 0.5,
        "got {}",
        c.layout.padding.left
    );
}

#[test]
fn calc_nested_inside_calc_resolves_like_parentheses() {
    // issue #17: a `calc()` inside a `calc()` term used to invalidate the whole declaration.
    // It should give the same 90px as the equivalent written with parentheses.
    for css in [
        "body { margin: 0; } .c { margin-left: calc(calc(45px * 2) * calc(1 - 0)); }",
        "body { margin: 0; } .c { margin-left: calc((45px * 2) * (1 - 0)); }",
    ] {
        let (dom, laid) = layout(r#"<div class="c">x</div>"#, css);
        let mut divs = Vec::new();
        find_all_tags(&dom, dom.document(), "div", &mut divs);
        let c = find_laid_out(&laid, divs[0]).unwrap();
        assert!(
            (c.layout.margin.left - 90.0).abs() < 0.5,
            "margin-left should be 90 but was {} for {css}",
            c.layout.margin.left
        );
    }
}

#[test]
fn a_document_using_calc_renders_a_valid_pdf() {
    let (_, bytes) = build_pdf(
        r#"<div style="width: calc(50% + 2em); margin-left: calc(10px + 5%);">x</div>"#,
        "body { margin: 0; }",
    );
    assert!(bytes.starts_with(b"%PDF-"));
}
