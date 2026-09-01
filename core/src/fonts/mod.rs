//! Font loading and shaping (harfrust/skrifa).

mod collection;
mod face;
mod font;
mod shape;
mod system;

pub use collection::FontCollection;
pub use face::{load_font_faces, LoadedFontFace};
pub use font::{warn_font_without_outlines, BoundingBox, Font, FontLoadError};
pub use shape::{measure_text, shape_text, ShapedGlyph, ShapedText};
pub use system::{
    ensure_cjk_fallback_font, load_fonts_for_uncovered_chars, load_missing_system_fonts,
    warn_uncovered_chars, SystemFonts,
};
