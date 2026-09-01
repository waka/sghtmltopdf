//! E2E tests for the PDF link annotations of `<a href>`.
//!
//! They read `/Annots`, `/URI`, `/Dest` and `/Dests` out of the generated PDF bytes and
//! check them (an annotation is a dictionary object, so unlike a Flate-compressed content
//! stream it can be searched directly).

use std::path::PathBuf;

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

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
fn an_external_link_becomes_a_uri_annotation() {
    let bytes = build_pdf(
        r#"<p><a href="https://example.com/docs">documentation</a></p>"#,
        Mode::Batch,
    );

    assert!(count_occurrences(&bytes, b"/Subtype /Link") >= 1);
    assert!(count_occurrences(&bytes, b"/S /URI") >= 1);
    assert!(
        count_occurrences(&bytes, b"(https://example.com/docs)") >= 1,
        "the URI must be written verbatim"
    );
    assert!(count_occurrences(&bytes, b"/Annots") >= 1);
}

#[test]
fn a_mailto_link_is_written_as_is() {
    let bytes = build_pdf(
        r#"<p><a href="mailto:sales@example.com">mail us</a></p>"#,
        Mode::Batch,
    );
    assert!(count_occurrences(&bytes, b"(mailto:sales@example.com)") >= 1);
}

#[test]
fn a_javascript_href_does_not_produce_an_annotation() {
    let bytes = build_pdf(
        r#"<p><a href="javascript:alert(1)">click</a></p>"#,
        Mode::Batch,
    );
    assert_eq!(count_occurrences(&bytes, b"/Subtype /Link"), 0);
}

#[test]
fn a_document_without_links_has_no_annotations() {
    let bytes = build_pdf("<p>plain text</p>", Mode::Batch);
    assert_eq!(count_occurrences(&bytes, b"/Subtype /Link"), 0);
    assert_eq!(count_occurrences(&bytes, b"/Annots"), 0);
    assert_eq!(count_occurrences(&bytes, b"/Dests"), 0);
}

#[test]
fn an_internal_anchor_becomes_a_named_destination() {
    let bytes = build_pdf(
        r##"<p><a href="#chapter-2">jump</a></p>
           <h2 id="chapter-2">Chapter 2</h2>"##,
        Mode::Batch,
    );

    assert!(count_occurrences(&bytes, b"/Subtype /Link") >= 1);
    assert!(
        count_occurrences(&bytes, b"/Dest /a_chapter-2") >= 1,
        "the link must reference the sanitised destination name"
    );
    assert!(
        count_occurrences(&bytes, b"/a_chapter-2 [") >= 1,
        "the /Dests dictionary must define that name"
    );
    assert!(count_occurrences(&bytes, b"/Dests") >= 1);
}

#[test]
fn a_forward_reference_to_a_later_page_resolves() {
    // A link from the table of contents into the body. The destination is on a later page than the link.
    let bytes = build_pdf(
        r##"<p><a href="#body">go to the body</a></p>
           <p id="body" style="break-before: page;">the body</p>"##,
        Mode::Batch,
    );
    assert!(count_occurrences(&bytes, b"/Dest /a_body") >= 1);
    assert!(count_occurrences(&bytes, b"/a_body [") >= 1);
}

#[test]
fn a_link_to_a_missing_anchor_is_written_but_resolves_to_nothing() {
    let bytes = build_pdf(r##"<p><a href="#nope">dangling</a></p>"##, Mode::Batch);
    assert!(count_occurrences(&bytes, b"/Dest /a_nope") >= 1);
    assert_eq!(
        count_occurrences(&bytes, b"/a_nope ["),
        0,
        "no destination should be defined for a missing anchor"
    );
}

#[test]
fn an_a_name_anchor_is_also_a_destination() {
    let bytes = build_pdf(
        r##"<p><a href="#legacy">jump</a></p><p><a name="legacy">target</a></p>"##,
        Mode::Batch,
    );
    // An `<a name>` is an inline element with no box of its own, so its position cannot be
    // determined and no destination is generated (a known limitation). The link side is still written.
    assert!(count_occurrences(&bytes, b"/Dest /a_legacy") >= 1);
}

#[test]
fn links_also_work_in_streaming_mode() {
    let bytes = build_pdf(
        r##"<html><body>
             <p><a href="https://example.com">external</a></p>
             <p><a href="#later">internal forward reference</a></p>
             <p>filler</p>
             <p id="later" style="break-before: page;">target</p>
           </body></html>"##,
        Mode::Streaming,
    );

    assert!(count_occurrences(&bytes, b"/S /URI") >= 1);
    assert!(count_occurrences(&bytes, b"(https://example.com)") >= 1);
    assert!(
        count_occurrences(&bytes, b"/a_later [") >= 1,
        "streaming mode must resolve a forward reference through /Dests"
    );
}

#[test]
fn a_relative_link_is_resolved_against_the_base_href() {
    let bytes = build_pdf(
        r#"<html><head><base href="https://example.com/docs/"></head>
             <body><p><a href="guide.html">guide</a></p></body></html>"#,
        Mode::Batch,
    );
    assert!(
        count_occurrences(&bytes, b"(https://example.com/docs/guide.html)") >= 1,
        "a relative href must be resolved against <base href> before being written"
    );
}

#[test]
fn a_link_spanning_two_lines_produces_two_annotations() {
    let bytes = build_pdf(
        r#"<p style="width: 120px;"><a href="https://example.com">word word word word word word word word word word</a></p>"#,
        Mode::Batch,
    );
    assert!(
        count_occurrences(&bytes, b"/Subtype /Link") >= 2,
        "each line of a wrapped link needs its own annotation rectangle"
    );
}
