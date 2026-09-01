//! Classification of whitespace characters.
//!
//! Unicode has many characters for which `char::is_whitespace` (the White_Space property)
//! is true, but CSS line layout must not treat them all alike. There are two axes.
//!
//! 1. **Whether it collapses** - CSS Text 3 section 4.1 makes only space (U+0020), tab
//!    (U+0009) and the segment break (U+000A) collapsible; every other Zs (`&nbsp;`, thin
//!    space and so on) is an ordinary, non-collapsing character.
//!    Blink's `Character::IsCollapsibleSpace` (space/LF/tab/CR) and Gecko's
//!    `nsTextFrameUtils::IsSpaceOrTab` (space/tab, with newlines handled separately) cover
//!    the same range. A non-collapsing character is not a word separator for line layout;
//!    it is passed to shaping and drawn at the font's own advance (three `&nbsp;` are three spaces wide).
//!
//! 2. **Whether a break is allowed there** - decided by the UAX #14 line breaking class.
//!    `&nbsp;` (GL) and figure space (GL) forbid a break either side, while thin space and
//!    friends (BA) and ZWSP (ZW) allow one immediately after.
//!
//! Advances survive even in a font without those glyphs, because the shaper (harfrust)
//! implements HarfBuzz's space fallback (`_hb_ot_shape_fallback_spaces`): with no matching
//! glyph it substitutes the space glyph and sets the prescribed advance, such as `em/2` or
//! `em/5`. There is nothing for us to supply.
//!
//! A known simplification: U+000B, U+0085, U+2028 and U+2029 are really forced line breaks,
//! but to avoid .notdef in a font lacking those glyphs they are treated here as collapsible
//! whitespace (that is, word separators), preserving the existing behaviour.

/// A zero-width break opportunity (the ZW class of UAX #14). Also what `<wbr>` amounts to.
pub(crate) const ZERO_WIDTH_SPACE: char = '\u{200b}';

/// Whether the character is collapsible whitespace (CSS Text 3's "collapsible white space").
///
/// Under `white-space: normal` a run of these becomes one inter-word space, and is dropped
/// at the start and end of a line. A whitespace character not listed here (`&nbsp;`, say) is an ordinary character.
pub(crate) fn is_collapsible(ch: char) -> bool {
    matches!(
        ch,
        '\u{20}'      // SPACE
        | '\u{9}'     // TAB
        | '\u{a}'     // LF (HTML's segment break)
        | '\u{d}'     // CR (normalised to LF by the parser, but handled defensively)
        | '\u{c}'     // FF (HTML ASCII whitespace)
        | '\u{b}'     // VT
        | '\u{85}'    // NEL
        | '\u{2028}'  // LINE SEPARATOR
        | '\u{2029}' // PARAGRAPH SEPARATOR
    )
}

/// Whether a string consists only of collapsible whitespace (that is, whether no box needs
/// to be created for it). Unlike `str::trim`, a string of only `&nbsp;` is not "whitespace
/// only" (it has content).
pub(crate) fn is_collapsible_only(text: &str) -> bool {
    text.chars().all(is_collapsible)
}

/// Whether a break is forbidden either side of the whitespace (the GL and WJ classes of UAX #14).
///
/// `&nbsp;` exists precisely for this, so it takes priority over `word-break: break-all`
/// (browsers do not break "10&nbsp;kg" either).
pub(crate) fn is_non_breaking(ch: char) -> bool {
    matches!(
        ch,
        '\u{a0}'      // NO-BREAK SPACE (GL)
        | '\u{2007}'  // FIGURE SPACE (GL, for aligning digits)
        | '\u{202f}'  // NARROW NO-BREAK SPACE (GL)
        | '\u{2060}'  // WORD JOINER (WJ)
        | '\u{feff}' // ZERO WIDTH NO-BREAK SPACE (WJ)
    )
}

/// Whether a break is allowed immediately after the whitespace (the BA class of UAX #14).
///
/// It covers the typographic spaces that have a width (thin space and so on). ZWSP (ZW) does
/// not appear here: it is folded into a break-opportunity flag rather than emitting a glyph,
/// so `inline::flatten_spans` removes it at the character stage.
/// U+3000 IDEOGRAPHIC SPACE is not listed either, since `inline::is_cjk` already treats it
/// as a break opportunity (and it should be subject to `word-break: keep-all`).
pub(crate) fn allows_break_after(ch: char) -> bool {
    matches!(
        ch,
        '\u{1680}'          // OGHAM SPACE MARK (BA)
        | '\u{2000}'..='\u{2006}' // EN QUAD to SIX-PER-EM SPACE (BA)
        | '\u{2008}'..='\u{200a}' // PUNCTUATION/THIN/HAIR SPACE (BA)
        | '\u{205f}' // MEDIUM MATHEMATICAL SPACE (BA)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_css_collapsible_set_collapses() {
        for ch in [' ', '\t', '\n', '\r'] {
            assert!(is_collapsible(ch), "{ch:?} should collapse");
        }
        // True for `char::is_whitespace`, but an ordinary character as far as CSS is concerned.
        for ch in ['\u{a0}', '\u{2009}', '\u{3000}', '\u{202f}', '\u{2007}'] {
            assert!(ch.is_whitespace(), "{ch:?} is Unicode white space");
            assert!(!is_collapsible(ch), "{ch:?} must not collapse");
        }
    }

    #[test]
    fn a_nbsp_only_string_is_not_whitespace_only() {
        assert!(is_collapsible_only(" \n\t "));
        assert!(is_collapsible_only(""));
        assert!(!is_collapsible_only("\u{a0}"));
        assert!(!is_collapsible_only(" \u{2009} "));
    }

    #[test]
    fn glue_and_break_after_classes_do_not_overlap() {
        for ch in ['\u{a0}', '\u{2007}', '\u{202f}', '\u{2060}', '\u{feff}'] {
            assert!(is_non_breaking(ch));
            assert!(!allows_break_after(ch));
        }
        for ch in ['\u{2002}', '\u{2009}', '\u{200a}', '\u{205f}'] {
            assert!(allows_break_after(ch));
            assert!(!is_non_breaking(ch));
        }
        // ZWSP does not survive as a character (`inline::flatten_spans` removes it), so it
        // appears in neither table.
        assert!(!allows_break_after(ZERO_WIDTH_SPACE));
        assert!(!is_non_breaking(ZERO_WIDTH_SPACE));
    }
}
