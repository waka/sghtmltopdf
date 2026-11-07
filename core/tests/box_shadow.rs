//! E2E tests for `box-shadow`.
//!
//! The same approach as `box_sizing.rs`: catch regressions by going through the real
//! pipeline (HTML parse, style cascade, pagination, PDF encode).

use std::collections::HashMap;

use sghtmltopdf::fonts::{Font, FontCollection};
use sghtmltopdf::html;
use sghtmltopdf::layout::{paginate_document, PageSettings};
use sghtmltopdf::pdf::encode_pdf;
use sghtmltopdf::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

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

/// Return every `stream` to `endstream` region in the PDF bytes, inflated and concatenated.
/// Each stream's `/Length N` is parsed and exactly `N` bytes are taken
/// (an implementation naively searching for `\nendstream` cuts in the wrong place when those
/// bytes happen to occur inside an embedded font binary, so this uses the same exact
/// implementation as the identically named helper in `engine.rs`'s test module).
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_subslice(&pdf_bytes[i..], b"/Length ") {
        let len_start = i + pos + b"/Length ".len();
        let mut len_end = len_start;
        while len_end < pdf_bytes.len() && pdf_bytes[len_end].is_ascii_digit() {
            len_end += 1;
        }
        let Some(length) = std::str::from_utf8(&pdf_bytes[len_start..len_end])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
        else {
            i = len_end.max(i + pos + 1);
            continue;
        };
        let Some(stream_rel) = find_subslice(&pdf_bytes[len_end..], b"stream\n") else {
            break;
        };
        let data_start = len_end + stream_rel + b"stream\n".len();
        let data_end = data_start + length;
        if data_end > pdf_bytes.len() {
            i = len_end;
            continue;
        }
        let raw = &pdf_bytes[data_start..data_end];

        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        } else {
            out.extend_from_slice(raw);
        }
        out.push(b'\n');

        i = data_end;
    }
    out
}

/// `/CreationDate`, `/ModDate`, the XMP packet's `xmp:CreateDate` and
/// `xmp:ModifyDate`, and the `/ID` that hashes them, are built from the wall clock
/// at the moment of writing, so a byte comparison of two separately written PDFs
/// fails on exactly those runs that cross a second boundary (it did fail in CI:
/// `D:20260827160357Z` against `D:20260827160358Z`, a one-byte difference; and
/// again once `/ModDate` and the XMP packet were added, at `04:39:25` against
/// `04:39:26`). All are fixed length, so blanking them before the comparison
/// leaves the xref offsets intact.
fn without_wall_clock_metadata(bytes: &[u8]) -> Vec<u8> {
    fn blank_value(bytes: &mut [u8], marker: &[u8], end: u8) {
        let Some(start) = bytes.windows(marker.len()).position(|w| w == marker) else {
            return;
        };
        let value = start + marker.len();
        let Some(len) = bytes[value..].iter().position(|&b| b == end) else {
            return;
        };
        bytes[value..value + len].fill(b'0');
    }

    let mut out = bytes.to_vec();
    blank_value(&mut out, b"/CreationDate (", b')');
    blank_value(&mut out, b"/ModDate (", b')');
    blank_value(&mut out, b"<xmp:CreateDate>", b'<');
    blank_value(&mut out, b"<xmp:ModifyDate>", b'<');
    blank_value(&mut out, b"/ID [", b']');
    out
}

/// One path painting or clipping operator in a content stream, with the subpaths built for it
/// (each the operator lines from its `m` or `re` on, joined with newlines).
#[derive(Debug)]
struct PathOp {
    operator: String,
    subpaths: Vec<String>,
}

/// The fills (`f`, `f*`) and even-odd clips (`W*`) in a content stream, in order.
fn path_ops(stream: &[u8]) -> Vec<PathOp> {
    let mut ops = Vec::new();
    let mut subpaths: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(stream).lines() {
        let operator = line.rsplit(' ').next().unwrap_or("");
        match operator {
            "m" | "re" => subpaths.push(line.to_string()),
            "l" | "c" | "h" => {
                if let Some(subpath) = subpaths.last_mut() {
                    subpath.push('\n');
                    subpath.push_str(line);
                }
            }
            // With no path built, the line is not an operator (it is in a font's bytes, say).
            "f" | "f*" | "W*" if !subpaths.is_empty() => ops.push(PathOp {
                operator: operator.to_string(),
                subpaths: std::mem::take(&mut subpaths),
            }),
            "n" | "S" | "s" | "B" | "b" | "W" => subpaths.clear(),
            _ => {}
        }
    }
    ops
}

fn build_pdf(html_src: &str, css: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
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
fn box_shadow_adds_extra_fill_drawing_before_the_background_end_to_end() {
    let with_shadow = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; background-color: white; \
                box-shadow: 4px 4px 8px rgba(0, 0, 0, 0.5); }",
    );
    let without_shadow = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; background-color: white; }",
    );
    assert!(
        with_shadow.len() > without_shadow.len(),
        "box-shadow should add extra drawing operators to the content stream"
    );
}

#[test]
fn box_shadow_none_draws_nothing_extra_end_to_end() {
    let with_none = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: none; }",
    );
    let without_declaration = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } .box { width: 100px; height: 60px; }",
    );
    assert_eq!(
        without_wall_clock_metadata(&with_none),
        without_wall_clock_metadata(&without_declaration)
    );
}

#[test]
fn box_shadow_inset_is_parsed_but_not_rendered_end_to_end() {
    // `inset` parses but drawing it is not supported (a known simplification).
    let with_inset = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: inset 4px 4px 8px rgba(0,0,0,0.5); }",
    );
    let without_declaration = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } .box { width: 100px; height: 60px; }",
    );
    assert_eq!(
        without_wall_clock_metadata(&with_inset),
        without_wall_clock_metadata(&without_declaration)
    );
}

#[test]
fn box_shadow_with_zero_blur_draws_exactly_one_rect_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: 4px 4px rgb(0, 0, 0); }",
    );
    let decompressed = decompressed_stream_bytes(&bytes);
    // With no blur (blur-radius: 0) the concentric rectangle approximation loop is skipped
    // and only the core rectangle is drawn. `rounded_rect_path` uses a rounded path
    // (`m`/`l`/`c`/`h`), so the number drawn can be counted from the occurrences of
    // `close_path` plus `fill_nonzero` (`h\nf\n`). The div itself has no background-color, so
    // this occurrence should be the single one from the box-shadow.
    assert_eq!(count_occurrences(&decompressed, b"h\nf\n"), 1);
}

#[test]
fn box_shadow_with_blur_paints_each_point_once_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: 0 0 20px rgba(0, 0, 0, 0.5); }",
    );
    let fills: Vec<PathOp> = path_ops(&decompressed_stream_bytes(&bytes))
        .into_iter()
        .filter(|op| op.operator != "W*")
        .collect();
    // The core is filled whole and the 8 bands of the blur ramp around it even-odd, each
    // between its own outline and the next one in. Overlapping fills used to stack the alpha,
    // so the shadow came out much darker than its colour.
    assert_eq!(fills.len(), 9, "{fills:?}");
    assert_eq!(fills[0].operator, "f");
    assert_eq!(fills[0].subpaths.len(), 1);
    for pair in fills.windows(2) {
        assert_eq!(pair[1].operator, "f*");
        assert_eq!(pair[1].subpaths.len(), 2);
        assert_eq!(pair[1].subpaths[1], pair[0].subpaths[0]);
    }
}

#[test]
fn an_outer_box_shadow_is_clipped_out_of_the_border_box_end_to_end() {
    for radius in ["0", "12px"] {
        let bytes = build_pdf(
            r#"<div class="box">x</div>"#,
            &format!(
                "body {{ margin: 0; }} \
                 .box {{ width: 100px; height: 60px; border-radius: {radius}; \
                        box-shadow: 0 0 0 0 black; }}"
            ),
        );
        let ops = path_ops(&decompressed_stream_bytes(&bytes));
        // A rectangle around the shadow with the border box cut out of it, then the shadow,
        // which here is exactly the border box, so nothing of it is visible.
        assert_eq!(ops.len(), 2, "radius={radius}: {ops:?}");
        assert_eq!(ops[0].operator, "W*");
        assert_eq!(ops[0].subpaths.len(), 2);
        assert!(ops[0].subpaths[0].ends_with(" re"));
        assert_eq!(ops[1].operator, "f");
        assert_eq!(ops[1].subpaths, vec![ops[0].subpaths[1].clone()]);
        assert_eq!(
            ops[1].subpaths[0].contains(" c\n"),
            radius != "0",
            "radius={radius}: the hole follows the border-radius"
        );
    }
}

#[test]
fn transparent_box_shadow_layers_draw_nothing_end_to_end() {
    let without_declaration = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } .box { width: 100px; height: 60px; }",
    );
    // `#00000005` has an alpha of 0.02, which rounds to the fully transparent step.
    for shadow in [
        "0 0 #0000",
        "0 0 #0000, 0 0 transparent",
        "0 1px 2px 0 #00000005",
    ] {
        let bytes = build_pdf(
            r#"<div class="box">x</div>"#,
            &format!(
                "body {{ margin: 0; }} \
                 .box {{ width: 100px; height: 60px; box-shadow: {shadow}; }}"
            ),
        );
        assert_eq!(
            without_wall_clock_metadata(&bytes),
            without_wall_clock_metadata(&without_declaration),
            "box-shadow: {shadow}"
        );
    }
}

#[test]
fn tailwind_shadow_draws_only_its_visible_layer_end_to_end() {
    let tailwind = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; \
                box-shadow: 0 0 #0000, 0 0 #0000, 0 0 #0000, 0 0 #0000, \
                            0 1px 3px 0 #00000014, 0 1px 2px 0 #00000005; }",
    );
    let visible_layer = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; box-shadow: 0 1px 3px 0 #00000014; }",
    );
    assert_eq!(
        without_wall_clock_metadata(&tailwind),
        without_wall_clock_metadata(&visible_layer)
    );
}

#[test]
fn box_shadow_comma_separated_list_draws_each_shadow_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; \
                box-shadow: 2px 2px rgb(255,0,0), 4px 4px rgb(0,0,255); }",
    );
    let decompressed = decompressed_stream_bytes(&bytes);
    // Each shadow has blur-radius: 0 (one core rectangle), so two together make 2.
    assert_eq!(count_occurrences(&decompressed, b"h\nf\n"), 2);
}

#[test]
fn box_shadow_and_border_radius_render_a_valid_pdf_end_to_end() {
    let bytes = build_pdf(
        r#"<div class="box">x</div>"#,
        "body { margin: 0; } \
         .box { width: 100px; height: 60px; border-radius: 12px; \
                box-shadow: 4px 4px 8px rgba(0, 0, 0, 0.4); }",
    );
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}
