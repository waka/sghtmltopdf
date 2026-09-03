//! Writing colour glyphs to the PDF as Type 3 fonts.
//!
//! Embedded-bitmap and `COLR`/`CPAL` v0 glyphs go into a font of their own,
//! separate from the outline glyphs. A Type 3 font is one whose glyphs are
//! each a small content stream (`/CharProcs`), so a glyph can draw an image
//! XObject or fill paths in solid colours directly.
//!
//! Type 3 wins on three counts:
//!
//! * The glyphs stay text. `/ToUnicode`, extraction, search and selection all
//!   work exactly as they do for ordinary characters, and positioning goes
//!   through the normal text machinery.
//! * The original font program is never embedded. A bitmap-only font like
//!   Noto Color Emoji has no `glyf`, so subsetting cannot shrink it (all
//!   10MB would come along) and viewers reject it as a font anyway.
//! * Placing an image and filling a path use the same mechanism.
//!
//! Type 3 is a simple font, so character codes are one byte and a single font
//! holds at most 256 glyphs. Anything beyond that goes into another one.

use pdf_writer::types::{SystemInfo, UnicodeCmap};
use pdf_writer::{Chunk, Content, Filter, Finish, Name, Rect as PdfRect, Ref, Str};

use crate::fonts::{ColorGlyph, Font, FontCollection};

use super::document::RefAllocator;
use super::font::{maybe_deflate, FontIds, FontUsage};
use super::img::{decode_png, has_alpha_plane, image_resource_name, raster_plane_chunks};
use super::options::PdfOutputOptions;

/// How many glyphs fit in one Type 3 font (character codes are one byte).
pub(super) const CODES_PER_COLOR_FONT: usize = 256;

/// The most Type 3 fonts we will allocate for a single face.
///
/// Streaming writes each page's resource dictionary the moment the page is
/// final, so a font cannot be added once we discover it is needed. We reserve
/// this many object IDs up front and write the unused ones as empty Type 3
/// fonts.
const MAX_COLOR_FONTS_PER_FACE: usize = 4;

/// How many colour glyphs one face can contribute. Beyond this a glyph falls
/// back to its outline, or to nothing if the font has none.
pub(super) const MAX_COLOR_GLYPHS_PER_FACE: usize = MAX_COLOR_FONTS_PER_FACE * CODES_PER_COLOR_FONT;

/// `/ToUnicode` CMap name and CIDSystemInfo, matching what the CIDFont side
/// (`super::font`) writes.
const TO_UNICODE_CMAP_NAME: Name<'static> = Name(b"Custom");
const TO_UNICODE_SYSTEM_INFO: SystemInfo<'static> = SystemInfo {
    registry: Str(b"Adobe"),
    ordering: Str(b"UCS"),
    supplement: 0,
};

/// The font used for ordinary outline glyphs (CIDFontType2 + Type0).
pub(super) struct SimpleFont {
    /// The name this font is registered under in a page's `/Resources /Font`.
    pub name: String,
    pub ids: FontIds,
}

/// One Type 3 font holding colour glyphs.
pub(super) struct ColorFont {
    pub name: String,
    pub font: Ref,
    pub to_unicode: Ref,
}

/// Which font resources the document has allocated.
///
/// For each font in the [`FontCollection`], this holds the Type0 font used for
/// outline glyphs and however many Type 3 fonts were reserved for its colour
/// glyphs. A page's `/Resources /Font` is simply everything listed here.
pub(super) struct FontPlan {
    /// For outline glyphs. `None` for a font with no outlines (a bitmap-only
    /// colour emoji font), whose font program is never embedded at all.
    simple: Vec<Option<SimpleFont>>,
    color: Vec<Vec<ColorFont>>,
}

impl FontPlan {
    /// `color_font_counts[i]` is how many Type 3 fonts to reserve for font `i`.
    ///
    /// Batch mode knows the real count after its first pass; streaming mode
    /// cannot, and passes the upper bound ([`MAX_COLOR_FONTS_PER_FACE`]).
    pub(super) fn new(
        fonts: &FontCollection,
        alloc: &mut RefAllocator,
        color_font_counts: &[usize],
    ) -> Self {
        let mut simple = Vec::with_capacity(fonts.len());
        let mut color = Vec::with_capacity(fonts.len());
        for (index, font) in fonts.fonts().iter().enumerate() {
            simple.push(font.has_outlines().then(|| SimpleFont {
                name: format!("F{index}"),
                ids: FontIds {
                    font_file: alloc.next(),
                    descriptor: alloc.next(),
                    cid_font: alloc.next(),
                    type0_font: alloc.next(),
                    to_unicode: alloc.next(),
                    // `encode_pdf` uses `/CIDToGIDMap /Identity` and never
                    // reads this, but allocate it so `FontIds` stays one type
                    // shared with the streaming writer.
                    cid_to_gid_map: alloc.next(),
                },
            }));
            let count = color_font_counts
                .get(index)
                .copied()
                .unwrap_or(0)
                .min(MAX_COLOR_FONTS_PER_FACE);
            color.push(
                (0..count)
                    .map(|ordinal| ColorFont {
                        name: format!("C{index}_{ordinal}"),
                        font: alloc.next(),
                        to_unicode: alloc.next(),
                    })
                    .collect(),
            );
        }
        Self { simple, color }
    }

    /// For streaming: reserve the upper bound for every font that could
    /// contribute colour glyphs.
    pub(super) fn upper_bound_counts(fonts: &FontCollection) -> Vec<usize> {
        fonts
            .fonts()
            .iter()
            .map(|font| {
                if font.has_color_glyphs() {
                    MAX_COLOR_FONTS_PER_FACE
                } else {
                    0
                }
            })
            .collect()
    }

    pub(super) fn simple(&self, index: usize) -> Option<&SimpleFont> {
        self.simple.get(index)?.as_ref()
    }

    pub(super) fn color(&self, index: usize, ordinal: usize) -> Option<&ColorFont> {
        self.color.get(index)?.get(ordinal)
    }

    /// The (name, object ID) pairs to write into a page's `/Resources /Font`.
    pub(super) fn resource_entries(&self) -> impl Iterator<Item = (&str, Ref)> {
        let simple = self
            .simple
            .iter()
            .flatten()
            .map(|f| (f.name.as_str(), f.ids.type0_font));
        let color = self
            .color
            .iter()
            .flatten()
            .map(|f| (f.name.as_str(), f.font));
        simple.chain(color)
    }
}

/// Write out every Type 3 font that was allocated.
///
/// Returns a list of `(Ref, Chunk)`, one indirect object per chunk: batch mode
/// `extend`s them into the `Pdf`, streaming mode sends them to the `Sink` in
/// order.
///
/// Slots that ended up unused (streaming reserved the upper bound but the
/// document needed fewer) are still referenced from every page's resource
/// dictionary, so they have to exist. They are written as empty Type 3 fonts.
pub(super) fn write_color_fonts(
    fonts: &FontCollection,
    plan: &FontPlan,
    usages: &[FontUsage],
    alloc: &mut RefAllocator,
    output: &PdfOutputOptions,
) -> Vec<(Ref, Chunk)> {
    let mut chunks = Vec::new();
    for (index, font) in fonts.fonts().iter().enumerate() {
        let empty = FontUsage::default();
        let usage = usages.get(index).unwrap_or(&empty);
        let mut ordinal = 0;
        while let Some(slot) = plan.color(index, ordinal) {
            write_one_color_font(
                &mut chunks,
                font,
                slot,
                &usage.color_glyphs_of(ordinal),
                alloc,
                output,
            );
            ordinal += 1;
        }
    }
    chunks
}

/// Write one Type 3 font, along with the image XObjects, glyph procedures and
/// `/ToUnicode` CMap its glyphs need.
fn write_one_color_font(
    chunks: &mut Vec<(Ref, Chunk)>,
    font: &Font,
    slot: &ColorFont,
    glyphs: &[(u8, u16, &str)],
    alloc: &mut RefAllocator,
    output: &PdfOutputOptions,
) {
    let units_per_em = font.units_per_em().max(1) as f32;

    // The glyph procedures, and the image XObjects they refer to.
    let mut procs: Vec<(u8, Ref, f32)> = Vec::with_capacity(glyphs.len());
    let mut images: Vec<Ref> = Vec::new();
    let mut bbox = GlyphBBox::default();
    for &(code, glyph_id, _) in glyphs {
        let advance = font.glyph_hor_advance(glyph_id).unwrap_or(0) as f32;
        let mut content = Content::new();
        // d0, not d1: this glyph decides its own colour.
        content.start_color_glyph(advance);
        match font.color_glyph(glyph_id) {
            Some(ColorGlyph::Bitmap(bitmap)) => {
                if let Some(image_ref) =
                    write_bitmap_image(chunks, &bitmap.png, alloc, output.grayscale)
                {
                    images.push(image_ref);
                    bbox.include(bitmap.x_min, bitmap.y_min, bitmap.x_max, bitmap.y_max);
                    content.save_state();
                    // An image XObject fills the unit square, so map that
                    // square onto the placement rectangle.
                    content.transform([
                        bitmap.x_max - bitmap.x_min,
                        0.0,
                        0.0,
                        bitmap.y_max - bitmap.y_min,
                        bitmap.x_min,
                        bitmap.y_min,
                    ]);
                    content.x_object(Name(image_resource_name(image_ref).as_bytes()));
                    content.restore_state();
                }
            }
            Some(ColorGlyph::LayersV0(layers)) => {
                bbox.include_font_bbox(font);
                for layer in &layers {
                    // A layer with no colour of its own (CPAL palette index
                    // 0xFFFF) inherits the text colour the caller set before
                    // `Tf`.
                    //
                    // Layer alpha is ignored. Translucency would need an
                    // ExtGState, and so another kind of resource reachable
                    // from a glyph procedure, for a case that barely occurs:
                    // COLRv0 palettes are opaque in practice.
                    content.save_state();
                    if let Some([r, g, b, _]) = layer.color {
                        let (r, g, b) = output.map_rgb((r, g, b));
                        content.set_fill_rgb(r, g, b);
                    }
                    let mut pen = PathPen::new(&mut content);
                    if pen.draw(font, layer.glyph_id) {
                        content.fill_nonzero();
                    }
                    content.restore_state();
                }
            }
            None => {}
        }
        let proc_ref = alloc.next();
        let bytes = maybe_deflate(&content.finish(), output.compress);
        let mut chunk = Chunk::new();
        let mut stream = chunk.stream(proc_ref, &bytes);
        if output.compress {
            stream.filter(Filter::FlateDecode);
        }
        stream.finish();
        chunks.push((proc_ref, chunk));
        procs.push((code, proc_ref, advance));
    }

    // `/ToUnicode`. Codes are one byte here, so the CMap uses one-byte codes
    // too.
    let mut cmap = UnicodeCmap::<u8>::new(TO_UNICODE_CMAP_NAME, TO_UNICODE_SYSTEM_INFO);
    for &(code, _, text) in glyphs {
        cmap.pair_with_multiple(code, text.chars());
    }
    let cmap_bytes = maybe_deflate(&cmap.finish(), output.compress);
    let mut chunk = Chunk::new();
    let mut to_unicode = chunk.cmap(slot.to_unicode, &cmap_bytes);
    to_unicode.name(TO_UNICODE_CMAP_NAME);
    to_unicode.system_info(TO_UNICODE_SYSTEM_INFO);
    if output.compress {
        to_unicode.filter(Filter::FlateDecode);
    }
    to_unicode.finish();
    chunks.push((slot.to_unicode, chunk));

    let last_code = procs.last().map(|(code, ..)| *code).unwrap_or(0);
    let mut chunk = Chunk::new();
    {
        let mut type3 = chunk.type3_font(slot.font);
        type3.name(Name(b"ColorGlyphs"));
        // Keep glyph space in font units. `/Widths` can then be written in
        // font units as well, so nothing has to convert to and from the
        // CIDFont side's thousandths.
        type3.matrix([1.0 / units_per_em, 0.0, 0.0, 1.0 / units_per_em, 0.0, 0.0]);
        type3.bbox(bbox.to_rect());
        {
            let mut char_procs = type3.char_procs();
            for (code, proc_ref, _) in &procs {
                char_procs.pair(Name(glyph_proc_name(*code).as_bytes()), *proc_ref);
            }
            char_procs.finish();
        }
        {
            let mut encoding = type3.encoding_custom();
            let mut differences = encoding.differences();
            for (code, ..) in &procs {
                differences.consecutive(*code, [Name(glyph_proc_name(*code).as_bytes())]);
            }
            differences.finish();
            encoding.finish();
        }
        type3.first_char(0);
        type3.last_char(last_code);
        // `/Widths` has to run consecutively from `/FirstChar`. Codes with no
        // procedure get width 0; nothing is ever drawn at them.
        let widths: Vec<f32> = (0..=last_code as usize)
            .map(|code| {
                procs
                    .iter()
                    .find(|(c, ..)| *c as usize == code)
                    .map(|(_, _, advance)| *advance)
                    .unwrap_or(0.0)
            })
            .collect();
        type3.widths(widths);
        {
            let mut resources = type3.resources();
            let mut xobjects = resources.x_objects();
            for image_ref in &images {
                xobjects.pair(Name(image_resource_name(*image_ref).as_bytes()), *image_ref);
            }
            xobjects.finish();
            resources.finish();
        }
        type3.to_unicode(slot.to_unicode);
        type3.finish();
    }
    chunks.push((slot.font, chunk));
}

/// Write a glyph's PNG as an image XObject and return its `Ref`, or `None` if
/// it will not decode, in which case the glyph draws nothing.
fn write_bitmap_image(
    chunks: &mut Vec<(Ref, Chunk)>,
    png: &[u8],
    alloc: &mut RefAllocator,
    grayscale: bool,
) -> Option<Ref> {
    let image = decode_png(png).ok()?;
    let root = alloc.next();
    // The alpha channel (`/SMask`) is a separate object; allocate one only
    // when the image actually has alpha.
    let alpha = has_alpha_plane(&image).then(|| alloc.next());
    chunks.extend(raster_plane_chunks(&image, root, alpha, grayscale));
    Some(root)
}

/// The glyph name used in a Type 3 `/CharProcs` and `/Encoding /Differences`.
fn glyph_proc_name(code: u8) -> String {
    format!("g{code}")
}

/// The rectangle enclosing the glyphs we wrote, in font units, for the Type 3
/// `/FontBBox`.
#[derive(Default)]
struct GlyphBBox {
    rect: Option<(f32, f32, f32, f32)>,
}

impl GlyphBBox {
    fn include(&mut self, x_min: f32, y_min: f32, x_max: f32, y_max: f32) {
        self.rect = Some(match self.rect {
            Some((a, b, c, d)) => (a.min(x_min), b.min(y_min), c.max(x_max), d.max(y_max)),
            None => (x_min, y_min, x_max, y_max),
        });
    }

    /// `COLR` layers are the font's own outlines, so the font-wide bounding
    /// box covers them.
    fn include_font_bbox(&mut self, font: &Font) {
        let b = font.bounding_box();
        self.include(
            b.x_min as f32,
            b.y_min as f32,
            b.x_max as f32,
            b.y_max as f32,
        );
    }

    fn to_rect(&self) -> PdfRect {
        // All zeroes when there are no glyphs at all, which PDF defines as
        // "imposes no bound".
        let (x_min, y_min, x_max, y_max) = self.rect.unwrap_or((0.0, 0.0, 0.0, 0.0));
        PdfRect::new(x_min, y_min, x_max, y_max)
    }
}

/// A pen that writes a glyph outline out as PDF path operators.
///
/// PDF has no quadratic bezier, so TrueType's `quad_to` is raised to a cubic.
struct PathPen<'a> {
    content: &'a mut Content,
    current: (f32, f32),
    drawn: bool,
}

impl<'a> PathPen<'a> {
    fn new(content: &'a mut Content) -> Self {
        Self {
            content,
            current: (0.0, 0.0),
            drawn: false,
        }
    }

    /// Feed `glyph_id`'s outline through. `true` if any path was written.
    fn draw(&mut self, font: &Font, glyph_id: u16) -> bool {
        font.draw_outline(glyph_id, self);
        self.drawn
    }
}

impl skrifa::outline::OutlinePen for PathPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.content.move_to(x, y);
        self.current = (x, y);
        self.drawn = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.content.line_to(x, y);
        self.current = (x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let (x0, y0) = self.current;
        let c1 = (x0 + 2.0 / 3.0 * (cx - x0), y0 + 2.0 / 3.0 * (cy - y0));
        let c2 = (x + 2.0 / 3.0 * (cx - x), y + 2.0 / 3.0 * (cy - y));
        self.content.cubic_to(c1.0, c1.1, c2.0, c2.1, x, y);
        self.current = (x, y);
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.content.cubic_to(c1x, c1y, c2x, c2y, x, y);
        self.current = (x, y);
    }

    fn close(&mut self) {
        self.content.close_path();
    }
}
