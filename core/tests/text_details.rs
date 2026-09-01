//! `text-shadow`/`text-overflow`/`word-break`/`overflow-wrap`/`hyphens`/
//! E2E tests for `text-emphasis`.
//!
//! The same approach as `typography.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, layout, PDF encode).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, LineBox,
    PageSettings,
};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn find_first_tag(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id)
        .find_map(|child| find_first_tag(dom, child, tag))
}

fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            children.iter().find_map(|c| find_laid_out(c, target))
        }
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .find_map(|item| find_laid_out(item, target)),
        _ => None,
    }
}

fn layout(html_src: &str, css: &str) -> (Dom, LaidOutBox) {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );
    (dom, laid)
}

/// Return the lines laid out by the (first) `<p>`.
fn lines_of_first_p(html_src: &str, css: &str) -> Vec<LineBox> {
    let (dom, laid) = layout(html_src, css);
    let p = find_first_tag(&dom, dom.document(), "p").expect("p not found");
    let found = find_laid_out(&laid, p).expect("p should be laid out");
    match &found.content {
        LaidOutContent::Inline(lines) => lines.clone(),
        other => panic!("expected inline content, got {other:?}"),
    }
}

/// The line's text (its runs concatenated).
fn line_text(line: &LineBox) -> String {
    line.runs.iter().map(|run| run.text.as_str()).collect()
}

fn build_pdf(html_src: &str, css: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
    bytes
}

/// A PDF's content stream is FlateDecode compressed, so searching for an operator requires
/// inflating first (the same helper as in `typography.rs`).
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    // "stream" also appears inside an `endstream`, so the scan position advances past the
    // `endstream` (otherwise every other stream would be missed).
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_subslice(&pdf_bytes[i..], b"stream\n") {
        let start = i + pos + b"stream\n".len();
        let Some(end_rel) = find_subslice(&pdf_bytes[start..], b"endstream") else {
            break;
        };
        let end = start + end_rel;
        let mut decoder = flate2::read::ZlibDecoder::new(&pdf_bytes[start..end]);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        }
        i = end + b"endstream".len();
    }
    out
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

// ===== word-break =====

#[test]
fn word_break_break_all_wraps_inside_a_long_word() {
    let css = "body { margin: 0; } p { width: 60px; word-break: break-all; }";
    let lines = lines_of_first_p("<p>abcdefghijklmnopqrstuvwxyz</p>", css);
    assert!(
        lines.len() > 1,
        "break-all should split the long word across lines, got {} line(s)",
        lines.len()
    );
    let joined: String = lines.iter().map(line_text).collect();
    assert_eq!(joined, "abcdefghijklmnopqrstuvwxyz");
}

#[test]
fn a_long_word_stays_on_one_line_by_default() {
    // With `word-break: normal` and `overflow-wrap: normal` (the initial values), a long word
    // is not split and overflows (the behaviour as before).
    let css = "body { margin: 0; } p { width: 60px; }";
    let lines = lines_of_first_p("<p>abcdefghijklmnopqrstuvwxyz</p>", css);
    assert_eq!(lines.len(), 1);
}

#[test]
fn word_break_keep_all_prevents_breaking_between_cjk_characters() {
    let css = "body { margin: 0; } p { width: 60px; word-break: keep-all; }";
    let lines = lines_of_first_p("<p>日本語のテキストです</p>", css);
    assert_eq!(
        lines.len(),
        1,
        "keep-all should not break between CJK characters"
    );
}

#[test]
fn cjk_text_breaks_between_characters_by_default() {
    let css = "body { margin: 0; } p { width: 60px; }";
    let lines = lines_of_first_p("<p>日本語のテキストです</p>", css);
    assert!(
        lines.len() > 1,
        "CJK text should break between characters by default"
    );
}

// ===== overflow-wrap =====

#[test]
fn overflow_wrap_break_word_splits_only_when_it_does_not_fit() {
    let css = "body { margin: 0; } p { width: 60px; overflow-wrap: break-word; }";
    let lines = lines_of_first_p("<p>abcdefghijklmnopqrstuvwxyz</p>", css);
    assert!(
        lines.len() > 1,
        "break-word should split the overflowing word"
    );
    let joined: String = lines.iter().map(line_text).collect();
    assert_eq!(joined, "abcdefghijklmnopqrstuvwxyz");
}

#[test]
fn overflow_wrap_keeps_short_words_intact() {
    // A word that fits is not split (the difference from `word-break: break-all`).
    let css = "body { margin: 0; } p { width: 200px; overflow-wrap: break-word; }";
    let lines = lines_of_first_p("<p>alpha beta gamma</p>", css);
    assert_eq!(lines.len(), 1);
    assert_eq!(line_text(&lines[0]), "alpha beta gamma");
}

#[test]
fn word_wrap_is_accepted_as_a_legacy_alias() {
    let css = "body { margin: 0; } p { width: 60px; word-wrap: break-word; }";
    let lines = lines_of_first_p("<p>abcdefghijklmnopqrstuvwxyz</p>", css);
    assert!(
        lines.len() > 1,
        "word-wrap should behave like overflow-wrap"
    );
}

// ===== hyphens =====

#[test]
fn a_soft_hyphen_is_a_break_opportunity_and_shows_a_hyphen() {
    // U+00AD is not drawn, and a hyphen appears only at the end of the line broken there.
    let css = "body { margin: 0; } p { width: 70px; }";
    let lines = lines_of_first_p("<p>super\u{00AD}califragilistic</p>", css);
    assert!(lines.len() > 1, "should break at the soft hyphen");
    assert!(
        line_text(&lines[0]).ends_with('-'),
        "the broken line should end with a hyphen, got {:?}",
        line_text(&lines[0])
    );
    let joined: String = lines.iter().map(line_text).collect();
    assert!(
        !joined.contains('\u{00AD}'),
        "the soft hyphen itself must never be rendered"
    );
}

#[test]
fn hyphens_none_disables_breaking_at_soft_hyphens() {
    let css = "body { margin: 0; } p { width: 70px; hyphens: none; }";
    let lines = lines_of_first_p("<p>super\u{00AD}califragilistic</p>", css);
    assert_eq!(lines.len(), 1, "hyphens: none should not break");
    assert_eq!(
        line_text(&lines[0]),
        "supercalifragilistic",
        "the soft hyphen must not be rendered"
    );
}

#[test]
fn a_soft_hyphen_that_fits_does_not_show_a_hyphen() {
    let css = "body { margin: 0; } p { width: 400px; }";
    let lines = lines_of_first_p("<p>super\u{00AD}cali</p>", css);
    assert_eq!(lines.len(), 1);
    assert_eq!(line_text(&lines[0]), "supercali");
}

// ===== text-overflow =====

#[test]
fn text_overflow_ellipsis_truncates_an_overflowing_line() {
    let css = "body { margin: 0; } \
               p { width: 80px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }";
    let lines = lines_of_first_p("<p>a very long single line of text</p>", css);
    assert_eq!(lines.len(), 1);
    let text = line_text(&lines[0]);
    assert!(
        text.ends_with('…'),
        "the truncated line should end with an ellipsis, got {text:?}"
    );
    let width: f32 = lines[0]
        .runs
        .last()
        .map(|run| run.x_offset + run.width)
        .unwrap_or(0.0);
    assert!(
        width <= 80.0 + 0.01,
        "the truncated line must fit the content width, got {width}"
    );
}

#[test]
fn text_overflow_needs_a_non_visible_overflow() {
    // With `overflow: visible` (the initial value), `text-overflow` has no effect (as the spec says).
    let css = "body { margin: 0; } \
               p { width: 80px; white-space: nowrap; text-overflow: ellipsis; }";
    let lines = lines_of_first_p("<p>a very long single line of text</p>", css);
    assert!(!line_text(&lines[0]).contains('…'));
}

#[test]
fn text_overflow_clip_leaves_the_line_untouched() {
    let css = "body { margin: 0; } \
               p { width: 80px; white-space: nowrap; overflow: hidden; text-overflow: clip; }";
    let lines = lines_of_first_p("<p>a very long single line of text</p>", css);
    assert!(!line_text(&lines[0]).contains('…'));
}

// ===== text-shadow =====

// A glyph run is always written with `TJ` so advance corrections can be interposed, so the
// number of text drawings is counted from the number of `TJ`s.
#[test]
fn text_shadow_adds_extra_glyph_draws_to_the_content_stream() {
    let plain = decompressed_stream_bytes(&build_pdf("<p>shadowed</p>", "body { margin: 0; }"));
    let shadowed = decompressed_stream_bytes(&build_pdf(
        "<p>shadowed</p>",
        "body { margin: 0; } p { text-shadow: 2px 2px red; }",
    ));
    assert!(
        count_occurrences(&shadowed, b"TJ") > count_occurrences(&plain, b"TJ"),
        "text-shadow should add extra text-showing operators"
    );
}

#[test]
fn a_blurred_text_shadow_draws_more_layers_than_a_sharp_one() {
    let sharp = decompressed_stream_bytes(&build_pdf(
        "<p>shadowed</p>",
        "body { margin: 0; } p { text-shadow: 2px 2px red; }",
    ));
    let blurred = decompressed_stream_bytes(&build_pdf(
        "<p>shadowed</p>",
        "body { margin: 0; } p { text-shadow: 2px 2px 4px red; }",
    ));
    assert!(
        count_occurrences(&blurred, b"TJ") > count_occurrences(&sharp, b"TJ"),
        "a blur radius should be approximated with extra layers"
    );
}

#[test]
fn text_shadow_does_not_affect_layout() {
    let css_plain = "body { margin: 0; } p { width: 200px; }";
    let css_shadow = "body { margin: 0; } p { width: 200px; text-shadow: 4px 4px 2px red; }";
    let plain = lines_of_first_p("<p>alpha beta gamma delta</p>", css_plain);
    let shadowed = lines_of_first_p("<p>alpha beta gamma delta</p>", css_shadow);
    assert_eq!(plain.len(), shadowed.len());
    assert_eq!(plain[0].rect.height, shadowed[0].rect.height);
}

// ===== text-emphasis =====

#[test]
fn text_emphasis_increases_the_line_height() {
    let plain = lines_of_first_p("<p>emphasis</p>", "body { margin: 0; }");
    let marked = lines_of_first_p(
        "<p>emphasis</p>",
        "body { margin: 0; } p { text-emphasis: filled dot red; }",
    );
    assert!(
        marked[0].rect.height > plain[0].rect.height,
        "emphasis marks should make room above the text: {} vs {}",
        marked[0].rect.height,
        plain[0].rect.height
    );
}

#[test]
fn text_emphasis_under_also_increases_the_line_height() {
    let plain = lines_of_first_p("<p>emphasis</p>", "body { margin: 0; }");
    let marked = lines_of_first_p(
        "<p>emphasis</p>",
        "body { margin: 0; } p { text-emphasis: dot; text-emphasis-position: under; }",
    );
    assert!(marked[0].rect.height > plain[0].rect.height);
}

#[test]
fn text_emphasis_marks_are_drawn_as_paths() {
    let plain = decompressed_stream_bytes(&build_pdf("<p>abc</p>", "body { margin: 0; }"));
    let marked = decompressed_stream_bytes(&build_pdf(
        "<p>abc</p>",
        "body { margin: 0; } p { text-emphasis: filled circle; }",
    ));
    // A circle is drawn with four Bezier curves (the `c` operator) per character.
    assert!(
        count_occurrences(&marked, b" c\n") > count_occurrences(&plain, b" c\n"),
        "emphasis marks should add curve operators"
    );
}

#[test]
fn text_emphasis_none_draws_nothing_extra() {
    let plain = decompressed_stream_bytes(&build_pdf("<p>abc</p>", "body { margin: 0; }"));
    let none = decompressed_stream_bytes(&build_pdf(
        "<p>abc</p>",
        "body { margin: 0; } p { text-emphasis: none; }",
    ));
    assert_eq!(
        count_occurrences(&none, b" c\n"),
        count_occurrences(&plain, b" c\n")
    );
}

// ===== E2E =====

#[test]
fn text_details_combined_render_a_valid_pdf_end_to_end() {
    let html_src = "<p class=\"shadow\">shadowed text</p>
        <p class=\"emphasis\">emphasized</p>
        <p class=\"ellipsis\">a very long single line that gets truncated</p>
        <p class=\"breakall\">abcdefghijklmnopqrstuvwxyz</p>
        <p class=\"hyphen\">super\u{00AD}califragilisticexpialidocious</p>";
    let css = "body { margin: 0; } \
        .shadow { text-shadow: 2px 2px 3px rgba(0,0,0,0.5); } \
        .emphasis { text-emphasis: filled sesame red; } \
        .ellipsis { width: 120px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; } \
        .breakall { width: 60px; word-break: break-all; } \
        .hyphen { width: 90px; }";
    build_pdf(html_src, css);
}
