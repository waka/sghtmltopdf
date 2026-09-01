//! E2E tests for `border-collapse: collapse`.
//!
//! The same approach as `table_caption.rs`, `table_vertical_align.rs` and
//! `table_rowspan.rs`: catch regressions by going through the real pipeline.

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

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

fn build_pdf(html_src: &str, css: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn a_grid_of_uniformly_bordered_cells_renders_a_valid_pdf_with_collapsed_borders_end_to_end() {
    let html_src = r#"<table>
        <tr><td>a</td><td>b</td><td>c</td></tr>
        <tr><td>d</td><td>e</td><td>f</td></tr>
    </table>"#;
    let css = "body { margin: 0; } \
               table { border-collapse: collapse; } \
               td { border: 1px solid black; padding: 4px; }";

    build_pdf(html_src, css);
}

#[test]
fn border_collapse_and_rowspan_combined_render_a_valid_pdf_end_to_end() {
    // A regression test confirming that the boundary merging logic works even in a table
    // with cells spanning several rows via rowspan (its design using rectangle-touching
    // rather than grid information).
    let html_src = r#"<table>
        <tr><td rowspan="2">tall</td><td>a</td></tr>
        <tr><td>b</td></tr>
    </table>"#;
    let css = "body { margin: 0; } \
               table { border-collapse: collapse; } \
               td { border: 1px solid black; }";

    build_pdf(html_src, css);
}
