//! E2E tests for percentage `height`/`min-height`/`max-height` (CSS 2.1 section 10.5).
//!
//! A percentage resolves against the height of its containing block when that height is
//! specified explicitly, and otherwise behaves as `auto` (`0` for `min-height`, `none` for
//! `max-height`).

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::layout::{
    build_box_tree, layout_document, paginate_document, resolve_images, LaidOutBox, LaidOutContent,
    PageSettings, Rect,
};
use sghtmltopdf::pdf::ImageAssetCache;
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

const RESET: &str = "* { margin: 0; padding: 0; box-sizing: border-box; } ";

fn h(dom: &Dom, laid: &LaidOutBox, tag: &str, index: usize) -> f32 {
    content_box(dom, laid, tag, index).height
}

#[test]
fn height_percentage_resolves_against_a_definite_height() {
    let (dom, laid) = layout(
        r#"<div class="box"><div class="fill"></div></div>"#,
        &format!("{RESET} .box {{ width: 400px; height: 200px; }} .fill {{ height: 100%; }}"),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 200.0);
}

#[test]
fn calc_with_a_percentage_height_resolves() {
    let (dom, laid) = layout(
        r#"<div class="box"><div class="fill"></div></div>"#,
        &format!("{RESET} .box {{ height: 200px; }} .fill {{ height: calc(100% - 20px); }}"),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 180.0);
}

#[test]
fn percentage_min_and_max_height_resolve() {
    let (dom, laid) = layout(
        r#"<div class="box"><div class="min"></div><div class="max">x<br>x<br>x<br>x<br>x<br>x<br>x<br>x<br>x<br>x</div></div>"#,
        &format!(
            "{RESET} .box {{ height: 200px; }} .min {{ min-height: 50%; }} .max {{ max-height: 25%; }}"
        ),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 100.0);
    assert_eq!(h(&dom, &laid, "div", 2), 50.0);
}

#[test]
fn definite_height_chains_through_percentages() {
    let (dom, laid) = layout(
        r#"<div class="a"><div class="b"><div class="c"></div></div></div>"#,
        &format!("{RESET} .a {{ height: 200px; }} .b {{ height: 50%; }} .c {{ height: 100%; }}"),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 100.0);
    assert_eq!(h(&dom, &laid, "div", 2), 100.0);
}

#[test]
fn percentage_height_resolves_against_the_content_box() {
    // The containing block is the content box: 200px high, less 2 * 20px padding and 2 * 5px
    // border under border-box sizing.
    let (dom, laid) = layout(
        r#"<div class="box"><div class="fill"></div></div>"#,
        &format!(
            "{RESET} .box {{ height: 200px; padding: 20px; border: 5px solid black; }} \
             .fill {{ height: 50%; }}"
        ),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 75.0);
}

#[test]
fn percentage_height_respects_the_childs_box_sizing() {
    let (dom, laid) = layout(
        r#"<div class="box"><div class="fill"></div></div>"#,
        &format!(
            "{RESET} .box {{ height: 200px; }} \
             .fill {{ height: 50%; padding: 10px 0; }}"
        ),
    );
    // border-box: 100px including padding.
    assert_eq!(h(&dom, &laid, "div", 1), 80.0);
    let (dom, laid) = layout(
        r#"<div class="box"><div class="fill"></div></div>"#,
        &format!(
            "{RESET} .box {{ height: 200px; }} \
             .fill {{ box-sizing: content-box; height: 50%; padding: 10px 0; }}"
        ),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 100.0);
}

#[test]
fn percentage_height_stays_auto_when_the_containing_block_height_is_auto() {
    let (dom, laid) = layout(
        r#"<div class="box"><div class="fill">text</div></div>"#,
        &format!("{RESET} .fill {{ height: 100%; min-height: 50%; max-height: 10%; }}"),
    );
    // `height` is auto, `min-height` is 0 and `max-height` is none, so the line height wins.
    let fill = h(&dom, &laid, "div", 1);
    assert!(fill > 0.0 && fill < 40.0, "got {fill}");
}

#[test]
fn percentage_height_resolves_through_an_auto_percentage_height_chain_as_auto() {
    // `.b` is auto (its parent `.a` has no height), so `.c` cannot resolve either.
    let (dom, laid) = layout(
        r#"<div class="a"><div class="b"><div class="c">text</div></div></div>"#,
        &format!("{RESET} .b {{ height: 50%; }} .c {{ height: 100%; }}"),
    );
    assert!(h(&dom, &laid, "div", 2) < 40.0);
}

#[test]
fn image_with_full_height_fits_inside_a_padded_border_box() {
    // A signature box: `h-48 p-2` holding `h-full w-full object-contain`.
    let html_src = format!(r#"<div class="sig"><img src="{}"></div>"#, jpeg_data_uri());
    let (dom, laid) = layout(
        &html_src,
        &format!(
            "{RESET} .sig {{ width: 300px; height: 192px; padding: 8px; }} \
             img {{ height: 100%; width: 100%; object-fit: contain; }}"
        ),
    );
    let sig = content_box(&dom, &laid, "div", 0);
    assert_eq!(sig.height, 176.0);
    let img = content_box(&dom, &laid, "img", 0);
    assert_eq!(img.height, 176.0);
    assert_eq!(img.width, 284.0);
}

#[test]
fn percentage_height_resolves_inside_a_float() {
    let (dom, laid) = layout(
        r#"<div class="f"><div class="fill"></div></div>"#,
        &format!(
            "{RESET} .f {{ float: left; width: 100px; height: 80px; }} .fill {{ height: 50%; }}"
        ),
    );
    assert_eq!(h(&dom, &laid, "div", 1), 40.0);
}

#[test]
fn flex_and_grid_items_keep_treating_percentage_height_as_auto() {
    // Stretched flex/grid items are not treated as definite (a documented limitation); the
    // percentage must not pick up the height of an ancestor block either.
    let (dom, laid) = layout(
        r#"<div class="box"><div class="row"><div class="item"><div class="fill">x</div></div></div></div>"#,
        &format!(
            "{RESET} .box {{ height: 300px; }} .row {{ display: flex; }} \
             .fill {{ height: 100%; }}"
        ),
    );
    assert!(h(&dom, &laid, "div", 3) < 40.0);
}

fn laid_out_in_pages(html_src: &str, css: &str, tag: &str, index: usize) -> LaidOutBox {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());
    let mut nodes = Vec::new();
    find_all_tags(&dom, dom.document(), tag, &mut nodes);
    let node = nodes[index];
    pages
        .iter()
        .flat_map(|page| page.boxes.iter())
        .find_map(|b| find_laid_out(b, node))
        .unwrap_or_else(|| panic!("<{tag}>[{index}] should be laid out"))
        .clone()
}

#[test]
fn absolute_box_resolves_percentage_height_against_the_padding_box() {
    let found = laid_out_in_pages(
        r#"<div class="host"><div class="abs"></div></div>"#,
        &format!(
            "{RESET} .host {{ position: relative; height: 200px; padding: 10px 0; }} \
             .abs {{ position: absolute; top: 0; left: 0; width: 10px; height: 50%; }}"
        ),
        "div",
        1,
    );
    // The padding box is 200px (border-box), so 50% is 100px.
    assert_eq!(found.layout.content.height, 100.0);
}

#[test]
fn root_percentage_heights_do_not_cut_the_document_off() {
    // `html, body { height: 100% }` is common. Multi-page content must still paginate.
    let dom = html::parse(format!("<p>{}</p>", "word ".repeat(6000)).as_bytes());
    let styles = compute_styles(
        &dom,
        &user_agent_stylesheet(),
        &parse_stylesheet("html, body { height: 100%; }"),
    );
    let fonts = test_fonts();
    let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());
    assert!(pages.len() > 1, "got {} pages", pages.len());
}
