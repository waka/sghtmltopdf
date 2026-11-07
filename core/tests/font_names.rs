//! E2E tests for how embedded fonts are named in the PDF.
//!
//! A subset font must be named `TAG+PostScriptName` (ISO 32000-1 9.6.4): six uppercase
//! letters that differ between subsets, a `+`, then the font's PostScript name. The name
//! appears as `/BaseFont` on the Type0 and the CIDFont dictionaries and as `/FontName` in
//! the FontDescriptor, in both the batch and the streaming writer.

use std::path::PathBuf;
use std::process::Command;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-fontnames-{}-{name}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        std::fs::write(self.dir.join(relative), contents).unwrap();
    }

    fn convert(&self, html: &str, extra: &[&str]) -> Vec<u8> {
        self.write("in.html", html);
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
        std::fs::read(&output).unwrap()
    }
}

/// Every `/<key> /Name` value in the PDF, in file order.
fn names_of(pdf: &[u8], key: &str) -> Vec<String> {
    let needle = format!("/{key} /");
    let needle = needle.as_bytes();
    let mut names = Vec::new();
    let mut i = 0;
    while i + needle.len() <= pdf.len() {
        if !pdf[i..].starts_with(needle) {
            i += 1;
            continue;
        }
        let start = i + needle.len();
        let end = pdf[start..]
            .iter()
            .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'_' | b'.')))
            .map_or(pdf.len(), |n| start + n);
        names.push(String::from_utf8_lossy(&pdf[start..end]).into_owned());
        i = end;
    }
    names
}

fn distinct(mut names: Vec<String>) -> Vec<String> {
    names.sort();
    names.dedup();
    names
}

/// Split `TAG+Name` and check the tag is six uppercase ASCII letters.
fn split_tag(name: &str) -> (&str, &str) {
    let (tag, rest) = name
        .split_once('+')
        .unwrap_or_else(|| panic!("`{name}` has no subset tag"));
    assert!(
        tag.len() == 6 && tag.bytes().all(|b| b.is_ascii_uppercase()),
        "`{tag}` is not six uppercase letters"
    );
    (tag, rest)
}

fn assert_named_after(pdf: &[u8], postscript_name: &str) -> String {
    let base_fonts = names_of(pdf, "BaseFont");
    let font_names = names_of(pdf, "FontName");
    assert_eq!(
        distinct(base_fonts.clone()),
        distinct(font_names),
        "/BaseFont and /FontName should agree"
    );
    assert_eq!(
        base_fonts.len(),
        2,
        "one Type0 and one CIDFont: {base_fonts:?}"
    );
    assert_eq!(
        base_fonts[0], base_fonts[1],
        "Type0 and CIDFont share the name"
    );
    let (_, name) = split_tag(&base_fonts[0]);
    assert_eq!(name, postscript_name);
    base_fonts[0].clone()
}

const PARAGRAPH: &str = "<p>Hello world</p>";

#[test]
fn a_font_is_named_after_its_postscript_name_with_a_subset_tag() {
    let pdf = Fixture::new("batch").convert(PARAGRAPH, &["--font", FONT_PATH]);
    assert_named_after(&pdf, "DejaVuSans");
}

#[test]
fn the_streaming_writer_names_the_font_the_same_way() {
    let fx = Fixture::new("streaming");
    let batch = fx.convert(PARAGRAPH, &["--font", FONT_PATH]);
    let streaming = fx.convert(PARAGRAPH, &["--font", FONT_PATH, "--streaming"]);
    // The same glyphs from the same font make the same subset, so the same tag.
    assert_eq!(
        assert_named_after(&streaming, "DejaVuSans"),
        assert_named_after(&batch, "DejaVuSans")
    );
}

#[test]
fn the_same_input_gives_the_same_names() {
    let fx = Fixture::new("determinism");
    let first = fx.convert(PARAGRAPH, &["--font", FONT_PATH]);
    let second = fx.convert(PARAGRAPH, &["--font", FONT_PATH]);
    assert_eq!(names_of(&first, "BaseFont"), names_of(&second, "BaseFont"));
}

#[test]
fn different_subsets_of_one_font_get_different_tags() {
    let fx = Fixture::new("tags");
    let hello = fx.convert("<p>Hello</p>", &["--font", FONT_PATH]);
    let other = fx.convert("<p>Goodbye</p>", &["--font", FONT_PATH]);
    assert_ne!(names_of(&hello, "BaseFont"), names_of(&other, "BaseFont"));
}

#[test]
fn two_subsets_of_one_font_in_one_document_get_different_tags() {
    for extra in [&[][..], &["--streaming"][..]] {
        let fx = Fixture::new("two-subsets");
        // Two families backed by the same file are two fonts, so two subsets.
        fx.write("a.ttf", std::fs::read(FONT_PATH).unwrap());
        fx.write("b.ttf", std::fs::read(FONT_PATH).unwrap());
        let pdf = fx.convert(
            r#"<style>
                 @font-face { font-family: A; src: url(a.ttf); }
                 @font-face { font-family: B; src: url(b.ttf); }
               </style>
               <p style="font-family: A">Hello</p>
               <p style="font-family: B">Goodbye</p>"#,
            extra,
        );
        let names = distinct(names_of(&pdf, "BaseFont"));
        assert_eq!(names.len(), 2, "two subsets, two names: {names:?}");
        let tags: Vec<_> = names.iter().map(|n| split_tag(n)).collect();
        assert_ne!(tags[0].0, tags[1].0);
        assert!(tags.iter().all(|(_, name)| *name == "DejaVuSans"));
    }
}

/// Overwrite the name ID of every `name` record with ID `from` so the font no longer has
/// that string. Checksums are not verified by the font readers.
fn without_name_id(mut font: Vec<u8>, from: u16) -> Vec<u8> {
    let be16 = |b: &[u8], at: usize| u16::from_be_bytes([b[at], b[at + 1]]) as usize;
    let be32 =
        |b: &[u8], at: usize| u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
    let tables = be16(&font, 4);
    let name_at = (0..tables)
        .map(|i| 12 + i * 16)
        .find(|&rec| &font[rec..rec + 4] == b"name")
        .map(|rec| be32(&font, rec + 8))
        .expect("the font has a name table");
    let count = be16(&font, name_at + 2);
    for i in 0..count {
        let rec = name_at + 6 + i * 12;
        if be16(&font, rec + 6) == from as usize {
            font[rec + 6..rec + 8].copy_from_slice(&0xFFF0u16.to_be_bytes());
        }
    }
    font
}

#[test]
fn a_font_without_a_postscript_name_is_named_after_its_family_and_subfamily() {
    let fx = Fixture::new("no-ps-name");
    fx.write(
        "nameless.ttf",
        without_name_id(std::fs::read(FONT_PATH).unwrap(), 6),
    );
    let font = fx.dir.join("nameless.ttf");
    let pdf = fx.convert(PARAGRAPH, &["--font", font.to_str().unwrap()]);
    assert_named_after(&pdf, "DejaVuSans-Book");
}

#[test]
fn a_font_without_any_name_still_gets_a_name() {
    let fx = Fixture::new("no-names");
    let mut data = std::fs::read(FONT_PATH).unwrap();
    for id in [1, 2, 6, 16, 17] {
        data = without_name_id(data, id);
    }
    fx.write("nameless.ttf", data);
    let font = fx.dir.join("nameless.ttf");
    let pdf = fx.convert(PARAGRAPH, &["--font", font.to_str().unwrap()]);
    assert_named_after(&pdf, "Font");
}
