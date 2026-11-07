//! Embedding fonts in the PDF (CIDFontType2 plus a Type0 with `/Encoding /Identity-H`).
//!
//! Based on the approach validated in `core/examples/spike_pdf_font_embedding.rs`, with
//! subsetting to only the glyphs actually used (the `subsetter` crate) and text extraction
//! support via a `/ToUnicode` CMap added on top.
//!
//! `subsetter::subset` removes the `cmap` table from the subsetted font by design (a
//! deliberate choice for PDF embedding), so the subsetted glyph IDs differ from the original
//! ones (they are repacked compactly). There are two embedding approaches:
//!
//! - [`embed_font`] (batch processing, for `pdf::document::encode_pdf`): glyph usage can be
//!   settled by scanning every page before the content streams are written, so the CIDs
//!   themselves are repacked into the subsetted glyph IDs (`/CIDToGIDMap /Identity`). It
//!   returns a mapping from original glyph ID to subsetted glyph ID (= CID), which the
//!   caller uses to translate glyph IDs while writing the content streams
//!
//! - [`embed_font_streaming_chunks`] (for streaming): the content stream is written
//!   immediately as each page is settled, so a CID is always the original glyph ID
//!   (never repacked). Only the font embedding is subsetted after every page is processed,
//!   and the two are reconciled by making `/CIDToGIDMap` an explicit stream holding the
//!   CID (= original GID) to subsetted GID mapping
//!
//! `pdf-writer` does no compression of its own, so the subsetted font bytes are zlib
//! (`/FlateDecode`) compressed with `flate2` before embedding.

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Chunk, Filter, Finish, Name, Pdf, Rect as PdfRect, Ref, Str};
use subsetter::GlyphRemapper;

use crate::fonts::Font;

use super::color_font::{CODES_PER_COLOR_FONT, MAX_COLOR_GLYPHS_PER_FACE};

/// The name used both for `/CMapName` in the `/ToUnicode` CMap and for `CMapName` inside the
/// embedded program. A mismatch would be ill-formed per the PDF spec, so every use is fed
/// from a single constant.
const TO_UNICODE_CMAP_NAME: Name<'static> = Name(b"Custom");

/// The value used both for `/CIDSystemInfo` in the `/ToUnicode` CMap and for `CIDSystemInfo`
/// inside the embedded program (always Adobe-UCS-0). Fed from a single constant for the same reason.
const TO_UNICODE_SYSTEM_INFO: SystemInfo<'static> = SystemInfo {
    registry: Str(b"Adobe"),
    ordering: Str(b"UCS"),
    supplement: 0,
};

/// The object IDs of one embedded font.
///
/// `cid_to_gid_map` is specific to [`embed_font_streaming_chunks`] (`embed_font` uses
/// `/CIDToGIDMap /Identity` and never refers to it).
#[derive(Debug, Clone, Copy)]
pub struct FontIds {
    pub font_file: Ref,
    pub descriptor: Ref,
    pub cid_font: Ref,
    pub type0_font: Ref,
    pub to_unicode: Ref,
    pub cid_to_gid_map: Ref,
}

/// What one font is used for, collected by walking the whole document first.
///
/// Glyphs are sorted into two groups. Ones drawn from outlines are subsetted
/// into a CIDFontType2; ones drawn in colour (embedded bitmaps, `COLR` v0) go
/// to a Type 3 font in [`super::color_font`]. That split is decided here once,
/// and the same table is consulted again when the content stream is written.
#[derive(Debug, Default)]
pub struct FontUsage {
    /// Original glyph ID -> (width in 1000-unit glyph space, the source text
    /// this glyph stands for).
    glyphs: BTreeMap<u16, (f32, String)>,
    /// Colour glyphs: original glyph ID -> its assignment.
    color: BTreeMap<u16, ColorGlyphUse>,
    /// The number the next colour glyph will be assigned.
    next_color: usize,
}

/// What one colour glyph was assigned.
#[derive(Debug)]
pub(super) struct ColorGlyphUse {
    /// The source text this glyph stands for, for `/ToUnicode`.
    pub text: String,
    /// Its running number within the document, from which both the Type 3
    /// font and the character code follow: code `index % 256` of font
    /// `index / 256`.
    pub index: usize,
}

impl FontUsage {
    /// Record a use of `glyph_id`. `text` is the source text it stands for,
    /// for building `/ToUnicode`: the cluster string looked back up from
    /// `ShapedGlyph::cluster`.
    ///
    /// It is not necessarily a single character, because of ligatures (`fl`
    /// becoming one glyph). Recording only one character would make text
    /// extraction and search read "float" as "foat".
    ///
    /// Several characters can also share one glyph. When a font has no
    /// `&nbsp;` glyph the shaper substitutes the space glyph (HarfBuzz's space
    /// fallback), so that glyph stands for both U+0020 and U+00A0 in the
    /// document. First-write-wins would mean that a single early `&nbsp;`
    /// makes every space in the document extract as U+00A0, breaking search
    /// and copy. On a collision the plain space wins.
    pub fn record(&mut self, font: &Font, glyph_id: u16, text: &str) {
        if let Some(existing) = self.color.get_mut(&glyph_id) {
            prefer_plain_space(&mut existing.text, text);
            return;
        }
        match self.glyphs.entry(glyph_id) {
            Entry::Occupied(mut slot) => prefer_plain_space(&mut slot.get_mut().1, text),
            Entry::Vacant(slot) => {
                // A glyph that can be drawn in colour goes to the Type 3
                // side. Type 3 is a simple font, so codes are one byte and
                // the number of fonts we reserve is capped; anything past the
                // cap falls back to the outline side.
                if self.next_color < MAX_COLOR_GLYPHS_PER_FACE && font.has_color_glyph(glyph_id) {
                    self.color.insert(
                        glyph_id,
                        ColorGlyphUse {
                            text: text.to_string(),
                            index: self.next_color,
                        },
                    );
                    self.next_color += 1;
                    return;
                }
                // A glyph from a font with no outlines and no colour
                // representation cannot be drawn at all. Recording it would
                // mean embedding a font program no viewer can read, so drop
                // it here.
                if !font.has_outlines() {
                    return;
                }
                let advance = font.glyph_hor_advance(glyph_id).unwrap_or(0) as f32;
                let width_1000 = advance * 1000.0 / font.units_per_em() as f32;
                slot.insert((width_1000, text.to_string()));
            }
        }
    }

    /// If `glyph_id` was recorded as a colour glyph, which Type 3 font it
    /// landed in and under which one-byte character code.
    pub(super) fn color_code(&self, glyph_id: u16) -> Option<(usize, u8)> {
        let index = self.color.get(&glyph_id)?.index;
        Some((
            index / CODES_PER_COLOR_FONT,
            (index % CODES_PER_COLOR_FONT) as u8,
        ))
    }

    /// The colour glyphs held by Type 3 font `ordinal`, in code order.
    pub(super) fn color_glyphs_of(&self, ordinal: usize) -> Vec<(u8, u16, &str)> {
        let mut out: Vec<(u8, u16, &str)> = self
            .color
            .iter()
            .filter(|(_, use_)| use_.index / CODES_PER_COLOR_FONT == ordinal)
            .map(|(&gid, use_)| {
                (
                    (use_.index % CODES_PER_COLOR_FONT) as u8,
                    gid,
                    use_.text.as_str(),
                )
            })
            .collect();
        out.sort_by_key(|(code, ..)| *code);
        out
    }

    /// How many Type 3 fonts the recorded colour glyphs need.
    pub(super) fn color_font_count(&self) -> usize {
        self.next_color.div_ceil(CODES_PER_COLOR_FONT)
    }
}

/// When a plain space and something like `&nbsp;` share one glyph, make
/// `/ToUnicode` point at the plain space; otherwise every space in the
/// document extracts as U+00A0 and search and copy break.
fn prefer_plain_space(recorded: &mut String, text: &str) {
    if text == " " && recorded != " " {
        *recorded = text.to_string();
    }
}

/// Embed `font` in the PDF (subsetting to only the glyphs recorded in `usage`).
///
/// Returns the mapping from original glyph ID to subsetted glyph ID (CID).
pub fn embed_font(
    pdf: &mut Pdf,
    font: &Font,
    ids: FontIds,
    usage: &FontUsage,
    compress: bool,
) -> BTreeMap<u16, u16> {
    let mut remapper = GlyphRemapper::new();
    remapper.remap(0); // .notdef
    for &old_gid in usage.glyphs.keys() {
        remapper.remap(old_gid);
    }

    let (subset_data, subsetted) = subset_or_whole(font, &remapper, subsetter::subset);
    let compressed = maybe_deflate(&subset_data, compress);
    let name = embedded_font_name(font, usage, subsetted);

    let mut font_file = pdf.stream(ids.font_file, &compressed);
    if compress {
        font_file.filter(Filter::FlateDecode);
    }
    // Length1 is the length of the font program itself *before* compression (as the PDF spec requires).
    font_file.pair(Name(b"Length1"), subset_data.len() as i32);
    font_file.finish();

    let units_per_em = font.units_per_em() as f32;
    let to_1000 = |font_units: f32| font_units * 1000.0 / units_per_em;
    let bbox = font.bounding_box();

    pdf.font_descriptor(ids.descriptor)
        .name(Name(name.as_bytes()))
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(PdfRect::new(
            to_1000(bbox.x_min as f32),
            to_1000(bbox.y_min as f32),
            to_1000(bbox.x_max as f32),
            to_1000(bbox.y_max as f32),
        ))
        .italic_angle(font.italic_angle())
        .ascent(to_1000(font.ascender() as f32))
        .descent(to_1000(font.descender() as f32))
        .cap_height(to_1000(
            font.capital_height().unwrap_or(font.ascender()) as f32
        ))
        .stem_v(if font.weight() >= 700 { 120.0 } else { 80.0 })
        .font_file2(ids.font_file);

    let old_to_new: BTreeMap<u16, u16> = usage
        .glyphs
        .keys()
        .map(|&old_gid| {
            let new_gid = remapper
                .get(old_gid)
                .expect("a glyph recorded in usage is always remapped");
            (old_gid, new_gid)
        })
        .collect();

    let mut cid_font = pdf.cid_font(ids.cid_font);
    cid_font.subtype(CidFontType::Type2);
    cid_font.base_font(Name(name.as_bytes()));
    cid_font.system_info(SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    });
    cid_font.font_descriptor(ids.descriptor);
    cid_font.default_width(0.0);
    {
        let mut w = cid_font.widths();
        for (&old_gid, (width, _)) in &usage.glyphs {
            let new_gid = old_to_new[&old_gid];
            w.same(new_gid, new_gid, *width);
        }
        w.finish();
    }
    cid_font.cid_to_gid_map_predefined(Name(b"Identity"));
    cid_font.finish();

    let mut cmap = UnicodeCmap::<u16>::new(TO_UNICODE_CMAP_NAME, TO_UNICODE_SYSTEM_INFO);
    for (&old_gid, (_, text)) in &usage.glyphs {
        cmap.pair_with_multiple(old_to_new[&old_gid], text.chars());
    }
    let cmap_bytes = maybe_deflate(&cmap.finish(), compress);
    let mut to_unicode = pdf.cmap(ids.to_unicode, &cmap_bytes);
    to_unicode.name(TO_UNICODE_CMAP_NAME);
    to_unicode.system_info(TO_UNICODE_SYSTEM_INFO);
    if compress {
        to_unicode.filter(Filter::FlateDecode);
    }
    to_unicode.finish();

    pdf.type0_font(ids.type0_font)
        .base_font(Name(name.as_bytes()))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(ids.cid_font)
        .to_unicode(ids.to_unicode);

    old_to_new
}

/// The streaming version of [`embed_font`]. CIDs are not repacked into subsetted glyph IDs
/// and always stay the original glyph IDs. Instead `/CIDToGIDMap` becomes an explicit stream
/// holding the mapping from CID (the original GID) to the subsetted GID.
///
/// The return value is each object as a `(Ref, Chunk)` pair. One `Chunk` contains exactly
/// one indirect object (so the caller can write each object to the `Sink` and record its
/// starting offset for the xref table). Writing them in the order returned is enough; no
/// reordering is needed.
pub fn embed_font_streaming_chunks(
    font: &Font,
    ids: FontIds,
    usage: &FontUsage,
    compress: bool,
) -> Vec<(Ref, Chunk)> {
    let mut chunks = Vec::with_capacity(6);

    let mut remapper = GlyphRemapper::new();
    remapper.remap(0); // .notdef
    for &old_gid in usage.glyphs.keys() {
        remapper.remap(old_gid);
    }
    let (subset_data, subsetted) = subset_or_whole(font, &remapper, subsetter::subset);

    let compressed_font = maybe_deflate(&subset_data, compress);
    let name = embedded_font_name(font, usage, subsetted);
    let mut chunk = Chunk::new();
    let mut font_file = chunk.stream(ids.font_file, &compressed_font);
    if compress {
        font_file.filter(Filter::FlateDecode);
    }
    font_file.pair(Name(b"Length1"), subset_data.len() as i32);
    font_file.finish();
    chunks.push((ids.font_file, chunk));

    let units_per_em = font.units_per_em() as f32;
    let to_1000 = |font_units: f32| font_units * 1000.0 / units_per_em;
    let bbox = font.bounding_box();

    let mut chunk = Chunk::new();
    chunk
        .font_descriptor(ids.descriptor)
        .name(Name(name.as_bytes()))
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(PdfRect::new(
            to_1000(bbox.x_min as f32),
            to_1000(bbox.y_min as f32),
            to_1000(bbox.x_max as f32),
            to_1000(bbox.y_max as f32),
        ))
        .italic_angle(font.italic_angle())
        .ascent(to_1000(font.ascender() as f32))
        .descent(to_1000(font.descender() as f32))
        .cap_height(to_1000(
            font.capital_height().unwrap_or(font.ascender()) as f32
        ))
        .stem_v(if font.weight() >= 700 { 120.0 } else { 80.0 })
        .font_file2(ids.font_file);
    chunks.push((ids.descriptor, chunk));

    // CIDToGIDMap: a table of two-byte GID values indexed by CID (= the original GID).
    // Unused CIDs stay 0 (.notdef).
    let max_gid = usage.glyphs.keys().copied().max().unwrap_or(0);
    let mut cid_to_gid_bytes = vec![0u8; (max_gid as usize + 1) * 2];
    for &old_gid in usage.glyphs.keys() {
        let new_gid = remapper
            .get(old_gid)
            .expect("a glyph recorded in usage is always remapped");
        let idx = old_gid as usize * 2;
        cid_to_gid_bytes[idx..idx + 2].copy_from_slice(&new_gid.to_be_bytes());
    }
    let compressed_cid_to_gid = maybe_deflate(&cid_to_gid_bytes, compress);
    let mut chunk = Chunk::new();
    let mut cid_to_gid_stream = chunk.stream(ids.cid_to_gid_map, &compressed_cid_to_gid);
    if compress {
        cid_to_gid_stream.filter(Filter::FlateDecode);
    }
    cid_to_gid_stream.finish();
    chunks.push((ids.cid_to_gid_map, chunk));

    let mut chunk = Chunk::new();
    let mut cid_font = chunk.cid_font(ids.cid_font);
    cid_font.subtype(CidFontType::Type2);
    cid_font.base_font(Name(name.as_bytes()));
    cid_font.system_info(SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    });
    cid_font.font_descriptor(ids.descriptor);
    cid_font.default_width(0.0);
    {
        // `/W` can be keyed on the original glyph ID (= CID) and written with the same
        // values as before subsetting (widths were recorded against original GIDs, so no conversion).
        let mut w = cid_font.widths();
        for (&old_gid, (width, _)) in &usage.glyphs {
            w.same(old_gid, old_gid, *width);
        }
        w.finish();
    }
    // Rather than Identity, use an explicit map to the real subsetted glyph positions.
    cid_font.cid_to_gid_map_stream(ids.cid_to_gid_map);
    cid_font.finish();
    chunks.push((ids.cid_font, chunk));

    let mut cmap = UnicodeCmap::<u16>::new(TO_UNICODE_CMAP_NAME, TO_UNICODE_SYSTEM_INFO);
    for (&old_gid, (_, text)) in &usage.glyphs {
        cmap.pair_with_multiple(old_gid, text.chars());
    }
    let cmap_bytes = maybe_deflate(&cmap.finish(), compress);
    let mut chunk = Chunk::new();
    let mut to_unicode = chunk.cmap(ids.to_unicode, &cmap_bytes);
    to_unicode.name(TO_UNICODE_CMAP_NAME);
    to_unicode.system_info(TO_UNICODE_SYSTEM_INFO);
    if compress {
        to_unicode.filter(Filter::FlateDecode);
    }
    to_unicode.finish();
    chunks.push((ids.to_unicode, chunk));

    let mut chunk = Chunk::new();
    chunk
        .type0_font(ids.type0_font)
        .base_font(Name(name.as_bytes()))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(ids.cid_font)
        .to_unicode(ids.to_unicode);
    chunks.push((ids.type0_font, chunk));

    chunks
}

/// The font program to embed: `font` cut down to the glyphs in `remapper` by `subset`, or the
/// whole file when that fails. The flag says which one it is.
fn subset_or_whole<E>(
    font: &Font,
    remapper: &GlyphRemapper,
    subset: impl FnOnce(&[u8], u32, &GlyphRemapper) -> Result<Vec<u8>, E>,
) -> (Vec<u8>, bool) {
    match subset(font.data(), font.face_index(), remapper) {
        Ok(data) => (data, true),
        Err(_) => (font.data().to_vec(), false),
    }
}

/// The name of an embedded font: the font's PostScript name, behind a six-letter tag and a
/// `+` when only a subset is embedded (`ABCDEF+DMSans-Regular`), as ISO 32000-1 9.6.4
/// requires. It is the `/FontName` of the descriptor and the `/BaseFont` of the CIDFont and,
/// for a CIDFontType2 as the spec says, of the Type0 font as well.
///
/// The tag is derived from the font and the glyphs in the subset, so different subsets of one
/// font get different tags and identical input gives identical output. A whole font, which
/// the spec does not give a tag, is named without one.
fn embedded_font_name(font: &Font, usage: &FontUsage, subsetted: bool) -> String {
    if !subsetted {
        return font.pdf_name().to_string();
    }
    format!(
        "{}+{}",
        subset_tag(font, usage.glyphs.keys().copied()),
        font.pdf_name()
    )
}

/// Six uppercase letters taken from an FNV-1a hash of the font's identity (name, face and
/// size of the file) and the glyph IDs of the subset. Hashing the glyph IDs rather than the
/// subsetted bytes keeps the cost off the font's size.
fn subset_tag(font: &Font, glyph_ids: impl Iterator<Item = u16>) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for &byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    feed(font.pdf_name().as_bytes());
    feed(&[0]);
    feed(&font.face_index().to_be_bytes());
    feed(&(font.data().len() as u64).to_be_bytes());
    for glyph_id in glyph_ids {
        feed(&glyph_id.to_be_bytes());
    }
    (0..6)
        .map(|_| {
            let letter = (b'A' + (hash % 26) as u8) as char;
            hash /= 26;
            letter
        })
        .collect()
}

/// Compress with zlib (`/FlateDecode`). Returns the data uncompressed when `compress` is
/// false (`--no-pdf-compression`). The caller decides whether to write `/Filter` on the same
/// condition.
pub(super) fn maybe_deflate(data: &[u8], compress: bool) -> Vec<u8> {
    if compress {
        deflate(data)
    } else {
        data.to_vec()
    }
}

pub(super) fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .expect("writing to an in-memory buffer cannot fail");
    encoder
        .finish()
        .expect("writing to an in-memory buffer cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_shrinks_compressible_data() {
        let data = vec![b'A'; 10_000];
        let compressed = deflate(&data);
        assert!(
            compressed.len() < data.len() / 10,
            "highly repetitive data should compress well: {} -> {}",
            data.len(),
            compressed.len()
        );
    }

    const TEST_FONT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    /// In a font with no `&nbsp;` glyph the shaper substitutes the space glyph, so one glyph
    /// represents both. Even then `/ToUnicode` should point at the ordinary space (otherwise
    /// every space in the document extracts as U+00A0 and text search and copying break).
    #[test]
    fn a_glyph_shared_by_a_space_and_a_no_break_space_maps_to_the_space() {
        let font = Font::load(TEST_FONT).expect("should load bundled test font");
        let space_glyph = 3;

        // The space wins even when the `&nbsp;` came first.
        let mut nbsp_first = FontUsage::default();
        nbsp_first.record(&font, space_glyph, "\u{a0}");
        nbsp_first.record(&font, space_glyph, " ");
        assert_eq!(nbsp_first.glyphs[&space_glyph].1, " ");

        // And stays the space in the reverse order too (`&nbsp;` does not overwrite it).
        let mut space_first = FontUsage::default();
        space_first.record(&font, space_glyph, " ");
        space_first.record(&font, space_glyph, "\u{a0}");
        assert_eq!(space_first.glyphs[&space_glyph].1, " ");
    }

    #[test]
    fn a_ligature_cluster_keeps_the_text_it_was_first_recorded_with() {
        // A ligature represents several characters with one glyph. Preferring the space must not break that record.
        let font = Font::load(TEST_FONT).expect("should load bundled test font");
        let mut usage = FontUsage::default();
        usage.record(&font, 100, "fl");
        usage.record(&font, 100, "fl");

        assert_eq!(usage.glyphs[&100].1, "fl");
    }

    #[test]
    fn a_subset_tag_is_six_uppercase_letters_that_follow_the_glyphs() {
        let font = Font::load(TEST_FONT).expect("should load bundled test font");
        let tag = subset_tag(&font, [3, 36, 68].into_iter());
        assert_eq!(tag.len(), 6);
        assert!(tag.bytes().all(|b| b.is_ascii_uppercase()));
        assert_eq!(tag, subset_tag(&font, [3, 36, 68].into_iter()));
        assert_ne!(tag, subset_tag(&font, [3, 36, 69].into_iter()));
    }

    #[test]
    fn a_font_embedded_whole_is_named_without_a_subset_tag() {
        let font = Font::load(TEST_FONT).expect("should load bundled test font");
        let mut usage = FontUsage::default();
        usage.record(&font, 36, "A");
        let remapper = GlyphRemapper::new();

        let (data, subsetted) = subset_or_whole(&font, &remapper, |_, _, _| {
            Err::<Vec<u8>, _>("cannot subset")
        });
        assert!(!subsetted);
        assert_eq!(data, font.data());
        assert_eq!(embedded_font_name(&font, &usage, subsetted), "DejaVuSans");

        let (_, subsetted) = subset_or_whole(&font, &remapper, |_, _, _| Ok::<_, ()>(vec![0]));
        assert!(subsetted);
        let name = embedded_font_name(&font, &usage, subsetted);
        assert!(name.ends_with("+DejaVuSans") && name.len() == 17, "{name}");
    }

    #[test]
    fn deflate_output_round_trips_via_zlib_decoder() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(50);
        let compressed = deflate(&data);

        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed).unwrap();

        assert_eq!(decompressed, data);
    }
}
