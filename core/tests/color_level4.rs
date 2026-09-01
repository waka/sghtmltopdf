//! E2E tests for Color Level 4 (`lab()`/`lch()`/`oklab()`/`oklch()`).
//!
//! The same approach as `box_sizing.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode).

use std::collections::HashMap;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

fn build_pdf(css: &str) -> Vec<u8> {
    let dom = html::parse(br#"<div class="box">color test</div>"#);
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn lab_background_color_renders_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(".box { background-color: lab(53.2408% 80.0925 67.2032); }");
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

#[test]
fn lch_background_color_renders_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(".box { background-color: lch(53.2408% 104.5518 39.999deg); }");
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

#[test]
fn oklab_background_color_renders_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(".box { background-color: oklab(62.8% 0.2249 0.1258); }");
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

/// Mask out the `/CreationDate` value.
///
/// A PDF's Info dictionary always carries the creation time, so comparing two separately
/// generated PDFs directly would fail only when the two generations straddled a second
/// boundary. The value is fixed-length (`D:YYYYMMDDHHMMSSZ`), so padding it to the same
/// length leaves every later byte position (the cross-reference table offsets) unchanged.
fn mask_creation_date(bytes: &[u8]) -> Vec<u8> {
    const KEY: &[u8] = b"/CreationDate (";
    let mut out = bytes.to_vec();
    let Some(key_at) = out.windows(KEY.len()).position(|w| w == KEY) else {
        return out;
    };
    let value_at = key_at + KEY.len();
    let Some(value_len) = out[value_at..].iter().position(|&b| b == b')') else {
        return out;
    };
    out[value_at..value_at + value_len].fill(b'X');
    out
}

/// Check that two PDFs are identical apart from the creation time.
///
/// On a mismatch it prints only the first position and its surroundings. Passing arrays of
/// tens of thousands of bytes to `assert_eq!` dumps both in full rather than the difference.
fn assert_same_pdf(left: &[u8], right: &[u8]) {
    let (left, right) = (mask_creation_date(left), mask_creation_date(right));
    let first_diff = left
        .iter()
        .zip(right.iter())
        .position(|(a, b)| a != b)
        .or_else(|| (left.len() != right.len()).then_some(left.len().min(right.len())));
    let Some(at) = first_diff else {
        return;
    };
    let window = |bytes: &[u8]| {
        let from = at.saturating_sub(40);
        let to = (at + 40).min(bytes.len());
        String::from_utf8_lossy(&bytes[from..to]).to_string()
    };
    panic!(
        "the PDFs differ from byte {at} ({} bytes vs {} bytes)\n  left : {:?}\n  right: {:?}",
        left.len(),
        right.len(),
        window(&left),
        window(&right)
    );
}

// oklch(59.686% 0.15619 49.7694deg) is the equivalent of rgb(198, 93, 6). This confirms the
// colour space conversion falls correctly to a RgbaColor through the whole pipeline (style
// cascade, PDF encode), by matching the bytes against giving the same RGB values directly.
#[test]
fn oklch_background_color_matches_equivalent_rgb_byte_for_byte() {
    let oklch_bytes = build_pdf(".box { background-color: oklch(59.686% 0.15619 49.7694deg); }");
    let rgb_bytes = build_pdf(".box { background-color: rgb(198, 93, 6); }");
    assert_same_pdf(&oklch_bytes, &rgb_bytes);
}

/// That the comparison above really does ignore a difference in the creation time.
///
/// It models two PDFs generated across a second boundary by rewriting only the seconds digit
/// of the date. Without it, this would fail only when the two generations landed in different seconds.
#[test]
fn the_comparison_ignores_the_creation_timestamp() {
    const KEY: &[u8] = b"/CreationDate (";
    let bytes = build_pdf(".box { background-color: rgb(1, 2, 3); }");

    let mut later = bytes.clone();
    let value_at = later.windows(KEY.len()).position(|w| w == KEY).unwrap() + KEY.len();
    // The last digit of the seconds in `D:YYYYMMDDHHMMSSZ`.
    let seconds_ones = value_at + 15;
    later[seconds_ones] = if later[seconds_ones] == b'9' {
        b'0'
    } else {
        b'9'
    };

    assert_ne!(
        bytes, later,
        "premise: only the date differs between the byte strings"
    );
    assert_same_pdf(&bytes, &later);
}
