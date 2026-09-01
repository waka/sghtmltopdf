//! The UA default stylesheet.
//!
//! Ported from the "Rendering" section of the WHATWG HTML spec, keeping only the
//! declarations that mean something for print/PDF output. Interaction states (focus,
//! hover), bidi and anything scroll-related are not ported.
//!
//! `thead`/`tbody`/`tfoot` stay `display: block` and have no boxes of their own.
//! Table row collection ([`crate::layout::box_tree`]) passes through them transparently to
//! find the `table-row` descendants, so in effect they are "transparent containers between
//! the table and its rows". `caption` has its own `display: table-caption` value, which
//! `box_tree.rs` detects specially alongside `table-row`.
//!
//! The thinking behind which elements get `display: none`: the initial `display` in
//! `ComputedStyle` is `Inline`, so an element with no UA rule is treated as inline and its
//! descendant text flows into the body. Embedded content we cannot draw (`svg`/`canvas`/
//! `video` and so on) and form controls are set to `display: none` explicitly so that their
//! alternative content and option text do not leak into the body. Form controls are instead
//! given a static `display: inline-block` appearance.

use super::stylesheet::{parse_stylesheet, Stylesheet};

const UA_CSS: &str = r#"
/* ===== Block-level elements ===== */

html, body, div, p,
h1, h2, h3, h4, h5, h6,
ul, ol, menu, dl, dt, dd,
thead, tbody, tfoot,
header, footer, section, article, aside, nav, main, hgroup, search,
blockquote, figure, figcaption, pre, hr, address,
form, fieldset, legend, details, summary, dialog, center {
  display: block;
}

table {
  display: table;
}

tr {
  display: table-row;
}

td, th {
  display: table-cell;
}

caption {
  display: table-caption;
}

li {
  display: list-item;
}

/* ===== Inline elements ===== */

span, a, b, strong, i, em, small, big, code, kbd, samp, var, cite, dfn,
label, abbr, q, sub, sup, u, s, strike, ins, del, mark, tt, font,
bdi, bdo, ruby, rt, rp, time, data, output, wbr, picture {
  display: inline;
}

/* ===== Elements that are hidden ===== */

/* Document metadata. The contents of `template` are moved to a separate tree at parse time
   (by `html5ever`'s `TreeSink`), but this states it explicitly for safety. */
head, script, style, title, meta, link, base, noscript, template {
  display: none;
}

/* Embedded content we cannot draw. Matched by element name only; namespaces are not
   considered. `layout::box_tree::child_kind` stops recursing at a `display: none` element,
   so removing one root removes the whole subtree (`<svg><text>` and the like).
   `picture` is excluded, because we do want to draw the `<img>` inside it.
   SVG can be drawn from `<img src="*.svg">` and `background-image: url(*.svg)`
   (`pdf::svg`), but an inline `<svg>` written directly in the HTML is removed here.
   Inline SVG would require joining the HTML DOM to the SVG DOM, which is a different job
   from an external reference. */
svg, math, canvas, video, audio, iframe, embed, object, param, track, source,
area, map {
  display: none;
}

/* Form controls whose value visualisation means nothing for business documents, and the
   options themselves (the `<select>` generates the visible text), stay hidden. */
option, optgroup, datalist, progress, meter {
  display: none;
}

input[type="hidden"] {
  display: none;
}

/* The `hidden` attribute. Origin priority (UA < Author) means an author
   `[hidden] { display: block }` always wins. */
[hidden] {
  display: none;
}

/* A `<dialog>` is hidden without the `open` attribute. */
dialog:not([open]) {
  display: none;
}

/* A `<details>` without the `open` attribute hides every child but `summary`.
   A bare text node directly inside it cannot be selected, so it cannot be hidden. */
details:not([open]) > *:not(summary) {
  display: none;
}

/* ===== Text decoration ===== */

b, strong, th,
h1, h2, h3, h4, h5, h6 {
  font-weight: bold;
}

i, em, cite, dfn, var, address {
  font-style: italic;
}

u, ins {
  text-decoration: underline;
}

s, strike, del {
  text-decoration: line-through;
}

/* `:link` matches an `<a>` with an `href` (`style::element_ref`). */
a:link {
  color: #0000ee;
  text-decoration: underline;
}

mark {
  background-color: #ffff00;
  color: #000000;
}

/* ===== Font sizes ===== */

/* `em` in `font-size` resolves against the parent's font size, so the UA rules can be
   written relatively (keyword values such as `smaller`/`larger` are not supported). */

h1 { font-size: 2em; }
h2 { font-size: 1.5em; }
h3 { font-size: 1.17em; }
h4 { font-size: 1em; }
h5 { font-size: 0.83em; }
h6 { font-size: 0.67em; }

small, sub, sup { font-size: 0.83em; }
big { font-size: 1.17em; }

/* The vertical shift is done with `vertical-align`.
   That it is independent of the shrinking (the `font-size` above) follows the CSS spec. */
sub { vertical-align: sub; }
sup { vertical-align: super; }

/* Monospace font. The generic family name `monospace` is resolved to a concrete font by
   `fonts::system` from its own candidate list. */
pre, code, kbd, samp, tt {
  font-family: monospace;
}

/* ===== Margins and padding ===== */

body {
  margin: 8px;
}

h1 { margin: 0.67em 0; }
h2 { margin: 0.83em 0; }
h3 { margin: 1em 0; }
h4 { margin: 1.33em 0; }
h5 { margin: 1.67em 0; }
h6 { margin: 2.33em 0; }

p, ul, ol, menu, dl, pre {
  margin: 16px 0;
}

blockquote, figure {
  margin: 16px 40px;
}

dd {
  margin-left: 40px;
}

ul, ol, menu {
  padding-left: 40px;
}

fieldset {
  margin: 0 2px;
  padding: 0.35em 0.75em 0.625em;
  border: 2px groove #c0c0c0;
}

legend {
  padding: 0 2px;
}

/* ===== Per-element rules ===== */

ul, menu {
  list-style-type: disc;
}

ol {
  list-style-type: decimal;
}

pre {
  white-space: pre;
}

hr {
  margin: 8px auto;
  border-top: 1px inset #808080;
}

center {
  text-align: center;
}

caption {
  text-align: center;
}

th {
  text-align: center;
}

/* ===== Static rendering of form controls ===== */

/* Placed in the line as a bordered box. The text inside (`value`/`placeholder`/the selected
   `<option>`) is generated during box tree construction.
   The size is decided here rather than from attributes. */
input, select, textarea, button {
  display: inline-block;
  border: 1px solid #767676;
  padding: 1px 2px;
  background-color: #ffffff;
  color: #000000;
  text-align: left;
  white-space: pre;
}

input, select {
  width: 12em;
  height: 1.6em;
  /* Keep the content's line height (which can grow, as with CJK) from spilling out of the
     box when it exceeds the box height. The same behaviour as browser form
     controls. */
  overflow: hidden;
}

textarea {
  width: 20em;
  height: 4em;
  overflow: hidden;
  font-family: monospace;
}

button, input[type="submit"], input[type="reset"], input[type="button"] {
  width: auto;
  padding: 2px 8px;
  background-color: #efefef;
  text-align: center;
}

/* Checkboxes and radios are small boxes. Filled in when `checked`. */
input[type="checkbox"], input[type="radio"] {
  width: 11px;
  height: 11px;
  padding: 0;
}

/* Percentages in `border-radius` are not supported, so a circle is made with a px value of
   half the box. */
input[type="radio"] {
  border-radius: 6px;
}

input[type="checkbox"][checked], input[type="radio"][checked] {
  background-color: #333333;
}

/* `disabled` is shown in light grey. */
input[disabled], select[disabled], textarea[disabled], button[disabled] {
  background-color: #ebebeb;
  color: #6d6d6d;
}

fieldset {
  min-width: 0;
}

/* Automatic quotation marks for `<q>`. The initial value of `quotes` is implemented, so
   this alone produces depth-appropriate quotes when nested. */
q::before {
  content: open-quote;
}

q::after {
  content: close-quote;
}
"#;

pub fn user_agent_stylesheet() -> Stylesheet {
    parse_stylesheet(UA_CSS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html::{self, Dom, NodeData, NodeId};
    use crate::style::values::{Display, FontStyle, FontWeight, TextAlign};
    use crate::style::{compute_styles, parse_stylesheet, ComputedStyle};

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    /// Compute `html_src` with the UA stylesheet alone and return `tag`'s computed style.
    fn style_of(html_src: &str, tag: &str) -> ComputedStyle {
        let dom = html::parse(html_src.as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
        let node = find(&dom, dom.document(), tag).expect("element not found");
        (*styles[&node]).clone()
    }

    #[test]
    fn html5_sectioning_elements_are_block_level() {
        for tag in [
            "article",
            "section",
            "header",
            "footer",
            "aside",
            "nav",
            "main",
            "hgroup",
            "figure",
            "figcaption",
            "details",
            "summary",
            "dialog",
        ] {
            let html_src = format!("<{tag}>x</{tag}>");
            // `dialog` is hidden without `open`, so that is checked in a separate test.
            let expected = if tag == "dialog" {
                Display::None
            } else {
                Display::Block
            };
            assert_eq!(
                style_of(&html_src, tag).display,
                expected,
                "unexpected display for <{tag}>"
            );
        }
    }

    #[test]
    fn phrasing_elements_stay_inline() {
        for tag in [
            "cite", "dfn", "var", "abbr", "time", "data", "output", "bdi", "bdo", "ruby", "rt",
            "rp", "mark", "big", "tt",
        ] {
            let html_src = format!("<p><{tag}>x</{tag}></p>");
            assert_eq!(
                style_of(&html_src, tag).display,
                Display::Inline,
                "unexpected display for <{tag}>"
            );
        }
    }

    #[test]
    fn undisplayable_embedded_content_is_hidden() {
        for tag in [
            "svg", "math", "canvas", "video", "audio", "iframe", "embed", "object",
        ] {
            let html_src = format!("<{tag}>x</{tag}>");
            assert_eq!(
                style_of(&html_src, tag).display,
                Display::None,
                "<{tag}> should be hidden"
            );
        }
    }

    #[test]
    fn form_controls_are_inline_blocks_with_a_border() {
        for tag in ["input", "select", "textarea", "button"] {
            let html_src = format!("<form><{tag}>x</{tag}></form>");
            let style = style_of(&html_src, tag);
            assert_eq!(
                style.display,
                Display::InlineBlock,
                "<{tag}> should be drawn as a static box"
            );
            assert_eq!(
                style.border_top_width.0, 1.0,
                "<{tag}> should have a border"
            );
        }
        // Those whose value visualisation means nothing, and the options themselves, stay hidden.
        for tag in ["progress", "meter", "datalist"] {
            let html_src = format!("<form><{tag}>x</{tag}></form>");
            assert_eq!(style_of(&html_src, tag).display, Display::None);
        }
        assert_eq!(
            style_of("<select><option>a</option></select>", "option").display,
            Display::None
        );
        assert_eq!(
            style_of(r#"<input type="hidden" value="x">"#, "input").display,
            Display::None
        );
        // The form itself, label and fieldset are not hidden: their contents read as prose.
        assert_eq!(style_of("<form>x</form>", "form").display, Display::Block);
        assert_eq!(
            style_of("<p><label>x</label></p>", "label").display,
            Display::Inline
        );
    }

    #[test]
    fn a_checked_checkbox_is_filled() {
        let unchecked = style_of(r#"<input type="checkbox">"#, "input");
        let checked = style_of(r#"<input type="checkbox" checked>"#, "input");
        assert_ne!(
            checked.background_color, unchecked.background_color,
            "a checked box must be visually distinct"
        );
    }

    #[test]
    fn a_disabled_control_is_greyed_out() {
        let normal = style_of("<input>", "input");
        let disabled = style_of("<input disabled>", "input");
        assert_ne!(disabled.background_color, normal.background_color);
        assert_ne!(disabled.color, normal.color);
    }

    #[test]
    fn headings_are_bold_and_shrink_with_the_level() {
        let sizes: Vec<f32> = ["h1", "h2", "h3", "h4", "h5", "h6"]
            .iter()
            .map(|tag| {
                let html_src = format!("<{tag}>x</{tag}>");
                let style = style_of(&html_src, tag);
                assert_eq!(
                    style.font_weight,
                    FontWeight::Bold,
                    "<{tag}> should be bold"
                );
                style.font_size.0
            })
            .collect();
        for pair in sizes.windows(2) {
            assert!(pair[0] > pair[1], "font sizes should decrease: {sizes:?}");
        }
        // Against the default 16px: h1 = 2em = 32px, h4 = 1em = 16px.
        assert_eq!(sizes[0], 32.0);
        assert_eq!(sizes[3], 16.0);
    }

    #[test]
    fn relative_font_sizes_resolve_against_the_parent() {
        // `<small>` is 0.83em. With a 20px parent (p) that is 16.6px (not relative to the root).
        let dom = html::parse(b"<p><small>x</small></p>");
        let styles = compute_styles(
            &dom,
            &user_agent_stylesheet(),
            &parse_stylesheet("p { font-size: 20px; }"),
        );
        let small = find(&dom, dom.document(), "small").expect("small not found");
        assert!((styles[&small].font_size.0 - 16.6).abs() < 0.01);
    }

    #[test]
    fn preformatted_and_code_use_the_monospace_generic_family() {
        for tag in ["pre", "code", "kbd", "samp", "tt"] {
            let html_src = format!("<{tag}>x</{tag}>");
            assert_eq!(
                style_of(&html_src, tag).font_family,
                vec!["monospace".to_string()],
                "<{tag}> should request the monospace generic family"
            );
        }
    }

    #[test]
    fn emphasis_elements_are_italic() {
        for tag in ["i", "em", "cite", "dfn", "var"] {
            let html_src = format!("<p><{tag}>x</{tag}></p>");
            assert_eq!(style_of(&html_src, tag).font_style, FontStyle::Italic);
        }
        assert_eq!(
            style_of("<address>x</address>", "address").font_style,
            FontStyle::Italic
        );
    }

    #[test]
    fn table_header_cells_are_bold_and_centered() {
        let style = style_of("<table><tr><th>x</th></tr></table>", "th");
        assert_eq!(style.font_weight, FontWeight::Bold);
        assert_eq!(style.text_align, TextAlign::Center);
    }

    #[test]
    fn hr_has_a_top_border_so_it_draws_a_line() {
        let style = style_of("<hr>", "hr");
        assert_eq!(style.display, Display::Block);
        assert_eq!(style.border_top_width.0, 1.0);
        assert_ne!(
            style.border_top_style,
            super::super::values::BorderStyle::None
        );
    }

    #[test]
    fn a_link_gets_the_default_link_decoration() {
        // `:link` matching is decided statically by `style::element_ref` from the presence of `href`.
        let with_href = style_of(r#"<p><a href="x">link</a></p>"#, "a");
        assert_eq!(with_href.color.blue, 0xee);
        assert!(with_href.text_decoration_line.underline);

        let without_href = style_of("<p><a>anchor</a></p>", "a");
        assert_eq!(
            without_href.color.blue, 0,
            "an <a> without href is not a link and keeps the inherited color"
        );
    }

    #[test]
    fn a_closed_details_hides_everything_but_its_summary() {
        let dom = html::parse(b"<details><summary>s</summary><p>body</p></details>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
        let summary = find(&dom, dom.document(), "summary").expect("summary not found");
        let body = find(&dom, dom.document(), "p").expect("p not found");
        assert_eq!(styles[&summary].display, Display::Block);
        assert_eq!(styles[&body].display, Display::None);
    }

    #[test]
    fn an_open_details_shows_everything() {
        let dom = html::parse(b"<details open><summary>s</summary><p>body</p></details>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
        let body = find(&dom, dom.document(), "p").expect("p not found");
        assert_eq!(styles[&body].display, Display::Block);
    }

    #[test]
    fn the_hidden_attribute_hides_any_element() {
        assert_eq!(
            style_of("<div hidden>x</div>", "div").display,
            Display::None
        );
    }

    #[test]
    fn author_css_overrides_the_hidden_attribute() {
        let dom = html::parse(b"<div hidden>x</div>");
        let styles = compute_styles(
            &dom,
            &user_agent_stylesheet(),
            &parse_stylesheet("[hidden] { display: block; }"),
        );
        let div = find(&dom, dom.document(), "div").expect("div not found");
        assert_eq!(
            styles[&div].display,
            Display::Block,
            "author origin must win over the UA rule"
        );
    }

    #[test]
    fn q_generates_quotation_marks() {
        let style = style_of("<p><q>x</q></p>", "q");
        assert_eq!(style.pseudo_before_content.as_deref(), Some("\u{201c}"));
        assert_eq!(style.pseudo_after_content.as_deref(), Some("\u{201d}"));
    }
}
