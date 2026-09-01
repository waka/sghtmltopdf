//! A collection of fonts, and fallback selection based on `font-family`, weight, style
//! and glyph coverage.
//!
//! Discovering system fonts (scanning the OS font directories) is [`super::system`]'s job.

use cssparser::UnicodeRange;

use crate::style::{FontStyle, FontWeight};

use super::font::Font;

/// Threshold for rounding a numeric `font-weight` to the two values Bold/Normal (600, the
/// same as `parse_font_weight` in `style::properties`). Rounding a font's own OS/2 weight
/// to those two values makes it comparable with `ComputedStyle::font_weight`.
const BOLD_WEIGHT_THRESHOLD: u16 = 600;

pub struct FontCollection {
    fonts: Vec<Font>,
    /// The family name declared in CSS for a font loaded from `@font-face` or from the
    /// system fonts. Entries that are `None` (fonts named explicitly with `--font` and the
    /// like) are matched on the font's own `name` table (`Font::family_name`).
    declared_families: Vec<Option<String>>,
    /// Overrides from an `@font-face` weight/style descriptor. Entries that are `None` are
    /// decided from the font's own `OS/2`/`post` metrics (`Font::weight`/`Font::is_italic`).
    declared_weights: Vec<Option<FontWeight>>,
    declared_styles: Vec<Option<FontStyle>>,
    /// The `unicode-range` descriptor from `@font-face`. Entries with an empty `Vec`
    /// (a `--font` or system font, or an `@font-face` with no `unicode-range`) are treated
    /// as implicitly covering the whole range (U+0-10FFFF)
    declared_unicode_ranges: Vec<Vec<UnicodeRange>>,
    /// Whether this entry was added as a fallback found automatically from character
    /// coverage. Fonts the user named (`--font`/`@font-face`) and fonts resolved from a
    /// family name are `false`. [`Self::can_render_with_matching_face`] uses it to tell
    /// "a face that happened to get pulled in" from "a face the user chose".
    auto_fallbacks: Vec<bool>,
}

impl FontCollection {
    pub fn new(fonts: Vec<Font>) -> Self {
        let len = fonts.len();
        Self {
            fonts,
            declared_families: vec![None; len],
            declared_weights: vec![None; len],
            declared_styles: vec![None; len],
            declared_unicode_ranges: vec![Vec::new(); len],
            auto_fallbacks: vec![false; len],
        }
    }

    /// Add a font loaded from `@font-face { font-family: ...; src: url(...); }` or from the
    /// system fonts. `family` is used for matching in preference to the font's own `name`
    /// table (a font file's internal name can differ from the name declared in CSS).
    /// `weight`/`style` take the `@font-face` descriptor values (what the CSS declares).
    /// Where CSS declares nothing, as with system fonts, pass `None` and let the font's own
    /// metrics decide. `unicode_range` takes the `@font-face` `unicode-range` descriptor,
    /// where an empty `Vec` means "unspecified (covers everything)".
    pub fn push_font_face(
        &mut self,
        family: String,
        weight: Option<FontWeight>,
        style: Option<FontStyle>,
        unicode_range: Vec<UnicodeRange>,
        font: Font,
    ) {
        self.fonts.push(font);
        self.declared_families.push(Some(family));
        self.declared_weights.push(weight);
        self.declared_styles.push(style);
        self.declared_unicode_ranges.push(unicode_range);
        self.auto_fallbacks.push(false);
    }

    /// Add a fallback font found automatically from character coverage
    /// (for `super::system::load_fonts_for_uncovered_chars`).
    pub fn push_fallback_font_face(&mut self, family: String, font: Font) {
        self.push_font_face(family, None, None, Vec::new(), font);
        if let Some(flag) = self.auto_fallbacks.last_mut() {
            *flag = true;
        }
    }

    pub fn fonts(&self) -> &[Font] {
        &self.fonts
    }

    pub fn get(&self, index: usize) -> Option<&Font> {
        self.fonts.get(index)
    }

    /// The family name font `index` declares in CSS (the `font-family` of an `@font-face`,
    /// or a system font resolved from a family name).
    ///
    /// `None` means "CSS declares nothing" (a font named explicitly with `--font`, say),
    /// in which case matching uses the font's own `name` table ([`Font::family_name`]).
    /// This exposes the same distinction [`Self::matches_family`] makes, which is needed
    /// when building the font database for SVG (`pdf::svg`) so fonts can also be looked up
    /// by the name they go by in CSS.
    pub fn declared_family(&self, index: usize) -> Option<&str> {
        self.declared_families.get(index)?.as_deref()
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// Return the index of a font whose name matches one in `families` (the CSS
    /// `font-family` list, in priority order) and that has a glyph for `c`.
    ///
    /// Selection order: (1) a font whose family matches, that can draw `c`, and that also
    /// really satisfies `weight`/`style`; (2) a font whose family matches and that can draw
    /// `c` (weight/style ignored); (3) the first font that can draw `c` regardless of
    /// family; (4) failing all that, the first font in the collection (which renders as
    /// tofu). `None` only when the collection is empty.
    ///
    /// Whether the chosen font really satisfies the requested `weight`/`style` can be
    /// checked separately with [`Self::is_bold`]/[`Self::is_italic`] (callers use that to
    /// decide whether faux bold or faux italic is needed).
    pub fn select_for_char(
        &self,
        families: &[String],
        weight: FontWeight,
        style: FontStyle,
        c: char,
    ) -> Option<usize> {
        if self.fonts.is_empty() {
            return None;
        }

        for family in families {
            if let Some(index) =
                self.best_match(weight, style, c, |i, f| self.matches_family(i, f, family))
            {
                return Some(index);
            }
        }

        // Even when no family matches, prefer a font whose weight/style match, ignoring the
        // family (a setting such as the default "sans-serif" commonly matches no font's
        // internal family name, and we do not want to give up the chance to pick a real
        // bold or italic face here either).
        if let Some(index) = self.best_match(weight, style, c, |_, _| true) {
            return Some(index);
        }

        Some(0)
    }

    /// Among the fonts satisfying `eligible`, whose `unicode-range` (if declared) contains
    /// `c`, and that have a glyph for `c`, prefer one that also really satisfies
    /// `weight`/`style` (decided by `Self::is_bold`/`Self::is_italic`).
    /// If none matches, return the first font meeting the conditions.
    ///
    /// `unicode-range` acts as a hard filter: if a declared range does not contain `c`, the
    /// font is excluded from the candidates even when it really does have a glyph for `c`.
    /// The first font meeting the conditions in scan order (= registration order = CSS
    /// source order) is taken, so overlapping ranges within the same family/weight/style
    /// naturally resolve as "declaration order wins".
    fn best_match(
        &self,
        weight: FontWeight,
        style: FontStyle,
        c: char,
        mut eligible: impl FnMut(usize, &Font) -> bool,
    ) -> Option<usize> {
        let mut first_match = None;
        for (i, f) in self.fonts.iter().enumerate() {
            if !eligible(i, f) || !self.in_unicode_range(i, c) || !f.has_glyph(c) {
                continue;
            }
            first_match.get_or_insert(i);
            if self.is_bold(i) == (weight == FontWeight::Bold)
                && self.is_italic(i) == (style == FontStyle::Italic)
            {
                return Some(i);
            }
        }
        first_match
    }

    /// Whether font `index`'s declared `unicode-range` (if any) contains `c`.
    /// Always `true` when no `unicode-range` was declared (an empty `Vec`), covering everything.
    fn in_unicode_range(&self, index: usize, c: char) -> bool {
        match self.declared_unicode_ranges.get(index) {
            Some(ranges) if !ranges.is_empty() => {
                let code_point = c as u32;
                ranges
                    .iter()
                    .any(|range| range.start <= code_point && code_point <= range.end)
            }
            _ => true,
        }
    }

    /// Whether a font matching `family` (from `--font`, `@font-face` or the system fonts
    /// alike) is already in the collection, ignoring weight and style.
    pub fn has_family(&self, family: &str) -> bool {
        self.fonts
            .iter()
            .enumerate()
            .any(|(i, f)| self.matches_family(i, f, family))
    }

    /// Whether a font matching `family` that also really satisfies `weight`/`style` is
    /// already in the collection. System font discovery uses this to look only for the
    /// weights and styles the existing fonts cannot cover (a Bold request against a family
    /// that only has Regular, say).
    pub fn has_matching_face(&self, family: &str, weight: FontWeight, style: FontStyle) -> bool {
        self.fonts.iter().enumerate().any(|(i, f)| {
            self.matches_family(i, f, family)
                && self.is_bold(i) == (weight == FontWeight::Bold)
                && self.is_italic(i) == (style == FontStyle::Italic)
        })
    }

    /// Whether the collection has a font that can actually draw `c`.
    ///
    /// [`Self::select_for_char`] always returns some font as long as the collection is not
    /// empty (its last resort is "the first font", which renders as tofu), so a caller that
    /// wants to know whether it will be tofu has to check whether the returned font really
    /// has a glyph for `c`. This packages that check up.
    pub fn can_render(
        &self,
        families: &[String],
        weight: FontWeight,
        style: FontStyle,
        c: char,
    ) -> bool {
        self.select_for_char(families, weight, style, c)
            .and_then(|index| self.get(index))
            .is_some_and(|font| font.has_glyph(c))
    }

    /// Whether `c` can be drawn by a face of the requested `weight`/`style`.
    ///
    /// [`Self::can_render`] only asks "will it be tofu", so it returns `true` when the only
    /// font with `c` is a Bold face pulled in by automatic fallback. Stopping the font
    /// search on that alone would draw regular-weight characters in the Bold face (an
    /// order-dependent bug where bold Japanese appearing earlier in the document makes all
    /// later Japanese bold).
    ///
    /// A match is only required of automatic fallback faces: when the only font with `c` is
    /// one the user named (`--font`/`@font-face`), this stays `true`. That one was chosen
    /// deliberately, so rather than swapping in a system font behind the user's back, we
    /// cover it with faux bold or faux italic.
    pub fn can_render_with_matching_face(
        &self,
        families: &[String],
        weight: FontWeight,
        style: FontStyle,
        c: char,
    ) -> bool {
        let Some(index) = self.select_for_char(families, weight, style, c) else {
            return false;
        };
        if !self.get(index).is_some_and(|font| font.has_glyph(c)) {
            return false;
        }
        if !self.auto_fallbacks.get(index).copied().unwrap_or(false) {
            return true;
        }
        self.is_bold(index) == (weight == FontWeight::Bold)
            && self.is_italic(index) == (style == FontStyle::Italic)
    }

    /// Whether font `index` really counts as Bold. An `@font-face` `font-weight`
    /// declaration wins; without one, the font's own OS/2 weight decides.
    pub fn is_bold(&self, index: usize) -> bool {
        match self.declared_weights.get(index).copied().flatten() {
            Some(weight) => weight == FontWeight::Bold,
            None => self
                .fonts
                .get(index)
                .is_some_and(|f| f.weight() >= BOLD_WEIGHT_THRESHOLD),
        }
    }

    /// Whether font `index` really counts as Italic. An `@font-face` `font-style`
    /// declaration wins; without one, the italic flags in the font's own `post`/OS2 tables
    /// decide.
    pub fn is_italic(&self, index: usize) -> bool {
        match self.declared_styles.get(index).copied().flatten() {
            Some(style) => style == FontStyle::Italic,
            None => self.fonts.get(index).is_some_and(|f| f.is_italic()),
        }
    }

    fn matches_family(&self, index: usize, font: &Font, family: &str) -> bool {
        match &self.declared_families[index] {
            Some(declared) => declared.eq_ignore_ascii_case(family),
            None => font
                .family_name()
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(family)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
    const DEJAVU_BOLD_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/DejaVuSans-Bold.ttf"
    );
    const CJK_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/NotoSansCJK-Regular.ttc"
    );

    fn dejavu() -> Font {
        Font::load(DEJAVU_PATH).expect("should load bundled DejaVu test font")
    }

    fn dejavu_bold() -> Font {
        Font::load(DEJAVU_BOLD_PATH).expect("should load bundled DejaVu Bold test font")
    }

    fn cjk() -> Font {
        // face index 0 = Noto Sans CJK JP
        Font::load_indexed(CJK_PATH, 0).expect("should load bundled CJK test font")
    }

    fn select(
        collection: &FontCollection,
        family: &str,
        weight: FontWeight,
        style: FontStyle,
        c: char,
    ) -> Option<usize> {
        collection.select_for_char(&[family.to_string()], weight, style, c)
    }

    #[test]
    fn selects_font_matching_family_name_when_it_has_the_glyph() {
        let collection = FontCollection::new(vec![dejavu(), cjk()]);
        let index = select(
            &collection,
            "DejaVu Sans",
            FontWeight::Normal,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn falls_back_to_any_font_that_has_the_glyph_when_family_does_not_match() {
        let collection = FontCollection::new(vec![dejavu(), cjk()]);
        // "sans-serif" matches neither font's name, so the choice should come from
        // coverage alone.
        let index = select(
            &collection,
            "sans-serif",
            FontWeight::Normal,
            FontStyle::Normal,
            '日',
        )
        .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn has_family_reflects_both_own_name_and_declared_overrides() {
        let mut collection = FontCollection::new(vec![dejavu()]);
        assert!(collection.has_family("DejaVu Sans"));
        assert!(!collection.has_family("Custom Brand"));

        collection.push_font_face("Custom Brand".to_string(), None, None, Vec::new(), cjk());
        assert!(collection.has_family("Custom Brand"));
    }

    #[test]
    fn has_matching_face_is_weight_aware_unlike_has_family() {
        let collection = FontCollection::new(vec![dejavu()]);
        // "DejaVu Sans" itself is registered, but only in Regular. A Bold request should
        // not match (in contrast to has_family, which ignores weight and is therefore true).
        assert!(collection.has_family("DejaVu Sans"));
        assert!(collection.has_matching_face("DejaVu Sans", FontWeight::Normal, FontStyle::Normal));
        assert!(!collection.has_matching_face("DejaVu Sans", FontWeight::Bold, FontStyle::Normal));
    }

    #[test]
    fn can_render_with_matching_face_rejects_a_fallback_face_of_the_wrong_weight() {
        // The state where automatic fallback has pulled in only a Bold face (which
        // happens when bold Japanese appears earlier in the document). can_render is true
        // because it "will not be tofu", but drawing regular text in Bold is wrong, so keep searching.
        let mut collection = FontCollection::new(vec![]);
        collection.push_fallback_font_face("DejaVu Sans".to_string(), dejavu_bold());

        assert!(collection.can_render(&[], FontWeight::Normal, FontStyle::Normal, 'A'));
        assert!(!collection.can_render_with_matching_face(
            &[],
            FontWeight::Normal,
            FontStyle::Normal,
            'A'
        ));
        assert!(collection.can_render_with_matching_face(
            &[],
            FontWeight::Bold,
            FontStyle::Normal,
            'A'
        ));

        // Once a Regular face is added, both weights are covered by a matching face.
        collection.push_fallback_font_face("DejaVu Sans".to_string(), dejavu());
        assert!(collection.can_render_with_matching_face(
            &[],
            FontWeight::Normal,
            FontStyle::Normal,
            'A'
        ));
    }

    #[test]
    fn can_render_with_matching_face_accepts_an_explicit_font_of_the_wrong_weight() {
        // When there is only the one font the user named (`--font`/`@font-face`), the rule
        // is to cover it with faux bold rather than swapping in a system font behind their
        // back, so a differing weight must not keep the search going.
        let collection = FontCollection::new(vec![dejavu()]);
        assert!(collection.can_render_with_matching_face(
            &[],
            FontWeight::Bold,
            FontStyle::Normal,
            'A'
        ));
    }

    #[test]
    fn font_face_declared_family_takes_priority_over_the_fonts_own_name_table() {
        // Register the same DejaVu Sans twice: index 0 plain (matched on the internal name
        // "DejaVu Sans"), and index 1 as if loaded from `@font-face { font-family: "Custom Brand"; }`.
        // "Custom Brand" matches neither font's internal name, so without the declared-name
        // override it would not be found by name, would fall back on coverage alone (to the
        // first entry, index 0), and would not be the expected index 1.
        let mut collection = FontCollection::new(vec![dejavu()]);
        collection.push_font_face("Custom Brand".to_string(), None, None, Vec::new(), dejavu());

        let index = select(
            &collection,
            "Custom Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn falls_back_to_first_font_when_no_font_has_the_glyph() {
        let collection = FontCollection::new(vec![dejavu()]);
        // DejaVu Sans has no CJK, so even the fallback lands on the first entry (0).
        let index = select(
            &collection,
            "sans-serif",
            FontWeight::Normal,
            FontStyle::Normal,
            '日',
        )
        .unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn empty_collection_returns_none() {
        let collection = FontCollection::new(vec![]);
        assert_eq!(
            collection.select_for_char(&[], FontWeight::Normal, FontStyle::Normal, 'A'),
            None
        );
    }

    #[test]
    fn is_bold_reads_the_fonts_own_os2_weight_when_no_font_face_override_is_set() {
        let collection = FontCollection::new(vec![dejavu(), dejavu_bold()]);
        assert!(!collection.is_bold(0));
        assert!(collection.is_bold(1));
    }

    #[test]
    fn is_bold_and_is_italic_prefer_the_font_face_declared_override() {
        let mut collection = FontCollection::new(vec![]);
        // Really a Regular DejaVu Sans, but registered as if loaded from
        // `@font-face { font-weight: bold; font-style: italic; }`. The declaration should win over the real metrics.
        collection.push_font_face(
            "Declared Brand".to_string(),
            Some(FontWeight::Bold),
            Some(FontStyle::Italic),
            Vec::new(),
            dejavu(),
        );
        assert!(collection.is_bold(0));
        assert!(collection.is_italic(0));
    }

    #[test]
    fn select_for_char_prefers_the_real_bold_face_over_the_regular_one() {
        let collection = FontCollection::new(vec![dejavu(), dejavu_bold()]);
        // Both match the family name "DejaVu Sans", but a request for weight: Bold should
        // pick index 1, which really is Bold (so index 0 does not have to rely on faux bold).
        let index = select(
            &collection,
            "DejaVu Sans",
            FontWeight::Bold,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn select_for_char_falls_back_to_the_regular_face_when_no_bold_face_matches() {
        let collection = FontCollection::new(vec![dejavu()]);
        // With no Bold font available, fall back to the family-matching Regular font
        // (the caller makes up the difference with faux bold).
        let index = select(
            &collection,
            "DejaVu Sans",
            FontWeight::Bold,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 0);
        assert!(!collection.is_bold(index));
    }

    #[test]
    fn unicode_range_excludes_a_font_even_when_it_has_the_glyph() {
        // index 0 is a DejaVu Sans that really does have a glyph for 'e-acute' (U+00E9), but
        // it declares `unicode-range: U+0-7F` (Basic Latin only), so it is out of scope.
        // index 1 is the same DejaVu Sans registered again with no range.
        // If the hard filter works, index 0 is excluded regardless of the glyph and index 1
        // is chosen.
        let mut collection = FontCollection::new(vec![]);
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x0,
                end: 0x7F,
            }],
            dejavu(),
        );
        collection.push_font_face("Brand".to_string(), None, None, Vec::new(), dejavu());

        assert!(
            collection.get(0).unwrap().has_glyph('é'),
            "test premise: DejaVu Sans should have a glyph for 'e-acute'"
        );

        let index = select(
            &collection,
            "Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            'é',
        )
        .unwrap();
        assert_eq!(index, 1);
    }

    #[test]
    fn unicode_range_does_not_exclude_a_char_inside_the_declared_range() {
        let mut collection = FontCollection::new(vec![]);
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x0,
                end: 0x7F,
            }],
            dejavu(),
        );

        let index = select(
            &collection,
            "Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn unicode_range_unspecified_covers_the_whole_unicode_range() {
        // Backward-compatibility check on existing behaviour: a registration with no
        // unicode_range still covers the whole range.
        let collection = FontCollection::new(vec![dejavu()]);
        let index = select(
            &collection,
            "DejaVu Sans",
            FontWeight::Normal,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn overlapping_unicode_ranges_prefer_the_first_declared_font() {
        // When two fonts with the same family, weight/style and overlapping ranges cover
        // the same character, the one registered earlier in the CSS source
        // (registration order = scan order) should win.
        let mut collection = FontCollection::new(vec![]);
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x0,
                end: 0x7F,
            }],
            dejavu(),
        );
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x0,
                end: 0xFF,
            }],
            dejavu_bold(),
        );

        let index = select(
            &collection,
            "Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn falls_back_to_tofu_when_every_font_excludes_the_char_by_range() {
        let mut collection = FontCollection::new(vec![]);
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x0,
                end: 0x7F,
            }],
            dejavu(),
        );

        let index = select(
            &collection,
            "Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            '日',
        )
        .unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn unicode_range_splits_a_latin_and_a_cjk_face_declared_under_the_same_family() {
        // The classic webfont delivery pattern: an alphanumeric font and a CJK font used
        // together under one family name, split by unicode-range.
        let mut collection = FontCollection::new(vec![]);
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x0,
                end: 0x24F,
            }],
            dejavu(),
        );
        collection.push_font_face(
            "Brand".to_string(),
            None,
            None,
            vec![UnicodeRange {
                start: 0x4E00,
                end: 0x9FFF,
            }],
            cjk(),
        );

        let latin_index = select(
            &collection,
            "Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(
            latin_index, 0,
            "Latin characters should use the Latin-range face"
        );

        let cjk_index = select(
            &collection,
            "Brand",
            FontWeight::Normal,
            FontStyle::Normal,
            '日',
        )
        .unwrap();
        assert_eq!(cjk_index, 1, "CJK characters should use the CJK-range face");
    }
}
