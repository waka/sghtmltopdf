//! E2E tests for Unicode whitespace characters, starting with `&nbsp;`.
//!
//! The same approach as `typography.rs`: checked through the real pipeline (HTML parse,
//! style cascade, layout). The criteria are the same two axes as `layout::white_space`:
//!
//!
//! - Only what CSS Text 3 covers (space, tab, newline) collapses; every other whitespace is
//!   an ordinary character drawn at the font's own advance.
//! - Where a break is allowed follows the UAX #14 line breaking classes (not at `&nbsp;`;
//!   allowed right after a thin space or a ZWSP).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, PageSettings,
};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

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
        return children.iter().find_map(|c| find_laid_out(c, target));
    }
    None
}

/// Return the lines the first `<p>` laid out, as a list of `(width, text)`.
fn p_lines(html_src: &str, css: &str) -> Vec<(f32, String)> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ]);
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p should be laid out");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    lines
        .iter()
        .map(|line| {
            (
                line.rect.width,
                line.runs.iter().map(|r| r.text.as_str()).collect(),
            )
        })
        .collect()
}

/// The width when laid out on one line at a width that does not wrap.
fn width_of(html_src: &str) -> f32 {
    let lines = p_lines(html_src, "body { margin: 0; } p { margin: 0; }");
    assert_eq!(lines.len(), 1, "expected a single line, got {lines:?}");
    lines[0].0
}

/// The number of lines when laid out in a 40px-wide `<p>` (to see whether a break opportunity exists).
fn narrow_lines(html_src: &str) -> Vec<(f32, String)> {
    p_lines(
        html_src,
        "body { margin: 0; } p { margin: 0; width: 40px; }",
    )
}

// ===== Collapsing =====

#[test]
fn runs_of_ordinary_spaces_still_collapse_into_one() {
    assert_eq!(
        width_of("<p>a   b</p>"),
        width_of("<p>a b</p>"),
        "space/tab/newline are the only collapsible characters"
    );
    assert_eq!(width_of("<p>a \t\n b</p>"), width_of("<p>a b</p>"));
}

#[test]
fn a_run_of_no_break_spaces_does_not_collapse() {
    // `&nbsp;&nbsp;&nbsp;` occupies the width of three spaces (used for aligning columns).
    // Back when it collapsed, it came out the same width as a single ordinary space.
    let one_space = width_of("<p>a b</p>");
    let three_nbsp = width_of("<p>a\u{a0}\u{a0}\u{a0}b</p>");

    assert!(
        three_nbsp > one_space + 1.0,
        "three &nbsp; must be wider than a single space ({three_nbsp} vs {one_space})"
    );
}

#[test]
fn a_no_break_space_is_as_wide_as_a_space() {
    // In DejaVu Sans the advances of `&nbsp;` and a space are equal. Even in a font lacking
    // the glyph, the shaper's space fallback assigns the same width.
    assert_eq!(width_of("<p>a\u{a0}b</p>"), width_of("<p>a b</p>"));
}

#[test]
fn fixed_width_spaces_keep_their_own_advance() {
    // A typographic space is not levelled to "one space" and is drawn at its own width.
    let none = width_of("<p>ab</p>");
    let hair = width_of("<p>a\u{200a}b</p>");
    let thin = width_of("<p>a\u{2009}b</p>");
    let space = width_of("<p>a b</p>");
    let em = width_of("<p>a\u{2003}b</p>");

    assert!(
        none < hair && hair < thin && thin < space && space < em,
        "expected none < hair < thin < space < em, got \
         {none} / {hair} / {thin} / {space} / {em}"
    );
}

#[test]
fn a_zero_width_space_takes_no_room() {
    assert_eq!(
        width_of("<p>a\u{200b}b</p>"),
        width_of("<p>ab</p>"),
        "U+200B must not add width"
    );
}

// ===== Break opportunities (UAX #14) =====

#[test]
fn a_no_break_space_does_not_offer_a_wrap_opportunity() {
    // "10 kg" can wrap, but "10&nbsp;kg" stays on one line (overflowing rather than being
    // split). `&nbsp;` is placed for exactly this.
    assert_eq!(narrow_lines("<p>10 kg</p>").len(), 2);

    let glued = narrow_lines("<p>10\u{a0}kg</p>");
    assert_eq!(glued.len(), 1, "&nbsp; must not break, got {glued:?}");
    assert_eq!(glued[0].1, "10\u{a0}kg");
}

#[test]
fn the_other_non_breaking_spaces_do_not_wrap_either() {
    for (name, ch) in [
        ("NARROW NO-BREAK SPACE", '\u{202f}'),
        ("FIGURE SPACE", '\u{2007}'),
        ("WORD JOINER", '\u{2060}'),
    ] {
        let lines = narrow_lines(&format!("<p>10{ch}kg</p>"));
        assert_eq!(lines.len(), 1, "{name} must not break, got {lines:?}");
    }
}

#[test]
fn a_thin_space_offers_a_wrap_opportunity() {
    // A break is allowed right after a whitespace of UAX #14 class BA.
    let lines = narrow_lines("<p>10\u{2009}kg</p>");
    assert_eq!(lines.len(), 2, "thin space should break, got {lines:?}");
    assert_eq!(lines[1].1, "kg", "the break belongs after the space");
}

#[test]
fn a_zero_width_space_offers_a_wrap_opportunity_inside_a_word() {
    // A ZWSP adds only a break opportunity, with no width (the `<wbr>` use).
    let broken = narrow_lines("<p>aaaaaa\u{200b}bbbbbb</p>");
    let unbroken = narrow_lines("<p>aaaaaabbbbbb</p>");

    assert_eq!(broken.len(), 2, "U+200B should break, got {broken:?}");
    assert_eq!(
        unbroken.len(),
        1,
        "without U+200B the long word overflows instead, got {unbroken:?}"
    );
}

#[test]
fn a_no_break_space_stays_glued_even_under_word_break_break_all() {
    // `word-break: break-all` allows a break anywhere, but the binding `&nbsp;` was placed
    // for wins over it (browsers do the same).
    let lines = p_lines(
        "<p>10\u{a0}kg</p>",
        "body { margin: 0; } p { margin: 0; width: 40px; word-break: break-all; }",
    );

    assert!(
        lines.iter().any(|(_, text)| text.contains("0\u{a0}k")),
        "the characters around the &nbsp; must stay on one line, got {lines:?}"
    );
}

// ===== `<wbr>` =====

#[test]
fn wbr_offers_a_wrap_opportunity_inside_a_long_word() {
    // The HTML spec's "line break opportunity". Used to wrap a long identifier or URL at a
    // chosen position.
    let broken = narrow_lines("<p>aaaaaa<wbr>bbbbbb</p>");
    let unbroken = narrow_lines("<p>aaaaaabbbbbb</p>");

    assert_eq!(broken.len(), 2, "<wbr> should break, got {broken:?}");
    assert_eq!(broken[0].1, "aaaaaa");
    assert_eq!(broken[1].1, "bbbbbb");
    assert_eq!(
        unbroken.len(),
        1,
        "without <wbr> the long word overflows instead, got {unbroken:?}"
    );
}

#[test]
fn wbr_adds_no_width_when_the_line_does_not_wrap() {
    assert_eq!(
        width_of("<p>aaa<wbr>bbb</p>"),
        width_of("<p>aaabbb</p>"),
        "<wbr> must not change the width of a line that fits"
    );
}

#[test]
fn wbr_only_offers_a_break_it_does_not_force_one() {
    // The difference from `<br>`: while it fits it stays on one line.
    let lines = p_lines("<p>aaa<wbr>bbb</p>", "body { margin: 0; } p { margin: 0; }");
    assert_eq!(lines.len(), 1, "<wbr> is not a forced break, got {lines:?}");
}

#[test]
fn wbr_survives_word_break_keep_all() {
    // `word-break: keep-all` means "do not break within a word", but a `<wbr>` is an
    // explicitly placed break opportunity and still takes effect (UAX #14 also treats the ZW
    // class independently of word boundaries).
    let lines = p_lines(
        "<p>aaaaaa<wbr>bbbbbb</p>",
        "body { margin: 0; } p { margin: 0; width: 40px; word-break: keep-all; }",
    );

    assert_eq!(lines.len(), 2, "<wbr> should still break, got {lines:?}");
}

/// Inflate and concatenate every PDF stream (to look inside the `/ToUnicode` CMap; the same
/// procedure as `typography.rs`).
fn decompressed_streams(html_src: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(
        &dom,
        &user_agent_stylesheet(),
        &parse_stylesheet("body { margin: 0; }"),
    );
    let fonts = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ]);
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find(&bytes[i..], b"stream\n") {
        let start = i + pos + b"stream\n".len();
        let Some(end_rel) = find(&bytes[start..], b"endstream") else {
            break;
        };
        let end = start + end_rel;
        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[start..end]);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        }
        i = end + b"endstream".len();
    }
    out
}

#[test]
fn wbr_leaves_no_character_in_the_laid_out_text() {
    // A `<wbr>` is a break opportunity, not a character. Passing a ZWSP through as a
    // character makes a font lacking the ZWSP glyph substitute the space glyph, putting a
    // ghost space in the PDF's text layer (extracting gives `inline word`).
    for html_src in [
        "<p>inline<wbr>word</p>",
        "<p>a<wbr>b<wbr>c</p>",
        "<p>\u{200b}text</p>",
    ] {
        let lines = p_lines(html_src, "body { margin: 0; } p { margin: 0; }");
        assert!(
            lines.iter().all(|(_, text)| !text.contains('\u{200b}')),
            "no run may carry a U+200B for {html_src}, got {lines:?}"
        );
    }
    assert_eq!(
        p_lines("<p>inline<wbr>word</p>", "body { margin: 0; }")[0].1,
        "inlineword",
        "the text either side of a <wbr> is contiguous"
    );
}

#[test]
fn wbr_leaves_nothing_behind_in_the_pdf_text_layer() {
    // Regression test: back when a `<wbr>` was passed through as a ZWSP "character", a font
    // lacking the ZWSP glyph substituted the space glyph and `/ToUnicode` mapped that glyph
    // to U+200B. As a result every space in the document extracted as U+200B, breaking copy
    // and paste and text search.
    let pdf = decompressed_streams("<p>inline<wbr>word stays</p>");
    let text = String::from_utf8_lossy(&pdf);

    assert!(
        !text.contains("<200B>"),
        "<wbr> must not reach the /ToUnicode CMap"
    );
    assert!(
        text.contains("<0020>"),
        "the space glyph must still map to U+0020, got:\n{text}"
    );
}

// ===== Box generation =====

#[test]
fn a_paragraph_holding_only_a_no_break_space_still_produces_a_line() {
    // Whitespace-only text creates no box, but `&nbsp;` is content and becomes a line.
    let blank = p_lines("<p> \n </p>", "body { margin: 0; }");
    let nbsp = p_lines("<p>\u{a0}</p>", "body { margin: 0; }");

    assert!(blank.is_empty(), "collapsible whitespace makes no line");
    assert_eq!(nbsp.len(), 1, "&nbsp; is content and makes a line");
}
