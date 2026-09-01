//! Tests that glyph advances agree between layout and drawing.
//!
//! Layout positions from the `x_advance` the shaper returns, but a PDF viewer advances a
//! glyph by the CIDFont's `/W` (one value per glyph ID). There are paths where the two
//! disagree, and the difference is made up by TJ array corrections (`pdf::document::show_run_glyphs`).
//!
//! Here we confirm from the content stream of a really generated PDF that "the total of the
//! corrections" equals "the difference between the layout width and the `/W`-derived width".

use std::collections::HashMap;
use std::io::Read;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html::{self, Dom, NodeData, NodeId};
use sghtmltopdf_core::layout::{
    build_box_tree, layout_document, paginate_document, LaidOutBox, LaidOutContent, PageSettings,
};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const DEJAVU: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
/// A font without glyphs for `&thinsp;` (U+2009) and the like. The shaper substitutes the
/// space glyph while replacing only the advance, so a disagreement with `/W` arises.
const NOTO_CJK: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/NotoSansCJK-Regular.ttc"
);

fn find_tag(dom: &Dom, id: NodeId, tag: &str) -> Option<NodeId> {
    if let NodeData::Element { name, .. } = &dom.node(id).data {
        if &*name.local == tag {
            return Some(id);
        }
    }
    dom.children(id).find_map(|child| find_tag(dom, child, tag))
}

fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
    if b.node == Some(target) {
        return Some(b);
    }
    if let LaidOutContent::Blocks(children) = &b.content {
        return children.iter().find_map(|c| find_laid_out(c, target));
    }
    None
}

/// For the first `<p>`, return the difference (px) between the total advance layout uses and
/// the total if advanced by `/W` alone. Exactly what the TJ corrections have to make up.
fn advance_gap_of_first_p(html_src: &str, css: &str, font_path: &str) -> f32 {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let font = Font::load(font_path).expect("should load test font");
    let fonts = FontCollection::new(vec![Font::load(font_path).expect("should load test font")]);
    let tree = build_box_tree(&dom, &styles);
    let laid = layout_document(
        &tree,
        &styles,
        &fonts,
        PageSettings::default().content_width(),
    );
    let p = find_tag(&dom, dom.document(), "p").expect("p not found");
    let p_box = find_laid_out(&laid, p).expect("p should be laid out");
    let LaidOutContent::Inline(lines) = &p_box.content else {
        panic!("expected inline content");
    };

    let units_per_em = font.units_per_em() as f32;
    let mut gap = 0.0;
    for line in lines {
        for run in &line.runs {
            for glyph in &run.glyphs {
                let pdf_advance = font.glyph_hor_advance(glyph.glyph_id).unwrap_or(0) as f32
                    * run.font_size
                    / units_per_em;
                gap += glyph.x_advance - pdf_advance;
            }
        }
    }
    gap
}

fn pdf_bytes(html_src: &str, css: &str, font_path: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(css));
    let fonts = FontCollection::new(vec![Font::load(font_path).expect("should load test font")]);
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings)
}

/// The content streams are FlateDecode compressed, so they are inflated and concatenated.
fn decompressed_streams(pdf: &[u8]) -> Vec<u8> {
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find(&pdf[i..], b"stream\n") {
        let start = i + pos + b"stream\n".len();
        let Some(end_rel) = find(&pdf[start..], b"endstream") else {
            break;
        };
        let end = start + end_rel;
        let mut decoded = Vec::new();
        if flate2::read::ZlibDecoder::new(&pdf[start..end])
            .read_to_end(&mut decoded)
            .is_ok()
        {
            out.extend_from_slice(&decoded);
            out.push(b'\n');
        }
        i = end + b"endstream".len();
    }
    out
}

/// Sum every correction appearing in a `[...] TJ` array (in 1/1000ths of text space).
///
/// The inside of a `(...)` string is a byte string (with escapes) and is skipped. To avoid
/// picking up the operands of an operator other than `TJ`, only arrays immediately followed
/// by `TJ` after the `]` are counted. The inflated bytes also contain the font file streams,
/// and starting the count at a `[` found there would pick up the preceding operator's
/// operands, so a `[` encountered part-way through restarts the count as the array's start.
fn sum_tj_adjustments(stream: &[u8]) -> f32 {
    fn take_number(buf: &mut String, out: &mut Vec<f32>) {
        if !buf.is_empty() {
            if let Ok(v) = buf.parse::<f32>() {
                out.push(v);
            }
            buf.clear();
        }
    }

    let mut total = 0.0;
    let mut i = 0;
    while i < stream.len() {
        if stream[i] != b'[' {
            i += 1;
            continue;
        }
        let mut numbers = Vec::new();
        let mut buf = String::new();
        let mut in_string = false;
        let mut escaped = false;
        let mut j = i + 1;
        while j < stream.len() {
            let b = stream[j];
            if in_string {
                if escaped {
                    escaped = false;
                } else if b == b'\\' {
                    escaped = true;
                } else if b == b')' {
                    in_string = false;
                }
                j += 1;
                continue;
            }
            if b == b']' {
                take_number(&mut buf, &mut numbers);
                break;
            }
            if b == b'[' {
                // The earlier `[` was not the start of an array. Restart the count here.
                numbers.clear();
                buf.clear();
            } else if b == b'(' {
                take_number(&mut buf, &mut numbers);
                in_string = true;
            } else if b.is_ascii_digit() || b == b'-' || b == b'+' || b == b'.' {
                buf.push(b as char);
            } else {
                take_number(&mut buf, &mut numbers);
            }
            j += 1;
        }
        // Skip the whitespace after `]` and look at the operator.
        let mut k = j + 1;
        while k < stream.len() && stream[k].is_ascii_whitespace() {
            k += 1;
        }
        if stream[k..].starts_with(b"TJ") {
            total += numbers.iter().sum::<f32>();
        }
        i = j + 1;
    }
    total
}

/// The total TJ correction in px. A TJ value is subtracted from the advance, so the sign is flipped.
fn tj_correction_px(html_src: &str, css: &str, font_path: &str, font_size: f32) -> f32 {
    let pdf = pdf_bytes(html_src, css, font_path);
    let stream = decompressed_streams(&pdf);
    -sum_tj_adjustments(&stream) / 1000.0 * font_size
}

const JUSTIFIED: &str = "<p>aaa bbb ccc ddd eee fff ggg hhh iii jjj kkk lll</p>";
const JUSTIFY_CSS: &str = "body { margin: 0; } \
                           p { margin: 0; text-align: justify; width: 300px; font-size: 16px; }";

#[test]
fn a_justified_line_is_drawn_as_wide_as_it_was_laid_out() {
    // `merge_adjacent_runs` restores an inter-word gap as "a space glyph whose advance is the
    // gap". A gap widened by `text-align: justify` does not match the space's own width, so
    // without the TJ correction the line falls short of the right edge by what it was stretched.
    let expected = advance_gap_of_first_p(JUSTIFIED, JUSTIFY_CSS, DEJAVU);
    assert!(
        expected > 1.0,
        "the test document should actually be stretched, got {expected}px"
    );

    let corrected = tj_correction_px(JUSTIFIED, JUSTIFY_CSS, DEJAVU, 16.0);
    assert!(
        (corrected - expected).abs() < 0.01,
        "TJ should make up the whole stretch: corrected={corrected}px expected={expected}px"
    );
}

#[test]
fn a_left_aligned_line_needs_no_correction() {
    // Without justification the gap is the space's own width, so no correction appears
    // (which also confirms the TJ array does not grow needlessly in an ordinary document).
    let css = "body { margin: 0; } p { margin: 0; width: 300px; font-size: 16px; }";
    let expected = advance_gap_of_first_p(JUSTIFIED, css, DEJAVU);
    assert!(
        expected.abs() < 0.01,
        "no stretch is expected here, got {expected}px"
    );

    let corrected = tj_correction_px(JUSTIFIED, css, DEJAVU, 16.0);
    assert!(
        corrected.abs() < 0.01,
        "there should be nothing to correct, got {corrected}px"
    );
}

#[test]
fn a_fixed_width_space_the_font_lacks_is_drawn_at_its_own_advance() {
    // For a fixed-width space the font lacks, the shaper substitutes the space glyph while
    // replacing only the advance with the prescribed value (em/5 for U+2009). `/W` is shared
    // with the ordinary space, so without the correction the following characters shift.
    let html_src = "<p>a\u{2009}b\u{2009}c</p>";
    let css = "body { margin: 0; } p { margin: 0; font-size: 16px; }";

    let expected = advance_gap_of_first_p(html_src, css, NOTO_CJK);
    assert!(
        expected.abs() > 0.1,
        "the font should be missing U+2009 for this test to mean anything, got {expected}px"
    );

    let corrected = tj_correction_px(html_src, css, NOTO_CJK, 16.0);
    assert!(
        (corrected - expected).abs() < 0.01,
        "TJ should absorb the difference: corrected={corrected}px expected={expected}px"
    );
}
