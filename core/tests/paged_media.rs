//! E2E tests for `@page` (overriding `size`/`margin` document-wide, and varying the margin
//! boxes with `:first`/`:left`/`:right`), `@media` (print/all always applied, screen always
//! ignored), the 16 margin boxes, and `counter(page)`/`counter(pages)`.
//!
//! The same approach as `background.rs`/`box_sizing.rs`: catch regressions by going through
//! the real pipeline. This feature is wired directly into `Engine` (`core/src/engine.rs`)
//! (resolving `@page`, the Mode::Streaming restriction on `counter(pages)` and pre-counting
//! the total pages are all the `Engine` layer's job), so unlike the other E2E test files it
//! uses the `Engine` API directly.

use sghtmltopdf_core::engine::{Engine, EngineError, EngineOptions, FontSpec, Mode};
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

/// A helper letting the expected `/MediaBox` be written in CSS px. It is written to the PDF
/// in pt (0.75x by default), so the conversion happens here.
fn media_box(width_px: f32, height_px: f32) -> String {
    format!(
        "/MediaBox [0 0 {} {}]",
        width_px * sghtmltopdf_core::pdf::DEFAULT_SCALE,
        height_px * sghtmltopdf_core::pdf::DEFAULT_SCALE
    )
}

/// A PDF's content stream is FlateDecode compressed, so searching for a character inside the
/// `/ToUnicode` CMap requires inflating first (the same logic as the identically named
/// functions in the `engine.rs` and `pdf::document` test modules, cutting the stream
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

fn build_pdf_batch(html: &str) -> Vec<u8> {
    let options = EngineOptions {
        mode: Mode::Batch,
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
fn at_page_size_and_margin_override_the_whole_document() {
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @page { size: 300px 400px; margin: 10px; }
           </style></head><body><p>hello</p></body></html>"#,
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(300.0, 400.0).as_bytes()),
        1
    );
}

#[test]
fn at_page_pseudo_class_size_is_ignored_only_unconditional_rules_apply() {
    // The size/margin declarations of `:first` and friends parse but are not applied (the
    // document keeps a single geometry throughout). Only the unconditional rules take effect.
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @page { size: 300px 400px; }
             @page :first { size: 999px 999px; }
           </style></head><body><p>hello</p></body></html>"#,
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(300.0, 400.0).as_bytes()),
        1
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(999.0, 999.0).as_bytes()),
        0
    );
}

#[test]
fn media_screen_rules_are_ignored_and_media_print_rules_apply() {
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @media screen { @page { size: 999px 999px; } }
             @media print { @page { size: 300px 400px; } }
           </style></head><body><p>hello</p></body></html>"#,
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(300.0, 400.0).as_bytes()),
        1
    );
}

#[test]
fn margin_box_content_renders_a_valid_pdf_with_the_expected_page_count() {
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @page {
               size: 200px 300px; margin: 0;
               @top-center { content: "Title"; }
               @bottom-left { content: "left"; }
               @bottom-center { content: "Page " counter(page) " of " counter(pages); }
               @bottom-right { content: counter(page); }
             }
             body { margin: 0; } div { height: 300px; }
           </style></head><body><div></div><div></div><div></div></body></html>"#,
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(200.0, 300.0).as_bytes()),
        3
    );
}

#[test]
fn counter_page_and_pages_resolve_to_the_correct_glyphs_across_pages() {
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @page {
               size: 200px 300px; margin: 0;
               @bottom-right { content: "Page " counter(page) " of " counter(pages); }
             }
             body { margin: 0; } div { height: 300px; }
           </style></head><body><div></div><div></div></body></html>"#,
    );
    let decompressed = decompressed_stream_bytes(&bytes);
    // A two-page document: "counter(pages)" is always '2' and "counter(page)" is '1' and '2'.
    // No digit appears in the body at all, so those ToUnicode CMap entries can only be
    // generated through the margin-box-specific glyph usage collection
    // (`collect_margin_box_usage`).
    assert!(count_occurrences(&decompressed, b"<0031>") > 0, "'1' glyph");
    assert!(count_occurrences(&decompressed, b"<0032>") > 0, "'2' glyph");
}

#[test]
fn at_page_first_selects_different_margin_box_content_than_other_pages() {
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @page {
               size: 200px 300px; margin: 0;
               @bottom-center { content: "normal"; }
             }
             @page :first {
               @bottom-center { content: "cover"; }
             }
             body { margin: 0; } div { height: 300px; }
           </style></head><body><div></div><div></div></body></html>"#,
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(200.0, 300.0).as_bytes()),
        2
    );
}

#[test]
fn counter_pages_in_a_margin_box_is_rejected_in_streaming_mode() {
    let options = EngineOptions {
        mode: Mode::Streaming,
        fonts: vec![font_spec()],
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    let result = engine.feed(
        br#"<html><head><style>
              @page { @bottom-center { content: counter(pages); } }
            </style></head><body><p>x</p></body></html>"#,
    );
    match result {
        Err(EngineError::UnsupportedInStreamingMode(_)) => {}
        other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
    }
}

#[test]
fn counter_page_alone_works_in_streaming_mode() {
    let options = EngineOptions {
        mode: Mode::Streaming,
        fonts: vec![font_spec()],
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    engine
        .feed(
            br#"<html><head><style>
                  @page { size: 200px 300px; margin: 0; @bottom-right { content: counter(page); } }
                  body { margin: 0; } div { height: 300px; }
                </style></head><body><div></div><div></div></body></html>"#,
        )
        .expect("counter(page) alone should be allowed in streaming mode");
    let bytes = engine.finish().unwrap();
    assert_eq!(
        count_occurrences(&bytes, media_box(200.0, 300.0).as_bytes()),
        2
    );
}

#[test]
fn all_paged_media_features_combined_render_a_valid_pdf_end_to_end() {
    let bytes = build_pdf_batch(
        r#"<html><head><style>
             @media print {
               @page {
                 size: A4;
                 margin: 80px 60px;
                 @top-left-corner { content: "TL"; }
                 @top-left { content: "left"; }
                 @top-center { content: "Invoice"; }
                 @top-right { content: "right"; }
                 @top-right-corner { content: "TR"; }
                 @left-top { content: "lt"; }
                 @left-middle { content: "lm"; }
                 @left-bottom { content: "lb"; }
                 @right-top { content: "rt"; }
                 @right-middle { content: "rm"; }
                 @right-bottom { content: "rb"; }
                 @bottom-left-corner { content: "BL"; }
                 @bottom-left { content: "sghtmltopdf"; }
                 @bottom-center { content: "Page " counter(page) " of " counter(pages); }
                 @bottom-right { content: counter(page); }
                 @bottom-right-corner { content: "BR"; }
               }
             }
             @media screen {
               @page { size: 1px 1px; }
             }
             body { margin: 0; font-family: sans-serif; }
             .item { height: 900px; }
           </style></head>
           <body><div class="item">1</div><div class="item">2</div></body></html>"#,
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
