//! E2E tests for `position: absolute`/`fixed`.
//!
//! Absolute positioning only works under `Mode::Batch`. The overlays are added once every
//! page is settled, so they are checked against the pagination result (`paginate_document`) and the layout result.

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, LaidOutBox, LaidOutContent, PageSettings};
use sghtmltopdf_core::sink::MemorySink;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn pages_of(html_src: &str) -> Vec<Vec<String>> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    fn texts(b: &LaidOutBox, out: &mut Vec<String>) {
        match &b.content {
            LaidOutContent::Inline(lines) => {
                let t: String = lines
                    .iter()
                    .flat_map(|l| l.runs.iter())
                    .map(|r| r.text.as_str())
                    .collect();
                if !t.trim().is_empty() {
                    out.push(t);
                }
            }
            LaidOutContent::Grid(grid) => {
                for c in grid.rows.iter().flat_map(|row| &row.items) {
                    texts(c, out);
                }
            }
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for c in children {
                    texts(c, out);
                }
            }
            LaidOutContent::Table(table) => {
                for row in &table.rows {
                    for cell in &row.cells {
                        texts(cell, out);
                    }
                }
            }
            LaidOutContent::Image(_) => {}
        }
    }
    pages
        .iter()
        .map(|page| {
            let mut out = Vec::new();
            for b in &page.boxes {
                texts(b, &mut out);
            }
            out
        })
        .collect()
}

/// Find, across every page, the border box of the box containing the given text.
fn find_box_rect(html_src: &str, needle: &str) -> (usize, sghtmltopdf_core::layout::Rect) {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    fn text_of(b: &LaidOutBox) -> String {
        let mut out = String::new();
        fn walk(b: &LaidOutBox, out: &mut String) {
            if let LaidOutContent::Inline(lines) = &b.content {
                for l in lines {
                    for r in &l.runs {
                        out.push_str(&r.text);
                    }
                }
            }
            if let LaidOutContent::Blocks(children) = &b.content {
                for c in children {
                    walk(c, out);
                }
            }
        }
        walk(b, &mut out);
        out
    }
    for (i, page) in pages.iter().enumerate() {
        for b in &page.boxes {
            if text_of(b).contains(needle) {
                return (i, b.layout.border_box());
            }
        }
    }
    panic!("no box containing {needle:?}");
}

#[test]
fn a_fixed_element_repeats_on_every_page() {
    let long = "<div style=\"height: 900px;\">tall</div>";
    let html_src = format!(
        "<body><p>one</p>{long}<p>two</p>\
         <div style=\"position: fixed; top: 100px; left: 50px;\">WATERMARK</div></body>"
    );
    let per_page = pages_of(&html_src);
    assert!(
        per_page.len() >= 2,
        "the document should span several pages"
    );
    for (i, texts) in per_page.iter().enumerate() {
        assert!(
            texts.iter().any(|t| t.contains("WATERMARK")),
            "page {i} should contain the fixed watermark, got {texts:?}"
        );
    }
}

#[test]
fn an_absolute_element_only_appears_once() {
    let long = "<div style=\"height: 900px;\">tall</div>";
    let html_src = format!(
        "<body><p>one</p>{long}<p>two</p>\
         <div style=\"position: absolute; top: 10px; left: 10px;\">ABS</div></body>"
    );
    let per_page = pages_of(&html_src);
    let count: usize = per_page
        .iter()
        .map(|texts| texts.iter().filter(|t| t.contains("ABS")).count())
        .sum();
    assert_eq!(count, 1, "an absolute element must not repeat");
    // With no positioned ancestor, the initial containing block is the first page.
    assert!(per_page[0].iter().any(|t| t.contains("ABS")));
}

#[test]
fn an_absolute_child_is_placed_relative_to_its_positioned_ancestor() {
    // An absolute badge at the top right of a card (relative, full page width). It is
    // positioned with `right` against the ancestor's padding box, so it lands towards the right of the page.
    let content_width = PageSettings::default().content_width();
    let with_right = r#"<body>
        <div style="position: relative; margin: 20px; padding: 10px; height: 100px;">
          <span style="position: absolute; top: 5px; right: 5px;">BADGE</span>
          card body
        </div></body>"#;
    let (_, badge_right) = find_box_rect(with_right, "BADGE");
    assert!(
        badge_right.x > content_width * 0.5,
        "a right-anchored badge should sit in the right half: x={} of {content_width}",
        badge_right.x
    );

    // With `left` against the same ancestor it lands towards the left (the ancestor's left edge = margin 20 + padding 10).
    let with_left = with_right.replace("right: 5px", "left: 5px");
    let (_, badge_left) = find_box_rect(&with_left, "BADGE");
    assert!(
        badge_left.x < content_width * 0.3,
        "a left-anchored badge should sit near the left: x={}",
        badge_left.x
    );
    assert!(badge_right.x > badge_left.x);
}

#[test]
fn a_left_absolute_sits_at_the_left_and_a_right_absolute_at_the_right() {
    let html_src = r#"<body><div style="position: relative; height: 200px; margin: 0;">
        <span style="position: absolute; left: 0;">L</span>
        <span style="position: absolute; right: 0;">R</span>
    </div></body>"#;
    let (_, l) = find_box_rect(html_src, "L");
    let (_, r) = find_box_rect(html_src, "R");
    assert!(r.x > l.x, "the right-anchored box must be further right");
}

#[test]
fn a_fixed_footer_uses_bottom() {
    // For fixed the cb height (the page height) is settled, so `bottom` works.
    let html_src = r#"<body><p>content</p>
        <div style="position: fixed; bottom: 20px; left: 40px;">FOOTER</div></body>"#;
    let (_, footer) = find_box_rect(html_src, "FOOTER");
    let page_height = PageSettings::default().size.height;
    // The footer is at the bottom of the page.
    assert!(
        footer.y > page_height * 0.7,
        "footer at y={} should be near the bottom of {page_height}",
        footer.y
    );
}

#[test]
fn absolute_elements_do_not_take_space_in_the_normal_flow() {
    // absolute is out of flow, so it does not affect the position of the normal-flow elements that follow.
    let without = find_box_rect("<body><p>A</p><p>B</p></body>", "B").1;
    let with = find_box_rect(
        r#"<body><p>A</p><div style="position: absolute; top: 0;">X</div><p>B</p></body>"#,
        "B",
    )
    .1;
    assert!(
        (with.y - without.y).abs() < 0.5,
        "the absolute element must not push B down: {} vs {}",
        with.y,
        without.y
    );
}

#[test]
fn a_document_with_absolute_and_fixed_encodes_to_a_valid_pdf_in_batch_mode() {
    let html_src = r#"<html><body>
        <div style="position: fixed; top: 300px; left: 200px;">COPY</div>
        <div style="position: relative; height: 100px;">
          <span style="position: absolute; top: 0; right: 0;">TAG</span>
          body
        </div>
      </body></html>"#;
    let options = EngineOptions {
        mode: Mode::Batch,
        fonts: vec![FontSpec {
            path: std::path::PathBuf::from(FONT_PATH),
            index: 0,
        }],
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    engine.feed(html_src.as_bytes()).unwrap();
    let bytes = engine.finish().unwrap();

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
}

/// The four below are regression tests that an `absolute` placed inside a box establishing a
/// new formatting context (a flex item, a grid item, a table cell, an inline-block) comes
/// out rather than vanishing silently.
/// Layout has a "measuring pass whose result is thrown away" for all of them, so as well as
/// not vanishing, they must not come out twice either.
fn absolute_count(html_src: &str, needle: &str) -> usize {
    pages_of(html_src)
        .iter()
        .map(|texts| texts.iter().filter(|t| t.contains(needle)).count())
        .sum()
}

#[test]
fn an_absolute_inside_a_flex_item_is_placed_relative_to_that_item() {
    // A two-column flex. Only the right item is positioned, and the badge is pinned to its left edge.
    let content_width = PageSettings::default().content_width();
    let html_src = r#"<body><div style="display: flex; margin: 0;">
        <div style="flex: 1; height: 100px;">left column</div>
        <div style="flex: 1; height: 100px; position: relative;">
          right column
          <span style="position: absolute; left: 0; top: 0;">BADGE</span>
        </div>
      </div></body>"#;

    assert_eq!(
        absolute_count(html_src, "BADGE"),
        1,
        "only one absolute comes out of a flex item (the measuring pass must not duplicate it)"
    );

    // The containing block is the right item, so it lands in the right half of the page.
    // Before they were collected it vanished before reaching here.
    let (_, badge) = find_box_rect(html_src, "BADGE");
    assert!(
        badge.x > content_width * 0.4,
        "the badge should sit at the left edge of the right column: x={} of {content_width}",
        badge.x
    );
}

#[test]
fn an_absolute_inside_a_grid_item_is_placed_relative_to_that_item() {
    let content_width = PageSettings::default().content_width();
    let html_src = r#"<body><div style="display: grid; grid-template-columns: 1fr 1fr; margin: 0;">
        <div style="height: 100px;">left cell</div>
        <div style="height: 100px; position: relative;">
          right cell
          <span style="position: absolute; left: 0; top: 0;">BADGE</span>
        </div>
      </div></body>"#;

    assert_eq!(absolute_count(html_src, "BADGE"), 1);
    let (_, badge) = find_box_rect(html_src, "BADGE");
    assert!(
        badge.x > content_width * 0.4,
        "the badge should sit at the left edge of the right grid item: x={badge:?}"
    );
}

#[test]
fn an_absolute_inside_a_table_cell_is_placed_relative_to_that_cell() {
    let content_width = PageSettings::default().content_width();
    let html_src = r#"<body><table style="width: 100%; margin: 0;"><tr>
        <td style="width: 50%;">left cell</td>
        <td style="width: 50%; position: relative;">
          right cell
          <span style="position: absolute; left: 0; top: 0;">BADGE</span>
        </td>
      </tr></table></body>"#;

    assert_eq!(absolute_count(html_src, "BADGE"), 1);
    let (_, badge) = find_box_rect(html_src, "BADGE");
    assert!(
        badge.x > content_width * 0.4,
        "the badge should sit at the left edge of the right cell: x={badge:?}"
    );
}

#[test]
fn an_absolute_inside_an_inline_block_is_not_dropped() {
    let html_src = r#"<body><p style="margin: 0;">
        <span style="display: inline-block; position: relative; width: 200px; height: 60px;">
          box
          <span style="position: absolute; left: 0; top: 0;">BADGE</span>
        </span>
      </p></body>"#;

    assert_eq!(absolute_count(html_src, "BADGE"), 1);
}
