//! E2E tests for `vertical-align` on table cells (top/middle/bottom/baseline).
//!
//! The same approach as `table_caption.rs`: catch regressions by going through the real
//! pipeline. The detailed coordinate checks run against the result of `layout_document` (before pagination).

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

fn first_line_y(cell: &LaidOutBox) -> f32 {
    let LaidOutContent::Inline(lines) = &cell.content else {
        panic!("expected inline content");
    };
    lines[0].rect.y
}

#[test]
fn vertical_align_bottom_pushes_shorter_cell_content_down_end_to_end() {
    let html_src = r#"<table>
        <tr><td class="short">a</td><td class="tall">b</td></tr>
    </table>"#;
    let css = "body { margin: 0; } \
               td { vertical-align: bottom; } \
               .short { height: 10px; } \
               .tall { height: 80px; }";

    let (dom, laid) = layout(html_src, css);
    let mut tds = Vec::new();
    find_all_tags(&dom, dom.document(), "td", &mut tds);
    assert_eq!(tds.len(), 2);

    let short_cell = find_laid_out(&laid, tds[0]).expect("short cell not found");
    let tall_cell = find_laid_out(&laid, tds[1]).expect("tall cell not found");

    assert!(
        first_line_y(tall_cell).abs() < 0.5,
        "the tallest cell's own content should not shift"
    );
    assert!(
        (first_line_y(short_cell) - 70.0).abs() < 0.5,
        "the shorter cell's content should be pushed down to the bottom (deficit=70px)"
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
}

#[test]
fn vertical_align_baseline_is_the_default_and_aligns_mixed_font_sizes_end_to_end() {
    let html_src = r#"<table><tr><td class="small">Ay</td><td class="large">Ay</td></tr></table>"#;
    let css = "body { margin: 0; } .small { font-size: 12px; } .large { font-size: 36px; }";

    let (dom, laid) = layout(html_src, css);
    let mut tds = Vec::new();
    find_all_tags(&dom, dom.document(), "td", &mut tds);
    let small_cell = find_laid_out(&laid, tds[0]).expect("small cell not found");
    let large_cell = find_laid_out(&laid, tds[1]).expect("large cell not found");

    let fonts = test_fonts();
    let baseline_y = |cell: &LaidOutBox| {
        let LaidOutContent::Inline(lines) = &cell.content else {
            panic!("expected inline content");
        };
        let run = lines[0].runs.first().expect("cell should have text");
        let font = fonts.get(run.font_index).expect("font should be loaded");
        lines[0].rect.y + font.baseline_offset(run.font_size, lines[0].rect.height)
    };

    assert!(
        (baseline_y(small_cell) - baseline_y(large_cell)).abs() < 0.5,
        "baseline should be shared across cells with different font sizes by default"
    );
}
