//! CSS Grid(`display: grid`)のE2Eテスト。
//!
//! `flexbox.rs`と同じ方針: 実際のパイプライン(HTMLパース→スタイルカスケード→
//! レイアウト→ページ分割→PDFエンコード)を通して回帰を検知する。

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

/// グリッドアイテム(`.g`直下の`<div>`)のcontent boxを、文書順に返す。
fn item_boxes(html_src: &str, css: &str) -> Vec<Rect> {
    let (dom, laid) = layout(html_src, css);
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    divs.iter()
        .skip(1) // 先頭はグリッドコンテナ自身
        .filter_map(|d| find_laid_out(&laid, *d).map(|b| b.layout.content))
        .collect()
}

const THREE_ITEMS: &str =
    r#"<div class="g"><div class="a">a</div><div class="b">b</div><div class="c">c</div></div>"#;

// ===== トラック定義 =====

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
    // 400pxに「最小150px」の列 → 2列(3つ目のアイテムは2行目へ折り返す)。
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 400px; \
         grid-template-columns: repeat(auto-fill, minmax(150px, 1fr)); }",
    );
    assert_eq!(boxes[0].width, 200.0);
    assert_eq!(boxes[1].width, 200.0);
    assert_eq!(boxes[2].x, 0.0, "3つ目は次の行へ折り返す");
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
        "minmaxの下限を下回ってはいけない: {}",
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
    // 3つ目は2行目。row-gapも効く。
    assert!(boxes[2].y - boxes[0].y >= 10.0);
}

// ===== 配置 =====

#[test]
fn grid_column_places_an_item_across_tracks() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-columns: repeat(3, 100px); } .a { grid-column: 1 / 3; }",
    );
    assert_eq!(boxes[0].width, 200.0, "1〜3ライン=2トラック分");
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
    // cは2列にまたがる2行目。
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
    assert_eq!(boxes[0].x, 100.0, "名前付きライン`mid`から始まる");
}

#[test]
fn grid_auto_flow_column_fills_columns_first() {
    let boxes = item_boxes(
        THREE_ITEMS,
        "body { margin: 0; } .g { display: grid; width: 300px; \
         grid-template-rows: 30px 30px; grid-auto-flow: column; }",
    );
    // 1列目に2つ縦に並び、3つ目が2列目の先頭へ。
    assert_eq!(boxes[0].x, boxes[1].x);
    assert!(boxes[1].y > boxes[0].y);
    assert!(boxes[2].x > boxes[0].x);
    assert_eq!(boxes[2].y, boxes[0].y);
}

#[test]
fn justify_items_aligns_items_in_the_inline_axis() {
    // `justify-items: start`ならアイテムは内容幅に縮み、トラック左端に寄る。
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
    assert_eq!(stretched[0].width, 100.0, "初期値はstretch");
    assert!(
        started[0].width < 100.0,
        "justify-items: startで内容幅に縮む: {}",
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
    assert_eq!(boxes[1].width, 100.0, "justify-selfが個別に上書きする");
}

// ===== ページ分割 =====

/// レイアウト済みツリーからテキストを収集する。
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

    assert!(pages.len() > 1, "1ページに収まらないグリッドは分割される");
    let total: usize = pages.iter().map(|p| p.len()).sum();
    assert_eq!(total, 80, "分割してもセルは1つも失われない");
    // 行の途中で切れていない(各ページの先頭セルは必ず1列目)。
    for page in &pages {
        if let Some(first) = page.first() {
            assert!(
                first.ends_with("c1"),
                "ページ先頭は行の1列目のはず: {first}"
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

// ===== ネストしたフォーマッティングコンテキスト =====

/// 内側のグリッドコンテナはブロックレベルなので、既定では外側のトラック幅を
/// 埋める。自然幅が0として測られていた頃は、トラックが潰れて内容が1語ずつ
/// 溢れていた。
#[test]
fn a_grid_inside_a_grid_item_fills_the_track() {
    let boxes = item_boxes(
        r#"<div class="g"><div class="item"><div class="k">key</div>
           <div class="v">a much longer description that needs room</div></div></div>"#,
        "body { margin: 0; } .g { display: grid; width: 400px; } \
         .item { display: grid; grid-template-columns: auto 1fr; gap: 10px; }",
    );

    let (item, key, value) = (boxes[0], boxes[1], boxes[2]);
    assert_eq!(item.width, 400.0, "内側のグリッドはトラック幅を埋める");
    assert!(key.width > 0.0, "auto列は内容幅になる: {key:?}");
    assert!(
        (value.width - (400.0 - 10.0 - key.width)).abs() < 0.5,
        "1fr列が残り幅を取る: key={key:?} value={value:?}"
    );
}

/// トラックが内容基準で決まる場合(明示的な`justify-content: flex-start`で
/// `auto`トラックが伸びない)は、内側のグリッドの自然幅がそのまま列幅になる。
/// 「潰れない」ことだけでなく「実際に測れている」ことの確認。
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
        "内容幅に縮むはず: {item:?}"
    );
    assert!(
        (item.width - (key.width + 10.0 + value.width)).abs() < 0.5,
        "内側の2列+gapの合計が外側の列幅になる: item={item:?} key={key:?} value={value:?}"
    );
}

/// テーブルを内側に持つ場合も、行のセル幅合計から自然幅が出る。
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
        "テーブルの自然幅で列が決まるはず: {:?}",
        item.layout.content
    );
}

/// `justify-content`の初期値`normal`では、余った幅を`auto`トラックが吸って
/// コンテナを埋める。明示的に`flex-start`と書いた場合は内容幅のまま左に寄る。
/// (トラック間での余白の配分比率はtaffy任せで、CSSの均等配分とは一致しない)
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
        "既定ではコンテナを埋める: {filled:?}"
    );

    let packed = item_boxes(
        HTML,
        "body { margin: 0; } .g { display: grid; grid-template-columns: auto auto; \
         gap: 10px; width: 400px; justify-content: flex-start; }",
    );
    assert!(
        packed[1].x + packed[1].width < 200.0,
        "flex-startなら内容幅のまま左に寄る: {packed:?}"
    );
}

/// Every text line on the page as (text, in-page y, height), in document order.
fn text_lines_on_page(page: &sghtmltopdf_core::layout::Page) -> Vec<(String, f32, f32)> {
    fn walk(b: &LaidOutBox, out: &mut Vec<(String, f32, f32)>) {
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for child in children {
                    walk(child, out);
                }
            }
            LaidOutContent::Grid(grid) => {
                for item in grid.rows.iter().flat_map(|row| &row.items) {
                    walk(item, out);
                }
            }
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    let text: String = line.runs.iter().map(|run| run.text.as_str()).collect();
                    if !text.trim().is_empty() {
                        out.push((text, line.rect.y, line.rect.height));
                    }
                }
            }
            _ => {}
        }
    }
    let mut out = Vec::new();
    for b in &page.boxes {
        walk(b, &mut out);
    }
    out
}

#[test]
fn grid_items_on_later_pages_are_placed_within_the_page() {
    // #18: in the row bands of the second and later pages, the band's own
    // coordinates were corrected to in-page ones but the items inside were
    // shifted the opposite way, landing far below the page, where nothing is
    // painted. The page count was right, but every page after the first was blank.
    let paragraphs: String = (0..150).map(|i| format!("<p>Line {i}</p>")).collect();
    let html = format!(r#"<div class="g">{paragraphs}</div>"#);
    let css = "* { margin: 0; padding: 0 } .g { display: grid; }";
    let dom = html::parse(html.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = test_fonts();
    let settings = PageSettings::default();
    let page_height = settings.content_height();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);

    assert!(pages.len() > 1, "150 paragraphs do not fit on one page");
    let mut seen = Vec::new();
    for (page_index, page) in pages.iter().enumerate() {
        let lines = text_lines_on_page(page);
        assert!(!lines.is_empty(), "page {page_index} must not be blank");
        for (text, y, height) in lines {
            assert!(
                y >= -0.01 && y + height <= page_height + 0.01,
                "{text:?} on page {page_index} is outside the page: y={y} height={height} page_height={page_height}"
            );
            seen.push(text);
        }
    }
    let expected: Vec<String> = (0..150).map(|i| format!("Line {i}")).collect();
    assert_eq!(
        seen, expected,
        "every paragraph appears exactly once, in order"
    );
}

#[test]
fn a_paginated_grid_under_collapsing_margins_stays_inside_its_pages() {
    // 行帯(`LaidOutGridRow`の`top`/`bottom`)だけが`shift_box_y_in_place`で
    // アイテムと逆向きに動いていたため、マージン相殺で子がシフトされる構造の
    // 中にあるグリッドは、2ページ目以降がページ上端より上(負のy)に置かれて
    // 欠けていた。
    let cells: String = (0..40).map(|i| format!("<div>g{i}</div>")).collect();
    let html =
        format!(r#"<div class="wrap"><div class="inner"><div class="g">{cells}</div></div></div>"#);
    let css = "* { margin: 0; padding: 0 } \
               .wrap { margin-top: 20px } \
               .inner { margin-top: 40px } \
               .g { display: grid; grid-template-columns: 1fr 1fr } \
               .g > div { height: 60px }";
    let dom = html::parse(html.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let settings = PageSettings::default();
    let page_height = settings.content_height();
    let pages = paginate_document(&dom, &styles, &test_fonts(), &settings);

    assert!(pages.len() > 1, "20行×60pxは1ページに収まらない");
    for (page_index, page) in pages.iter().enumerate() {
        let lines = text_lines_on_page(page);
        assert!(!lines.is_empty(), "ページ{page_index}が空");
        for (text, y, height) in &lines {
            assert!(
                *y >= -0.01 && y + height <= page_height + 0.01,
                "{text:?}がページ{page_index}の外に出ている: y={y} height={height}"
            );
        }
        let first_y = lines[0].1;
        let expected = if page_index == 0 { 40.0 } else { 0.0 };
        assert!(
            (first_y - expected).abs() < 0.01,
            "ページ{page_index}の先頭行はy={expected}のはず(相殺後の上マージンは1ページ目にだけ効く): y={first_y}"
        );
    }
}

#[test]
fn a_grid_row_that_does_not_fit_the_rest_of_the_page_starts_on_the_next_page() {
    // 行帯の分割判定は「この断片に既に1行以上ある」ことを条件にしていたため、
    // グリッドの最初の行帯だけは、ページの残り高さに収まらなくてもその場に
    // 置かれ、ページ下端からはみ出して欠けていた(blockとflexは次ページへ送る)。
    let settings = PageSettings::default();
    let filler_height = settings.content_height() - 30.0;
    let html = r#"<div class="filler"></div><div class="g"><div>ga</div><div>gb</div><div>gc</div><div>gd</div></div>"#;
    let css = format!(
        "* {{ margin: 0; padding: 0 }} .filler {{ height: {filler_height}px }} \
         .g {{ display: grid; grid-template-columns: 1fr 1fr }} .g > div {{ height: 40px }}"
    );
    let dom = html::parse(html.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(&css));
    let page_height = settings.content_height();
    let pages = paginate_document(&dom, &styles, &test_fonts(), &settings);

    assert_eq!(pages.len(), 2, "グリッドは2ページ目へ送られる");
    assert!(
        text_lines_on_page(&pages[0]).is_empty(),
        "1ページ目には残り30pxしかないので、40pxの行帯は置けない: {:?}",
        text_lines_on_page(&pages[0])
    );
    let second = text_lines_on_page(&pages[1]);
    assert_eq!(
        second.len(),
        4,
        "2行帯4アイテムが2ページ目に載る: {second:?}"
    );
    for (text, y, height) in &second {
        assert!(
            *y >= -0.01 && y + height <= page_height + 0.01,
            "{text:?}がページからはみ出している: y={y} height={height}"
        );
    }
    assert!(
        (second[0].1 - 0.0).abs() < 0.01,
        "2ページ目の先頭はページ上端から始まる: {:?}",
        second[0]
    );
}
