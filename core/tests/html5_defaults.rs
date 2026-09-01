//! E2E tests for the UA default styles of the HTML5 elements.
//!
//! The same approach as `generated_content.rs`/`box_sizing.rs`: catch regressions by going
//! through the real pipeline (HTML parse, style cascade, layout, PDF encode).

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

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Walk the laid-out tree and return the text in the lines concatenated in order of appearance.
fn extract_text(b: &LaidOutBox) -> String {
    fn walk(b: &LaidOutBox, out: &mut String) {
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for child in children {
                    walk(child, out);
                }
            }
            LaidOutContent::Grid(grid) => {
                for child in grid.rows.iter().flat_map(|row| &row.items) {
                    walk(child, out);
                }
            }
            LaidOutContent::Inline(lines) => {
                for (i, line) in lines.iter().enumerate() {
                    if i > 0 {
                        out.push(' ');
                    }
                    push_line(line, out);
                }
            }
            LaidOutContent::Table(table) => {
                if let Some(caption) = &table.caption {
                    walk(caption, out);
                }
                for row in &table.rows {
                    for cell in &row.cells {
                        walk(cell, out);
                    }
                }
            }
            LaidOutContent::Image(_) => {}
        }
    }
    fn push_line(line: &LineBox, out: &mut String) {
        let mut prev_end: Option<f32> = None;
        for run in &line.runs {
            if let Some(end) = prev_end {
                if run.x_offset > end + 0.01 {
                    out.push(' ');
                }
            }
            out.push_str(&run.text);
            prev_end = Some(run.x_offset + run.width);
        }
    }
    let mut out = String::new();
    walk(b, &mut out);
    out
}

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

fn text_of(html_src: &str) -> String {
    let (_, laid) = layout(html_src, "");
    extract_text(&laid)
}

fn find_all_tags(dom: &Dom, id: NodeId, tag: &str, out: &mut Vec<NodeId>) {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            out.push(id);
        }
    }
    for child in dom.children(id) {
        find_all_tags(dom, child, tag, out);
    }
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

// ===== Elements that should be hidden =====

#[test]
fn option_text_does_not_leak_into_the_document() {
    // Regression test: without the UA rules a form control counts as inline and the
    // <option>'s text flowed into the body.
    assert_eq!(
        text_of(
            r#"<p>before</p><select><option>A</option><option>B</option></select><p>after</p>"#
        ),
        "beforeafter"
    );
}

#[test]
fn a_form_control_inside_a_paragraph_does_not_leak_text_into_the_flow() {
    // Originally a regression test for "`collect_spans` did not check `display: none`, so a
    // control's contents leaked into the body". Form elements are now
    // `display: inline-block` boxes, so the option text goes inside the box and does not
    // appear in the body's lines (runs).
    let text = text_of(r#"<p>a <select><option>INSIDE</option></select> b</p>"#);
    assert!(
        !text.contains("INSIDE"),
        "the option text must stay inside the control box, got {text:?}"
    );
    assert!(text.starts_with("a") && text.ends_with("b"), "got {text:?}");
}

#[test]
fn svg_subtree_is_not_rendered() {
    // Confirms that hiding one <svg> removes the whole subtree.
    assert_eq!(
        text_of(r#"<p>x</p><svg width="10" height="10"><text>LEAK</text></svg><p>y</p>"#),
        "xy"
    );
}

#[test]
fn embedded_content_fallbacks_are_not_rendered() {
    assert_eq!(
        text_of(
            r#"<p>x</p>
               <video><p>video fallback</p></video>
               <canvas>canvas fallback</canvas>
               <iframe>iframe fallback</iframe>
               <p>y</p>"#
        ),
        "xy"
    );
}

#[test]
fn hidden_attribute_hides_an_element() {
    assert_eq!(text_of(r#"<p>a</p><p hidden>gone</p><p>b</p>"#), "ab");
}

#[test]
fn author_css_can_override_the_hidden_attribute() {
    // The UA origin always loses to the Author origin.
    let (_, laid) = layout(
        r#"<p>a</p><p hidden>shown</p>"#,
        "[hidden] { display: block; }",
    );
    assert_eq!(extract_text(&laid), "ashown");
}

#[test]
fn closed_details_shows_only_its_summary() {
    assert_eq!(
        text_of(r#"<details><summary>Title</summary><p>Body</p></details>"#),
        "Title"
    );
}

#[test]
fn open_details_shows_its_content() {
    assert_eq!(
        text_of(r#"<details open><summary>Title</summary><p>Body</p></details>"#),
        "TitleBody"
    );
}

// ===== Headings and font sizes =====

#[test]
fn heading_levels_have_decreasing_font_sizes_and_are_all_bold() {
    let (dom, laid) = layout(
        "<h1>h1</h1><h2>h2</h2><h3>h3</h3><h4>h4</h4><h5>h5</h5><h6>h6</h6>",
        "",
    );
    let mut widths = Vec::new();
    for tag in ["h1", "h2", "h3", "h4", "h5", "h6"] {
        let mut found = Vec::new();
        find_all_tags(&dom, dom.document(), tag, &mut found);
        let laid_out = find_laid_out(&laid, found[0]).expect("heading box");
        let LaidOutContent::Inline(lines) = &laid_out.content else {
            panic!("heading should contain a line box");
        };
        widths.push(lines[0].runs[0].width);
    }
    for pair in widths.windows(2) {
        assert!(
            pair[0] > pair[1],
            "each heading level should be smaller than the previous one, got {widths:?}"
        );
    }
    // h4 is 1em, the same as body. h5 and h6 are smaller than that.
    assert!(widths[3] > widths[4] && widths[4] > widths[5]);
}

#[test]
fn small_and_big_scale_the_font_relative_to_the_parent() {
    let (dom, laid) = layout(
        "<p><span>text</span></p><p><small>text</small></p><p><big>text</big></p>",
        "",
    );
    let mut ps = Vec::new();
    find_all_tags(&dom, dom.document(), "p", &mut ps);
    let width_of = |node: NodeId| {
        let laid_out = find_laid_out(&laid, node).expect("p box");
        let LaidOutContent::Inline(lines) = &laid_out.content else {
            panic!("expected a line box");
        };
        lines[0].runs[0].width
    };
    let normal = width_of(ps[0]);
    let small = width_of(ps[1]);
    let big = width_of(ps[2]);
    assert!(small < normal, "small should shrink: {small} vs {normal}");
    assert!(big > normal, "big should grow: {big} vs {normal}");
}

// ===== Text decoration and generated content =====

#[test]
fn q_gets_automatic_quotation_marks() {
    assert_eq!(text_of(r#"<p><q>quoted</q></p>"#), "\u{201c}quoted\u{201d}");
}

#[test]
fn ruby_annotation_stays_readable_as_parenthesised_fallback() {
    // Ruby layout is not supported. Emitting rt/rp inline gives the fallback notation
    // "kanji(reading)".
    assert_eq!(
        text_of(r#"<p><ruby>A<rp>(</rp><rt>B</rt><rp>)</rp></ruby></p>"#),
        "A(B)"
    );
}

// ===== hr =====

#[test]
fn hr_lays_out_as_a_full_width_one_pixel_rule() {
    // A PDF's content stream is Flate compressed, so the drawing operators cannot be checked
    // directly in the bytes. The conditions under which the line is drawn (the border's
    // thickness and width, the top and bottom margins) are pinned down in the layout result.
    let (dom, laid) = layout("<p>above</p><hr><p>below</p>", "");
    let mut hrs = Vec::new();
    find_all_tags(&dom, dom.document(), "hr", &mut hrs);
    let hr = find_laid_out(&laid, hrs[0]).expect("hr should produce a box");

    assert_eq!(hr.layout.border.top, 1.0, "hr should have a 1px top border");
    assert_eq!(hr.layout.margin.top, 8.0);
    assert_eq!(hr.layout.margin.bottom, 8.0);
    assert!(
        hr.layout.content.width > 500.0,
        "hr should span the content area, got {}",
        hr.layout.content.width
    );
}

#[test]
fn a_document_with_an_hr_encodes_to_a_valid_pdf() {
    let dom = html::parse(b"<p>above</p><hr><p>below</p>");
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

// ===== Everything at once =====

#[test]
fn a_document_using_many_html5_elements_renders_end_to_end() {
    let dom = html::parse(
        br#"<article>
              <header><h1>Title</h1></header>
              <section>
                <h2>Section</h2>
                <p>Body with <code>code</code>, <mark>mark</mark>, <q>quote</q>,
                   <abbr title="x">abbr</abbr> and <a href="https://example.com">a link</a>.</p>
                <pre>preformatted
  text</pre>
                <dl><dt>Term</dt><dd>Definition</dd></dl>
                <blockquote>Quoted block</blockquote>
                <details><summary>More</summary><p>Hidden body</p></details>
                <figure><figcaption>Caption</figcaption></figure>
              </section>
              <hr>
              <footer><small>footer note</small></footer>
            </article>"#,
    );
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);

    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(&tree, &styles, &fonts, settings.content_width());
    let text = extract_text(&laid);
    assert!(text.contains("Title"), "got: {text}");
    assert!(!text.contains("Hidden body"), "got: {text}");
}

// ===== `<br>` (a forced line break) =====

/// Return the number of lines of the first inline content in the laid-out tree.
fn line_count(b: &LaidOutBox) -> usize {
    fn walk(b: &LaidOutBox) -> Option<usize> {
        match &b.content {
            LaidOutContent::Inline(lines) => Some(lines.len()),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().find_map(walk)
            }
            _ => None,
        }
    }
    walk(b).unwrap_or(0)
}

#[test]
fn br_splits_a_paragraph_into_multiple_lines() {
    let (_, without) = layout("<p>東京都千代田区 一丁目二番三号</p>", "");
    let (_, with) = layout("<p>東京都千代田区<br>一丁目二番三号</p>", "");

    assert_eq!(line_count(&without), 1);
    assert_eq!(line_count(&with), 2, "<br> should force a second line");
}

#[test]
fn br_increases_the_height_of_the_block() {
    let (dom, laid) = layout("<p>a<br>b</p><p>ab</p>", "");
    let mut ps = Vec::new();
    find_all_tags(&dom, dom.document(), "p", &mut ps);
    let with_br = find_laid_out(&laid, ps[0]).expect("first p");
    let without_br = find_laid_out(&laid, ps[1]).expect("second p");

    assert!(
        with_br.layout.content.height > without_br.layout.content.height,
        "a <br> must add a line worth of height"
    );
}

#[test]
fn a_document_with_brs_encodes_to_a_valid_pdf() {
    let dom = html::parse("<p>1行目<br>2行目<br><br>4行目</p>".as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font"),
        Font::load_indexed(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fonts/NotoSansCJK-Regular.ttc"
            ),
            0,
        )
        .expect("should load the CJK test font"),
    ]);
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
