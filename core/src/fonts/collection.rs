//! A collection of fonts, and fallback selection based on `font-family`, weight, style
//! and glyph coverage.
//!
//! Discovering system fonts (scanning the OS font directories) is [`super::system`]'s job.

use cssparser::UnicodeRange;

use crate::style::{FontStyle, FontWeight};

use super::font::Font;

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
    /// Selection order: (1) of the fonts whose family matches and that can draw `c`, the
    /// one closest to `weight`/`style` (see [`Self::best_match`]); (2) regardless of family,
    /// the family of the first registered font that can draw `c` (one that is bold/italic
    /// as requested if there is one), and in it the face closest to `weight`/`style`
    /// (see [`Self::first_registered_match`]); (3) failing all that, the first font
    /// in the collection (which renders as tofu). `None` only when the collection is empty.
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
        // bold or italic face here either). Registration order picks the family: the first
        // font that can draw `c` and is bold/italic as requested, or else the first that
        // can draw `c`. Weight closeness only picks the face within that family, so a
        // fallback family registered later (a CJK font added for Japanese text, say) never
        // takes over Latin text because one of its faces is nearer the requested weight.
        if let Some(first) = self.first_registered_match(weight, style, c) {
            let family = self.family_of(first);
            return self.best_match(weight, style, c, |i, _| {
                i == first
                    || family.is_some_and(|family| {
                        self.family_of(i)
                            .is_some_and(|other| other.eq_ignore_ascii_case(family))
                    })
            });
        }

        Some(0)
    }

    /// The first font in registration order whose `unicode-range` (if declared) contains
    /// `c`, that has a glyph for `c` and that is bold/italic as `weight`/`style` ask; failing
    /// that, the first font that can draw `c` at all.
    fn first_registered_match(
        &self,
        weight: FontWeight,
        style: FontStyle,
        c: char,
    ) -> Option<usize> {
        let mut first_match = None;
        for (i, f) in self.fonts.iter().enumerate() {
            if !self.in_unicode_range(i, c) || !f.has_glyph(c) {
                continue;
            }
            first_match.get_or_insert(i);
            if self.is_bold(i) == weight.is_bold()
                && self.is_italic(i) == (style == FontStyle::Italic)
            {
                return Some(i);
            }
        }
        first_match
    }

    /// The family font `index` is matched under: its CSS-declared family, or else the
    /// family in its own `name` table (the same names [`Self::matches_family`] compares).
    fn family_of(&self, index: usize) -> Option<&str> {
        match self.declared_families.get(index)? {
            Some(declared) => Some(declared),
            None => self.fonts.get(index)?.family_name_str(),
        }
    }

    /// Among the fonts satisfying `eligible`, whose `unicode-range` (if declared) contains
    /// `c`, and that have a glyph for `c`, pick the one closest to `weight`/`style`, in the
    /// order of CSS Fonts 4 § 5.2 step 4: first the style (a face of the requested style
    /// if there is one, any face otherwise), then the weight ([`weight_preference`]).
    ///
    /// `unicode-range` acts as a hard filter: if a declared range does not contain `c`, the
    /// font is excluded from the candidates even when it really does have a glyph for `c`.
    /// Between equally good fonts the first in scan order (= registration order = CSS source
    /// order) is taken, so overlapping ranges within the same family/weight/style naturally
    /// resolve as "declaration order wins".
    fn best_match(
        &self,
        weight: FontWeight,
        style: FontStyle,
        c: char,
        mut eligible: impl FnMut(usize, &Font) -> bool,
    ) -> Option<usize> {
        let wants_italic = style == FontStyle::Italic;
        self.fonts
            .iter()
            .enumerate()
            .filter(|&(i, f)| eligible(i, f) && self.in_unicode_range(i, c) && f.has_glyph(c))
            .min_by_key(|&(i, _)| {
                (
                    self.is_italic(i) != wants_italic,
                    weight_preference(weight, self.weight(i)),
                )
            })
            .map(|(i, _)| i)
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

    /// Whether a font matching `family` that also really satisfies `weight`/`style` (bold or
    /// not, italic or not) is already in the collection. System font discovery uses this to look only for the
    /// weights and styles the existing fonts cannot cover (a Bold request against a family
    /// that only has Regular, say).
    pub fn has_matching_face(&self, family: &str, weight: FontWeight, style: FontStyle) -> bool {
        self.fonts.iter().enumerate().any(|(i, f)| {
            self.matches_family(i, f, family)
                && self.is_bold(i) == weight.is_bold()
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
        self.is_bold(index) == weight.is_bold()
            && self.is_italic(index) == (style == FontStyle::Italic)
    }

    /// The weight of font `index`. An `@font-face` `font-weight` declaration wins; without
    /// one, the font's own OS/2 weight decides.
    fn weight(&self, index: usize) -> FontWeight {
        match self.declared_weights.get(index).copied().flatten() {
            Some(weight) => weight,
            None => self
                .fonts
                .get(index)
                .map_or(FontWeight::NORMAL, |f| FontWeight(f.weight())),
        }
    }

    /// Whether font `index` really counts as Bold ([`FontWeight::is_bold`] of its weight).
    pub fn is_bold(&self, index: usize) -> bool {
        self.weight(index).is_bold()
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

/// How well a face of weight `face` suits a request for `desired`: the lower, the better.
/// This is the weight step of CSS Fonts 4 § 5.2 (step 4): an exact match first; then, for
/// a desired weight of 400 to 500, heavier weights up to 500 ascending, lighter weights
/// descending and weights above 500 ascending; below 400, lighter weights descending and
/// then heavier ones ascending; above 500, heavier weights ascending and then lighter ones
/// descending.
fn weight_preference(desired: FontWeight, face: FontWeight) -> (u8, u16) {
    let (desired, face) = (desired.0, face.0);
    let (lighter, heavier) = (desired.saturating_sub(face), face.saturating_sub(desired));
    match desired {
        400..=500 if face >= desired && face <= 500 => (0, heavier),
        400..=500 if face < desired => (1, lighter),
        400..=500 => (2, heavier),
        ..400 if face <= desired => (0, lighter),
        ..400 => (1, heavier),
        _ if face >= desired => (0, heavier),
        _ => (1, lighter),
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
        assert!(collection.has_matching_face("DejaVu Sans", FontWeight::NORMAL, FontStyle::Normal));
        assert!(!collection.has_matching_face("DejaVu Sans", FontWeight::BOLD, FontStyle::Normal));
    }

    #[test]
    fn can_render_with_matching_face_rejects_a_fallback_face_of_the_wrong_weight() {
        // The state where automatic fallback has pulled in only a Bold face (which
        // happens when bold Japanese appears earlier in the document). can_render is true
        // because it "will not be tofu", but drawing regular text in Bold is wrong, so keep searching.
        let mut collection = FontCollection::new(vec![]);
        collection.push_fallback_font_face("DejaVu Sans".to_string(), dejavu_bold());

        assert!(collection.can_render(&[], FontWeight::NORMAL, FontStyle::Normal, 'A'));
        assert!(!collection.can_render_with_matching_face(
            &[],
            FontWeight::NORMAL,
            FontStyle::Normal,
            'A'
        ));
        assert!(collection.can_render_with_matching_face(
            &[],
            FontWeight::BOLD,
            FontStyle::Normal,
            'A'
        ));

        // Once a Regular face is added, both weights are covered by a matching face.
        collection.push_fallback_font_face("DejaVu Sans".to_string(), dejavu());
        assert!(collection.can_render_with_matching_face(
            &[],
            FontWeight::NORMAL,
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
            FontWeight::BOLD,
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
            collection.select_for_char(&[], FontWeight::NORMAL, FontStyle::Normal, 'A'),
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
            Some(FontWeight::BOLD),
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
            FontWeight::BOLD,
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
            FontWeight::BOLD,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(index, 0);
        assert!(!collection.is_bold(index));
    }

    /// One `@font-face` family "Brand" with a face per `(weight, style)`, declared in the
    /// given order. Every face is the same Regular DejaVu Sans; only the declared
    /// descriptors tell them apart.
    fn brand_faces(faces: &[(u16, FontStyle)]) -> FontCollection {
        let mut collection = FontCollection::new(vec![]);
        for &(weight, style) in faces {
            collection.push_font_face(
                "Brand".to_string(),
                Some(FontWeight(weight)),
                Some(style),
                Vec::new(),
                dejavu(),
            );
        }
        collection
    }

    /// The declared `(weight, style)` of the face chosen for `weight`/`style`.
    fn chosen(
        collection: &FontCollection,
        faces: &[(u16, FontStyle)],
        weight: u16,
        style: FontStyle,
    ) -> (u16, FontStyle) {
        let index = select(collection, "Brand", FontWeight(weight), style, 'A').unwrap();
        faces[index]
    }

    /// The declared weight of the upright face chosen for `weight`, with upright faces of
    /// the `declared` weights.
    fn chosen_weight(declared: &[u16], weight: u16) -> u16 {
        let faces: Vec<_> = declared.iter().map(|&w| (w, FontStyle::Normal)).collect();
        chosen(&brand_faces(&faces), &faces, weight, FontStyle::Normal).0
    }

    #[test]
    fn select_for_char_matches_font_face_weights_regardless_of_declaration_order() {
        // #151: with 300 to 700 declared, each weight gets its own face (100 and 200 the
        // lightest, 800 and 900 the heaviest), whatever order the rules come in.
        for declared in [
            [300, 400, 500, 600, 700],
            [700, 600, 500, 400, 300],
            [500, 300, 700, 400, 600],
        ] {
            for (requested, expected) in [
                (100, 300),
                (200, 300),
                (300, 300),
                (400, 400),
                (500, 500),
                (600, 600),
                (700, 700),
                (800, 700),
                (900, 700),
            ] {
                assert_eq!(
                    chosen_weight(&declared, requested),
                    expected,
                    "font-weight: {requested} with faces declared as {declared:?}"
                );
            }
        }
    }

    #[test]
    fn select_for_char_falls_back_between_weights_in_css_fonts_4_order() {
        // 400 to 500: heavier up to 500, then lighter, then above 500.
        assert_eq!(chosen_weight(&[400, 500, 600], 450), 500);
        assert_eq!(chosen_weight(&[300, 600], 400), 300);
        assert_eq!(chosen_weight(&[600, 300], 500), 300);
        assert_eq!(chosen_weight(&[900, 600], 400), 600);
        // Below 400: lighter first, then heavier.
        assert_eq!(chosen_weight(&[400, 200], 300), 200);
        assert_eq!(chosen_weight(&[500, 400], 300), 400);
        // Above 500: heavier first, then lighter.
        assert_eq!(chosen_weight(&[500, 800, 700], 600), 700);
        assert_eq!(chosen_weight(&[400, 500], 600), 500);
        // Only 400 and 500, in either order: 400 and 500 each get their own face.
        for declared in [[400, 500], [500, 400]] {
            assert_eq!(chosen_weight(&declared, 400), 400);
            assert_eq!(chosen_weight(&declared, 500), 500);
        }
    }

    #[test]
    fn select_for_char_leaves_synthetic_bold_to_a_bold_request_without_a_heavy_face() {
        // 700 against 400 and 500 takes the 500 face, which is not bold, so the caller
        // still draws it with synthetic bold.
        let faces = [(500, FontStyle::Normal), (400, FontStyle::Normal)];
        let collection = brand_faces(&faces);
        let index = select(
            &collection,
            "Brand",
            FontWeight::BOLD,
            FontStyle::Normal,
            'A',
        )
        .unwrap();
        assert_eq!(faces[index].0, 500);
        assert!(!collection.is_bold(index));
    }

    #[test]
    fn select_for_char_keeps_registration_order_between_families_when_none_is_named() {
        // A `--font` Medium (500) registered before a fallback at 400: text with no
        // `font-family` stays in the first family, even though the other face is the
        // closer weight. Within that family, the closest face still wins.
        let face = |collection: &mut FontCollection, family: &str, weight: u16| {
            collection.push_font_face(
                family.to_string(),
                Some(FontWeight(weight)),
                Some(FontStyle::Normal),
                Vec::new(),
                dejavu(),
            );
        };
        let normal = |collection: &FontCollection, weight: FontWeight| {
            collection
                .select_for_char(&[], weight, FontStyle::Normal, 'A')
                .unwrap()
        };

        let mut collection = FontCollection::new(vec![]);
        face(&mut collection, "Main", 500);
        face(&mut collection, "Other", 400);
        assert_eq!(normal(&collection, FontWeight::NORMAL), 0);

        // With a 300 and a 400 face of `Main` ahead of `Other`, 400 takes Main's 400 face
        // and 300 its 300 face.
        let mut collection = FontCollection::new(vec![]);
        face(&mut collection, "Main", 300);
        face(&mut collection, "Other", 400);
        face(&mut collection, "Main", 400);
        assert_eq!(normal(&collection, FontWeight::NORMAL), 2);
        assert_eq!(normal(&collection, FontWeight(300)), 0);

        // A bold request still goes to the first registered bold face, as before, when the
        // first family has none.
        let mut collection = FontCollection::new(vec![]);
        face(&mut collection, "Main", 500);
        face(&mut collection, "Other", 700);
        assert_eq!(normal(&collection, FontWeight::BOLD), 1);
    }

    #[test]
    fn select_for_char_matches_the_style_before_the_weight() {
        // As in CSS Fonts 4 § 5.2, a face of the right style wins over one of the right
        // weight: italic 400 text takes the 700 italic face rather than the 400 upright one,
        // and upright 700 text takes the 400 upright face (with synthetic bold).
        let faces = [(400, FontStyle::Normal), (700, FontStyle::Italic)];
        let collection = brand_faces(&faces);
        assert_eq!(
            chosen(&collection, &faces, 400, FontStyle::Italic),
            (700, FontStyle::Italic)
        );
        assert_eq!(
            chosen(&collection, &faces, 700, FontStyle::Normal),
            (400, FontStyle::Normal)
        );
        // With no face of the requested style, the weight decides among the others.
        let faces = [(300, FontStyle::Normal), (500, FontStyle::Normal)];
        let collection = brand_faces(&faces);
        assert_eq!(
            chosen(&collection, &faces, 500, FontStyle::Italic),
            (500, FontStyle::Normal)
        );
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
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
            FontWeight::NORMAL,
            FontStyle::Normal,
            '日',
        )
        .unwrap();
        assert_eq!(cjk_index, 1, "CJK characters should use the CJK-range face");
    }
}
