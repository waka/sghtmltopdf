//! E2E tests for path and URL resolution of SVG references.
//!
//! An SVG goes through the same `img::fetch` as a raster image, so containment (the base
//! directory, `--allow`, `--disable-local-file-access`) and whether remote fetching is
//! allowed should be shared. Without really checking that "should be", an SVG being readable
//! through some other path would go unnoticed. This concerns reading, so the **refused**
//! cases are covered as thoroughly as the permitted ones.
//!
//! Format identification is done from the bytes (never the extension or the declared mime
//! type), which is checked here too.

#![cfg(feature = "svg")]

use std::path::PathBuf;
use std::process::Command;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const PNG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_opaque.png"
);
const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");

/// A minimal 20x10 SVG: one blue rectangle.
const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
  <rect width="20" height="10" fill="#0000ff"/>
</svg>"##;

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// Whether the SVG was embedded as vectors (that is, became a Form XObject).
fn embedded_as_vector(pdf: &[u8]) -> bool {
    count_occurrences(pdf, b"/Subtype /Form") > 0
}

/// Whether it was embedded as a raster image.
fn embedded_as_raster(pdf: &[u8]) -> bool {
    count_occurrences(pdf, b"/Subtype /Image") > 0
}

/// The working directory for one test.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sghtmltopdf-svgpath-{}-{name}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        self.write_bytes(relative, contents.as_bytes())
    }

    fn write_bytes(&self, relative: &str, contents: &[u8]) -> PathBuf {
        let path = self.dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.dir.join(relative)
    }

    /// Convert `html` (a relative path under this directory) to PDF.
    fn convert(&self, html: &str, extra: &[&str]) -> Outcome {
        let output = self.dir.join("out.pdf");
        let result = Command::new(BIN)
            .arg(self.dir.join(html))
            .args(["--font", FONT_PATH])
            .args(extra)
            .arg("-o")
            .arg(&output)
            .output()
            .expect("failed to run the sghtmltopdf binary");
        Outcome {
            success: result.status.success(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
            pdf: std::fs::read(&output).unwrap_or_default(),
        }
    }

    /// The text of the reason it was refused. With `--load-media-error-handling abort` the
    /// reason it could not be fetched comes out as the error itself, so that is what is read
    /// (the default `ignore` carries on silently and shows no reason).
    fn refusal_reason(&self, html: &str, extra: &[&str]) -> String {
        let mut args = extra.to_vec();
        args.extend(["--load-media-error-handling", "abort"]);
        let outcome = self.convert(html, &args);
        assert!(
            !outcome.success,
            "abort should fail the conversion for a refused reference, stderr: {}",
            outcome.stderr
        );
        outcome.stderr
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

struct Outcome {
    success: bool,
    stderr: String,
    pdf: Vec<u8>,
}

impl Outcome {
    /// The conversion succeeds and the SVG is in as vectors.
    fn assert_rendered(&self) {
        assert!(
            self.success,
            "the conversion should succeed, stderr: {}",
            self.stderr
        );
        assert!(self.pdf.starts_with(b"%PDF-"));
        assert!(
            embedded_as_vector(&self.pdf),
            "the SVG should be embedded as a form XObject, stderr: {}",
            self.stderr
        );
    }

    /// The conversion itself succeeds, but the SVG is neither read nor drawn.
    ///
    /// The default on a failed fetch is `--load-media-error-handling ignore`, which silently
    /// becomes an empty replaced element as with a raster image (the whole document carries
    /// on). Use [`Fixture::refusal_reason`] to check the reason too.
    fn assert_refused(&self) {
        assert!(
            self.success,
            "a refused reference should not fail the whole conversion, stderr: {}",
            self.stderr
        );
        assert!(self.pdf.starts_with(b"%PDF-"));
        assert!(
            !embedded_as_vector(&self.pdf),
            "a refused SVG must not end up in the PDF, stderr: {}",
            self.stderr
        );
    }
}

fn html_with(src: &str) -> String {
    format!(r#"<body style="margin:0"><img src="{src}"></body>"#)
}

// ===== Inside the base directory =====

#[test]
fn a_plain_relative_reference_resolves_against_the_documents_directory() {
    let fx = Fixture::new("relative");
    fx.write("logo.svg", SVG);
    fx.write("in.html", &html_with("logo.svg"));
    fx.convert("in.html", &[]).assert_rendered();
}

#[test]
fn a_reference_into_a_subdirectory_resolves() {
    let fx = Fixture::new("subdir");
    fx.write("assets/logo.svg", SVG);
    fx.write("in.html", &html_with("assets/logo.svg"));
    fx.convert("in.html", &[]).assert_rendered();
}

/// A `..` is allowed as long as it stays inside the base directory.
#[test]
fn a_parent_reference_that_stays_inside_the_base_directory_resolves() {
    let fx = Fixture::new("inside-parent");
    fx.write("logo.svg", SVG);
    fx.write("assets/in.html", &html_with("../logo.svg"));
    // base_dir is the directory holding the input HTML (assets/), so `../logo.svg` goes
    // outside it. `--allow` names the base directory explicitly.
    fx.convert("assets/in.html", &["--allow", fx.dir.to_str().unwrap()])
        .assert_rendered();
}

#[test]
fn a_dot_segment_inside_the_base_directory_resolves() {
    let fx = Fixture::new("dot-segment");
    fx.write("images/logo.svg", SVG);
    fx.write("in.html", &html_with("assets/../images/logo.svg"));
    fx.convert("in.html", &[]).assert_rendered();
}

/// A root-relative path (`/logo.svg`) resolves against the "site root", that is the base
/// directory (it does not read from the OS filesystem root).
#[test]
fn a_root_relative_reference_is_resolved_against_the_base_directory() {
    let fx = Fixture::new("root-relative");
    fx.write("logo.svg", SVG);
    fx.write("in.html", &html_with("/logo.svg"));
    fx.convert("in.html", &[]).assert_rendered();
}

#[test]
fn base_href_prefixes_a_relative_reference() {
    let fx = Fixture::new("base-href");
    fx.write("assets/logo.svg", SVG);
    fx.write(
        "in.html",
        r#"<head><base href="assets/"></head><body style="margin:0"><img src="logo.svg"></body>"#,
    );
    fx.convert("in.html", &[]).assert_rendered();
}

// ===== Outside the base directory, and access control =====

/// A reference such as `<img src="../../secret.svg">` is refused by default.
#[test]
fn a_reference_that_escapes_the_base_directory_is_refused() {
    let fx = Fixture::new("escape");
    fx.write("secret.svg", SVG);
    fx.write("public/in.html", &html_with("../secret.svg"));
    fx.convert("public/in.html", &[]).assert_refused();
    let reason = fx.refusal_reason("public/in.html", &[]);
    assert!(
        reason.contains("base directory"),
        "the reason should name the containment, got: {reason}"
    );
}

#[test]
fn stacked_parent_segments_do_not_slip_past_the_containment() {
    let fx = Fixture::new("stacked-escape");
    fx.write("secret.svg", SVG);
    fx.write("public/deep/in.html", &html_with("../../../secret.svg"));
    fx.convert("public/deep/in.html", &[]).assert_refused();
}

/// Inside a directory named with `--allow`, it is readable even outside the base directory.
#[test]
fn allow_permits_a_reference_outside_the_base_directory() {
    let fx = Fixture::new("allow");
    let outside = fx.write("outside/logo.svg", SVG);
    fx.write("public/in.html", &html_with("../outside/logo.svg"));
    fx.convert(
        "public/in.html",
        &["--allow", outside.parent().unwrap().to_str().unwrap()],
    )
    .assert_rendered();
}

/// Even with `--allow`, anything outside the named directory is unreadable.
#[test]
fn allow_does_not_permit_a_reference_outside_the_allowed_directory() {
    let fx = Fixture::new("allow-elsewhere");
    fx.write("secret/logo.svg", SVG);
    let permitted = fx.path("other");
    std::fs::create_dir_all(&permitted).unwrap();
    fx.write("public/in.html", &html_with("../secret/logo.svg"));
    let allow = ["--allow", permitted.to_str().unwrap()];
    fx.convert("public/in.html", &allow).assert_refused();
    let reason = fx.refusal_reason("public/in.html", &allow);
    assert!(
        reason.contains("--allow"),
        "the reason should mention --allow, got: {reason}"
    );
}

/// `--disable-local-file-access` refuses even a reference inside the base directory
/// (the default in server mode).
#[test]
fn disable_local_file_access_refuses_even_a_reference_inside_the_base_directory() {
    let fx = Fixture::new("no-local");
    fx.write("logo.svg", SVG);
    fx.write("in.html", &html_with("logo.svg"));
    let flag = ["--disable-local-file-access"];
    fx.convert("in.html", &flag).assert_refused();
    let reason = fx.refusal_reason("in.html", &flag);
    assert!(
        reason.contains("local files"),
        "the reason should say local file access is off, got: {reason}"
    );
}

/// Remote fetching is disabled by default. It is refused without ever going to the network.
#[test]
fn a_remote_reference_is_refused_unless_remote_assets_are_allowed() {
    let fx = Fixture::new("remote");
    fx.write("in.html", &html_with("https://example.invalid/logo.svg"));
    fx.convert("in.html", &[]).assert_refused();
    let reason = fx.refusal_reason("in.html", &[]);
    assert!(
        reason.contains("--allow-remote-assets"),
        "the reason should point at the opt-in flag, got: {reason}"
    );
}

/// For when a failed fetch should fail the whole document.
#[test]
fn load_media_error_handling_abort_fails_the_conversion_for_an_unreachable_svg() {
    let fx = Fixture::new("abort");
    fx.write("public/in.html", &html_with("../missing.svg"));
    let outcome = fx.convert("public/in.html", &["--load-media-error-handling", "abort"]);
    assert!(
        !outcome.success,
        "abort should make an unresolvable SVG fail the conversion, stderr: {}",
        outcome.stderr
    );
}

// ===== Format identification is done from the bytes =====

/// The extension is ignored. A `.txt` whose contents are SVG is drawn as SVG.
#[test]
fn the_extension_does_not_decide_the_format() {
    let fx = Fixture::new("ext-svg");
    fx.write("logo.txt", SVG);
    fx.write("in.html", &html_with("logo.txt"));
    fx.convert("in.html", &[]).assert_rendered();
}

/// And the reverse. A `.svg` whose contents are PNG is drawn as a raster image.
#[test]
fn a_png_named_svg_is_still_embedded_as_a_raster_image() {
    let fx = Fixture::new("png-named-svg");
    let png = std::fs::read(PNG_PATH).expect("fixture image should exist");
    fx.write_bytes("logo.svg", &png);
    fx.write("in.html", &html_with("logo.svg"));
    let outcome = fx.convert("in.html", &[]);
    assert!(outcome.success, "stderr: {}", outcome.stderr);
    assert!(
        embedded_as_raster(&outcome.pdf),
        "PNG bytes should be embedded as an image XObject regardless of the .svg name"
    );
    assert!(
        !embedded_as_vector(&outcome.pdf),
        "PNG bytes must not be run through the SVG path"
    );
}

/// A `data:` URI takes the same path (the bytes decide). It works even with the wrong declared mime type.
#[test]
fn a_data_uri_svg_is_rendered_even_when_the_declared_mime_type_is_wrong() {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;

    let fx = Fixture::new("data-uri");
    let encoded = STANDARD.encode(SVG);
    // Deliberately declared as `image/png`.
    fx.write(
        "in.html",
        &html_with(&format!("data:image/png;base64,{encoded}")),
    );
    fx.convert("in.html", &[]).assert_rendered();
}

/// Percent-encode as `%XX` (a minimal implementation for the tests, escaping everything
/// outside RFC 3986's unreserved set).
fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// An SVG data URI is normally written percent-encoded rather than with `;base64`.
/// Accepting only base64 would reject this, the most ordinary form.
#[test]
fn a_percent_encoded_svg_data_uri_is_rendered() {
    let fx = Fixture::new("data-uri-percent");
    fx.write(
        "in.html",
        &html_with(&format!("data:image/svg+xml,{}", percent_encode(SVG))),
    );
    fx.convert("in.html", &[]).assert_rendered();
}

/// Even with a conventional parameter such as `;utf8,`, it is read as percent-encoded when
/// there is no `;base64`.
#[test]
fn a_data_uri_with_a_charset_style_parameter_but_no_base64_is_rendered() {
    let fx = Fixture::new("data-uri-utf8");
    fx.write(
        "in.html",
        &html_with(&format!("data:image/svg+xml;utf8,{}", percent_encode(SVG))),
    );
    fx.convert("in.html", &[]).assert_rendered();

    let charset = Fixture::new("data-uri-charset");
    charset.write(
        "in.html",
        &html_with(&format!(
            "data:image/svg+xml;charset=utf-8,{}",
            percent_encode(SVG)
        )),
    );
    charset.convert("in.html", &[]).assert_rendered();
}

/// The same inside a CSS `url()` (`background-image` takes the same path as `<img src>`).
#[test]
fn a_percent_encoded_svg_data_uri_works_in_a_css_url() {
    let fx = Fixture::new("data-uri-css");
    fx.write(
        "in.html",
        &format!(
            r#"<body style="margin:0"><div style="width:60px;height:30px;
                 background-image:url('data:image/svg+xml,{}');
                 background-repeat:no-repeat"></div></body>"#,
            percent_encode(SVG)
        ),
    );
    fx.convert("in.html", &[]).assert_rendered();
}

/// Raw, unencoded SVG works too (which also confirms whitespace is not dropped: dropping it
/// would give `<svgxmlns=...>`, which cannot be parsed). Quotes clash inside an HTML
/// attribute, so the attribute is single-quoted.
#[test]
fn an_unencoded_svg_data_uri_is_rendered() {
    let fx = Fixture::new("data-uri-raw");
    fx.write(
        "in.html",
        &format!(r#"<body style="margin:0"><img src='data:image/svg+xml,{SVG}'></body>"#),
    );
    fx.convert("in.html", &[]).assert_rendered();
}

/// A gzip-compressed SVG (`.svgz`). It is sniffed by the magic bytes `1f 8b` and the
/// decompression is left to usvg.
#[test]
fn a_gzipped_svgz_is_rendered() {
    use std::io::Write;

    let fx = Fixture::new("svgz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(SVG.as_bytes()).unwrap();
    let gzipped = encoder.finish().unwrap();
    assert_eq!(&gzipped[..2], &[0x1F, 0x8B], "fixture should be gzip");

    fx.write_bytes("logo.svgz", &gzipped);
    fx.write("in.html", &html_with("logo.svgz"));
    fx.convert("in.html", &[]).assert_rendered();
}

/// `background-image: url(...)` goes through the same resolution and containment as `<img src>`.
#[test]
fn a_background_image_reference_uses_the_same_containment() {
    let fx = Fixture::new("background");
    fx.write("secret.svg", SVG);
    fx.write(
        "public/in.html",
        r#"<body style="margin:0"><div style="width:60px;height:30px;
             background-image:url(../secret.svg);background-repeat:no-repeat"></div></body>"#,
    );
    fx.convert("public/in.html", &[]).assert_refused();

    let ok = Fixture::new("background-ok");
    ok.write("logo.svg", SVG);
    ok.write(
        "in.html",
        r#"<body style="margin:0"><div style="width:60px;height:30px;
             background-image:url(logo.svg);background-repeat:no-repeat"></div></body>"#,
    );
    ok.convert("in.html", &[]).assert_rendered();
}

/// Referencing the same SVG two different ways still fetches it once, as long as they
/// resolve to the same file. (The `src` string is the key, so a different spelling makes a
/// separate entry. What is checked here is the "same string, one fetch" side.)
#[test]
fn the_same_reference_is_fetched_once() {
    let fx = Fixture::new("dedup");
    fx.write("logo.svg", SVG);
    fx.write(
        "in.html",
        r#"<body style="margin:0">
             <img src="logo.svg"><img src="logo.svg"><img src="logo.svg">
           </body>"#,
    );
    let outcome = fx.convert("in.html", &[]);
    outcome.assert_rendered();
    assert_eq!(
        count_occurrences(&outcome.pdf, b"/Subtype /Form"),
        1,
        "three references to the same file should share one form XObject"
    );
}

/// Pointing at a directory must not bring the whole conversion down.
#[test]
fn a_reference_to_a_directory_is_refused_without_crashing() {
    let fx = Fixture::new("dir-ref");
    std::fs::create_dir_all(fx.path("logo.svg")).unwrap();
    fx.write("in.html", &html_with("logo.svg"));
    fx.convert("in.html", &[]).assert_refused();
}

/// An empty file cannot be interpreted as SVG either. It warns and carries on.
#[test]
fn an_empty_file_is_refused_without_crashing() {
    let fx = Fixture::new("empty");
    fx.write("logo.svg", "");
    fx.write("in.html", &html_with("logo.svg"));
    fx.convert("in.html", &[]).assert_refused();
}
