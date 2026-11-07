//! E2E tests for `content` (attr()/counter()/counters()), the CSS counters, `quotes` and
//! `::first-letter`.
//!
//! The same approach as `list_style.rs`/`box_model.rs`: catch regressions by going through
//! the real pipeline (HTML parse, style cascade, layout, PDF encode).

use std::collections::HashMap;

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, LineBox,
    PageSettings, TextRun,
};
use sghtmltopdf::pdf::encode_pdf;
use sghtmltopdf::style::{
    compute_styles, parse_stylesheet, user_agent_stylesheet, ComputedStyle, RgbaColor,
};

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

    (engine_page_count, bytes)
}

/// Walk the laid-out tree and return the text in the lines concatenated in order of
/// appearance (markers excluded). Generated content sits on the same lines as ordinary
/// spans, so this one helper covers `::before`, `::after` and `::first-letter` alike.
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
                // Lines wrapped within the same inline box were originally joined by a single
                // space, so a space is put back between them. Between separate boxes (a block
                // boundary) nothing is added (a block boundary corresponds to a line break,
                // not a space).
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
        // Inter-word whitespace is expressed as the gap between runs and is not part of
        // `run.text` (as `layout::inline` specifies). The word-boundary space is recovered from the x_offset gap.
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

fn layout(html_src: &str, css: &str) -> LaidOutBox {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let tree = build_box_tree(&dom, &styles);
    layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    )
}

#[test]
fn content_attr_reads_the_element_own_attribute_value() {
    let laid = layout(
        r#"<div class="label" data-status="Active">x</div>"#,
        r#".label::before { content: attr(data-status) ": "; }"#,
    );
    assert_eq!(extract_text(&laid), "Active: x");
}

#[test]
fn counter_reset_on_an_element_stays_visible_to_its_following_siblings() {
    // Regression test: if h2's counter-reset were popped right after h2's own processing,
    // the section value referenced by the second and later h3s would disappear.
    let laid = layout(
        r#"<h2>Intro</h2><h3>Background</h3><h3>Motivation</h3>"#,
        "h2 { counter-reset: section; } \
         h3 { counter-increment: section; } \
         h3::before { content: counter(section) \". \"; }",
    );
    // ::before comes before its own text, giving the order "1. Background".
    assert_eq!(extract_text(&laid), "Intro1. Background2. Motivation");
}

#[test]
fn nested_counters_are_scoped_per_ancestor_and_joined_with_the_separator() {
    let laid = layout(
        r#"
        <ol class="custom">
          <li>First
            <ol>
              <li>Nested A</li>
              <li>Nested B</li>
            </ol>
          </li>
          <li>Second</li>
        </ol>
        "#,
        "ol.custom { counter-reset: item; } \
         ol.custom li { counter-increment: item; } \
         ol.custom li::before { content: counters(item, \".\") \" \"; } \
         ol.custom li ol { counter-reset: item; }",
    );
    let text = extract_text(&laid);
    // The first li has a nested <ol> (a block element) as a child. Its ::before still goes
    // in front of its text, in the anonymous block wrapping "First".
    assert!(text.contains("1 First"), "text was: {text:?}");
    assert!(text.contains("1.1 Nested A"), "text was: {text:?}");
    assert!(text.contains("1.2 Nested B"), "text was: {text:?}");
    // The nested scope is popped when the li containing "First" is left, so Second sees only
    // the outer item counter (2).
    assert!(text.contains("2 Second"), "text was: {text:?}");
}

#[test]
fn after_content_is_resolved_after_descendants_so_it_reflects_their_counter_changes() {
    let laid = layout(
        r#"<p class="section">Heading <span class="mark">note</span></p>"#,
        "p.section { counter-reset: n; } \
         span.mark { counter-increment: n; } \
         p.section::after { content: \" total: \" counter(n); }",
    );
    assert_eq!(extract_text(&laid), "Heading note total: 1");
}

#[test]
fn nested_quotes_use_the_pair_matching_their_nesting_depth() {
    let laid = layout(
        r#"<p>She said <q>hello <q>nested</q> world</q> to everyone.</p>"#,
        "q { quotes: \"\\201C\" \"\\201D\" \"\\2018\" \"\\2019\"; } \
         q::before { content: open-quote; } \
         q::after { content: close-quote; }",
    );
    let text = extract_text(&laid);
    assert_eq!(
        text,
        "She said \u{201C}hello \u{2018}nested\u{2019} world\u{201D} to everyone."
    );
}

#[test]
fn first_letter_splits_only_the_first_character_into_its_own_styled_run() {
    let laid = layout(
        r#"<p class="dropcap">Hello world</p>"#,
        "p.dropcap::first-letter { font-size: 2.5em; }",
    );
    let base_font_size = sghtmltopdf::style::ComputedStyle::default().font_size.0;

    fn first_line(b: &LaidOutBox) -> Option<&LineBox> {
        match &b.content {
            LaidOutContent::Inline(lines) => lines.first(),
            LaidOutContent::Blocks(children) => children.iter().find_map(first_line),
            _ => None,
        }
    }
    let line = first_line(&laid).expect("expected inline content");
    assert_eq!(line.runs[0].text, "H");
    assert_eq!(line.runs[0].font_size, base_font_size * 2.5);
    assert!(line.runs[1..].iter().all(|r| r.font_size == base_font_size));
}

#[test]
fn all_generated_content_features_combined_render_a_valid_pdf_end_to_end() {
    let html_src = r#"
        <body>
        <h2>Introduction</h2>
        <h3>Background</h3>
        <h3>Motivation</h3>
        <h2>Methods</h2>
        <h3>Setup</h3>

        <div class="attr-test" data-label="Status">Active</div>

        <p>She said <q>hello <q>nested</q> world</q> to everyone.</p>

        <p class="dropcap">This paragraph has a drop cap.</p>

        <ol class="custom">
          <li>First
            <ol>
              <li>Nested A</li>
              <li>Nested B</li>
            </ol>
          </li>
          <li>Second</li>
        </ol>
        </body>
    "#;
    let css = "
        body { counter-reset: chapter; margin: 0; }
        h2 { counter-reset: section; counter-increment: chapter; }
        h2::before { content: \"Chapter \" counter(chapter) \": \"; }
        h3 { counter-increment: section; }
        h3::before { content: counter(chapter) \".\" counter(section) \" \"; }
        .attr-test::before { content: attr(data-label) \": \"; }
        q { quotes: \"\\201C\" \"\\201D\" \"\\2018\" \"\\2019\"; }
        q::before { content: open-quote; }
        q::after { content: close-quote; }
        p.dropcap::first-letter { font-size: 2.5em; font-weight: bold; color: #a33; }
        ol.custom { counter-reset: item; }
        ol.custom li { counter-increment: item; }
        ol.custom li::before { content: counters(item, \".\") \" \"; }
        ol.custom li ol { counter-reset: item; }
    ";
    let (page_count, bytes) = build_pdf(html_src, css);
    assert!(page_count >= 1);
    assert!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0,
        "the font should be embedded to render the text"
    );
}

// ---- ::before / ::after as boxes of their own (#152) ----

/// The text runs of every line, in order.
fn runs(b: &LaidOutBox) -> Vec<&TextRun> {
    fn walk<'a>(b: &'a LaidOutBox, out: &mut Vec<&'a TextRun>) {
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().for_each(|c| walk(c, out))
            }
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    out.extend(&line.runs);
                    line.atomics.iter().for_each(|a| walk(&a.content, out));
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(b, &mut out);
    out
}

fn run<'a>(b: &'a LaidOutBox, text: &str) -> &'a TextRun {
    runs(b)
        .into_iter()
        .find(|r| r.text.contains(text))
        .unwrap_or_else(|| panic!("no run {text:?}"))
}

/// The first line with a run holding `text`, as (top of the line, left of the run).
fn line_of(b: &LaidOutBox, text: &str) -> (f32, f32) {
    fn walk(b: &LaidOutBox, text: &str) -> Option<(f32, f32)> {
        match &b.content {
            LaidOutContent::Blocks(children) => children.iter().find_map(|c| walk(c, text)),
            LaidOutContent::Inline(lines) => lines.iter().find_map(|line| {
                let r = line.runs.iter().find(|r| r.text.contains(text))?;
                Some((line.rect.y, line.rect.x + r.x_offset))
            }),
            _ => None,
        }
    }
    walk(b, text).unwrap_or_else(|| panic!("no line with {text:?}"))
}

fn first_element(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id).find_map(|c| first_element(dom, c, tag))
}

fn find_laid_box(b: &LaidOutBox, node: NodeId) -> Option<&LaidOutBox> {
    if b.node == Some(node) {
        return Some(b);
    }
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            children.iter().find_map(|c| find_laid_box(c, node))
        }
        _ => None,
    }
}

#[test]
fn declarations_on_before_and_after_apply_to_their_own_text() {
    let laid = layout(
        "<p>Quoted text</p>",
        "p { font-size: 10px; color: #00f } \
         p::before { content: \"X\"; font-size: 48px; color: #f00 } \
         p::after { content: \"Y\"; font-weight: bold }",
    );
    let color = |red, blue| RgbaColor {
        red,
        green: 0,
        blue,
        alpha: 1.0,
    };
    let x = run(&laid, "X");
    assert_eq!((x.font_size, x.color), (48.0, color(255, 0)));
    let quoted = run(&laid, "Quoted");
    assert_eq!(
        (quoted.font_size, quoted.color, quoted.bold),
        (10.0, color(0, 255), false)
    );
    // What `::after` does not set, it inherits from the paragraph.
    let y = run(&laid, "Y");
    assert_eq!((y.font_size, y.color, y.bold), (10.0, color(0, 255), true));
}

#[test]
fn a_display_block_before_is_a_line_of_its_own_above_the_text() {
    let laid = layout(
        "<p>Quoted text</p>",
        "p::before { content: \"X\"; display: block }",
    );
    let (x_top, x_left) = line_of(&laid, "X");
    let (quoted_top, quoted_left) = line_of(&laid, "Quoted");
    assert!(quoted_top > x_top, "{quoted_top} should be below {x_top}");
    assert_eq!(quoted_left, x_left);
}

#[test]
fn an_inline_block_before_keeps_its_margin() {
    let laid = layout(
        "<p>Quoted text</p>",
        "body { margin: 0 } \
         p::before { content: \"X\"; display: inline-block; margin-right: 100px }",
    );
    let (_, quoted_left) = line_of(&laid, "Quoted");
    assert_eq!(quoted_left, run(&laid, "X").width + 100.0);
}

#[test]
fn an_absolutely_positioned_before_is_placed_against_its_element_out_of_the_flow() {
    let dom = html::parse(b"<p>Quoted text</p>");
    let author = parse_stylesheet(
        "p { position: relative; margin: 50px 0 0 20px } \
         p::before { content: \"X\"; position: absolute; top: 5px; left: 300px }",
    );
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);
    let pages = paginate_document(&dom, &styles, &test_fonts(), &PageSettings::default());
    let p = first_element(&dom, dom.document(), "p").expect("p not found");
    let find = |node: NodeId| {
        pages[0]
            .boxes
            .iter()
            .find_map(|b| find_laid_box(b, node))
            .expect("box not found")
    };
    let p_box = find(p).layout.border_box();
    let x_box = find(p.before()).layout.border_box();
    assert_eq!((x_box.x, x_box.y), (p_box.x + 300.0, p_box.y + 5.0));
    // It takes no room in the paragraph's line.
    let (_, quoted_left) = line_of(find(p), "Quoted");
    assert_eq!(quoted_left, p_box.x);
}

#[test]
fn before_and_after_are_generated_on_an_element_with_block_children() {
    let laid = layout(
        "<blockquote><p>Text</p></blockquote>",
        "p { margin: 0 } \
         blockquote::before { content: \"[\" } blockquote::after { content: \"]\" }",
    );
    assert_eq!(extract_text(&laid), "[Text]");
}

#[test]
fn a_display_none_pseudo_element_generates_nothing() {
    let laid = layout(
        "<p>Quoted text</p>",
        "p::before { content: \"X\"; display: none } p::after { content: \"Y\" }",
    );
    assert_eq!(extract_text(&laid), "Quoted textY");
    let base = ComputedStyle::default().font_size.0;
    assert_eq!(run(&laid, "Y").font_size, base);
}
