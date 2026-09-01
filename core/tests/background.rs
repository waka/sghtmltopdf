//! E2E tests for `background-position`/`-size`/`-repeat`/`-attachment` and the `background`
//! shorthand.
//!
//! The same approach as `typography.rs`/`box_model.rs`: catch regressions by going through
//! the real pipeline (HTML parse, style cascade, background image decode, pagination, PDF
//! encode).

use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, resolve_background_images, PageSettings};
use sghtmltopdf_core::pdf::{encode_pdf, ImageAssetCache};
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const PNG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_opaque.png"
);

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

/// Encode `spike_opaque.png` (20x16, an existing fixture) into a data URI, so the path
/// really decoded by `ImageAssetCache` is exercised without depending on network or file
/// I/O.
fn png_data_uri() -> String {
    let bytes = std::fs::read(PNG_PATH).expect("fixture image should exist");
    format!("data:image/png;base64,{}", STANDARD.encode(bytes))
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// A PDF's content stream is FlateDecode compressed, so searching for an operator inside it
/// such as `Do` requires inflating first
/// (the same logic as the identically named function in the `pdf::document` test module).
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_subslice(&pdf_bytes[i..], b"stream\n") {
        let start = i + pos + b"stream\n".len();
        let Some(end_rel) = find_subslice(&pdf_bytes[start..], b"\nendstream") else {
            break;
        };
        let end = start + end_rel;
        let raw = &pdf_bytes[start..end];

        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        } else {
            out.extend_from_slice(raw);
        }
        i = end + b"\nendstream".len();
    }
    out
}

/// Run the whole real pipeline (parse, cascade, background image decode, pagination, PDF
/// encode) from HTML plus CSS.
fn build_pdf(html_src: &str, css: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    // Nothing is really accessed over the network or from a local file (only data URIs are
    // used), so base_dir can be anything.
    let image_cache = ImageAssetCache::new(PathBuf::from("."), false);
    let background_images = resolve_background_images(&styles, &image_cache);

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &background_images, &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn background_shorthand_with_cover_and_no_repeat_draws_a_single_tile_end_to_end() {
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 100px; height: 60px;
               background: url("{}") no-repeat center / cover;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(
        count_occurrences(&decompressed, b" Do\n"),
        1,
        "no-repeat should draw exactly one tile"
    );
}

#[test]
fn background_repeat_tiles_the_image_across_the_box_end_to_end() {
    // With `repeat` (the default) on a box (100x60) larger than the intrinsic size (20x16),
    // 5 columns (steps of 20) by 4 rows (steps of 16) = 20 tiles are laid.
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 100px; height: 64px;
               background-image: url("{}");
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 5 * 4);
}

#[test]
fn background_repeat_x_only_tiles_horizontally_end_to_end() {
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 60px; height: 16px;
               background-image: url("{}");
               background-repeat: repeat-x;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    // A width of 60 gives 3 columns in steps of 20, and only one row (repeat-x does not tile vertically).
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 3);
}

#[test]
fn background_size_percentage_and_position_percentage_render_a_valid_pdf_end_to_end() {
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 200px; height: 100px;
               background-image: url("{}");
               background-repeat: no-repeat;
               background-size: 50% 50%;
               background-position: 100% 100%;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 1);
}

#[test]
fn background_attachment_fixed_still_renders_like_scroll_end_to_end() {
    // `fixed` is treated the same as `scroll`, so it should draw one image as usual without crashing.

    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 100px; height: 60px;
               background-image: url("{}");
               background-attachment: fixed;
               background-repeat: no-repeat;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 1);
}

#[test]
fn all_background_details_combined_render_a_valid_pdf_end_to_end() {
    let uri = png_data_uri();
    let html_src = r#"
        <div class="cover">cover</div>
        <div class="contain">contain</div>
        <div class="tiled">tiled</div>
        <div class="shorthand">shorthand</div>
        "#;
    let css = format!(
        r#"body {{ margin: 0; }}
           div {{ width: 100px; height: 60px; }}
           .cover {{ background-image: url("{uri}"); background-size: cover; background-repeat: no-repeat; }}
           .contain {{ background-image: url("{uri}"); background-size: contain; background-repeat: no-repeat; background-position: center; }}
           .tiled {{ background-image: url("{uri}"); background-repeat: repeat-y; }}
           .shorthand {{ background: url("{uri}") no-repeat right bottom / contain; }}
        "#
    );
    let bytes = build_pdf(html_src, &css);
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
