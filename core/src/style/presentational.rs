//! Translating legacy HTML presentational attributes into the corresponding CSS declarations.
//!
//! In the cascade they sit "stronger than the UA stylesheet, weaker than author CSS", so
//! author CSS can always override them.
//!
//! Attribute values have no parser of their own: they are assembled into CSS declaration
//! text and handed to [`parse_inline_style`]. Colours, lengths and percentages are then
//! interpreted by exactly the same code as the `style` attribute, and invalid values are
//! dropped by the existing "ignore invalid declarations" behaviour.

use html5ever::Attribute;

use crate::html::{Dom, NodeData, NodeId};

use super::properties::PropertyDeclaration;
use super::stylesheet::parse_inline_style;

/// Font sizes (px) for `<font size>` 1 to 7. The default 16px is `size="3"`.
const FONT_SIZE_TABLE: [f32; 7] = [10.0, 13.0, 16.0, 18.0, 24.0, 32.0, 48.0];

/// Return the CSS declarations implied by `element`'s legacy presentational attributes.
pub(super) fn presentational_hint_declarations(
    dom: &Dom,
    element: NodeId,
) -> Vec<PropertyDeclaration> {
    let NodeData::Element { name, attrs, .. } = &dom.node(element).data else {
        return Vec::new();
    };
    let tag = name.local.to_string();
    let mut css = String::new();

    // Attributes that apply to any element.
    if let Some(value) = attr(attrs, "align") {
        push_align(&tag, value, &mut css);
    }

    match tag.as_str() {
        "body" => {
            push_color(&mut css, "background-color", attr(attrs, "bgcolor"));
            push_color(&mut css, "color", attr(attrs, "text"));
        }
        "table" => {
            push_length(&mut css, "width", attr(attrs, "width"));
            push_length(&mut css, "height", attr(attrs, "height"));
            push_color(&mut css, "background-color", attr(attrs, "bgcolor"));
            if let Some(border) = attr(attrs, "border").and_then(parse_pixels) {
                if border > 0.0 {
                    css.push_str(&format!("border: {border}px outset currentColor;"));
                }
            }
            if let Some(spacing) = attr(attrs, "cellspacing").and_then(parse_pixels) {
                css.push_str(&format!("border-spacing: {spacing}px;"));
            }
        }
        "tr" | "thead" | "tbody" | "tfoot" => {
            push_color(&mut css, "background-color", attr(attrs, "bgcolor"));
            push_vertical_align(&mut css, attr(attrs, "valign"));
        }
        "td" | "th" => {
            push_length(&mut css, "width", attr(attrs, "width"));
            push_length(&mut css, "height", attr(attrs, "height"));
            push_color(&mut css, "background-color", attr(attrs, "bgcolor"));
            push_vertical_align(&mut css, attr(attrs, "valign"));
            if attrs.iter().any(|a| &*a.name.local == "nowrap") {
                css.push_str("white-space: nowrap;");
            }
            // `<table border/cellpadding>` are "attributes on the table whose effect shows
            // on the cells". Find the nearest `<table>` ancestor.
            if let Some(table) = nearest_table(dom, element) {
                let NodeData::Element {
                    attrs: table_attrs, ..
                } = &dom.node(table).data
                else {
                    unreachable!("nearest_table returns an element");
                };
                if attr(table_attrs, "border").and_then(parse_pixels) > Some(0.0) {
                    css.push_str("border: 1px inset currentColor;");
                }
                if let Some(padding) = attr(table_attrs, "cellpadding").and_then(parse_pixels) {
                    css.push_str(&format!("padding: {padding}px;"));
                }
            }
        }
        "col" | "colgroup" => push_length(&mut css, "width", attr(attrs, "width")),
        "img" => {
            if let Some(border) = attr(attrs, "border").and_then(parse_pixels) {
                css.push_str(&format!("border: {border}px solid currentColor;"));
            }
            if let Some(h) = attr(attrs, "hspace").and_then(parse_pixels) {
                css.push_str(&format!("margin-left: {h}px; margin-right: {h}px;"));
            }
            if let Some(v) = attr(attrs, "vspace").and_then(parse_pixels) {
                css.push_str(&format!("margin-top: {v}px; margin-bottom: {v}px;"));
            }
        }
        "hr" => {
            push_length(&mut css, "width", attr(attrs, "width"));
            if let Some(size) = attr(attrs, "size").and_then(parse_pixels) {
                css.push_str(&format!("height: {size}px;"));
            }
            if attrs.iter().any(|a| &*a.name.local == "noshade") {
                css.push_str("border-style: solid;");
            }
        }
        "font" => {
            push_color(&mut css, "color", attr(attrs, "color"));
            if let Some(face) = attr(attrs, "face") {
                // Quote the family name (to guard against names with spaces and clashes with keywords).
                let families: Vec<String> = face
                    .split(',')
                    .map(|f| format!("\"{}\"", f.trim().replace('"', "")))
                    .collect();
                css.push_str(&format!("font-family: {};", families.join(", ")));
            }
            if let Some(size) = attr(attrs, "size").and_then(parse_font_size) {
                css.push_str(&format!("font-size: {size}px;"));
            }
        }
        "ul" | "ol" | "li" => {
            if let Some(list_style) = attr(attrs, "type").and_then(|t| legacy_list_type(&tag, t)) {
                css.push_str(&format!("list-style-type: {list_style};"));
            }
        }
        "br" => {
            if let Some(clear) = attr(attrs, "clear") {
                let value = match clear.trim().to_ascii_lowercase().as_str() {
                    "left" => "left",
                    "right" => "right",
                    "all" => "both",
                    "none" => "none",
                    _ => "",
                };
                if !value.is_empty() {
                    css.push_str(&format!("clear: {value};"));
                }
            }
        }
        _ => {}
    }

    if css.is_empty() {
        return Vec::new();
    }
    parse_inline_style(&css)
}

fn attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| &*a.value)
}

/// The `align` attribute. On a table itself it means aligning the table (in CSS terms,
/// `float` or left/right margins); on any other element it means the `text-align` of its contents.
fn push_align(tag: &str, value: &str, css: &mut String) {
    let value = value.trim().to_ascii_lowercase();
    if tag == "table" || tag == "img" {
        match value.as_str() {
            "left" => css.push_str("float: left;"),
            "right" => css.push_str("float: right;"),
            "center" if tag == "table" => css.push_str("margin-left: auto; margin-right: auto;"),
            _ => {}
        }
        return;
    }
    if matches!(value.as_str(), "left" | "right" | "center" | "justify") {
        css.push_str(&format!("text-align: {value};"));
    }
}

fn push_vertical_align(css: &mut String, value: Option<&str>) {
    let Some(value) = value else { return };
    let value = value.trim().to_ascii_lowercase();
    if matches!(value.as_str(), "top" | "middle" | "bottom" | "baseline") {
        css.push_str(&format!("vertical-align: {value};"));
    }
}

/// Accepts both `width="100"` (treated as px) and `width="50%"`.
fn push_length(css: &mut String, property: &str, value: Option<&str>) {
    let Some(value) = value else { return };
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    if let Some(percent) = value.strip_suffix('%') {
        if percent.trim().parse::<f32>().is_ok() {
            css.push_str(&format!("{property}: {value};"));
        }
        return;
    }
    if let Some(px) = parse_pixels(value) {
        css.push_str(&format!("{property}: {px}px;"));
    }
}

/// Write out a legacy colour as a CSS colour (hex notation without the `#`,
/// as in `bgcolor="ffffff"`, is accepted too).
fn push_color(css: &mut String, property: &str, value: Option<&str>) {
    let Some(value) = value else { return };
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let is_bare_hex = !value.starts_with('#')
        && matches!(value.len(), 3 | 6)
        && value.chars().all(|c| c.is_ascii_hexdigit());
    if is_bare_hex {
        css.push_str(&format!("{property}: #{value};"));
    } else {
        css.push_str(&format!("{property}: {value};"));
    }
}

/// Accepts a unitless number (`"100"`) and a value with a trailing `px` (`"100px"`, which
/// is invalid HTML but does occur). Negative values are ignored.
fn parse_pixels(value: &str) -> Option<f32> {
    let value = value.trim();
    let value = value.strip_suffix("px").unwrap_or(value);
    value.trim().parse::<f32>().ok().filter(|v| *v >= 0.0)
}

/// `<font size>`. Absolute values `1` to `7`, and relative values `+N`/`-N`.
fn parse_font_size(value: &str) -> Option<f32> {
    let value = value.trim();
    let (base_index, rest) = match value.strip_prefix('+') {
        Some(rest) => (3i32, rest),
        None => match value.strip_prefix('-') {
            Some(rest) => (3i32, rest),
            None => (0i32, value),
        },
    };
    let n: i32 = rest.trim().parse().ok()?;
    let index = if base_index == 0 {
        n
    } else if value.starts_with('+') {
        3 + n
    } else {
        3 - n
    };
    let clamped = index.clamp(1, FONT_SIZE_TABLE.len() as i32);
    Some(FONT_SIZE_TABLE[(clamped - 1) as usize])
}

/// `<ul type>` (`disc`/`circle`/`square`) and `<ol type>`/`<li type>`
/// (`1`/`a`/`A`/`i`/`I`).
fn legacy_list_type(tag: &str, value: &str) -> Option<&'static str> {
    let value = value.trim();
    match value.to_ascii_lowercase().as_str() {
        "disc" => Some("disc"),
        "circle" => Some("circle"),
        "square" => Some("square"),
        _ if tag == "ul" => None,
        _ => match value {
            "1" => Some("decimal"),
            "a" => Some("lower-alpha"),
            "A" => Some("upper-alpha"),
            "i" => Some("lower-roman"),
            "I" => Some("upper-roman"),
            _ => None,
        },
    }
}

/// The nearest `<table>` ancestor of `node`.
fn nearest_table(dom: &Dom, node: NodeId) -> Option<NodeId> {
    let mut current = dom.parent(node);
    while let Some(id) = current {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == "table" {
                return Some(id);
            }
        }
        current = dom.parent(id);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;
    use crate::style::values::{
        Clear, Color, Float, ListStyleType, SpecifiedLength, SpecifiedLengthPercentage,
        SpecifiedLengthPercentageOrAuto, SpecifiedVerticalAlign, TextAlign, WhiteSpace,
    };

    /// Extract the "px length" of a specified value (None for a percentage or auto).
    fn px_of(value: &SpecifiedLengthPercentageOrAuto) -> Option<f32> {
        match value {
            SpecifiedLengthPercentageOrAuto::LengthPercentage(
                SpecifiedLengthPercentage::Length(SpecifiedLength::Px(px)),
            ) => Some(*px),
            _ => None,
        }
    }

    /// Extract the "percentage" of a specified value (a ratio from 0 to 1).
    fn percent_of(value: &SpecifiedLengthPercentageOrAuto) -> Option<f32> {
        match value {
            SpecifiedLengthPercentageOrAuto::LengthPercentage(
                SpecifiedLengthPercentage::Percentage(p),
            ) => Some(*p),
            _ => None,
        }
    }

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    fn hints_for(html_src: &str, tag: &str) -> Vec<PropertyDeclaration> {
        let dom = html::parse(html_src.as_bytes());
        let node = find(&dom, dom.document(), tag).expect("element not found");
        presentational_hint_declarations(&dom, node)
    }

    #[test]
    fn align_maps_to_text_align_on_ordinary_elements() {
        let hints = hints_for(r#"<p align="center">x</p>"#, "p");
        assert!(hints.contains(&PropertyDeclaration::TextAlign(TextAlign::Center)));
    }

    #[test]
    fn align_on_a_table_moves_the_table_itself() {
        let hints = hints_for(
            r#"<table align="center"><tr><td>x</td></tr></table>"#,
            "table",
        );
        assert!(hints.contains(&PropertyDeclaration::MarginLeft(
            SpecifiedLengthPercentageOrAuto::Auto
        )));
        assert!(hints.contains(&PropertyDeclaration::MarginRight(
            SpecifiedLengthPercentageOrAuto::Auto
        )));

        let hints = hints_for(
            r#"<table align="right"><tr><td>x</td></tr></table>"#,
            "table",
        );
        assert!(hints.contains(&PropertyDeclaration::Float(Float::Right)));
    }

    #[test]
    fn width_attribute_accepts_bare_numbers_and_percentages() {
        let hints = hints_for(r#"<table width="300"><tr><td>x</td></tr></table>"#, "table");
        assert_eq!(
            hints.iter().find_map(|d| match d {
                PropertyDeclaration::Width(w) => px_of(w),
                _ => None,
            }),
            Some(300.0)
        );

        let hints = hints_for(r#"<table width="50%"><tr><td>x</td></tr></table>"#, "table");
        assert_eq!(
            hints.iter().find_map(|d| match d {
                PropertyDeclaration::Width(w) => percent_of(w),
                _ => None,
            }),
            Some(0.5)
        );
    }

    #[test]
    fn bgcolor_accepts_named_hash_and_bare_hex_colors() {
        for source in [
            r#"<table bgcolor="red"><tr><td>x</td></tr></table>"#,
            r##"<table bgcolor="#ff0000"><tr><td>x</td></tr></table>"##,
            r#"<table bgcolor="ff0000"><tr><td>x</td></tr></table>"#,
        ] {
            let hints = hints_for(source, "table");
            let color = hints
                .iter()
                .find_map(|d| match d {
                    PropertyDeclaration::BackgroundColor(Color::Rgba {
                        red, green, blue, ..
                    }) => Some((*red, *green, *blue)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no background-color for {source}"));
            assert_eq!(color, (255, 0, 0));
        }
    }

    #[test]
    fn an_invalid_color_is_dropped_like_an_invalid_css_declaration() {
        let hints = hints_for(
            r#"<table bgcolor="definitely not a color"><tr><td>x</td></tr></table>"#,
            "table",
        );
        assert!(hints
            .iter()
            .all(|d| !matches!(d, PropertyDeclaration::BackgroundColor(_))));
    }

    #[test]
    fn table_border_puts_a_border_on_the_table_and_on_its_cells() {
        let source = r#"<table border="2"><tr><td>x</td></tr></table>"#;
        let table_hints = hints_for(source, "table");
        assert!(table_hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::BorderTopWidth(SpecifiedLength::Px(w)) if *w == 2.0
        )));

        let cell_hints = hints_for(source, "td");
        assert!(cell_hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::BorderTopWidth(SpecifiedLength::Px(w)) if *w == 1.0
        )));
    }

    #[test]
    fn table_border_zero_draws_nothing() {
        let source = r#"<table border="0"><tr><td>x</td></tr></table>"#;
        assert!(hints_for(source, "table")
            .iter()
            .all(|d| !matches!(d, PropertyDeclaration::BorderTopWidth(_))));
        assert!(hints_for(source, "td")
            .iter()
            .all(|d| !matches!(d, PropertyDeclaration::BorderTopWidth(_))));
    }

    #[test]
    fn cellpadding_becomes_padding_on_the_cells() {
        let hints = hints_for(
            r#"<table cellpadding="6"><tr><td>x</td></tr></table>"#,
            "td",
        );
        assert!(hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::PaddingTop(SpecifiedLengthPercentage::Length(
                SpecifiedLength::Px(v)
            )) if *v == 6.0
        )));
    }

    #[test]
    fn cellspacing_becomes_border_spacing_on_the_table() {
        let hints = hints_for(
            r#"<table cellspacing="4"><tr><td>x</td></tr></table>"#,
            "table",
        );
        assert!(hints
            .iter()
            .any(|d| matches!(d, PropertyDeclaration::BorderSpacing(..))));
    }

    #[test]
    fn cell_attributes_map_to_alignment_and_wrapping() {
        let source = r#"<table><tr><td align="right" valign="top" nowrap>x</td></tr></table>"#;
        let hints = hints_for(source, "td");
        assert!(hints.contains(&PropertyDeclaration::TextAlign(TextAlign::Right)));
        assert!(hints.contains(&PropertyDeclaration::VerticalAlign(
            SpecifiedVerticalAlign::Top
        )));
        assert!(hints.contains(&PropertyDeclaration::WhiteSpace(WhiteSpace::Nowrap)));
    }

    #[test]
    fn nested_tables_use_the_nearest_table_for_cell_hints() {
        let dom = html::parse(
            br#"<table cellpadding="10"><tr><td>
                  <table cellpadding="2"><tr><td id="inner">x</td></tr></table>
                </td></tr></table>"#,
        );
        // The inner `<td id="inner">` uses the inner table's cellpadding.
        fn find_by_id(dom: &Dom, id: NodeId, target: &str) -> Option<NodeId> {
            if let NodeData::Element { attrs, .. } = &dom.node(id).data {
                if attrs
                    .iter()
                    .any(|a| &*a.name.local == "id" && &*a.value == target)
                {
                    return Some(id);
                }
            }
            dom.children(id).find_map(|c| find_by_id(dom, c, target))
        }
        let inner = find_by_id(&dom, dom.document(), "inner").expect("inner cell not found");
        let hints = presentational_hint_declarations(&dom, inner);
        assert!(hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::PaddingTop(SpecifiedLengthPercentage::Length(
                SpecifiedLength::Px(v)
            )) if *v == 2.0
        )));
    }

    #[test]
    fn font_attributes_map_to_color_family_and_size() {
        let hints = hints_for(
            r##"<p><font color="#00ff00" face="Times New Roman, serif" size="5">x</font></p>"##,
            "font",
        );
        assert!(hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::Color(Color::Rgba { red, green, blue, .. })
                if (*red, *green, *blue) == (0, 255, 0)
        )));
        assert!(hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::FontFamily(f) if f[0] == "Times New Roman"
        )));
        assert!(hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::FontSize(SpecifiedLength::Px(v)) if *v == 24.0
        )));
    }

    #[test]
    fn font_size_supports_relative_values_and_clamps_out_of_range() {
        assert_eq!(parse_font_size("3"), Some(16.0));
        assert_eq!(parse_font_size("+1"), Some(18.0));
        assert_eq!(parse_font_size("-2"), Some(10.0));
        assert_eq!(parse_font_size("99"), Some(48.0));
        assert_eq!(parse_font_size("0"), Some(10.0));
        assert_eq!(parse_font_size("not a number"), None);
    }

    #[test]
    fn list_type_attribute_maps_to_list_style_type() {
        let hints = hints_for(r#"<ol type="A"><li>x</li></ol>"#, "ol");
        assert!(hints.contains(&PropertyDeclaration::ListStyleType(
            ListStyleType::UpperAlpha
        )));

        let hints = hints_for(r#"<ul type="square"><li>x</li></ul>"#, "ul");
        assert!(hints.contains(&PropertyDeclaration::ListStyleType(ListStyleType::Square)));

        // `<ul type="1">` is meaningless in HTML, so nothing is emitted.
        let hints = hints_for(r#"<ul type="1"><li>x</li></ul>"#, "ul");
        assert!(hints
            .iter()
            .all(|d| !matches!(d, PropertyDeclaration::ListStyleType(_))));
    }

    #[test]
    fn br_clear_maps_to_the_clear_property() {
        let hints = hints_for(r#"<p>a<br clear="all">b</p>"#, "br");
        assert!(hints.contains(&PropertyDeclaration::Clear(Clear::Both)));
    }

    #[test]
    fn hr_attributes_map_to_width_height_and_border_style() {
        let hints = hints_for(r#"<hr width="50%" size="4" noshade>"#, "hr");
        assert_eq!(
            hints.iter().find_map(|d| match d {
                PropertyDeclaration::Width(w) => percent_of(w),
                _ => None,
            }),
            Some(0.5)
        );
        assert_eq!(
            hints.iter().find_map(|d| match d {
                PropertyDeclaration::Height(h) => px_of(h),
                _ => None,
            }),
            Some(4.0)
        );
    }

    #[test]
    fn img_spacing_attributes_map_to_margins_and_borders() {
        let hints = hints_for(
            r#"<img src="a.png" border="2" hspace="5" vspace="7">"#,
            "img",
        );
        assert_eq!(
            hints.iter().find_map(|d| match d {
                PropertyDeclaration::MarginLeft(m) => px_of(m),
                _ => None,
            }),
            Some(5.0)
        );
        assert_eq!(
            hints.iter().find_map(|d| match d {
                PropertyDeclaration::MarginTop(m) => px_of(m),
                _ => None,
            }),
            Some(7.0)
        );
        assert!(hints.iter().any(|d| matches!(
            d,
            PropertyDeclaration::BorderTopWidth(SpecifiedLength::Px(w)) if *w == 2.0
        )));
    }

    #[test]
    fn an_element_without_presentational_attributes_produces_nothing() {
        assert!(hints_for(r#"<p class="x">text</p>"#, "p").is_empty());
    }

    // ===== Position in the cascade =====

    #[test]
    fn author_css_overrides_a_presentational_attribute() {
        use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

        let dom = html::parse(br#"<p align="center">x</p>"#);
        let styles = compute_styles(
            &dom,
            &user_agent_stylesheet(),
            &parse_stylesheet("p { text-align: right; }"),
        );
        let p = find(&dom, dom.document(), "p").expect("p not found");
        assert_eq!(
            styles[&p].text_align,
            TextAlign::Right,
            "author CSS must win over a presentational attribute"
        );
    }

    #[test]
    fn a_presentational_attribute_overrides_the_ua_stylesheet() {
        use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

        // The UA stylesheet has `th { text-align: center }`. The attribute is stronger.
        let dom = html::parse(br#"<table><tr><th align="left">x</th></tr></table>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
        let th = find(&dom, dom.document(), "th").expect("th not found");
        assert_eq!(styles[&th].text_align, TextAlign::Left);
    }

    #[test]
    fn inline_style_still_beats_a_presentational_attribute() {
        use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

        let dom = html::parse(br#"<p align="center" style="text-align: justify;">x</p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
        let p = find(&dom, dom.document(), "p").expect("p not found");
        assert_eq!(styles[&p].text_align, TextAlign::Justify);
    }
}
