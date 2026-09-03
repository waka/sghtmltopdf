//! End-to-end tests for colour fonts (#12).
//!
//! Two things are covered:
//!
//! * Embedded bitmaps (`CBDT`/`CBLC`, `sbix`) — a font like Noto Color Emoji,
//!   which carries no outlines at all. It used to be rejected during font
//!   selection, so the emoji simply vanished.
//! * `COLR`/`CPAL` v0 — solid colour layers. These did get drawn before,
//!   because the font also has base outlines, but they came out monochrome.
//!
//! Both are written to the PDF as Type 3 fonts. The glyphs stay text, so
//! `/ToUnicode` keeps extraction and search working, and the original font
//! program is never embedded.

use std::collections::HashMap;
use std::io::Read;

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::fonts::{ColorGlyph, Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, PageSettings};
use sghtmltopdf_core::pdf::{encode_pdf_with_options, LinkSettings, PdfOutputOptions};
use sghtmltopdf_core::sink::MemorySink;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const DEJAVU: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const NOTO_COLOR_EMOJI: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/NotoColorEmoji.ttf"
);

// ---------------------------------------------------------------------------
// Building a COLRv0 test font
// ---------------------------------------------------------------------------

/// Build a font by adding `COLR` v0 and `CPAL` v0 tables to DejaVu Sans.
///
/// The repository has no COLR font, and the real ones (Bungee Spice and
/// friends) are COLRv1, which is out of scope — so grow one from a font we do
/// have. The glyph for `base` is defined as `layers`, a list of (glyph id,
/// palette index) painted one over the other.
fn dejavu_with_colr_v0(base: char, layers: &[(char, u16)], palette: &[[u8; 4]]) -> Vec<u8> {
    let data = std::fs::read(DEJAVU).expect("should read bundled DejaVu");
    let font = Font::from_bytes(data.clone(), 0).expect("should parse bundled DejaVu");
    let base_gid = font.glyph_id(base).expect("base glyph must exist");
    let layer_gids: Vec<(u16, u16)> = layers
        .iter()
        .map(|(c, palette_index)| {
            (
                font.glyph_id(*c).expect("layer glyph must exist"),
                *palette_index,
            )
        })
        .collect();

    add_tables(
        &data,
        &[
            (*b"COLR", colr_v0_table(base_gid, &layer_gids)),
            (*b"CPAL", cpal_v0_table(palette)),
        ],
    )
}

/// A `COLR` table (version 0) holding exactly one base glyph.
fn colr_v0_table(base_gid: u16, layers: &[(u16, u16)]) -> Vec<u8> {
    let mut out = Vec::new();
    let header_len = 14u32;
    let base_records_len = 6u32; // one base glyph record
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&1u16.to_be_bytes()); // numBaseGlyphRecords
    out.extend_from_slice(&header_len.to_be_bytes()); // baseGlyphRecordsOffset
    out.extend_from_slice(&(header_len + base_records_len).to_be_bytes()); // layerRecordsOffset
    out.extend_from_slice(&(layers.len() as u16).to_be_bytes()); // numLayerRecords

    out.extend_from_slice(&base_gid.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // firstLayerIndex
    out.extend_from_slice(&(layers.len() as u16).to_be_bytes()); // numLayers

    for (gid, palette_index) in layers {
        out.extend_from_slice(&gid.to_be_bytes());
        out.extend_from_slice(&palette_index.to_be_bytes());
    }
    out
}

/// A `CPAL` table (version 0) holding exactly one palette.
fn cpal_v0_table(colors: &[[u8; 4]]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_be_bytes()); // version
    out.extend_from_slice(&(colors.len() as u16).to_be_bytes()); // numPaletteEntries
    out.extend_from_slice(&1u16.to_be_bytes()); // numPalettes
    out.extend_from_slice(&(colors.len() as u16).to_be_bytes()); // numColorRecords
    out.extend_from_slice(&14u32.to_be_bytes()); // colorRecordsArrayOffset
    out.extend_from_slice(&0u16.to_be_bytes()); // colorRecordIndices[0]
    for color in colors {
        out.extend_from_slice(color); // BGRA
    }
    out
}

/// Rebuild the sfnt table directory with the `extra` tables added.
fn add_tables(font: &[u8], extra: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let num_tables = u16::from_be_bytes([font[4], font[5]]) as usize;
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(num_tables + extra.len());
    for i in 0..num_tables {
        let rec = 12 + i * 16;
        let tag = [font[rec], font[rec + 1], font[rec + 2], font[rec + 3]];
        let offset = u32::from_be_bytes(font[rec + 8..rec + 12].try_into().unwrap()) as usize;
        let length = u32::from_be_bytes(font[rec + 12..rec + 16].try_into().unwrap()) as usize;
        tables.push((tag, font[offset..offset + length].to_vec()));
    }
    tables.extend(extra.iter().cloned());
    tables.sort_by_key(|(tag, _)| *tag);

    let count = tables.len();
    let mut out = Vec::new();
    out.extend_from_slice(&font[0..4]); // sfntVersion
    out.extend_from_slice(&(count as u16).to_be_bytes());
    let entry_selector = (usize::BITS - 1 - count.leading_zeros()) as u16;
    let search_range = (1u16 << entry_selector) * 16;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&((count as u16) * 16 - search_range).to_be_bytes());

    let mut offset = 12 + count * 16;
    let mut records = Vec::new();
    for (tag, body) in &tables {
        records.push((*tag, offset as u32, body.len() as u32));
        offset += (body.len() + 3) & !3;
    }
    for (tag, offset, length) in &records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&0u32.to_be_bytes()); // checkSum; skrifa does not verify it
        out.extend_from_slice(&offset.to_be_bytes());
        out.extend_from_slice(&length.to_be_bytes());
    }
    for (_, body) in &tables {
        out.extend_from_slice(body);
        out.resize((out.len() + 3) & !3, 0);
    }
    out
}

// ---------------------------------------------------------------------------
// Reading the font
// ---------------------------------------------------------------------------

#[test]
fn a_colr_v0_glyph_reads_back_as_solid_colour_layers() {
    // Define 'A' as the outline of 'A' in red with the outline of 'B' in blue
    // painted over it.
    let data = dejavu_with_colr_v0(
        'A',
        &[('A', 0), ('B', 1)],
        &[[0, 0, 255, 255], [255, 0, 0, 255]], // BGRA: red, blue
    );
    let font = Font::from_bytes(data, 0).expect("should parse the synthesised COLR font");

    assert!(font.has_color_glyphs());
    let gid = font.glyph_id('A').unwrap();
    let Some(ColorGlyph::LayersV0(layers)) = font.color_glyph(gid) else {
        panic!("a COLRv0 base glyph should read back as a layer list");
    };

    assert_eq!(layers.len(), 2);
    assert_eq!(layers[0].glyph_id, font.glyph_id('A').unwrap());
    assert_eq!(layers[0].color, Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(layers[1].glyph_id, font.glyph_id('B').unwrap());
    assert_eq!(layers[1].color, Some([0.0, 0.0, 1.0, 1.0]));
}

/// Palette index 0xFFFF means "whatever the text colour is". Rather than
/// pinning a colour down, report `None` and leave it to the fill colour in
/// effect where the glyph is drawn.
#[test]
fn a_colr_v0_layer_using_the_text_colour_has_no_colour_of_its_own() {
    let data = dejavu_with_colr_v0('A', &[('A', 0xFFFF)], &[[0, 0, 255, 255]]);
    let font = Font::from_bytes(data, 0).expect("should parse the synthesised COLR font");

    let gid = font.glyph_id('A').unwrap();
    let Some(ColorGlyph::LayersV0(layers)) = font.color_glyph(gid) else {
        panic!("a COLRv0 base glyph should read back as a layer list");
    };
    assert_eq!(layers.len(), 1);
    assert_eq!(layers[0].color, None);
}

/// Even in a COLR font, a character that is not registered as a base glyph
/// stays an ordinary outline glyph and goes into the Type0 font.
#[test]
fn a_plain_glyph_in_a_colr_font_is_not_a_colour_glyph() {
    let data = dejavu_with_colr_v0('A', &[('A', 0)], &[[0, 0, 255, 255]]);
    let font = Font::from_bytes(data, 0).expect("should parse the synthesised COLR font");

    assert!(font.color_glyph(font.glyph_id('Z').unwrap()).is_none());
}

// ---------------------------------------------------------------------------
// Writing the PDF
// ---------------------------------------------------------------------------

fn pdf_bytes(html_src: &str, fonts: FontCollection) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    encode_pdf_with_options(
        &pages,
        &styles,
        &HashMap::new(),
        &fonts,
        &settings,
        &LinkSettings::default(),
        // With compression off the content streams can be inspected as
        // plain bytes.
        &PdfOutputOptions {
            compress: false,
            ..PdfOutputOptions::default()
        },
    )
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

fn emoji_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(DEJAVU).expect("should load DejaVu"),
        Font::load(NOTO_COLOR_EMOJI).expect("should load Noto Color Emoji"),
    ])
}

/// The emoji is written as a Type 3 font whose glyph draws an image XObject,
/// with the alpha channel as an `/SMask`.
#[test]
fn a_bitmap_emoji_is_written_as_a_type3_font_drawing_an_image() {
    let pdf = pdf_bytes("<p>A \u{1F389} B</p>", emoji_fonts());

    assert!(
        count(&pdf, b"/Subtype /Type3") >= 1,
        "colour glyphs should be written as a Type 3 font"
    );
    assert!(
        count(&pdf, b"/SMask") >= 1,
        "the PNG's alpha channel should be written as an /SMask"
    );
    assert!(
        count(&pdf, b"/CharProcs") >= 1,
        "a Type 3 font has /CharProcs"
    );
}

/// The font program of an outline-less font is never embedded — the 9.9MB PDF
/// described in #12.
#[test]
fn the_bitmap_font_program_itself_is_never_embedded() {
    let pdf = pdf_bytes("<p>A \u{1F389} B</p>", emoji_fonts());

    assert_eq!(
        count(&pdf, b"/FontFile2"),
        1,
        "the only font program embedded should be DejaVu, which has outlines"
    );
    assert!(
        pdf.len() < 1_000_000,
        "the 10MB bitmap font must not be carried along whole: {} bytes",
        pdf.len()
    );
}

/// The emoji stays text, so extraction and search keep working.
#[test]
fn a_bitmap_emoji_keeps_its_text_through_to_unicode() {
    let pdf = pdf_bytes("<p>A \u{1F389} B</p>", emoji_fonts());
    let text = String::from_utf8_lossy(&pdf).into_owned();

    // `UnicodeCmap` writes surrogate pairs as 16-bit units: U+1F389 is
    // D83C DF89.
    assert!(
        text.contains("<d83cdf89>") || text.contains("<D83CDF89>"),
        "no /ToUnicode entry for the emoji"
    );
}

/// The emoji contributes its own advance width to layout: neither a gap with
/// nothing drawn in it, nor zero width with the neighbours overlapping.
#[test]
fn a_bitmap_emoji_occupies_its_own_advance_width() {
    let with_emoji = text_width("<p>A\u{1F389}B</p>", emoji_fonts());
    let without = text_width("<p>AB</p>", emoji_fonts());
    assert!(
        with_emoji > without + 10.0,
        "the emoji did not widen the line: {with_emoji} vs {without}"
    );
}

/// The word space after an emoji is measured with the text font, not with the
/// emoji font.
///
/// Noto Color Emoji is monospaced at ~1.25em, so if the gap were measured with
/// it every space following an emoji would be about four times too wide.
#[test]
fn a_word_space_after_an_emoji_is_measured_with_the_text_font() {
    let after_emoji = text_width("<p>\u{1F389} A</p>", emoji_fonts())
        - text_width("<p>\u{1F389}A</p>", emoji_fonts());
    let after_a_letter =
        text_width("<p>B A</p>", emoji_fonts()) - text_width("<p>BA</p>", emoji_fonts());

    assert!(
        (after_emoji - after_a_letter).abs() < 0.01,
        "a space is a space whatever precedes it: {after_emoji} vs {after_a_letter}"
    );
}

fn text_width(html_src: &str, fonts: FontCollection) -> f32 {
    use sghtmltopdf_core::layout::{LaidOutBox, LaidOutContent};

    fn walk(b: &LaidOutBox, out: &mut f32) {
        match &b.content {
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    let width: f32 = line.runs.iter().map(|r| r.width).sum();
                    *out = out.max(line.runs.last().map_or(width, |r| r.x_offset + r.width));
                }
            }
            LaidOutContent::Blocks(children) => {
                for child in children {
                    walk(child, out);
                }
            }
            _ => {}
        }
    }

    let dom = html::parse(html_src.as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let pages = paginate_document(&dom, &styles, &fonts, &PageSettings::default());
    let mut width = 0.0;
    for page in &pages {
        for b in &page.boxes {
            walk(b, &mut width);
        }
    }
    width
}

/// COLRv0 is painted as coloured layers: each palette colour should appear
/// verbatim as a PDF `rg` (non-stroking colour) operator.
#[test]
fn a_colr_v0_glyph_is_painted_with_its_palette_colours() {
    let data = dejavu_with_colr_v0(
        'A',
        &[('A', 0), ('B', 1)],
        &[[0, 0, 255, 255], [255, 0, 0, 255]], // BGRA: red, blue
    );
    let font = Font::from_bytes(data, 0).expect("should parse the synthesised COLR font");
    let pdf = pdf_bytes("<p>A</p>", FontCollection::new(vec![font]));

    assert!(
        count(&pdf, b"/Subtype /Type3") >= 1,
        "a COLR base glyph should be written as a Type 3 font"
    );
    let text = String::from_utf8_lossy(&pdf).into_owned();
    assert!(text.contains("1 0 0 rg"), "the red layer was not filled");
    assert!(text.contains("0 0 1 rg"), "the blue layer was not filled");
}

/// Streaming output writes the colour font too.
///
/// Streaming finishes each page's resource dictionary at that page, so a
/// colour font cannot be added once we find out it is needed. Check that this
/// separate path reaches the same result as batch mode.
#[test]
fn streaming_output_also_writes_the_colour_font() {
    for mode in [Mode::Batch, Mode::Streaming] {
        let options = EngineOptions {
            mode,
            fonts: vec![
                FontSpec {
                    path: DEJAVU.into(),
                    index: 0,
                },
                FontSpec {
                    path: NOTO_COLOR_EMOJI.into(),
                    index: 0,
                },
            ],
            output: PdfOutputOptions {
                compress: false,
                ..PdfOutputOptions::default()
            },
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine
            .feed("<p>A \u{1F389} B</p>".as_bytes())
            .expect("feed should succeed");
        let pdf = engine.finish().expect("render should succeed");

        assert!(
            count(&pdf, b"/Subtype /Type3") >= 1,
            "{mode:?}: colour glyphs should become a Type 3 font"
        );
        assert_eq!(
            count(&pdf, b"/FontFile2"),
            1,
            "{mode:?}: the bitmap font's program must not be embedded"
        );
        assert!(
            count(&pdf, b"/SMask") >= 1,
            "{mode:?}: the emoji's alpha channel was not written"
        );
    }
}

/// Compression changes nothing but the encoding: inflate the streams and the
/// Type 3 glyph procedures are there.
#[test]
fn compressed_output_still_contains_the_glyph_procedures() {
    let dom = html::parse("<p>A \u{1F389} B</p>".as_bytes());
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &parse_stylesheet(""));
    let fonts = emoji_fonts();
    let settings = PageSettings::default();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let pdf = encode_pdf_with_options(
        &pages,
        &styles,
        &HashMap::new(),
        &fonts,
        &settings,
        &LinkSettings::default(),
        &PdfOutputOptions::default(),
    );

    assert!(count(&pdf, b"/CharProcs") >= 1);
    let streams = decompressed_streams(&pdf);
    assert!(
        count(&streams, b"d0") >= 1,
        "a Type 3 glyph procedure starts with d0"
    );
}

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
