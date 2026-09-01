//! E2E tests for CSS Grid (`display: grid`).
//!
//! The same approach as `flexbox.rs`: catch regressions by going through the real pipeline
//! (HTML parse, style cascade, layout, pagination, PDF encode).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, PageSettings,
    Rect,
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
        _ => None,
    }
}

fn layout(html_src: &str, css: &str) -> (Dom, LaidOutBox) {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
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

/// Return the content boxes of the grid items (the `<div>`s directly under `.g`) in document order.
fn item_boxes(html_src: &str, css: &str) -> Vec<Rect> {
    let (dom, laid) = layout(html_src, css);
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    divs.iter()
        .skip(1) // the first is the grid container itself
        .filter_map(|d| find_laid_out(&laid, *d).map(|b| b.layout.content))
        .collect()
}

const THREE_ITEMS: &str =
    r#"<div class="g"><div class="a">a</div><div class="b">b</div><div class="c">c</div></div>"#;

// ===== Track definitions =====

#[test]
fn fixed_length_tracks_lay_items_side_by_side() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; grid-template-columns: 100px 200px 100px; }",
    );
    assert_eq!(boxes[0].x, 0.0);
    assert_eq!(boxes[0].width, 100.0);
    assert_eq!(boxes[1].x, 100.0);
    assert_eq!(boxes[1].width, 200.0);
    assert_eq!(boxes[2].x, 300.0);
    assert_eq!(boxes[2].width, 100.0);
}

#[test]
fn fr_units_share_the_free_space() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 400px; \
         grid-template-columns: 1fr 2fr 1fr; }",
    );
    assert_eq!(boxes[0].width, 100.0);
    assert_eq!(boxes[1].width, 200.0);
    assert_eq!(boxes[2].width, 100.0);
}

#[test]
fn repeat_expands_to_the_given_count() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: repeat(3, 1fr); }",
    );
    assert_eq!(
        boxes.iter().map(|b| b.width).collect::<Vec<_>>(),
        vec![100.0, 100.0, 100.0]
    );
}

#[test]
fn repeat_auto_fill_derives_the_column_count_from_the_container() {
    // 400px with "at least 150px" columns gives 2 columns (the third item wraps to the second row).
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 400px; \
         grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); }",
    );
    assert_eq!(boxes[0].width, 200.0);
    assert_eq!(boxes[1].width, 200.0);
    assert_eq!(boxes[2].x, 0.0, "the third wraps to the next row");
    assert!(boxes[2].y > boxes[0].y);
}

#[test]
fn minmax_clamps_a_flexible_track() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 400px; \
         grid-template-columns: minmax(250px, 1fr) 1fr; }",
    );
    assert!(
        boxes[0].width >= 250.0,
        "it must not go below minmax's lower bound: {}",
        boxes[0].width
    );
}

#[test]
fn gap_inserts_space_between_tracks() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 400px; \
         grid-template-columns: 1fr 1fr; gap: 10px; }",
    );
    assert_eq!(boxes[0].width, 195.0);
    assert_eq!(boxes[1].x, 205.0);
    // The third is on the second row. row-gap applies too.
    assert!(boxes[2].y - boxes[0].y >= 10.0);
}

// ===== Placement =====

#[test]
fn grid_column_places_an_item_across_tracks() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: repeat(3, 100px); } .a { grid-column: 1 / 3; }",
    );
    assert_eq!(boxes[0].width, 200.0, "lines 1 to 3 = two tracks");
    assert_eq!(boxes[1].x, 200.0);
}

#[test]
fn grid_column_span_syntax_is_supported() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: repeat(3, 100px); } .a { grid-column: span 2; }",
    );
    assert_eq!(boxes[0].width, 200.0);
}

#[test]
fn grid_template_areas_place_items_by_name() {
    let boxes = item_boxes(
        THREE_ITEMS,
        r#"body { margin: 0; }
           .g { display: grid; width: 300px;
                grid-template-columns: 100px 200px;
                grid-template-areas: "a b" "c c"; }
           .a { grid-area: a; } .b { grid-area: b; } .c { grid-area: c; }"#,
    );
    assert_eq!(boxes[0].x, 0.0);
    assert_eq!(boxes[0].width, 100.0);
    assert_eq!(boxes[1].x, 100.0);
    assert_eq!(boxes[1].width, 200.0);
    // c is on the second row, spanning two columns.
    assert_eq!(boxes[2].x, 0.0);
    assert_eq!(boxes[2].width, 300.0);
    assert!(boxes[2].y > boxes[0].y);
}

#[test]
fn named_grid_lines_can_be_referenced() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: [start] 100px [mid] 100px [end2] 100px; } \
         .a { grid-column-start: mid; }",
    );
    assert_eq!(boxes[0].x, 100.0, "it starts at the named line `mid`");
}

#[test]
fn grid_auto_flow_column_fills_columns_first() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-rows: 30px 30px; grid-auto-flow: column; }",
    );
    // Two stack vertically in the first column and the third goes to the top of the second.
    assert_eq!(boxes[0].x, boxes[1].x);
    assert!(boxes[1].y > boxes[0].y);
    assert!(boxes[2].x > boxes[0].x);
    assert_eq!(boxes[2].y, boxes[0].y);
}

#[test]
fn justify_items_aligns_items_in_the_inline_axis() {
    // With `justify-items: start` an item shrinks to its content width and sits at the track's left edge.
    let stretched = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: 100px 100px 100px; }",
    );
    let started = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: 100px 100px 100px; justify-items: start; }",
    );
    assert_eq!(stretched[0].width, 100.0, "the initial value is stretch");
    assert!(
        started[0].width < 100.0,
        "justify-items: start shrinks it to the content width: {}",
        started[0].width
    );
    assert_eq!(started[0].x, 0.0);
}

#[test]
fn justify_self_overrides_justify_items_for_one_item() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: 100px 100px 100px; justify-items: start; } \
         .b { justify-self: stretch; }",
    );
    assert!(boxes[0].width < 100.0);
    assert_eq!(
        boxes[1].width, 100.0,
        "justify-self overrides it individually"
    );
}

// ===== Pagination =====

/// Collect the text from the laid-out tree.
fn collect_texts(b: &LaidOutBox, out: &mut Vec<String>) {
    match &b.content {
        LaidOutContent::Inline(lines) => {
            let text: String = lines
                .iter()
                .flat_map(|line| line.runs.iter())
                .map(|run| run.text.as_str())
                .collect();
            if !text.trim().is_empty() {
                out.push(text);
            }
        }
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_texts(child, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for item in grid.rows.iter().flat_map(|row| &row.items) {
                collect_texts(item, out);
            }
        }
        _ => {}
    }
}

fn paginate(html_src: &str, css: &str) -> Vec<Vec<String>> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    pages
        .iter()
        .map(|page| {
            let mut texts = Vec::new();
            for b in &page.boxes {
                collect_texts(b, &mut texts);
            }
            texts
        })
        .collect()
}

#[test]
fn a_tall_grid_splits_across_pages_by_row() {
    let cells: String = (1..=40)
        .map(|i| format!("<div>r{i}c1</div><div>r{i}c2</div>"))
        .collect();
    let pages = paginate(
        &format!(r#"<div class="g">{cells}</div>"#),
        "body { margin: 0; } .g { display: grid; grid-template-columns: 1fr 1fr; } \
         .g > div { height: 40px; }",
    );

    assert!(
        pages.len() > 1,
        "a grid that does not fit one page is split"
    );
    let total: usize = pages.iter().map(|p| p.len()).sum();
    assert_eq!(total, 80, "not a single cell is lost in the split");
    // It is not cut mid-row (the first cell of every page is always in the first column).
    for page in &pages {
        if let Some(first) = page.first() {
            assert!(
                first.ends_with("c1"),
                "the top of a page should be the first column of a row: {first}"
            );
        }
    }
}

#[test]
fn a_grid_that_fits_stays_on_one_page() {
    let pages = paginate(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; grid-template-columns: 1fr 1fr 1fr; }",
    );
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0].len(), 3);
}

// ===== E2E =====

#[test]
fn grid_renders_a_valid_pdf_end_to_end() {
    let html_src = r#"
        <div class="layout">
          <div class="header">header</div>
          <div class="side">side</div>
          <div class="main">main content</div>
          <div class="footer">footer</div>
        </div>"#;
    let css = r#"body { margin: 0; }
        .layout { display: grid; grid-template-columns: 120px 1fr; gap: 8px;
                  grid-template-areas: "header header" "side main" "footer footer"; }
        .header { grid-area: header; background-color: #cde; }
        .side { grid-area: side; background-color: #edc; }
        .main { grid-area: main; }
        .footer { grid-area: footer; background-color: #dec; }"#;

    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(bytes.windows(5).any(|w| w == b"%%EOF"));

    let mut texts = Vec::new();
    for page in &pages {
        for b in &page.boxes {
            collect_texts(b, &mut texts);
        }
    }
    assert!(texts.iter().any(|t| t.contains("header")));
    assert!(texts.iter().any(|t| t.contains("footer")));
}

// ===== Nested formatting contexts =====

/// The inner grid container is block-level, so by default it fills the outer track's width.
/// Back when its natural width measured as 0, the track collapsed and the content overflowed
/// one word at a time.
#[test]
fn a_grid_inside_a_grid_item_fills_the_track() {
    let boxes = item_boxes(
        r#"<div class="g"><div class="item"><div class="k">key</div>
           <div class="v">a much longer description that needs room</div></div></div>"#,
        "body { margin: 0; } .g { display: grid; width: 400px; } \
         .item { display: grid; grid-template-columns: auto 1fr; gap: 10px; }",
    );

    let (item, key, value) = (boxes[0], boxes[1], boxes[2]);
    assert_eq!(item.width, 400.0, "the inner grid fills the track width");
    assert!(
        key.width > 0.0,
        "an auto column takes the content width: {key:?}"
    );
    assert!(
        (value.width - (400.0 - 10.0 - key.width)).abs() < 0.5,
        "the 1fr column takes the remaining width: key={key:?} value={value:?}"
    );
}

/// Where the tracks are decided from the content (an explicit `justify-content: flex-start`,
/// so the `auto` tracks do not grow), the inner grid's natural width becomes the column width
/// outright. This checks not just "it does not collapse" but that it really is measured.
#[test]
fn a_nested_grid_is_measured_by_its_own_columns() {
    let boxes = item_boxes(
        r#"<div class="g"><div class="item"><div class="k">key</div>
           <div class="v">value</div></div></div>"#,
        "body { margin: 0; } \
         .g { display: grid; grid-template-columns: auto; justify-content: flex-start; width: 400px; } \
         .item { display: grid; grid-template-columns: max-content max-content; gap: 10px; }",
    );

    let (item, key, value) = (boxes[0], boxes[1], boxes[2]);
    assert!(
        item.width > 0.0 && item.width < 400.0,
        "it should shrink to the content width: {item:?}"
    );
    assert!(
        (item.width - (key.width + 10.0 + value.width)).abs() < 0.5,
        "the inner two columns plus the gap make the outer column width: item={item:?} key={key:?} value={value:?}"
    );
}

/// With a table inside, the natural width comes from the sum of a row's cell widths too.
#[test]
fn a_table_inside_a_grid_item_is_measured_by_its_rows() {
    let (dom, laid) = layout(
        r#"<div class="g"><div class="item"><table><tr><td>alpha</td><td>beta</td></tr></table></div></div>"#,
        "body { margin: 0; } \
         .g { display: grid; grid-template-columns: auto; justify-content: flex-start; width: 400px; } \
         .item { }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let item = find_laid_out(&laid, divs[1]).expect("item box");

    assert!(
        item.layout.content.width > 0.0 && item.layout.content.width < 400.0,
        "the column should be decided by the table's natural width: {:?}",
        item.layout.content
    );
}

/// With `justify-content`'s initial value `normal`, the `auto` tracks absorb the leftover width
/// and fill the container. Written explicitly as `flex-start` it keeps its content width and
/// sits to the left (how the slack is divided between tracks is left to taffy).
#[test]
fn auto_tracks_absorb_the_free_space_unless_justify_content_says_otherwise() {
    const HTML: &str = r#"<div class="g"><div class="a">key</div><div class="b">value</div></div>"#;
    let filled = item_boxes(
        HTML,
        "body { margin: 0; } .g { display: grid; grid-template-columns: auto auto; \
         gap: 10px; width: 400px; }",
    );
    let right_edge = filled[1].x + filled[1].width;
    assert!(
        (right_edge - 400.0).abs() < 0.5,
        "by default it fills the container: {filled:?}"
    );

    let packed = item_boxes(
        HTML,
        "body { margin: 0; } .g { display: grid; grid-template-columns: auto auto; \
         gap: 10px; width: 400px; justify-content: flex-start; }",
    );
    assert!(
        packed[1].x + packed[1].width < 200.0,
        "with flex-start it keeps the content width and sits to the left: {packed:?}"
    );
}
