//! E2E tests for `<base href>`.
//!
//! It checks moving the local path base (`<base href="assets/">`) by really feeding HTML
//! through `Engine` and seeing whether the image is embedded.

use std::path::PathBuf;

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const IMAGE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_opaque.png"
);

fn font_spec() -> FontSpec {
    FontSpec {
        path: PathBuf::from(FONT_PATH),
        index: 0,
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Build a temporary directory holding only `<base_dir>/assets/img.png`, for checking `<base>` resolution.
fn fixture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-base-href-{name}"));
    let assets = dir.join("assets");
    std::fs::create_dir_all(&assets).expect("should create the fixture directory");
    std::fs::copy(IMAGE_PATH, assets.join("img.png")).expect("should copy the fixture image");
    dir
}

fn build_pdf(html: &str, base_dir: PathBuf) -> Vec<u8> {
    let options = EngineOptions {
        mode: Mode::Batch,
        fonts: vec![font_spec()],
        base_dir: Some(base_dir),
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    engine.feed(html.as_bytes()).unwrap();
    let bytes = engine.finish().unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    bytes
}

#[test]
fn base_href_moves_the_directory_relative_references_are_resolved_against() {
    let dir = fixture_dir("resolves");
    let bytes = build_pdf(
        r#"<html><head><base href="assets/"></head>
             <body><img src="img.png" width="20" height="20"></body></html>"#,
        dir,
    );
    assert!(
        count_occurrences(&bytes, b"/Subtype /Image") > 0,
        "the image should be embedded through the <base href> directory"
    );
}

#[test]
fn without_base_href_the_same_reference_does_not_resolve() {
    // The control: with no `<base href>`, `img.png` points directly under base_dir and does
    // not exist (the document is still generated on failure).
    let dir = fixture_dir("without");
    let bytes = build_pdf(
        r#"<html><body><img src="img.png" width="20" height="20"></body></html>"#,
        dir,
    );
    assert_eq!(
        count_occurrences(&bytes, b"/Subtype /Image"),
        0,
        "no image should be embedded when the path does not resolve"
    );
}

#[test]
fn an_absolute_reference_ignores_the_base_href() {
    let dir = fixture_dir("absolute");
    // A data: URI is unaffected by `<base href>` (being an absolute reference).
    let bytes = build_pdf(
        r#"<html><head><base href="assets/"></head>
             <body><img src="data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==" width="10" height="10"></body></html>"#,
        dir,
    );
    assert!(
        count_occurrences(&bytes, b"/Subtype /Image") > 0,
        "a data: URI must still be embedded"
    );
}

#[test]
fn base_href_also_applies_in_streaming_mode() {
    let dir = fixture_dir("streaming");
    let options = EngineOptions {
        mode: Mode::Streaming,
        fonts: vec![font_spec()],
        base_dir: Some(dir),
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    engine
        .feed(
            br#"<html><head><base href="assets/"></head><body>
                  <p>first</p><img src="img.png" width="20" height="20"><p>last</p>
                </body></html>"#,
        )
        .unwrap();
    let bytes = engine.finish().unwrap();

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(
        count_occurrences(&bytes, b"/Subtype /Image") > 0,
        "streaming mode reads <base href> from the already-parsed <head>"
    );
}
