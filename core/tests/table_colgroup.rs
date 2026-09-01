//! E2E tests for `<colgroup>`/`<col>` (column width settings).
//!
//! The same approach as `table_rowspan.rs`/`table_caption.rs`: catch regressions by going
//! through the real pipeline (HTML parse, style cascade, layout, PDF encode).

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

/// Return the cell widths (border box) of the first table's first row.
fn first_row_cell_widths(html_src: &str, css: &str) -> Vec<f32> {
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

    fn find_table(b: &LaidOutBox) -> Option<Vec<f32>> {
        match &b.content {
            LaidOutContent::Table(table) => Some(
                table.rows[0]
                    .cells
                    .iter()
                    .map(|c| c.layout.border_box().width)
                    .collect(),
            ),
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                children.iter().find_map(find_table)
            }
            LaidOutContent::Grid(grid) => grid
                .rows
                .iter()
                .flat_map(|row| &row.items)
                .find_map(find_table),
            _ => None,
        }
    }
    find_table(&laid).expect("table not found")
}

#[test]
fn col_widths_shape_an_invoice_like_table() {
    // The layout common in an invoice: a wide item column with narrow quantity, unit price and amount columns.
    let widths = first_row_cell_widths(
        r#"<table>
             <colgroup>
               <col class="item">
               <col class="qty">
               <col class="price">
               <col class="amount">
             </colgroup>
             <thead><tr><th>品目</th><th>数量</th><th>単価</th><th>金額</th></tr></thead>
             <tbody><tr><td>サンプル商品</td><td>1</td><td>1,000</td><td>1,000</td></tr></tbody>
           </table>"#,
        "body { margin: 0; } table { border-spacing: 0; } \
         .qty, .price, .amount { width: 80px; }",
    );

    assert_eq!(widths.len(), 4);
    for w in &widths[1..] {
        assert!((w - 80.0).abs() < 0.5, "got {widths:?}");
    }
    assert!(
        widths[0] > 200.0,
        "the item column should take all the remaining width: {widths:?}"
    );
}

#[test]
fn a_table_with_colgroup_encodes_to_a_valid_pdf() {
    let dom = html::parse(
        br#"<table>
              <colgroup><col style="width: 30%;"><col span="2" style="width: 35%;"></colgroup>
              <tr><td>a</td><td>b</td><td>c</td></tr>
            </table>"#,
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
