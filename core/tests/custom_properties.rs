//! E2E tests for CSS Custom Properties (`--foo`/`var()`).
//!
//! The same approach as `box_sizing.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode).

use std::path::PathBuf;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::img::{DocumentImageCache, ImageFetcher};
use sghtmltopdf_core::layout::{build_box_tree, layout_document, paginate_document, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, extract_author_stylesheet, user_agent_stylesheet};

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

fn no_remote_fetcher() -> ImageFetcher {
    ImageFetcher::new(PathBuf::from("."), false)
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

fn find_laid_out(
    b: &sghtmltopdf_core::layout::LaidOutBox,
    target: NodeId,
) -> Option<&sghtmltopdf_core::layout::LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    if let sghtmltopdf_core::layout::LaidOutContent::Blocks(children) = &b.content {
        for child in children {
            if let Some(found) = find_laid_out(child, target) {
                return Some(found);
            }
        }
    }
    None
}

/// Build a single DOM with the css embedded in a `<style>` tag. Resolving `var()` and custom
/// properties is `extract_author_stylesheet`'s job (a DOM walk plus text substitution)
/// rather than `parse_stylesheet`'s, so the helpers in this test file never hand a one-off
/// CSS string straight to `parse_stylesheet` and always go through this path.
fn dom_with_style(html_body: &str, css: &str) -> Dom {
    html::parse(
        format!("<html><head><style>{css}</style></head><body>{html_body}</body></html>")
            .as_bytes(),
    )
}

fn extract_stylesheet(dom: &Dom) -> sghtmltopdf_core::style::Stylesheet {
    let fetcher = no_remote_fetcher();
    let cache = DocumentImageCache::new();
    extract_author_stylesheet(dom, &fetcher, &cache)
}

fn layout(html_body: &str, css: &str) -> (Dom, sghtmltopdf_core::layout::LaidOutBox) {
    let dom = dom_with_style(html_body, css);
    let author = extract_stylesheet(&dom);
    let ua = user_agent_stylesheet();
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

fn build_pdf(html_body: &str, css: &str) -> Vec<u8> {
    let dom = dom_with_style(html_body, css);
    let author = extract_stylesheet(&dom);
    let ua = user_agent_stylesheet();
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(
        &pages,
        &styles,
        &std::collections::HashMap::new(),
        &fonts,
        &settings,
    );

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn var_resolves_inside_a_layout_property() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --box-width: 120px; } \
         body { margin: 0; } \
         .box { width: var(--box-width); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 120.0);
}

#[test]
fn a_custom_property_referencing_another_one_resolves_transitively() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --base: 40px; --double: var(--base); } \
         body { margin: 0; } \
         .box { width: var(--double); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 40.0);
}

#[test]
fn fallback_value_is_used_when_the_custom_property_is_undefined() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        "body { margin: 0; } \
         .box { width: var(--undefined, 90px); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 90.0);
}

#[test]
fn an_undefined_var_without_a_fallback_leaves_the_declaration_ignored() {
    // An undefined `var` with no fallback leaves its text unsubstituted, which falls
    // naturally into the existing "an unsupported or invalid declaration is ignored" path,
    // so `width` counts as unset (auto).
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        "body { margin: 0; } \
         .box { width: var(--undefined); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    // Being auto, the content width stretches to the parent's available width (it does not collapse to 0px).
    assert!(div.layout.content.width > 90.0);
}

#[test]
fn later_declaration_wins_across_the_whole_document_regardless_of_selector_scope() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --w: 50px; } \
         .box { --w: 75px; } \
         body { margin: 0; } \
         .box { width: var(--w); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 75.0);
}

#[test]
fn custom_properties_declared_in_one_style_tag_resolve_in_another() {
    // extract_author_stylesheet concatenates several <style> tags before substituting, so a
    // `--foo` declared in one tag should be reachable from a `var(--foo)` in another
    // (one flat namespace across the document).
    let dom = html::parse(
        br#"<html><head>
            <style>:root { --brand-width: 64px; }</style>
            <style>body { margin: 0; } .box { width: var(--brand-width); }</style>
            </head><body><div class="box">content</div></body></html>"#,
    );
    let author = extract_stylesheet(&dom);
    let ua = user_agent_stylesheet();
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
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 64.0);
}

#[test]
fn extract_author_stylesheet_resolves_var_via_the_style_tag_helper() {
    let dom = dom_with_style(
        "<div class=\"box\">content</div>",
        ":root { --w: 33px; } .box { width: var(--w); }",
    );
    let sheet = extract_stylesheet(&dom);
    // Custom property declarations are already resolved by text substitution, so the `:root`
    // rule is discarded as having no declarations and only the one for `.box` survives.
    assert_eq!(sheet.rules.len(), 1);
}

#[test]
fn var_inside_nested_calc_resolves_the_tailwind_space_y_shape() {
    // issue #17: Tailwind v4's `space-y-*`/`divide-*` emit
    // `calc(calc(var(--spacing) * N) * calc(1 - var(--tw-space-y-reverse)))`
    // 15px * 6 * (1 - 0) = 90px.
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --spacing: 15px; --reverse: 0; } \
         body { margin: 0; } \
         .box { margin-left: calc(calc(var(--spacing) * 6) * calc(1 - var(--reverse))); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let b = find_laid_out(&laid, divs[0]).unwrap();
    assert!(
        (b.layout.margin.left - 90.0).abs() < 0.5,
        "margin-left should be 90 but was {}",
        b.layout.margin.left
    );
}

#[test]
fn custom_properties_render_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">custom properties test</div>"#,
        ":root { --gap: 15px; --color: rgb(10, 20, 30); } \
         body { margin: 0; } \
         .box { padding: var(--gap); background-color: var(--color); }",
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
