//! E2E tests for `opacity`/`transform`.
//!
//! The same approach as `paged_media.rs`, using the `Engine` API directly and checking both
//! batch and streaming modes (`opacity` is implemented as a PDF transparency group, a Form
//! XObject with `/Group /S /Transparency`, so this confirms that `/Subtype /Form` and
//! `/Transparency` really appear in the generated PDF bytes).

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn font_spec() -> FontSpec {
    FontSpec {
        path: FONT_PATH.into(),
        index: 0,
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// A PDF's content stream is FlateDecode compressed, so searching for an operator inside it
/// such as `cm` requires inflating first
/// (the same logic as the identically named function in `paged_media.rs`).
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

fn build_pdf(html: &str, mode: Mode) -> Vec<u8> {
    let options = EngineOptions {
        mode,
        fonts: vec![font_spec()],
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    engine.feed(html.as_bytes()).unwrap();
    let bytes = engine.finish().unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn transform_emits_a_cm_operator_but_a_plain_box_does_not() {
    let with_transform = build_pdf(
        r#"<html><head><style>
             .box { width: 50px; height: 50px; background-color: red; transform: rotate(10deg); }
           </style></head><body><div class="box"></div></body></html>"#,
        Mode::Batch,
    );
    let plain = build_pdf(
        r#"<html><head><style>
             .box { width: 50px; height: 50px; background-color: red; }
           </style></head><body><div class="box"></div></body></html>"#,
        Mode::Batch,
    );
    let with_transform_content = decompressed_stream_bytes(&with_transform);
    let plain_content = decompressed_stream_bytes(&plain);
    // Every page starts with one CTM converting CSS px to pt, so even a page not using
    // `transform` emits one `cm`.
    // `transform` stacks another `cm` on top of that.
    let plain_cm = count_occurrences(&plain_content, b" cm\n");
    assert_eq!(plain_cm, 1, "only the page scale CTM should be present");
    assert!(
        count_occurrences(&with_transform_content, b" cm\n") > plain_cm,
        "transform must emit an additional cm operator"
    );
}

#[test]
fn opacity_below_one_creates_an_isolated_transparency_group_form_xobject() {
    let bytes = build_pdf(
        r#"<html><head><style>
             .box { width: 50px; height: 50px; background-color: red; opacity: 0.5; }
           </style></head><body><div class="box"></div></body></html>"#,
        Mode::Batch,
    );
    assert!(count_occurrences(&bytes, b"/Subtype /Form") > 0);
    assert!(count_occurrences(&bytes, b"/S /Transparency") > 0);
}

#[test]
fn opacity_of_one_does_not_create_a_form_xobject() {
    let bytes = build_pdf(
        r#"<html><head><style>
             .box { width: 50px; height: 50px; background-color: red; }
           </style></head><body><div class="box"></div></body></html>"#,
        Mode::Batch,
    );
    assert_eq!(count_occurrences(&bytes, b"/Subtype /Form"), 0);
}

#[test]
fn nested_opacity_creates_two_form_xobjects() {
    let bytes = build_pdf(
        r#"<html><head><style>
             .outer { width: 100px; height: 100px; background-color: green; opacity: 0.7; }
             .inner { width: 50px; height: 50px; background-color: yellow; opacity: 0.5; }
           </style></head><body>
             <div class="outer"><div class="inner"></div></div>
           </body></html>"#,
        Mode::Batch,
    );
    assert_eq!(count_occurrences(&bytes, b"/Subtype /Form"), 2);
}

#[test]
fn opacity_and_transform_combined_render_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<html><head><style>
             .box {
               width: 80px; height: 40px; background-color: blue;
               opacity: 0.6; transform: rotate(15deg) scale(1.2);
             }
           </style></head><body><div class="box">hi</div></body></html>"#,
        Mode::Batch,
    );
    assert!(count_occurrences(&bytes, b"/Subtype /Form") > 0);
    assert!(count_occurrences(&bytes, b"/S /Transparency") > 0);
}

#[test]
fn opacity_works_end_to_end_in_streaming_mode_too() {
    let bytes = build_pdf(
        r#"<html><head><style>
             .box { width: 50px; height: 50px; background-color: red; opacity: 0.5; }
           </style></head><body><div class="box"></div></body></html>"#,
        Mode::Streaming,
    );
    assert!(count_occurrences(&bytes, b"/Subtype /Form") > 0);
    assert!(count_occurrences(&bytes, b"/S /Transparency") > 0);
}

#[test]
fn transform_works_end_to_end_in_streaming_mode_too() {
    let bytes = build_pdf(
        r#"<html><head><style>
             .box { width: 50px; height: 50px; background-color: red; transform: rotate(10deg); }
           </style></head><body><div class="box"></div></body></html>"#,
        Mode::Streaming,
    );
    let content = decompressed_stream_bytes(&bytes);
    assert!(count_occurrences(&content, b" cm\n") > 0);
}
