//! E2E tests for `aspect-ratio`.
//!
//! The same approach as `min_max_size.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, layout, PDF encode).

use std::collections::HashMap;

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::layout::{
    build_box_tree, layout_document, paginate_document, resolve_images, LaidOutBox, LaidOutContent,
    PageSettings, Rect,
};
use sghtmltopdf::pdf::{encode_pdf, ImageAssetCache};
use sghtmltopdf::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

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
    resolve_images(&mut tree, &dom, &styles, &cache);
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

// ===== `<img>` under min-*/max-* (CSS2.2 section 10.4) =====

fn image_size(css: &str) -> (f32, f32) {
    let html_src = format!(r#"<img src="{}">"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, &format!("body {{ margin: 0; }} {css}"));
    let content = content_box(&dom, &laid, "img", 0);
    (content.width, content.height)
}

#[test]
fn max_width_on_an_image_shrinks_the_height_with_it() {
    assert_eq!(image_size("img { max-width: 16px; }"), (16.0, 12.0));
}

#[test]
fn max_height_on_an_image_shrinks_the_width_with_it() {
    assert_eq!(image_size("img { max-height: 12px; }"), (16.0, 12.0));
}

#[test]
fn min_width_on_an_image_grows_the_height_with_it() {
    assert_eq!(image_size("img { min-width: 64px; }"), (64.0, 48.0));
}

#[test]
fn min_height_on_an_image_grows_the_width_with_it() {
    assert_eq!(image_size("img { min-height: 48px; }"), (64.0, 48.0));
}

#[test]
fn the_tighter_of_max_width_and_max_height_decides_the_image_size() {
    // Both are exceeded. The height limit is the tighter one (6/24 < 16/32).
    assert_eq!(
        image_size("img { max-width: 16px; max-height: 6px; }"),
        (8.0, 6.0)
    );
    // The width limit is the tighter one (8/32 < 12/24).
    assert_eq!(
        image_size("img { max-width: 8px; max-height: 12px; }"),
        (8.0, 6.0)
    );
}

#[test]
fn a_conflicting_limit_on_the_other_axis_wins_over_the_ratio() {
    // min-width alone would give 64x48, but max-height caps the height (the ratio is broken).
    assert_eq!(
        image_size("img { min-width: 64px; max-height: 30px; }"),
        (64.0, 30.0)
    );
}

#[test]
fn an_image_within_its_limits_keeps_its_intrinsic_size() {
    assert_eq!(
        image_size("img { max-width: 240px; max-height: 240px; }"),
        (32.0, 24.0)
    );
}

#[test]
fn padding_does_not_eat_into_an_image_under_border_box_sizing() {
    // The intrinsic size is the content box whatever `box-sizing` says.
    assert_eq!(
        image_size("img { box-sizing: border-box; padding: 2px; }"),
        (32.0, 24.0)
    );
}

#[test]
fn max_limits_on_an_image_are_border_box_relative_under_border_box_sizing() {
    // max-width: 20px includes padding 2px on each side, so the content box is 16px wide.
    assert_eq!(
        image_size("img { box-sizing: border-box; padding: 2px; max-width: 20px; }"),
        (16.0, 12.0)
    );
}

#[test]
fn a_flex_item_image_keeps_its_ratio_under_max_height() {
    let html_src = format!(r#"<div class="row"><img src="{}"></div>"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .row { display: flex; flex-wrap: wrap; } \
         img { max-width: 240px; max-height: 12px; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (16.0, 12.0));
}

#[test]
fn a_flex_item_image_keeps_its_ratio_under_a_percentage_max_width() {
    let html_src = format!(r#"<div class="row"><img src="{}"></div>"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .row { display: flex; } img { max-width: 50%; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (32.0, 24.0));
}

#[test]
fn a_percentage_max_width_on_a_flex_item_image_resolves_against_the_container() {
    // 50% of the 40px container is 20px, under the intrinsic 32px.
    let html_src = format!(r#"<div class="row"><img src="{}"></div>"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .row { display: flex; width: 40px; } img { max-width: 50%; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (20.0, 15.0));
}

#[test]
fn a_grid_item_image_keeps_its_ratio_under_a_percentage_max_width() {
    // 50% of the 100px column is 50px, over the intrinsic 32px.
    let html_src = format!(r#"<div class="grid"><img src="{}"></div>"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .grid { display: grid; grid-template-columns: 100px 100px; } \
         img { max-width: 50%; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (32.0, 24.0));
}

#[test]
fn a_grid_item_image_is_not_stretched_to_a_taller_row() {
    let html_src = format!(
        r#"<div class="grid"><img src="{}"><div class="tall"></div></div>"#,
        jpeg_data_uri()
    );
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .grid { display: grid; grid-template-columns: 100px 100px; } \
         .tall { height: 80px; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (32.0, 24.0));
}

#[test]
fn an_inline_image_keeps_its_ratio_under_max_height() {
    let html_src = format!(r#"<p>text <img src="{}"> text</p>"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, "body { margin: 0; } img { max-height: 12px; }");
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (16.0, 12.0));
}

#[test]
fn max_height_keeps_the_ratio_of_an_image_with_a_css_height() {
    assert_eq!(
        image_size("img { height: 24px; max-height: 12px; }"),
        (16.0, 12.0)
    );
}

// ===== `<img>` sized by its `width`/`height` attributes (#86) =====

fn attr_image_size(attrs: &str, css: &str) -> (f32, f32) {
    let html_src = format!(r#"<img src="{}" {attrs}>"#, jpeg_data_uri());
    let (dom, laid) = layout(&html_src, &format!("body {{ margin: 0; }} {css}"));
    let content = content_box(&dom, &laid, "img", 0);
    (content.width, content.height)
}

#[test]
fn a_width_attribute_derives_the_height_from_the_intrinsic_ratio() {
    assert_eq!(attr_image_size(r#"width="64""#, ""), (64.0, 48.0));
}

#[test]
fn a_height_attribute_derives_the_width_from_the_intrinsic_ratio() {
    assert_eq!(attr_image_size(r#"height="48""#, ""), (64.0, 48.0));
}

#[test]
fn both_size_attributes_win_over_the_intrinsic_ratio() {
    assert_eq!(
        attr_image_size(r#"width="10" height="10""#, ""),
        (10.0, 10.0)
    );
}

#[test]
fn max_width_keeps_the_ratio_of_an_image_sized_by_its_width_attribute() {
    assert_eq!(
        attr_image_size(r#"width="32""#, "img { max-width: 16px; }"),
        (16.0, 12.0)
    );
    assert_eq!(
        attr_image_size(r#"width="32""#, "img { max-width: 16px; height: auto; }"),
        (16.0, 12.0)
    );
}

#[test]
fn height_auto_overrides_the_height_attribute() {
    assert_eq!(
        attr_image_size(
            r#"width="32" height="24""#,
            "img { max-width: 16px; height: auto; }"
        ),
        (16.0, 12.0)
    );
}

#[test]
fn without_height_auto_the_height_attribute_stays_in_effect() {
    // Same as browsers: only the width is clamped.
    assert_eq!(
        attr_image_size(r#"width="32" height="24""#, "img { max-width: 16px; }"),
        (16.0, 24.0)
    );
}

#[test]
fn max_height_keeps_the_ratio_of_an_image_sized_by_its_height_attribute() {
    assert_eq!(
        attr_image_size(r#"height="24""#, "img { max-height: 12px; }"),
        (16.0, 12.0)
    );
}

#[test]
fn a_width_attribute_is_kept_when_css_sets_only_the_height() {
    assert_eq!(
        attr_image_size(r#"width="40""#, "img { height: 60px; }"),
        (40.0, 60.0)
    );
}

#[test]
fn the_responsive_image_pattern_scales_an_image_with_size_attributes_down() {
    let html_src = format!(
        r#"<div class="narrow"><img src="{}" width="32" height="24"></div>"#,
        jpeg_data_uri()
    );
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .narrow { width: 16px; } img { max-width: 100%; height: auto; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (16.0, 12.0));
}

#[test]
fn a_percentage_width_attribute_resolves_against_the_containing_block() {
    let html_src = format!(
        r#"<div class="box"><img src="{}" width="50%"></div>"#,
        jpeg_data_uri()
    );
    let (dom, laid) = layout(&html_src, "body { margin: 0; } .box { width: 40px; }");
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (20.0, 15.0));
}

#[test]
fn a_flex_item_image_sized_by_its_attributes_keeps_its_ratio_under_max_width() {
    let html_src = format!(
        r#"<div class="row"><img src="{}" width="32" height="24"></div>"#,
        jpeg_data_uri()
    );
    let (dom, laid) = layout(
        &html_src,
        "body { margin: 0; } .row { display: flex; } img { max-width: 16px; height: auto; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (16.0, 12.0));
}

#[test]
fn a_failed_image_still_reserves_the_space_of_its_size_attributes() {
    let (dom, laid) = layout(
        r#"<img src="data:image/png;base64,AAAA" width="50" height="50">"#,
        "body { margin: 0; }",
    );
    let content = content_box(&dom, &laid, "img", 0);
    assert_eq!((content.width, content.height), (50.0, 50.0));
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
