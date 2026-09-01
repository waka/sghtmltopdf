//! Loading font files.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::rc::Rc;

use harfrust::{FontRef, Shaper, ShaperData};
use self_cell::self_cell;
use skrifa::charmap::Charmap;
use skrifa::metrics::GlyphMetrics;
use skrifa::prelude::{LocationRef, Size};
use skrifa::raw::TableProvider;
use skrifa::MetadataProvider;

/// The set of borrowed views built from the bytes.
///
/// harfrust's `Shaper` and skrifa's `Charmap`/`GlyphMetrics` all borrow the font bytes.
/// Rebuilding them individually would mean looking up the tables again every time, so
/// they are built together, once, and kept.
struct FaceView<'a> {
    shaper: Shaper<'a>,
    charmap: Charmap<'a>,
    glyph_metrics: GlyphMetrics<'a>,
}

/// What `FaceView` borrows from.
///
/// `ShaperData` caches the tables used during shaping and owns them rather than borrowing
/// the bytes. `Shaper` borrows both (the bytes and the `ShaperData`), so both go into the
/// `self_cell` owner.
struct FaceOwner {
    bytes: Vec<u8>,
    index: u32,
    shaper_data: ShaperData,
}

self_cell!(
    /// Holds the font bytes together with the views built from them.
    ///
    /// The views borrow the bytes, so putting them in a struct naively would be
    /// self-referential. Building them walks the font's tables, so rebuilding on every
    /// call would make layout spend most of its time here.
    struct OwnedFace {
        owner: FaceOwner,
        #[covariant]
        dependent: FaceView,
    }
);

/// Cache key for a shaping plan. harfrust infers the direction, script and language from
/// the buffer contents, and the plan is determined by those three plus the face.
type PlanKey = (
    harfrust::Direction,
    harfrust::Script,
    Option<harfrust::Language>,
);

/// The rectangle enclosing every glyph in the font, in font units.
///
/// Taken verbatim from the `head` table. Used for `/FontBBox` in the PDF FontDescriptor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoundingBox {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

/// A loaded font.
///
/// Holds the file's raw bytes plus the shaping and metrics views built from them.
/// Metrics that never change are read once at construction as [`Metrics`], and glyph
/// lookups are memoised by [`Font::glyph_id`].
pub struct Font {
    face: OwnedFace,
    index: u32,
    metrics: Metrics,
    /// Memo of character -> glyph ID (`None` if absent from the cmap).
    ///
    /// A document contains few distinct characters, so a plain `HashMap` is plenty.
    /// The contents follow from the font, so the cache is transparent and does not change
    /// any externally observable behaviour.
    glyphs: RefCell<HashMap<char, Option<u16>>>,
    /// Memo of shaping plans ([`Font::shape_plan`]).
    plans: RefCell<HashMap<PlanKey, Rc<harfrust::ShapePlan>>>,
}

impl Clone for Font {
    /// Copy the bytes and rebuild the views (the views borrow the source's bytes, so they
    /// cannot be carried over directly).
    fn clone(&self) -> Self {
        Self::from_bytes(self.data().to_vec(), self.index)
            .expect("the source is a valid font, so this cannot fail")
    }
}

impl fmt::Debug for Font {
    /// The views do not implement `Debug`, so print only enough to identify the font.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Font")
            .field("family_name", &self.metrics.family_name)
            .field("index", &self.index)
            .field("bytes", &self.data().len())
            .finish()
    }
}

/// Metrics that only need reading from the font once.
///
/// Each value is in font units. The `head`/`hhea`/`OS/2`/`post` tables store them as
/// integers, so even the ones skrifa returns as `f32` are converted back to integers
/// here (no rounding occurs).
#[derive(Debug, Clone)]
struct Metrics {
    units_per_em: u16,
    ascender: i16,
    descender: i16,
    /// Line gap (`lineGap` from `hhea`, or `sTypoLineGap` if `OS/2` sets USE_TYPO_METRICS).
    /// Used to compute `line-height: normal`.
    line_gap: i16,
    capital_height: Option<i16>,
    x_height: Option<i16>,
    subscript_y_offset: Option<i16>,
    superscript_y_offset: Option<i16>,
    italic_angle: f32,
    is_italic: bool,
    underline: Option<(i16, i16)>,
    strikeout: Option<(i16, i16)>,
    is_monospaced: bool,
    weight: u16,
    bounding_box: BoundingBox,
    family_name: Option<String>,
    /// Whether the font has glyph outlines (`glyf`/CFF/CFF2).
    ///
    /// Some fonts have a `cmap` but no outlines, such as bitmap-only colour emoji fonts
    /// (`CBDT`/`CBLC`). They look like they "have" a character while being unable to draw
    /// anything, so we track this to keep them out of font selection.
    has_outlines: bool,
}

impl Metrics {
    fn read(font: &FontRef<'_>) -> Self {
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let attributes = font.attributes();

        // skrifa's `Metrics` does not carry the subscript/superscript Y offsets, so read
        // the `OS/2` table directly.
        let os2 = font.os2().ok();

        let italic_angle = m.italic_angle;
        let family_name = font
            .localized_strings(skrifa::string::StringId::TYPOGRAPHIC_FAMILY_NAME)
            .english_or_first()
            .or_else(|| {
                font.localized_strings(skrifa::string::StringId::FAMILY_NAME)
                    .english_or_first()
            })
            .map(|name| name.chars().collect());

        Self {
            units_per_em: m.units_per_em,
            ascender: m.ascent as i16,
            descender: m.descent as i16,
            line_gap: m.leading as i16,
            capital_height: m.cap_height.map(|v| v as i16),
            x_height: m.x_height.map(|v| v as i16),
            subscript_y_offset: os2.as_ref().map(|t| t.y_subscript_y_offset()),
            superscript_y_offset: os2.as_ref().map(|t| t.y_superscript_y_offset()),
            italic_angle,
            // Even when `OS/2` does not set Italic, treat the face as slanted if `post`'s
            // italic angle is non-zero (this decides whether faux italic is needed for an
            // italic request, so what matters is whether it really slants).
            is_italic: matches!(attributes.style, skrifa::attribute::Style::Italic)
                || italic_angle != 0.0,
            underline: m.underline.map(|d| (d.offset as i16, d.thickness as i16)),
            strikeout: m.strikeout.map(|d| (d.offset as i16, d.thickness as i16)),
            is_monospaced: m.is_monospace,
            weight: attributes.weight.value() as u16,
            has_outlines: font.outline_glyphs().format().is_some(),
            bounding_box: m
                .bounds
                .map(|b| BoundingBox {
                    x_min: b.x_min as i16,
                    y_min: b.y_min as i16,
                    x_max: b.x_max as i16,
                    y_max: b.y_max as i16,
                })
                .unwrap_or_default(),
            family_name,
        }
    }
}

/// Warn that a font without outlines was not used.
///
/// `source` is something the user named, such as a `--font` path or an `@font-face` family
/// name. Fonts dropped by automatic discovery are not covered here (those end up in the
/// "no font can render this" warning instead).
pub fn warn_font_without_outlines(source: &str) {
    eprintln!(
        "warning: {source} has no outlines (it is a bitmap-only colour emoji\n  \
         font), so it will not be used. Colour fonts are not supported.\n  \
         For emoji, specify a monochrome outline version such as Noto Emoji"
    );
}

#[derive(Debug)]
pub struct FontLoadError(String);

impl fmt::Display for FontLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to load the font: {}", self.0)
    }
}

impl std::error::Error for FontLoadError {}

impl Font {
    /// Load a font from a local file path.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FontLoadError> {
        Self::load_indexed(path, 0)
    }

    /// Load a font from a local file path. For a file containing several faces, such as a
    /// TrueType Collection (`.ttc`), `index` selects the face.
    pub fn load_indexed(path: impl AsRef<Path>, index: u32) -> Result<Self, FontLoadError> {
        let path = path.as_ref();
        let data =
            std::fs::read(path).map_err(|e| FontLoadError(format!("{}: {e}", path.display())))?;
        Self::from_bytes(data, index)
    }

    /// Build a font from bytes already read (for a file containing several faces, such as
    /// a TrueType Collection, `index` selects the face).
    pub fn from_bytes(data: Vec<u8>, index: u32) -> Result<Self, FontLoadError> {
        // `ShaperData` and the metrics do not borrow the bytes, so parse once and build
        // them here. Only the things that borrow the bytes, such as `Shaper`, are built
        // in the `self_cell` closure below.
        let (shaper_data, metrics) = {
            let font = parse_font(&data, index)?;
            (ShaperData::new(&font), Metrics::read(&font))
        };

        let owner = FaceOwner {
            bytes: data,
            index,
            shaper_data,
        };
        let face = OwnedFace::try_new(owner, |owner| {
            let font = parse_font(&owner.bytes, owner.index)?;
            Ok(FaceView {
                shaper: owner.shaper_data.shaper(&font).build(),
                charmap: font.charmap(),
                glyph_metrics: font.glyph_metrics(Size::unscaled(), LocationRef::default()),
            })
        })?;

        Ok(Self {
            face,
            index,
            metrics,
            glyphs: RefCell::new(HashMap::new()),
            plans: RefCell::new(HashMap::new()),
        })
    }

    fn view(&self) -> &FaceView<'_> {
        self.face.borrow_dependent()
    }

    pub(crate) fn shaper(&self) -> &Shaper<'_> {
        &self.view().shaper
    }

    /// The shaping plan for `key` (direction, script and language).
    ///
    /// Shaping asks for a plan on every call, but building one costs more than the shaping
    /// itself. Layout shapes per word (per run of consistent style and font), so without
    /// reusing plans per face, building them would dominate the running time. A plan is
    /// determined by the face and the key alone, so the cache is transparent and the
    /// results do not change.
    pub(crate) fn shape_plan(&self, key: &PlanKey) -> Rc<harfrust::ShapePlan> {
        if let Some(cached) = self.plans.borrow().get(key) {
            return Rc::clone(cached);
        }
        let plan = Rc::new(harfrust::ShapePlan::new(
            self.shaper(),
            key.0,
            Some(key.1),
            key.2.as_ref(),
            &[],
        ));
        self.plans
            .borrow_mut()
            .insert(key.clone(), Rc::clone(&plan));
        plan
    }

    /// The font file's raw bytes (needed to embed the font in the PDF, among other things).
    pub fn data(&self) -> &[u8] {
        &self.face.borrow_owner().bytes
    }

    /// Face index within a file containing several faces, such as a TrueType Collection (`.ttc`).
    pub fn face_index(&self) -> u32 {
        self.index
    }

    pub fn units_per_em(&self) -> u16 {
        self.metrics.units_per_em
    }

    pub fn ascender(&self) -> i16 {
        self.metrics.ascender
    }

    pub fn descender(&self) -> i16 {
        self.metrics.descender
    }

    /// The used value of `line-height: normal`, in px.
    ///
    /// CSS `normal` is "the line spacing the font recommends", which is ascender +
    /// descender + line gap. Approximating it with a fixed ratio (1.2em, say) makes glyphs
    /// spill out of the line box in fonts where ascender + descender exceeds it (around
    /// 1.4em is not unusual for CJK fonts), overlapping the neighbouring lines and any
    /// borders.
    pub fn normal_line_height(&self, font_size: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        if units_per_em <= 0.0 {
            return 0.0;
        }
        let content = self.ascender() as f32 - self.descender() as f32;
        (content + self.metrics.line_gap as f32) / units_per_em * font_size
    }

    pub fn capital_height(&self) -> Option<i16> {
        self.metrics.capital_height
    }

    /// Distance from the top of the line box to the baseline, derived from the ascender
    /// and descender (the font's em box is centred vertically within the line box).
    /// Used both by `vertical-align: baseline` on table cells (which needs the baseline of
    /// the cell content's first line) and by text drawing (`render_line`).
    pub fn baseline_offset(&self, font_size: f32, line_height: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        let ascent = self.ascender() as f32 / units_per_em * font_size;
        let descent = -(self.descender() as f32) / units_per_em * font_size;
        let half_leading = (line_height - (ascent + descent)) / 2.0;
        ascent + half_leading
    }

    /// x-height, in px. Approximated as half the ascender when the `OS/2` table lacks it
    /// (the reference for `vertical-align: middle`).
    pub fn x_height(&self, font_size: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        match self.metrics.x_height {
            Some(x) => x as f32 / units_per_em * font_size,
            None => self.ascender() as f32 / units_per_em * font_size * 0.5,
        }
    }

    /// How far `vertical-align: sub` moves down, in px (a positive value). Approximated as
    /// `0.2em` when the font's `OS/2` lacks a subscript Y offset.
    pub fn subscript_offset(&self, font_size: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        match self.metrics.subscript_y_offset {
            Some(y_offset) => y_offset as f32 / units_per_em * font_size,
            None => font_size * 0.2,
        }
    }

    /// How far `vertical-align: super` moves up, in px (positive). `0.33em` when absent.
    pub fn superscript_offset(&self, font_size: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        match self.metrics.superscript_y_offset {
            Some(y_offset) => y_offset as f32 / units_per_em * font_size,
            None => font_size * 0.33,
        }
    }

    pub fn italic_angle(&self) -> f32 {
        self.metrics.italic_angle
    }

    pub fn is_italic(&self) -> bool {
        self.metrics.is_italic
    }

    /// Centre position of the underline (a signed offset from the baseline, in font units,
    /// positive upwards) and its thickness. `None` if the font has no `post` table.
    pub fn underline_metrics(&self) -> Option<(i16, i16)> {
        self.metrics.underline
    }

    /// Centre position of the strikethrough (a signed offset from the baseline, in font
    /// units, positive upwards) and its thickness. `None` if the font has no `OS/2` table.
    pub fn strikeout_metrics(&self) -> Option<(i16, i16)> {
        self.metrics.strikeout
    }

    pub fn is_monospaced(&self) -> bool {
        self.metrics.is_monospaced
    }

    /// Weight from the OS/2 table (400 = regular, 700 = bold).
    pub fn weight(&self) -> u16 {
        self.metrics.weight
    }

    pub fn bounding_box(&self) -> BoundingBox {
        self.metrics.bounding_box
    }

    /// Horizontal advance of `glyph_id`, in font units.
    pub fn glyph_hor_advance(&self, glyph_id: u16) -> Option<u16> {
        self.view()
            .glyph_metrics
            .advance_width(skrifa::GlyphId::from(glyph_id))
            .map(|advance| advance as u16)
    }

    /// Whether this font has a glyph for `c`, and the glyph ID it maps to (`None` if absent
    /// from the cmap). Used to decide font-family fallback, that is, which font can draw
    /// this character.
    pub fn glyph_id(&self, c: char) -> Option<u16> {
        if let Some(cached) = self.glyphs.borrow().get(&c) {
            return *cached;
        }
        let found = self.view().charmap.map(c).map(|id| id.to_u32() as u16);
        self.glyphs.borrow_mut().insert(c, found);
        found
    }

    /// Whether `c` can actually be drawn.
    ///
    /// Being in the `cmap` is not enough: the font must also have outlines
    /// ([`Self::has_outlines`]). Colour emoji fonts do have a `cmap`, so without this check
    /// we would decide we can draw and silently emit invisible text.
    pub fn has_glyph(&self, c: char) -> bool {
        self.has_outlines() && self.glyph_id(c).is_some()
    }

    /// Whether the font has glyph outlines (`glyf`/CFF/CFF2). A font where this is `false`,
    /// such as a bitmap-only colour emoji font, can draw nothing.
    pub fn has_outlines(&self) -> bool {
        self.metrics.has_outlines
    }

    /// Font name (the `name` table's Typographic Family, or Family if absent).
    /// Returns the English name if there is one, otherwise the first name found.
    pub fn family_name(&self) -> Option<String> {
        self.metrics.family_name.clone()
    }
}

/// Read face number `index` from the bytes. For anything other than a TrueType Collection,
/// an `index` of 0 is treated as the single face.
fn parse_font(data: &[u8], index: u32) -> Result<FontRef<'_>, FontLoadError> {
    FontRef::from_index(data, index).map_err(|e| FontLoadError(format!("invalid font data: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    #[test]
    fn loads_a_valid_font_file() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        assert!(font.units_per_em() > 0);
    }

    #[test]
    fn load_fails_for_missing_file() {
        let result = Font::load("/nonexistent/path/does-not-exist.ttf");
        assert!(result.is_err());
    }

    #[test]
    fn reports_family_name() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        assert_eq!(font.family_name().as_deref(), Some("DejaVu Sans"));
    }

    #[test]
    fn has_glyph_distinguishes_covered_and_uncovered_characters() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        assert!(font.has_glyph('A'));
        // DejaVu Sans contains no CJK characters.
        assert!(!font.has_glyph('日'));
    }

    #[test]
    fn from_bytes_rejects_invalid_font_data() {
        let result = Font::from_bytes(b"not a font file".to_vec(), 0);
        assert!(result.is_err());
    }

    #[test]
    fn reads_the_metrics_the_pdf_font_descriptor_needs() {
        // Check that every value written to the FontDescriptor is present, using DejaVu Sans's known values.
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");

        assert_eq!(font.units_per_em(), 2048);
        assert_eq!(font.ascender(), 1901);
        assert_eq!(font.descender(), -483);
        assert_eq!(font.weight(), 400);
        assert!(!font.is_italic());
        assert!(!font.is_monospaced());
        assert_eq!(font.italic_angle(), 0.0);

        let bbox = font.bounding_box();
        assert_eq!(bbox.x_min, -2090);
        assert_eq!(bbox.y_min, -948);
        assert_eq!(bbox.x_max, 3673);
        assert_eq!(bbox.y_max, 2524);

        assert_eq!(font.underline_metrics(), Some((-40, 90)));
        assert_eq!(font.strikeout_metrics(), Some((530, 102)));
    }

    #[test]
    fn maps_characters_to_glyphs_with_advances() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");

        let gid = font.glyph_id('A').expect("DejaVu Sans has an A");
        let advance = font
            .glyph_hor_advance(gid)
            .expect("if the glyph exists, its advance can be read too");
        assert!(advance > 0);
        // A space is narrower than an A.
        let space = font.glyph_id(' ').expect("DejaVu Sans has a space");
        assert!(font.glyph_hor_advance(space).unwrap() < advance);
    }

    #[test]
    fn selects_the_requested_face_from_a_collection() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fonts/NotoSansCJK-Regular.ttc"
        );
        let font = Font::load_indexed(path, 0).expect("should load face 0 of the collection");
        assert_eq!(font.face_index(), 0);
        assert!(font.has_glyph('日'));
    }

    #[test]
    fn baseline_offset_is_between_zero_and_the_line_height() {
        // The baseline should sit roughly the ascender below the top of the line
        // (with the line height matching the font's own metrics, the half-leading is near zero).
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        let units_per_em = font.units_per_em() as f32;
        let ascent = font.ascender() as f32 / units_per_em * 16.0;
        let descent = -(font.descender() as f32) / units_per_em * 16.0;
        let line_height = ascent + descent;

        let offset = font.baseline_offset(16.0, line_height);
        assert!(
            (offset - ascent).abs() < 0.01,
            "with no extra leading, the baseline offset should equal the ascent: {offset} vs {ascent}"
        );
        assert!(offset > 0.0 && offset < line_height);
    }

    #[test]
    fn normal_line_height_is_the_fonts_own_content_area_plus_line_gap() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        let units_per_em = font.units_per_em() as f32;
        let expected = (font.ascender() as f32 - font.descender() as f32) / units_per_em * 16.0;

        // DejaVu Sans has a `lineGap` of 0, so ascender + descender is exactly the
        // `normal` line spacing.
        let normal = font.normal_line_height(16.0);
        assert!(
            (normal - expected).abs() < 0.01,
            "normal line height should be ascent + descent (+ line gap): {normal} vs {expected}"
        );
    }

    #[test]
    fn normal_line_height_always_covers_the_glyphs_content_area() {
        // If a font made `normal` smaller than ascender + descender, the half-leading
        // would go negative and glyphs would spill out of the line box.
        let paths = [
            TEST_FONT_PATH,
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fonts/NotoSansCJK-Regular.ttc"
            ),
        ];
        for path in paths {
            let font = Font::load(path).expect("should load test font");
            let units_per_em = font.units_per_em() as f32;
            let content = (font.ascender() as f32 - font.descender() as f32) / units_per_em * 16.0;
            assert!(
                font.normal_line_height(16.0) >= content - 0.01,
                "{path}: normal line height {} must not be shorter than the content area {content}",
                font.normal_line_height(16.0)
            );
        }
    }
}

#[cfg(test)]
mod outline_tests {
    use super::*;

    const TEST_FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
    /// A bitmap-only font (CBDT/CBLC) with no glyph outlines at all.
    const COLOR_EMOJI_FONT_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/NotoColorEmoji.ttf"
    );

    #[test]
    fn a_normal_font_has_outlines() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        assert!(font.has_outlines());
        assert!(font.has_glyph('A'));
    }

    /// A colour emoji font has a `cmap`, so it looks like it has the character, but with no
    /// outlines it can draw nothing. Deciding it "can draw" would silently emit invisible
    /// text and bloat the PDF for nothing.
    #[test]
    fn a_colour_font_covers_nothing() {
        let font = Font::load(COLOR_EMOJI_FONT_PATH).expect("should load bundled colour font");

        assert!(!font.has_outlines());
        assert!(
            font.glyph_id('\u{1F389}').is_some(),
            "it has a cmap, so a glyph ID can be looked up"
        );
        assert!(
            !font.has_glyph('\u{1F389}'),
            "with no outlines it cannot be said to be drawable"
        );
    }
}
