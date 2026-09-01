//! Text shaping and glyph advance lookup via harfrust.

use super::font::Font;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShapedGlyph {
    pub glyph_id: u16,
    /// UTF-8 byte offset of this glyph in the original text
    /// (used to map a glyph back to its source character, e.g. when building a PDF `/ToUnicode` CMap).
    pub cluster: u32,
    /// Advance width and offset relative to the drawing position, in px.
    pub x_advance: f32,
    pub x_offset: f32,
    pub y_offset: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ShapedText {
    pub glyphs: Vec<ShapedGlyph>,
    /// Sum of the advance widths of all glyphs, in px.
    pub width: f32,
}

/// Shape `text` with `font` at `font_size` (px).
pub fn shape_text(font: &Font, text: &str, font_size: f32) -> ShapedText {
    let units_per_em = font.units_per_em() as f32;
    let scale = if units_per_em > 0.0 {
        font_size / units_per_em
    } else {
        0.0
    };

    let mut buffer = harfrust::UnicodeBuffer::new();
    buffer.push_str(text);
    // Pin down direction, script and language here, and let the plan be keyed on them
    buffer.guess_segment_properties();
    let plan = font.shape_plan(&(buffer.direction(), buffer.script(), buffer.language()));
    let output = font
        .shaper()
        .shape(buffer, harfrust::ShapeOptions::new().plan(Some(&plan)));

    let mut glyphs = Vec::with_capacity(output.len());
    let mut width = 0.0;
    for (info, pos) in output.glyph_infos().iter().zip(output.glyph_positions()) {
        let x_advance = pos.x_advance as f32 * scale;
        glyphs.push(ShapedGlyph {
            glyph_id: info.glyph_id as u16,
            cluster: info.cluster,
            x_advance,
            x_offset: pos.x_offset as f32 * scale,
            y_offset: pos.y_offset as f32 * scale,
        });
        width += x_advance;
    }

    ShapedText { glyphs, width }
}

/// Simple API returning only the drawn width of the text (px), which is all line breaking needs.
pub fn measure_text(font: &Font, text: &str, font_size: f32) -> f32 {
    shape_text(font, text, font_size).width
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    fn test_font() -> Font {
        Font::load(TEST_FONT_PATH).expect("should load bundled test font")
    }

    #[test]
    fn shapes_ascii_text_into_matching_glyph_count() {
        let font = test_font();
        let shaped = shape_text(&font, "Hi", 16.0);

        assert_eq!(shaped.glyphs.len(), 2);
        assert!(shaped.width > 0.0);
    }

    #[test]
    fn empty_text_produces_no_glyphs() {
        let font = test_font();
        let shaped = shape_text(&font, "", 16.0);

        assert!(shaped.glyphs.is_empty());
        assert_eq!(shaped.width, 0.0);
    }

    #[test]
    fn width_scales_linearly_with_font_size() {
        let font = test_font();
        let small = measure_text(&font, "Hello, world!", 10.0);
        let large = measure_text(&font, "Hello, world!", 20.0);

        assert!(
            (large - small * 2.0).abs() < 0.01,
            "width should scale linearly with font-size: small={small}, large={large}"
        );
    }

    #[test]
    fn longer_text_measures_wider() {
        let font = test_font();
        let short = measure_text(&font, "I", 16.0);
        let long = measure_text(&font, "Illustration", 16.0);

        assert!(long > short);
    }
}
