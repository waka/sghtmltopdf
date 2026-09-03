//! テーブルの行単位ページ分割のE2Eテスト。
//!
//! 回帰の本体: 実装前は「テーブルはページ分割に対してアトミック」だったため、
//! ページに収まらない行が描画されずに出力から失われ、空ページも生まれていた。

use std::collections::HashMap;
use std::path::PathBuf;

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, LaidOutContent, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::sink::MemorySink;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn table_html(row_count: usize) -> String {
    let rows: String = (0..row_count)
        .map(|i| format!("<tr><td>{i}</td><td>item {i}</td><td>1,000</td></tr>"))
        .collect();
    format!("<table border=\"1\" cellspacing=\"0\">{rows}</table>")
}

/// ページ分割の結果から、ページごとの「テーブル行の数」を返す。
fn rows_per_page(html_src: &str) -> Vec<usize> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    fn count(b: &sghtmltopdf_core::layout::LaidOutBox) -> usize {
        match &b.content {
            LaidOutContent::Table(table) => table.rows.len(),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().map(count).sum()
            }
            _ => 0,
        }
    }
    pages
        .iter()
        .map(|page| page.boxes.iter().map(count).sum())
        .collect()
}

#[test]
fn a_long_table_is_split_across_pages_without_losing_rows() {
    let counts = rows_per_page(&table_html(80));

    assert!(counts.len() >= 2, "80 rows must not fit on one page");
    assert_eq!(
        counts.iter().sum::<usize>(),
        80,
        "every row must appear exactly once, got {counts:?}"
    );
    assert!(
        counts.iter().all(|&c| c > 0),
        "no page may be empty, got {counts:?}"
    );
}

#[test]
fn a_short_table_still_fits_on_a_single_page() {
    let counts = rows_per_page(&table_html(3));
    assert_eq!(counts, vec![3]);
}

#[test]
fn a_table_that_exactly_fills_pages_does_not_produce_an_empty_page() {
    for row_count in [40, 41, 42, 43, 44] {
        let counts = rows_per_page(&table_html(row_count));
        assert_eq!(
            counts.iter().sum::<usize>(),
            row_count,
            "row_count={row_count} got {counts:?}"
        );
        assert!(
            counts.iter().all(|&c| c > 0),
            "row_count={row_count} produced an empty page: {counts:?}"
        );
    }
}

#[test]
fn content_after_a_split_table_continues_on_the_last_page() {
    let html_src = format!("{}<p>after the table</p>", table_html(80));
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    fn has_inline_text(b: &sghtmltopdf_core::layout::LaidOutBox) -> bool {
        match &b.content {
            LaidOutContent::Inline(lines) => lines.iter().any(|l| !l.runs.is_empty()),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().any(has_inline_text)
            }
            _ => false,
        }
    }
    let last = pages.last().expect("at least one page");
    assert!(
        last.boxes.iter().any(has_inline_text),
        "the paragraph after the table must be placed on the last page"
    );
}

#[test]
fn a_caption_stays_with_the_first_fragment() {
    let html_src = format!(
        "<table border=\"1\" cellspacing=\"0\"><caption>Title</caption>{}</table>",
        (0..80)
            .map(|i| format!("<tr><td>{i}</td></tr>"))
            .collect::<String>()
    );
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    fn captions(b: &sghtmltopdf_core::layout::LaidOutBox) -> usize {
        match &b.content {
            LaidOutContent::Table(table) => usize::from(table.caption.is_some()),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().map(captions).sum()
            }
            _ => 0,
        }
    }
    let per_page: Vec<usize> = pages
        .iter()
        .map(|p| p.boxes.iter().map(captions).sum())
        .collect();
    assert_eq!(per_page[0], 1, "the caption belongs to the first fragment");
    assert!(
        per_page[1..].iter().all(|&c| c == 0),
        "the caption must not repeat: {per_page:?}"
    );
}

#[test]
fn a_split_table_encodes_to_a_valid_pdf() {
    let dom = html::parse(table_html(80).as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
    assert!(
        bytes.windows(9).filter(|w| *w == b"/Type /Pa").count() >= 2,
        "the document should contain several pages"
    );
}

#[test]
fn a_long_table_also_splits_in_streaming_mode() {
    let options = EngineOptions {
        mode: Mode::Streaming,
        fonts: vec![FontSpec {
            path: PathBuf::from(FONT_PATH),
            index: 0,
        }],
        ..EngineOptions::default()
    };
    let mut engine = Engine::new(options, MemorySink::new());
    engine
        .feed(format!("<html><body>{}</body></html>", table_html(80)).as_bytes())
        .unwrap();
    let bytes = engine.finish().unwrap();

    assert!(bytes.starts_with(b"%PDF-"));
    // `/Count N`(ページツリー)で複数ページになっていることを確認する。
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("/Count 1\n"),
        "the streamed document should have more than one page"
    );
}

// ===== `<thead>`のページまたぎ繰り返し =====

fn table_with_head(row_count: usize) -> String {
    let rows: String = (0..row_count)
        .map(|i| format!("<tr><td>{i}</td><td>item {i}</td></tr>"))
        .collect();
    format!(
        "<table border=\"1\" cellspacing=\"0\">\
           <thead><tr><th>No</th><th>Item</th></tr></thead>\
           <tbody>{rows}</tbody>\
         </table>"
    )
}

/// ページごとに、各行の1つ目のセルのテキストを返す。
fn first_cell_texts_per_page(html_src: &str) -> Vec<Vec<String>> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    fn text_of(b: &sghtmltopdf_core::layout::LaidOutBox) -> String {
        match &b.content {
            LaidOutContent::Inline(lines) => lines
                .iter()
                .flat_map(|l| l.runs.iter())
                .map(|r| r.text.as_str())
                .collect(),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().map(text_of).collect()
            }
            _ => String::new(),
        }
    }
    fn rows_of(b: &sghtmltopdf_core::layout::LaidOutBox, out: &mut Vec<String>) {
        match &b.content {
            LaidOutContent::Table(table) => {
                for row in &table.rows {
                    out.push(row.cells.first().map(text_of).unwrap_or_default());
                }
            }
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for c in children {
                    rows_of(c, out);
                }
            }
            LaidOutContent::Grid(grid) => {
                for c in grid.rows.iter().flat_map(|row| &row.items) {
                    rows_of(c, out);
                }
            }
            _ => {}
        }
    }
    pages
        .iter()
        .map(|page| {
            let mut out = Vec::new();
            for b in &page.boxes {
                rows_of(b, &mut out);
            }
            out
        })
        .collect()
}

#[test]
fn the_table_header_repeats_on_every_page() {
    let per_page = first_cell_texts_per_page(&table_with_head(80));
    assert!(per_page.len() >= 2, "expected several pages");

    for (i, rows) in per_page.iter().enumerate() {
        assert_eq!(
            rows.first().map(String::as_str),
            Some("No"),
            "page {i} should start with the repeated header, got {rows:?}"
        );
    }
    // 見出しは各ページに1回だけ。
    for rows in &per_page {
        assert_eq!(rows.iter().filter(|t| *t == "No").count(), 1);
    }
    // 本文の行は複製されない(0〜79が1回ずつ)。
    let body: Vec<&String> = per_page.iter().flatten().filter(|t| *t != "No").collect();
    assert_eq!(body.len(), 80);
    assert_eq!(body[0], "0");
    assert_eq!(body[79], "79");
}

#[test]
fn a_table_that_fits_on_one_page_does_not_duplicate_its_header() {
    let per_page = first_cell_texts_per_page(&table_with_head(3));
    assert_eq!(per_page.len(), 1);
    assert_eq!(per_page[0].iter().filter(|t| *t == "No").count(), 1);
}

#[test]
fn tfoot_is_moved_to_the_end_of_the_table() {
    // HTML4では`<tfoot>`を`<tbody>`より前に書く決まりだった。
    let html_src = "<table>\
          <thead><tr><td>H</td></tr></thead>\
          <tfoot><tr><td>F</td></tr></tfoot>\
          <tbody><tr><td>B1</td></tr><tr><td>B2</td></tr></tbody>\
        </table>";
    let per_page = first_cell_texts_per_page(html_src);
    assert_eq!(per_page.len(), 1);
    assert_eq!(per_page[0], vec!["H", "B1", "B2", "F"]);
}

#[test]
fn a_document_with_a_repeated_header_encodes_to_a_valid_pdf() {
    let dom = html::parse(table_with_head(80).as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
}

/// Checks that every box on a page (down to the table cells) stays between the
/// top and the bottom of it. Returns the ones that do not, as
/// `(page index, top, bottom)`.
fn boxes_outside_pages(pages: &[sghtmltopdf_core::layout::Page]) -> Vec<(usize, f32, f32)> {
    fn walk(
        page_index: usize,
        b: &sghtmltopdf_core::layout::LaidOutBox,
        page_height: f32,
        out: &mut Vec<(usize, f32, f32)>,
    ) {
        let top = b.layout.content.y - b.layout.padding.top - b.layout.border.top;
        let bottom =
            top + b.layout.content.height + b.layout.padding.bottom + b.layout.border.bottom;
        if top < -0.01 || bottom > page_height + 0.01 {
            out.push((page_index, top, bottom));
        }
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for child in children {
                    walk(page_index, child, page_height, out);
                }
            }
            LaidOutContent::Table(table) => {
                for cell in table.rows.iter().flat_map(|row| &row.cells) {
                    walk(page_index, cell, page_height, out);
                }
            }
            _ => {}
        }
    }
    let page_height = PageSettings::default().content_height();
    let mut out = Vec::new();
    for (i, page) in pages.iter().enumerate() {
        for b in &page.boxes {
            walk(i, b, page_height, &mut out);
        }
    }
    out
}

#[test]
fn a_table_whose_first_row_does_not_fit_starts_on_the_next_page() {
    // Splitting required the fragment to already hold a row, so the first row of
    // a table was laid down where it was however little room was left on the
    // page, and was cut off by the page edge.
    let settings = PageSettings::default();
    let filler_height = settings.content_height() - 30.0;
    let html_src = r#"<div class="filler"></div><table><tr><td>first</td></tr><tr><td>second</td></tr></table>"#;
    let css = format!(
        "* {{ margin: 0; padding: 0 }} .filler {{ height: {filler_height}px }} td {{ height: 40px }}"
    );
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(&css));
    let pages = paginate_document(&dom, &styles, &test_fonts(), &settings);

    assert_eq!(pages.len(), 2, "the table moves to the second page");
    assert_eq!(
        rows_on_page(&pages[0]),
        0,
        "only 30px are left on the first page, too little for a 40px row"
    );
    assert_eq!(rows_on_page(&pages[1]), 2);
    assert!(
        boxes_outside_pages(&pages).is_empty(),
        "some boxes stick out of their page: {:?}",
        boxes_outside_pages(&pages)
    );
}

fn rows_on_page(page: &sghtmltopdf_core::layout::Page) -> usize {
    fn count(b: &sghtmltopdf_core::layout::LaidOutBox) -> usize {
        match &b.content {
            LaidOutContent::Table(table) => table.rows.len(),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().map(count).sum()
            }
            _ => 0,
        }
    }
    page.boxes.iter().map(count).sum()
}
