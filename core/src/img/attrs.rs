//! Extraction of DOM attributes from `<img>` elements.
//!
//! `width`/`height` here means the HTML attributes (unitless integer px), not the CSS values.
//!
//! The HTML spec's "rules for parsing non-negative integers" skip leading whitespace,
//! then read digits for as long as they continue and ignore the rest (the whole string
//! does not have to be digits). That is why a value with a unit, such as `width="100px"`,
//! is read as `100` by browsers. We match that: instead of a whole-string match like
//! `str::parse`, we take only the leading run of digits.

use html5ever::Attribute;

use crate::html::{Dom, NodeData, NodeId};

/// Attributes read from an `<img>` element (raw values, before URL resolution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImgAttrs {
    /// Value of the `src` attribute (an empty string becomes `None`, so it never lands here).
    pub src: String,
    /// Value of the `width` attribute (in px; `None` if absent or non-numeric).
    pub width: Option<u32>,
    /// Value of the `height` attribute (in px; `None` if absent or non-numeric).
    pub height: Option<u32>,
    /// Value of the `alt` attribute. `None` if the attribute is absent; an explicitly empty
    /// value such as `alt=""` is kept as `Some(String::new())`, following the HTML
    /// convention of distinguishing decorative `alt=""` from an unset attribute.
    pub alt: Option<String>,
}

/// Read the attributes only if `node` is an `<img>` element carrying a `src` attribute.
///
/// Returns `None` if it is not an `<img>`, or if `src` is missing (or present but empty).
/// Callers treat that as a "replaced element with no image".
pub fn read_img_attrs(dom: &Dom, node: NodeId) -> Option<ImgAttrs> {
    let NodeData::Element { name, attrs, .. } = &dom.node(node).data else {
        return None;
    };
    if &*name.local != "img" {
        return None;
    }

    let src = find_attr(attrs, "src")
        .map(|value| value.to_string())
        .filter(|s| !s.is_empty())?;
    let width = read_pixel_attr(attrs, "width");
    let height = read_pixel_attr(attrs, "height");
    let alt = find_attr(attrs, "alt").map(|value| value.to_string());

    Some(ImgAttrs {
        src,
        width,
        height,
        alt,
    })
}

fn find_attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.as_ref())
}

fn read_pixel_attr(attrs: &[Attribute], name: &str) -> Option<u32> {
    find_attr(attrs, name).and_then(parse_non_negative_integer_prefix)
}

/// A simplified version of the HTML spec's "rules for parsing non-negative integers":
/// skip leading whitespace, then read as many digits as follow and interpret them as
/// decimal (everything after the first non-digit is ignored).
/// `None` if there is no digit at all. A leading `-` never starts collecting digits, so it naturally yields `None`.
fn parse_non_negative_integer_prefix(value: &str) -> Option<u32> {
    let digits: String = value
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn find(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find(dom, child, tag))
    }

    #[test]
    fn reads_all_attributes_when_present() {
        let dom =
            html::parse(br#"<img src="logo.png" width="120" height="40" alt="Company logo">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
        assert_eq!(attrs.src, "logo.png");
        assert_eq!(attrs.width, Some(120));
        assert_eq!(attrs.height, Some(40));
        assert_eq!(attrs.alt.as_deref(), Some("Company logo"));
    }

    #[test]
    fn missing_optional_attributes_are_none() {
        let dom = html::parse(br#"<img src="logo.png">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
        assert_eq!(attrs.width, None);
        assert_eq!(attrs.height, None);
        assert_eq!(attrs.alt, None);
    }

    #[test]
    fn empty_alt_is_distinguished_from_missing_alt() {
        let dom = html::parse(br#"<img src="deco.png" alt="">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
        assert_eq!(attrs.alt.as_deref(), Some(""));
    }

    #[test]
    fn width_and_height_accept_a_trailing_unit_suffix() {
        // Per the HTML spec, a width/height attribute value is read only up to its leading
        // run of digits, so values with a unit such as `100px` or `50%` are effectively
        // read as px (matching what browsers actually do).
        let dom = html::parse(br#"<img src="logo.png" width="100px" height="50%">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
        assert_eq!(attrs.width, Some(100));
        assert_eq!(attrs.height, Some(50));
    }

    #[test]
    fn width_ignores_leading_and_trailing_whitespace() {
        let dom = html::parse(br#"<img src="logo.png" width=" 42 ">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
        assert_eq!(attrs.width, Some(42));
    }

    #[test]
    fn non_numeric_width_and_height_are_none() {
        let dom = html::parse(br#"<img src="logo.png" width="huge" height="-1">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
        assert_eq!(attrs.width, None);
        assert_eq!(
            attrs.height, None,
            "negative numbers are not valid non-negative integers"
        );
    }

    #[test]
    fn missing_src_is_none() {
        let dom = html::parse(br#"<img alt="no src">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        assert_eq!(read_img_attrs(&dom, img), None);
    }

    #[test]
    fn empty_src_is_none() {
        let dom = html::parse(br#"<img src="">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        assert_eq!(read_img_attrs(&dom, img), None);
    }

    #[test]
    fn non_img_element_is_none() {
        let dom = html::parse(br#"<div src="not-an-img.png"></div>"#);
        let div = find(&dom, dom.document(), "div").expect("div not found");

        assert_eq!(read_img_attrs(&dom, div), None);
    }

    #[test]
    fn released_node_is_none() {
        let mut dom = html::parse(br#"<div><img src="logo.png"></div>"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");
        dom.release_subtree(img);

        assert_eq!(read_img_attrs(&dom, img), None);
    }
}
