//! End-to-end tests for `position: relative` (waka/sghtmltopdf#29).
//!
//! The offset has to move the text, images, `<span>`s and child blocks as well as
//! the background and border. These run the same document as the reproduction in
//! the issue (`@page { margin: 0.5in }`, `* { margin: 0; padding: 0 }`) all the
//! way through pagination, and check that the decoration rectangle (border box)
//! and the coordinates of the contents agree.

use std::path::PathBuf;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::img::{DocumentImageCache, ImageFetcher};
use sghtmltopdf_core::layout::{paginate_document, LaidOutBox, LaidOutContent, PageSettings};
use sghtmltopdf_core::style::{compute_styles, extract_author_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id).find_map(|child| find(dom, child, tag))
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

/// Lays out the same document as the `probe` in the issue up to the first page,
/// and hands the layout of the first `tag` element to `f`.
fn with_probe<T>(css: &str, body: &str, tag: &str, f: impl FnOnce(&LaidOutBox) -> T) -> T {
    let html_src = format!(
        "<!DOCTYPE html><html><head><style>\
         @page {{ size: letter; margin: 0.5in }} * {{ margin: 0; padding: 0 }} {css}\
         </style></head><body>{body}</body></html>"
    );
    let dom = html::parse(html_src.as_bytes());
    let fetcher = ImageFetcher::new(PathBuf::from("."), false);
    let cache = DocumentImageCache::new();
    let author = extract_author_stylesheet(&dom, &fetcher, &cache);
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);
    let fonts = test_fonts();
    let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());
    let node = find(&dom, dom.document(), tag).expect("probe element not found");
    let b = pages[0]
        .boxes
        .iter()
        .find_map(|b| find_laid_out(b, node))
        .expect("probe element not laid out on page 1");
    f(b)
}

fn first_line(b: &LaidOutBox) -> &sghtmltopdf_core::layout::LineBox {
    let LaidOutContent::Inline(lines) = &b.content else {
        panic!("expected inline content");
    };
    &lines[0]
}

const OFFSET: f32 = 120.0;

#[test]
fn text_moves_with_the_background_of_a_relative_block() {
    with_probe(
        ".b { position: relative; left: 120px; background: #ffd; width: 160px }",
        r#"<div class="b">TEXT</div>"#,
        "div",
        |b| {
            let line = first_line(b);
            assert_eq!(b.layout.border_box().x, OFFSET);
            assert_eq!(line.rect.x, b.layout.content.x);
        },
    );
}

#[test]
fn text_moves_with_the_border_of_a_relative_block() {
    with_probe(
        ".b { position: relative; left: 120px; border: 2px solid #000; width: 160px }",
        r#"<div class="b">TEXT</div>"#,
        "div",
        |b| {
            assert_eq!(b.layout.border_box().x, OFFSET);
            assert_eq!(first_line(b).rect.x, OFFSET + 2.0);
        },
    );
}

#[test]
fn a_nested_block_child_moves_with_its_relative_parent() {
    with_probe(
        ".b { position: relative; left: 120px; background: #ffd; width: 160px }",
        r#"<div class="b"><p>TEXT</p></div>"#,
        "p",
        |p| {
            assert_eq!(p.layout.content.x, OFFSET);
            assert_eq!(first_line(p).rect.x, OFFSET);
        },
    );
}

#[test]
fn an_image_moves_with_its_relative_parent() {
    with_probe(
        ".b { position: relative; left: 120px; width: 160px }",
        r#"<div class="b"><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=" style="width:40px;height:40px"></div>"#,
        "div",
        |b| {
            let line = first_line(b);
            assert_eq!(line.rect.x, OFFSET);
            let img = line
                .atomics
                .first()
                .expect("img should be an atomic inline");
            assert_eq!(img.content.layout.content.x, OFFSET);
        },
    );
}

#[test]
fn top_moves_the_text_with_the_background() {
    with_probe(
        ".b { position: relative; top: 40px; background: #ffd; width: 160px }",
        r#"<div class="b">TEXT</div>"#,
        "div",
        |b| {
            assert_eq!(b.layout.border_box().y, 40.0);
            assert_eq!(first_line(b).rect.y, 40.0);
        },
    );
}

#[test]
fn a_relative_span_moves_its_own_text_only() {
    with_probe(
        ".s { position: relative; left: 120px }",
        r#"<div>A <span class="s">TEXT</span></div>"#,
        "div",
        |b| {
            let line = first_line(b);
            let a = line.runs.iter().find(|r| r.text == "A").expect("run A");
            let text = line
                .runs
                .iter()
                .find(|r| r.text == "TEXT")
                .expect("run TEXT");
            assert_eq!(a.x_offset, 0.0);
            // The run would normally sit right after "A " (a.width plus the
            // space); it moves 120px to the right.
            assert!(
                text.x_offset > a.width + OFFSET && text.x_offset < a.width + OFFSET + 10.0,
                "TEXT run should sit 120px right of its normal position, got {}",
                text.x_offset
            );
        },
    );
}
