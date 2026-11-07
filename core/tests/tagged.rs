//! E2E tests for tagged PDF output (`--tagged`, issue #132).
//!
//! The expectations come from ISO 32000-1: 14.7 (logical structure: `/StructTreeRoot`,
//! structure elements, marked-content references, the parent tree), 14.8 (tagged PDF:
//! `/MarkInfo`, the standard structure types, `/Alt` on a `Figure`, page furniture as
//! artifacts, `/Lang`). The PDFs are written uncompressed so the content streams can be
//! searched directly.

use std::path::PathBuf;

use sghtmltopdf::engine::{Engine, EngineError, EngineOptions, FontSpec, Mode};
use sghtmltopdf::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

/// A 1x1 PNG.
const PIXEL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn count(haystack: &[u8], needle: &str) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle.as_bytes())
        .count()
}

fn try_build(html: &str, tagged: bool, mode: Mode) -> Result<Vec<u8>, EngineError<std::io::Error>> {
    let mut options = EngineOptions::default();
    options.mode = mode;
    options.fonts = vec![FontSpec {
        path: PathBuf::from(FONT_PATH),
        index: 0,
    }];
    options.output.compress = false;
    options.output.tagged = tagged;
    let mut engine = Engine::new(options, MemorySink::new());
    engine.feed(html.as_bytes())?;
    engine.finish()
}

fn build(html: &str, tagged: bool) -> Vec<u8> {
    try_build(html, tagged, Mode::Batch).unwrap()
}

/// The `MCID` numbers following `/MCID `, in file order.
fn mcids(bytes: &[u8]) -> Vec<u32> {
    let text = String::from_utf8_lossy(bytes);
    text.split("/MCID ")
        .skip(1)
        .map(|rest| {
            rest.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap()
        })
        .collect()
}

#[test]
fn an_untagged_pdf_has_no_structure_tree() {
    let bytes = build("<h1>Title</h1><p>Text</p>", false);
    assert_eq!(count(&bytes, "/StructTreeRoot"), 0);
    assert_eq!(count(&bytes, "/MarkInfo"), 0);
    assert_eq!(count(&bytes, "/MCID"), 0);
}

#[test]
fn a_tagged_pdf_is_marked_and_has_a_structure_tree_root() {
    let bytes = build("<h1>Title</h1><p>Text</p>", true);
    assert_eq!(count(&bytes, "/MarkInfo <<\n    /Marked true"), 1);
    assert_eq!(count(&bytes, "/Type /StructTreeRoot"), 1);
    assert_eq!(
        count(&bytes, "/StructTreeRoot "),
        1,
        "the catalog points at it"
    );
    assert_eq!(count(&bytes, "/S /Document"), 1);
}

#[test]
fn headings_and_paragraphs_get_their_standard_structure_types() {
    let bytes = build(
        "<h1>One</h1><h2>Two</h2><h3>Three</h3><p>para</p><p>another</p>",
        true,
    );
    assert_eq!(count(&bytes, "/S /H1"), 1);
    assert_eq!(count(&bytes, "/S /H2"), 1);
    assert_eq!(count(&bytes, "/S /H3"), 1);
    assert_eq!(count(&bytes, "/S /P\n"), 2);
}

#[test]
fn the_document_language_comes_from_the_html_element() {
    let bytes = build(
        r#"<html lang="fr"><body><p>Bonjour</p></body></html>"#,
        true,
    );
    assert_eq!(count(&bytes, "/Lang (fr)"), 1, "written once, not twice");
}

#[test]
fn a_list_is_l_li_lbl_and_lbody() {
    let bytes = build("<ul><li>one</li><li>two</li></ul>", true);
    assert_eq!(count(&bytes, "/S /L\n"), 1);
    assert_eq!(count(&bytes, "/S /LI"), 2);
    assert_eq!(count(&bytes, "/S /Lbl"), 2, "each marker is a label");
    assert_eq!(count(&bytes, "/S /LBody"), 2);
}

#[test]
fn an_image_with_alt_text_is_a_figure_carrying_it() {
    let html = format!(r#"<img src="{PIXEL}" alt="a pixel" width="10" height="10">"#);
    let bytes = build(&html, true);
    assert_eq!(count(&bytes, "/S /Figure"), 1);
    assert_eq!(count(&bytes, "/Alt (a pixel)"), 1);
    assert_eq!(
        count(&bytes, "/Figure\n"),
        1,
        "the content is tagged Figure"
    );
}

#[test]
fn an_image_without_alt_text_is_an_artifact() {
    let html = format!(r#"<img src="{PIXEL}" width="10" height="10"><img src="{PIXEL}" alt="">"#);
    let bytes = build(&html, true);
    assert_eq!(count(&bytes, "/S /Figure"), 0);
    assert_eq!(count(&bytes, "/Alt"), 0);
}

#[test]
fn a_link_is_a_link_element_inside_its_paragraph() {
    let bytes = build(
        r#"<p>see <a href="https://example.com/">the docs</a> now</p>"#,
        true,
    );
    assert_eq!(count(&bytes, "/S /Link"), 1);
    assert_eq!(count(&bytes, "/S /P\n"), 1);
}

#[test]
fn every_marked_content_id_is_referenced_by_exactly_one_structure_element() {
    let bytes = build(
        "<h1>Title</h1><p>one</p><ul><li>a</li><li>b</li></ul><p>two</p>",
        true,
    );
    let ids = mcids(&bytes);
    let content_ops = count(&bytes, "BDC");
    assert!(content_ops > 0);
    // Each MCID is written once in the content stream (`BDC`) and once in an `MCR`.
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(ids.len(), sorted.len() * 2);
    assert_eq!(count(&bytes, "/Type /MCR"), sorted.len());
    // The ids on a page are 0, 1, 2, ... without gaps.
    assert_eq!(sorted, (0..sorted.len() as u32).collect::<Vec<_>>());
}

#[test]
fn the_parent_tree_has_an_entry_per_marked_content_id() {
    let bytes = build("<h1>Title</h1><p>text</p>", true);
    assert_eq!(count(&bytes, "/StructParents 0"), 1);
    assert_eq!(count(&bytes, "/ParentTree <<"), 1);
    assert_eq!(count(&bytes, "/Nums [0 "), 1);
    let ids = mcids(&bytes);
    let array_line = String::from_utf8_lossy(&bytes)
        .lines()
        .find(|line| line.starts_with('[') && line.contains(" 0 R"))
        .expect("the parent tree's array for page 0")
        .to_string();
    assert_eq!(array_line.matches(" 0 R").count(), ids.len() / 2);
}

#[test]
fn page_furniture_is_not_tagged() {
    let html =
        "<style>@page { margin: 40px; @bottom-center { content: \"FOOTER\" } }</style><p>body</p>";
    let with_footer = build(html, true);
    let without = build("<p>body</p>", true);
    assert_eq!(
        mcids(&with_footer).len(),
        mcids(&without).len(),
        "a margin box adds no marked content ids"
    );
    assert!(count(&with_footer, "/Artifact BMC") > count(&without, "/Artifact BMC"));
}

#[test]
fn decoration_is_an_artifact_and_empty_artifacts_are_not_written() {
    let bytes = build(
        r#"<div style="background:#ddd;border:1px solid black">boxed</div><p>plain</p>"#,
        true,
    );
    assert!(count(&bytes, "/Artifact BMC") >= 1);
    assert_eq!(count(&bytes, "/Artifact BMC\nEMC"), 0);
}

#[test]
fn a_tagged_pdf_cannot_be_written_in_streaming_mode() {
    match try_build("<p>text</p>", true, Mode::Streaming) {
        Err(EngineError::UnsupportedInStreamingMode(message)) => {
            assert!(message.contains("tagged"), "{message}");
        }
        other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
    }
}

// ISO 32000-1 14.8.4.3.4: a table is Table > (THead | TBody | TFoot) > TR > (TH | TD), and a
// header cell carries /Scope (14.8.5.4.2, Table attributes). Spans are /ColSpan and /RowSpan.
#[test]
fn a_table_is_table_row_group_row_and_cells() {
    let bytes = build(
        r#"<table><caption>Sales</caption>
           <thead><tr><th>Item</th><th scope="row">Qty</th></tr></thead>
           <tbody><tr><td colspan="2">wide</td></tr><tr><td>a</td><td rowspan="1">b</td></tr></tbody>
           <tfoot><tr><td>total</td><td>3</td></tr></tfoot></table>"#,
        true,
    );
    assert_eq!(count(&bytes, "/S /Table"), 1);
    assert_eq!(count(&bytes, "/S /Caption"), 1);
    assert_eq!(count(&bytes, "/S /THead"), 1);
    assert_eq!(count(&bytes, "/S /TBody"), 1);
    assert_eq!(count(&bytes, "/S /TFoot"), 1);
    assert_eq!(
        count(
            &bytes, "/S /TR
"
        ),
        4
    );
    assert_eq!(
        count(
            &bytes, "/S /TH
"
        ),
        2
    );
    assert_eq!(
        count(
            &bytes, "/S /TD
"
        ),
        5
    );
    assert_eq!(
        count(&bytes, "/Scope /Column"),
        1,
        "a <thead> header labels its column"
    );
    assert_eq!(count(&bytes, "/Scope /Row"), 1, "scope=row is honoured");
    assert_eq!(count(&bytes, "/ColSpan 2"), 1);
    assert_eq!(count(&bytes, "/RowSpan"), 0, "a span of 1 is the default");
}

#[test]
fn a_header_repeated_on_later_pages_is_an_artifact() {
    let rows = "<tr><td>row</td></tr>".repeat(40);
    let html = format!(
        "<style>@page {{ size: 200px 120px; margin: 10px }}</style>\
         <table><thead><tr><th>Head</th></tr></thead><tbody>{rows}</tbody></table>"
    );
    let bytes = build(&html, true);
    assert!(
        count(&bytes, "/MediaBox") > 1,
        "the table spans several pages"
    );
    assert_eq!(
        count(
            &bytes, "/S /TH
"
        ),
        1,
        "the header is in the tree once"
    );
    assert_eq!(
        count(
            &bytes, "/S /TR
"
        ),
        41
    );
    assert_eq!(
        count(&bytes, "/S /Table"),
        1,
        "one table, however many pages"
    );
}

// ISO 32000-1 14.7.4.4: marked content in a Form XObject is referenced by an MCR with /Stm and
// reached through the form's own /StructParents key.
#[test]
fn text_under_opacity_stays_in_the_structure_tree() {
    let bytes = build(
        r#"<div style="opacity:.5"><h2>Faded</h2><p>text</p></div><p>plain</p>"#,
        true,
    );
    assert_eq!(count(&bytes, "/S /H2"), 1);
    assert_eq!(count(&bytes, "/S /P\n"), 2);
    assert_eq!(
        count(&bytes, "/Stm "),
        2,
        "the heading and paragraph in the form"
    );
    assert_eq!(count(&bytes, "/StructParents "), 2, "the page and the form");
}

// ISO 32000-1 14.7.4.3 / 14.8.4.4: a link annotation is an OBJR child of its Link element and
// has a /StructParent key leading back to it through the parent tree.
#[test]
fn a_link_annotation_belongs_to_its_link_element() {
    let bytes = build(
        r#"<p><a href="https://a.example/">one</a> and <a href="https://b.example/">two</a></p>"#,
        true,
    );
    assert_eq!(count(&bytes, "/Subtype /Link"), 2);
    assert_eq!(count(&bytes, "/Type /OBJR"), 2);
    assert_eq!(count(&bytes, "/StructParent "), 2);
    assert_eq!(
        count(&bytes, "/S /Link"),
        2,
        "no extra Link elements were needed"
    );
}

#[test]
fn a_link_around_an_image_still_gets_a_link_element_for_its_annotation() {
    let html = format!(
        r#"<a href="https://a.example/"><img src="{PIXEL}" alt="x" width="10" height="10"></a>"#
    );
    let bytes = build(&html, true);
    assert_eq!(count(&bytes, "/Subtype /Link"), 1);
    assert_eq!(count(&bytes, "/Type /OBJR"), 1);
    assert_eq!(count(&bytes, "/StructParent "), 1);
}

// Each annotation must land in the Link element over its own text, also when many links on a
// page share a target or there are many links overall (the lookup is indexed by target).
#[test]
fn many_links_each_get_their_own_link_element() {
    let mut html = String::new();
    for i in 0..300 {
        // Every third link repeats the same target.
        let target = if i % 3 == 0 { 0 } else { i };
        html.push_str(&format!(
            r#"<p><a href="https://e.example/{target}">link {i}</a></p>"#
        ));
    }
    let bytes = build(&html, true);
    assert_eq!(count(&bytes, "/Subtype /Link"), 300);
    assert_eq!(count(&bytes, "/Type /OBJR"), 300);
    assert_eq!(
        count(&bytes, "/S /Link"),
        300,
        "none needed a Link element of its own"
    );
}
