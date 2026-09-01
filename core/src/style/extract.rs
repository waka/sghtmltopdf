//! Assembling the author stylesheet from the `<style>` and `<link rel=stylesheet>`
//! elements in the DOM.
//!
//! Extracting `style="..."` attributes (inline styles) is not handled here.
//!
//! DOM traversal (no I/O, [`collect_css_sources`]) is kept separate from href resolution
//! (which does I/O). Every CSS source, inline and external alike, is concatenated in
//! document order before `parse_stylesheet` is called once, so a relative `url()` inside a
//! fetched external stylesheet always resolves against the original HTML's `base_dir`
//! (a per-stylesheet base is not supported).
//!
//! Each CSS source's text has its `@import`s expanded recursively by [`resolve_imports`]
//! before concatenation. `parse_stylesheet` itself knows nothing about `@import` (it is
//! ignored by cssparser's error recovery), so a direct call that bypasses
//! `extract_author_stylesheet` still leaves `@import` unexpanded and simply ignored.
//!
//! After concatenation and before parsing, [`substitute_custom_properties`] resolves CSS
//! Custom Properties (`--foo`/`var`) by text substitution (treating the whole document as
//! one flat namespace across `<style>` and `<link>`).

use crate::html::{is_stylesheet_link, Dom, NodeData, NodeId};
use crate::img::{DocumentImageCache, ImageFetcher};

use super::custom_properties::substitute_custom_properties;
use super::import::resolve_imports;
use super::stylesheet::{parse_stylesheet, Stylesheet};

/// One CSS source in the DOM (in document order).
#[derive(Debug, Clone, PartialEq, Eq)]
enum CssSource {
    /// The text content of a `<style>` element.
    Inline(String),
    /// The href of a `<link rel=stylesheet href="...">` (the raw, unresolved value).
    External(String),
}

/// Concatenate the CSS of every `<style>` and `<link rel=stylesheet>` in the DOM,
/// preserving document order, and parse it.
///
/// When fetching an external stylesheet (`<link>`) fails - a network error, an SSRF block,
/// a non-2xx response, invalid UTF-8; all treated alike - only that stylesheet is ignored,
/// with a warning on standard error, and processing continues (a broken or blocked URL
/// must not stop the whole document).
pub fn extract_author_stylesheet(
    dom: &Dom,
    fetcher: &ImageFetcher,
    cache: &DocumentImageCache,
) -> Stylesheet {
    let mut css = String::new();
    for source in collect_css_sources(dom) {
        match source {
            CssSource::Inline(text) => {
                css.push_str(&resolve_imports(&text, fetcher, cache, 0));
                css.push('\n');
            }
            CssSource::External(href) => match cache.get_or_fetch(fetcher, &href) {
                Ok(bytes) => match std::str::from_utf8(&bytes) {
                    Ok(text) => {
                        css.push_str(&resolve_imports(text, fetcher, cache, 0));
                        css.push('\n');
                    }
                    Err(_) => {
                        eprintln!(
                            "warning: the external stylesheet was fetched but is not valid UTF-8: {href}"
                        );
                    }
                },
                Err(e) => {
                    eprintln!("warning: failed to fetch an external stylesheet: {href}: {e}");
                }
            },
        }
    }
    parse_stylesheet(&substitute_custom_properties(&css))
}

/// Walk the DOM tree once and enumerate either "inline CSS text" or "a `<link>` href",
/// preserving document order. Does no I/O at all (a pure DOM walk).
fn collect_css_sources(dom: &Dom) -> Vec<CssSource> {
    let mut sources = Vec::new();
    collect_css_sources_rec(dom, dom.document(), &mut sources);
    sources
}

fn collect_css_sources_rec(dom: &Dom, node: NodeId, out: &mut Vec<CssSource>) {
    if let NodeData::Element { name, attrs, .. } = &dom.node(node).data {
        if &*name.local == "style" {
            let mut text = String::new();
            for child in dom.children(node) {
                if let NodeData::Text { contents } = &dom.node(child).data {
                    text.push_str(contents);
                    text.push('\n');
                }
            }
            out.push(CssSource::Inline(text));
            return;
        }
        if &*name.local == "link" && is_stylesheet_link(attrs) {
            let href = attrs
                .iter()
                .find(|attr| &*attr.name.local == "href")
                .map(|attr| attr.value.to_string())
                .filter(|s| !s.is_empty());
            if let Some(href) = href {
                out.push(CssSource::External(href));
            }
            return; // <link> is a void element (it has no children).
        }
    }
    for child in dom.children(node) {
        collect_css_sources_rec(dom, child, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;
    use std::path::PathBuf;

    fn no_remote_fetcher() -> ImageFetcher {
        ImageFetcher::new(PathBuf::from("."), false)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-style-extract-test-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn extracts_and_parses_style_tag_contents() {
        let dom = html::parse(
            br#"<html><head><style>p { color: rgb(1, 2, 3); }</style></head>
                <body><p>text</p></body></html>"#,
        );
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();
        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 1);
    }

    #[test]
    fn combines_multiple_style_tags() {
        let dom = html::parse(
            br#"<html><head>
                <style>p { color: rgb(1, 2, 3); }</style>
                <style>div { color: rgb(4, 5, 6); }</style>
                </head><body></body></html>"#,
        );
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();
        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 2);
    }

    #[test]
    fn returns_empty_stylesheet_when_no_style_tags() {
        let dom = html::parse(b"<p>text</p>");
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();
        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn fetches_and_parses_a_local_external_stylesheet() {
        let dir = temp_dir("fetches_local");
        std::fs::write(dir.join("main.css"), b"p { color: rgb(1, 2, 3); }").unwrap();
        let dom = html::parse(
            br#"<html><head><link rel="stylesheet" href="main.css"></head><body></body></html>"#,
        );
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 1);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn preserves_document_order_between_link_and_style() {
        // So that later-wins cascade order is preserved, the order in which <link> and
        // <style> appear (here <link> first) should survive concatenation and parsing.
        use super::super::values::SpecifiedLength;
        use crate::style::PropertyDeclaration;

        let dir = temp_dir("preserves_order");
        std::fs::write(dir.join("main.css"), b"p { font-size: 11px; }").unwrap();
        let dom = html::parse(
            br#"<html><head>
                <link rel="stylesheet" href="main.css">
                <style>p { font-size: 22px; }</style>
                </head><body></body></html>"#,
        );
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 2);

        let font_size_px = |decls: &[PropertyDeclaration]| match decls.first() {
            Some(PropertyDeclaration::FontSize(SpecifiedLength::Px(px))) => *px,
            other => panic!("expected a single font-size: Px(_) declaration, got {other:?}"),
        };
        assert_eq!(
            font_size_px(&sheet.rules[0].declarations),
            11.0,
            "the <link> (appearing first) should parse first"
        );
        assert_eq!(
            font_size_px(&sheet.rules[1].declarations),
            22.0,
            "the <style> (appearing second) should parse second, so it wins the cascade"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_external_stylesheet_is_skipped_without_panicking() {
        let dom = html::parse(
            br#"<html><head><link rel="stylesheet" href="does-not-exist.css"></head>
                <body></body></html>"#,
        );
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn ignores_a_link_that_is_not_a_stylesheet() {
        let dom = html::parse(
            br#"<html><head><link rel="icon" href="favicon.ico"></head><body></body></html>"#,
        );
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn ignores_a_stylesheet_link_with_no_href() {
        let dom = html::parse(br#"<html><head><link rel="stylesheet"></head><body></body></html>"#);
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert!(sheet.rules.is_empty());
    }

    #[test]
    fn expands_at_import_inside_a_style_tag() {
        let dir = temp_dir("import_in_style_tag");
        std::fs::write(dir.join("imported.css"), b"p { color: rgb(1, 2, 3); }").unwrap();
        let dom = html::parse(
            br#"<html><head><style>@import url("imported.css"); div { color: rgb(4, 5, 6); }</style></head>
                <body></body></html>"#,
        );
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 2);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expands_at_import_inside_a_fetched_external_stylesheet() {
        let dir = temp_dir("import_in_external");
        std::fs::write(
            dir.join("main.css"),
            br#"@import url("base.css"); p { color: rgb(1, 2, 3); }"#,
        )
        .unwrap();
        std::fs::write(dir.join("base.css"), b"div { color: rgb(4, 5, 6); }").unwrap();
        let dom = html::parse(
            br#"<html><head><link rel="stylesheet" href="main.css"></head><body></body></html>"#,
        );
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 2);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_at_import_is_skipped_without_failing_the_whole_stylesheet() {
        let dom = html::parse(
            br#"<html><head><style>@import url("does-not-exist.css"); p { color: rgb(1, 2, 3); }</style></head>
                <body></body></html>"#,
        );
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();

        let sheet = extract_author_stylesheet(&dom, &fetcher, &cache);
        assert_eq!(sheet.rules.len(), 1);
    }
}
