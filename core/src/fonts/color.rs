//! Reading colour glyphs: embedded bitmaps and `COLR`/`CPAL` version 0.
//!
//! Two formats are handled:
//!
//! * Embedded bitmaps (`CBDT`/`CBLC`, `sbix`, `EBDT`/`EBLC`). A font like Noto
//!   Color Emoji has nothing else — it carries no outlines at all.
//! * `COLR`/`CPAL` version 0: solid-colour layers, where a glyph is drawn by
//!   filling a series of ordinary outline glyphs each in one colour.
//!
//! COLRv1 (gradients, transforms, compositing) is out of scope, and so is
//! OpenType SVG. A glyph we cannot read here comes back as `None` and the
//! caller treats it as an ordinary outline glyph, which for a COLRv1 font
//! means its base outlines are drawn in monochrome.

use skrifa::bitmap::{BitmapData, BitmapGlyph, BitmapStrikes, Origin};
use skrifa::color::{
    Brush, ColorGlyphCollection, ColorGlyphFormat, ColorPainter, ColorPalettes, CompositeMode,
    Transform,
};
use skrifa::prelude::{LocationRef, Size};
use skrifa::raw::types::BoundingBox;
use skrifa::{FontRef, GlyphId};

/// A colour glyph in a form the PDF writer can emit directly.
#[derive(Debug, Clone)]
pub enum ColorGlyph {
    /// An embedded bitmap, as PNG.
    Bitmap(ColorBitmap),
    /// `COLR` v0 solid-colour layers, painted first to last.
    LayersV0(Vec<ColorLayer>),
}

/// One embedded bitmap and the rectangle it occupies, in font units with the
/// origin on the baseline. The PDF writer draws an image XObject into exactly
/// this rectangle.
#[derive(Debug, Clone)]
pub struct ColorBitmap {
    /// The PNG bytes, untouched. Decoding happens in the PDF writer.
    pub png: Vec<u8>,
    pub x_min: f32,
    pub y_min: f32,
    pub x_max: f32,
    pub y_max: f32,
}

/// One `COLR` v0 layer: fill the outline of `glyph_id` with `color`.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorLayer {
    pub glyph_id: u16,
    /// The layer colour as RGBA in 0.0..=1.0. `None` is CPAL palette index
    /// `0xFFFF`, meaning "the current text colour": the caller fills with
    /// whatever colour the surrounding text uses.
    pub color: Option<[f32; 4]>,
}

/// Whether this font has any colour glyph machinery at all. Says nothing about
/// whether a given glyph can actually be read.
///
/// Read once when the font is loaded, because
/// [`Font::can_render`](super::Font::can_render) consults it for every font
/// selection decision.
pub(super) fn has_color_glyphs(font: &FontRef<'_>) -> bool {
    use skrifa::raw::TableProvider;

    BitmapStrikes::new(font).format().is_some() || font.colr().is_ok()
}

/// Read the colour representation of `glyph_id`, or `None` if it has none.
///
/// `COLR` v0 wins over a bitmap: it stays vector all the way into the PDF, so
/// it scales cleanly and takes less space.
pub(super) fn read(font: &FontRef<'_>, glyph_id: u16) -> Option<ColorGlyph> {
    let gid = GlyphId::from(glyph_id);
    if let Some(layers) = read_colr_v0(font, gid) {
        return Some(ColorGlyph::LayersV0(layers));
    }
    read_bitmap(font, gid).map(ColorGlyph::Bitmap)
}

fn read_colr_v0(font: &FontRef<'_>, gid: GlyphId) -> Option<Vec<ColorLayer>> {
    let glyph = ColorGlyphCollection::new(font).get_with_format(gid, ColorGlyphFormat::ColrV0)?;
    let palettes = ColorPalettes::new(font);
    let palette = palettes.get(0);
    let mut collector = LayerCollector {
        palette: palette.as_ref().map(|p| p.colors()).unwrap_or(&[]),
        layers: Vec::new(),
    };
    glyph.paint(LocationRef::default(), &mut collector).ok()?;
    (!collector.layers.is_empty()).then_some(collector.layers)
}

/// Folds a `COLR` v0 traversal down into a flat list of layers.
///
/// v0 has no transforms, no clips and no compositing: the traversal only ever
/// calls `fill_glyph` with a solid brush, so every other callback does nothing.
struct LayerCollector<'a> {
    palette: &'a [skrifa::color::Color],
    layers: Vec<ColorLayer>,
}

/// The CPAL index meaning "use the text colour rather than a palette entry".
const PALETTE_INDEX_TEXT_COLOR: u16 = 0xFFFF;

impl ColorPainter for LayerCollector<'_> {
    fn push_transform(&mut self, _transform: Transform) {}
    fn pop_transform(&mut self) {}
    fn push_clip_glyph(&mut self, _glyph_id: GlyphId) {}
    fn push_clip_box(&mut self, _clip_box: BoundingBox<f32>) {}
    fn pop_clip(&mut self) {}
    fn fill(&mut self, _brush: Brush<'_>) {}
    fn push_layer(&mut self, _composite_mode: CompositeMode) {}

    fn fill_glyph(
        &mut self,
        glyph_id: GlyphId,
        _brush_transform: Option<Transform>,
        brush: Brush<'_>,
    ) {
        // A v0 brush is always solid. A gradient would mean v1, which we do
        // not handle.
        let Brush::Solid {
            palette_index,
            alpha,
        } = brush
        else {
            return;
        };
        let color = if palette_index == PALETTE_INDEX_TEXT_COLOR {
            None
        } else {
            // An index that is not in the palette means a broken font. Fall
            // back to the text colour: dropping the layer would punch a hole
            // in the artwork.
            self.palette.get(palette_index as usize).map(|record| {
                [
                    record.red as f32 / 255.0,
                    record.green as f32 / 255.0,
                    record.blue as f32 / 255.0,
                    record.alpha as f32 / 255.0 * alpha,
                ]
            })
        };
        self.layers.push(ColorLayer {
            glyph_id: glyph_id.to_u32() as u16,
            color,
        });
    }
}

fn read_bitmap(font: &FontRef<'_>, gid: GlyphId) -> Option<ColorBitmap> {
    // `Size::unscaled()` asks for the largest strike. A PDF is resolution
    // independent, so always take the most detailed bitmap on offer.
    let glyph = BitmapStrikes::new(font).glyph_for_size(Size::unscaled(), gid)?;
    // Only PNG. That is how 32-bit `CBDT` glyphs and `sbix` glyphs are stored,
    // which covers every colour emoji font in practice. Raw `Bgra` pixels and
    // the monochrome `Mask` formats are not handled.
    let BitmapData::Png(png) = glyph.data else {
        return None;
    };
    let (x_min, y_min, x_max, y_max) = placement(&glyph, font)?;
    Some(ColorBitmap {
        png: png.to_vec(),
        x_min,
        y_min,
        x_max,
        y_max,
    })
}

/// The rectangle the bitmap occupies in glyph space (font units, origin on the
/// baseline).
///
/// skrifa reports outer bearings (`bearing_*`, font units) separately from
/// inner bearings (`inner_bearing_*`, pixels); the latter have to be scaled by
/// `ppem` first. `placement_origin` says whether the inner Y bearing measures
/// to the top of the rectangle (`CBDT`) or to its bottom (`sbix`).
fn placement(glyph: &BitmapGlyph<'_>, font: &FontRef<'_>) -> Option<(f32, f32, f32, f32)> {
    use skrifa::raw::TableProvider;

    let units_per_em = font.head().ok()?.units_per_em() as f32;
    if units_per_em <= 0.0 || glyph.ppem_x <= 0.0 || glyph.ppem_y <= 0.0 {
        return None;
    }
    let (scale_x, scale_y) = (units_per_em / glyph.ppem_x, units_per_em / glyph.ppem_y);

    let x_min = glyph.bearing_x + glyph.inner_bearing_x * scale_x;
    let x_max = x_min + glyph.width as f32 * scale_x;
    let height = glyph.height as f32 * scale_y;
    let (y_min, y_max) = match glyph.placement_origin {
        Origin::TopLeft => {
            let top = glyph.bearing_y + glyph.inner_bearing_y * scale_y;
            (top - height, top)
        }
        Origin::BottomLeft => {
            let bottom = glyph.bearing_y + glyph.inner_bearing_y * scale_y;
            (bottom, bottom + height)
        }
    };
    Some((x_min, y_min, x_max, y_max))
}
