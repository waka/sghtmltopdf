//! E2E tests for the legacy HTML presentational attributes.
//!
//! They confirm, through the real pipeline, that wkhtmltopdf-era business-document HTML
//! (built on `<table border cellpadding>`) comes out looking right.

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
        _ => None,
    }
}

fn first_cell(dom: &Dom, laid: &LaidOutBox) -> LaidOutBox {
    let td = find_tag(dom, dom.document(), "td").expect("td not found");
    find_laid_out(laid, td).expect("cell box not found").clone()
}

#[test]
fn a_wkhtmltopdf_era_invoice_table_gets_borders_and_padding() {
    let (dom, laid) = layout(
        r##"<table border="1" cellpadding="6" cellspacing="0" width="100%">
             <tr bgcolor="#eeeeee"><th align="left">品目</th><th align="right">金額</th></tr>
             <tr><td>サンプル</td><td align="right">1,000</td></tr>
           </table>"##,
        "body { margin: 0; }",
    );

    let cell = first_cell(&dom, &laid);
    assert_eq!(cell.layout.border.top, 1.0, "cells get a 1px border");
    assert_eq!(cell.layout.padding.top, 6.0, "cellpadding becomes padding");

    let table = find_tag(&dom, dom.document(), "table").expect("table not found");
    let table_box = find_laid_out(&laid, table).expect("table box not found");
    assert_eq!(table_box.layout.border.top, 1.0, "the table itself too");
}

#[test]
fn author_css_can_still_override_the_attributes() {
    let (dom, laid) = layout(
        r#"<table border="1" cellpadding="6"><tr><td>x</td></tr></table>"#,
        "body { margin: 0; } td { padding: 0; border-width: 0; }",
    );
    let cell = first_cell(&dom, &laid);
    assert_eq!(cell.layout.padding.top, 0.0);
    assert_eq!(cell.layout.border.top, 0.0);
}

#[test]
fn table_width_and_cell_width_attributes_size_the_table() {
    let (dom, laid) = layout(
        r#"<table width="400" cellspacing="0"><tr><td width="100">a</td><td>b</td></tr></table>"#,
        "body { margin: 0; }",
    );
    let table = find_tag(&dom, dom.document(), "table").expect("table not found");
    let table_box = find_laid_out(&laid, table).expect("table box not found");
    assert!(
        (table_box.layout.content.width - 400.0).abs() < 0.5,
        "got {}",
        table_box.layout.content.width
    );
}

#[test]
fn font_element_changes_color_and_size() {
    let (dom, laid) = layout(
        r##"<p><font size="6" color="#ff0000">big red</font></p>"##,
        "body { margin: 0; }",
    );
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p box not found");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };
    let run = &lines[0].runs[0];
    assert_eq!(run.font_size, 32.0, "size=6 maps to 32px");
    assert_eq!(
        (run.color.red, run.color.green, run.color.blue),
        (255, 0, 0)
    );
}

#[test]
fn center_element_and_align_attribute_center_their_content() {
    let (dom, laid) = layout(
        r#"<center>centered</center><p align="right">right</p>"#,
        "body { margin: 0; }",
    );
    let center = find_tag(&dom, dom.document(), "center").expect("center not found");
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");

    let line_x = |node: NodeId| {
        let b = find_laid_out(&laid, node).expect("box not found");
        let LaidOutContent::Inline(lines) = &b.content else {
            panic!("expected inline content");
        };
        lines[0].runs[0].x_offset + lines[0].rect.x
    };
    assert!(line_x(center) > 100.0, "centered text should be indented");
    assert!(
        line_x(p) > 100.0,
        "right-aligned text should be pushed right"
    );
}

#[test]
fn ol_type_attribute_changes_the_marker_style() {
    let (dom, laid) = layout(r#"<ol type="A"><li>first</li></ol>"#, "body { margin: 0; }");
    let li = find_tag(&dom, dom.document(), "li").expect("li not found");
    let li_box = find_laid_out(&laid, li).expect("li box not found");
    let marker = li_box.marker.as_ref().expect("list marker not laid out");
    let text: String = marker.runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(text, "A.");
}

#[test]
fn a_legacy_document_encodes_to_a_valid_pdf() {
    let dom = html::parse(
        r##"<body bgcolor="#ffffff" text="#000000">
               <center><font size="5" face="DejaVu Sans">請求書</font></center>
               <hr width="80%" size="2" noshade>
               <table border="1" cellpadding="4" cellspacing="0" width="100%">
                 <tr bgcolor="#dddddd"><th>品目</th><th>数量</th><th>金額</th></tr>
                 <tr><td>商品A</td><td align="center">2</td><td align="right">2,000</td></tr>
               </table>
             </body>"##
            .as_bytes(),
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

#[test]
fn tr_bgcolor_paints_a_row_background() {
    // A `<tr bgcolor>` (and the CSS `tr { background-color }`) is painted as a rectangle
    // covering that row's cells. The fill operators are Flate compressed, so this is checked
    // by the generated PDF's size changing with and without the row background.
    let with_bg = build_pdf(
        r##"<table cellspacing="0"><tr bgcolor="#ff0000"><td>a</td><td>b</td></tr></table>"##,
    );
    let without_bg = build_pdf(r#"<table cellspacing="0"><tr><td>a</td><td>b</td></tr></table>"#);
    assert!(
        with_bg.len() > without_bg.len(),
        "a row background should add drawing operators ({} vs {})",
        with_bg.len(),
        without_bg.len()
    );
}

fn build_pdf(html_src: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("body { margin: 0; }");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
    assert!(bytes.starts_with(b"%PDF-"));
    bytes
}
