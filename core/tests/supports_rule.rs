//! End-to-end tests for `@supports` (#89): extraction from `<style>` -> cascade -> layout.

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

/// `.c`'s left edge for the stylesheet, with margins reset.
fn probe_x(css: &str) -> f32 {
    let (dom, laid) = layout(
        r#"<div class="c">X</div>"#,
        &format!("* {{ margin: 0; padding: 0 }} {css}"),
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    find_laid_out(&laid, divs[0]).unwrap().layout.content.x
}

/// The reproduction from issue #89.
#[test]
fn rule_inside_a_true_supports_block_is_applied() {
    let css = "@supports (display: grid) { .c { --x: 9px } } \
               .c { margin-left: var(--x, 1px) }";
    assert_eq!(probe_x(css), 9.0);
}

#[test]
fn rule_inside_a_false_supports_block_is_not_applied() {
    for cond in [
        "not (display: grid)",
        "(display: nonsense-value)",
        "font-tech(color-COLRv1)",
    ] {
        let css =
            format!("@supports {cond} {{ .c {{ --x: 9px }} }} .c {{ margin-left: var(--x, 1px) }}");
        assert_eq!(probe_x(&css), 1.0, "{cond}");
    }
}

#[test]
fn supports_nested_in_media_and_layer_is_applied() {
    let css = "@layer a { @media print { @supports (gap: 1px) { .c { margin-left: 7px } } } }";
    assert_eq!(probe_x(css), 7.0);
}
