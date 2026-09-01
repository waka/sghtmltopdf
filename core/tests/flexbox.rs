//! E2E tests for Flexbox (`display: flex`).
//!
//! The same approach as `box_sizing.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode).

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
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            children.iter().find_map(|c| find_laid_out(c, target))
        }
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .find_map(|item| find_laid_out(item, target)),
        LaidOutContent::Inline(_) | LaidOutContent::Table(_) | LaidOutContent::Image(_) => None,
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

fn divs(dom: &Dom) -> Vec<NodeId> {
    let mut out = Vec::new();
    find_all_tags(dom, dom.document(), "div", &mut out);
    out
}

#[test]
fn flex_direction_row_places_items_side_by_side() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; width: 300px; } \
         .a, .b { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    let b = find_laid_out(&laid, d[2]).unwrap();

    assert_eq!(a.layout.border_box().x, 0.0);
    assert_eq!(b.layout.border_box().x, 50.0);
    assert_eq!(a.layout.border_box().y, b.layout.border_box().y);
}

#[test]
fn flex_direction_column_stacks_items_vertically() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; flex-direction: column; width: 300px; } \
         .a, .b { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    let b = find_laid_out(&laid, d[2]).unwrap();

    assert_eq!(a.layout.border_box().y, 0.0);
    assert_eq!(b.layout.border_box().y, 20.0);
    assert_eq!(a.layout.border_box().x, b.layout.border_box().x);
}

#[test]
fn justify_content_space_between_pushes_items_to_the_edges() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; justify-content: space-between; width: 300px; } \
         .a, .b { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    let b = find_laid_out(&laid, d[2]).unwrap();

    assert_eq!(a.layout.border_box().x, 0.0);
    assert_eq!(b.layout.border_box().x, 250.0);
}

#[test]
fn align_items_center_centers_items_on_the_cross_axis() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; align-items: center; width: 300px; height: 100px; } \
         .a { width: 50px; height: 20px; } \
         .b { width: 50px; height: 40px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    // A 20px-tall item is centred about the middle (50px) of a 100px-tall container, so its
    // top edge should be 50 - 10 = 40px.
    assert_eq!(a.layout.border_box().y, 40.0);
}

#[test]
fn flex_grow_distributes_remaining_space() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; width: 300px; } \
         .a { flex-grow: 1; height: 20px; } \
         .b { width: 100px; height: 20px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    // .b is fixed at 100px and .a takes all of the remaining 200px through flex-grow:1.
    assert_eq!(a.layout.border_box().width, 200.0);
}

#[test]
fn flex_shrink_zero_prevents_an_item_from_shrinking_below_its_basis() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; width: 150px; } \
         .a { width: 100px; flex-shrink: 0; height: 20px; } \
         .b { width: 100px; height: 20px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    assert_eq!(a.layout.border_box().width, 100.0);
}

#[test]
fn gap_adds_space_between_items() {
    let (dom, laid) = layout(
        r#"<div class="container"><div class="a">a</div><div class="b">b</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; gap: 10px; width: 300px; } \
         .a, .b { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    let b = find_laid_out(&laid, d[2]).unwrap();
    assert_eq!(b.layout.border_box().x, 60.0);
}

#[test]
fn a_flex_item_can_contain_ordinary_block_and_table_content() {
    let (dom, laid) = layout(
        r#"<div class="container">
             <div class="a"><p>text</p></div>
             <table class="b"><tr><td>cell</td></tr></table>
           </div>"#,
        "body { margin: 0; } \
         .container { display: flex; width: 300px; } \
         .a { width: 100px; } \
         .b { width: 100px; }",
    );
    let mut tables = Vec::new();
    find_all_tags(&dom, dom.document(), "table", &mut tables);
    let table_box = find_laid_out(&laid, tables[0]).unwrap();
    assert!(matches!(table_box.content, LaidOutContent::Table(_)));
}

#[test]
fn a_flex_item_with_padding_grows_to_fit_its_wrapped_text() {
    // The known size taffy passes to measure is border-box based, so measuring the content
    // without subtracting the padding would break lines at a wider width than the real one,
    // leaving the item too short and its content overflowing. That the content height does
    // not change with or without padding confirms it is measured at the content-box width.
    let text = "wrap this text onto two lines";
    let (dom, laid) = layout(
        &format!(
            r#"<div class="container">
                 <div class="plain">{text}</div>
                 <div class="padded">{text}</div>
               </div>"#
        ),
        // With stretch both would grow to the line's cross size and the difference would
        // vanish, so flex-start is used to see the natural height.
        "body { margin: 0; } \
         .container { display: flex; align-items: flex-start; width: 400px; } \
         .plain { flex: 0 0 100px; } \
         .padded { flex: 0 0 100px; padding: 10px; }",
    );
    let d = divs(&dom);
    let plain = find_laid_out(&laid, d[1]).unwrap();
    let padded = find_laid_out(&laid, d[2]).unwrap();

    // The content width is the same (100px), so a box taller by the padding is correct.
    assert_eq!(
        padded.layout.border_box().height,
        plain.layout.border_box().height + 20.0
    );
    // Premise: this text wraps onto several lines at a width of 100px (fitting on one line
    // would hide the regression, so it is confirmed by being taller than the line spacing).
    assert!(plain.layout.border_box().height > 20.0);
}

#[test]
fn a_nested_flex_container_lays_out_inside_a_flex_item() {
    let (dom, laid) = layout(
        r#"<div class="outer">
             <div class="inner"><div class="x">x</div><div class="y">y</div></div>
           </div>"#,
        "body { margin: 0; } \
         .outer { display: flex; width: 300px; } \
         .inner { display: flex; width: 200px; } \
         .x, .y { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    // d[0]=outer, d[1]=inner, d[2]=x, d[3]=y
    let y = find_laid_out(&laid, d[3]).unwrap();
    assert_eq!(y.layout.border_box().x, 50.0);
}

#[test]
fn a_flex_container_is_treated_as_an_atomic_unit_across_page_breaks() {
    // Create a state where the body as a whole is slightly larger than the height left on the
    // page, and confirm that the flex container is moved whole to the next page rather than
    // being split internally (treated as atomic, like `display: table`).
    let page_height = PageSettings::default().content_height();
    let filler_height = page_height - 30.0;
    let html =
        r#"<div class="filler">filler</div><div class="container"><div class="a">a</div></div>"#;
    let css = format!(
        "body {{ margin: 0; }} \
         .filler {{ height: {filler_height}px; }} \
         .container {{ display: flex; width: 300px; }} \
         .a {{ width: 50px; height: 60px; }}"
    );
    let dom = html::parse(html.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(&css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    assert_eq!(pages.len(), 2, "container should be pushed whole to page 2");

    let mut container_ids = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut container_ids);
    let container_node = container_ids[1]; // in the order filler, container, a

    fn found_on_page(page: &sghtmltopdf_core::layout::Page, target: NodeId) -> Option<&LaidOutBox> {
        page.boxes.iter().find_map(|b| find_laid_out(b, target))
    }
    assert!(
        found_on_page(&pages[0], container_node).is_none(),
        "container must not appear (even partially) on page 1"
    );
    let on_page2 = found_on_page(&pages[1], container_node).expect("container should be on page 2");
    assert!(matches!(on_page2.content, LaidOutContent::Flex(_)));
}

#[test]
fn flexbox_renders_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="invoice-header">
             <div class="company">Acme Corp</div>
             <div class="date">2026-07-23</div>
           </div>"#,
        "body { margin: 0; } \
         .invoice-header { display: flex; justify-content: space-between; align-items: center; }",
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

// ===== Anonymous flex items made from bare text =====

/// Concatenate and return the line text of the laid-out tree in order of appearance.
fn laid_out_text(b: &LaidOutBox) -> String {
    fn walk(b: &LaidOutBox, out: &mut String) {
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for child in children {
                    walk(child, out);
                }
            }
            LaidOutContent::Grid(grid) => {
                for child in grid.rows.iter().flat_map(|row| &row.items) {
                    walk(child, out);
                }
            }
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    for run in &line.runs {
                        out.push_str(&run.text);
                    }
                }
            }
            LaidOutContent::Table(_) | LaidOutContent::Image(_) => {}
        }
    }
    let mut out = String::new();
    walk(b, &mut out);
    out
}

/// Return the first flex item that is an anonymous box (whose `node` is `None`).
fn first_anonymous_flex_item(b: &LaidOutBox) -> Option<&LaidOutBox> {
    match &b.content {
        LaidOutContent::Flex(children) => children
            .iter()
            .find(|item| item.node.is_none())
            .or_else(|| children.iter().find_map(first_anonymous_flex_item)),
        LaidOutContent::Blocks(children) => children.iter().find_map(first_anonymous_flex_item),
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .find_map(first_anonymous_flex_item),
        _ => None,
    }
}

#[test]
fn bare_text_in_a_flex_container_is_rendered() {
    // Regression test: text not wrapped in an element did not become a flex item and vanished
    // from the output entirely (leaving an empty frame for something like
    // `<div style="display:flex">seal</div>`).
    let (_, laid) = layout(
        r#"<div class="container">bare</div>"#,
        "body { margin: 0; } .container { display: flex; width: 200px; }",
    );

    let text = laid_out_text(&laid);
    assert!(
        text.contains("bare"),
        "the bare text should be rendered, got {text:?}"
    );

    // Confirm it survives to PDF too.
    build_pdf(
        r#"<div class="container">bare</div>"#,
        "body { margin: 0; } .container { display: flex; width: 200px; }",
    );
}

#[test]
fn bare_text_is_positioned_by_the_flex_alignment_properties() {
    // An anonymous item is aligned like any other flex item.
    let (_, laid) = layout(
        r#"<div class="container">x</div>"#,
        "body { margin: 0; } \
         .container { display: flex; justify-content: flex-end; width: 200px; }",
    );

    let item = first_anonymous_flex_item(&laid).expect("expected an anonymous flex item");
    assert!(
        item.layout.border_box().x > 0.0,
        "an end-aligned item should not sit at the container's left edge"
    );
}

#[test]
fn whitespace_between_flex_items_does_not_become_an_item() {
    // Making an anonymous item out of the newlines and indentation between elements would mix
    // in zero-width items and throw the spacing off.
    let (dom, laid) = layout(
        "<div class=\"container\">\n  <div class=\"a\">a</div>\n  <div class=\"b\">b</div>\n</div>",
        "body { margin: 0; } \
         .container { display: flex; width: 300px; } \
         .a, .b { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    let a = find_laid_out(&laid, d[1]).unwrap();
    let b = find_laid_out(&laid, d[2]).unwrap();

    assert_eq!(a.layout.border_box().x, 0.0);
    assert_eq!(b.layout.border_box().x, 50.0);
    assert!(first_anonymous_flex_item(&laid).is_none());
}

#[test]
fn a_text_run_and_an_element_become_separate_items() {
    let (dom, laid) = layout(
        r#"<div class="container">left<div class="e">e</div></div>"#,
        "body { margin: 0; } \
         .container { display: flex; width: 300px; } \
         .e { width: 50px; height: 20px; }",
    );
    let d = divs(&dom);
    let element_item = find_laid_out(&laid, d[1]).unwrap();
    let anonymous = first_anonymous_flex_item(&laid).expect("expected an anonymous flex item");

    assert_eq!(anonymous.layout.border_box().x, 0.0);
    assert!(
        element_item.layout.border_box().x > 0.0,
        "the element item should follow the anonymous text item"
    );
    assert_eq!(laid_out_text(anonymous), "left");
}

/// The case where a flex item's contents are themselves a flex container. Back when the
/// natural width measured as 0, the inner container collapsed to zero width too
/// (grid-in-grid, flex-in-grid and grid-in-flex take the same path).
#[test]
fn a_flex_inside_a_flex_item_does_not_collapse_to_zero() {
    let (dom, laid) = layout(
        r#"<div class="outer"><div class="item"><div class="a">alpha</div><div class="b">beta</div></div></div>"#,
        "body { margin: 0; } \
         .outer { display: flex; width: 400px; } \
         .item { display: flex; gap: 10px; }",
    );
    let d = divs(&dom);
    let item = find_laid_out(&laid, d[1]).unwrap();
    let a = find_laid_out(&laid, d[2]).unwrap();
    let b = find_laid_out(&laid, d[3]).unwrap();

    assert!(
        item.layout.content.width > 0.0,
        "the inner flex container must not collapse: {:?}",
        item.layout.content
    );
    assert!(a.layout.content.width > 0.0 && b.layout.content.width > 0.0);
    // The main axis is horizontal, so the inner natural width is the sum of the two items plus the gap.
    assert!(
        item.layout.content.width >= a.layout.content.width + b.layout.content.width,
        "a={:?} b={:?} item={:?}",
        a.layout.content,
        b.layout.content,
        item.layout.content
    );
}

// ===== The measured natural width is not rounded =====

/// Return the number of laid-out lines of the given node.
fn line_count(b: &LaidOutBox) -> usize {
    match &b.content {
        LaidOutContent::Inline(lines) => lines.len(),
        _ => panic!("not a box with inline content"),
    }
}

#[test]
fn a_flex_item_is_not_rounded_below_the_width_its_text_needs() {
    // Regression test: taffy rounds the final layout to integers by default, so the natural
    // width from measuring was truncated, the item came out narrower than its content, and a
    // line that should have fitted wrapped. Which way it rounds depends on the fraction, so
    // it surfaced as only certain content wrapping despite identical formatting.
    //
    // The two below are 146.16px and 129.98px natural width in DejaVuSans at 14px. Rounding
    // truncated only the former to 146, leaving it 0.16px short and dropping `EUR` to a second line.
    for value in ["1 USD = 0.9143 EUR", "1 USD 0.9143 EUR"] {
        let (dom, laid) = layout(
            &format!(
                r#"<div class="row"><span class="k">Exchange rate</span><span class="v">{value}</span></div>"#
            ),
            "body { margin: 0; font-size: 14px; } \
             .row { display: flex; justify-content: space-between; gap: 24px; } \
             .k { white-space: nowrap; } \
             .v { text-align: right; }",
        );
        let mut spans = Vec::new();
        find_all_tags(&dom, dom.document(), "span", &mut spans);
        let v = find_laid_out(&laid, spans[1]).unwrap();
        assert_eq!(
            line_count(v),
            1,
            "there is room on the line, so it must not wrap: {value:?} width={:?}",
            v.layout.content
        );
    }
}

#[test]
fn flex_item_widths_keep_their_fractional_part() {
    // The groundwork for the test above. It looks directly at the item's width not being
    // rounded to an integer (if the rounding came back, the vanishing fraction would show it first).
    let (dom, laid) = layout(
        r#"<div class="row"><span class="v">1 USD = 0.9143 EUR</span></div>"#,
        "body { margin: 0; font-size: 14px; } \
         .row { display: flex; }",
    );
    let mut spans = Vec::new();
    find_all_tags(&dom, dom.document(), "span", &mut spans);
    let v = find_laid_out(&laid, spans[0]).unwrap();
    assert!(
        v.layout.content.width.fract() != 0.0,
        "the natural width's fraction has been lost: {:?}",
        v.layout.content
    );
}
