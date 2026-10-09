//! E2E tests for CSS Custom Properties (`--foo`/`var()`).
//!
//! The same approach as `box_sizing.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode).

use std::path::PathBuf;

use sghtmltopdf::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf::img::{DocumentImageCache, ImageFetcher};
use sghtmltopdf::layout::{build_box_tree, layout_document, paginate_document, PageSettings};
use sghtmltopdf::pdf::encode_pdf;
use sghtmltopdf::sink::MemorySink;
use sghtmltopdf::style::{
    compute_styles, extract_author_stylesheet, user_agent_stylesheet, BorderStyle, ComputedStyle,
};

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

fn no_remote_fetcher() -> ImageFetcher {
    ImageFetcher::new(PathBuf::from("."), false)
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

fn find_laid_out(
    b: &sghtmltopdf::layout::LaidOutBox,
    target: NodeId,
) -> Option<&sghtmltopdf::layout::LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    if let sghtmltopdf::layout::LaidOutContent::Blocks(children) = &b.content {
        for child in children {
            if let Some(found) = find_laid_out(child, target) {
                return Some(found);
            }
        }
    }
    None
}

/// Build a single DOM with the css embedded in a `<style>` tag. The helpers in this test file
/// always go through `extract_author_stylesheet` (the path the engine takes).
fn dom_with_style(html_body: &str, css: &str) -> Dom {
    html::parse(
        format!("<html><head><style>{css}</style></head><body>{html_body}</body></html>")
            .as_bytes(),
    )
}

fn extract_stylesheet(dom: &Dom) -> sghtmltopdf::style::Stylesheet {
    let fetcher = no_remote_fetcher();
    let cache = DocumentImageCache::new();
    extract_author_stylesheet(dom, &fetcher, &cache)
}

fn layout(html_body: &str, css: &str) -> (Dom, sghtmltopdf::layout::LaidOutBox) {
    let dom = dom_with_style(html_body, css);
    let author = extract_stylesheet(&dom);
    let ua = user_agent_stylesheet();
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

fn build_pdf(html_body: &str, css: &str) -> Vec<u8> {
    let dom = dom_with_style(html_body, css);
    let author = extract_stylesheet(&dom);
    let ua = user_agent_stylesheet();
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(
        &pages,
        &styles,
        &std::collections::HashMap::new(),
        &fonts,
        &settings,
    );

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn var_resolves_inside_a_layout_property() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --box-width: 120px; } \
         body { margin: 0; } \
         .box { width: var(--box-width); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 120.0);
}

#[test]
fn a_custom_property_referencing_another_one_resolves_transitively() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --base: 40px; --double: var(--base); } \
         body { margin: 0; } \
         .box { width: var(--double); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 40.0);
}

#[test]
fn fallback_value_is_used_when_the_custom_property_is_undefined() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        "body { margin: 0; } \
         .box { width: var(--undefined, 90px); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 90.0);
}

#[test]
fn an_undefined_var_without_a_fallback_leaves_the_declaration_ignored() {
    // An undefined `var` with no fallback makes the declaration invalid at computed-value
    // time, so `width` behaves as unset (auto).
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        "body { margin: 0; } \
         .box { width: var(--undefined); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    // Being auto, the content width stretches to the parent's available width (it does not collapse to 0px).
    assert!(div.layout.content.width > 90.0);
}

#[test]
fn later_rule_of_equal_specificity_wins_for_the_same_element() {
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --w: 50px; } \
         .box { --w: 75px; } \
         body { margin: 0; } \
         .box { width: var(--w); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 75.0);
}

#[test]
fn custom_properties_declared_in_one_style_tag_resolve_in_another() {
    // extract_author_stylesheet concatenates several <style> tags, so a `--foo` declared in
    // one tag is reachable from a `var(--foo)` in another.
    let dom = html::parse(
        br#"<html><head>
            <style>:root { --brand-width: 64px; }</style>
            <style>body { margin: 0; } .box { width: var(--brand-width); }</style>
            </head><body><div class="box">content</div></body></html>"#,
    );
    let author = extract_stylesheet(&dom);
    let ua = user_agent_stylesheet();
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );

    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.width, 64.0);
}

#[test]
fn extract_author_stylesheet_keeps_custom_properties_for_style_computation() {
    let dom = dom_with_style(
        "<div class=\"box\">content</div>",
        ":root { --w: 33px; } .box { width: var(--w); }",
    );
    let sheet = extract_stylesheet(&dom);
    // Custom properties are resolved per element during style computation, not while
    // parsing, so both rules survive in the stylesheet.
    assert_eq!(sheet.rules.len(), 2);
}

#[test]
fn var_inside_nested_calc_resolves_the_tailwind_space_y_shape() {
    // issue #17: Tailwind v4's `space-y-*`/`divide-*` emit
    // `calc(calc(var(--spacing) * N) * calc(1 - var(--tw-space-y-reverse)))`
    // 15px * 6 * (1 - 0) = 90px.
    let (dom, laid) = layout(
        r#"<div class="box">content</div>"#,
        ":root { --spacing: 15px; --reverse: 0; } \
         body { margin: 0; } \
         .box { margin-left: calc(calc(var(--spacing) * 6) * calc(1 - var(--reverse))); }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let b = find_laid_out(&laid, divs[0]).unwrap();
    assert!(
        (b.layout.margin.left - 90.0).abs() < 0.5,
        "margin-left should be 90 but was {}",
        b.layout.margin.left
    );
}

#[test]
fn custom_properties_render_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">custom properties test</div>"#,
        ":root { --gap: 15px; --color: rgb(10, 20, 30); } \
         body { margin: 0; } \
         .box { padding: var(--gap); background-color: var(--color); }",
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

// ---- Cascade and inheritance (issue #75) ----

/// Compute the styles for `body` and return the style of the `<div>` carrying `class`.
fn style_of(body: &str, css: &str, class: &str) -> ComputedStyle {
    let dom = dom_with_style(body, css);
    let author = extract_stylesheet(&dom);
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let id = divs
        .into_iter()
        .find(|id| match &dom.node(*id).data {
            NodeData::Element { attrs, .. } => attrs
                .iter()
                .any(|a| &*a.name.local == "class" && a.value.split(' ').any(|c| c == class)),
            _ => false,
        })
        .expect("element with the class");
    (*styles[&id]).clone()
}

fn margin_left_of(body: &str, css: &str, class: &str) -> f32 {
    use sghtmltopdf::style::{LengthPercentage, LengthPercentageOrAuto as L};
    match style_of(body, css, class).margin_left {
        L::LengthPercentage(LengthPercentage::Length(px)) => px,
        L::LengthPercentage(LengthPercentage::Calc { px, percent: 0.0 }) => px,
        other => panic!("unexpected margin-left {other:?}"),
    }
}

#[test]
fn a_rule_for_a_sibling_does_not_leak_into_another_element() {
    // `.later` is a sibling of `.probe`, so its `--shift` must not reach `.probe`.
    let css =
        ":root { --shift: 90px } .later { --shift: 0px } .probe { margin-left: var(--shift) }";
    let body = r#"<div class="probe">X</div><div class="later">Y</div>"#;
    assert_eq!(margin_left_of(body, css, "probe"), 90.0);
}

#[test]
fn a_custom_property_is_inherited_from_the_ancestor_only() {
    let css =
        ".outer { --shift: 90px } .other { --shift: 0px } .probe { margin-left: var(--shift) }";
    let body = r#"<div class="outer"><div class="probe">X</div></div><div class="other">Y</div>"#;
    assert_eq!(margin_left_of(body, css, "probe"), 90.0);
}

#[test]
fn a_custom_property_declared_on_the_same_element_is_not_overridden_by_other_rules() {
    let css = ".probe { --shift: 90px; margin-left: var(--shift) } .other { --shift: 0px }";
    let body = r#"<div class="probe">X</div><div class="other">Y</div>"#;
    assert_eq!(margin_left_of(body, css, "probe"), 90.0);
}

#[test]
fn a_descendant_redefinition_applies_to_its_own_subtree() {
    let css = ".a { --x: 10px } .b { --x: 20px } .c { margin-left: var(--x) }";
    let body = r#"<div class="a"><div class="c first"></div><div class="b"><div class="c second"></div></div></div>"#;
    assert_eq!(margin_left_of(body, css, "first"), 10.0);
    assert_eq!(margin_left_of(body, css, "second"), 20.0);
}

#[test]
fn specificity_and_order_decide_between_custom_property_declarations() {
    let body = r#"<div id="i" class="c">X</div>"#;
    let css = "#i { --x: 3px } .c { --x: 1px } div { --x: 2px } .c { margin-left: var(--x) }";
    assert_eq!(margin_left_of(body, css, "c"), 3.0);
    let css = ".c { --x: 1px } .c { --x: 2px } .c { margin-left: var(--x) }";
    assert_eq!(margin_left_of(body, css, "c"), 2.0);
}

#[test]
fn inline_style_overrides_the_stylesheet_for_custom_properties() {
    let body = r#"<div class="c" style="--x: 7px">X</div>"#;
    let css = ".c { --x: 1px; margin-left: var(--x) }";
    assert_eq!(margin_left_of(body, css, "c"), 7.0);
}

#[test]
fn important_custom_property_beats_inline_style_and_later_rules() {
    let body = r#"<div class="c" style="--x: 7px">X</div>"#;
    let css = ".c { --x: 1px !important; margin-left: var(--x) } .c { --x: 2px }";
    assert_eq!(margin_left_of(body, css, "c"), 1.0);
}

#[test]
fn var_inside_the_inline_style_attribute_is_resolved() {
    let body = r#"<div class="p"><div class="c" style="margin-left: var(--x)">X</div></div>"#;
    let css = ".p { --x: 12px }";
    assert_eq!(margin_left_of(body, css, "c"), 12.0);
}

#[test]
fn a_custom_property_can_reference_one_declared_on_an_ancestor() {
    let body = r#"<div class="p"><div class="c">X</div></div>"#;
    let css =
        ".p { --base: 5px } .c { --double: calc(var(--base) * 2); margin-left: var(--double) }";
    assert_eq!(margin_left_of(body, css, "c"), 10.0);
}

#[test]
fn a_reference_cycle_makes_the_properties_invalid_so_the_fallback_is_used() {
    let body = r#"<div class="c">X</div>"#;
    let css = ".c { --a: var(--b); --b: var(--a); margin-left: var(--a, 50px) }";
    assert_eq!(margin_left_of(body, css, "c"), 50.0);
}

#[test]
fn em_in_a_custom_property_resolves_against_the_element_that_uses_it() {
    let body = r#"<div class="p"><div class="c">X</div></div>"#;
    let css = ".p { --gap: 2em; font-size: 10px } .c { font-size: 20px; margin-left: var(--gap) }";
    assert_eq!(margin_left_of(body, css, "c"), 40.0);
}

#[test]
fn an_unresolvable_var_resets_a_non_inherited_property_to_its_initial_value() {
    let body = r#"<div class="c">X</div>"#;
    let css = ".c { margin-left: 30px } .c { margin-left: var(--nope) }";
    assert_eq!(margin_left_of(body, css, "c"), 0.0);
}

#[test]
fn an_unresolvable_var_makes_an_inherited_property_inherit() {
    let body = r#"<div class="p"><div class="c">X</div></div>"#;
    let css = ".p { color: rgb(1, 2, 3) } .c { color: rgb(9, 9, 9) } .c { color: var(--nope) }";
    let style = style_of(body, css, "c");
    assert_eq!(
        (style.color.red, style.color.green, style.color.blue),
        (1, 2, 3)
    );
}

#[test]
fn an_unresolvable_shorthand_resets_all_of_its_longhands() {
    use sghtmltopdf::style::LengthPercentage;
    let body = r#"<div class="c">X</div>"#;
    let css = ".c { padding: 4px } .c { padding: 1px var(--nope) }";
    let style = style_of(body, css, "c");
    assert_eq!(style.padding_top, LengthPercentage::Length(0.0));
    assert_eq!(style.padding_left, LengthPercentage::Length(0.0));
}

#[test]
fn initial_and_inherit_work_on_custom_properties() {
    let body = r#"<div class="p"><div class="c">X</div></div>"#;
    let css = ".p { --x: 8px } .c { --x: 99px; --x: inherit; margin-left: var(--x, 1px) }";
    assert_eq!(margin_left_of(body, css, "c"), 8.0);
    let css = ".p { --x: 8px } .c { --x: initial; margin-left: var(--x, 1px) }";
    assert_eq!(margin_left_of(body, css, "c"), 1.0);
}

const TAILWIND_BORDER_CSS: &str = "\
    .border { border-style: var(--tw-border-style); border-width: 4px; width: 200px; height: 40px } \
    :root { --tw-border-style: solid } \
    .border-none { --tw-border-style: none } \
    .border-dashed { --tw-border-style: dashed }";

#[test]
fn tailwind_border_utilities_each_get_their_own_style() {
    let body = r#"<div class="border plain">a</div>
        <div class="border border-dashed dashed">b</div>
        <div class="border border-none none">c</div>"#;
    assert_eq!(
        style_of(body, TAILWIND_BORDER_CSS, "plain").border_top_style,
        BorderStyle::Solid
    );
    assert_eq!(
        style_of(body, TAILWIND_BORDER_CSS, "dashed").border_top_style,
        BorderStyle::Dashed
    );
    assert_eq!(
        style_of(body, TAILWIND_BORDER_CSS, "none").border_top_style,
        BorderStyle::None
    );
}

#[test]
fn before_content_can_use_a_custom_property() {
    let body = r#"<div class="c">X</div>"#;
    let css = r#".c { --label: "hi" } .c::before { content: var(--label) }"#;
    assert_eq!(
        style_of(body, css, "c").pseudo_before_content.as_deref(),
        Some("hi")
    );
}

fn render_with_engine(css: &str, body_parts: &[&str], mode: Mode) -> Vec<u8> {
    let mut options = EngineOptions::default();
    options.mode = mode;
    options.fonts = vec![FontSpec {
        path: PathBuf::from(FONT_PATH),
        index: 0,
    }];
    // Left uncompressed so the fill colour operators can be counted.
    options.output.compress = false;
    let mut engine = Engine::new(options, MemorySink::new());
    let head = format!("<html><head><style>{css}</style></head><body>");
    engine.feed(head.as_bytes()).unwrap();
    for part in body_parts {
        engine.feed(part.as_bytes()).unwrap();
    }
    engine.feed(b"</body></html>").unwrap();
    engine.finish().unwrap()
}

#[test]
fn page_size_can_use_root_custom_properties_in_both_modes() {
    let css = ":root { --w: 300px; --h: 400px } @page { size: var(--w) var(--h); margin: 0 }";
    let expected = format!(
        "/MediaBox [0 0 {} {}]",
        300.0 * sghtmltopdf::pdf::DEFAULT_SCALE,
        400.0 * sghtmltopdf::pdf::DEFAULT_SCALE
    );
    for mode in [Mode::Batch, Mode::Streaming] {
        let bytes = render_with_engine(css, &["<p>x</p>"], mode);
        assert_eq!(
            count_occurrences(&bytes, expected.as_bytes()),
            1,
            "mode {mode:?}"
        );
    }
}

#[test]
fn page_margin_boxes_see_root_custom_properties_only() {
    // The margin-box text is drawn in the colour taken from `:root`'s `--c`.
    let css = ":root { --c: #cc0000 } @page { @top-center { content: \"hi\"; color: var(--c) } }";
    let bytes = render_with_engine(css, &["<p>x</p>"], Mode::Batch);
    assert!(count_occurrences(&bytes, b"0.8 0 0 rg") > 0);
    // A property declared on some other element is not visible to `@page`.
    let css = ".x { --c: #cc0000 } @page { @top-center { content: \"hi\"; color: var(--c) } }";
    let bytes = render_with_engine(css, &["<p>x</p>"], Mode::Batch);
    assert_eq!(count_occurrences(&bytes, b"0.8 0 0 rg"), 0);
}

#[test]
fn streaming_mode_resolves_custom_properties_like_batch() {
    // `--c` comes from `:root`, is overridden on one top-level element and inherited by its
    // child, and is read through `color: var(--c)`.
    let css = ":root { --c: #cc0000 } .blue { --c: #0000cc } .use { color: var(--c) }";
    let parts = [
        r#"<div class="use">a</div>"#,
        r#"<div class="blue"><p class="use">b</p></div>"#,
        r#"<div class="use">c</div>"#,
    ];
    for mode in [Mode::Batch, Mode::Streaming] {
        let bytes = render_with_engine(css, &parts, mode);
        assert_eq!(
            count_occurrences(&bytes, b"0.8 0 0 rg"),
            2,
            "red in {mode:?}"
        );
        assert_eq!(
            count_occurrences(&bytes, b"0 0 0.8 rg"),
            1,
            "blue in {mode:?}"
        );
    }
}

#[test]
fn custom_properties_on_body_reach_streamed_top_level_elements() {
    let css = "body { --c: #cc0000 } .use { color: var(--c) }";
    let parts = [r#"<div class="use">a</div>"#, r#"<div class="use">b</div>"#];
    for mode in [Mode::Batch, Mode::Streaming] {
        let bytes = render_with_engine(css, &parts, mode);
        assert_eq!(
            count_occurrences(&bytes, b"0.8 0 0 rg"),
            2,
            "red in {mode:?}"
        );
    }
}

// ---- `:host` in Tailwind v4's theme (issue #102) ----

#[test]
fn a_theme_declared_on_root_and_host_applies() {
    // Tailwind v4 declares every theme variable on `:root, :host`, inside `@layer theme`.
    let body = r#"<div class="probe">X</div>"#;
    let probe = ".probe { margin-left: var(--shift, 0px) }";
    for theme in [
        ":root { --shift: 120px }",
        ":root, :host { --shift: 120px }",
        "@layer theme { :root, :host { --shift: 120px } }",
    ] {
        assert_eq!(
            margin_left_of(body, &format!("{theme} {probe}"), "probe"),
            120.0,
            "{theme}"
        );
    }
}

#[test]
fn a_preflight_rule_on_html_and_host_applies() {
    // Tailwind v4's preflight puts `html, :host { ... }`.
    let (dom, laid) = layout(
        r#"<div class="probe">X</div>"#,
        "body { margin: 0 } html, :host { padding-left: 120px }",
    );
    let mut divs = Vec::new();
    find_all_tags(&dom, dom.document(), "div", &mut divs);
    let div = find_laid_out(&laid, divs[0]).unwrap();
    assert_eq!(div.layout.content.x, 120.0);
}
