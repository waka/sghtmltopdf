//! Check the selector behaviour in streaming mode against batch mode.
//!
//! `Mode::Streaming` treats a top-level element directly under `<body>` as final and
//! processes it as soon as the next sibling appears. So a selector looking "after" that
//! element is decided against a partial DOM as of that moment, and the result can differ from batch.
//!
//! This file's job is to pin down which selectors really do diverge.
//! The input is `feed` one element at a time (the CLI reads in 64KiB units, but that is
//! merely a question of where the chunk boundaries happen to fall; the engine's contract is
//! "the same result however it is chopped up", so the finest chopping is used).

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
/// The colour applied to a matched element. Counted as a PDF fill colour operator.
const MARK_CSS: &str = "color: #cc0000";
const MARK_OP: &[u8] = b"0.8 0 0 rg";

fn options(mode: Mode) -> EngineOptions {
    EngineOptions {
        mode,
        fonts: vec![FontSpec {
            path: std::path::PathBuf::from(FONT_PATH),
            index: 0,
        }],
        output: sghtmltopdf_core::pdf::PdfOutputOptions {
            // Left uncompressed so the fill colour operators can be counted.
            compress: false,
            ..Default::default()
        },
        ..EngineOptions::default()
    }
}

/// Return the number of elements matched by `selector`. `body` is the contents of `<body>`.
fn matched_count(selector: &str, body: &str, mode: Mode) -> usize {
    let head = format!("<html><head><style>{selector} {{ {MARK_CSS} }}</style></head><body>");
    let mut engine = Engine::new(options(mode), MemorySink::new());
    engine.feed(head.as_bytes()).unwrap();
    // Feed the top-level elements one at a time, so nothing depends on the chunk boundaries.
    for element in split_top_level(body) {
        engine.feed(element.as_bytes()).unwrap();
    }
    engine.feed(b"</body></html>").unwrap();
    let bytes = engine.finish().unwrap();

    bytes
        .windows(MARK_OP.len())
        .filter(|w| *w == MARK_OP)
        .count()
}

/// Split a sequence such as `<p>a</p><div>b</div>` into its top-level elements.
/// Only one level of nesting is assumed (limited to the input this test uses).
fn split_top_level(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    let mut rest = body;
    while let Some(open) = rest.find('<') {
        current.push_str(&rest[..open]);
        let close = rest[open..].find('>').expect("tag should be closed") + open;
        let tag = &rest[open..=close];
        current.push_str(tag);
        if tag.starts_with("</") {
            depth -= 1;
        } else if !tag.ends_with("/>") {
            depth += 1;
        }
        if depth == 0 {
            out.push(std::mem::take(&mut current));
        }
        rest = &rest[close + 1..];
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Selectors whose result matches between batch and streaming.
///
/// The ones looking at the preceding sibling (`+`, `~`, `:first-child` and so on) can be
/// here because in a document using them the freeing of a top-level element is limited to its
/// descendants and the element itself is kept (`style::needs_preceding_siblings` decides that).
#[test]
fn these_selectors_behave_the_same_in_both_modes() {
    // (selector, body, match count)
    let cases: &[(&str, &str, usize)] = &[
        // The ones needing the preceding sibling.
        ("p:first-child", "<div>D</div><p>a</p><p>b</p>", 0),
        ("p:nth-child(2)", "<div>D</div><p>a</p><p>b</p>", 1),
        ("p:first-of-type", "<div>D</div><p>a</p><p>b</p>", 1),
        ("p:nth-of-type(2)", "<p>a</p><p>b</p><p>c</p>", 1),
        ("p:only-child", "<p>a</p><p>b</p>", 0),
        ("p:only-child", "<p>a</p>", 1),
        ("div + p", "<div>D</div><p>a</p><p>b</p>", 1),
        ("div ~ p", "<div>D</div><p>a</p><p>b</p>", 2),
        ("p + p", "<p>a</p><p>b</p><p>c</p>", 2),
        // The ones looking only at "is there a sibling after me" coincide with the condition
        // for being final (the next sibling appeared), so the decision does not waver.
        ("p:last-child", "<p>a</p><div>b</div><p>c</p>", 1),
        ("p:last-child", "<p>a</p><p>b</p><div>c</div>", 0),
        // The ones looking only at their own children have all of them by the time it is final.
        ("p:empty", "<p></p><p>b</p>", 0),
        (
            "section:has(h1)",
            "<section><h1>x</h1></section><p>b</p>",
            1,
        ),
        // The next sibling is the very trigger for being final, so it is always visible.
        ("div:has(+ p)", "<div>a</div><p>b</p><p>c</p>", 1),
        // Position-independent ones match by definition.
        (":is(div, h1)", "<p>a</p><div>b</div>", 1),
        (":where(div, h1)", "<p>a</p><div>b</div>", 1),
    ];

    for (selector, body, expected) in cases {
        assert_eq!(
            matched_count(selector, body, Mode::Batch),
            *expected,
            "the batch result differs from the expectation: {selector} / {body}"
        );
        assert_eq!(
            matched_count(selector, body, Mode::Streaming),
            *expected,
            "a selector that must not diverge in streaming: {selector} / {body}"
        );
    }
}

/// Selectors whose result differs between batch and streaming (pinning down the current behaviour).
///
/// All of them need to know "whether more elements of the same type follow", but a top-level
/// element only becomes final when the next sibling appears, so what comes after is unknown.
/// This is the part keeping the preceding sibling cannot cover.
///
/// What is listed here must match what `style::streaming_unsafe_selectors` warns about.
#[test]
fn these_selectors_diverge_in_streaming_mode() {
    // (selector, body, batch, streaming)
    let cases: &[(&str, &str, usize, usize)] = &[
        ("p:last-of-type", "<p>a</p><div>D</div><p>b</p>", 1, 2),
        ("p:only-of-type", "<p>a</p><div>D</div><p>b</p>", 0, 1),
        ("p:nth-last-child(2)", "<p>a</p><p>b</p><p>c</p>", 1, 2),
        (
            "p:nth-last-of-type(1)",
            "<p>a</p><div>D</div><p>b</p>",
            1,
            2,
        ),
        ("div:has(~ h1)", "<div>a</div><p>x</p><h1>b</h1>", 1, 0),
    ];

    for (selector, body, batch, streaming) in cases {
        assert_eq!(
            matched_count(selector, body, Mode::Batch),
            *batch,
            "the batch result differs from the expectation: {selector} / {body}"
        );
        assert_eq!(
            matched_count(selector, body, Mode::Streaming),
            *streaming,
            "the streaming result differs from the expectation: {selector} / {body}"
        );
    }
}
