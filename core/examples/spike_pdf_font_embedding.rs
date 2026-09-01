//! T9 spike: a PoC embedding a TrueType font with pdf-writer and generating a PDF that draws
//! text with its real glyphs.
//!
//! What we want to check:
//! - Whether a TrueType font can be embedded as a CIDFontType2 (Identity-H encoding) and the
//!   glyph ID sequence T7's `shape_text()` returns used directly for PDF text drawing
//! - Whether the glyphs of the actually embedded font are displayed, rather than a base14
//!   font (as in T1's spike_krilla and spike_pdf_writer), accented characters included
//!
//! Run with: `cargo run --example spike_pdf_font_embedding`
//! (it uses the bundled test font added in T7, `core/tests/fonts/DejaVuSans.ttf`)

use std::collections::BTreeMap;

use pdf_writer::{Content, Finish, Name, Pdf, Rect as PdfRect, Ref, Str};
use sghtmltopdf_core::fonts::{shape_text, Font};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

fn main() {
    let font = Font::load(FONT_PATH).expect("should load bundled test font");

    let text = "Hello, world! caf\u{e9} r\u{e9}sum\u{e9}";
    let font_size = 24.0;
    let shaped = shape_text(&font, text, font_size);

    // Identity-H: a two-byte code is used directly as the CID (= GlyphID, CIDToGIDMap=Identity).
    let mut glyph_bytes = Vec::with_capacity(shaped.glyphs.len() * 2);
    for g in &shaped.glyphs {
        glyph_bytes.extend_from_slice(&g.glyph_id.to_be_bytes());
    }

    // PDF's /W is expressed in a 1000-unit/em glyph space.
    let units_per_em = font.units_per_em() as f32;
    let to_1000 = |font_units: f32| font_units * 1000.0 / units_per_em;

    // The width of each glyph used (glyph IDs are not contiguous, so they are recorded individually).
    let mut widths: BTreeMap<u16, f32> = BTreeMap::new();
    for g in &shaped.glyphs {
        widths.entry(g.glyph_id).or_insert_with(|| {
            let advance = font.glyph_hor_advance(g.glyph_id).unwrap_or(0) as f32;
            to_1000(advance)
        });
    }

    let mut ids = 0..;
    let mut next_id = || Ref::new(ids.next().unwrap() + 1);

    let catalog_id = next_id();
    let pages_tree_id = next_id();
    let page_id = next_id();
    let content_id = next_id();
    let font_file_id = next_id();
    let descriptor_id = next_id();
    let cid_font_id = next_id();
    let type0_font_id = next_id();

    let mut pdf = Pdf::new();

    pdf.catalog(catalog_id).pages(pages_tree_id);
    pdf.pages(pages_tree_id).kids([page_id]).count(1);

    let mut page = pdf.page(page_id);
    page.parent(pages_tree_id);
    page.media_box(PdfRect::new(0.0, 0.0, 300.0, 150.0));
    page.contents(content_id);
    page.resources().fonts().pair(Name(b"F1"), type0_font_id);
    page.finish();

    let mut content = Content::new();
    content.begin_text();
    content.set_font(Name(b"F1"), font_size);
    content.next_line(20.0, 90.0);
    content.show(Str(&glyph_bytes));
    content.end_text();
    pdf.stream(content_id, &content.finish());

    // Embed the font program itself (TrueType, uncompressed).
    let font_data = font.data();
    let mut font_file = pdf.stream(font_file_id, font_data);
    font_file.pair(Name(b"Length1"), font_data.len() as i32);
    font_file.finish();

    let bbox = font.bounding_box();
    pdf.font_descriptor(descriptor_id)
        .name(Name(b"DejaVuSans"))
        .flags(pdf_writer::types::FontFlags::NON_SYMBOLIC)
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
        .font_file2(font_file_id);

    let mut cid_font = pdf.cid_font(cid_font_id);
    cid_font.subtype(pdf_writer::types::CidFontType::Type2);
    cid_font.base_font(Name(b"DejaVuSans"));
    cid_font.system_info(pdf_writer::types::SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    });
    cid_font.font_descriptor(descriptor_id);
    cid_font.default_width(0.0);
    {
        let mut w = cid_font.widths();
        for (&gid, &width) in &widths {
            w.same(gid, gid, width);
        }
        w.finish();
    }
    cid_font.cid_to_gid_map_predefined(Name(b"Identity"));
    cid_font.finish();

    pdf.type0_font(type0_font_id)
        .base_font(Name(b"DejaVuSans"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(cid_font_id);

    let bytes = pdf.finish();

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/spike_pdf_font_embedding.pdf");
    std::fs::write(&out, &bytes).unwrap();
    eprintln!("wrote {} bytes to {}", bytes.len(), out.display());
}
