//! `Mode::Streaming` lays out each child of `<body>` on its own, as soon as it is final. The
//! requirement is that the result matches batch, which lays out `<body>` whole, so every test
//! here takes batch's output as the expected value and compares the text positions (and
//! drawing order) of the two PDFs page by page.
//!
//! - `position: absolute`/`fixed` (#97): a positioned box inside a top-level element used to
//!   be dropped with nothing said, and a top-level one was laid out in the flow.
//! - The flow itself: `<body>`'s top margin was lost, margins between top-level elements did
//!   not collapse, and text or inline elements directly under `<body>` were not wrapped in an
//!   anonymous block (text was not drawn, whitespace took up space).

use sghtmltopdf::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
/// The colour given to the positioned element, to tell its text apart from the flow's.
const MARK_OP: &str = "0.8 0 0 rg";

fn render(html: &str, mode: Mode) -> Vec<u8> {
    let mut options = EngineOptions::default();
    options.mode = mode;
    options.fonts = vec![FontSpec {
        path: std::path::PathBuf::from(FONT_PATH),
        index: 0,
    }];
    // Left uncompressed so the content streams can be read.
    options.output.compress = false;
    let mut engine = Engine::new(options, MemorySink::new());
    engine.feed(html.as_bytes()).unwrap();
    engine.finish().unwrap()
}

/// One entry per text object (`BT` .. `ET`) on a page: its `Tm` line, and whether it is
/// drawn in the mark colour.
type PageTexts = Vec<(String, bool)>;

/// The text objects of every page, in drawing order.
fn page_texts(pdf: &[u8]) -> Vec<PageTexts> {
    let pdf = String::from_utf8_lossy(pdf);
    pdf.split("stream\n")
        .skip(1)
        .map(|s| s.split("endstream").next().unwrap())
        // A page's content stream starts by scaling px to pt.
        .filter(|s| s.starts_with("0.75 0 0 0.75 0 0 cm"))
        .map(|stream| {
            stream
                .split("BT\n")
                .skip(1)
                .map(|text| {
                    let tm = text
                        .lines()
                        .find(|l| l.ends_with(" Tm"))
                        .expect("a text object sets its matrix")
                        .to_string();
                    (tm, text.contains(MARK_OP))
                })
                .collect()
        })
        .collect()
}

/// The `Tm` lines of the marked text, page by page.
fn marked_positions(pages: &[PageTexts]) -> Vec<Vec<String>> {
    pages
        .iter()
        .map(|page| {
            page.iter()
                .filter(|(_, marked)| *marked)
                .map(|(tm, _)| tm.clone())
                .collect()
        })
        .collect()
}

/// Render `html` both ways, check streaming against batch, and return batch's page texts for
/// the caller to check that the case really shows what it is meant to.
fn assert_streaming_matches_batch(html: &str) -> Vec<PageTexts> {
    let batch = page_texts(&render(html, Mode::Batch));
    let streaming = page_texts(&render(html, Mode::Streaming));
    assert_eq!(streaming, batch, "streaming differs from batch for {html}");
    batch
}

// ---- position: absolute / fixed (#97) ----

/// The reproduction from the issue, unchanged but for the mark colour.
#[test]
fn a_nested_absolute_is_drawn_where_batch_draws_it() {
    let batch = assert_streaming_matches_batch(
        r#"<body>
 <div style="position:relative;height:40px">
   <div style="position:absolute;right:0;top:0;color:#cc0000">ABS</div>
 </div>
 <p>tail</p>
</body>"#,
    );
    assert_eq!(marked_positions(&batch)[0].len(), 1, "{batch:?}");
}

/// The issue: "The parent does not need `position: relative`. A plain parent gives the same
/// result." With no positioned ancestor batch puts the box on the first page.
#[test]
fn a_nested_absolute_in_a_plain_parent_is_drawn_where_batch_draws_it() {
    let batch = assert_streaming_matches_batch(
        r#"<body>
 <div style="height:40px">
   <div style="position:absolute;right:0;top:0;color:#cc0000">ABS</div>
 </div>
 <p>tail</p>
</body>"#,
    );
    assert_eq!(marked_positions(&batch)[0].len(), 1, "{batch:?}");
}

/// The ancestor lands on a later page, after the first has been written.
#[test]
fn a_nested_absolute_on_a_later_page_is_drawn_where_batch_draws_it() {
    let batch = assert_streaming_matches_batch(
        r#"<body>
 <div style="height:1200px">tall</div>
 <div style="position:relative;height:40px">
   <div style="position:absolute;right:0;top:10px;color:#cc0000">ABS</div>
 </div>
 <p>tail</p>
</body>"#,
    );
    let marked = marked_positions(&batch);
    assert!(
        marked[0].is_empty() && marked.iter().flatten().count() == 1,
        "batch draws ABS once, after the first page: {marked:?}"
    );
}

/// A positioned box is painted after the in-flow content of its page (CSS 2.1 Appendix E), so
/// the flow of a later top-level element on the same page must not cover it.
#[test]
fn a_nested_absolute_is_drawn_after_the_flow_that_follows_it_on_the_page() {
    let batch = assert_streaming_matches_batch(
        r#"<body>
 <div style="position:relative;height:40px">
   <div style="position:absolute;left:0;top:60px;color:#cc0000">ABS</div>
 </div>
 <p>tail</p>
 <p>more</p>
</body>"#,
    );
    assert!(
        batch[0].last().is_some_and(|(_, marked)| *marked),
        "batch draws ABS last: {batch:?}"
    );
}

/// Each absolute box is drawn once, wherever in the element it sits.
#[test]
fn several_nested_absolutes_are_each_drawn_where_batch_draws_them() {
    let batch = assert_streaming_matches_batch(
        r#"<body>
 <section>
   <div style="position:relative;height:40px">
     <span style="position:absolute;left:0;top:0;color:#cc0000">L</span>
     <span style="position:absolute;right:0;top:0;color:#cc0000">R</span>
   </div>
   <div style="display:flex">
     <div style="position:relative;flex:1;height:40px">
       <span style="position:absolute;right:0;bottom:0;color:#cc0000">F</span>
     </div>
   </div>
 </section>
</body>"#,
    );
    assert_eq!(marked_positions(&batch)[0].len(), 3, "{batch:?}");
}

/// An absolute element directly under `<body>` is out of the flow, as batch lays it out: placed
/// against the page area, it takes no space from the elements around it.
#[test]
fn a_top_level_absolute_is_drawn_where_batch_draws_it() {
    let batch = assert_streaming_matches_batch(
        r#"<body>
 <p>before</p>
 <div style="position:absolute;right:0;top:20px;color:#cc0000">ABS</div>
 <p>after</p>
</body>"#,
    );
    assert_eq!(marked_positions(&batch)[0].len(), 1, "{batch:?}");
}

/// `fixed` repeats on every page in batch. Met before any page is written, streaming can
/// repeat it on every page too, whether it sits directly under `<body>` or inside an element.
#[test]
fn a_fixed_element_met_before_the_first_page_is_written_repeats_on_every_page() {
    for fixed in [
        r#"<div style="position:fixed;left:0;bottom:0;color:#cc0000">FIX</div>"#,
        r#"<div><span style="position:fixed;right:0;top:0;color:#cc0000">FIX</span></div>"#,
    ] {
        let batch = assert_streaming_matches_batch(&format!(
            r#"<body>{fixed}<p>one</p><div style="height:1500px">tall</div><p>two</p></body>"#
        ));
        let marked = marked_positions(&batch);
        assert!(
            marked.len() == 3 && marked.iter().all(|page| page.len() == 1),
            "batch draws FIX once on each of the 3 pages: {marked:?}"
        );
    }
}

// ---- The flow of the children of <body> ----

/// `<body>`'s default 8px top margin stays above the first element.
#[test]
fn the_body_top_margin_is_kept() {
    assert_streaming_matches_batch(r#"<body><div style="height:40px">x</div><p>tail</p></body>"#);
}

/// Adjacent vertical margins of top-level elements collapse (CSS2.1 8.3.1), negative ones
/// included, and the first one collapses with `<body>`'s own.
#[test]
fn margins_between_top_level_elements_collapse() {
    assert_streaming_matches_batch(
        r#"<body><p>a</p><p>b</p><h1>c</h1><div style="margin:-5px 0 30px">d</div><p>e</p></body>"#,
    );
    // A nested first child's margin is lifted through its parent too.
    assert_streaming_matches_batch(r#"<body><div><p>nested</p></div><p>b</p></body>"#);
}

/// With top padding `<body>`'s margin does not collapse with its first child's.
#[test]
fn the_body_margin_does_not_collapse_through_padding() {
    assert_streaming_matches_batch(r#"<body style="padding:5px"><p>a</p><p>b</p></body>"#);
}

/// The positions carry over a page break, natural or forced.
#[test]
fn the_flow_matches_batch_across_page_breaks() {
    let batch = assert_streaming_matches_batch(
        r#"<body><p>a</p><div style="height:1500px">t</div><p>b</p><p>c</p></body>"#,
    );
    assert_eq!(batch.len(), 3, "{batch:?}");
    let batch = assert_streaming_matches_batch(
        r#"<body><p>a</p><p style="break-before:page">b</p><p>c</p></body>"#,
    );
    assert_eq!(batch.len(), 2, "{batch:?}");
}

/// Whitespace between blocks generates no box (CSS2.1 9.2.2.1), so it takes no space.
#[test]
fn whitespace_between_top_level_elements_takes_no_space() {
    assert_streaming_matches_batch(r#"<body><div style="height:40px"></div> <p>tail</p></body>"#);
    assert_streaming_matches_batch("<body>\n <div>a</div>\n   \n <div>b</div>\n</body>");
}

/// Text directly under `<body>` goes in an anonymous block (CSS2.1 9.2.1.1) and is drawn.
#[test]
fn text_directly_under_body_is_drawn() {
    let batch = assert_streaming_matches_batch(r#"<body><div>a</div>hello<div>b</div></body>"#);
    assert_eq!(batch[0].len(), 3, "a, hello and b: {batch:?}");
}

/// A run of text and inline elements directly under `<body>` shares one anonymous block, so
/// it is laid out on one line, as in batch. A `display: none` element does not end the run.
#[test]
fn inline_content_directly_under_body_shares_lines() {
    let html = r#"<body>Hello <b>bold</b> and <i>it</i><p>para</p>tail text</body>"#;
    assert_streaming_matches_batch(html);
    // Three lines in batch: the run before `<p>`, `para` and the run after it.
    let pdf = render(html, Mode::Batch);
    let line_ys: std::collections::BTreeSet<String> = String::from_utf8_lossy(&pdf)
        .lines()
        .filter(|l| l.ends_with(" Tm"))
        .map(|l| l.rsplit(' ').nth(1).unwrap().to_string())
        .collect();
    assert_eq!(line_ys.len(), 3, "{line_ys:?}");
    assert_streaming_matches_batch(
        r#"<body><p>a</p><span style="display:none">x</span>run <b>on</b><p>b</p></body>"#,
    );
}

/// An inline element's vertical margins do not apply (CSS2.1 10.6.1), also directly under
/// `<body>`.
#[test]
fn an_inline_element_directly_under_body_has_no_vertical_margin() {
    assert_streaming_matches_batch(
        r#"<body><div>a</div><span style="margin:20px">s</span><div>b</div></body>"#,
    );
}
