//! Classification of `<img src>` values into URLs and paths.

use std::path::{Component, Path, PathBuf};

use base64::alphabet::STANDARD as BASE64_STANDARD_ALPHABET;
use base64::engine::general_purpose::GeneralPurposeConfig;
use base64::engine::{DecodePaddingMode, GeneralPurpose};
use base64::Engine;

/// The result of classifying an `<img src>` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImgSrc {
    /// A value treated as a local file path relative to `base_dir`.
    ///
    /// Anything that matched none of `http`/`https`/`data:` lands here.
    /// `file:` is handled separately (and rejected), so it never lands here, following
    /// the rule that a local relative path must never be confused with a URL scheme.
    LocalPath(String),
    /// An absolute `http`/`https` URL. The actual fetch is performed by the fetcher,
    /// according to its security policy.
    RemoteUrl(String),
    /// A `data:` URI. Decoded as base64 if `;base64` is present, and as percent-encoding
    /// otherwise (RFC 2397).
    ///
    /// A non-base64 payload is rare for raster images, but for SVG
    /// `data:image/svg+xml,%3Csvg...%3E` is the most common form, so both are accepted.
    /// Whether the decoded result is readable as an image is decided by
    /// `pdf::img::decode_image` from the bytes themselves.
    DataUri { mime_type: String, bytes: Vec<u8> },
}

/// Resolve `raw` (the raw reference value) against `<base href>`.
///
/// If `base` is `None`, or `raw` is an absolute reference (`http(s)`/`data:`), `raw` is
/// returned unchanged. If `base` is an absolute `http(s)` URL the two are joined as URLs;
/// otherwise `base` is treated as a directory prefix for a local path (a root-relative
/// `raw` is not prefixed in either case, since it resolves against the base's root).
pub fn resolve_against_base_href(base: Option<&str>, raw: &str) -> String {
    let raw_trimmed = raw.trim();
    let Some(base) = base.map(str::trim).filter(|b| !b.is_empty()) else {
        return raw_trimmed.to_string();
    };
    if starts_with_ignore_ascii_case(raw_trimmed, "http://")
        || starts_with_ignore_ascii_case(raw_trimmed, "https://")
        || starts_with_ignore_ascii_case(raw_trimmed, "data:")
        || starts_with_ignore_ascii_case(raw_trimmed, "file:")
    {
        return raw_trimmed.to_string();
    }

    let base_is_url = starts_with_ignore_ascii_case(base, "http://")
        || starts_with_ignore_ascii_case(base, "https://");
    if !base_is_url {
        // Prefix it as the base directory for a local path. A root-relative reference is
        // left alone, because `resolve_local_asset_path` resolves it against `base_dir`'s root.
        if raw_trimmed.starts_with('/') {
            return raw_trimmed.to_string();
        }
        let base_dir = base.trim_end_matches('/');
        if base_dir.is_empty() {
            return raw_trimmed.to_string();
        }
        return format!("{base_dir}/{raw_trimmed}");
    }

    // Protocol-relative (`//example.com/x`).
    if let Some(rest) = raw_trimmed.strip_prefix("//") {
        let scheme = base.split(':').next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    // Root-relative (`/x`) resolves against the base URL's origin.
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    if raw_trimmed.starts_with('/') {
        let origin_end = base[scheme_end..]
            .find('/')
            .map(|i| scheme_end + i)
            .unwrap_or(base.len());
        return format!("{}{raw_trimmed}", &base[..origin_end]);
    }
    // Anything else is appended to the base URL up to its last `/`.
    let dir_end = base[scheme_end..]
        .rfind('/')
        .map(|i| scheme_end + i + 1)
        .unwrap_or(base.len());
    let mut resolved = base[..dir_end].to_string();
    if !resolved.ends_with('/') {
        resolved.push('/');
    }
    resolved.push_str(raw_trimmed);
    resolved
}

/// Classify the value of a `src` attribute. Values that should never be fetched at all -
/// an undecodable `data:` URI, the `file:` scheme - return `None` (the caller treats that as a replaced element with no image).
pub fn classify_img_src(src: &str) -> Option<ImgSrc> {
    let trimmed = src.trim();

    if let Some(rest) = strip_prefix_ignore_ascii_case(trimmed, "data:") {
        return parse_data_uri(rest);
    }
    if starts_with_ignore_ascii_case(trimmed, "http://")
        || starts_with_ignore_ascii_case(trimmed, "https://")
    {
        return Some(ImgSrc::RemoteUrl(trimmed.to_string()));
    }
    if starts_with_ignore_ascii_case(trimmed, "file:") {
        return None;
    }

    Some(ImgSrc::LocalPath(trimmed.to_string()))
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value.len() >= prefix.len()
        && value.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    starts_with_ignore_ascii_case(value, prefix).then(|| &value[prefix.len()..])
}

/// Interpret what follows `data:` (not including `data:` itself) as `[<mediatype>][;base64],<data>` (RFC 2397).
fn parse_data_uri(rest: &str) -> Option<ImgSrc> {
    let (meta, data) = rest.split_once(',')?;

    let mut mime_type = String::new();
    let mut is_base64 = false;
    for (i, segment) in meta.split(';').enumerate() {
        if i == 0 {
            mime_type = segment.to_string();
        } else if segment.eq_ignore_ascii_case("base64") {
            is_base64 = true;
        }
        // Other parameters such as charset are irrelevant to embedding an image, so ignore them.
    }
    let bytes = if is_base64 {
        // Strip whitespace before decoding, so a base64 payload broken across lines is
        // accepted. Padding (`=`) is optional either way.
        let cleaned: Vec<u8> = data.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        lenient_base64().decode(cleaned).ok()?
    } else {
        percent_decode(data)
    };

    Some(ImgSrc::DataUri { mime_type, bytes })
}

/// Undo percent-encoding (`%XX`). Anything not in `%XX` form is passed through.
///
/// Tabs and newlines alone are stripped (the URL standard drops those two before parsing
/// a URL). That lets a `data:` URI wrapped across lines inside an HTML attribute or a CSS
/// `url()` be accepted without picking up stray control characters. **Spaces are kept**,
/// because in an unencoded SVG (`data:image/svg+xml,<svg ...>`) they separate tags.
fn percent_decode(data: &str) -> Vec<u8> {
    let bytes = data.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\t' | b'\r' | b'\n' => i += 1,
            // Read two hex digits byte by byte. `data` is UTF-8, so even if what follows
            // `%` is mid-way through a multi-byte character this stays safe as long as we
            // work in bytes (anything unreadable as hex passes through as a literal `%`).
            b'%' if i + 3 <= bytes.len() => {
                match (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(hi << 4 | lo);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    out
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// A standard base64 decoder that accepts both padded and unpadded input.
fn lenient_base64() -> GeneralPurpose {
    GeneralPurpose::new(
        &BASE64_STANDARD_ALPHABET,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
}

/// Resolve an [`ImgSrc::LocalPath`] (or any other local asset reference of the same kind:
/// `url()` in `@font-face`, `<link href>` and so on) into a real file path relative to
/// `base_dir`. A reference that escapes `base_dir` returns `None`.
///
/// If `raw` starts with `/` (root-relative, as in `<link href="/stylesheets/main.css" />`,
/// the usual form with the Rails asset pipeline), we read that as meaning "the site root"
/// and treat it as relative to `base_dir`. A naive `base_dir.join(raw)` would not do:
/// given an absolute argument, `Path::join` discards `base_dir` entirely (on Unix) and
/// reads from the OS filesystem root, which is both unintended and environment-dependent.
/// So the leading `/` is stripped explicitly before joining.
///
/// # Handling of `..`
///
/// `base_dir` is treated as the root, and any `..` escaping it is rejected. Otherwise,
/// converting untrusted HTML would let a reference like `<img src="../../../../etc/passwd">`
/// read outside base_dir. To reference outside it deliberately, name the range with
/// `--allow`.
///
/// The check is lexical and never touches the filesystem (so a path that does not exist
/// is judged the same way). Symlinks under base_dir are therefore followed. That is a
/// deliberate line, drawn so setups like Capistrano's `public/system` keep working; to
/// close the boundary over symlinks too, use `--allow` (which compares real paths).
pub fn resolve_local_asset_path(base_dir: &Path, raw: &str) -> ResolvedAssetPath {
    let mut parts = Vec::new();
    // How many levels we have escaped above base_dir. Without `--allow`, 1 or more is a rejection.
    let mut up = 0usize;
    let mut absolute = false;

    // Drop the leading `/` to make it "site-root relative", then fold away `.` and `..`.
    for component in Path::new(raw.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                // Nothing left to fold means we have gone above base_dir.
                if parts.pop().is_none() {
                    up += 1;
                }
            }
            // A marker of an absolute path (`/`, or `C:` on Windows). The leading `/` is
            // already stripped, so reaching here means `raw` used some other absolute form.
            Component::RootDir | Component::Prefix(_) => absolute = true,
        }
    }

    let mut path = base_dir.to_path_buf();
    for _ in 0..up {
        path.push("..");
    }
    path.extend(&parts);

    ResolvedAssetPath {
        path,
        escapes_base_dir: up > 0 || absolute,
    }
}

/// The result of [`resolve_local_asset_path`].
pub struct ResolvedAssetPath {
    /// The resolved path.
    pub path: PathBuf,
    /// Whether `..` or similar takes it outside `base_dir`.
    /// By default a reference with this set to `true` is rejected. When `--allow` is given,
    /// that decides the range instead, so the permitted directories are checked rather than this flag.
    pub escapes_base_dir: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_bare_relative_path_as_local() {
        assert_eq!(
            classify_img_src("logo.png"),
            Some(ImgSrc::LocalPath("logo.png".to_string()))
        );
    }

    #[test]
    fn classifies_dot_relative_and_absolute_local_paths() {
        assert_eq!(
            classify_img_src("./assets/logo.png"),
            Some(ImgSrc::LocalPath("./assets/logo.png".to_string()))
        );
        assert_eq!(
            classify_img_src("../images/logo.png"),
            Some(ImgSrc::LocalPath("../images/logo.png".to_string()))
        );
        assert_eq!(
            classify_img_src("/var/www/images/logo.png"),
            Some(ImgSrc::LocalPath("/var/www/images/logo.png".to_string()))
        );
    }

    #[test]
    fn classifies_http_and_https_urls_as_remote() {
        assert_eq!(
            classify_img_src("http://example.com/x.png"),
            Some(ImgSrc::RemoteUrl("http://example.com/x.png".to_string()))
        );
        assert_eq!(
            classify_img_src("HTTPS://example.com/x.png"),
            Some(ImgSrc::RemoteUrl("HTTPS://example.com/x.png".to_string())),
            "scheme comparison should be case-insensitive"
        );
    }

    #[test]
    fn rejects_the_file_scheme() {
        assert_eq!(classify_img_src("file:///etc/passwd"), None);
        assert_eq!(classify_img_src("FILE:///etc/passwd"), None);
    }

    #[test]
    fn decodes_a_base64_data_uri() {
        // The base64 of "hi".
        let src = "data:image/png;base64,aGk=";
        assert_eq!(
            classify_img_src(src),
            Some(ImgSrc::DataUri {
                mime_type: "image/png".to_string(),
                bytes: b"hi".to_vec(),
            })
        );
    }

    #[test]
    fn decodes_a_data_uri_missing_padding() {
        let src = "data:image/png;base64,aGk";
        assert_eq!(
            classify_img_src(src),
            Some(ImgSrc::DataUri {
                mime_type: "image/png".to_string(),
                bytes: b"hi".to_vec(),
            })
        );
    }

    #[test]
    fn ignores_whitespace_inside_a_data_uri_payload() {
        let src = "data:image/png;base64,\n  aGk=\n";
        assert_eq!(
            classify_img_src(src),
            Some(ImgSrc::DataUri {
                mime_type: "image/png".to_string(),
                bytes: b"hi".to_vec(),
            })
        );
    }

    /// The common way to write an SVG is `data:image/svg+xml,%3Csvg...%3E` (not base64),
    /// so a percent-encoded payload is accepted too.
    #[test]
    fn decodes_a_percent_encoded_data_uri() {
        assert_eq!(
            classify_img_src("data:text/plain,Hello%20World"),
            Some(ImgSrc::DataUri {
                mime_type: "text/plain".to_string(),
                bytes: b"Hello World".to_vec(),
            })
        );
    }

    #[test]
    fn decodes_a_percent_encoded_svg_data_uri() {
        let src =
            "data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%2F%3E";
        assert_eq!(
            classify_img_src(src),
            Some(ImgSrc::DataUri {
                mime_type: "image/svg+xml".to_string(),
                bytes: br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#.to_vec(),
            })
        );
    }

    /// An unencoded payload passes through as-is (including conventional parameters such
    /// as `;utf8,`). Spaces are meaningful, so they are not dropped.
    #[test]
    fn passes_through_an_unencoded_data_uri_payload() {
        assert_eq!(
            classify_img_src("data:image/svg+xml;utf8,<svg id='a b'/>"),
            Some(ImgSrc::DataUri {
                mime_type: "image/svg+xml".to_string(),
                bytes: b"<svg id='a b'/>".to_vec(),
            })
        );
    }

    /// Tabs and newlines in a data: URI written across lines are dropped (as the URL standard does).
    #[test]
    fn strips_tabs_and_newlines_but_not_spaces_from_a_percent_encoded_payload() {
        assert_eq!(
            classify_img_src("data:image/svg+xml,%3Csvg\n\t id='a b'%2F%3E"),
            Some(ImgSrc::DataUri {
                mime_type: "image/svg+xml".to_string(),
                bytes: b"<svg id='a b'/>".to_vec(),
            })
        );
    }

    /// A `%` not followed by two hex digits passes through as a literal `%`
    /// (a broken escape must not throw away the whole SVG).
    #[test]
    fn a_stray_percent_is_kept_verbatim() {
        assert_eq!(
            classify_img_src("data:text/plain,100% and %zz and %4"),
            Some(ImgSrc::DataUri {
                mime_type: "text/plain".to_string(),
                bytes: b"100% and %zz and %4".to_vec(),
            })
        );
    }

    /// Percent-encoding restores non-ASCII bytes too (confirming we read byte by byte
    /// rather than cutting a UTF-8 sequence in half).
    #[test]
    fn decodes_percent_escapes_of_non_ascii_bytes() {
        // "\u{3042}" = E3 81 82
        assert_eq!(
            classify_img_src("data:text/plain,%E3%81%82"),
            Some(ImgSrc::DataUri {
                mime_type: "text/plain".to_string(),
                bytes: "あ".as_bytes().to_vec(),
            })
        );
        // Unencoded non-ASCII passes through as well.
        assert_eq!(
            classify_img_src("data:text/plain,あ%20い"),
            Some(ImgSrc::DataUri {
                mime_type: "text/plain".to_string(),
                bytes: "あ い".as_bytes().to_vec(),
            })
        );
    }

    #[test]
    fn rejects_a_data_uri_with_invalid_base64_payload() {
        assert_eq!(
            classify_img_src("data:image/png;base64,not-valid-base64!!!"),
            None
        );
    }

    #[test]
    fn rejects_a_data_uri_missing_a_comma() {
        assert_eq!(classify_img_src("data:image/png;base64"), None);
    }

    /// Helper returning only references that stay inside base_dir (containment check included).
    fn within(base: &str, raw: &str) -> Option<PathBuf> {
        let resolved = resolve_local_asset_path(Path::new(base), raw);
        (!resolved.escapes_base_dir).then_some(resolved.path)
    }

    #[test]
    fn resolve_local_asset_path_joins_a_plain_relative_path() {
        assert_eq!(
            within("/var/www/app", "logo.png"),
            Some(PathBuf::from("/var/www/app/logo.png"))
        );
    }

    #[test]
    fn a_parent_reference_that_escapes_base_dir_is_flagged() {
        // `<img src="../../../../etc/passwd">` from untrusted HTML.
        let resolved =
            resolve_local_asset_path(Path::new("/var/www/app"), "../../../../etc/passwd");
        assert!(
            resolved.escapes_base_dir,
            "a reference escaping base_dir should be flagged"
        );
        // The path itself resolves plainly, so it can be checked against `--allow`.
        assert_eq!(
            resolved.path,
            Path::new("/var/www/app/../../../../etc/passwd")
        );
    }

    #[test]
    fn a_parent_reference_that_stays_inside_base_dir_is_allowed() {
        // `assets/../images/x.png` stays inside base_dir, so it is allowed.
        assert_eq!(
            within("/var/www/app", "assets/../images/x.png"),
            Some(PathBuf::from("/var/www/app/images/x.png"))
        );
    }

    #[test]
    fn stacked_parent_references_are_counted_correctly() {
        // Two stacked `..` must not collapse into one (miscounting would let it through).
        let resolved = resolve_local_asset_path(Path::new("/var/www/app"), "../../etc/passwd");
        assert!(resolved.escapes_base_dir);
        assert_eq!(resolved.path, Path::new("/var/www/app/../../etc/passwd"));
    }

    #[test]
    fn a_parent_reference_after_descending_only_escapes_when_it_goes_too_far() {
        // a/../.. escapes by one level.
        let resolved = resolve_local_asset_path(Path::new("/var/www/app"), "a/../../x");
        assert!(resolved.escapes_base_dir);
        assert_eq!(resolved.path, Path::new("/var/www/app/../x"));
    }

    #[test]
    fn resolve_local_asset_path_treats_a_leading_slash_as_relative_to_base_dir() {
        // With a naive `base_dir.join(raw)`, Path::join discards base_dir entirely when
        // handed an absolute path (on Unix). This checks that a root-relative href
        // (the `<link href="/stylesheets/main.css" />` case) does not escape base_dir
        // to the OS filesystem root.
        assert_eq!(
            within("/var/www/app", "/stylesheets/main.css"),
            Some(PathBuf::from("/var/www/app/stylesheets/main.css")),
            "a root-relative href must stay inside base_dir, not escape to the OS filesystem root"
        );
    }

    #[test]
    fn resolve_local_asset_path_strips_multiple_leading_slashes() {
        assert_eq!(
            within("/var/www/app", "//evil.example/x"),
            Some(PathBuf::from("/var/www/app/evil.example/x"))
        );
    }

    #[test]
    fn resolve_local_asset_path_normalizes_dot_relative_paths() {
        // `.` is folded away
        assert_eq!(
            within("/var/www/app", "./assets/x.css"),
            Some(PathBuf::from("/var/www/app/assets/x.css"))
        );
    }

    // ===== `<base href>` =====

    #[test]
    fn base_href_is_ignored_for_absolute_references() {
        for raw in [
            "https://cdn.example.com/a.png",
            "http://cdn.example.com/a.png",
            "data:image/png;base64,AAA",
        ] {
            assert_eq!(
                resolve_against_base_href(Some("https://example.com/docs/"), raw),
                raw
            );
        }
    }

    #[test]
    fn no_base_href_leaves_the_reference_untouched() {
        assert_eq!(resolve_against_base_href(None, "img/a.png"), "img/a.png");
        assert_eq!(
            resolve_against_base_href(Some("   "), "img/a.png"),
            "img/a.png"
        );
    }

    #[test]
    fn a_url_base_resolves_relative_references_against_its_directory() {
        assert_eq!(
            resolve_against_base_href(Some("https://example.com/docs/index.html"), "img/a.png"),
            "https://example.com/docs/img/a.png"
        );
        assert_eq!(
            resolve_against_base_href(Some("https://example.com/docs/"), "a.png"),
            "https://example.com/docs/a.png"
        );
        // A base without a trailing `/` uses the part that can be read as a directory.
        assert_eq!(
            resolve_against_base_href(Some("https://example.com/docs"), "a.png"),
            "https://example.com/a.png"
        );
    }

    #[test]
    fn a_url_base_resolves_root_relative_and_protocol_relative_references() {
        assert_eq!(
            resolve_against_base_href(Some("https://example.com/docs/index.html"), "/a.png"),
            "https://example.com/a.png"
        );
        assert_eq!(
            resolve_against_base_href(Some("https://example.com/docs/"), "//cdn.example.net/a.png"),
            "https://cdn.example.net/a.png"
        );
    }

    #[test]
    fn a_path_base_is_prepended_as_a_directory() {
        assert_eq!(
            resolve_against_base_href(Some("assets/"), "img/a.png"),
            "assets/img/a.png"
        );
        assert_eq!(
            resolve_against_base_href(Some("assets"), "a.png"),
            "assets/a.png"
        );
        // A root-relative reference is not prefixed with the base directory
        // (`resolve_local_asset_path` resolves it against `base_dir`'s root).
        assert_eq!(
            resolve_against_base_href(Some("assets/"), "/a.png"),
            "/a.png"
        );
    }

    #[test]
    fn a_resolved_relative_reference_classifies_as_a_remote_url() {
        let resolved = resolve_against_base_href(Some("https://example.com/docs/"), "a.png");
        assert_eq!(
            classify_img_src(&resolved),
            Some(ImgSrc::RemoteUrl(
                "https://example.com/docs/a.png".to_string()
            ))
        );
    }
}
