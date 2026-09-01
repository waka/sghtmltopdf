//! E2E tests for the fonts used by `<text>` inside an SVG.
//!
//! The requirement is that "a font usable in the HTML document is usable inside the
//! translated PDF chunk too". The SVG translation (svg2pdf) has its own font resolution
//! machinery (usvg's `fontdb`) separate from this engine's, so without handing the
//! document's fonts over, the text **appears in HTML but vanishes inside the SVG**.
//! [`SvgFontDb`] is that bridge, and both ends of it are checked here.
//!
//! How it is checked: the fonts svg2pdf embeds are named `/BaseFont /TAG+FamilyName`
//! (a six-character subset tag, a `+`, then the family name). This engine's own font output
//! is `/BaseFont /EmbeddedFont`, with no tag.
//! So the presence of a `/BaseFont` containing a `+` tells us whether the fonts reached the
//! SVG side and were embedded.
//!
//! Without the `svg-text` feature, text inside an SVG is not drawn (and rustybuzz, resvg and
//! the rest are not pulled in). That behaviour is checked here too.

#![cfg(feature = "svg")]

use std::path::PathBuf;
use std::process::Command;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::pdf::SvgFontDb;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const BOLD_FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/DejaVuSans-Bold.ttf"
);
const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");

/// The family name `DejaVuSans.ttf` gives in its `name` table.
const INTERNAL_FAMILY: &str = "DejaVu Sans";

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Collect every `/BaseFont /...` value in the PDF.
fn base_font_names(pdf: &[u8]) -> Vec<String> {
    const KEY: &[u8] = b"/BaseFont /";
    let mut names = Vec::new();
    let mut i = 0;
    while i + KEY.len() <= pdf.len() {
        if !pdf[i..].starts_with(KEY) {
            i += 1;
            continue;
        }
        let start = i + KEY.len();
        let end = pdf[start..]
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || *b == b'+' || *b == b'-'))
            .map_or(pdf.len(), |n| start + n);
        names.push(String::from_utf8_lossy(&pdf[start..end]).into_owned());
        i = end;
    }
    names
}

/// The names of the fonts svg2pdf embedded (the ones carrying a subset tag `TAG+`).
/// This engine's own output is `EmbeddedFont` with no tag, so it does not get mixed in.
///
/// `/BaseFont` is written in both the Type0 dictionary and the CIDFont dictionary, so one
/// font appears twice. Duplicates are folded to return "how many distinct fonts were embedded".
fn svg_embedded_fonts(pdf: &[u8]) -> Vec<String> {
    let mut names: Vec<String> = base_font_names(pdf)
        .into_iter()
        .filter(|name| name.contains('+'))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The number of embedded font programs (`/FontFile2`). One per subset.
fn embedded_font_programs(pdf: &[u8]) -> usize {
    count_occurrences(pdf, b"/FontFile2")
}

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sghtmltopdf-svgfont-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
    }

    #[cfg_attr(not(feature = "svg-text"), allow(dead_code))]
    fn copy_font(&self, from: &str, to: &str) {
        std::fs::copy(from, self.dir.join(to)).unwrap();
    }

    /// With `svg-text` disabled only the tests looking at stderr remain, so this is unused.
    #[cfg_attr(not(feature = "svg-text"), allow(dead_code))]
    fn convert(&self, extra: &[&str]) -> Vec<u8> {
        self.convert_capturing_stderr(extra).0
    }

    /// The conversion result plus the stderr from it (for checking the warnings).
    fn convert_capturing_stderr(&self, extra: &[&str]) -> (Vec<u8>, String) {
        let output = self.dir.join("out.pdf");
        let result = Command::new(BIN)
            .arg(self.dir.join("in.html"))
            .args(extra)
            .arg("-o")
            .arg(&output)
            .output()
            .expect("failed to run the sghtmltopdf binary");
        assert!(
            result.status.success(),
            "the conversion should succeed, stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let pdf = std::fs::read(&output).expect("output PDF should exist");
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(
            count_occurrences(&pdf, b"/Subtype /Form") > 0,
            "the SVG itself should always be embedded, whatever happens to its text"
        );
        (pdf, String::from_utf8_lossy(&result.stderr).into_owned())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

/// An SVG with one `<text>`. With `family` as `None`, no `font-family` is written.
fn svg_with_text(family: Option<&str>) -> String {
    let family_attr = family
        .map(|f| format!(r#" font-family="{f}""#))
        .unwrap_or_default();
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="40">
              <rect width="200" height="40" fill="#eeeeee"/>
              <text x="5" y="25"{family_attr} font-size="16" fill="#000000">Hamburgefonstiv</text>
            </svg>"##
    )
}

// ===== `SvgFontDb` (the bridge itself) =====

/// A database built from the document's font collection contains the collection's fonts.
/// With `svg-text` disabled it is empty (text inside an SVG is not drawn).
#[test]
fn the_font_db_is_built_from_the_documents_font_collection() {
    let collection = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load the test font"),
        Font::load(BOLD_FONT_PATH).expect("should load the bold test font"),
    ]);
    let db = SvgFontDb::from_collection(&collection);

    if cfg!(feature = "svg-text") {
        assert_eq!(
            db.len(),
            collection.len(),
            "every font available to the document should be available to the SVG"
        );
    } else {
        assert!(
            db.is_empty(),
            "without `svg-text` the SVG font db stays empty"
        );
    }
}

#[test]
fn an_empty_font_db_has_no_faces() {
    assert!(SvgFontDb::empty().is_empty());
    assert_eq!(SvgFontDb::empty().len(), 0);
}

/// Building one from a document with no fonts merely gives an empty database; it does not panic.
#[test]
fn an_empty_collection_produces_an_empty_font_db() {
    let db = SvgFontDb::from_collection(&FontCollection::new(Vec::new()));
    assert!(db.is_empty());
}

// ===== With `svg-text` enabled: the document's fonts reach the SVG =====

/// A font passed with `--font` can be looked up from inside the SVG by its internal family name.
#[cfg(feature = "svg-text")]
#[test]
fn a_font_given_to_the_document_is_usable_from_the_svg_by_its_internal_family_name() {
    let fx = Fixture::new("internal-name");
    fx.write("logo.svg", &svg_with_text(Some(INTERNAL_FAMILY)));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    let pdf = fx.convert(&["--font", FONT_PATH]);

    let embedded = svg_embedded_fonts(&pdf);
    assert!(
        embedded.iter().any(|name| name.contains("DejaVuSans")),
        "the document's font should be embedded for the SVG's text, got {embedded:?}"
    );
}

/// A `<text>` with no `font-family` is drawn in the document's default font rather than
/// usvg's default ("Times New Roman"). Making a locally absent name the default would
/// guarantee that unstyled text disappears.
#[cfg(feature = "svg-text")]
#[test]
fn text_without_a_font_family_falls_back_to_the_documents_font() {
    let fx = Fixture::new("default-family");
    fx.write("logo.svg", &svg_with_text(None));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    let pdf = fx.convert(&["--font", FONT_PATH]);

    let embedded = svg_embedded_fonts(&pdf);
    assert!(
        embedded.iter().any(|name| name.contains("DejaVuSans")),
        "text with no font-family should use the document's font, got {embedded:?}"
    );
}

/// A name declared with `@font-face` can be looked up from the SVG too. The font's internal
/// `name` table says `DejaVu Sans`, so the declared name `BrandFace` cannot resolve unless it
/// is registered as an alias.
#[cfg(feature = "svg-text")]
#[test]
fn a_font_face_declared_family_name_is_usable_from_the_svg() {
    let fx = Fixture::new("declared-name");
    fx.copy_font(FONT_PATH, "brand.ttf");
    fx.write("logo.svg", &svg_with_text(Some("BrandFace")));
    fx.write(
        "in.html",
        r#"<style>
             @font-face { font-family: BrandFace; src: url(brand.ttf); }
             body { margin: 0; font-family: BrandFace; }
           </style>
           <body><img src="logo.svg" width="200" height="40"></body>"#,
    );
    // `--font` is not passed. The only font reachable from the SVG is the `@font-face` one.
    let pdf = fx.convert(&[]);

    let embedded = svg_embedded_fonts(&pdf);
    assert!(
        !embedded.is_empty(),
        "the @font-face font should be embedded for the SVG's text, got {embedded:?}"
    );
}

/// A family the document does not have is not silently substituted with some other font:
/// it is drawn in the **document's default font**.
///
/// usvg's default selection function appends `Family::Serif` to the candidates. fontdb's
/// default for `serif` is "Times New Roman", which this engine does not have, so without
/// pointing the generic families at the document the text would silently disappear.
/// "Drawn in the document's font" beats "disappears", and matches the HTML-side behaviour.
#[cfg(feature = "svg-text")]
#[test]
fn a_family_the_document_does_not_have_falls_back_to_the_documents_font() {
    let fx = Fixture::new("unknown-family");
    fx.write(
        "logo.svg",
        &svg_with_text(Some("NoSuchFamilyExistsAnywhere")),
    );
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    let pdf = fx.convert(&["--font", FONT_PATH]);

    let embedded = svg_embedded_fonts(&pdf);
    assert!(
        embedded.iter().any(|name| name.contains("DejaVuSans")),
        "an unknown family should fall back to the document's font, got {embedded:?}"
    );
    // The substitute is always a font the document has. No system font is searched for, so a
    // font absent from the document can never creep in.
    assert_eq!(
        embedded.len(),
        1,
        "only the document's own font should be embedded, got {embedded:?}"
    );
}

/// A generic family inside an SVG (`serif`/`sans-serif`/`monospace`) resolves to the same
/// font as on the HTML side. Passing `--mono-font` makes the SVG's `monospace` that too.
#[cfg(feature = "svg-text")]
#[test]
fn generic_families_inside_an_svg_resolve_to_the_documents_generic_fonts() {
    let fx = Fixture::new("generic-mono");
    fx.write("logo.svg", &svg_with_text(Some("monospace")));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    // Pass different files for the default font and for `monospace`, and see which was used.
    let pdf = fx.convert(&["--font", FONT_PATH, "--mono-font", BOLD_FONT_PATH]);

    let embedded = svg_embedded_fonts(&pdf);
    assert!(
        embedded.iter().any(|name| name.contains("Bold")),
        "`monospace` in the SVG should use --mono-font, got {embedded:?}"
    );
}

/// With nothing given for a generic family it falls back to the default font
/// (it does not go looking for "Times New Roman" and disappear).
#[cfg(feature = "svg-text")]
#[test]
fn an_unset_generic_family_falls_back_to_the_documents_font() {
    let fx = Fixture::new("generic-serif");
    fx.write("logo.svg", &svg_with_text(Some("serif")));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    // `--serif-font` is not passed.
    let pdf = fx.convert(&["--font", FONT_PATH]);

    let embedded = svg_embedded_fonts(&pdf);
    assert!(
        embedded.iter().any(|name| name.contains("DejaVuSans")),
        "`serif` with no --serif-font should fall back to the document's font, got {embedded:?}"
    );
}

/// The document-side and SVG-side font embeddings happen independently.
/// Even for the same font file, the subsets differ (they need different glyphs).
#[cfg(feature = "svg-text")]
#[test]
fn the_svg_text_font_is_embedded_alongside_the_documents_own_font() {
    let fx = Fixture::new("both-embedded");
    fx.write("logo.svg", &svg_with_text(Some(INTERNAL_FAMILY)));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><p>document text</p>
             <img src="logo.svg" width="200" height="40"></body>"#,
    );
    let pdf = fx.convert(&["--font", FONT_PATH]);

    // Both the document side (`/BaseFont /EmbeddedFont`) and the SVG side (tagged) are present.
    let names = base_font_names(&pdf);
    assert!(
        names.iter().any(|n| n == "EmbeddedFont"),
        "the document's own text should still embed its font, got {names:?}"
    );
    assert!(
        !svg_embedded_fonts(&pdf).is_empty(),
        "the SVG's text should embed its own subset, got {names:?}"
    );
    assert!(
        embedded_font_programs(&pdf) >= 2,
        "two independent subsets should be embedded"
    );
}

/// Referencing the same SVG several times embeds the font only once
/// (it is shared along with the SVG's chunk).
#[cfg(feature = "svg-text")]
#[test]
fn a_repeated_svg_does_not_embed_its_font_twice() {
    let one = Fixture::new("repeat-one");
    one.write("logo.svg", &svg_with_text(Some(INTERNAL_FAMILY)));
    one.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    let single = one.convert(&["--font", FONT_PATH]);

    let three = Fixture::new("repeat-three");
    three.write("logo.svg", &svg_with_text(Some(INTERNAL_FAMILY)));
    three.write(
        "in.html",
        r#"<body style="margin:0">
             <img src="logo.svg" width="200" height="40">
             <img src="logo.svg" width="200" height="40">
             <img src="logo.svg" width="200" height="40">
           </body>"#,
    );
    let repeated = three.convert(&["--font", FONT_PATH]);

    assert_eq!(
        svg_embedded_fonts(&repeated).len(),
        1,
        "the shared SVG chunk should carry exactly one font subset"
    );
    assert_eq!(
        embedded_font_programs(&repeated),
        embedded_font_programs(&single),
        "referencing the same SVG three times should not embed its font program again"
    );
}

/// The same for the streaming writer (a chunk for an SVG containing fonts has even more
/// objects, so it is also a path where the xref breaks easily).
#[cfg(feature = "svg-text")]
#[test]
fn streaming_mode_also_embeds_the_documents_font_for_svg_text() {
    let fx = Fixture::new("streaming");
    fx.write("logo.svg", &svg_with_text(Some(INTERNAL_FAMILY)));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    let pdf = fx.convert(&["--font", FONT_PATH, "--streaming"]);

    assert!(
        !svg_embedded_fonts(&pdf).is_empty(),
        "streaming mode should embed the SVG's font too"
    );
}

// ===== With `svg-text` disabled =====

/// The text is not drawn, but the SVG's other shapes are and the conversion succeeds.
/// Disappearing silently is hard to diagnose, so it warns (usvg's and svg2pdf's warnings go
/// through the `log` crate and vanish in this crate, which configures no logger).
#[cfg(not(feature = "svg-text"))]
#[test]
fn without_the_svg_text_feature_the_text_is_dropped_but_the_svg_still_renders() {
    let fx = Fixture::new("no-text-feature");
    fx.write("logo.svg", &svg_with_text(Some(INTERNAL_FAMILY)));
    fx.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    // `convert` confirms the presence of `/Subtype /Form` (that is, the rectangle is drawn).
    let (pdf, stderr) = fx.convert_capturing_stderr(&["--font", FONT_PATH]);

    assert!(
        svg_embedded_fonts(&pdf).is_empty(),
        "without `svg-text` no font should be embedded for the SVG"
    );
    assert!(
        stderr.contains("<text>") && stderr.contains("svg-text"),
        "dropping the text should be reported, got: {stderr}"
    );

    // The number of embedded font programs is unchanged compared with the same-sized SVG
    // containing no text (that is, `<text>` added nothing).
    let plain = Fixture::new("no-text-feature-plain");
    plain.write(
        "logo.svg",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="40">
              <rect width="200" height="40" fill="#eeeeee"/>
            </svg>"##,
    );
    plain.write(
        "in.html",
        r#"<body style="margin:0"><img src="logo.svg" width="200" height="40"></body>"#,
    );
    let (plain_pdf, plain_stderr) = plain.convert_capturing_stderr(&["--font", FONT_PATH]);
    assert_eq!(
        embedded_font_programs(&pdf),
        embedded_font_programs(&plain_pdf),
        "a dropped <text> should not embed a font program"
    );
    // An SVG with no `<text>` does not warn (warning too much makes it meaningless).
    assert!(
        !plain_stderr.contains("<text>"),
        "an SVG with no text should not warn, got: {plain_stderr}"
    );
}
