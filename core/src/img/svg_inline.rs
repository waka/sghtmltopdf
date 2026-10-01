//! Serialising an inline `<svg>` element (from the HTML DOM) into an SVG document.
//!
//! The HTML parser has already done the hard part of the foreign-content rules: element
//! names such as `linearGradient` and attributes such as `viewBox` have their SVG casing
//! restored, and `xlink:href`/`xml:space`/`xmlns:*` carry their prefix. What is left is to
//! write the subtree out as XML for the SVG renderer ([`crate::pdf::svg`]):
//!
//! - the root gets `xmlns` (and `xmlns:xlink`) when the HTML markup left them out, which it
//!   usually does;
//! - `currentColor` is resolved by setting the root's `color` attribute to the element's
//!   computed CSS `color` (the renderer inherits it down the tree);
//! - a `width`/`height` the renderer cannot use as a size (`100%`, `2em`) is dropped, and when
//!   neither a `viewBox` nor a usable size is left, the CSS default replaced size 300x150
//!   stands in for the missing side.

use std::fmt::Write;

use html5ever::Attribute;

use crate::html::{Dom, NodeData, NodeId};
use crate::style::RgbaColor;

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// The CSS default size of a replaced element with neither an intrinsic size nor a ratio.
const DEFAULT_WIDTH: f32 = 300.0;
const DEFAULT_HEIGHT: f32 = 150.0;

/// Serialise the `<svg>` element `root` and its subtree. `color` is the element's computed
/// CSS `color`, used for `currentColor`.
pub fn serialize_inline_svg(dom: &Dom, root: NodeId, color: RgbaColor) -> String {
    let mut out = String::new();
    write_element(dom, root, true, color, &mut out);
    out
}

fn write_element(dom: &Dom, node: NodeId, is_root: bool, color: RgbaColor, out: &mut String) {
    let NodeData::Element { name, attrs, .. } = &dom.node(node).data else {
        return;
    };
    let tag = &*name.local;
    out.push('<');
    out.push_str(tag);
    if is_root {
        write_root_attributes(attrs, color, out);
    } else {
        for attr in attrs {
            write_attribute(&qualified_name(attr), &attr.value, out);
        }
    }
    let mut children = dom.children(node).peekable();
    if children.peek().is_none() {
        out.push_str("/>");
        return;
    }
    out.push('>');
    for child in children {
        match &dom.node(child).data {
            NodeData::Element { .. } => write_element(dom, child, false, color, out),
            NodeData::Text { contents } => escape_into(contents, false, out),
            _ => {}
        }
    }
    let _ = write!(out, "</{tag}>");
}

fn write_root_attributes(attrs: &[Attribute], color: RgbaColor, out: &mut String) {
    let mut has_xmlns = false;
    let mut has_xmlns_xlink = false;
    let mut view_box: Option<(f32, f32)> = None;
    let mut width = None;
    let mut height = None;

    for attr in attrs {
        let name = qualified_name(attr);
        match name.as_str() {
            "xmlns" => has_xmlns = true,
            "xmlns:xlink" => has_xmlns_xlink = true,
            "viewBox" => view_box = parse_view_box_size(&attr.value),
            // CSS `color` is applied below; `width`/`height` are normalised below.
            "color" => continue,
            "width" => {
                width = svg_px_length(&attr.value);
                continue;
            }
            "height" => {
                height = svg_px_length(&attr.value);
                continue;
            }
            _ => {}
        }
        write_attribute(&name, &attr.value, out);
    }

    if !has_xmlns {
        write_attribute("xmlns", SVG_NS, out);
    }
    if !has_xmlns_xlink {
        write_attribute("xmlns:xlink", XLINK_NS, out);
    }
    // A missing side follows the viewBox's aspect ratio (the renderer would otherwise take the
    // viewBox's own height and letterbox the drawing). With no usable viewBox, the CSS default
    // replaced size applies.
    match (view_box, width, height) {
        (Some((vw, vh)), Some(w), None) => height = Some(w * vh / vw),
        (Some((vw, vh)), None, Some(h)) => width = Some(h * vw / vh),
        (None, _, _) => {
            width.get_or_insert(DEFAULT_WIDTH);
            height.get_or_insert(DEFAULT_HEIGHT);
        }
        _ => {}
    }
    if let Some(width) = width {
        write_attribute("width", &width.to_string(), out);
    }
    if let Some(height) = height {
        write_attribute("height", &height.to_string(), out);
    }
    write_attribute("color", &css_color(color), out);
}

/// The width and height of a `viewBox` (`None` unless four numbers with a positive size).
fn parse_view_box_size(value: &str) -> Option<(f32, f32)> {
    let numbers: Vec<f32> = value
        .split(|c: char| c.is_ascii_whitespace() || c == ',')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse::<f32>())
        .collect::<Result<_, _>>()
        .ok()?;
    match numbers[..] {
        [_, _, w, h] if w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite() => Some((w, h)),
        _ => None,
    }
}

/// A `width`/`height` attribute the renderer can take as a size: a positive number,
/// optionally in `px`. Percentages and font-relative units depend on the layout, which the
/// CSS side handles (see the presentational hints), so they are dropped here.
fn svg_px_length(value: &str) -> Option<f32> {
    let value = value.trim();
    let number = value.strip_suffix("px").unwrap_or(value).trim_end();
    number
        .parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

fn css_color(color: RgbaColor) -> String {
    if color.alpha >= 1.0 {
        format!("rgb({},{},{})", color.red, color.green, color.blue)
    } else {
        format!(
            "rgba({},{},{},{})",
            color.red,
            color.green,
            color.blue,
            color.alpha.max(0.0)
        )
    }
}

/// `prefix:local` for a prefixed attribute (`xlink:href`, `xml:space`, `xmlns:xlink`),
/// otherwise the local name.
fn qualified_name(attr: &Attribute) -> String {
    // html5ever gives the plain `xmlns` attribute an empty (not absent) prefix.
    match attr
        .name
        .prefix
        .as_ref()
        .filter(|prefix| !prefix.is_empty())
    {
        Some(prefix) => format!("{prefix}:{}", attr.name.local),
        None => attr.name.local.to_string(),
    }
}

fn write_attribute(name: &str, value: &str, out: &mut String) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    escape_into(value, true, out);
    out.push('"');
}

/// Escape for XML text (or, with `attribute`, for a double-quoted attribute value) and drop
/// the characters XML 1.0 cannot carry at all.
fn escape_into(text: &str, attribute: bool, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\t' | '\n' | '\r' if attribute => {
                let _ = write!(out, "&#{};", c as u32);
            }
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn serialize(markup: &str) -> String {
        let dom = html::parse(markup.as_bytes());
        let svg = find_svg(&dom, dom.document()).expect("no <svg>");
        serialize_inline_svg(&dom, svg, RgbaColor::BLACK)
    }

    fn find_svg(dom: &Dom, id: NodeId) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == "svg" {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find_svg(dom, child))
    }

    #[test]
    fn adds_the_namespaces_the_html_markup_left_out() {
        let out = serialize(r#"<svg viewBox="0 0 10 1"><rect width="10" height="1"/></svg>"#);
        assert!(
            out.contains(r#"xmlns="http://www.w3.org/2000/svg""#),
            "{out}"
        );
        assert!(out.contains("xmlns:xlink="), "{out}");
        assert!(out.contains(r#"viewBox="0 0 10 1""#), "{out}");
        assert!(out.contains("<rect"), "{out}");
    }

    #[test]
    fn does_not_duplicate_an_existing_xmlns() {
        let out = serialize(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><path d="M0 0"/></svg>"#,
        );
        assert_eq!(out.matches("xmlns=").count(), 1, "{out}");
    }

    #[test]
    fn keeps_svg_name_casing_and_prefixed_attributes() {
        let out = serialize(
            r##"<svg viewBox="0 0 4 4"><defs><linearGradient id="g"><stop offset="0"/></linearGradient></defs><use xlink:href="#g"/></svg>"##,
        );
        assert!(out.contains("<linearGradient"), "{out}");
        assert!(out.contains(r##"xlink:href="#g""##), "{out}");
    }

    #[test]
    fn sets_the_color_attribute_for_current_color() {
        let dom = html::parse(br#"<svg viewBox="0 0 1 1" color="blue"></svg>"#);
        let svg = find_svg(&dom, dom.document()).unwrap();
        let red = RgbaColor {
            red: 255,
            green: 0,
            blue: 0,
            alpha: 1.0,
        };
        let out = serialize_inline_svg(&dom, svg, red);
        assert!(out.contains(r#"color="rgb(255,0,0)""#), "{out}");
        assert_eq!(out.matches("color=").count(), 1, "{out}");
    }

    #[test]
    fn escapes_text_and_attribute_values() {
        let out = serialize(
            r#"<svg viewBox="0 0 1 1"><text data-x="a&quot;b&amp;c">1 &lt; 2 &amp; 3 &nbsp;</text></svg>"#,
        );
        assert!(out.contains(r#"data-x="a&quot;b&amp;c""#), "{out}");
        assert!(out.contains(">1 &lt; 2 &amp; 3 \u{a0}</text>"), "{out}");
    }

    #[test]
    fn style_element_text_is_kept() {
        let out = serialize(
            r#"<svg viewBox="0 0 1 1"><style>.a > rect { fill: red }</style><rect class="a"/></svg>"#,
        );
        assert!(
            out.contains("<style>.a &gt; rect { fill: red }</style>"),
            "{out}"
        );
    }

    #[test]
    fn a_missing_side_follows_the_view_box_ratio() {
        let out = serialize(r#"<svg viewBox="0 0 120 30" width="240"><rect/></svg>"#);
        assert!(
            out.contains(r#"width="240""#) && out.contains(r#"height="60""#),
            "{out}"
        );
        let out = serialize(r#"<svg viewBox="0 0 120 30" height="10"><rect/></svg>"#);
        assert!(out.contains(r#"width="40""#), "{out}");
    }

    #[test]
    fn unusable_sizes_are_dropped_and_the_default_size_fills_in_without_a_view_box() {
        let out = serialize(r#"<svg width="100%" height="2em"><rect/></svg>"#);
        assert!(out.contains(r#"width="300""#), "{out}");
        assert!(out.contains(r#"height="150""#), "{out}");
        let out = serialize(r#"<svg viewBox="0 0 24 24" width="100%"><rect/></svg>"#);
        assert!(!out.contains("width="), "{out}");
        let out = serialize(r#"<svg viewBox="0 0 24 24" height="2em"><rect/></svg>"#);
        assert!(!out.contains("height="), "{out}");
        let out = serialize(r#"<svg viewBox="0 0 24 24" width="48px"><rect/></svg>"#);
        assert!(out.contains(r#"width="48""#), "{out}");
    }
}
