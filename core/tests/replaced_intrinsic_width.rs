//! E2E tests for how a block-level `<img>` contributes to its container's intrinsic width.
//!
//! The contribution of a replaced element is its used width (`width`, clamped by
//! `min-width`/`max-width`), not the natural size of the image. Without that, a downscaled
//! block image makes its flex item, table column or float as wide as the file.

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
    resolve_images(&mut tree, &dom, &cache);
    let laid = layout_document(&tree, &styles, &fonts, 720.0);
    let mut nodes = Vec::new();
    find_all_tags(&dom, dom.document(), tag, &mut nodes);
    find_laid_out(&laid, nodes[index])
        .unwrap_or_else(|| panic!("<{tag}>[{index}] should be laid out"))
        .layout
        .content
}

const BASE: &str = "* { margin: 0; padding: 0; box-sizing: border-box } \
    .row { display: flex; width: 540px; align-items: flex-start } .name { margin-left: auto }";

/// The height of one line of the sibling text, measured with wrapping disabled.
fn one_line_height() -> f32 {
    content_box(
        &format!("{BASE} .name {{ white-space: nowrap }}"),
        "<div class=row><div>x</div><div class=name>The Little Campus, inc.</div></div>",
        "div",
        2,
    )
    .height
}

/// Returns the content boxes of the image's flex item and of the text sibling.
fn flex_row(css: &str, natural: u32) -> (Rect, Rect) {
    let body = format!(
        "<div class=row><div><img src=\"{}\"></div><div class=name>The Little Campus, inc.</div></div>",
        png_data_uri(natural, 20)
    );
    let css = format!("{BASE} {css}");
    (
        content_box(&css, &body, "div", 1),
        content_box(&css, &body, "div", 2),
    )
}

#[test]
fn a_downscaled_block_image_does_not_make_its_flex_item_as_wide_as_the_file() {
    let (item, name) = flex_row("img { display: block; width: 160px }", 600);
    assert!(
        (item.width - 160.0).abs() < 0.5,
        "item width {}",
        item.width
    );
    assert!(
        (name.height - one_line_height()).abs() < 0.5,
        "the sibling wrapped: height {}",
        name.height
    );
}

#[test]
fn a_percentage_max_width_does_not_bring_back_the_natural_width() {
    let (item, name) = flex_row("img { display: block; width: 160px; max-width: 100% }", 600);
    assert!(
        (item.width - 160.0).abs() < 0.5,
        "item width {}",
        item.width
    );
    assert!((name.height - one_line_height()).abs() < 0.5);
}

#[test]
fn a_border_box_image_contributes_its_width_including_padding_and_border() {
    let (item, name) = flex_row(
        "img { display: block; width: 160px; padding: 10px; border: 2px solid }",
        600,
    );
    assert!(
        (item.width - 160.0).abs() < 0.5,
        "item width {}",
        item.width
    );
    assert!((name.height - one_line_height()).abs() < 0.5);
}

#[test]
fn max_width_in_pixels_caps_the_contribution() {
    let (item, _) = flex_row("img { display: block; max-width: 100px }", 600);
    assert!(
        (item.width - 100.0).abs() < 0.5,
        "item width {}",
        item.width
    );
}

#[test]
fn an_image_without_a_css_width_still_counts_at_its_natural_width() {
    let (item, _) = flex_row("img { display: block }", 200);
    assert!(
        (item.width - 200.0).abs() < 0.5,
        "item width {}",
        item.width
    );
}

#[test]
fn a_percentage_width_counts_as_auto_for_the_contribution() {
    let (item, _) = flex_row("img { display: block; width: 100% }", 200);
    assert!(
        (item.width - 200.0).abs() < 0.5,
        "item width {}",
        item.width
    );
}

#[test]
fn a_downscaled_block_image_does_not_widen_its_table_column() {
    let body = format!(
        "<table><tr><td><img src=\"{}\"></td><td>The Little Campus, inc.</td></tr></table>",
        png_data_uri(600, 20)
    );
    let css =
        "* { margin: 0; padding: 0 } table { width: auto } img { display: block; width: 160px }";
    let first = content_box(css, &body, "td", 0);
    let second = content_box(css, &body, "td", 1);
    assert!(
        first.width < second.width,
        "first {} second {}",
        first.width,
        second.width
    );
    assert!(
        second.height < 30.0,
        "second column wrapped: height {}",
        second.height
    );
}

#[test]
fn an_image_that_failed_to_load_keeps_its_width_attribute_next_to_a_css_height() {
    // No decoded size means no aspect ratio, so the width cannot be derived from the height.
    let body = "<div class=row><div><img src=\"missing.png\" width=\"80\"></div><div class=name>x</div></div>";
    let css = format!("{BASE} img {{ display: block; height: 60px }}");
    let item = content_box(&css, body, "div", 1);
    assert!((item.width - 80.0).abs() < 0.5, "item width {}", item.width);
}

#[test]
fn a_float_shrinks_to_the_used_width_of_its_block_image() {
    let body = format!("<div class=f><img src=\"{}\"></div>", png_data_uri(600, 20));
    let css = "* { margin: 0; padding: 0 } .f { float: left } img { display: block; width: 160px }";
    let float = content_box(css, &body, "div", 0);
    assert!(
        (float.width - 160.0).abs() < 0.5,
        "float width {}",
        float.width
    );
}
