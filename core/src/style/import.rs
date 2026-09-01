//! Detecting and recursively expanding `@import` statements (text preprocessing before parsing).
//!
//! Rather than bringing I/O into `parse_stylesheet` itself, this is implemented as a
//! string-level expansion of the CSS text before parsing. Detection does not strictly
//! validate the CSS rule that `@import` may only appear at the top; a statement is detected
//! and expanded wherever it appears.
//! The fetched content is spliced in exactly where the `@import` statement was: a genuine
//! in-place substitution, not a hoist to the top.

use std::ops::Range;

use cssparser::{Delimiter, Parser, ParserInput, Token};

use crate::img::{DocumentImageCache, ImageFetcher};

/// Recursion depth limit, to guard against import cycles. Deciding a visited set through URL
/// normalisation is not worth the cost here, so a simple depth limit stands in for it.
const MAX_IMPORT_DEPTH: u32 = 16;

struct ImportStatement {
    href: String,
    /// Byte range of the whole statement (from `@import` to the terminating `;`) in the original css.
    range: Range<usize>,
}

/// Detect the `@import` statements in `css` and return the CSS text with them recursively
/// expanded from what was fetched. An `@import` that fails to fetch or decode, or that
/// exceeds [`MAX_IMPORT_DEPTH`], is skipped on its own with a warning on standard error,
/// and processing continues (the same policy as images and external stylesheets).
pub fn resolve_imports(
    css: &str,
    fetcher: &ImageFetcher,
    cache: &DocumentImageCache,
    depth: u32,
) -> String {
    let imports = find_imports(css);
    if imports.is_empty() {
        return css.to_string();
    }

    let mut result = String::with_capacity(css.len());
    let mut cursor = 0usize;

    for import in &imports {
        result.push_str(&css[cursor..import.range.start]);
        cursor = import.range.end;

        if depth >= MAX_IMPORT_DEPTH {
            eprintln!(
                "warning: ignoring an @import nested too deeply (limit {MAX_IMPORT_DEPTH} levels): {}",
                import.href
            );
            continue;
        }

        match cache.get_or_fetch(fetcher, &import.href) {
            Ok(bytes) => match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    result.push_str(&resolve_imports(text, fetcher, cache, depth + 1));
                    result.push('\n');
                }
                Err(_) => eprintln!(
                    "warning: the CSS fetched by @import is not valid UTF-8: {}",
                    import.href
                ),
            },
            Err(e) => eprintln!("warning: @import failed to fetch: {}: {e}", import.href),
        }
    }
    result.push_str(&css[cursor..]);
    result
}

/// Detect the `@import` statements in `css` by scanning tokens (no I/O).
fn find_imports(css: &str) -> Vec<ImportStatement> {
    let mut input = ParserInput::new(css);
    let mut parser = Parser::new(&mut input);
    let mut found = Vec::new();

    loop {
        let start_state = parser.state();
        match parser.next_including_whitespace_and_comments() {
            Ok(Token::AtKeyword(name)) if name.eq_ignore_ascii_case("import") => {
                // Even with a media query (`@import url(...) screen;`), only the href is
                // taken and the media part is discarded, treating it as an unconditional
                // import (`@media` itself is out of scope).
                let href = parser
                    .parse_until_before::<_, _, ()>(Delimiter::Semicolon, |input| {
                        input
                            .expect_url_or_string()
                            .map(|s| s.as_ref().to_string())
                            .map_err(|_| input.new_custom_error(()))
                    })
                    .ok();
                let _ = parser.next(); // Skip to the terminating `;` (if there is one).
                let end = parser.position().byte_index();
                if let Some(href) = href {
                    found.push(ImportStatement {
                        href,
                        range: start_state.position().byte_index()..end,
                    });
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn no_remote_fetcher() -> ImageFetcher {
        ImageFetcher::new(PathBuf::from("."), false)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-style-import-test-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn leaves_css_without_import_unchanged() {
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();
        let css = "p { color: red; }";
        assert_eq!(resolve_imports(css, &fetcher, &cache, 0), css);
    }

    #[test]
    fn splices_imported_content_in_place() {
        let dir = temp_dir("splices_in_place");
        std::fs::write(dir.join("other.css"), b"p { color: blue; }").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        let css = r#"a { color: green; } @import url("other.css"); div { color: red; }"#;
        let expanded = resolve_imports(css, &fetcher, &cache, 0);

        let a_pos = expanded.find("a {").unwrap();
        let p_pos = expanded.find("p {").unwrap();
        let div_pos = expanded.find("div {").unwrap();
        assert!(
            a_pos < p_pos && p_pos < div_pos,
            "imported content should be spliced exactly where the @import statement was, \
             not hoisted to the front: {expanded:?}"
        );
        assert!(!expanded.contains("@import"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn recursively_expands_nested_imports() {
        let dir = temp_dir("nested");
        std::fs::write(
            dir.join("a.css"),
            br#"@import url("b.css"); a { color: red; }"#,
        )
        .unwrap();
        std::fs::write(dir.join("b.css"), b"b { color: blue; }").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        let css = r#"@import url("a.css");"#;
        let expanded = resolve_imports(css, &fetcher, &cache, 0);
        assert!(expanded.contains("b { color: blue; }"));
        assert!(expanded.contains("a { color: red; }"));
        assert!(!expanded.contains("@import"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_circular_import_is_guarded_by_the_depth_limit_without_hanging() {
        let dir = temp_dir("circular");
        std::fs::write(
            dir.join("a.css"),
            br#"@import url("b.css"); a { color: red; }"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("b.css"),
            br#"@import url("a.css"); b { color: blue; }"#,
        )
        .unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);
        let cache = DocumentImageCache::new();

        // Check that it does not recurse forever but stops at MAX_IMPORT_DEPTH.
        let expanded = resolve_imports(r#"@import url("a.css");"#, &fetcher, &cache, 0);
        assert!(expanded.contains("a { color: red; }"));
        assert!(expanded.contains("b { color: blue; }"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_import_is_skipped_without_panicking() {
        let fetcher = no_remote_fetcher();
        let cache = DocumentImageCache::new();
        let css = r#"@import url("does-not-exist.css"); p { color: red; }"#;
        let expanded = resolve_imports(css, &fetcher, &cache, 0);
        assert_eq!(expanded.trim(), "p { color: red; }");
    }
}
