//! Loading the actual font file named by an `@font-face` rule.
//!
//! `src: url(...)` is resolved and fetched through the same [`ImageFetcher`] as `<img>`,
//! `<link>` and `@import`. So it handles local paths (relative to the HTML file's own
//! directory), `http(s)` and `data:` URIs alike, and the same rules apply for
//! `<base href>` and for local/remote access control. `src: local(...)` is resolved from
//! [`super::system::SystemFonts`] by full name or PostScript name.

use cssparser::UnicodeRange;

use crate::img::ImageFetcher;
use crate::style::{FontFaceRule, FontFaceSource, FontStyle, FontWeight};

use super::font::{warn_font_without_outlines, Font};
use super::system::SystemFonts;

/// A font loaded from `@font-face`, plus the family name, weight, style and unicode-range declared in the CSS.
pub struct LoadedFontFace {
    pub family: String,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub unicode_range: Vec<UnicodeRange>,
    pub font: Font,
}

/// For each of `font_faces`, try the `url(...)`/`local(...)` entries listed in `src` in
/// order and take the first that loads (`format()` hints are not validated, so an
/// unsupported format such as WOFF/WOFF2 simply fails to parse and we move on to the next
/// candidate). An `@font-face` rule where no `src` loaded is warned about on standard
/// error and ignored (one missing font must not fail the whole conversion).
pub fn load_font_faces(
    font_faces: &[FontFaceRule],
    fetcher: &ImageFetcher,
    system: &SystemFonts,
) -> Vec<LoadedFontFace> {
    font_faces
        .iter()
        .filter_map(|rule| load_one(rule, fetcher, system))
        .collect()
}

fn load_one(
    rule: &FontFaceRule,
    fetcher: &ImageFetcher,
    system: &SystemFonts,
) -> Option<LoadedFontFace> {
    for src in &rule.src {
        let font = match src {
            // Loading goes through the same [`ImageFetcher`] as `<img>`, `<link>` and `@import`.
            // Classification is left to the same `resolve` (hard-coding `LocalPath` here
            // would treat a `data:` URI as a file path and always fail).
            FontFaceSource::Url(raw) => fetcher
                .resolve(raw)
                .and_then(|src| fetcher.fetch(&src).ok())
                .and_then(|bytes| Font::from_bytes(bytes, 0).ok()),
            FontFaceSource::Local(name) => system.load_by_full_name(name),
        };
        if let Some(font) = font {
            // A font that loads but has no outlines can draw nothing, so skip it and try the next src.
            if !font.has_outlines() {
                warn_font_without_outlines(&format!("the src of @font-face \"{}\"", rule.family));
                continue;
            }
            return Some(LoadedFontFace {
                family: rule.family.clone(),
                weight: rule.weight,
                style: rule.style,
                unicode_range: rule.unicode_range.clone(),
                font,
            });
        }
    }
    eprintln!(
        "warning: failed to load @font-face \"{}\" (no usable src found)",
        rule.family
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{FontStyle, FontWeight};
    use std::path::Path;

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts");

    /// Fetcher for the tests.
    /// The default matches the CLI: local reads allowed, no directory restriction.
    fn fetcher() -> ImageFetcher {
        ImageFetcher::new(Path::new(DEJAVU_PATH).to_path_buf(), false)
    }

    fn rule(family: &str, src: Vec<FontFaceSource>) -> FontFaceRule {
        FontFaceRule {
            family: family.to_string(),
            src,
            weight: FontWeight::Normal,
            style: FontStyle::Normal,
            unicode_range: Vec::new(),
        }
    }

    /// Turn a font under `tests/fonts` into a string with it embedded directly in a `data:` URI.
    fn data_uri(file_name: &str) -> String {
        use base64::Engine;

        let bytes = std::fs::read(Path::new(DEJAVU_PATH).join(file_name)).expect("test font");
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!("data:font/ttf;base64,{encoded}")
    }

    fn no_system_fonts() -> SystemFonts {
        // Scan an empty local directory to produce a state with no system fonts at all
        // (for tests that are not about local() resolution).
        SystemFonts::from_dir(Path::new(DEJAVU_PATH).join("does-not-exist").as_path())
    }

    #[test]
    fn loads_a_font_from_a_relative_url() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    #[test]
    fn resolves_a_root_relative_url_within_base_dir() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("/DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    /// A `data:` URI with the font body base64-encoded. For string input, or conversion
    /// through the HTTP server, it is the only way to make a font self-contained.
    #[test]
    fn loads_a_font_from_a_data_uri() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url(data_uri("DejaVuSansMono.ttf"))],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    /// A `data:` URI reads no file, so it still works when local reads are forbidden.
    #[test]
    fn a_data_uri_source_survives_disabled_local_file_access() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url(data_uri("DejaVuSansMono.ttf"))],
        )];
        let blocked = fetcher().with_local_access(false, Vec::new());

        let loaded = load_font_faces(&rules, &blocked, &no_system_fonts());
        assert_eq!(loaded.len(), 1);
    }

    /// `<base href>` applies to `url()` in `@font-face` too.
    #[test]
    fn resolves_a_url_source_against_base_href() {
        // Put base_dir at tests/ and let `<base href>` supply the fonts/ prefix.
        let base = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).to_path_buf();
        let based = ImageFetcher::new(base, false).with_base_href(Some("fonts/".to_string()));

        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &based, &no_system_fonts());

        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn falls_through_to_the_next_src_when_the_first_one_is_missing() {
        let rules = vec![rule(
            "Custom Brand",
            vec![
                FontFaceSource::Url("does-not-exist.ttf".to_string()),
                FontFaceSource::Url("DejaVuSans.ttf".to_string()),
            ],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    #[test]
    fn skips_a_font_face_rule_whose_sources_are_all_unusable() {
        let rules = vec![rule(
            "Missing Brand",
            vec![FontFaceSource::Url("does-not-exist.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert!(loaded.is_empty());
    }

    #[test]
    fn resolves_local_source_from_the_system_font_database() {
        let system = SystemFonts::from_dir(Path::new(DEJAVU_PATH));
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Local("DejaVu Sans".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &system);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    /// `--disable-local-file-access` applies to `url()` in `@font-face` too.
    #[test]
    fn a_url_source_is_refused_when_local_file_access_is_disabled() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let blocked = ImageFetcher::new(Path::new(DEJAVU_PATH).to_path_buf(), false)
            .with_local_access(false, Vec::new());

        let loaded = load_font_faces(&rules, &blocked, &no_system_fonts());
        assert!(
            loaded.is_empty(),
            "forbidding local reads must also block a url() font"
        );
    }

    /// A font outside the directories permitted by `--allow` must not be reachable via `..`.
    #[test]
    fn a_url_source_outside_the_allowed_dirs_is_refused() {
        let base = Path::new(DEJAVU_PATH).to_path_buf();
        // Do not permit base_dir itself, only a non-existent subdirectory under it.
        let allowed = vec![base.join("allowed-subdir")];
        let restricted = ImageFetcher::new(base, false).with_local_access(true, allowed);

        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &restricted, &no_system_fonts());
        assert!(
            loaded.is_empty(),
            "a font outside the --allow range must not be readable"
        );
    }

    #[test]
    fn falls_through_from_an_unresolvable_local_source_to_a_url() {
        let system = SystemFonts::from_dir(Path::new(DEJAVU_PATH));
        let rules = vec![rule(
            "Custom Brand",
            vec![
                FontFaceSource::Local("Definitely Not Installed".to_string()),
                FontFaceSource::Url("DejaVuSans.ttf".to_string()),
            ],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &system);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }
}
