//! Spike: a PoC checking whether font subsetting and per-page streaming output can coexist.
//!
//! Today's `pdf::document::encode_pdf` is a two-pass design: "pass 1 walks every page and
//! tallies glyph usage, subsets (repacking the glyph IDs), and pass 2 writes the content
//! streams". The content stream itself depends on the subsetting result (the original GID to
//! CID table). That means no page's content stream can be written until the whole-document
//! walk is done, which is incompatible with streaming output that writes each page as it is
//! settled.
//!
//! What is checked here is whether the following design breaks that dependency:
//! - The content stream always uses the original glyph IDs as CIDs
//!   (still `/Encoding /Identity-H`, with no repacking). That lets a page's content stream
//!   be settled and written the moment that page's shaping is done, without waiting on any
//!   other page
//! - Font embedding (the `/FontFile2` itself) is still subsetted, but `/CIDToGIDMap` becomes
//!   an explicit stream (`cid_to_gid_map_stream`) holding the CID (= original GID) to
//!   subsetted GID mapping rather than `/Identity`, reconciling the content stream's CIDs
//!   (original GIDs) with the subsetted font
//!
//! With that, all that has to be kept as each page is settled is the lightweight `FontUsage`
//! (a tally of glyph IDs, widths and representative Unicode characters); the layout result
//! and the content stream bytes themselves can be discarded each time. The font embedding
//! objects are still written once after every page (appended as a Chunk at the end).
//!
//! Run with: `cargo run --example spike_streaming_font_subset`
//! Check with: `python3 -c "import fitz; d=fitz.open('target/spike_streaming_font_subset.pdf'); \
//!   print([p.get_text() for p in d])"` for text extraction, plus a visual check of the glyphs.

use std::collections::BTreeMap;

use pdf_writer::types::{CidFontType, FontFlags, SystemInfo};
use pdf_writer::writers::Catalog;
use pdf_writer::{Chunk, Content, Filter, Finish, Name, Rect as PdfRect, Ref, Str};
use sghtmltopdf_core::fonts::{shape_text, Font};
use subsetter::GlyphRemapper;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

/// A fake Sink writing the bytes out the moment each page is settled (as in spike_pdf_writer.rs).
struct FakeSink {
    output: Vec<u8>,
    offsets: Vec<(Ref, usize)>,
}

impl FakeSink {
    fn new() -> Self {
        let output = b"%PDF-1.7\n%\x80\x80\x80\x80\n\n".to_vec();
        Self {
            output,
            offsets: Vec::new(),
        }
    }

    fn write_chunk(&mut self, id: Ref, chunk: &Chunk) {
        self.offsets.push((id, self.output.len()));
        self.output.extend_from_slice(chunk.as_bytes());
    }

    fn finish(mut self, root: Ref) -> Vec<u8> {
        let xref_offset = self.output.len();
        let size = self
            .offsets
            .iter()
            .map(|(id, _)| id.get())
            .max()
            .unwrap_or(0)
            + 1;

        self.offsets.sort_by_key(|(id, _)| id.get());
        self.output
            .extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        self.output.extend_from_slice(b"0000000000 65535 f \n");
        for (_, offset) in &self.offsets {
            self.output
                .extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }

        self.output.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root {} 0 R >>\n", root.get()).as_bytes(),
        );
        self.output
            .extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());
        self.output
    }
}

/// A lightweight tally of the glyphs used across the document (the content streams themselves are not kept).
#[derive(Default)]
struct FontUsage {
    /// Original glyph ID -> (width in 1000-unit/em glyph space, a representative Unicode character).
    glyphs: BTreeMap<u16, (f32, char)>,
}

fn main() {
    let font = Font::load(FONT_PATH).expect("should load bundled test font");
    let units_per_em = font.units_per_em() as f32;
    let to_1000 = |font_units: f32| font_units * 1000.0 / units_per_em;

    // Two pages, each with different text (that is, a different glyph set).
    // Page 2 shares some glyphs with page 1 while also containing characters absent from page 1 ("Q", "Z" and so on).
    let page_texts = ["Hello, world!", "Quick zebra jumps."];

    let mut ids = 0..;
    let mut next_id = || Ref::new(ids.next().unwrap() + 1);

    let catalog_id = next_id();
    let pages_tree_id = next_id();
    let font_file_id = next_id();
    let descriptor_id = next_id();
    let cid_font_id = next_id();
    let type0_font_id = next_id();
    let cid_to_gid_id = next_id();

    let mut sink = FakeSink::new();
    let mut usage = FontUsage::default();
    let mut page_ids = Vec::new();

    // --- Per-page incremental processing: the content stream is settled and written as a
    //     Chunk right after shaping, without waiting on any other page ---
    for text in page_texts {
        let page_id = next_id();
        let content_id = next_id();
        page_ids.push(page_id);

        let shaped = shape_text(&font, text, 24.0);

        // CIDs are not remapped; the original glyph IDs are always used as-is.
        let mut glyph_bytes = Vec::with_capacity(shaped.glyphs.len() * 2);
        for g in &shaped.glyphs {
            glyph_bytes.extend_from_slice(&g.glyph_id.to_be_bytes());
            let unicode = text[g.cluster as usize..].chars().next().unwrap_or('?');
            usage.glyphs.entry(g.glyph_id).or_insert_with(|| {
                let advance = font.glyph_hor_advance(g.glyph_id).unwrap_or(0) as f32;
                (to_1000(advance), unicode)
            });
        }

        let mut content = Content::new();
        content.begin_text();
        content.set_font(Name(b"F1"), 24.0);
        content.next_line(20.0, 90.0);
        content.show(Str(&glyph_bytes));
        content.end_text();

        let mut chunk = Chunk::new();
        chunk.stream(content_id, &content.finish());
        sink.write_chunk(content_id, &chunk);

        let mut chunk = Chunk::new();
        chunk
            .page(page_id)
            .parent(pages_tree_id)
            .media_box(PdfRect::new(0.0, 0.0, 300.0, 150.0))
            .contents(content_id)
            .resources()
            .fonts()
            .pair(Name(b"F1"), type0_font_id);
        sink.write_chunk(page_id, &chunk);
    }

    // --- From here on is the one-off post-processing after every page. All that was kept is
    //     the lightweight `usage` (the glyph ID set); no raw content stream data and no
    //     layout result was retained ---

    let mut remapper = GlyphRemapper::new();
    remapper.remap(0); // .notdef
    for &old_gid in usage.glyphs.keys() {
        remapper.remap(old_gid);
    }
    let subset_data = subsetter::subset(font.data(), font.face_index(), &remapper)
        .expect("subsetting should succeed for the bundled test font");

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

    let compressed_cid_to_gid = deflate(&cid_to_gid_bytes);
    let mut chunk = Chunk::new();
    let mut cid_to_gid_stream = chunk.stream(cid_to_gid_id, &compressed_cid_to_gid);
    cid_to_gid_stream.filter(Filter::FlateDecode);
    cid_to_gid_stream.finish();
    sink.write_chunk(cid_to_gid_id, &chunk);

    let compressed_font = deflate(&subset_data);
    let mut chunk = Chunk::new();
    let mut font_file = chunk.stream(font_file_id, &compressed_font);
    font_file.filter(Filter::FlateDecode);
    font_file.pair(Name(b"Length1"), subset_data.len() as i32);
    font_file.finish();
    sink.write_chunk(font_file_id, &chunk);

    let bbox = font.bounding_box();
    let mut chunk = Chunk::new();
    chunk
        .font_descriptor(descriptor_id)
        .name(Name(b"DejaVuSans"))
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
        .stem_v(80.0)
        .font_file2(font_file_id);
    sink.write_chunk(descriptor_id, &chunk);

    let mut chunk = Chunk::new();
    let mut cid_font = chunk.cid_font(cid_font_id);
    cid_font.subtype(CidFontType::Type2);
    cid_font.base_font(Name(b"DejaVuSans"));
    cid_font.system_info(SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    });
    cid_font.font_descriptor(descriptor_id);
    cid_font.default_width(0.0);
    {
        // `/W` can be keyed on the original glyph ID (= CID) and written with the same values
        // as before subsetting (widths were recorded against original GIDs, so no conversion).
        let mut w = cid_font.widths();
        for (&old_gid, &(width, _)) in &usage.glyphs {
            w.same(old_gid, old_gid, width);
        }
        w.finish();
    }
    // Rather than Identity, use an explicit map to the real subsetted glyph positions.
    cid_font.cid_to_gid_map_stream(cid_to_gid_id);
    cid_font.finish();
    sink.write_chunk(cid_font_id, &chunk);

    let mut chunk = Chunk::new();
    chunk
        .type0_font(type0_font_id)
        .base_font(Name(b"DejaVuSans"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(cid_font_id);
    sink.write_chunk(type0_font_id, &chunk);

    let mut chunk = Chunk::new();
    chunk
        .pages(pages_tree_id)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);
    sink.write_chunk(pages_tree_id, &chunk);

    let mut chunk = Chunk::new();
    chunk
        .indirect(catalog_id)
        .start::<Catalog>()
        .pages(pages_tree_id);
    sink.write_chunk(catalog_id, &chunk);

    let pdf = sink.finish(catalog_id);

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/spike_streaming_font_subset.pdf");
    std::fs::write(&out, &pdf).unwrap();
    eprintln!(
        "wrote {} bytes to {} (max_gid={max_gid}, used_glyphs={})",
        pdf.len(),
        out.display(),
        usage.glyphs.len()
    );
}

fn deflate(data: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}
