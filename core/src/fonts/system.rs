//! Resolving system fonts by scanning the OS font directories (using `fontdb`).
//!
//! The CSS generic family names (`monospace`/`serif`/`sans-serif`) are resolved from our
//! own candidate lists rather than left to `fontdb`'s generic resolution (fontconfig, on
//! Linux). In a minimal environment with no fontconfig, `fontdb` falls back to hard-coded
//! defaults (`Arial` and the like) that are inconsistent across OSes and are not
//! necessarily installed. If every candidate misses, `monospace` alone looks for a
//! monospaced face using `fontdb`'s per-face metadata (`FaceInfo::monospaced`, which does
//! not depend on fontconfig). Failing even that, resolution is given up and left to the
//! glyph-coverage fallback [`crate::fonts::FontCollection`] already has.
//! `sans-serif` was originally "not resolved, being the same as the default `font-family`",
//! but since the default `font-family` was separated out to empty (unset), it now resolves
//! to a gothic face only when written explicitly. An element with nothing specified goes
//! through an empty `font-family` to `select_for_char`'s fallback (that is, the `--font`
//! font), so the behaviour of `--font` being the default is preserved. When
//! `--gothic-font` is given, it takes priority as `sans-serif`.
//!
//! Separately from resolution by family name, there is also a path for finding a system
//! font that can draw a character when no font in the document can
//! ([`SystemFonts::load_covering`], [`load_fonts_for_uncovered_chars`]).
//! Where no family name offers any clue - a Japanese document with no `font-family`, say -
//! [`FontCollection`]'s glyph-coverage fallback has no candidate to choose from at all,
//! so this is where that gap is filled.

use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use skrifa::MetadataProvider;

use crate::html::{Dom, NodeData, NodeId};
use crate::style::{ComputedStyle, Display, FontStyle, FontWeight};

use super::collection::FontCollection;
use super::font::Font;

/// The CSS generic family names (matched case-insensitively).
const GENERIC_FAMILIES: &[&str] = &["serif", "sans-serif", "monospace", "cursive", "fantasy"];

/// Candidate concrete font names for each generic family, in priority order (chosen for being likely to exist).
///
/// `cursive`/`fantasy` have no candidates (so they are not resolved): they vary too much
/// between environments and there is little practical demand. `sans-serif` is resolved only
/// when written explicitly, since the default `font-family` was separated out to empty.
const GENERIC_FAMILY_CANDIDATES: &[(&str, &[&str])] = &[
    (
        "monospace",
        &[
            "DejaVu Sans Mono",
            "Liberation Mono",
            "Noto Sans Mono",
            "Ubuntu Mono",
            "Menlo",
            "Consolas",
            "Courier New",
            "Courier",
        ],
    ),
    (
        "serif",
        &[
            "DejaVu Serif",
            "Liberation Serif",
            "Noto Serif",
            "Times New Roman",
            "Times",
            "Georgia",
            // The mincho face bundled with the official Docker image. Placed last so it is
            // picked up in a minimal environment (that image) where every latin candidate misses.
            "BIZ UDPMincho",
            "BIZ UDMincho",
        ],
    ),
    (
        // `sans-serif` looks for a gothic face only when specified explicitly. Latin
        // gothic candidates (in practice `--gothic-font` overrides this deterministically).
        "sans-serif",
        &[
            "DejaVu Sans",
            "Liberation Sans",
            "Noto Sans",
            "Arial",
            "Helvetica",
            "Ubuntu",
            "Verdana",
            "Tahoma",
            // The gothic face bundled with the official Docker image (same reason as `serif` above).
            "BIZ UDPGothic",
            "BIZ UDGothic",
        ],
    ),
];

/// Candidate fonts that can draw CJK (kanji, kana, hangul), in priority order.
///
/// The unified CJK ideographs are shared between Japanese, Korean and Chinese, so rather
/// than splitting by language there is one candidate list, tried in order while checking
/// whether each can actually draw the character ([`Font::has_glyph`]). Hiragino Sans, for
/// example, has no hangul, so a hangul search naturally moves on to the next candidate.
///
/// The order - Japanese, then Korean, then Simplified Chinese, then Traditional Chinese -
/// reflects Japanese being dominant in the business-document use case. Glyph shapes differ by
/// country, but that cannot be decided without the `lang` attribute, so the initial scope
/// leaves it alone (when it guesses wrong, `--gothic-font` and friends override deterministically).
const CJK_FAMILY_CANDIDATES: &[&str] = &[
    // Japanese
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Yu Gothic",
    "Meiryo",
    "MS Gothic",
    "IPAGothic",
    "TakaoPGothic",
    "VL PGothic",
    // The gothic face bundled with the official Docker image.
    "BIZ UDPGothic",
    "BIZ UDGothic",
    // Korean
    "Noto Sans CJK KR",
    "Noto Sans KR",
    "Apple SD Gothic Neo",
    "Malgun Gothic",
    // Simplified Chinese
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "PingFang SC",
    "Microsoft YaHei",
    "SimSun",
    // Traditional Chinese
    "Noto Sans CJK TC",
    "Noto Sans TC",
    "PingFang TC",
    "Microsoft JhengHei",
    "PMingLiU",
];

/// Whether `c` is a CJK character (used to decide whether to consult [`CJK_FAMILY_CANDIDATES`]).
///
/// This is only a narrowing step so the candidate list is tried before the full scan
/// ([`SystemFonts::load_any_covering`]), so the boundaries need not be exact (anything
/// missed here is caught by the full scan).
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F      // CJK symbols and punctuation
        | 0x3040..=0x309F    // Hiragana
        | 0x30A0..=0x30FF    // Katakana
        | 0x3130..=0x318F    // Hangul compatibility jamo
        | 0x3400..=0x4DBF    // CJK unified ideographs extension A
        | 0x4E00..=0x9FFF    // CJK unified ideographs
        | 0xAC00..=0xD7AF    // Hangul syllables
        | 0xF900..=0xFAFF    // CJK compatibility ideographs
        | 0xFF00..=0xFFEF    // Halfwidth and fullwidth forms
        | 0x20000..=0x2FA1F  // CJK unified ideographs extension B and later
    )
}

pub struct SystemFonts {
    db: fontdb::Database,
}

impl SystemFonts {
    /// Build the database by scanning the OS font directories
    /// (metadata only; the font files themselves are not read yet).
    pub fn scan() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        Self { db }
    }

    #[cfg(test)]
    pub(super) fn from_dir(dir: &std::path::Path) -> Self {
        let mut db = fontdb::Database::new();
        db.load_fonts_dir(dir);
        Self { db }
    }

    /// Load the system font named `family` (case-insensitively).
    /// `weight`/`style` are passed straight to `fontdb`'s CSS-like matching, so with
    /// `weight: Bold`, for example, a real Bold face in that family is chosen if one exists
    /// (if not, `fontdb` returns the closest face instead, and the caller then confirms the
    /// reality with `FontCollection::is_bold` and the like before applying faux bold).
    /// `None` if no font matches.
    pub fn load(&self, family: &str, weight: FontWeight, style: FontStyle) -> Option<Font> {
        // `fontdb::Database::query` only matches family names exactly (case-sensitively),
        // so first find the actual registered name ignoring case, then query again with
        // that name.
        let exact_name = self
            .db
            .faces()
            .flat_map(|info| info.families.iter())
            .find(|(name, _)| name.eq_ignore_ascii_case(family))
            .map(|(name, _)| name.clone())?;

        let query = fontdb::Query {
            families: &[fontdb::Family::Name(&exact_name)],
            weight: to_fontdb_weight(weight),
            style: to_fontdb_style(style),
            ..Default::default()
        };
        let id = self.db.query(&query)?;
        self.db
            .with_face_data(id, |data, index| {
                Font::from_bytes(data.to_vec(), index).ok()
            })
            .flatten()
            // A font with no outlines (a bitmap colour emoji font, say) can draw nothing
            // even when the name matches, so it is not taken. Every system font lookup
            // goes through here, so the check lives in this one place.
            .filter(|font| font.has_outlines())
    }

    /// Resolve a CSS generic family name (`monospace`/`serif`) to a concrete font by trying
    /// our own candidate list ([`GENERIC_FAMILY_CANDIDATES`]) in priority order. If every
    /// candidate misses, `monospace` alone looks for a monospaced face using `fontdb`'s
    /// `FaceInfo::monospaced` flag. Returns `None` for `sans-serif` (deliberately out of
    /// scope, being the same as the default `font-family`), for `cursive`/`fantasy`, for a
    /// name that is not generic, and when nothing is found.
    pub fn load_generic(
        &self,
        generic: &str,
        weight: FontWeight,
        style: FontStyle,
    ) -> Option<Font> {
        let candidates = GENERIC_FAMILY_CANDIDATES
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(generic))
            .map(|(_, candidates)| *candidates)?;

        for candidate in candidates {
            if let Some(font) = self.load(candidate, weight, style) {
                return Some(font);
            }
        }

        if generic.eq_ignore_ascii_case("monospace") {
            return self.load_any_monospaced(weight, style);
        }
        None
    }

    /// Find one system font that can draw `c` and return it along with the real family name
    /// it was found under.
    ///
    /// Resolution has two stages (fontconfig's generic resolution is not used):
    ///
    /// 1. For CJK, `load` each of [`CJK_FAMILY_CANDIDATES`] in turn and take the first that
    ///    can actually draw `c`
    /// 2. If the candidates miss, scan every face with [`Self::load_any_covering`]
    ///    (the last resort)
    ///
    /// The family name comes back too so the added font can be registered with
    /// [`FontCollection::push_font_face`] under its real family name. An empty or made-up
    /// name would cause unintended matches in `matches_family`.
    pub fn load_covering(
        &self,
        c: char,
        weight: FontWeight,
        style: FontStyle,
    ) -> Option<(String, Font)> {
        if is_cjk(c) {
            for candidate in CJK_FAMILY_CANDIDATES {
                match self.load(candidate, weight, style) {
                    Some(font) if font.has_glyph(c) => {
                        return Some(((*candidate).to_string(), font))
                    }
                    _ => continue,
                }
            }
        }
        self.load_any_covering(c, weight, style)
    }

    /// Scan every face in the DB looking for one that can draw `c` (the last resort).
    ///
    /// Whether the glyph exists is decided by reading the cmap on the spot, avoiding the
    /// conversion to a [`Font`] (which copies the data), and the face that hits is then
    /// `load`ed again by family name (so `load` handles the weight/style face selection).
    /// Reading every face's contents is still involved, so this sits where it is only
    /// reached once every candidate in the list has missed.
    fn load_any_covering(
        &self,
        c: char,
        weight: FontWeight,
        style: FontStyle,
    ) -> Option<(String, Font)> {
        for info in self.db.faces() {
            let Some((family, _)) = info.families.first() else {
                continue;
            };
            // Being in the `cmap` is not enough: outlines are required too (the same check
            // as `Font::has_glyph`, done here without building a `Font`). Colour emoji
            // fonts have a `cmap`, so without this we would wrongly decide we can draw.
            let covered = self
                .db
                .with_face_data(info.id, |data, index| {
                    skrifa::FontRef::from_index(data, index)
                        .map(|font| {
                            font.charmap().map(c).is_some()
                                && font.outline_glyphs().format().is_some()
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if !covered {
                continue;
            }
            // Another face of the same family (Bold, say) may be the right one, so look it
            // up again by family name and pick the face matching weight/style. Only if that
            // lookup fails do we take the face we used for the check.
            if let Some(font) = self.load(family, weight, style) {
                if font.has_glyph(c) {
                    return Some((family.clone(), font));
                }
            }
            let font = self
                .db
                .with_face_data(info.id, |data, index| {
                    Font::from_bytes(data.to_vec(), index).ok()
                })
                .flatten();
            if let Some(font) = font.filter(|font| font.has_glyph(c)) {
                return Some((family.clone(), font));
            }
        }
        None
    }

    /// Pick one face the font's own metadata calls "monospaced", then `load` it again by
    /// family name (so `load` handles the weight/style face selection).
    fn load_any_monospaced(&self, weight: FontWeight, style: FontStyle) -> Option<Font> {
        // Even with the monospace flag set, `load` may not take it (no outlines), so try
        // them in turn rather than stopping at the first. Colour emoji fonts are registered
        // as monospaced (every glyph has the same advance) and really do turn up here.
        self.db
            .faces()
            .filter(|info| info.monospaced)
            .filter_map(|info| info.families.first().map(|(name, _)| name.clone()))
            .find_map(|family| self.load(&family, weight, style))
    }

    /// For `src: local(...)` in `@font-face`. Directly load the one face matching `name`
    /// (a full name or PostScript name, case-insensitively).
    /// Unlike `load` (a CSS-style fallback search by family name plus weight/style), it does
    /// no fuzzy weight/style matching: `local()` means exactly one face, named uniquely.
    pub fn load_by_full_name(&self, name: &str) -> Option<Font> {
        let info = self.db.faces().find(|info| {
            info.post_script_name.eq_ignore_ascii_case(name)
                || info
                    .families
                    .iter()
                    .any(|(family_name, _)| family_name.eq_ignore_ascii_case(name))
        })?;
        self.db
            .with_face_data(info.id, |data, index| {
                Font::from_bytes(data.to_vec(), index).ok()
            })
            .flatten()
    }
}

fn to_fontdb_weight(weight: FontWeight) -> fontdb::Weight {
    match weight {
        FontWeight::Normal => fontdb::Weight::NORMAL,
        FontWeight::Bold => fontdb::Weight::BOLD,
    }
}

fn to_fontdb_style(style: FontStyle) -> fontdb::Style {
    match style {
        FontStyle::Normal => fontdb::Style::Normal,
        FontStyle::Italic => fontdb::Style::Italic,
    }
}

/// Of the font-family/weight/style combinations used in `styles`, load from `system` only
/// those `fonts` does not already have, and add them to `fonts`.
///
/// The check is per (family, weight, style) rather than per `family`, so if `--font` loaded
/// only the Regular of a family and the document uses bold, only that family's Bold face
/// is looked up from the system.
///
/// A CSS generic family name (`monospace` and friends) is resolved by
/// [`SystemFonts::load_generic`] and registered in `fonts` with the generic name itself as
/// the declared family name. That way `font-family: monospace` matches through
/// [`FontCollection::select_for_char`]'s ordinary family matching.
///
/// Scan order is fixed to document order ([`NodeId`] order), as in [`document_chars`].
/// `styles` is a `HashMap`, so its iteration order changes from run to run; iterating it
/// directly would make the order of the fonts added here (and so the PDF font numbers)
/// vary, emitting different bytes from the same HTML.
pub fn load_missing_system_fonts(
    fonts: &mut FontCollection,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    system: &SystemFonts,
) {
    let mut node_ids: Vec<NodeId> = styles.keys().copied().collect();
    node_ids.sort_by_key(|id| id.0);

    let mut seen = HashSet::new();
    for style in node_ids.iter().filter_map(|id| styles.get(id)) {
        for family in &style.font_family {
            let key = (family.clone(), style.font_weight, style.font_style);
            if !seen.insert(key) {
                continue;
            }
            if fonts.has_matching_face(family, style.font_weight, style.font_style) {
                continue;
            }
            let is_generic = GENERIC_FAMILIES
                .iter()
                .any(|g| g.eq_ignore_ascii_case(family));
            let font = if is_generic {
                system.load_generic(family, style.font_weight, style.font_style)
            } else {
                system.load(family, style.font_weight, style.font_style)
            };
            if let Some(font) = font {
                fonts.push_font_face(family.clone(), None, None, Vec::new(), font);
            }
        }
    }
}

/// For characters in the document that no font in `fonts` can draw, look for a system font
/// that can and add it to `fonts`.
///
/// Where [`load_missing_system_fonts`] works from family names, this works from the
/// characters actually used. It is the path that rescues cases where the family name is no
/// clue at all, such as a Japanese document with no `font-family`, and it is called
/// immediately after `load_missing_system_fonts`.
///
/// It covers the characters of text nodes plus the strings generated by `::before`/`::after`
/// ([`ComputedStyle::pseudo_before_content`] and friends, with counter() and quotes already
/// resolved). List marker symbols are not followed (initial scope).
///
/// The remaining characters are re-checked each time a font is added, so for a Japanese
/// document one CJK font covers everything that follows. Scan order is fixed to document
/// order ([`NodeId`] order) so the order of the fonts added (and so the PDF font numbers)
/// does not vary between runs.
pub fn load_fonts_for_uncovered_chars(
    fonts: &mut FontCollection,
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    system: &SystemFonts,
) {
    let mut seen = HashSet::new();
    // Collected up front because of borrowing (`fonts` is touched mutably, so `styles` and
    // `fonts` cannot both be borrowed during the walk).
    let chars: Vec<(char, FontWeight, FontStyle, Vec<String>)> = document_chars(dom, styles)
        .map(|(c, style)| {
            (
                c,
                style.font_weight,
                style.font_style,
                style.font_family.clone(),
            )
        })
        .collect();
    for (c, weight, style, families) in chars {
        cover_char(fonts, system, &mut seen, c, weight, style, &families);
    }
}

/// Enumerate the characters actually drawn in the document, in document order ([`NodeId`] order).
///
/// This covers the characters of text nodes plus the strings generated by `::before`/`::after`
/// ([`ComputedStyle::pseudo_before_content`] and friends, with `counter()` and quotes resolved).
/// List marker symbols are not followed (initial scope). Whitespace and control characters
/// are excluded, being handled by layout before any glyph lookup.
///
/// The order is fixed to document order so that the order of the fonts added here (and so
/// the PDF font numbers) does not vary between runs (`styles` is a `HashMap`, so its key
/// iteration order is unspecified).
fn document_chars<'a>(
    dom: &'a Dom,
    styles: &'a HashMap<NodeId, Rc<ComputedStyle>>,
) -> impl Iterator<Item = (char, &'a ComputedStyle)> {
    let mut out = Vec::new();
    collect_rendered_chars(dom, dom.document(), styles, &mut out);
    out.into_iter()
}

/// Collect the text and generated content under `node` in document order.
///
/// Elements with `display: none` are skipped along with their subtree (matching where
/// [`crate::layout::box_tree`] stops recursing). Including characters that are never drawn
/// would embed an unused CJK font because of one Japanese character in a `<script>` or
/// `<style>`, and would even change which font the body text uses.
///
/// A text node inherits its parent's computed style, so `display` is visible there too, but
/// when an element sits between the `display: none` element and the text
/// (`<div style="display:none"><span>...`) that element computes to `inline`, so a
/// per-node filter is not enough and the tree has to be walked.
///
/// `visibility: hidden` is not covered. It is only invisible, still occupying space, so a
/// font is needed to decide the line height.
fn collect_rendered_chars<'a>(
    dom: &'a Dom,
    node: NodeId,
    styles: &'a HashMap<NodeId, Rc<ComputedStyle>>,
    out: &mut Vec<(char, &'a ComputedStyle)>,
) {
    let Some(style) = styles.get(&node).map(Rc::as_ref) else {
        return;
    };
    if style.display == Display::None {
        return;
    }

    let text = match &dom.node(node).data {
        NodeData::Text { contents } => Some(contents.as_str()),
        _ => None,
    };
    let generated = [
        style.pseudo_before_content.as_deref(),
        style.pseudo_after_content.as_deref(),
    ];
    out.extend(
        text.into_iter()
            .chain(generated.into_iter().flatten())
            .flat_map(str::chars)
            .filter(|c| !c.is_whitespace() && !c.is_control())
            .map(|c| (c, style)),
    );

    for child in dom.children(node) {
        collect_rendered_chars(dom, child, styles, out);
    }
}

/// If no font in `fonts` can draw `c` at `weight`/`style`, look one up from the system and
/// add it. Combinations already checked are remembered in `seen` to avoid searching twice.
///
/// See [`FontCollection::can_render_with_matching_face`] for why "can draw" includes
/// matching the weight and style.
fn cover_char(
    fonts: &mut FontCollection,
    system: &SystemFonts,
    seen: &mut HashSet<(char, FontWeight, FontStyle)>,
    c: char,
    weight: FontWeight,
    font_style: FontStyle,
    families: &[String],
) {
    if !seen.insert((c, weight, font_style)) {
        return;
    }
    if fonts.can_render_with_matching_face(families, weight, font_style, c) {
        return;
    }
    if let Some((family, font)) = system.load_covering(c, weight, font_style) {
        fonts.push_fallback_font_face(family, font);
    }
}

/// Add a font covering representative CJK characters up front, for cases such as streaming
/// mode where the document's characters cannot be collected in advance.
///
/// [`crate::pdf::StreamingPdfWriter`] fixes the font count at `new`, so it cannot top up
/// with [`load_fonts_for_uncovered_chars`] as it reads. Instead we cover CJK up front,
/// which the default (latin) font would certainly render as tofu. Scripts other than CJK
/// are still uncovered, so the caller pairs this with a warning for any character that
/// remains undrawable.
///
/// Two representative characters are tried so that both kana and kanji are checked.
/// Normally the first font has both, so the second character is already covered and nothing
/// more is added.
pub fn ensure_cjk_fallback_font(fonts: &mut FontCollection, system: &SystemFonts) {
    const REPRESENTATIVE_CHARS: &[char] = &['漢', 'あ'];

    // Search as "family unset, Regular, Normal", the same as the default `ComputedStyle`.
    let mut seen = HashSet::new();
    for &c in REPRESENTATIVE_CHARS {
        cover_char(
            fonts,
            system,
            &mut seen,
            c,
            FontWeight::Normal,
            FontStyle::Normal,
            &[],
        );
    }
}

/// If any character in the document is left that no font can draw, warn once per character.
///
/// The last net against silently emitting tofu. `warned` is the set of characters already
/// warned about, carried by the caller so that repeated calls (as in streaming mode) do not
/// warn about the same character over and over.
pub fn warn_uncovered_chars(
    fonts: &FontCollection,
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    warned: &mut HashSet<char>,
) {
    for (c, style) in document_chars(dom, styles) {
        if fonts.can_render(&style.font_family, style.font_weight, style.font_style, c) {
            continue;
        }
        if !warned.insert(c) {
            continue;
        }
        eprintln!(
            "warning: no font can draw the character \"{c}\" (it will render as tofu).\n  \
             Specify a font with --font/--gothic-font or @font-face"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;
    use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet, Stylesheet};

    const FONTS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts");

    #[test]
    fn loads_a_font_by_family_name_case_insensitively() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let font = system
            .load("dejavu sans", FontWeight::Normal, FontStyle::Normal)
            .expect("should find DejaVu Sans regardless of case");
        assert!(font.has_glyph('A'));
    }

    #[test]
    fn returns_none_for_an_unknown_family() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        assert!(system
            .load(
                "Definitely Not A Real Font",
                FontWeight::Normal,
                FontStyle::Normal
            )
            .is_none());
    }

    #[test]
    fn loads_the_real_bold_face_when_the_family_has_one() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let font = system
            .load("DejaVu Sans", FontWeight::Bold, FontStyle::Normal)
            .expect("should find a DejaVu Sans face");
        assert!(
            font.weight() >= 600,
            "should resolve to the real bold face (DejaVuSans-Bold.ttf), not the regular one"
        );
    }

    #[test]
    fn load_missing_system_fonts_adds_fonts_used_by_the_document() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let dom = html::parse(br#"<p style="font-family: 'DejaVu Sans';">text</p>"#);
        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());

        let mut fonts = FontCollection::new(vec![]);
        load_missing_system_fonts(&mut fonts, &styles, &system);

        assert_eq!(fonts.len(), 1);
        assert!(fonts.has_family("DejaVu Sans"));
    }

    #[test]
    fn load_missing_system_fonts_adds_fonts_in_document_order() {
        // `styles` is a `HashMap`, so its iteration order changes from run to run. Without
        // fixing the order to document order, the order of the fonts added (and so the PDF
        // font numbers) would vary and the same HTML would emit different bytes (breaking
        // "same HTML, same output").
        //
        // A `HashMap`'s hash keys differ per instance, so a broken order may not show up in
        // a single run. Rebuild `styles` and repeat every time.

        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let html = br#"<p style="font-family: 'DejaVu Sans Mono';">a</p>
                       <p style="font-family: 'Noto Sans CJK JP';">b</p>
                       <p style="font-family: 'DejaVu Sans';">c</p>"#;

        for _ in 0..20 {
            let dom = html::parse(html);
            let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());

            let mut fonts = FontCollection::new(vec![]);
            load_missing_system_fonts(&mut fonts, &styles, &system);

            let families: Vec<String> = fonts
                .fonts()
                .iter()
                .map(|f| f.family_name().unwrap_or_default())
                .collect();
            assert_eq!(
                families,
                vec!["DejaVu Sans Mono", "Noto Sans CJK JP", "DejaVu Sans"],
                "fonts should be added in document order, not in HashMap order"
            );
        }
    }

    #[test]
    fn load_missing_system_fonts_skips_families_already_present() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let dom = html::parse(br#"<p style="font-family: 'DejaVu Sans';">text</p>"#);
        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());

        let mut fonts = FontCollection::new(vec![Font::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fonts/DejaVuSans.ttf"
        ))
        .unwrap()]);
        load_missing_system_fonts(&mut fonts, &styles, &system);

        assert_eq!(
            fonts.len(),
            1,
            "already-loaded family should not be duplicated"
        );
    }

    #[test]
    fn load_missing_system_fonts_still_searches_for_a_missing_weight_of_a_known_family() {
        // With only the Regular DejaVu Sans loaded via `--font`, the document also uses the
        // Bold (<b>) of the same family. The family already exists but the Bold face does
        // not, so only that weight should be looked up from the system.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let dom = html::parse(br#"<p style="font-family: 'DejaVu Sans';">a <b>b</b></p>"#);
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());

        let mut fonts = FontCollection::new(vec![Font::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fonts/DejaVuSans.ttf"
        ))
        .unwrap()]);
        load_missing_system_fonts(&mut fonts, &styles, &system);

        assert_eq!(
            fonts.len(),
            2,
            "the missing bold face should be added alongside the existing regular one"
        );
        assert!(
            fonts.has_matching_face("DejaVu Sans", FontWeight::Bold, FontStyle::Normal),
            "a real bold face should now be available for DejaVu Sans"
        );
    }

    #[test]
    fn explicit_sans_serif_resolves_to_a_system_gothic_face() {
        // Written explicitly, `sans-serif` resolves to a gothic face from the candidate list.
        // The fixture's "DejaVu Sans" is a candidate, so it is picked up. The default
        // `font-family` is separated out to empty (unset), so this resolution does not break
        // the default `--font` behaviour.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let author = parse_stylesheet("p { font-family: sans-serif; }");
        let dom = html::parse(b"<p>text</p>");
        let styles = compute_styles(&dom, &Stylesheet::default(), &author);

        let mut fonts = FontCollection::new(vec![]);
        load_missing_system_fonts(&mut fonts, &styles, &system);

        assert_eq!(fonts.len(), 1);
        assert!(
            fonts.has_family("sans-serif"),
            "the resolved gothic face must be registered under the generic name"
        );
    }

    #[test]
    fn an_element_without_an_explicit_font_family_does_not_trigger_a_lookup() {
        // The default `font-family` is empty (unset), so it falls back to the `--font` font,
        // and no system font lookup happens.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let dom = html::parse(b"<p>text</p>");
        let styles = compute_styles(&dom, &Stylesheet::default(), &Stylesheet::default());

        let mut fonts = FontCollection::new(vec![]);
        load_missing_system_fonts(&mut fonts, &styles, &system);

        assert!(
            fonts.is_empty(),
            "an unspecified font-family must not look up system fonts"
        );
    }

    #[test]
    fn load_generic_resolves_monospace_through_the_candidate_list() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let font = system
            .load_generic("monospace", FontWeight::Normal, FontStyle::Normal)
            .expect("DejaVu Sans Mono is in the candidate list and exists in the fixtures");
        assert_eq!(font.family_name().as_deref(), Some("DejaVu Sans Mono"));
    }

    #[test]
    fn load_generic_is_case_insensitive() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        assert!(system
            .load_generic("MONOSPACE", FontWeight::Normal, FontStyle::Normal)
            .is_some());
    }

    #[test]
    fn load_generic_returns_none_for_families_we_deliberately_skip() {
        // `cursive`/`fantasy` have no candidates. `Helvetica` is not a generic name.
        // (`sans-serif` is now resolved, so it is not included here.)
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        for generic in ["cursive", "fantasy", "Helvetica"] {
            assert!(
                system
                    .load_generic(generic, FontWeight::Normal, FontStyle::Normal)
                    .is_none(),
                "{generic} should not be resolved as a generic family"
            );
        }
    }

    #[test]
    fn load_generic_resolves_sans_serif_to_a_gothic_candidate() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let font = system
            .load_generic("sans-serif", FontWeight::Normal, FontStyle::Normal)
            .expect("DejaVu Sans is in the sans-serif candidate list and exists in the fixtures");
        assert_eq!(font.family_name().as_deref(), Some("DejaVu Sans"));
    }

    #[test]
    fn load_generic_returns_none_when_no_candidate_exists() {
        // The serif candidates ("DejaVu Serif" and friends) are not in the fixture. Unlike
        // monospace, there is no flag-based fallback search, so this returns None.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        assert!(system
            .load_generic("serif", FontWeight::Normal, FontStyle::Normal)
            .is_none());
    }

    #[test]
    fn load_any_monospaced_finds_a_monospaced_face_by_its_metadata_flag() {
        // The fallback path for when every candidate misses (fontdb's `FaceInfo::monospaced`
        // flag). The fixture's DejaVu Sans Mono carries the monospace flag, which lets us
        // exercise the path itself directly.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let font = system
            .load_any_monospaced(FontWeight::Normal, FontStyle::Normal)
            .expect("the fixture directory contains a monospaced face");
        assert_eq!(font.family_name().as_deref(), Some("DejaVu Sans Mono"));
    }

    #[test]
    fn load_missing_system_fonts_registers_monospace_under_the_generic_name() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let author = parse_stylesheet("pre { font-family: monospace; }");
        let dom = html::parse(b"<pre>text</pre>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);

        let mut fonts = FontCollection::new(vec![]);
        load_missing_system_fonts(&mut fonts, &styles, &system);

        assert_eq!(fonts.len(), 1);
        assert!(
            fonts.has_family("monospace"),
            "the resolved face must be registered under the generic name so that \
             `font-family: monospace` matches it during selection"
        );
    }

    #[test]
    fn load_covering_finds_a_cjk_face_from_the_candidate_list() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let (family, font) = system
            .load_covering('本', FontWeight::Normal, FontStyle::Normal)
            .expect("the fixture directory contains a CJK face");
        assert!(font.has_glyph('本'));
        assert!(
            family.contains("Noto Sans CJK"),
            "should come from the CJK candidate list, got {family}"
        );
    }

    #[test]
    fn load_covering_falls_back_to_a_full_scan_for_non_cjk_scripts() {
        // Cyrillic has no candidate list, so it can only be found through the full face
        // scan (`load_any_covering`).
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let (_, font) = system
            .load_covering('Д', FontWeight::Normal, FontStyle::Normal)
            .expect("the fixture fonts cover Cyrillic");
        assert!(font.has_glyph('Д'));
    }

    #[test]
    fn load_covering_gives_up_on_a_character_no_font_can_render() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        // No fixture font has a private-use-area character.
        assert!(system
            .load_covering('\u{E000}', FontWeight::Normal, FontStyle::Normal)
            .is_none());
    }

    #[test]
    fn load_fonts_for_uncovered_chars_ignores_text_that_is_not_rendered() {
        // Including characters that are never drawn would embed an unused CJK font because
        // of one Japanese character in a `<script>`, and would even change which font the
        // body text uses.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));

        let count_for = |body: &str| {
            let dom = html::parse(format!("<p>a</p>{body}").as_bytes());
            let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());
            let latin = system
                .load("DejaVu Sans", FontWeight::Normal, FontStyle::Normal)
                .expect("fixture");
            let mut fonts = FontCollection::new(vec![latin]);
            load_fonts_for_uncovered_chars(&mut fonts, &dom, &styles, &system);
            fonts.len()
        };

        let baseline = count_for("");
        for hidden in [
            r#"<script>var s = "領収書";</script>"#,
            "<style>/* 領収書 */</style>",
            "<title>領収書</title>",
            r#"<div style="display: none">領収書</div>"#,
            // With an element between `display: none` and the text, that element's own
            // `display` computes to `inline`. A per-node filter would miss it, so this
            // checks that walking the tree drops it.
            r#"<div style="display: none"><span>領収書</span></div>"#,
        ] {
            assert_eq!(
                count_for(hidden),
                baseline,
                "a font was added for Japanese that is never drawn: {hidden}"
            );
        }

        // Japanese that really is drawn is still topped up from coverage as before.
        assert_eq!(count_for("<p>領収書</p>"), baseline + 1);
    }

    #[test]
    fn load_fonts_for_uncovered_chars_adds_a_cjk_face_for_japanese_text() {
        // A Japanese document with no `font-family`. The family name is no clue at all, so
        // the CJK font has to be found from character coverage.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let dom = html::parse("<p>本文です。</p>".as_bytes());
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());

        let latin = system
            .load("DejaVu Sans", FontWeight::Normal, FontStyle::Normal)
            .expect("fixture");
        let mut fonts = FontCollection::new(vec![latin]);
        assert!(
            !fonts.can_render(&[], FontWeight::Normal, FontStyle::Normal, '本'),
            "precondition: a latin-only collection cannot render Japanese"
        );

        load_fonts_for_uncovered_chars(&mut fonts, &dom, &styles, &system);

        assert!(fonts.can_render(&[], FontWeight::Normal, FontStyle::Normal, '本'));
        assert_eq!(
            fonts.len(),
            2,
            "one CJK face should cover every character in the document"
        );
    }

    #[test]
    fn load_fonts_for_uncovered_chars_covers_generated_content_too() {
        // The string generated by `::before` is drawn too, so it is covered.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let author = parse_stylesheet(r#"p::before { content: "第"; }"#);
        let dom = html::parse(b"<p>text</p>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);

        let latin = system
            .load("DejaVu Sans", FontWeight::Normal, FontStyle::Normal)
            .expect("fixture");
        let mut fonts = FontCollection::new(vec![latin]);
        load_fonts_for_uncovered_chars(&mut fonts, &dom, &styles, &system);

        assert!(fonts.can_render(&[], FontWeight::Normal, FontStyle::Normal, '第'));
    }

    #[test]
    fn load_fonts_for_uncovered_chars_adds_nothing_when_everything_is_covered() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let dom = html::parse(b"<p>plain latin text</p>");
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &Stylesheet::default());

        let latin = system
            .load("DejaVu Sans", FontWeight::Normal, FontStyle::Normal)
            .expect("fixture");
        let mut fonts = FontCollection::new(vec![latin]);
        load_fonts_for_uncovered_chars(&mut fonts, &dom, &styles, &system);

        assert_eq!(fonts.len(), 1, "no font should be added");
    }

    #[test]
    fn ensure_cjk_fallback_font_adds_a_single_face_covering_kana_and_kanji() {
        // The up-front pass for streaming. Two representative characters are tried, but one
        // CJK font has both, so only one is added.
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let latin = system
            .load("DejaVu Sans", FontWeight::Normal, FontStyle::Normal)
            .expect("fixture");
        let mut fonts = FontCollection::new(vec![latin]);

        ensure_cjk_fallback_font(&mut fonts, &system);

        assert_eq!(fonts.len(), 2);
        assert!(fonts.can_render(&[], FontWeight::Normal, FontStyle::Normal, '漢'));
        assert!(fonts.can_render(&[], FontWeight::Normal, FontStyle::Normal, 'あ'));
    }

    #[test]
    fn ensure_cjk_fallback_font_is_a_noop_when_cjk_is_already_covered() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let cjk = system
            .load("Noto Sans CJK JP", FontWeight::Normal, FontStyle::Normal)
            .expect("fixture");
        let mut fonts = FontCollection::new(vec![cjk]);

        ensure_cjk_fallback_font(&mut fonts, &system);

        assert_eq!(fonts.len(), 1);
    }

    /// Regression test for colour emoji fonts.
    ///
    /// A font with a `cmap` but no outlines must be kept out of the automatic search by
    /// character coverage. Letting one through would adopt a font that can draw nothing as
    /// "the font that can draw this character", silently producing invisible text and a huge
    /// PDF (which really happened with Noto Color Emoji).
    #[test]
    fn a_colour_font_is_not_picked_up_by_the_coverage_search() {
        // Build a directory holding only the colour font, so it is the only search candidate
        // (passing `FONTS_DIR` directly would mix in fonts that have outlines).
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-fonts-colour-only-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(
            std::path::Path::new(FONTS_DIR).join("NotoColorEmoji.ttf"),
            dir.join("NotoColorEmoji.ttf"),
        )
        .unwrap();

        let system = SystemFonts::from_dir(&dir);
        assert!(
            system
                .load_covering('\u{1F389}', FontWeight::Normal, FontStyle::Normal)
                .is_none(),
            "a font without outlines must not be judged able to draw the emoji"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
