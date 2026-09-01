//! `text-align`/`line-height`/`text-indent`/`white-space`/`letter-spacing`/
//! E2E tests for `word-spacing`/`text-transform`.
//!
//! The same approach as `fragmentation.rs`/`float_position.rs`: catch regressions by going
//! through the real pipeline (HTML parse, style cascade, pagination, PDF encode). The
//! detailed coordinate checks run against the result of `layout_document` (before
//! pagination), and `build_pdf` separately confirms that the whole pipeline through to PDF
//! encoding does not crash and produces valid output.

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, PageSettings,
};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
/// A font whose ascent plus descent exceeds 1.2em (1.448em). Approximating
/// `line-height: normal` with a fixed multiplier would let its glyphs spill out of the line box.
const TALL_METRICS_FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/NotoSansCJK-Regular.ttc"
);

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

/// A PDF's content stream is FlateDecode compressed, so searching for an operator inside it
/// such as `Tc` requires inflating first
/// (the same logic as the identically named function in the `pdf::document` test module).
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_subslice(&pdf_bytes[i..], b"stream\n") {
        let start = i + pos + b"stream\n".len();
        let Some(end_rel) = find_subslice(&pdf_bytes[start..], b"\nendstream") else {
            break;
        };
        let end = start + end_rel;
        let raw = &pdf_bytes[start..end];

        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        } else {
            out.extend_from_slice(raw);
        }
        i = end + b"\nendstream".len();
    }
    out
}

/// Run the whole real pipeline (parse, cascade, pagination, PDF encode) from HTML plus CSS
/// (the same as `fragmentation.rs::build_pdf`).
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

/// The shared helper running as far as `layout_document` (before pagination).
fn layout(html_src: &str, css: &str) -> (Dom, LaidOutBox) {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
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

#[test]
fn text_align_right_shifts_the_line_end_to_end() {
    let html_src = r#"<p class="text">hi</p>"#;

    let (dom, laid) = layout(
        html_src,
        "body { margin: 0; } .text { width: 300px; margin: 0; }",
    );
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let left_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(left_lines) = &left_box.content else {
        panic!("expected inline content");
    };
    assert_eq!(left_lines[0].runs[0].x_offset, 0.0);

    let (dom, laid) = layout(
        html_src,
        "body { margin: 0; } .text { width: 300px; margin: 0; text-align: right; }",
    );
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let right_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(right_lines) = &right_box.content else {
        panic!("expected inline content");
    };
    // text-align's effect shows up in each run's `x_offset` rather than the line's band
    // (`rect.x`), which stays the left edge used for the float band calculation.
    assert!(right_lines[0].runs[0].x_offset > 0.0);

    let (page_count, bytes) = build_pdf(
        html_src,
        "body { margin: 0; } .text { width: 300px; margin: 0; text-align: right; }",
    );
    assert_eq!(page_count, 1);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn text_align_justify_stretches_non_last_lines_end_to_end() {
    let html_src = r#"<p class="text">hello world foo bar baz qux quux corge grault garply</p>"#;
    let css = "body { margin: 0; } .text { width: 150px; margin: 0; text-align: justify; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert!(lines.len() >= 2, "expected wrapping to at least 2 lines");
    assert_eq!(
        lines[0].rect.width, 150.0,
        "the first (non-last) line should stretch to fill the available width"
    );
    assert!(
        lines.last().unwrap().rect.width < 150.0,
        "the last line should not be stretched"
    );

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

#[test]
fn line_height_number_increases_line_spacing_end_to_end() {
    let html_src = r#"<p class="text">line one and two and three and four words</p>"#;
    let narrow_css = "body { margin: 0; } .text { width: 100px; margin: 0; }";

    let (dom, laid) = layout(html_src, narrow_css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(normal_lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert!(normal_lines.len() >= 2);
    let normal_gap = normal_lines[1].rect.y - normal_lines[0].rect.y;

    let tall_css = "body { margin: 0; } .text { width: 100px; margin: 0; line-height: 3; }";
    let (dom, laid) = layout(html_src, tall_css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(tall_lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert!(tall_lines.len() >= 2);
    let tall_gap = tall_lines[1].rect.y - tall_lines[0].rect.y;

    assert!(
        tall_gap > normal_gap * 2.0,
        "line-height: 3 should noticeably widen line spacing (normal={normal_gap}, tall={tall_gap})"
    );

    let (page_count, _) = build_pdf(html_src, tall_css);
    assert_eq!(page_count, 1);
}

#[test]
fn text_indent_offsets_only_the_first_line_end_to_end() {
    let html_src = r#"<p class="text">hello world foo bar baz qux quux</p>"#;
    let css = "body { margin: 0; } .text { width: 100px; margin: 0; text-indent: 20px; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert!(lines.len() >= 2);
    assert_eq!(lines[0].rect.x, 20.0);
    assert_eq!(lines[1].rect.x, 0.0);

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

#[test]
fn white_space_pre_preserves_formatting_end_to_end() {
    let html_src = "<pre>a    b\nc    d</pre>";

    let (dom, laid) = layout(html_src, "body { margin: 0; }");
    let pre = find_tag(&dom, dom.document(), "pre").expect("pre not found");
    let pre_box = find_laid_out(&laid, pre).expect("pre box not found");
    let LaidOutContent::Inline(lines) = &pre_box.content else {
        panic!("expected inline content");
    };
    assert_eq!(lines.len(), 2, "explicit newline should split into 2 lines");
    let first_text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(
        first_text, "a    b",
        "runs of whitespace should be preserved"
    );

    let (page_count, bytes) = build_pdf(html_src, "body { margin: 0; }");
    assert_eq!(page_count, 1);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn white_space_nowrap_overflows_instead_of_wrapping_end_to_end() {
    let html_src = r#"<p class="text">hello world foo bar</p>"#;
    let css = "body { margin: 0; } .text { width: 60px; margin: 0; white-space: nowrap; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert_eq!(lines.len(), 1);
    assert!(lines[0].rect.width > 60.0);

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

#[test]
fn letter_spacing_and_word_spacing_widen_layout_and_emit_tc_end_to_end() {
    let html_src = r#"<p class="text">hi there</p>"#;
    let css = "p.text { letter-spacing: 4px; word-spacing: 10px; }";

    let (page_count, bytes) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
    assert!(
        count_occurrences(&decompressed_stream_bytes(&bytes), b" Tc\n") > 0,
        "letter-spacing should emit a Tc operator in the final PDF"
    );
}

#[test]
fn text_transform_uppercase_applies_end_to_end() {
    let html_src = r#"<p class="text">hello</p>"#;
    let css = "p.text { text-transform: uppercase; }";

    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(text, "HELLO");

    let (page_count, _) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
}

/// Return the width of the single line the first `<p>` laid out, and that line's run count.
fn first_p_line(html_src: &str, css: &str) -> (f32, usize) {
    let (dom, laid) = layout(html_src, css);
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    assert_eq!(lines.len(), 1, "expected a single line, got {lines:?}");
    (lines[0].rect.width, lines[0].runs.len())
}

#[test]
fn whitespace_between_inline_elements_still_separates_the_words_end_to_end() {
    // Regression test (issue #3): the whitespace between inline elements was discarded and
    // `<span>one</span> <span>two</span>` came out as `onetwo`. Inter-word whitespace appears
    // as a run's x offset, so this is checked by the line width.
    let css = "body { margin: 0; } p { margin: 0; }";
    let (separate, separate_runs) = first_p_line("<p><span>one</span> <span>two</span></p>", css);
    let (plain, _) = first_p_line("<p>one two</p>", css);
    let (joined, _) = first_p_line("<p>onetwo</p>", css);

    assert_eq!(separate_runs, 2, "one run per <span>");
    assert!(
        (separate - plain).abs() < 0.01,
        "should be as wide as the same text without elements: {separate} vs {plain}"
    );
    assert!(
        separate > joined + 1.0,
        "the word gap must be there: {separate} vs {joined} without a space"
    );
}

#[test]
fn trailing_whitespace_after_an_inline_element_does_not_widen_the_line_end_to_end() {
    // Trailing whitespace survives as a span but must not affect the line's width
    // (or `text-align: right`/`justify` would be off).
    let css = "body { margin: 0; } p { margin: 0; }";
    let (with_whitespace, _) = first_p_line("<p><span>one</span>\n</p>", css);
    let (without, _) = first_p_line("<p><span>one</span></p>", css);

    assert_eq!(
        with_whitespace, without,
        "trailing whitespace must not add width"
    );
}

#[test]
fn a_non_breaking_space_between_inline_elements_still_separates_the_words_end_to_end() {
    // Also issue #3. `&nbsp;` makes `char::is_whitespace` true too, so it was discarded along
    // with the whitespace-only text nodes.
    let css = "body { margin: 0; } p { margin: 0; }";
    let (nbsp, _) = first_p_line("<p><span>one</span>\u{a0}<span>two</span></p>", css);
    let (joined, _) = first_p_line("<p>onetwo</p>", css);

    assert!(
        nbsp > joined + 1.0,
        "&nbsp; must separate the words: {nbsp} vs {joined} without a space"
    );
}

#[test]
fn combined_typography_properties_render_a_valid_pdf_end_to_end() {
    // justify + line-height + text-indent + letter-spacing + white-space: pre
    // combined into one document, the whole pipeline does not fall apart.
    let html_src = r#"<div>
        <p class="justified">hello world foo bar baz qux quux corge grault garply</p>
        <pre class="preformatted">line one
line   two</pre>
    </div>"#;
    let css = "body { margin: 0; } \
               .justified { width: 150px; margin: 0; text-align: justify; \
                            line-height: 1.5; text-indent: 10px; letter-spacing: 1px; }";

    let (page_count, bytes) = build_pdf(html_src, css);
    assert_eq!(page_count, 1);
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&decompressed_stream_bytes(&bytes), b" Tc\n") > 0);
}

/// Return `tag`'s first box from the result of laying out with `fonts`.
fn layout_with(html_src: &str, css: &str, fonts: &FontCollection) -> (Dom, LaidOutBox) {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        fonts,
        PageSettings::default().content_width(),
    );
    (dom, laid)
}

/// Collect every line under `b`.
fn collect_lines(b: &LaidOutBox, out: &mut Vec<sghtmltopdf_core::layout::LineBox>) {
    match &b.content {
        LaidOutContent::Inline(lines) => out.extend(lines.iter().cloned()),
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_lines(child, out);
            }
        }
        LaidOutContent::Table(table) => {
            for cell in table.rows.iter().flat_map(|row| &row.cells) {
                collect_lines(cell, out);
            }
        }
        _ => {}
    }
}

#[test]
fn line_height_normal_fits_the_glyphs_of_a_font_taller_than_1_2em() {
    // Approximating `line-height: normal` as font-size*1.2 lets the glyphs of a font whose
    // ascent plus descent exceeds 1.2em spill out of the line box, so the last line of a
    // stack of blocks overlaps the parent's bottom edge (a cell's border-bottom, say).
    // The overflow is proportional to the font-size, so it stands out in a sequence of
    // increasing font sizes.
    let fonts = FontCollection::new(vec![
        Font::load(TALL_METRICS_FONT_PATH).expect("should load the CJK test font")
    ]);
    let html_src = r#"<table><tr><td>
        <div class="small">Label</div>
        <div class="large">Value</div>
    </td></tr></table>"#;
    let css = "body { margin: 0; } td { padding: 0; } \
               .small { font-size: 9px; } .large { font-size: 11px; }";

    let (_, laid) = layout_with(html_src, css, &fonts);
    let mut lines = Vec::new();
    collect_lines(&laid, &mut lines);
    assert_eq!(lines.len(), 2, "expected one line per div");

    for line in &lines {
        let descent = line
            .runs
            .iter()
            .map(|run| run.descent)
            .fold(0.0f32, f32::max);
        let glyph_bottom = line.baseline + descent;
        assert!(
            glyph_bottom <= line.rect.height + 0.01,
            "glyphs must fit inside their line box: bottom={glyph_bottom} height={}",
            line.rect.height
        );
        let ascent = line
            .runs
            .iter()
            .map(|run| run.ascent)
            .fold(0.0f32, f32::max);
        assert!(
            line.baseline >= ascent - 0.01,
            "the baseline must leave room for the ascent: baseline={} ascent={ascent}",
            line.baseline
        );
    }
}

#[test]
fn line_height_normal_follows_the_fonts_own_metrics() {
    // `normal` differs per font. A font with larger metrics gives a taller line at the same
    // font-size.
    let html_src = r#"<p>x</p>"#;
    let css = "body { margin: 0; } p { margin: 0; font-size: 10px; }";

    let dejavu = FontCollection::new(vec![Font::load(FONT_PATH).unwrap()]);
    let tall = FontCollection::new(vec![Font::load(TALL_METRICS_FONT_PATH).unwrap()]);

    let mut heights = Vec::new();
    for fonts in [&dejavu, &tall] {
        let (dom, laid) = layout_with(html_src, css, fonts);
        let p = find_tag(&dom, dom.document(), "p").expect("p not found");
        let p_box = find_laid_out(&laid, p).expect("p box not found");
        let LaidOutContent::Inline(lines) = &p_box.content else {
            panic!("expected inline content");
        };
        let font = fonts.get(0).unwrap();
        assert!(
            (lines[0].rect.height - font.normal_line_height(10.0)).abs() < 0.01,
            "the line height should be the font's own normal line height: {} vs {}",
            lines[0].rect.height,
            font.normal_line_height(10.0)
        );
        heights.push(lines[0].rect.height);
    }

    assert!(
        heights[1] > heights[0],
        "the font with taller metrics should produce a taller line: {heights:?}"
    );
}
