//! E2E tests for `box-shadow`.
//!
//! The same approach as `box_sizing.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode).

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

/// Return every `stream` to `endstream` region in the PDF bytes, inflated and concatenated.
/// Each stream's `/Length N` is parsed and exactly `N` bytes are taken
/// (an implementation naively searching for `\nendstream` cuts in the wrong place when those
/// bytes happen to occur inside an embedded font binary, so this uses the same exact
/// implementation as the identically named helper in `engine.rs`'s test module).
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_subslice(&pdf_bytes[i..], b"/Length ") {
        let len_start = i + pos + b"/Length ".len();
        let mut len_end = len_start;
        while len_end < pdf_bytes.len() && pdf_bytes[len_end].is_ascii_digit() {
            len_end += 1;
        }
        let Some(length) = std::str::from_utf8(&pdf_bytes[len_start..len_end])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            i = len_end.max(i + pos + 1);
            continue;
        };
        let Some(stream_rel) = find_subslice(&pdf_bytes[len_end..], b"stream\n") else {
            break;
        };
        let data_start = len_end + stream_rel + b"stream\n".len();
        let data_end = data_start + length;
        if data_end > pdf_bytes.len() {
            i = len_end;
            continue;
        }
        let raw = &pdf_bytes[data_start..data_end];

        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        } else {
            out.extend_from_slice(raw);
        }
        out.push(b'\n');

        i = data_end;
    }
    out
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
fn box_shadow_adds_extra_fill_drawing_before_the_background_end_to_end() {
    let with_shadow = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; background-color: white; \
                box-shadow: 4px 4px 8px rgba(0, 0, 0, 0.5); }",
    );
    let without_shadow = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; background-color: white; }",
    );
    assert!(
        with_shadow.len() > without_shadow.len(),
        "box-shadow should add extra drawing operators to the content stream"
    );
}

#[test]
fn box_shadow_none_draws_nothing_extra_end_to_end() {
    let with_none = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: none; }",
    );
    let without_declaration = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } .box { width: 100px; height: 60px; }",
    );
    assert_eq!(with_none, without_declaration);
}

#[test]
fn box_shadow_inset_is_parsed_but_not_rendered_end_to_end() {
    // `inset` parses but drawing it is not supported (a known simplification).
    let with_inset = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: inset 4px 4px 8px rgba(0,0,0,0.5); }",
    );
    let without_declaration = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } .box { width: 100px; height: 60px; }",
    );
    assert_eq!(with_inset, without_declaration);
}

#[test]
fn box_shadow_with_zero_blur_draws_exactly_one_rect_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: 4px 4px rgb(0, 0, 0); }",
    );
    let decompressed = decompressed_stream_bytes(&bytes);
    // With no blur (blur-radius: 0) the concentric rectangle approximation loop is skipped
    // and only the core rectangle is drawn. `rounded_rect_path` uses a rounded path
    // (`m`/`l`/`c`/`h`), so the number drawn can be counted from the occurrences of
    // `close_path` plus `fill_nonzero` (`h\nf\n`). The div itself has no background-color, so
    // this occurrence should be the single one from the box-shadow.
    assert_eq!(count_occurrences(&decompressed, b"h\nf\n"), 1);
}

#[test]
fn box_shadow_with_blur_draws_multiple_concentric_rects_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: 0 0 20px rgba(0, 0, 0, 0.5); }",
    );
    let decompressed = decompressed_stream_bytes(&bytes);
    // The blur approximation is 4 rings plus the core rectangle = 5.
    assert_eq!(count_occurrences(&decompressed, b"h\nf\n"), 5);
}

#[test]
fn box_shadow_comma_separated_list_draws_each_shadow_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; \
                box-shadow: 2px 2px rgb(255,0,0), 4px 4px rgb(0,0,255); }",
    );
    let decompressed = decompressed_stream_bytes(&bytes);
    // Each shadow has blur-radius: 0 (one core rectangle), so two together make 2.
    assert_eq!(count_occurrences(&decompressed, b"h\nf\n"), 2);
}

#[test]
fn box_shadow_and_border_radius_render_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; border-radius: 12px; \
                box-shadow: 4px 4px 8px rgba(0, 0, 0, 0.4); }",
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
