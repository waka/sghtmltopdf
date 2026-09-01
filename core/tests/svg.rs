//! E2E tests for `<img src="*.svg">` and `background-image: url(*.svg)`.
//!
//! Unlike a raster image, an SVG goes into the PDF as a cluster of several objects: one Form
//! XObject plus what it references. So there are mainly two things to confirm:
//!
//! 1. It goes in as vectors without rasterisation (becoming a Form XObject rather than an
//!    Image XObject, with path drawing operators appearing)
//! 2. Splicing in several objects does not break the xref
//!
//! The second is exercised through both writers. The library's `encode_pdf` fixes up the
//! offsets with `Chunk::extend`, but the path that writes to a `Sink`
//! (`StreamingPdfWriter`, which the CLI uses with or without `--streaming`) builds the xref
//! itself and has to count each object's position within the chunk.
//!
//! There is no pixel comparison of the drawing result (that is svg2pdf's job). All that is
//! checked here is whether this engine joins the SVG into the PDF's structure correctly.

#![cfg(feature = "svg")]

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{
    paginate_document_with_absolutes, resolve_background_images, PageSettings,
};
use sghtmltopdf_core::pdf::{encode_pdf, ImageAssetCache};
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");

/// A 20x10 SVG. Fills and strokes only, depending on neither fonts nor raster images.
const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
  <rect x="0" y="0" width="20" height="10" fill="#0000ff"/>
  <circle cx="10" cy="5" r="4" fill="#ff0000" stroke="#00ff00" stroke-width="1"/>
</svg>"##;

/// An SVG with a gradient and `opacity`. svg2pdf's chunk then contains Shading, Pattern,
/// ExtGState, an ICCBased stream and a nested Form XObject, and **the object numbering no
/// longer matches the order in the byte string** (the ICC profile referenced from inside the
/// gradient is numbered earlier but written at the end of the chunk). That is the case most
/// likely to break the code counting the xref offsets, so it is here deliberately.
const SVG_WITH_GRADIENT: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 60" width="100" height="60">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#ff5500"/>
      <stop offset="1" stop-color="#0055ff"/>
    </linearGradient>
  </defs>
  <rect x="2" y="2" width="96" height="56" rx="8" fill="url(#g)" stroke="#222" stroke-width="2"/>
  <circle cx="30" cy="30" r="16" fill="#fff" opacity="0.7"/>
  <path d="M60 12 L88 48 L60 48 Z" fill="#0a0" stroke="#050" stroke-width="2"/>
</svg>"##;

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

fn find_from(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| from + at)
}

/// The content stream is FlateDecode compressed, so finding an operator requires inflating
/// first (the same logic as the identically named function in `tests/background.rs`).
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_from(pdf_bytes, b"stream\n", i) {
        let start = pos + b"stream\n".len();
        let Some(end) = find_from(pdf_bytes, b"\nendstream", start) else {
            break;
        };
        let raw = &pdf_bytes[start..end];

        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        } else {
            out.extend_from_slice(raw);
        }
        i = end + b"\nendstream".len();
    }
    out
}

/// Confirm that every xref table entry really points at the start of its object. Splicing in
/// an SVG adds many objects at once, and one wrong offset makes the whole PDF unreadable.
///
/// It also checks that `/Size` matches the entry count (that is, that everything from 1
/// upwards is written). The streaming writer's xref is built on that assumption.
fn assert_xref_is_consistent(pdf: &[u8]) {
    let startxref = rfind(pdf, b"startxref").expect("PDF should end with startxref");
    let xref_offset: usize = ascii_number_after(pdf, startxref + b"startxref".len())
        .expect("startxref should be followed by an offset");
    assert!(
        pdf[xref_offset..].starts_with(b"xref"),
        "startxref should point at the xref table"
    );

    // After `xref\n0 {size}\n` come the entries, 20 bytes per line.
    let header_end = find_from(pdf, b"\n", xref_offset + b"xref".len() + 1)
        .expect("xref subsection header should end with a newline");
    let subsection = std::str::from_utf8(&pdf[xref_offset + b"xref\n".len()..header_end])
        .expect("xref subsection header should be ASCII");
    let (first, size) = subsection
        .split_once(' ')
        .expect("xref subsection header should be `first count`");
    assert_eq!(first, "0", "the subsection should start at object 0");
    let size: usize = size.trim().parse().expect("count should be a number");

    // An entry is a fixed 20 bytes (`nnnnnnnnnn ggggg t` plus a 2-byte line ending).
    // The first is object 0's free entry. The real objects are 1..size.
    let entries_start = header_end + 1;
    let mut in_use = 0;
    for id in 1..size {
        let entry_at = entries_start + id * 20;
        let entry = std::str::from_utf8(&pdf[entry_at..entry_at + 20])
            .unwrap_or_else(|_| panic!("xref entry for object {id} should be ASCII"));
        // An unused number becomes an `f` (free) entry. `encode_pdf` can leave numbers
        // allocated but unused, so only the `n` entries are checked.
        if entry.as_bytes()[17] == b'f' {
            continue;
        }
        assert_eq!(
            entry.as_bytes()[17],
            b'n',
            "xref entry for object {id} should be marked `n` or `f`, got {entry:?}"
        );
        in_use += 1;
        let offset: usize = entry[..10]
            .parse()
            .unwrap_or_else(|_| panic!("xref entry for object {id} should start with an offset"));
        let expected = format!("{id} 0 obj");
        assert!(
            pdf[offset..].starts_with(expected.as_bytes()),
            "xref says object {id} is at {offset}, but that is not where `{expected}` starts \
             (found {:?})",
            String::from_utf8_lossy(&pdf[offset..(offset + 24).min(pdf.len())])
        );
    }

    // The total number of `N 0 obj` in the file matches the number of `n` entries
    // (that is, no object is missing from the xref).
    let written = (1..size)
        .filter(|id| count_occurrences(pdf, format!("\n{id} 0 obj\n").as_bytes()) > 0)
        .count();
    assert_eq!(
        written, in_use,
        "every object written to the file should have an in-use xref entry"
    );
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

fn ascii_number_after(bytes: &[u8], from: usize) -> Option<usize> {
    let rest = &bytes[from..];
    let start = rest.iter().position(|b| b.is_ascii_digit())?;
    let end = rest[start..]
        .iter()
        .position(|b| !b.is_ascii_digit())
        .map_or(rest.len(), |i| start + i);
    std::str::from_utf8(&rest[start..end]).ok()?.parse().ok()
}

/// Write the HTML and the SVG into a temporary directory and convert to PDF through the CLI.
/// Passing `--streaming` in `extra` selects streaming page settling.
fn convert(html: &str, extra: &[&str], name: &str) -> Vec<u8> {
    convert_svg_file(html, SVG, extra, name)
}

fn convert_svg_file(html: &str, svg: &str, extra: &[&str], name: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-svg-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("logo.svg"), svg).unwrap();
    let input = dir.join("input.html");
    std::fs::write(&input, html).unwrap();
    let output = dir.join("out.pdf");

    let result = Command::new(BIN)
        .arg(&input)
        .args(["--font", FONT_PATH])
        .args(extra)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("failed to run the sghtmltopdf binary");
    assert!(
        result.status.success(),
        "CLI should succeed, stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !stderr.contains("warning:"),
        "converting an SVG should not warn, got: {stderr}"
    );

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    cleanup(&dir);
    bytes
}

fn cleanup(dir: &Path) {
    std::fs::remove_dir_all(dir).ok();
}

/// The sign that the SVG went in as vectors. Rasterised, it would be `/Subtype /Image` with
/// neither `/Subtype /Form` nor any path operator.
fn assert_embedded_as_vector(pdf: &[u8]) {
    assert!(
        count_occurrences(pdf, b"/Subtype /Form") > 0,
        "an SVG should become a form XObject"
    );
    assert_eq!(
        count_occurrences(pdf, b"/Subtype /Image"),
        0,
        "an SVG must not be rasterised into an image XObject"
    );

    let content = decompressed_stream_bytes(pdf);
    assert!(
        count_occurrences(&content, b" c\n") > 0 || count_occurrences(&content, b" c ") > 0,
        "the circle should be drawn as Bezier curves in a content stream"
    );
    assert!(
        count_occurrences(&content, b" Do\n") > 0,
        "the form XObject should be invoked with Do"
    );
}

#[test]
fn an_img_pointing_at_an_svg_is_embedded_as_vector_graphics() {
    let pdf = convert(
        r#"<body style="margin:0"><img src="logo.svg"></body>"#,
        &[],
        "img",
    );
    assert_embedded_as_vector(&pdf);
    assert_xref_is_consistent(&pdf);
}

/// With no dimensions on the `<img>` it is laid out at the SVG's intrinsic size (20x10).
/// Under `--zoom 1` and the default scale of 0.75, a 20px-wide box appears as the `cm`
/// `20 0 0 10`.
#[test]
fn an_svg_without_attributes_lays_out_at_its_intrinsic_size() {
    let pdf = convert(
        r#"<body style="margin:0"><img src="logo.svg"></body>"#,
        &[],
        "intrinsic",
    );
    let content = decompressed_stream_bytes(&pdf);
    let text = String::from_utf8_lossy(&content);
    assert!(
        text.contains("20 0 0 10 "),
        "the unit-square form XObject should be scaled to the SVG's intrinsic 20x10, \
         content was: {text}"
    );
}

/// Dimensions given by attribute win over the intrinsic size (being normalised to the unit
/// square, it scales with the `cm` alone, just like a raster).
#[test]
fn width_and_height_attributes_scale_the_svg() {
    let pdf = convert(
        r#"<body style="margin:0"><img src="logo.svg" width="100" height="50"></body>"#,
        &[],
        "scaled",
    );
    let content = decompressed_stream_bytes(&pdf);
    let text = String::from_utf8_lossy(&content);
    assert!(
        text.contains("100 0 0 50 "),
        "the form XObject should be scaled to 100x50, content was: {text}"
    );
}

// ===== object-fit / object-position =====

/// A 40x10 SVG. The ratio is chosen so `object-fit`'s different horizontal and vertical effects show.
const WIDE_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="10">
  <rect width="40" height="10" fill="#0000ff"/>
</svg>"##;

/// An SVG with a **fractional** intrinsic size of 40.6 x 10.4. Rounding to integers gives
/// 41x10, changing the ratio from 3.904 to 4.100, about 5%. `object-fit` is decided by that
/// ratio, so rounding would fail here.
const FRACTIONAL_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 406 104" width="40.6" height="10.4">
  <rect width="406" height="104" fill="#0000ff"/>
</svg>"##;

/// Extract the `cm` (`a b c d e f cm`) immediately before a Form XObject is drawn.
/// That is the nearest `cm` before the `Do`.
fn xobject_cm(pdf: &[u8]) -> [f32; 6] {
    let content = decompressed_stream_bytes(pdf);
    let text = String::from_utf8_lossy(&content).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    let draw_at = lines
        .iter()
        .position(|line| line.ends_with(" Do"))
        .unwrap_or_else(|| panic!("no `Do` in the content stream: {text}"));
    let cm = lines[..draw_at]
        .iter()
        .rev()
        .find(|line| line.ends_with(" cm"))
        .unwrap_or_else(|| panic!("no `cm` before the `Do`: {text}"));
    let values: Vec<f32> = cm
        .trim_end_matches(" cm")
        .split_whitespace()
        .map(|v| v.parse().expect("cm operands should be numbers"))
        .collect();
    values.try_into().expect("a cm has 6 operands")
}

#[track_caller]
fn assert_close(actual: f32, expected: f32, what: &str) {
    assert!(
        (actual - expected).abs() < 0.05,
        "{what}: expected about {expected}, got {actual}"
    );
}

/// For each of `object-fit`'s five values, the drawn rectangle follows the spec.
/// A 40x10 SVG (a 4:1 ratio) goes into a 100x50 box.
#[test]
fn object_fit_scales_an_svg_the_same_way_it_scales_a_raster_image() {
    // (value, expected width, expected height)
    let cases = [
        // Stretched to fill the box (the ratio is not preserved).
        ("fill", 100.0, 50.0),
        // The width fills first: 100 / 4 = 25.
        ("contain", 100.0, 25.0),
        // The height fills first: 50 * 4 = 200.
        ("cover", 200.0, 50.0),
        // The intrinsic size, unchanged.
        ("none", 40.0, 10.0),
        // The intrinsic size fits the box, so it is the same as `none`.
        ("scale-down", 40.0, 10.0),
    ];
    for (fit, width, height) in cases {
        let html = format!(
            r#"<body style="margin:0"><img src="logo.svg"
                 style="width:100px;height:50px;object-fit:{fit}"></body>"#
        );
        let pdf = convert_svg_file(&html, WIDE_SVG, &[], &format!("fit-{fit}"));
        let cm = xobject_cm(&pdf);
        assert_close(cm[0], width, &format!("object-fit: {fit} width"));
        assert_close(cm[3], height, &format!("object-fit: {fit} height"));
    }
}

/// Even an SVG with a fractional intrinsic size preserves its aspect ratio. Rounding would
/// make `contain`'s height 24.4 rather than 25.6 (a 5% error in the ratio).
#[test]
fn object_fit_keeps_a_fractional_intrinsic_aspect_ratio() {
    let ratio = 40.6 / 10.4;

    for (fit, expect) in [
        ("contain", (100.0, 100.0 / ratio)),
        ("cover", (50.0 * ratio, 50.0)),
        ("none", (40.6, 10.4)),
    ] {
        let html = format!(
            r#"<body style="margin:0"><img src="logo.svg"
                 style="width:100px;height:50px;object-fit:{fit}"></body>"#
        );
        let pdf = convert_svg_file(&html, FRACTIONAL_SVG, &[], &format!("frac-{fit}"));
        let cm = xobject_cm(&pdf);
        assert_close(cm[0], expect.0, &format!("object-fit: {fit} width"));
        assert_close(cm[3], expect.1, &format!("object-fit: {fit} height"));
    }
}

/// `scale-down` shrinks only when the intrinsic size is larger than the box
/// (and is then the same as `contain`).
#[test]
fn object_fit_scale_down_shrinks_only_when_the_svg_is_larger_than_the_box() {
    let html = r#"<body style="margin:0"><img src="logo.svg"
         style="width:20px;height:20px;object-fit:scale-down"></body>"#;
    let pdf = convert_svg_file(html, WIDE_SVG, &[], "scale-down-large");
    let cm = xobject_cm(&pdf);
    // Fitting 40x10 into 20x20 fills the width first: 20 x 5.
    assert_close(cm[0], 20.0, "scale-down width");
    assert_close(cm[3], 5.0, "scale-down height");
}

/// `object-position` moves where the fitted rectangle sits. Going from the default (50% 50%)
/// to `0% 0%` moves it to the top left.
#[test]
fn object_position_moves_the_svg_within_the_content_box() {
    let centred = convert_svg_file(
        r#"<body style="margin:0"><img src="logo.svg"
             style="width:100px;height:50px;object-fit:contain"></body>"#,
        WIDE_SVG,
        &[],
        "pos-centre",
    );
    let top_left = convert_svg_file(
        r#"<body style="margin:0"><img src="logo.svg"
             style="width:100px;height:50px;object-fit:contain;object-position:0% 0%"></body>"#,
        WIDE_SVG,
        &[],
        "pos-topleft",
    );

    let (c, tl) = (xobject_cm(&centred), xobject_cm(&top_left));
    // The size does not change.
    assert_close(tl[0], c[0], "width should not change with object-position");
    assert_close(tl[3], c[3], "height should not change with object-position");
    // A rectangle 25px shorter is moved to the top, so in PDF coordinates (origin at the bottom) y goes up.
    assert_close(tl[5] - c[5], 12.5, "object-position: 0% 0% should raise it");
}

/// `object-fit: cover` overflows, so it is clipped to the content box
/// (the clip is written as the sequence `re W n`).
#[test]
fn object_fit_cover_is_clipped_to_the_content_box() {
    let pdf = convert_svg_file(
        r#"<body style="margin:0"><img src="logo.svg"
             style="width:100px;height:50px;object-fit:cover"></body>"#,
        WIDE_SVG,
        &[],
        "cover-clip",
    );
    let content = decompressed_stream_bytes(&pdf);
    let text = String::from_utf8_lossy(&content);
    // The content box's rectangle, then `W` (a nonzero clip), then `n` (end without drawing the path).
    assert!(
        text.contains("100 50 re\nW\nn\n"),
        "the content box should be set as a clip path, content was: {text}"
    );
    // It is drawn at a width that overflows (without the clip it would spill onto the page).
    assert_close(xobject_cm(&pdf)[0], 200.0, "cover width");
}

/// With only `width` given, the height follows from the fractional intrinsic ratio.
#[test]
fn a_single_specified_dimension_derives_the_other_from_the_exact_ratio() {
    let pdf = convert_svg_file(
        r#"<body style="margin:0"><img src="logo.svg" style="width:203px"></body>"#,
        FRACTIONAL_SVG,
        &[],
        "derive-height",
    );
    let cm = xobject_cm(&pdf);
    // 203 / (40.6/10.4) = 52. The rounded 41x10 ratio would give 49.5.
    assert_close(cm[0], 203.0, "width");
    assert_close(cm[3], 52.0, "height derived from the exact ratio");
}

#[test]
fn an_svg_works_as_a_background_image() {
    let pdf = convert(
        r#"<body style="margin:0">
             <div style="width:60px;height:30px;background-image:url(logo.svg);
                         background-repeat:no-repeat"></div>
           </body>"#,
        &[],
        "background",
    );
    assert_embedded_as_vector(&pdf);
    assert_xref_is_consistent(&pdf);
}

/// The streaming writer builds the xref itself, so this confirms the offsets do not shift
/// when an SVG's several objects go in (that check is this file's main purpose).
#[test]
fn streaming_mode_writes_a_consistent_xref_with_an_svg() {
    let pdf = convert(
        r#"<body style="margin:0"><img src="logo.svg"></body>"#,
        &["--streaming"],
        "streaming",
    );
    assert_embedded_as_vector(&pdf);
    assert_xref_is_consistent(&pdf);
}

/// However many times the same SVG is referenced, only one Form XObject is written
/// (the same per-`Rc` memoisation as a raster image).
#[test]
fn the_same_svg_referenced_twice_is_embedded_once() {
    let pdf = convert(
        r#"<body style="margin:0">
             <img src="logo.svg"><img src="logo.svg"><img src="logo.svg">
           </body>"#,
        &[],
        "dedup",
    );
    assert_eq!(
        count_occurrences(&pdf, b"/Subtype /Form"),
        1,
        "three references to the same SVG should share one form XObject"
    );
    assert_xref_is_consistent(&pdf);
}

/// In an SVG with a gradient the object numbering and the byte order disagree (see the
/// comment on [`SVG_WITH_GRADIENT`]). This confirms the `Sink` path's xref is still built
/// correctly.
#[test]
fn a_gradient_svg_keeps_the_xref_consistent_when_object_order_is_not_monotonic() {
    for (mode, name) in [
        (&[][..], "gradient-batch"),
        (&["--streaming"][..], "gradient-streaming"),
    ] {
        let pdf = convert_svg_file(
            r#"<body style="margin:0"><img src="logo.svg" width="300" height="180"></body>"#,
            SVG_WITH_GRADIENT,
            mode,
            name,
        );
        assert_embedded_as_vector(&pdf);
        assert_xref_is_consistent(&pdf);
        // The sign that the gradient went in as vectors (as a Shading).
        assert!(
            count_occurrences(&pdf, b"/ShadingType") > 0,
            "the linear gradient should stay a PDF shading, not be flattened, in {name}"
        );
    }
}

/// The library's `encode_pdf` takes a different writing path from the CLI
/// (`Chunk::extend`), so it is checked independently too.
#[test]
fn encode_pdf_embeds_an_svg_with_a_consistent_xref() {
    let svg_data_uri = format!(
        "data:image/svg+xml;base64,{}",
        STANDARD.encode(SVG_WITH_GRADIENT)
    );
    let html_src = r#"<body style="margin:0"><img src="PLACEHOLDER"></body>"#
        .replace("PLACEHOLDER", &svg_data_uri);

    let mut dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("");
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load the test font")
    ]);
    let settings = PageSettings::default();

    // Only data URIs are used, so base_dir can be anything (no I/O happens).
    let image_cache = ImageAssetCache::new(PathBuf::from("."), false);
    let background_images = resolve_background_images(&styles, &image_cache);
    let pages =
        paginate_document_with_absolutes(&mut dom, &styles, &fonts, &settings, &image_cache);
    let pdf = encode_pdf(&pages, &styles, &background_images, &fonts, &settings);

    assert!(pdf.starts_with(b"%PDF-"));
    assert_embedded_as_vector(&pdf);
    assert_xref_is_consistent(&pdf);
}

/// Do not let an `<image href="...">` inside an SVG read a file.
///
/// usvg's default resolver calls `std::fs::read` on the href directly, which would bypass
/// the `<img>`-side containment (the base directory, `--allow`,
/// `--disable-local-file-access`). `pdf::svg` replaces it, so this confirms the reference is
/// refused and none of what it would read reaches the PDF.
#[test]
fn an_svg_cannot_read_files_through_a_nested_image_href() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-svg-{}-exfil", std::process::id()));
    let public = dir.join("public");
    std::fs::create_dir_all(&public).unwrap();

    // A "secret" SVG placed outside base_dir. A magenta fill (`1 0 1 rg`) appearing in the
    // PDF would mean the contents leaked.
    std::fs::write(
        dir.join("secret.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="20">
              <rect width="100" height="20" fill="#ff00ff"/>
            </svg>"##,
    )
    .unwrap();

    let evil = format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="60">
             <rect width="200" height="60" fill="#dddddd"/>
             <image href="../secret.svg" x="0" y="0" width="100" height="20"/>
             <image href="{}" x="0" y="30" width="100" height="20"/>
             <image href="/etc/passwd" x="100" y="0" width="50" height="20"/>
           </svg>"##,
        dir.join("secret.svg").display()
    );
    std::fs::write(public.join("evil.svg"), evil).unwrap();
    let input = public.join("input.html");
    std::fs::write(&input, r#"<body><img src="evil.svg"></body>"#).unwrap();
    let output = public.join("out.pdf");

    let result = Command::new(BIN)
        .arg(&input)
        .args(["--font", FONT_PATH])
        .arg("-o")
        .arg(&output)
        .output()
        .expect("failed to run the sghtmltopdf binary");
    assert!(result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("external references inside an SVG are not loaded"),
        "each nested href should be refused with a warning, got: {stderr}"
    );

    let pdf = std::fs::read(&output).unwrap();
    let content = decompressed_stream_bytes(&pdf);
    assert_eq!(
        count_occurrences(&content, b"1 0 1 rg"),
        0,
        "the referenced SVG's magenta fill must not appear in the PDF"
    );
    assert_eq!(
        count_occurrences(&pdf, b"root:"),
        0,
        "/etc/passwd must not appear in the PDF"
    );
    // Even with the reference refused, the SVG itself (the grey background) is drawn.
    assert!(count_occurrences(&pdf, b"/Subtype /Form") > 0);

    cleanup(&dir);
}

// ===== Inline SVG (unsupported) =====

/// An `<svg>` written directly in the HTML is not drawn. `<img src="*.svg">` can be drawn,
/// so rather than silently producing nothing, it warns.
#[test]
fn an_inline_svg_is_not_rendered_and_says_so() {
    for (mode, name) in [
        (&[][..], "inline-batch"),
        (&["--streaming"][..], "inline-streaming"),
    ] {
        let dir =
            std::env::temp_dir().join(format!("sghtmltopdf-svg-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("input.html");
        std::fs::write(
            &input,
            r##"<body style="margin:0"><p>before</p>
                 <svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
                   <rect width="40" height="20" fill="#ff0000"/>
                   <text x="2" y="14">INLINE</text>
                 </svg>
                 <p>after</p></body>"##,
        )
        .unwrap();
        let output = dir.join("out.pdf");

        let result = Command::new(BIN)
            .arg(&input)
            .args(["--font", FONT_PATH])
            .args(mode)
            .arg("-o")
            .arg(&output)
            .output()
            .expect("failed to run the sghtmltopdf binary");
        assert!(result.status.success(), "conversion should still succeed");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            stderr.contains("<svg> element") && stderr.contains("not drawn"),
            "an inline <svg> should be reported, got: {stderr}"
        );

        let pdf = std::fs::read(&output).unwrap();
        assert_eq!(
            count_occurrences(&pdf, b"/Subtype /Form"),
            0,
            "an inline <svg> must not produce a form XObject in {name}"
        );
        // The whole subtree is removed, so the `<text>` inside never flows into the body either.
        let content = decompressed_stream_bytes(&pdf);
        assert_eq!(
            count_occurrences(&content, b"INLINE"),
            0,
            "the inline SVG's text must not leak into the page in {name}"
        );
        cleanup(&dir);
    }
}

/// The warning appears once per document (so a document making heavy use of inline SVG does
/// not fill up with the same warning).
#[test]
fn the_inline_svg_warning_is_emitted_once_per_document() {
    let dir = std::env::temp_dir().join(format!(
        "sghtmltopdf-svg-{}-inline-once",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut html = String::from(r#"<body style="margin:0">"#);
    for _ in 0..5 {
        html.push_str(
            r#"<p><svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"></svg></p>"#,
        );
    }
    html.push_str("</body>");
    let input = dir.join("input.html");
    std::fs::write(&input, &html).unwrap();

    let result = Command::new(BIN)
        .arg(&input)
        .args(["--font", FONT_PATH])
        .arg("-o")
        .arg(dir.join("out.pdf"))
        .output()
        .expect("failed to run the sghtmltopdf binary");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(
        stderr.matches("<svg> element").count(),
        1,
        "the warning should appear once, got: {stderr}"
    );
    // It does report how many there were.
    assert!(
        stderr.contains("5 inline"),
        "the warning should count them, got: {stderr}"
    );
    cleanup(&dir);
}

/// A document with only `<img src="*.svg">` gets no inline SVG warning
/// (emitting one would cast doubt on a usage that is perfectly fine).
#[test]
fn referencing_an_svg_from_img_does_not_warn_about_inline_svg() {
    // `convert` confirms that no warning appears.
    let pdf = convert(
        r#"<body style="margin:0"><img src="logo.svg"></body>"#,
        &[],
        "no-inline-warning",
    );
    assert_embedded_as_vector(&pdf);
}

/// A broken SVG is treated as a replaced element with no image and the conversion itself
/// succeeds (the same handling as a failed raster decode).
#[test]
fn a_broken_svg_does_not_abort_the_conversion() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-svg-{}-broken", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("logo.svg"), "<svg><this is not xml").unwrap();
    let input = dir.join("input.html");
    std::fs::write(&input, r#"<body><img src="logo.svg"><p>after</p></body>"#).unwrap();
    let output = dir.join("out.pdf");

    let result = Command::new(BIN)
        .arg(&input)
        .args(["--font", FONT_PATH])
        .arg("-o")
        .arg(&output)
        .output()
        .expect("failed to run the sghtmltopdf binary");
    assert!(
        result.status.success(),
        "a broken SVG should not fail the conversion by default, stderr: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert!(bytes.starts_with(b"%PDF-"));
    assert_xref_is_consistent(&bytes);
    cleanup(&dir);
}
