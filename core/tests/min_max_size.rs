//! E2E tests for `min-width`/`max-width`/`min-height`/`max-height`.
//!
//! The same approach as `box_model.rs`/`flexbox.rs`: catch regressions by going through the
//! real pipeline (HTML parse, style cascade, layout, PDF encode).

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
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            children.iter().find_map(|c| find_laid_out(c, target))
        }
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .find_map(|item| find_laid_out(item, target)),
        LaidOutContent::Inline(lines) => lines
            .iter()
            .flat_map(|line| line.atomics.iter())
            .find_map(|atomic| find_laid_out(&atomic.content, target)),
        LaidOutContent::Table(table) => table
            .rows
            .iter()
            .flat_map(|row| row.cells.iter())
            .find_map(|cell| find_laid_out(cell, target)),
        LaidOutContent::Image(_) => None,
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

/// Look up the content box of an element carrying an `id` attribute.
fn content_box(
    dom: &Dom,
    laid: &LaidOutBox,
    tag: &str,
    index: usize,
) -> sghtmltopdf_core::layout::Rect {
    let mut nodes = Vec::new();
    find_all_tags(dom, dom.document(), tag, &mut nodes);
    let node = nodes[index];
    find_laid_out(laid, node)
        .unwrap_or_else(|| panic!("<{tag}>[{index}] should be laid out"))
        .layout
        .content
}

#[test]
fn max_width_limits_the_used_width_of_a_block() {
    let (dom, laid) = layout(
        "<div>constrained</div>",
        "body { margin: 0; } div { max-width: 200px; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 200.0);
}

#[test]
fn min_width_expands_a_narrower_block() {
    let (dom, laid) = layout(
        "<div>widened</div>",
        "body { margin: 0; } div { width: 50px; min-width: 200px; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).width, 200.0);
}

#[test]
fn min_width_wins_when_it_exceeds_max_width() {
    // CSS2.1 section 10.4: max-width is applied before min-width, so min wins.
    let (dom, laid) = layout(
        "<div>min wins</div>",
        "body { margin: 0; } div { width: 400px; min-width: 300px; max-width: 100px; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).width, 300.0);
}

#[test]
fn auto_width_clamped_by_max_width_is_centered_by_auto_margins() {
    // If the margin autos stayed squashed to 0 by the `width: auto` branch it would not centre.
    // Re-solving the horizontal equation after the clamp brings it to the centre.
    let containing_width = PageSettings::default().content_width();
    let (dom, laid) = layout(
        "<div>centered</div>",
        "body { margin: 0; } div { max-width: 200px; margin-left: auto; margin-right: auto; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 200.0);
    let expected_x = (containing_width - 200.0) / 2.0;
    assert!(
        (content.x - expected_x).abs() < 0.01,
        "expected x={expected_x}, got {}",
        content.x
    );
}

#[test]
fn min_and_max_width_are_border_box_relative_under_border_box_sizing() {
    // Under `box-sizing: border-box` the value is border-box based, so the content width is
    // that minus padding and border (the same rule as `box-sizing`).
    let (dom, laid) = layout(
        "<div>bb</div>",
        "body { margin: 0; } \
         div { box-sizing: border-box; max-width: 200px; padding: 0 20px; border: 0 solid black; border-left-width: 5px; border-right-width: 5px; border-style: solid; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 200.0 - 40.0 - 10.0);
}

#[test]
fn min_height_expands_and_max_height_shrinks_the_used_height() {
    let (dom, laid) = layout(
        r#"<div class="tall"></div><div class="short">a</div>"#,
        "body { margin: 0; } .tall { min-height: 120px; } .short { max-height: 5px; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).height, 120.0);
    assert_eq!(content_box(&dom, &laid, "div", 1).height, 5.0);
}

#[test]
fn explicit_height_is_clamped_by_max_height() {
    let (dom, laid) = layout(
        "<div>clamped</div>",
        "body { margin: 0; } div { height: 300px; max-height: 80px; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).height, 80.0);
}

#[test]
fn percentage_min_width_resolves_against_the_containing_block() {
    let containing_width = PageSettings::default().content_width();
    let (dom, laid) = layout(
        "<div>half</div>",
        "body { margin: 0; } div { width: 10px; min-width: 50%; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert!(
        (content.width - containing_width * 0.5).abs() < 0.01,
        "expected {}, got {}",
        containing_width * 0.5,
        content.width
    );
}

#[test]
fn percentage_min_height_is_ignored() {
    // It is ignored, the containing block's height being indefinite (as with `height: %`).
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { min-height: 50%; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert!(
        content.height < 100.0,
        "percentage min-height should be ignored, got height={}",
        content.height
    );
}

#[test]
fn calc_min_width_is_supported() {
    let containing_width = PageSettings::default().content_width();
    let (dom, laid) = layout(
        "<div>calc</div>",
        "body { margin: 0; } div { width: 10px; min-width: calc(100% - 100px); }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert!(
        (content.width - (containing_width - 100.0)).abs() < 0.01,
        "expected {}, got {}",
        containing_width - 100.0,
        content.width
    );
}

#[test]
fn max_width_applies_to_floats() {
    let (dom, laid) = layout(
        r#"<div class="f">float</div><p>text that wraps beside the float</p>"#,
        "body { margin: 0; } .f { float: left; width: 400px; max-width: 120px; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).width, 120.0);
}

#[test]
fn max_width_applies_to_inline_blocks() {
    let (dom, laid) = layout(
        r#"<p><span class="ib">inline block content</span></p>"#,
        "body { margin: 0; } .ib { display: inline-block; width: 300px; max-width: 90px; }",
    );
    assert_eq!(content_box(&dom, &laid, "span", 0).width, 90.0);
}

#[test]
fn min_width_applies_to_absolutely_positioned_boxes() {
    let dom = html::parse(br#"<div class="host"><div class="badge">B</div></div>"#);
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(
        "body { margin: 0; } .host { position: relative; height: 200px; } \
         .badge { position: absolute; top: 0; left: 0; width: 10px; min-width: 80px; }",
    );
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let badge = divs[1];
    let found = pages
        .iter()
        .flat_map(|page| page.boxes.iter())
        .find_map(|b| find_laid_out(b, badge))
        .expect("absolutely positioned badge should be laid out");
    assert_eq!(found.layout.content.width, 80.0);
}

#[test]
fn min_and_max_width_apply_to_flex_items() {
    let (dom, laid) = layout(
        r#"<div class="row"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } .row { display: flex; width: 400px; } \
         .a { flex: 0 0 300px; max-width: 100px; } \
         .b { flex: 0 0 50px; min-width: 250px; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 1).width, 100.0);
    assert_eq!(content_box(&dom, &laid, "div", 2).width, 250.0);
}

#[test]
fn cell_min_width_widens_its_column_in_auto_table_layout() {
    // A cell's min-width pushes up the column's natural width.
    let narrow = table_first_row_widths(
        r#"<table><tr><td class="c">x</td><td>yyyyyyyyyyyyyyyy</td></tr></table>"#,
        "body { margin: 0; } table { width: 400px; }",
    );
    let widened = table_first_row_widths(
        r#"<table><tr><td class="c">x</td><td>yyyyyyyyyyyyyyyy</td></tr></table>"#,
        "body { margin: 0; } table { width: 400px; } .c { min-width: 200px; }",
    );
    assert!(
        widened[0] > narrow[0] * 2.0,
        "min-width should widen the first column: {narrow:?} -> {widened:?}"
    );
}

#[test]
fn cell_min_width_becomes_the_column_width_in_fixed_table_layout() {
    let widths = table_first_row_widths(
        r#"<table><tr><td class="c">x</td><td>y</td></tr></table>"#,
        "body { margin: 0; } table { table-layout: fixed; width: 400px; } \
         .c { min-width: 150px; }",
    );
    assert!(
        (widths[0] - 150.0).abs() < 0.01,
        "expected the min-width to become the column width, got {widths:?}"
    );
}

fn table_first_row_widths(html_src: &str, css: &str) -> Vec<f32> {
    let (_, laid) = layout(html_src, css);

    fn find_table(b: &LaidOutBox) -> Option<Vec<f32>> {
        match &b.content {
            LaidOutContent::Table(table) => Some(
                table.rows[0]
                    .cells
                    .iter()
                    .map(|c| c.layout.border_box().width)
                    .collect(),
            ),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().find_map(find_table)
            }
            _ => None,
        }
    }
    find_table(&laid).expect("table not found")
}

#[test]
fn min_and_max_size_features_combined_render_a_valid_pdf_end_to_end() {
    let html_src = r#"
        <div class="card">max-width card, centered</div>
        <div class="tall">min-height</div>
        <div class="row"><div class="a">a</div><div class="b">b</div></div>
        <table><tr><td class="c">x</td><td>y</td></tr></table>
        <p><span class="ib">inline block</span></p>
    "#;
    let css = "body { margin: 0; } \
        .card { max-width: 300px; margin: 0 auto; border: 1px solid black; } \
        .tall { min-height: 120px; background-color: #eee; } \
        .row { display: flex; width: 400px; } \
        .a { flex: 1; max-width: 100px; } .b { flex: 1; min-width: 200px; } \
        table { width: 400px; } .c { min-width: 150px; } \
        .ib { display: inline-block; max-width: 80px; }";

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
    assert_eq!(count_occurrences(&bytes, b"/MediaBox"), pages.len());
}
