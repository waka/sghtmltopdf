//! E2E tests for the PDF outline built from `bookmark-level`, `bookmark-label` and
//! `bookmark-state`.
//!
//! They read `/Outlines`, the `/First`/`/Next` chains and the `/Dest` of each item out of the
//! generated PDF bytes (an outline item is a plain dictionary object, so unlike a
//! Flate-compressed content stream it can be parsed directly).

use std::collections::HashMap;
use std::path::PathBuf;

use sghtmltopdf::engine::{Engine, EngineOptions, FontSpec, HeaderFooterHtml, Mode};
use sghtmltopdf::sink::MemorySink;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn build_pdf(html: &str, mode: Mode) -> Vec<u8> {
    build_pdf_with(html, mode, |_| {})
}

fn build_pdf_with(html: &str, mode: Mode, configure: impl FnOnce(&mut EngineOptions)) -> Vec<u8> {
    let mut options = EngineOptions::default();
    options.mode = mode;
    options.fonts = vec![FontSpec {
        path: PathBuf::from(FONT_PATH),
        index: 0,
    }];
    configure(&mut options);
    let mut engine = Engine::new(options, MemorySink::new());
    engine.feed(html.as_bytes()).unwrap();
    let bytes = engine.finish().unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    bytes
}

/// One outline item, flattened in outline order.
#[derive(Debug, PartialEq)]
struct Item {
    title: String,
    /// 0 for a top-level item.
    depth: usize,
    /// The 0-based index of the destination page.
    page: usize,
    /// The destination's top position, in pt from the bottom of the page.
    top: f32,
    count: Option<i32>,
}

struct Pdf {
    objects: HashMap<u32, Vec<u8>>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

impl Pdf {
    fn parse(bytes: &[u8]) -> Self {
        let mut objects = HashMap::new();
        let mut at = 0;
        while let Some(end) = find(bytes, b" 0 obj", at) {
            let start = bytes[..end]
                .iter()
                .rposition(|b| !b.is_ascii_digit())
                .map_or(0, |p| p + 1);
            let number = std::str::from_utf8(&bytes[start..end])
                .ok()
                .and_then(|s| s.parse::<u32>().ok());
            let close = find(bytes, b"endobj", end).unwrap_or(bytes.len());
            if let Some(number) = number {
                objects.insert(number, bytes[end + 6..close].to_vec());
            }
            at = close;
        }
        Self { objects }
    }

    /// An object's bytes as text, one char per byte (so PDFDocEncoding Latin-1 survives).
    fn text(&self, number: u32) -> String {
        self.objects[&number].iter().map(|&b| b as char).collect()
    }

    fn catalog(&self) -> String {
        self.objects
            .keys()
            .map(|&n| self.text(n))
            .find(|t| t.contains("/Type /Catalog"))
            .expect("a catalog")
    }

    fn page_refs(&self) -> Vec<u32> {
        let pages = self
            .objects
            .keys()
            .map(|&n| self.text(n))
            .find(|t| t.contains("/Type /Pages"))
            .expect("a page tree");
        let kids = &pages[pages.find("/Kids [").unwrap() + 7..];
        kids[..kids.find(']').unwrap()]
            .split("0 R")
            .filter_map(|s| s.trim().parse().ok())
            .collect()
    }

    /// The outline items in outline order, or `None` for a PDF with no `/Outlines`.
    fn outline(&self) -> Option<Vec<Item>> {
        let root = ref_after(&self.catalog(), "/Outlines")?;
        let root_text = self.text(root);
        assert!(root_text.contains("/Type /Outlines"), "{root_text}");
        let pages = self.page_refs();
        let mut items = Vec::new();
        self.walk(ref_after(&root_text, "/First"), root, 0, &pages, &mut items);
        let top_level = items.iter().filter(|i| i.depth == 0).count();
        assert!(top_level > 0, "an outline without items is not written");
        Some(items)
    }

    /// Follow a `/Next` chain from `first` and recurse into each item's `/First`, checking
    /// the `/Parent` and `/Prev` links on the way.
    fn walk(
        &self,
        first: Option<u32>,
        parent: u32,
        depth: usize,
        pages: &[u32],
        out: &mut Vec<Item>,
    ) {
        let mut current = first;
        let mut prev = None;
        while let Some(number) = current {
            let text = self.text(number);
            assert_eq!(ref_after(&text, "/Parent"), Some(parent), "{text}");
            assert_eq!(ref_after(&text, "/Prev"), prev, "{text}");
            let dest = &text[text.find("/Dest [").expect("a destination") + 7..];
            let dest: Vec<&str> = dest[..dest.find(']').unwrap()].split_whitespace().collect();
            assert_eq!(dest[3], "/XYZ", "{text}");
            let page_ref: u32 = dest[0].parse().unwrap();
            out.push(Item {
                title: title_of(&text),
                depth,
                page: pages.iter().position(|&p| p == page_ref).expect("a page"),
                top: dest[5].parse().unwrap(),
                count: count_of(&text),
            });
            let children = ref_after(&text, "/First");
            if children.is_some() {
                let last = ref_after(&text, "/Last").expect("/Last with /First");
                self.walk(children, number, depth + 1, pages, out);
                assert!(self.text(last).contains(&format!("/Parent {number} 0 R")));
            }
            prev = Some(number);
            current = ref_after(&text, "/Next");
        }
    }
}

fn ref_after(text: &str, key: &str) -> Option<u32> {
    let at = text.find(&format!("{key} "))? + key.len() + 1;
    let mut tokens = text[at..].split_whitespace();
    let number = tokens.next()?.parse().ok()?;
    (tokens.next()? == "0" && tokens.next()?.starts_with('R')).then_some(number)
}

fn count_of(text: &str) -> Option<i32> {
    let at = text.find("/Count ")? + 7;
    text[at..].split_whitespace().next()?.parse().ok()
}

/// Decode the `/Title` text string: a literal in PDFDocEncoding, or hex UTF-16BE.
fn title_of(text: &str) -> String {
    let at = text.find("/Title ").expect("a title") + 7;
    let rest = &text[at..];
    if let Some(hex) = rest.strip_prefix('<') {
        let hex = &hex[..hex.find('>').unwrap()];
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(&bytes[..2], [0xFE, 0xFF], "UTF-16BE needs its BOM");
        let units: Vec<u16> = bytes[2..]
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16(&units).unwrap();
    }
    // A literal string: parentheses nest, and a backslash escapes the next character.
    let mut out = String::new();
    let mut depth = 0;
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '(' if depth == 0 => depth = 1,
            '(' => {
                depth += 1;
                out.push(c);
            }
            ')' if depth == 1 => break,
            ')' => {
                depth -= 1;
                out.push(c);
            }
            '\\' => out.push(chars.next().unwrap()),
            c => out.push(c),
        }
    }
    out
}

fn outline(html: &str, mode: Mode) -> Option<Vec<Item>> {
    Pdf::parse(&build_pdf(html, mode)).outline()
}

fn titles(items: &[Item]) -> Vec<&str> {
    items.iter().map(|i| i.title.as_str()).collect()
}

fn depths(items: &[Item]) -> Vec<usize> {
    items.iter().map(|i| i.depth).collect()
}

const HEADINGS: &str = "<style>h1 { bookmark-level: 1; } h2 { bookmark-level: 2; } \
                        h3 { bookmark-level: 3; }</style>";

#[test]
fn bookmark_level_builds_a_nested_outline_in_document_order() {
    let html = format!(
        "{HEADINGS}<h1>One</h1><h2>One A</h2><h3>One A i</h3><h2>One B</h2><h1>Two</h1><h2>Two A</h2>"
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(&html, mode).expect("an outline");
        assert_eq!(
            titles(&items),
            ["One", "One A", "One A i", "One B", "Two", "Two A"],
            "{mode:?}"
        );
        assert_eq!(depths(&items), [0, 1, 2, 1, 0, 1], "{mode:?}");
    }
}

#[test]
fn a_positioned_or_floated_heading_keeps_its_place_in_the_outline() {
    // Batch mode lays an absolutely positioned box out after the rest of the page. The
    // outline still follows the document, so 1.4 stays under Chapter 1.
    let html = "<style>h1 { bookmark-level: 1; } h2 { bookmark-level: 2; } \
                .fl { float: right; } .abs { position: absolute; top: 0; right: 0; }</style>\
                <h1>Chapter 1</h1><h2>1.1</h2><h2 class=\"fl\">1.2</h2><h2>1.3</h2>\
                <h2 class=\"abs\">1.4</h2><h2>1.5</h2><h1>Chapter 2</h1>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).expect("an outline");
        assert_eq!(
            titles(&items),
            ["Chapter 1", "1.1", "1.2", "1.3", "1.4", "1.5", "Chapter 2"],
            "{mode:?}"
        );
        assert_eq!(depths(&items), [0, 1, 1, 1, 1, 1, 0], "{mode:?}");
    }
}

#[test]
fn a_skipped_level_nests_under_the_nearest_shallower_entry() {
    let html = format!(
        "{HEADINGS}<h3>Orphan</h3><h1>One</h1><h3>Deep</h3><h2>Back up</h2><h3>Under it</h3>"
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(&html, mode).expect("an outline");
        assert_eq!(
            titles(&items),
            ["Orphan", "One", "Deep", "Back up", "Under it"],
            "{mode:?}"
        );
        assert_eq!(depths(&items), [0, 0, 1, 1, 2], "{mode:?}");
    }
}

#[test]
fn the_default_label_is_the_text_of_the_element_with_whitespace_collapsed() {
    let items = outline(
        "<style>h1 { bookmark-level: 1; }</style><h1>  Hello\n   <em>big</em>   world </h1>",
        Mode::Batch,
    )
    .unwrap();
    assert_eq!(titles(&items), ["Hello big world"]);
}

#[test]
fn bookmark_label_takes_a_string() {
    let html =
        "<style>h1 { bookmark-level: 1; bookmark-label: \"Chapter 1\"; }</style><h1>Intro</h1>";
    for mode in [Mode::Batch, Mode::Streaming] {
        assert_eq!(
            titles(&outline(html, mode).unwrap()),
            ["Chapter 1"],
            "{mode:?}"
        );
    }
}

#[test]
fn bookmark_label_takes_content_and_attr_in_a_list() {
    let html = "<style>h1 { bookmark-level: 1; bookmark-label: \"1. \" content() \" (\" attr(data-tag) \")\"; } \
                h2 { bookmark-level: 2; bookmark-label: content(text); }</style>\
                <h1 data-tag=\"x\">Intro <b>text</b></h1><h2>Plain</h2>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(titles(&items), ["1. Intro text (x)", "Plain"], "{mode:?}");
    }
}

#[test]
fn a_non_ascii_label_is_written_as_utf16() {
    let label = "Zażółć gęślą jaźń - 日本語 - 😀";
    let html = format!("<style>h1 {{ bookmark-level: 1; }}</style><h1>{label}</h1>");
    let bytes = build_pdf(&html, Mode::Batch);
    assert_eq!(titles(&Pdf::parse(&bytes).outline().unwrap()), [label]);
    let bytes = build_pdf(&html, Mode::Streaming);
    assert_eq!(titles(&Pdf::parse(&bytes).outline().unwrap()), [label]);
}

#[test]
fn a_label_with_pdf_delimiters_survives() {
    let html = "<style>h1 { bookmark-level: 1; }</style><h1>a (b) \\ c</h1>";
    assert_eq!(titles(&outline(html, Mode::Batch).unwrap()), ["a (b) \\ c"]);
}

#[test]
fn destinations_follow_page_breaks_and_sit_at_the_top_of_the_element() {
    let html = format!(
        "{HEADINGS}<style>div {{ height: 400px; }} h1 {{ break-before: page; }}</style>\
         <h1>First</h1><div></div><h1>Second</h1><div></div><h2>Third</h2><h1>Fourth</h1>"
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(&html, mode).expect("an outline");
        assert_eq!(
            titles(&items),
            ["First", "Second", "Third", "Fourth"],
            "{mode:?}"
        );
        let pages: Vec<usize> = items.iter().map(|i| i.page).collect();
        assert_eq!(pages[..2], [0, 1], "{mode:?}");
        assert!(pages[2] >= 1 && pages[3] > pages[1], "{mode:?}: {pages:?}");
        // `top` is in pt from the bottom of the page: just below the top margin.
        for item in &items[..2] {
            assert!(item.top > 700.0 && item.top < 842.0, "{mode:?}: {item:?}");
        }
        for item in &items {
            assert!(item.top > 0.0 && item.top < 842.0, "{mode:?}: {item:?}");
        }
        // Third follows Second's 400px block on the same page, so it sits lower.
        if pages[2] == pages[1] {
            assert!(items[2].top < items[1].top, "{mode:?}");
        }
    }
}

#[test]
fn an_element_split_across_pages_is_listed_once_at_its_first_page() {
    let html =
        "<style>div { bookmark-level: 1; bookmark-label: \"Long\"; } p { margin: 0; }</style>\
                <div>"
            .to_string()
            + &"<p>line</p>".repeat(200)
            + "</div>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(&html, mode).unwrap();
        assert_eq!(titles(&items), ["Long"], "{mode:?}");
        assert_eq!(items[0].page, 0, "{mode:?}");
    }
}

#[test]
fn a_document_without_bookmark_level_has_no_outline() {
    let html = "<style>h1 { bookmark-label: \"x\"; bookmark-level: none; }</style><h1>Title</h1><p>text</p>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let bytes = build_pdf(html, mode);
        assert!(find(&bytes, b"/Outlines", 0).is_none(), "{mode:?}");
        assert!(Pdf::parse(&bytes).outline().is_none(), "{mode:?}");
    }
}

#[test]
fn display_none_elements_get_no_entry() {
    let html = format!(
        "{HEADINGS}<h1>Shown</h1><h1 style=\"display: none\">Hidden</h1>\
         <div style=\"display: none\"><h2>Inside hidden</h2></div><h2>Also shown</h2>"
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(&html, mode).unwrap();
        assert_eq!(titles(&items), ["Shown", "Also shown"], "{mode:?}");
    }
}

#[test]
fn text_of_a_display_none_descendant_is_not_in_the_label() {
    let html = "<style>h1 { bookmark-level: 1; }</style><h1>Shown <span style=\"display: none\">secret</span>text</h1>";
    assert_eq!(titles(&outline(html, Mode::Batch).unwrap()), ["Shown text"]);
}

#[test]
fn bookmark_level_must_be_a_positive_integer() {
    let html = "<style>h1 { bookmark-level: 0; } h2 { bookmark-level: 1.5; } h3 { bookmark-level: 2; bookmark-level: x; }</style>\
                <h1>a</h1><h2>b</h2><h3>c</h3>";
    let items = outline(html, Mode::Batch).unwrap();
    assert_eq!(titles(&items), ["c"]);
}

#[test]
fn bookmark_state_closed_gives_a_negative_count() {
    let html = format!(
        "{HEADINGS}<style>h1.c {{ bookmark-state: closed; }}</style>\
         <h1 class=c>Closed</h1><h2>a</h2><h2>b</h2><h1>Open</h1><h2>c</h2><h3>d</h3>"
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(&html, mode).unwrap();
        let counts: Vec<Option<i32>> = items.iter().map(|i| i.count).collect();
        assert_eq!(
            counts,
            [Some(-2), None, None, Some(2), Some(1), None],
            "{mode:?}"
        );
    }
}

#[test]
fn the_outline_root_counts_the_visible_items() {
    let html = format!(
        "{HEADINGS}<style>h2 {{ bookmark-state: closed; }}</style>\
         <h1>a</h1><h2>b</h2><h3>c</h3><h1>d</h1>"
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let bytes = build_pdf(&html, mode);
        let pdf = Pdf::parse(&bytes);
        let root = ref_after(&pdf.catalog(), "/Outlines").unwrap();
        // a, b and d are visible; c is hidden under the closed b.
        assert_eq!(count_of(&pdf.text(root)), Some(3), "{mode:?}");
    }
}

#[test]
fn elements_in_headers_and_footers_get_no_entry() {
    let html = format!("{HEADINGS}<h1>Body</h1>");
    for mode in [Mode::Batch, Mode::Streaming] {
        let bytes = build_pdf_with(&html, mode, |options| {
            let mut header_footer = HeaderFooterHtml::default();
            header_footer.placeholders.page_token = "[page]".to_string();
            header_footer.placeholders.total_pages_token = "[topage]".to_string();
            header_footer.header =
                Some("<div style=\"bookmark-level: 1\">In the header</div>".to_string());
            header_footer.footer =
                Some("<h1 style=\"bookmark-level: 1\">In the footer</h1>".to_string());
            options.header_footer_html = header_footer;
        });
        let items = Pdf::parse(&bytes).outline().unwrap();
        assert_eq!(titles(&items), ["Body"], "{mode:?}");
    }
}

#[test]
fn bookmarks_work_in_a_table_cell_and_a_flex_item() {
    let html = "<style>.b { bookmark-level: 1; }</style>\
                <table><tr><td class=b>Cell</td></tr></table>\
                <div style=\"display: flex\"><div class=b>Flex item</div></div>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(titles(&items), ["Cell", "Flex item"], "{mode:?}");
    }
}

#[test]
fn a_cover_page_shifts_the_destinations_without_adding_entries() {
    let html = format!("{HEADINGS}<h1>Body</h1>");
    let bytes = build_pdf_with(&html, Mode::Batch, |options| {
        options.cover_html = Some("<h1 style=\"bookmark-level: 1\">Cover title</h1>".to_string());
    });
    let items = Pdf::parse(&bytes).outline().unwrap();
    assert_eq!(titles(&items), ["Body"]);
    assert_eq!(items[0].page, 1);
}

#[test]
fn line_breaks_and_block_children_separate_the_words_of_a_label() {
    let html = "<style>h1, h2 { bookmark-level: 1; }</style>\
                <h1>Chapter 1<br>Introduction</h1>\
                <h2><div>First</div><p>Second</p>third</h2>\
                <h1><b>Foo</b>bar <i>baz</i></h1>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(
            titles(&items),
            ["Chapter 1 Introduction", "First Second third", "Foobar baz"],
            "{mode:?}"
        );
    }
}

#[test]
fn a_label_keeps_no_break_spaces() {
    let html = "<style>h1 { bookmark-level: 1; }</style><h1> a&nbsp;b \t\n c&nbsp;</h1>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(titles(&items), ["a\u{a0}b c\u{a0}"], "{mode:?}");
    }
}

#[test]
fn table_rows_and_row_groups_get_entries_on_their_own_pages() {
    let html = "<style>td { height: 300px; padding: 0; } tbody { bookmark-level: 1; \
                bookmark-label: \"Body\"; } tr { bookmark-level: 2; bookmark-label: attr(id); }\
                </style><table><tbody>\
                <tr id=r1><td>a</td></tr><tr id=r2><td>b</td></tr>\
                <tr id=r3><td>c</td></tr><tr id=r4><td>d</td></tr></tbody></table>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(titles(&items), ["Body", "r1", "r2", "r3", "r4"], "{mode:?}");
        assert_eq!(depths(&items), [0, 1, 1, 1, 1], "{mode:?}");
        let pages: Vec<usize> = items.iter().map(|i| i.page).collect();
        assert!(
            pages.windows(2).all(|w| w[0] <= w[1]),
            "{mode:?}: {pages:?}"
        );
        assert!(pages[4] > pages[1], "{mode:?}: {pages:?}");
        for item in &items {
            assert!(item.top > 0.0 && item.top < 842.0, "{mode:?}: {item:?}");
        }
    }
}

#[test]
fn floats_positioned_and_inline_blocks_get_entries_once() {
    let html = "<style>.b { bookmark-level: 1; } .f { float: left; width: 50px; } \
                .a { position: absolute; top: 10px; left: 10px; } \
                .x { position: fixed; top: 0; left: 0; } \
                .i { display: inline-block; }</style>\
                <div class='b f'>float</div><div class='b a'>abs</div>\
                <div class='b x'>fixed</div><span class='b i'>inline-block</span>\
                <div style='display: grid'><div class=b>grid item</div></div>\
                <div style='height: 1500px'></div>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let mut got = titles(&outline(html, mode).unwrap())
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        got.sort();
        let mut want = vec!["float", "abs", "inline-block", "grid item", "fixed"];
        want.sort();
        // Streaming does not lay out fixed or absolute boxes.
        if mode == Mode::Streaming {
            want.retain(|t| *t != "fixed" && *t != "abs");
            got.retain(|t| t != "fixed" && t != "abs");
        }
        assert_eq!(got, want, "{mode:?}");
    }
}

#[test]
fn an_element_with_an_empty_label_gets_no_entry() {
    let html = "<style>h1, h2 { bookmark-level: 1; }</style>\
                <h1> </h1><h2 style=\"bookmark-label: ''\">text</h2><h1>Kept</h1>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(titles(&items), ["Kept"], "{mode:?}");
    }
}

#[test]
fn bookmark_level_on_html_and_body_is_ignored() {
    let html = "<style>html, body { bookmark-level: 1; } h1 { bookmark-level: 2; }</style>\
                <h1>Heading</h1>";
    for mode in [Mode::Batch, Mode::Streaming] {
        let items = outline(html, mode).unwrap();
        assert_eq!(titles(&items), ["Heading"], "{mode:?}");
        assert_eq!(depths(&items), [0], "{mode:?}");
    }
}
