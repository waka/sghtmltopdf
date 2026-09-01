//! E2E tests for the logical properties (`margin-inline`, `padding-block`,
//! `inset-inline-start` and so on).
//!
//! The only writing mode supported is `horizontal-tb` plus LTR, so the logical properties
//! are treated as a fixed mapping to the physical ones (`inline-start` = left,
//! `inline-end` = right, `block-start` = top, `block-end` = bottom). Tailwind v4 emits
//! `px-*`, `py-*`, `mx-auto`, `space-y-*` and the like in this form, so ignoring them loses
//! the horizontal padding and the centring entirely (#21).

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, LaidOutBox, LaidOutContent, Layout, PageSettings,
};
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
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

/// Lay out `<div class="c">x</div>` under `body { margin: 0 }` and return that div's `Layout`.
fn layout_div(css: &str) -> Layout {
    let dom = html::parse(br#"<div class="c">x</div>"#);
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(&format!("body {{ margin: 0; }} {css}"));
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    find_laid_out(&laid, divs[0]).unwrap().layout
}

/// From the computed style of `<div class="c">x</div>` under `body { margin: 0 }`, return the
/// horizontal corner radii in the order top-left, top-right, bottom-left, bottom-right.
fn corner_radii(css: &str) -> [f32; 4] {
    let dom = html::parse(br#"<div class="c">x</div>"#);
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(&format!("body {{ margin: 0; }} {css}"));
    let styles = compute_styles(&dom, &ua, &author);
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let style = &styles[&divs[0]];
    [
        style.border_top_left_radius.horizontal.0,
        style.border_top_right_radius.horizontal.0,
        style.border_bottom_left_radius.horizontal.0,
        style.border_bottom_right_radius.horizontal.0,
    ]
}

fn assert_near(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 0.5,
        "{what} should be {expected} but was {actual}"
    );
}

#[test]
fn padding_inline_start_maps_to_padding_left() {
    let l = layout_div(".c { padding-inline-start: 90px; }");
    assert_near(l.padding.left, 90.0, "padding-left");
    assert_near(l.padding.right, 0.0, "padding-right");
}

#[test]
fn padding_inline_and_block_shorthands_take_one_or_two_values() {
    let l = layout_div(".c { padding-inline: 10px 20px; padding-block: 5px; }");
    assert_near(l.padding.left, 10.0, "padding-left");
    assert_near(l.padding.right, 20.0, "padding-right");
    assert_near(l.padding.top, 5.0, "padding-top");
    assert_near(l.padding.bottom, 5.0, "padding-bottom");
}

#[test]
fn margin_inline_start_maps_to_margin_left() {
    let l = layout_div(".c { margin-inline-start: 90px; }");
    assert_near(l.margin.left, 90.0, "margin-left");
    assert_near(l.content.x, 90.0, "content x");
}

#[test]
fn margin_block_end_maps_to_margin_bottom() {
    let l = layout_div(".c { margin-block: 12px 30px; }");
    assert_near(l.margin.top, 12.0, "margin-top");
    assert_near(l.margin.bottom, 30.0, "margin-bottom");
}

#[test]
fn margin_inline_auto_centres_a_fixed_width_block() {
    let content_width = PageSettings::default().content_width();
    let l = layout_div(".c { width: 100px; margin-inline: auto; }");
    assert_near(l.content.x, (content_width - 100.0) / 2.0, "content x");
}

#[test]
fn a_later_physical_declaration_overrides_an_earlier_logical_one() {
    let l = layout_div(".c { padding-inline-start: 90px; padding-left: 10px; }");
    assert_near(l.padding.left, 10.0, "padding-left");
    let l = layout_div(".c { padding-left: 10px; padding-inline-start: 90px; }");
    assert_near(l.padding.left, 90.0, "padding-left");
}

#[test]
fn inset_inline_start_offsets_a_relative_box() {
    let l =
        layout_div(".c { position: relative; inset-inline-start: 120px; inset-block-start: 6px; }");
    assert_near(l.content.x, 120.0, "content x");
    assert_near(l.content.y, 6.0, "content y");
}

#[test]
fn inset_shorthand_expands_like_margin() {
    let l = layout_div(".c { position: relative; inset: 6px 0 0 120px; }");
    assert_near(l.content.x, 120.0, "content x");
    assert_near(l.content.y, 6.0, "content y");
}

#[test]
fn border_inline_start_maps_to_border_left() {
    let l =
        layout_div(".c { border-inline-start: 4px solid black; border-block: 2px solid black; }");
    assert_near(l.border.left, 4.0, "border-left");
    assert_near(l.border.right, 0.0, "border-right");
    assert_near(l.border.top, 2.0, "border-top");
    assert_near(l.border.bottom, 2.0, "border-bottom");
}

#[test]
fn border_inline_width_shorthand_takes_two_values() {
    let l = layout_div(
        ".c { border-style: solid; border-inline-width: 1px 3px; border-block-width: 5px; }",
    );
    assert_near(l.border.left, 1.0, "border-left");
    assert_near(l.border.right, 3.0, "border-right");
    assert_near(l.border.top, 5.0, "border-top");
    assert_near(l.border.bottom, 5.0, "border-bottom");
}

#[test]
fn logical_corner_radii_map_to_the_physical_corners() {
    // The first is the block direction and the second the inline direction.
    assert_eq!(
        corner_radii(".c { border-start-start-radius: 1px }"),
        [1.0, 0.0, 0.0, 0.0]
    );
    assert_eq!(
        corner_radii(".c { border-start-end-radius: 2px }"),
        [0.0, 2.0, 0.0, 0.0]
    );
    assert_eq!(
        corner_radii(".c { border-end-start-radius: 3px }"),
        [0.0, 0.0, 3.0, 0.0]
    );
    assert_eq!(
        corner_radii(".c { border-end-end-radius: 4px }"),
        [0.0, 0.0, 0.0, 4.0]
    );
}
