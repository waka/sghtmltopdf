//! End-to-end tests for `@layer` (cascade layers) (#20).
//!
//! Tailwind v4 wraps its entire output in a `@layer` block, so dropping the
//! block leaves the document completely unstyled. Layer precedence is not
//! implemented; the rules inside are hoisted to the top level in source order.
//!
//! Same approach as `custom_properties.rs`: catch regressions through the real
//! path, extraction from `<style>` -> cascade -> layout.

use std::path::PathBuf;

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::img::{DocumentImageCache, ImageFetcher};
use sghtmltopdf::layout::{
    build_box_tree, layout_document, LaidOutBox, LaidOutContent, PageSettings,
};
use sghtmltopdf::style::{compute_styles, extract_author_stylesheet, user_agent_stylesheet};

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

fn layout(html_body: &str, css: &str) -> (Dom, LaidOutBox) {
    let dom = html::parse(
        format!("<html><head><style>{css}</style></head><body>{html_body}</body></html>")
            .as_bytes(),
    );
    let fetcher = ImageFetcher::new(PathBuf::from("."), false);
    let cache = DocumentImageCache::new();
    let author = extract_author_stylesheet(&dom, &fetcher, &cache);
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

/// The reproduction from issue #20 itself. `.probe { margin-left: 90px }` in
/// three forms, plain, wrapped in `@layer utilities { }`, and placed after
/// `@layer base, utilities;`, must put the left edge of `X` in the same place.
fn probe_x(css: &str) -> f32 {
    let (dom, laid) = layout(
        r#"<div class="probe">X</div>"#,
        &format!("* {{ margin: 0; padding: 0 }} {css}"),
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    find_laid_out(&laid, divs[0]).unwrap().layout.content.x
}

#[test]
fn rule_inside_a_layer_block_is_applied() {
    let plain = probe_x(".probe { margin-left: 90px }");
    assert_eq!(plain, 90.0);
    assert_eq!(
        probe_x("@layer utilities { .probe { margin-left: 90px } }"),
        plain,
        "a rule wrapped in @layer must be applied like a plain rule"
    );
}

#[test]
fn layer_statement_does_not_poison_subsequent_rules() {
    assert_eq!(
        probe_x("@layer base, utilities; .probe { margin-left: 90px }"),
        90.0
    );
}

/// A shrunken version of the shape Tailwind v4 emits: an order statement, several
/// layer blocks, custom properties on `:root`, and a nested `@media`. Also checks
/// that the text substitution of `var()` still works inside a `@layer`.
#[test]
fn tailwind_v4_shaped_bundle_is_applied() {
    let css = r#"
        @layer theme, base, components, utilities;
        @layer theme {
          :root { --spacing: 0.25rem; --color-red: rgb(255, 0, 0); }
        }
        @layer base {
          *, ::before, ::after { box-sizing: border-box; margin: 0; padding: 0; }
        }
        @layer components;
        @layer utilities {
          .ml-8 { margin-left: calc(var(--spacing) * 8); }
          .w-40 { width: calc(var(--spacing) * 40); }
          @media print { .print\:w-20 { width: calc(var(--spacing) * 20); } }
        }
    "#;
    let (dom, laid) = layout(r#"<div class="ml-8 w-40 print:w-20">X</div>"#, css);
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    // 0.25rem = 4px; ml-8 = 32px; print:w-20 = 80px (last one wins, overriding
    // the 160px of w-40)
    assert_eq!(div.layout.content.x, 32.0);
    assert_eq!(div.layout.content.width, 80.0);
}
