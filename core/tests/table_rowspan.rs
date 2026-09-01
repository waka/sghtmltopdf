//! E2E tests for `rowspan` on table cells.
//!
//! The same approach as `table_caption.rs`/`table_vertical_align.rs`: catch regressions by
//! going through the real pipeline. The detailed coordinate checks run against the result of
//! `layout_document` (before pagination).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
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

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
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
        LaidOutContent::Table(table) => table
            .caption
            .as_deref()
            .and_then(|c| find_laid_out(c, target))
            .or_else(|| {
                table
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .find_map(|cell| find_laid_out(cell, target))
            }),
        LaidOutContent::Inline(_) | LaidOutContent::Image(_) => None,
    }
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

#[test]
fn rowspan_cell_spans_two_rows_and_the_next_row_flows_around_it_end_to_end() {
    let html_src = r#"<table>
        <tr><td rowspan="2" style="height: 80px;">tall</td><td style="height: 10px;">a</td></tr>
        <tr><td style="height: 10px;">b</td></tr>
    </table>"#;
    let css = "body { margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let mut tds = Vec::new();
    find_all_tags(&dom, dom.document(), "td", &mut tds);
    assert_eq!(tds.len(), 3);

    let tall = find_laid_out(&laid, tds[0]).expect("tall cell not found");
    let a = find_laid_out(&laid, tds[1]).expect("cell a not found");
    let b = find_laid_out(&laid, tds[2]).expect("cell b not found");

    assert!(
        (tall.layout.margin_box_height() - 80.0).abs() < 0.5,
        "the rowspan cell should span the combined height of both rows: {}",
        tall.layout.margin_box_height()
    );
    // "b" avoids the column "tall" occupies (col0) and flows into the same column as "a"
    // (col1), starting directly below "tall" (y=40px).
    assert!(
        (b.layout.border_box().x - a.layout.border_box().x).abs() < 0.5,
        "cell b should land in the same column as cell a, skipping the rowspan cell's column"
    );
    assert!(
        (b.layout.border_box().y - 40.0).abs() < 0.5,
        "cell b should start after row0's height(40px): {}",
        b.layout.border_box().y
    );

    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

#[test]
fn table_without_rowspan_behaves_as_before_end_to_end() {
    let html_src = r#"<table><tr><td>a</td><td>b</td></tr></table>"#;
    let css = "body { margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let mut tds = Vec::new();
    find_all_tags(&dom, dom.document(), "td", &mut tds);
    let a = find_laid_out(&laid, tds[0]).expect("cell a not found");
    let b = find_laid_out(&laid, tds[1]).expect("cell b not found");

    assert_eq!(a.layout.border_box().y, 0.0);
    assert_eq!(b.layout.border_box().y, 0.0);
    assert!(
        (b.layout.border_box().x - (a.layout.border_box().x + a.layout.border_box().width)).abs()
            < 0.5,
        "cells without rowspan should still sit side by side with no gap"
    );
}

#[test]
fn a_row_whose_only_cell_has_a_rowspan_still_opens_a_column_for_the_next_row() {
    // Where the first row is a single rowspan=2 cell, "the maximum colspan sum per row"
    // gives a column count of only 1, and the second row's cell had nowhere to go and vanished.
    let html_src = r#"<table style="width: 400px;">
        <tr><td rowspan="2" style="width: 150px;">Logo</td></tr>
        <tr><td>Second row text</td></tr>
    </table>"#;
    let css = "body { margin: 0; }";

    let (dom, laid) = layout(html_src, css);
    let mut tds = Vec::new();
    find_all_tags(&dom, dom.document(), "td", &mut tds);
    assert_eq!(tds.len(), 2);

    let logo = find_laid_out(&laid, tds[0]).expect("rowspan cell not found");
    let second = find_laid_out(&laid, tds[1]).expect("second row cell not found");

    // The second row's cell goes to col1, avoiding the col0 the rowspan occupies, so it
    // should be placed to the right of "Logo" with a width of its own.
    let logo_box = logo.layout.border_box();
    let logo_right = logo_box.x + logo_box.width;
    assert!(
        second.layout.border_box().x >= logo_right,
        "the second row's cell should sit in the column next to the rowspan cell \
         (x={}, rowspan cell right edge={logo_right})",
        second.layout.border_box().x
    );
    assert!(
        second.layout.content.width > 0.0,
        "the second row's cell should get a share of the table width, got {}",
        second.layout.content.width
    );
    // The text really is laid out as a line (collapsing to a zero-width column would lose the line itself).
    let LaidOutContent::Inline(lines) = &second.content else {
        panic!("expected inline content in the second row's cell");
    };
    let text: String = lines
        .iter()
        .flat_map(|line| line.runs.iter())
        .map(|run| run.text.as_str())
        .collect();
    assert!(
        text.contains("Second"),
        "the second row's text should survive layout, got {text:?}"
    );
}
