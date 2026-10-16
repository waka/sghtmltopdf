//! Extraction of DOM attributes from `<img>` elements.
//!
//! `width`/`height` are not read here; they become CSS hints in `style::presentational`.

use html5ever::Attribute;

use crate::html::{Dom, NodeData, NodeId};

/// Attributes read from an `<img>` element (raw values, before URL resolution).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImgAttrs {
    /// Value of the `src` attribute (an empty string becomes `None`, so it never lands here).
    pub src: String,
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
    let alt = find_attr(attrs, "alt").map(|value| value.to_string());

    Some(ImgAttrs { src, alt })
}

fn find_attr<'a>(attrs: &'a [Attribute], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|attr| &*attr.name.local == name)
        .map(|attr| attr.value.as_ref())
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
        assert_eq!(attrs.alt.as_deref(), Some("Company logo"));
    }

    #[test]
    fn missing_optional_attributes_are_none() {
        let dom = html::parse(br#"<img src="logo.png">"#);
        let img = find(&dom, dom.document(), "img").expect("img not found");

        let attrs = read_img_attrs(&dom, img).expect("expected Some");
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
