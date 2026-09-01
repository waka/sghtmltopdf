//! E2E tests for the anonymous table box generation of CSS2.1 17.2.1.
//!
//! The same approach as `table_rowspan.rs`: catch regressions by going through the real
//! pipeline. The structural checks run against the result of `layout_document` (before pagination).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
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

fn layout(html_src: &str) -> LaidOutBox {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("body { margin: 0; }");
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

/// Collect the drawn rows (line boxes carrying text) along with their text and position.
fn collect_text_lines(b: &LaidOutBox, out: &mut Vec<(String, f32, f32)>) {
    match &b.content {
        LaidOutContent::Inline(lines) => {
            for line in lines {
                let text: String = line.runs.iter().map(|run| run.text.as_str()).collect();
                if !text.trim().is_empty() {
                    out.push((text, line.rect.x, line.rect.y));
                }
            }
        }
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_text_lines(child, out);
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = table.caption.as_deref() {
                collect_text_lines(caption, out);
            }
            for cell in table.rows.iter().flat_map(|row| &row.cells) {
                collect_text_lines(cell, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for item in grid.rows.iter().flat_map(|row| &row.items) {
                collect_text_lines(item, out);
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

fn texts(html_src: &str) -> Vec<String> {
    let laid = layout(html_src);
    let mut lines = Vec::new();
    collect_text_lines(&laid, &mut lines);
    lines.into_iter().map(|(text, _, _)| text).collect()
}

/// Go from layout all the way to PDF and confirm it produces a one-page PDF without crashing.
fn renders_to_a_valid_pdf(html_src: &str) {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("body { margin: 0; }");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
    assert!(bytes.starts_with(b"%PDF-"));
}

const CELLS_WITHOUT_A_ROW: &str = r#"<div style="display: table">
    <div style="display: table-cell">alpha</div>
    <div style="display: table-cell">beta</div>
</div>"#;

#[test]
fn table_cells_without_a_row_get_an_anonymous_row() {
    // CSS2.1 17.2.1 rule 2.1. Without a row box, the cells vanished from the output entirely.
    assert_eq!(texts(CELLS_WITHOUT_A_ROW), vec!["alpha", "beta"]);

    let laid = layout(CELLS_WITHOUT_A_ROW);
    let mut lines = Vec::new();
    collect_text_lines(&laid, &mut lines);
    let (_, alpha_x, alpha_y) = lines[0];
    let (_, beta_x, beta_y) = lines[1];
    assert!(
        beta_x > alpha_x,
        "the two cells should sit side by side: alpha at {alpha_x}, beta at {beta_x}"
    );
    assert!(
        (alpha_y - beta_y).abs() < 0.5,
        "both cells belong to the same anonymous row, so they share a baseline row"
    );

    renders_to_a_valid_pdf(CELLS_WITHOUT_A_ROW);
}

#[test]
fn a_plain_block_inside_a_table_gets_an_anonymous_cell_and_row() {
    let html_src = r#"<div style="display: table"><div>alpha</div></div>"#;
    assert_eq!(texts(html_src), vec!["alpha"]);
    renders_to_a_valid_pdf(html_src);
}

#[test]
fn a_plain_block_inside_a_table_row_gets_an_anonymous_cell() {
    // CSS2.1 17.2.1 rule 2.2.
    let html_src = r#"<div style="display: table">
        <div style="display: table-row">
            <div style="display: table-cell">alpha</div>
            <div>beta</div>
        </div>
    </div>"#;
    assert_eq!(texts(html_src), vec!["alpha", "beta"]);
    renders_to_a_valid_pdf(html_src);
}

#[test]
fn consecutive_non_cell_children_share_one_anonymous_cell() {
    // A consecutive run of "children that are not cells" gathers into one anonymous cell
    // (rule 2.2), so they stack vertically. Separate cells would sit side by side.
    let html_src = r#"<div style="display: table">
        <div style="display: table-row">
            <div>alpha</div>
            <div>beta</div>
        </div>
    </div>"#;
    let laid = layout(html_src);
    let mut lines = Vec::new();
    collect_text_lines(&laid, &mut lines);
    assert_eq!(lines.len(), 2);
    let (_, alpha_x, alpha_y) = lines[0];
    let (_, beta_x, beta_y) = lines[1];
    assert!(
        (alpha_x - beta_x).abs() < 0.5,
        "both blocks belong to the same anonymous cell, so they share the left edge"
    );
    assert!(
        beta_y > alpha_y,
        "and stack vertically inside it: {alpha_y} then {beta_y}"
    );
}

#[test]
fn explicit_rows_and_real_table_elements_are_unchanged() {
    let with_row = r#"<div style="display: table">
        <div style="display: table-row">
            <div style="display: table-cell">alpha</div>
            <div style="display: table-cell">beta</div>
        </div>
    </div>"#;
    assert_eq!(texts(with_row), vec!["alpha", "beta"]);
    assert_eq!(
        texts(r#"<table><tr><td>alpha</td><td>beta</td></tr></table>"#),
        vec!["alpha", "beta"]
    );
}

#[test]
fn whitespace_and_column_elements_do_not_create_anonymous_boxes() {
    // Whitespace between rows and cells, and `<colgroup>`/`<col>`, create no anonymous box.
    let html_src = r#"<table>
        <colgroup><col style="width: 50px"><col></colgroup>
        <tr><td>alpha</td><td>beta</td></tr>
    </table>"#;
    assert_eq!(texts(html_src), vec!["alpha", "beta"]);
}
