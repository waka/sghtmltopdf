//! Golden PDF comparison tests, one per page-break pattern.
//!
//! Where the unit tests in `paginate.rs` cover the page-breaking algorithm itself
//! exhaustively, these go through the whole real pipeline (HTML parse, style cascade,
//! pagination, PDF encode) to confirm that each page-break pattern is reflected correctly in
//! the final PDF output, and to catch future regressions.
//!
//! The comparison granularity is the page count derived from the number of `/MediaBox`
//! occurrences, rather than a byte-for-byte match of the PDF (which is fragile, easily
//! shifted by font embedding or object number allocation). It also confirms that the page
//! count `paginate_document` returns matches the page count of the PDF actually written.
//! Detailed checks of patterns such as `break-inside: avoid`, where "the page count does not
//! change but the placement does", are left to the unit tests in `paginate.rs`; here we only
//! confirm that the whole pipeline does not crash and produces a PDF with a sensible page count.

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, PageSettings,
};
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

fn page_count_in_pdf(bytes: &[u8]) -> usize {
    count_occurrences(bytes, b"/MediaBox")
}

/// Run the whole real pipeline (parse, cascade, pagination, PDF encode) from HTML plus CSS.
/// Returns both the page count `paginate_document` returns and the page count derived from
/// the PDF bytes actually written.
fn build_pdf(html_src: &str, css: &str) -> (usize, Vec<u8>) {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let engine_page_count = pages.len();
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    assert_eq!(
        page_count_in_pdf(&bytes),
        engine_page_count,
        "PDF page count should match the layout engine's own page count"
    );

    (engine_page_count, bytes)
}

#[test]
fn break_before_always_forces_a_second_page_end_to_end() {
    let html_src = r#"<div><p class="a">A</p><p class="b">B</p></div>"#;

    let (without, _) = build_pdf(html_src, ".a, .b { height: 50px; margin: 0; }");
    assert_eq!(
        without, 1,
        "without break-before, both tiny paragraphs should fit on one page"
    );

    let (with, _) = build_pdf(
        html_src,
        ".a, .b { height: 50px; margin: 0; } \
         .b { break-before: always; }",
    );
    assert_eq!(
        with, 2,
        "break-before: always should force a second page end-to-end"
    );
}

#[test]
fn break_after_always_forces_a_second_page_end_to_end() {
    let html_src = r#"<div><p class="a">A</p><p class="b">B</p></div>"#;

    let (without, _) = build_pdf(html_src, ".a, .b { height: 50px; margin: 0; }");
    assert_eq!(without, 1);

    let (with, _) = build_pdf(
        html_src,
        ".a, .b { height: 50px; margin: 0; } \
         .a { break-after: always; }",
    );
    assert_eq!(
        with, 2,
        "break-after: always should force a second page end-to-end"
    );
}

#[test]
fn break_inside_avoid_renders_a_valid_multi_page_pdf_end_to_end() {
    // break-inside: avoid usually only changes "what goes on which page" without changing
    // the total page count (see the unit tests in `paginate.rs` for the detailed checks).
    // Here we confirm that the whole pipeline does not fall apart on this combination of CSS
    // and gives the expected page count.
    let settings = PageSettings::default();
    let filler_height = settings.content_height() - 200.0;
    let html_src = r#"<div class="filler"></div>
           <div class="wrapper">
               <p class="a">A</p><p class="b">B</p><p class="c">C</p><p class="d">D</p>
           </div>"#;
    let css = format!(
        ".filler {{ height: {filler_height}px; margin: 0; }} \
         .wrapper {{ break-inside: avoid; margin: 0; }} \
         .a, .b, .c, .d {{ height: 100px; margin: 0; }}"
    );

    let (page_count, bytes) = build_pdf(html_src, &css);
    assert_eq!(page_count, 2);
    assert!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0,
        "font should still be embedded"
    );
}

/// Measure how a paragraph of `word_count` words breaks into lines at an explicit `width`
/// (px): the line count, and that the line heights are uniform. The same idea as the
/// identically named helper in `paginate.rs`'s unit tests: work back from that uniform line
/// height to the `filler` height, to aim at a specific natural break point within the page.
fn measure_paragraph_lines(word_count: usize, width: f32) -> (usize, f32) {
    let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
    let html_src = format!(r#"<p class="target">{}</p>"#, words.join(" "));
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(&format!(".target {{ width: {width}px; margin: 0; }}"));
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );

    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    let height = lines[0].rect.height;
    assert!(
        lines.iter().all(|l| (l.rect.height - height).abs() < 0.01),
        "this test relies on every wrapped line having the same height"
    );
    (lines.len(), height)
}

fn find_tag(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id).find_map(|child| find_tag(dom, child, tag))
}

fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    if let LaidOutContent::Blocks(children) = &b.content {
        for child in children {
            if let Some(found) = find_laid_out(child, target) {
                return Some(found);
            }
        }
    }
    None
}

#[test]
fn orphans_forces_a_second_page_end_to_end() {
    // Re-confirm through the whole pipeline (as far as PDF encoding) the same scenario as the
    // unit test in `paginate.rs`
    // (`orphans_defers_the_whole_paragraph_when_too_few_lines_would_fit`): naturally only one
    // line fits, which cannot satisfy orphans: 3.
    //
    // The total page count alone cannot prove that orphans really took effect (with no line
    // fitting at all it would split into two pages either way). The detailed check of how the
    // within-page placement changed is left to the unit tests in `paginate.rs`; here we only
    // catch regressions in "the whole pipeline does not fall apart on this combination and
    // gives the expected page count".

    let word_count = 60;
    let width = 200.0;
    let (n, line_height) = measure_paragraph_lines(word_count, width);
    assert!(n >= 4, "expected several wrapped lines, got {n}");

    let settings = PageSettings::default();
    let target_fit = 1usize;
    let orphans = 3;
    let desired_remaining = (target_fit as f32 + 0.5) * line_height;
    let filler_height = settings.content_height() - 8.0 - desired_remaining;

    let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
    let html_src = format!(
        r#"<div class="filler"></div><p class="target">{}</p>"#,
        words.join(" ")
    );
    let css = format!(
        ".filler {{ height: {filler_height}px; margin: 0; }} \
         .target {{ width: {width}px; margin: 0; orphans: {orphans}; }}"
    );

    let (page_count, _) = build_pdf(&html_src, &css);
    assert_eq!(
        page_count, 2,
        "orphans: {orphans} should force the whole paragraph onto a second page end-to-end"
    );
}

#[test]
fn widows_forces_lines_forward_end_to_end() {
    let word_count = 60;
    let width = 200.0;
    let (n, line_height) = measure_paragraph_lines(word_count, width);
    assert!(n >= 8, "expected several wrapped lines, got {n}");

    let settings = PageSettings::default();
    // Naturally (n - 1) lines fit on this page and only one is left for the next (which
    // cannot satisfy widows: 3, so the break point should be brought forward). As with the
    // orphans test, this is not a detailed check of the break point but a regression check
    // that the pipeline does not fall apart and gives the expected page count.
    let target_fit = n - 1;
    let desired_remaining = (target_fit as f32 + 0.5) * line_height;
    let filler_height = settings.content_height() - 8.0 - desired_remaining;

    let words: Vec<String> = (0..word_count).map(|i| format!("word{i}")).collect();
    let html_src = format!(
        r#"<div class="filler"></div><p class="target">{}</p>"#,
        words.join(" ")
    );
    let css = format!(
        ".filler {{ height: {filler_height}px; margin: 0; }} \
         .target {{ width: {width}px; margin: 0; widows: 3; }}"
    );

    let (page_count, _) = build_pdf(&html_src, &css);
    assert_eq!(
        page_count, 2,
        "the paragraph should still split across exactly two pages"
    );
}
