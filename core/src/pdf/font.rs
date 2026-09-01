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

/// The usage of one font (collected by scanning the whole document in a first pass).
#[derive(Debug, Default)]
pub struct FontUsage {
    /// Original glyph ID -> (width in 1000-unit/em glyph space, the original text that glyph represents).
    glyphs: BTreeMap<u16, (f32, String)>,
}

impl FontUsage {
    /// Record a use of `glyph_id`. `text` is the original text for `/ToUnicode` generation
    /// (the cluster string recovered from `ShapedGlyph::cluster`).
    ///
    /// It is not necessarily one character, because of ligatures (`fl` becoming one glyph,
    /// say). Keeping only one character would make "float" extract and search as "foat".
    ///
    /// Several characters can also share one glyph. When a font lacks a `&nbsp;` glyph the
    /// shaper substitutes the space glyph (HarfBuzz's space fallback), so that glyph
    /// represents both U+0020 and U+00A0 in the document. Leaving it first-wins would mean
    /// that one `&nbsp;` appearing earlier makes every later space extract as U+00A0,
    /// breaking text search and copying. On a collision, the ordinary space wins.
    pub fn record(&mut self, font: &Font, glyph_id: u16, text: &str) {
        match self.glyphs.entry(glyph_id) {
            Entry::Vacant(slot) => {
                let advance = font.glyph_hor_advance(glyph_id).unwrap_or(0) as f32;
                let width_1000 = advance * 1000.0 / font.units_per_em() as f32;
                slot.insert((width_1000, text.to_string()));
            }
            Entry::Occupied(mut slot) => {
                if text == " " && slot.get().1 != " " {
                    slot.get_mut().1 = text.to_string();
                }
            }
        }
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

    let subset_data = subsetter::subset(font.data(), font.face_index(), &remapper)
        .unwrap_or_else(|_| font.data().to_vec());
    let compressed = maybe_deflate(&subset_data, compress);

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
        .name(Name(b"EmbeddedFont"))
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
    cid_font.base_font(Name(b"EmbeddedFont"));
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
        .base_font(Name(b"EmbeddedFont"))
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
    let subset_data = subsetter::subset(font.data(), font.face_index(), &remapper)
        .unwrap_or_else(|_| font.data().to_vec());

    let compressed_font = maybe_deflate(&subset_data, compress);
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
        .name(Name(b"EmbeddedFont"))
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
    cid_font.base_font(Name(b"EmbeddedFont"));
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
        .base_font(Name(b"EmbeddedFont"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(ids.cid_font)
        .to_unicode(ids.to_unicode);
    chunks.push((ids.type0_font, chunk));

    chunks
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
    fn deflate_output_round_trips_via_zlib_decoder() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(50);
        let compressed = deflate(&data);

        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed).unwrap();

        assert_eq!(decompressed, data);
    }
}
