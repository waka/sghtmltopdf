//! E2E tests for how a block-level child's own `width`/`min-width`/`max-width` feeds the
//! shrink-to-fit width of a float, an `inline-block` or a flex item (issue #87).
//!
//! The child contributes its used width, not the natural width of its content, so the
//! parent is as wide as the child instead of the child sticking out of it.

use std::io::Write;

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::layout::{
    build_box_tree, layout_document, resolve_images, LaidOutBox, LaidOutContent, Rect,
};
use sghtmltopdf::pdf::ImageAssetCache;
use sghtmltopdf::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

/// A solid black greyscale PNG of the given natural size, as a data URI.
fn png_data_uri(width: u32, height: u32) -> String {
    use base64::Engine;
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = flate2::Crc::new();
        crc.update(kind);
        crc.update(data);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    }
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(&mut png, b"IHDR", &ihdr);
    let rows = vec![0u8; (1 + width as usize) * height as usize];
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&rows).unwrap();
    chunk(&mut png, b"IDAT", &z.finish().unwrap());
    chunk(&mut png, b"IEND", &[]);
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    )
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

/// Lay `body` out in a 720px wide area and return the content box of `<tag>[index]`.
fn content_box(css: &str, body: &str, tag: &str, index: usize) -> Rect {
    let html_src = format!("<!DOCTYPE html><html><body>{body}</body></html>");
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let mut tree = build_box_tree(&dom, &styles);
    let cache = ImageAssetCache::new(std::path::PathBuf::from("."), false);
    resolve_images(&mut tree, &dom, &styles, &cache);
    let laid = layout_document(&tree, &styles, &fonts, 720.0);
    let mut nodes = Vec::new();
    find_all_tags(&dom, dom.document(), tag, &mut nodes);
    find_laid_out(&laid, nodes[index])
        .unwrap_or_else(|| panic!("<{tag}>[{index}] should be laid out"))
        .layout
        .content
}

const BASE: &str = "* { margin: 0; padding: 0 } .f { float: left } \
    .ib { display: inline-block } .row { display: flex; align-items: flex-start }";

/// The content width of the `.w` wrapper around `<div style="{child}">x</div>`.
fn wrapper_around_text(wrapper: &str, child: &str) -> f32 {
    let body = format!("<div class=\"{wrapper}\"><div style=\"{child}\">x</div></div>");
    content_box(BASE, &body, "div", 0).width
}

/// The content width of the `.w` wrapper around a 32x24 block image styled with `img`.
fn wrapper_around_image(wrapper: &str, img: &str) -> f32 {
    let body = format!(
        "<div class=\"{wrapper}\"><img src=\"{}\"></div>",
        png_data_uri(32, 24)
    );
    content_box(&format!("{BASE} img {{ {img} }}"), &body, "div", 0).width
}

/// The same as [`wrapper_around_image`], with the wrapper as the item of a flex row.
fn flex_item_around_image(img: &str) -> f32 {
    let body = format!(
        "<div class=row><div><img src=\"{}\"></div></div>",
        png_data_uri(32, 24)
    );
    content_box(&format!("{BASE} img {{ {img} }}"), &body, "div", 1).width
}

fn assert_width(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 0.5,
        "width {actual}, expected {expected}"
    );
}

#[test]
fn a_float_shrinks_to_a_childs_definite_width() {
    assert_width(wrapper_around_text("f", "width: 100px"), 100.0);
}

#[test]
fn a_float_grows_to_a_childs_min_width() {
    assert_width(wrapper_around_text("f", "min-width: 100px"), 100.0);
}

#[test]
fn a_float_shrinks_to_a_childs_max_width() {
    assert_width(wrapper_around_text("f", "max-width: 5px"), 5.0);
}

#[test]
fn a_childs_max_width_caps_its_definite_width() {
    assert_width(
        wrapper_around_text("f", "width: 100px; max-width: 40px"),
        40.0,
    );
}

#[test]
fn min_and_max_width_are_border_box_relative_under_border_box_sizing() {
    // min-width 100px border-box: the child's border box is 100px wide whatever its padding.
    assert_width(
        wrapper_around_text(
            "f",
            "box-sizing: border-box; min-width: 100px; padding: 0 10px",
        ),
        100.0,
    );
}

#[test]
fn an_inline_block_grows_to_a_childs_min_width() {
    assert_width(wrapper_around_text("ib", "min-width: 100px"), 100.0);
}

#[test]
fn a_percentage_max_width_on_a_child_does_not_collapse_the_float() {
    // The percentage has no basis while the float's width is being found, so it counts as `none`.
    let natural = wrapper_around_text("f", "");
    assert!(natural > 1.0, "natural width {natural}");
    assert_width(wrapper_around_text("f", "max-width: 50%"), natural);
}

#[test]
fn a_percentage_min_width_on_a_child_does_not_inflate_the_float() {
    let natural = wrapper_around_text("f", "");
    assert_width(wrapper_around_text("f", "min-width: 50%"), natural);
}

#[test]
fn a_float_shrinks_to_its_block_images_definite_width() {
    assert_width(
        wrapper_around_image("f", "display: block; width: 16px"),
        16.0,
    );
}

#[test]
fn a_float_shrinks_to_its_block_images_width_derived_from_height() {
    assert_width(
        wrapper_around_image("f", "display: block; height: 12px"),
        16.0,
    );
}

#[test]
fn a_float_shrinks_to_its_block_images_max_width() {
    assert_width(
        wrapper_around_image("f", "display: block; max-width: 16px"),
        16.0,
    );
}

#[test]
fn an_inline_block_shrinks_to_its_block_images_max_width() {
    assert_width(
        wrapper_around_image("ib", "display: block; max-width: 16px"),
        16.0,
    );
}

#[test]
fn a_flex_item_shrinks_to_its_block_images_max_width() {
    assert_width(
        flex_item_around_image("display: block; max-width: 16px"),
        16.0,
    );
}

#[test]
fn a_block_images_max_width_caps_its_definite_width() {
    assert_width(
        wrapper_around_image("f", "display: block; width: 32px; max-width: 16px"),
        16.0,
    );
}

#[test]
fn a_float_shrinks_to_its_inline_images_max_width() {
    assert_width(wrapper_around_image("f", "max-width: 16px"), 16.0);
}

// A `calc()` with no percentage in it is as definite as a plain length.

#[test]
fn a_float_shrinks_to_a_childs_pure_length_calc_width() {
    assert_width(
        wrapper_around_text("f", "font-size: 10px; width: calc(110px - 1em)"),
        100.0,
    );
}

#[test]
fn a_float_grows_to_a_childs_pure_length_calc_min_width() {
    assert_width(
        wrapper_around_text("f", "font-size: 10px; min-width: calc(110px - 1em)"),
        100.0,
    );
}

#[test]
fn a_float_shrinks_to_a_childs_pure_length_calc_max_width() {
    assert_width(
        wrapper_around_text(
            "f",
            "font-size: 10px; width: 100px; max-width: calc(50px - 1em)",
        ),
        40.0,
    );
}

#[test]
fn a_calc_with_a_percentage_on_a_child_still_counts_as_none() {
    let natural = wrapper_around_text("f", "");
    assert_width(
        wrapper_around_text("f", "max-width: calc(50% - 1px)"),
        natural,
    );
}

#[test]
fn a_float_shrinks_to_its_block_images_pure_length_calc_max_width() {
    assert_width(
        wrapper_around_image(
            "f",
            "display: block; font-size: 10px; max-width: calc(26px - 1em)",
        ),
        16.0,
    );
}

#[test]
fn a_float_shrinks_to_its_block_images_pure_length_calc_width() {
    assert_width(
        wrapper_around_image(
            "f",
            "display: block; font-size: 10px; width: calc(26px - 1em)",
        ),
        16.0,
    );
}
