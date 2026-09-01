//! E2E tests for `vertical-align` in an inline context.
//!
//! They confirm that the forms used in practice, such as `H<sub>2</sub>O` and a superscript
//! footnote number, come out as intended in both the layout result and the PDF output.

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

fn find_tag(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id).find_map(|c| find_tag(dom, c, tag))
}

fn first_line(laid: &LaidOutBox) -> LineBox {
    fn walk(b: &LaidOutBox) -> Option<LineBox> {
        match &b.content {
            LaidOutContent::Inline(lines) => lines.first().cloned(),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().find_map(walk)
            }
            LaidOutContent::Grid(grid) => {
                grid.rows.iter().flat_map(|row| &row.items).find_map(walk)
            }
            _ => None,
        }
    }
    walk(laid).expect("no line box found")
}

/// Return the PDF baseline (the distance from the top of the line, positive downwards) of
/// the run matching `text` in the line. `baseline_shift` is positive upwards, so it is added with the sign flipped.
fn run_baseline(line: &LineBox, text: &str) -> f32 {
    let run = line
        .runs
        .iter()
        .find(|r| r.text == text)
        .unwrap_or_else(|| panic!("run {text:?} not found"));
    line.baseline - run.baseline_shift
}

#[test]
fn h2o_puts_the_subscript_below_the_baseline() {
    let (_, laid) = layout("<p>H<sub>2</sub>O</p>", "");
    let line = first_line(&laid);

    assert!(
        run_baseline(&line, "2") > run_baseline(&line, "H"),
        "the subscript baseline must sit lower than the normal text"
    );
    assert_eq!(run_baseline(&line, "H"), run_baseline(&line, "O"));
}

#[test]
fn a_footnote_marker_sits_above_the_baseline() {
    let (_, laid) = layout("<p>text<sup>1</sup></p>", "");
    let line = first_line(&laid);

    assert!(
        run_baseline(&line, "1") < run_baseline(&line, "text"),
        "the superscript baseline must sit higher than the normal text"
    );
}

#[test]
fn sub_and_sup_use_a_smaller_font_but_the_same_shift_direction() {
    let (_, laid) = layout("<p>x<sub>a</sub><sup>b</sup></p>", "");
    let line = first_line(&laid);
    let base = line.runs.iter().find(|r| r.text == "x").unwrap();
    let sub = line.runs.iter().find(|r| r.text == "a").unwrap();
    let sup = line.runs.iter().find(|r| r.text == "b").unwrap();

    assert!(
        sub.font_size < base.font_size,
        "UA stylesheet shrinks <sub>"
    );
    assert!(
        sup.font_size < base.font_size,
        "UA stylesheet shrinks <sup>"
    );
    assert!(sub.baseline_shift < 0.0 && sup.baseline_shift > 0.0);
}

#[test]
fn a_superscript_grows_the_line_box_like_a_browser_does() {
    let (_, plain) = layout("<p>text</p>", "");
    let (_, with_sup) = layout("<p>text<sup>1</sup></p>", "");

    assert!(
        first_line(&with_sup).rect.height > first_line(&plain).rect.height,
        "a raised run that sticks out must grow the line box"
    );
}

#[test]
fn a_document_without_vertical_align_keeps_its_line_geometry() {
    // Regression check: the line heights and baselines of a document not using
    // `vertical-align` are as they were.
    let (dom, laid) = layout("<p>plain text only</p>", "p { line-height: 24px; }");
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let _ = p;
    let line = first_line(&laid);
    assert_eq!(line.rect.height, 24.0);
    assert!(line.baseline > 0.0 && line.baseline < 24.0);
}

#[test]
fn a_document_using_vertical_align_encodes_to_a_valid_pdf() {
    let dom = html::parse(
        br#"<p>H<sub>2</sub>SO<sub>4</sub> and E=mc<sup>2</sup>,
              <span style="vertical-align: top;">top</span>
              <span style="vertical-align: middle;">middle</span>
              <span style="vertical-align: -6px;">lowered</span></p>"#,
    );
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
}

// ===== Inline element backgrounds =====

#[test]
fn mark_paints_an_inline_background_behind_its_text() {
    let (_, laid) = layout("<p>plain <mark>marked</mark> plain</p>", "");
    let line = first_line(&laid);

    let marked = line.runs.iter().find(|r| r.text == "marked").unwrap();
    assert!(
        marked.background_color.alpha > 0.0,
        "<mark> should carry a background color on its run"
    );
    assert_eq!(
        (
            marked.background_color.red,
            marked.background_color.green,
            marked.background_color.blue
        ),
        (255, 255, 0)
    );

    for run in line.runs.iter().filter(|r| r.text != "marked") {
        assert_eq!(
            run.background_color.alpha, 0.0,
            "text outside <mark> must stay transparent"
        );
    }
}

#[test]
fn a_block_background_does_not_leak_onto_its_text_runs() {
    // Regression test: a text node's computed style clones even the parent's non-inherited
    // properties, so a naive implementation paints the block's background a second time as
    // an inline background.
    let (_, laid) = layout("<p>text</p>", "p { background-color: rgb(0, 128, 0); }");
    let line = first_line(&laid);
    assert_eq!(line.runs[0].background_color.alpha, 0.0);
}

#[test]
fn an_inline_background_moves_with_a_raised_run() {
    let (_, laid) = layout(
        "<p>x<span>up</span></p>",
        "span { background-color: rgb(255, 0, 0); vertical-align: 10px; }",
    );
    let line = first_line(&laid);
    let raised = line.runs.iter().find(|r| r.text == "up").unwrap();
    assert_eq!(raised.baseline_shift, 10.0);
    assert!(raised.background_color.alpha > 0.0);
    // The background rectangle is built from the run's ascent and descent, so the metrics are needed.
    assert!(raised.ascent > 0.0 && raised.descent > 0.0);
}

#[test]
fn probe_block_heights() {
    let (_, laid) = layout(
        r#"<p>text <span class="b">B</span> tail</p><p><span class="c">C1<br>L2</span> <span class="c">C2</span></p><p>after</p>"#,
        ".b { display: inline-block; padding: 2px 8px; } .c { display: inline-block; width: 100px; border: 1px solid #999; }",
    );
    let dom2 = html::parse(r#"<p>text <span class="b">B</span> tail</p><p><span class="c">C1<br>L2</span> <span class="c">C2</span></p><p>after</p>"#.as_bytes());
    let styles2 = compute_styles(&dom2, &user_agent_stylesheet(), &parse_stylesheet(".b { display: inline-block; padding: 2px 8px; } .c { display: inline-block; width: 100px; border: 1px solid #999; }"));
    let pages = paginate_document(&dom2, &styles2, &test_fonts(), &PageSettings::default());
    for (i, page) in pages.iter().enumerate() {
        fn dump(b: &LaidOutBox, page: usize, depth: usize) {
            match &b.content {
                LaidOutContent::Inline(lines) => {
                    for l in lines {
                        println!(
                            "P{page}{:i$} LINE y={} h={} atomics={}",
                            "",
                            l.rect.y,
                            l.rect.height,
                            l.atomics.len(),
                            i = depth * 2
                        );
                        for a in &l.atomics {
                            println!(
                                "P{page}{:i$}   ATOMIC y={} h={}",
                                "",
                                a.content.layout.content.y,
                                a.content.layout.content.height,
                                i = depth * 2
                            );
                        }
                    }
                }
                LaidOutContent::Blocks(children) => {
                    println!(
                        "P{page}{:i$} BLOCK node={:?} y={}",
                        "",
                        b.node,
                        b.layout.content.y,
                        i = depth * 2
                    );
                    for c in children {
                        dump(c, page, depth + 1)
                    }
                }
                _ => {}
            }
        }
        for b in &page.boxes {
            dump(b, i, 0);
        }
    }

    fn walk(b: &LaidOutBox, depth: usize) {
        println!(
            "{:i$}box node={:?} content={:?}",
            "",
            b.node,
            b.layout.content,
            i = depth * 2
        );
        if let LaidOutContent::Blocks(children) = &b.content {
            for c in children {
                walk(c, depth + 1)
            }
        }
        if let LaidOutContent::Inline(lines) = &b.content {
            for l in lines {
                println!(
                    "{:i$}  line y={} h={} atomics={}",
                    "",
                    l.rect.y,
                    l.rect.height,
                    l.atomics.len(),
                    i = depth * 2
                );
            }
        }
    }
    walk(&laid, 0);
}
