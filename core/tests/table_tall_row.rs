//! E2E tests for a table row taller than a page (#163).
//!
//! The row cannot be kept whole on any page, so it has to start where it is and split through
//! the content of its cells, as a block does. Before, it was moved to a fresh page, leaving
//! the page before it nearly empty, and what its cell held past one page was lost.

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html;
use sghtmltopdf::layout::{paginate_document, LaidOutBox, LaidOutContent, Page, PageSettings};
use sghtmltopdf::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn paginate(html_src: &str, css: &str) -> Vec<Page> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ]);
    paginate_document(&dom, &styles, &fonts, &PageSettings::default())
}

/// The text of every line on the page, with its top, in drawing order.
fn lines_of(page: &Page) -> Vec<(String, f32)> {
    fn walk(b: &LaidOutBox, out: &mut Vec<(String, f32)>) {
        match &b.content {
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    let text: String = line.runs.iter().map(|r| r.text.as_str()).collect();
                    if !text.trim().is_empty() {
                        out.push((text, line.rect.y));
                    }
                }
            }
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().for_each(|c| walk(c, out));
            }
            LaidOutContent::Table(table) => {
                for cell in table.rows.iter().flat_map(|row| &row.cells) {
                    walk(cell, out);
                }
            }
            LaidOutContent::Grid(grid) => {
                for item in grid.rows.iter().flat_map(|row| &row.items) {
                    walk(item, out);
                }
            }
            LaidOutContent::Image(_) => {}
        }
    }
    let mut out = Vec::new();
    for b in &page.boxes {
        walk(b, &mut out);
    }
    out
}

fn texts_per_page(pages: &[Page]) -> Vec<Vec<String>> {
    pages
        .iter()
        .map(|p| lines_of(p).into_iter().map(|(t, _)| t).collect())
        .collect()
}

fn paragraphs(count: usize) -> String {
    (1..=count)
        .map(|i| format!("<p>LINE-{i} x</p>"))
        .collect::<String>()
}

/// The page every line `LINE-1`..`LINE-count` is on, asserting each is drawn exactly once,
/// that they run in order, and that each lies within its page (below the page's end it would
/// be laid out but never drawn).
fn assert_all_lines_in_order(pages: &[Page], count: usize) -> Vec<usize> {
    let page_height = PageSettings::default().content_height();
    for (i, page) in pages.iter().enumerate() {
        for (text, top) in lines_of(page) {
            assert!(
                (0.0..page_height).contains(&top),
                "page {i}: {text:?} is at {top}, outside the page (height {page_height})"
            );
        }
    }
    let texts = texts_per_page(pages);
    let seen: Vec<(usize, String)> = texts
        .iter()
        .enumerate()
        .flat_map(|(page, lines)| {
            lines
                .iter()
                .filter(|t| t.starts_with("LINE-"))
                .map(move |t| (page, t.clone()))
        })
        .collect();
    let expected: Vec<String> = (1..=count).map(|i| format!("LINE-{i} x")).collect();
    assert_eq!(
        seen.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(),
        expected,
        "every line is drawn once, in order (per page: {:?})",
        texts.iter().map(Vec::len).collect::<Vec<_>>()
    );
    seen.iter().map(|(page, _)| *page).collect()
}

fn single_cell_table(count: usize) -> String {
    format!(
        "<p>HEAD</p><table><tr><td>{}</td></tr></table>",
        paragraphs(count)
    )
}

#[test]
fn a_row_taller_than_a_page_starts_where_it_is() {
    let pages = paginate(&single_cell_table(60), "");
    let texts = texts_per_page(&pages);

    assert!(
        texts[0].iter().any(|t| t == "HEAD") && texts[0].iter().any(|t| t == "LINE-1 x"),
        "the row starts on the page that holds HEAD, got {texts:?}"
    );
}

#[test]
fn what_a_cell_holds_past_one_page_is_not_lost() {
    for count in [60, 120] {
        let pages = paginate(&single_cell_table(count), "");
        let on = assert_all_lines_in_order(&pages, count);
        assert_eq!(on[0], 0, "LINE-1 is on the first page ({count} lines)");
        assert!(
            *on.last().unwrap() >= 2,
            "{count} lines run past two pages, last is on page {}",
            on.last().unwrap()
        );
    }
}

#[test]
fn a_row_splits_into_as_many_pages_as_the_same_content_in_a_div() {
    // The issue's own comparison: the same 60 paragraphs in a `<div>` take three pages.
    let in_div = paginate(&format!("<p>HEAD</p><div>{}</div>", paragraphs(60)), "");
    let in_row = paginate(&single_cell_table(60), "");

    assert_eq!(in_div.len(), 3);
    assert_eq!(in_row.len(), in_div.len());
}

#[test]
fn the_pieces_of_a_split_row_stay_within_their_pages() {
    let pages = paginate(&single_cell_table(120), "");
    let page_height = PageSettings::default().content_height();

    for (i, page) in pages.iter().enumerate() {
        for (text, top) in lines_of(page) {
            assert!(
                (0.0..=page_height).contains(&top),
                "page {i}: {text:?} starts at {top}, outside 0..{page_height}"
            );
        }
    }
}

#[test]
fn a_row_taller_than_a_page_in_a_table_with_a_head_repeats_the_head() {
    let html_src = format!(
        "<table><thead><tr><th>HEADCELL</th></tr></thead>\
         <tbody><tr><td>{}</td></tr></tbody></table>",
        paragraphs(60)
    );
    let pages = paginate(&html_src, "");
    assert_all_lines_in_order(&pages, 60);

    let texts = texts_per_page(&pages);
    assert!(pages.len() >= 3);
    for (i, lines) in texts.iter().enumerate() {
        assert_eq!(
            lines.iter().filter(|t| *t == "HEADCELL").count(),
            1,
            "page {i} has the head once: {lines:?}"
        );
    }
    // The head is above the row on every page.
    for (i, page) in pages.iter().enumerate() {
        let lines = lines_of(page);
        let head = lines.iter().find(|(t, _)| t == "HEADCELL").unwrap().1;
        let first = lines
            .iter()
            .find(|(t, _)| t.starts_with("LINE-"))
            .unwrap()
            .1;
        assert!(
            head < first,
            "page {i}: the head at {head}, text at {first}"
        );
    }
}

/// The pages a line with exactly this text is on.
fn pages_with(pages: &[Page], text: &str) -> Vec<usize> {
    texts_per_page(pages)
        .iter()
        .enumerate()
        .filter(|(_, lines)| lines.iter().any(|t| t == text))
        .map(|(i, _)| i)
        .collect()
}

#[test]
fn a_short_cell_aligned_to_the_top_stays_on_the_page_the_row_starts() {
    // CSS 2.1 17.5.3: `vertical-align: top` puts the cell's content at the top of the row.
    let html_src = format!(
        "<p>HEAD</p><table><tr><td style=\"vertical-align: top\">SIDE</td><td>{}</td></tr></table>",
        paragraphs(60)
    );
    let pages = paginate(&html_src, "");
    assert_all_lines_in_order(&pages, 60);

    assert_eq!(pages_with(&pages, "SIDE"), vec![0]);
}

#[test]
fn a_short_cell_beside_a_tall_one_is_drawn_once_within_a_page() {
    // The default is `vertical-align: middle`, so SIDE sits in the middle of the row's
    // height, far below the first page: it still has to be drawn.
    let html_src = format!(
        "<p>HEAD</p><table><tr><td>SIDE</td><td>{}</td></tr></table>",
        paragraphs(60)
    );
    let pages = paginate(&html_src, "");
    assert_all_lines_in_order(&pages, 60);

    assert_eq!(pages_with(&pages, "SIDE").len(), 1);
}

#[test]
fn rows_after_a_split_row_follow_the_end_of_its_content() {
    let html_src = format!(
        "<p>HEAD</p><table><tr><td>{}</td></tr><tr><td>AFTER</td></tr></table>",
        paragraphs(60)
    );
    let pages = paginate(&html_src, "");
    assert_all_lines_in_order(&pages, 60);

    let last = pages.last().unwrap();
    let lines = lines_of(last);
    let last_line = lines
        .iter()
        .filter(|(t, _)| t.starts_with("LINE-"))
        .map(|(_, y)| *y)
        .fold(f32::MIN, f32::max);
    let after = lines
        .iter()
        .find(|(t, _)| t == "AFTER")
        .unwrap_or_else(|| panic!("AFTER is on the last page: {lines:?}"))
        .1;
    assert!(
        after > last_line,
        "AFTER at {after} is below the last line at {last_line}"
    );
}

#[test]
fn content_after_the_table_continues_below_a_split_row() {
    let html_src = format!("{}<p>THE-END</p>", single_cell_table(60));
    let pages = paginate(&html_src, "");
    assert_all_lines_in_order(&pages, 60);

    let lines = lines_of(pages.last().unwrap());
    let last_line = lines
        .iter()
        .filter(|(t, _)| t.starts_with("LINE-"))
        .map(|(_, y)| *y)
        .fold(f32::MIN, f32::max);
    let end = lines.iter().find(|(t, _)| t == "THE-END").unwrap().1;
    assert!(
        end > last_line,
        "THE-END at {end}, last line at {last_line}"
    );
}

#[test]
fn a_row_that_fits_a_page_still_moves_to_the_next_one_whole() {
    // Only a row that fits nowhere starts where it is. One that fits an empty page keeps
    // moving there rather than being cut at the foot of this one.
    let settings = PageSettings::default();
    let filler = settings.content_height() - 100.0;
    let html_src = format!(
        "<div class=\"filler\"></div><table><tr><td>{}</td></tr></table>",
        paragraphs(10)
    );
    let css = format!("* {{ margin: 0 }} .filler {{ height: {filler}px }}");
    let pages = paginate(&html_src, &css);
    let texts = texts_per_page(&pages);

    assert_eq!(pages.len(), 2);
    assert!(
        texts[0].is_empty(),
        "nothing of the row on page 1: {texts:?}"
    );
    assert_eq!(texts[1].len(), 10);
}

#[test]
fn a_row_nested_tables_split_through_still_starts_where_it_is() {
    // The issue: with nested tables the row was pushed to the next page too.
    let html_src = format!(
        "<p>HEAD</p><table><tr><td>{}</td></tr></table>",
        (1..=60)
            .map(|i| format!("<table><tr><td>LINE-{i} x</td></tr></table>"))
            .collect::<String>()
    );
    let pages = paginate(&html_src, "");
    let on = assert_all_lines_in_order(&pages, 60);
    assert_eq!(on[0], 0, "the first nested table is on the page of HEAD");
}

#[test]
fn a_split_row_encodes_to_a_pdf_with_every_page() {
    let src = single_cell_table(60);
    let dom = html::parse(src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ]);
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = sghtmltopdf::pdf::encode_pdf(
        &pages,
        &styles,
        &std::collections::HashMap::new(),
        &fonts,
        &settings,
    );

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(String::from_utf8_lossy(&bytes).contains("/Count 3"));
}

#[test]
fn a_row_taller_than_a_page_also_splits_in_streaming_mode() {
    use sghtmltopdf::engine::{Engine, EngineOptions, FontSpec, Mode};
    use sghtmltopdf::sink::MemorySink;

    let mut options = EngineOptions::default();
    options.mode = Mode::Streaming;
    options.fonts = vec![FontSpec {
        path: std::path::PathBuf::from(FONT_PATH),
        index: 0,
    }];
    let mut engine = Engine::new(options, MemorySink::new());
    engine
        .feed(format!("<html><body>{}</body></html>", single_cell_table(60)).as_bytes())
        .unwrap();
    let bytes = engine.finish().unwrap();

    assert!(String::from_utf8_lossy(&bytes).contains("/Count 3"));
}
