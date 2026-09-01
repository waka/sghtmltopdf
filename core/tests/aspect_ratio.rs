//! E2E tests for `aspect-ratio`.
//!
//! The same approach as `min_max_size.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, layout, PDF encode).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, resolve_images, LaidOutBox, LaidOutContent,
    PageSettings, Rect,
};
use sghtmltopdf_core::pdf::{encode_pdf, ImageAssetCache};
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

/// A 32x24 (4:3) JPEG. Used to check the intrinsic aspect ratio.
const JPEG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_gradient.jpg"
);

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn jpeg_data_uri() -> String {
    use base64::Engine;
    let jpeg = std::fs::read(JPEG_PATH).expect("fixture image");
    format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
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

/// Layout that also resolves images (for HTML containing an `<img>`).
fn layout(html_src: &str, css: &str) -> (Dom, LaidOutBox) {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let mut tree = build_box_tree(&dom, &styles);
    let cache = ImageAssetCache::new(std::path::PathBuf::from("."), false);
    resolve_images(&mut tree, &dom, &cache);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );
    (dom, laid)
}

fn content_box(dom: &Dom, laid: &LaidOutBox, tag: &str, index: usize) -> Rect {
    let mut nodes = Vec::new();
    find_all_tags(dom, dom.document(), tag, &mut nodes);
    let node = nodes[index];
    find_laid_out(laid, node)
        .unwrap_or_else(|| panic!("<{tag}>[{index}] should be laid out"))
        .layout
        .content
}

// ===== Non-replaced elements =====

#[test]
fn height_is_derived_from_width_and_the_ratio() {
    let (dom, laid) = layout(
        "<div>banner</div>",
        "body { margin: 0; } div { width: 300px; aspect-ratio: 3 / 1; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 300.0);
    assert_eq!(content.height, 100.0);
}

#[test]
fn a_ratio_without_a_denominator_is_over_one() {
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { width: 300px; aspect-ratio: 2; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).height, 150.0);
}

#[test]
fn a_percentage_width_also_derives_the_height() {
    let containing_width = PageSettings::default().content_width();
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { width: 50%; aspect-ratio: 4 / 1; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    let expected = containing_width * 0.5 / 4.0;
    assert!(
        (content.height - expected).abs() < 0.01,
        "expected {expected}, got {}",
        content.height
    );
}

#[test]
fn an_explicit_height_wins_over_the_ratio() {
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { width: 300px; height: 40px; aspect-ratio: 1 / 1; }",
    );
    assert_eq!(content_box(&dom, &laid, "div", 0).height, 40.0);
}

#[test]
fn max_height_clamps_the_derived_height() {
    // The order is "derive from the ratio, then clamp" (no recomputation to preserve the ratio).
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { width: 300px; aspect-ratio: 1 / 1; max-height: 50px; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 300.0);
    assert_eq!(content.height, 50.0);
}

#[test]
fn a_block_level_auto_width_stays_stretched() {
    // As the CSS spec requires, `width: auto` on a normal-flow block prefers stretch and does
    // not derive the width from the ratio.
    let containing_width = PageSettings::default().content_width();
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { height: 50px; aspect-ratio: 1 / 1; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, containing_width);
    assert_eq!(content.height, 50.0);
}

#[test]
fn the_ratio_applies_to_the_border_box_under_border_box_sizing() {
    // A border box of 200x100 (a 2:1 ratio) gives a content height of 100 minus padding/border.
    let (dom, laid) = layout(
        "<div>x</div>",
        "body { margin: 0; } div { box-sizing: border-box; width: 200px; \
         aspect-ratio: 2 / 1; padding: 10px; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 180.0);
    assert_eq!(content.height, 80.0);
}

// ===== A shrink-to-fit context (deriving the width from the height) =====

#[test]
fn a_float_derives_its_width_from_the_height_and_ratio() {
    let (dom, laid) = layout(
        r#"<div class="f"></div><p>text</p>"#,
        "body { margin: 0; } .f { float: left; height: 60px; aspect-ratio: 2 / 1; }",
    );
    let content = content_box(&dom, &laid, "div", 0);
    assert_eq!(content.width, 120.0);
    assert_eq!(content.height, 60.0);
}

#[test]
fn an_inline_block_derives_its_width_from_the_height_and_ratio() {
    let (dom, laid) = layout(
        r#"<p><span class="ib"></span></p>"#,
        "body { margin: 0; } .ib { display: inline-block; height: 30px; aspect-ratio: 3 / 1; }",
    );
    assert_eq!(content_box(&dom, &laid, "span", 0).width, 90.0);
}

#[test]
fn an_absolutely_positioned_box_derives_its_width_from_the_height_and_ratio() {
    let dom = html::parse(br#"<div class="host"><div class="badge"></div></div>"#);
    let styles = compute_styles(
        &dom,
        &user_agent_stylesheet(),
        &parse_stylesheet(
            "body { margin: 0; } .host { position: relative; height: 200px; } \
             .badge { position: absolute; top: 0; left: 0; height: 40px; aspect-ratio: 4 / 1; }",
        ),
    );
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let found = pages
        .iter()
        .flat_map(|page| page.boxes.iter())
        .find_map(|b| find_laid_out(b, divs[1]))
        .expect("absolutely positioned badge should be laid out");
    assert_eq!(found.layout.content.width, 160.0);
}

// ===== `<img>` (a replaced element) =====

#[test]
fn an_image_with_only_a_css_width_keeps_its_intrinsic_ratio() {
    // The fixture is 32x24 (4:3). This case used to lose the ratio and give a height of 0.
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, "body { margin: 0; } img { width: 160px; }");
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!(content.width, 160.0);
    assert_eq!(content.height, 120.0);
}

#[test]
fn an_image_with_only_a_css_height_keeps_its_intrinsic_ratio() {
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, "body { margin: 0; } img { height: 48px; }");
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!(content.width, 64.0);
    assert_eq!(content.height, 48.0);
}

#[test]
fn a_percentage_width_image_keeps_its_intrinsic_ratio() {
    let containing_width = PageSettings::default().content_width();
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, "body { margin: 0; } img { width: 100%; }");
    let content = content_box(&dom, &laid, "img", 0);
    assert!(
        (content.width - containing_width).abs() < 0.01,
        "expected full width, got {}",
        content.width
    );
    let expected = containing_width * 24.0 / 32.0;
    assert!(
        (content.height - expected).abs() < 0.01,
        "expected {expected}, got {}",
        content.height
    );
}

#[test]
fn an_explicit_ratio_overrides_the_intrinsic_ratio_of_an_image() {
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } img { width: 100px; aspect-ratio: 1 / 1; }",
    );
    assert_eq!(content_box(&dom, &laid, "img", 0).height, 100.0);
}

#[test]
fn the_auto_keyword_makes_an_image_prefer_its_intrinsic_ratio() {
    // `aspect-ratio: auto 1/1` means "prefer the intrinsic ratio if there is one".
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } img { width: 160px; aspect-ratio: auto 1 / 1; }",
    );
    assert_eq!(content_box(&dom, &laid, "img", 0).height, 120.0);
}

#[test]
fn an_image_without_any_css_size_still_uses_its_intrinsic_size() {
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, "body { margin: 0; }");
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!(content.width, 32.0);
    assert_eq!(content.height, 24.0);
}

#[test]
fn an_explicit_ratio_applies_to_an_image_without_any_css_size() {
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } img { aspect-ratio: 1 / 1; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!(content.width, 32.0, "intrinsic width is kept");
    assert_eq!(content.height, 32.0, "height follows the specified ratio");
}

// ===== flex =====

#[test]
fn a_flex_item_uses_the_ratio() {
    let (dom, laid) = layout(
        r#"<div class="row"><div class="a"></div></div>"#,
        "body { margin: 0; } .row { display: flex; width: 400px; } \
         .a { width: 120px; aspect-ratio: 2 / 1; }",
    );
    let content = content_box(&dom, &laid, "div", 1);
    assert_eq!(content.width, 120.0);
    assert_eq!(content.height, 60.0);
}

// ===== E2E =====

#[test]
fn aspect_ratio_renders_a_valid_pdf_end_to_end() {
    let html_src = format!(
        r#"<div class="banner">3:1 banner</div>
           <img class="hero" src="{}">
           <div class="row"><div class="tile"></div></div>"#,
        jpeg_data_uri()
    );
    let css = "body { margin: 0; } \
        .banner { width: 300px; aspect-ratio: 3 / 1; background-color: #cde; } \
        .hero { width: 100%; } \
        .row { display: flex; width: 400px; } \
        .tile { width: 100px; aspect-ratio: 1 / 1; background-color: #edc; }";

    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
}
