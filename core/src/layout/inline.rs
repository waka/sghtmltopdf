//! The Inline Formatting Context: line breaking of text by a simple greedy algorithm, and line box placement.

use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use crate::fonts::{measure_text, shape_text, Font, FontCollection, ShapedGlyph};
use crate::html::NodeId;
use crate::style::{
    BoxSizing, ComputedStyle, ComputedTextShadow, EmphasisPosition, EmphasisStyle, FontStyle,
    FontWeight, Hyphens, LengthPercentage, LengthPercentageOrAuto, LineHeight, OverflowWrap,
    RgbaColor, TextAlign, TextOverflow, TextTransform, VerticalAlign, WhiteSpace, WordBreak,
};

use super::block::{LaidOutBox, PosCtx};
use super::box_tree::{BoxContent, InlineSpan, LayoutBox};
use super::float_ctx::FloatContext;
use super::geometry::Rect;
use super::white_space;

/// The placeholder character for `display: inline-block` (U+FFFC OBJECT REPLACEMENT
/// CHARACTER). It is never actually drawn; line layout uses it only to hold the box's place.
const ATOMIC_PLACEHOLDER: char = '\u{FFFC}';

/// The drawing information for one `text-emphasis` mark. A mark's size is half the
/// `font-size` (the value the spec recommends).
#[derive(Debug, Clone, PartialEq)]
pub struct EmphasisMark {
    pub style: EmphasisStyle,
    pub color: RgbaColor,
    pub position: EmphasisPosition,
    /// The mark's bounding size (px). `font_size * 0.5`.
    pub size: f32,
}

/// The height a `text-emphasis` mark demands of the line, as a ratio of the font-size
/// (the spec's recommended `0.5em`).
const EMPHASIS_SIZE_RATIO: f32 = 0.5;

/// A soft hyphen (U+00AD). It is never drawn and counts only as a break opportunity.
const SOFT_HYPHEN: char = '\u{00AD}';

/// The hyphen shown at the end of a line (when broken at a soft hyphen).
const HYPHEN: &str = "-";

/// The ellipsis of `text-overflow: ellipsis` (U+2026). In a font without that glyph it
/// falls back to a hyphen.
const ELLIPSIS: &str = "…";

/// A run of consecutive text in one style and one font (part of a word, or a whole word).
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// The index within [`FontCollection`] of the font used to draw this run.
    pub font_index: usize,
    pub font_size: f32,
    pub color: RgbaColor,
    /// The href of the `<a href>` enclosing this run. The PDF layer creates one `/Link`
    /// annotation per distinct value.
    pub link: Option<Rc<str>>,
    /// This run's `background-color`. The drawing layer paints an inline element's
    /// background as the run's rectangle, from ascent to descent (`<mark>` and so on).
    pub background_color: RgbaColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub line_through: bool,
    /// The original text of this run (kept so a character can be recovered from
    /// `ShapedGlyph::cluster`, which the `/ToUnicode` CMap in the PDF output needs).
    pub text: String,
    pub glyphs: Vec<ShapedGlyph>,
    /// The x coordinate relative to the left edge of the line box (`LineBox::rect`).
    pub x_offset: f32,
    pub width: f32,
    /// This run's computed line height (px). For `line-height: normal` it comes from this
    /// run's font metrics; a `<number>` is already multiplied by this run's `font_size`.
    pub line_height: f32,
    /// The resolved `letter-spacing` in px. The PDF drawing layer
    /// (`pdf::document::render_line`) passes it straight through as `Tc` (character spacing);
    /// it is already reflected in the width calculation during layout too.
    pub letter_spacing: f32,
    /// The resolved `word-spacing` in px. Used solely for the inter-word gap calculation
    /// (not for drawing: PDF's `Tw` has no effect on composite fonts, so it is realised only by adding to the gap).
    pub word_spacing: f32,
    /// The ascent (px, above the baseline) at this run's font and size.
    /// Used to compute the line box's height and baseline position.
    pub ascent: f32,
    /// The descent likewise (px, below the baseline, a positive value).
    pub descent: f32,
    /// The offset from the baseline from `vertical-align` (px, positive being upwards).
    /// The drawing layer adds it to `line.baseline` to find each run's baseline.
    pub baseline_shift: f32,
    /// The `text-shadow` (inherited and colour-resolved). The drawing layer paints it behind
    /// the text itself. It does not affect layout. Empty means no shadow.
    /// It is `Option`al because having none is the norm, so an empty one need not allocate
    /// an `Rc` (there are hundreds of thousands of runs in a document).
    pub text_shadow: Option<Rc<[ComputedTextShadow]>>,
    /// The `text-emphasis` marks. `None` means no mark. The height of the marks is already
    /// added to `ascent`/`descent`.
    pub emphasis: Option<EmphasisMark>,
    /// The position of this run's style within the IFC (an index into `span_styles`).
    /// Kept so a hyphen or ellipsis can be regenerated in the same style during line layout.
    pub(super) style_index: usize,
    /// Whether what precedes this run is a break opportunity from a soft hyphen. If the line
    /// breaks here, a hyphen is shown at the end of the previous line.
    pub(super) hyphen_before: bool,
    /// Whether what precedes this run is a break opportunity at all (a soft hyphen, a ZWSP,
    /// or a `<wbr>`). Whether to show a hyphen is held separately by `hyphen_before`.
    pub(super) break_before: bool,
    /// The computed `vertical-align` applied to this run (used only during layout, to resolve
    /// `top`/`bottom` after the line height is settled).
    pub(super) vertical_align: VerticalAlign,
}

#[derive(Debug, Clone)]
pub struct LineBox {
    pub rect: Rect,
    pub runs: Vec<TextRun>,
    /// The distance from the top of the line box to the baseline (px). With `vertical-align`
    /// involved the baseline differs per run, so it is settled and kept at layout time.
    pub baseline: f32,
    /// The `display: inline-block` boxes placed on this line.
    pub atomics: Vec<AtomicInline>,
}

/// An atomic inline box placed on a line (`display: inline-block`).
#[derive(Debug, Clone)]
pub struct AtomicInline {
    /// The laid-out contents. `layout::block` corrects the coordinates once the line's position is settled.
    pub content: LaidOutBox,
    /// The x offset from the left edge of the line box.
    pub x_offset: f32,
    /// The margin box's dimensions (used for line advance and wrapping decisions).
    pub margin_box_width: f32,
    pub margin_box_height: f32,
    /// The offset from the baseline from `vertical-align` (px, positive being up).
    pub baseline_shift: f32,
    /// This box's `vertical-align` (kept so `top`/`bottom` can be resolved after the line
    /// height is settled).
    pub(super) vertical_align: VerticalAlign,
}

/// One character plus a reference to the [`InlineSpan`] (that is, the computed style) it belongs to.
#[derive(Debug, Clone, Copy)]
struct StyledChar {
    ch: char,
    style_index: usize,
    /// For a `display: inline-block` placeholder, the index of its `InlineSpan`.
    /// `ch` is U+FFFC (OBJECT REPLACEMENT CHARACTER).
    atomic_span: Option<usize>,
    /// Whether this is a forced break character from a `<br>`. `ch` is `'\n'`. The
    /// `white-space: pre` path ignores this flag and breaks on `'\n'` alone, so a `<br>`
    /// inside a `<pre>` becomes a break naturally too.
    is_forced_break: bool,
    /// Whether a soft hyphen (U+00AD) preceded this character. The soft hyphen itself is
    /// never drawn, so it is removed from the string and converted into this flag as a break
    /// opportunity. If the line breaks here, a hyphen is shown at the end of the line.
    hyphen_before: bool,
    /// Whether a break opportunity precedes this character at all (a soft hyphen, a ZWSP or a `<wbr>`).
    ///
    /// It is separate from `hyphen_before` because whether a hyphen is shown differs.
    /// A ZWSP is nothing but a "zero-width break opportunity", so it is folded into this
    /// flag without emitting a single glyph. Kept as a character, a font lacking a ZWSP glyph
    /// would substitute the space glyph, which then collides with an ordinary space in
    /// `/ToUnicode` and breaks PDF text extraction (a space would come out as U+200B).
    break_before: bool,
}

/// The input unit of line layout in normal flow (`white-space: normal`/`nowrap`).
enum InlineItem<'a> {
    Word {
        chars: &'a [StyledChar],
        /// Whether whitespace preceded it (deciding whether to insert an inter-word space).
        space_before: bool,
    },
    /// A `display: inline-block` box.
    /// `span_index` is an index into `InlineSpan`.
    Atomic {
        span_index: usize,
        space_before: bool,
    },
    /// A forced break from a `<br>`. `style_index` is the `<br>` element's own style
    /// (used to compute the empty line's height).
    ForcedBreak { style_index: usize },
}

/// Break `spans` (the list of per-text-node runs) into lines fitting `available_width` and
/// return the line boxes stacked vertically from `(origin_x, origin_y)`. Where the style
/// (`<b>` and so on) or the font (CSS `font-family` fallback) changes mid-word, that word is
/// shaped as several [`TextRun`]s.
///
/// When `float_ctx` is `Some`, the float-occupied band at each line's Y position is queried
/// as the line starts, narrowing `available_width`/`origin_x` dynamically (text flowing
/// around floats). With `None` (no floats, or an unrelated call such as pre-measuring table
/// column widths) the fixed `available_width`/`origin_x` are used, as before.
///
/// `container_style` is the computed style of the block container establishing this IFC
/// (`None` for an anonymous box or a measuring pass). `text-align` is a property that
/// applies to the block container, so it is read from here rather than from the values on
/// the inline boxes in the line.
#[allow(clippy::too_many_arguments)]
pub(crate) fn layout_inline_content(
    spans: &[InlineSpan],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    available_width: f32,
    origin_x: f32,
    origin_y: f32,
    float_ctx: Option<&FloatContext>,
    container_style: Option<&ComputedStyle>,
    pos: &mut PosCtx,
) -> Vec<LineBox> {
    if fonts.is_empty() || spans.is_empty() {
        return Vec::new();
    }

    let (chars, span_styles, span_links) = flatten_spans(spans, styles);
    // `text-align`/`text-indent`/`white-space` are represented by the computed values of the
    // first span in the IFC (a design that works around the missing box_style on an anonymous
    // box). `display: inline-block` spans are excluded, though: such a box has its own IFC
    // inside, so its `white-space` and the like must not govern the parent's line layout
    // (the UA stylesheet's `white-space: pre` on `input` would otherwise make a whole
    // paragraph pre). In an IFC holding nothing but boxes (`<p><input></p>`, say) there is no
    // text-derived representative, so the initial values
    // (`white-space: normal`/`text-indent: 0`) are used
    let representative = spans
        .iter()
        .position(|span| span.atomic.is_none())
        .and_then(|i| span_styles.get(i));
    let white_space = representative.map(|s| s.white_space).unwrap_or_default();
    // `text-align` is a property that applies to the block container, so the container's
    // computed value wins over the inline representative. That both moves the logo in
    // `<div style="text-align: right"><img></div>` to the right (issue #19) and fixes the
    // `<div style="text-align: right"><span style="text-align: left">WORD</span></div>`
    // where the first span's value would win. Only when there is no container
    // (an anonymous box or a measuring pass) does it fall back to the representative.
    let text_align = container_style
        .map(|s| s.text_align)
        .or_else(|| representative.map(|s| s.text_align))
        .unwrap_or_default();
    // `word-break`/`overflow-wrap` are also handled by the IFC representative, like `white-space`.
    let word_break = representative.map(|s| s.word_break).unwrap_or_default();
    let overflow_wrap = representative.map(|s| s.overflow_wrap).unwrap_or_default();
    // A percentage resolves against this IFC's containing width (`available_width`), the same
    // "the used value is resolved by the consumer" pattern as `width`/`margin`.
    let text_indent = representative
        .map(|s| resolve_length_percentage(s.text_indent, available_width))
        .unwrap_or(0.0);
    if white_space == WhiteSpace::Pre {
        // The `white-space: pre` path cannot handle `display: inline-block` boxes (a known
        // limitation). The placeholder character is removed here so it is not drawn as a glyph.
        //
        let chars: Vec<StyledChar> = chars
            .into_iter()
            .filter(|sc| sc.atomic_span.is_none())
            .collect();
        return layout_pre_content(
            &chars,
            &span_styles,
            &span_links,
            fonts,
            available_width,
            origin_x,
            origin_y,
            float_ctx,
        );
    }

    let items = split_into_items(&chars);
    if items.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let mut current_runs: Vec<TextRun> = Vec::new();
    let mut current_atomics: Vec<AtomicInline> = Vec::new();
    let mut current_width = 0.0f32;
    let mut cursor_y = origin_y;
    let mut line_left = origin_x;
    let mut line_available_width = available_width;
    // The positions of the word boundaries in the line currently being built (an x from the
    // left edge of the line box: the `current_width` before the inter-word space is added).
    // `text-align: justify` distributes its extra space at these points (a word at the start
    // of a line is not recorded as a boundary, being the line's left edge already). Text runs
    // and boxes such as `<img>` accumulate separately into `current_runs`/`current_atomics`,
    // so these are held as x coordinates rather than indices, to apply the same rule to both.
    let mut word_boundaries: Vec<f32> = Vec::new();
    // The line height demanded by a `<br>` when the previous item was a forced break.
    // Used to add one empty line for a trailing `<br>`.
    let mut trailing_break_height: Option<f32> = None;

    for item in items {
        let (word, word_space_before) = match item {
            InlineItem::Word {
                chars,
                space_before,
            } => {
                trailing_break_height = None;
                (chars, space_before)
            }
            InlineItem::Atomic {
                span_index,
                space_before,
            } => {
                trailing_break_height = None;
                let Some(atomic) = spans.get(span_index).and_then(|s| s.atomic.as_deref()) else {
                    continue;
                };
                let style = span_styles.get(span_index).cloned().unwrap_or_default();

                // For an empty line, query the band first (the same procedure as an ordinary word).
                if current_runs.is_empty() && current_atomics.is_empty() {
                    (line_left, line_available_width) =
                        line_band(float_ctx, cursor_y, 0.0, origin_x, available_width);
                    if lines.is_empty() {
                        line_left += text_indent;
                        line_available_width -= text_indent;
                    }
                }

                let laid = layout_atomic_inline(atomic, styles, fonts, line_available_width, pos);
                let margin_box_width = margin_box_width_of(&laid);
                let margin_box_height = laid.layout.margin_box_height();

                let gap_width = if space_before {
                    current_runs
                        .last()
                        .map(|last| {
                            measure_space_width(fonts, last.font_index, last.font_size)
                                + last.word_spacing
                        })
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                let line_is_empty = current_runs.is_empty() && current_atomics.is_empty();

                // If it does not fit on the line, break first (the box itself is never split).
                if !line_is_empty
                    && white_space != WhiteSpace::Nowrap
                    && current_width + gap_width + margin_box_width > line_available_width
                {
                    let line_height = line_height_for(&current_runs);
                    lines.push(finish_line(
                        std::mem::take(&mut current_runs),
                        std::mem::take(&mut current_atomics),
                        current_width,
                        line_left,
                        cursor_y,
                        line_height,
                        fonts,
                    ));
                    apply_text_align(
                        lines.last_mut().expect("just pushed"),
                        text_align,
                        false,
                        line_available_width,
                        &word_boundaries,
                    );
                    word_boundaries.clear();
                    cursor_y += lines.last().expect("just pushed").rect.height;
                    current_width = 0.0;
                    (line_left, line_available_width) = line_band(
                        float_ctx,
                        cursor_y,
                        margin_box_height,
                        origin_x,
                        available_width,
                    );
                } else if !line_is_empty {
                    // The space before the box also stretches under `justify` (like a text run).
                    if space_before {
                        word_boundaries.push(current_width);
                    }
                    current_width += gap_width;
                }

                current_atomics.push(AtomicInline {
                    content: laid,
                    x_offset: current_width,
                    margin_box_width,
                    margin_box_height,
                    baseline_shift: 0.0,
                    vertical_align: style.vertical_align,
                });
                current_width += margin_box_width;
                continue;
            }
            InlineItem::ForcedBreak { style_index } => {
                // A forced break settles the line regardless of the width remaining
                // (it applies even under `white-space: nowrap`).
                let break_height = span_styles
                    .get(style_index)
                    .map(|style| empty_line_height(style, fonts))
                    .unwrap_or(0.0);
                if current_runs.is_empty() && current_atomics.is_empty() {
                    // A forced break with nothing on the line (consecutive `<br>`s, or a
                    // `<br>` at the start of a paragraph) becomes an empty line with only a height.
                    (line_left, line_available_width) =
                        line_band(float_ctx, cursor_y, break_height, origin_x, available_width);
                    lines.push(finish_line(
                        Vec::new(),
                        Vec::new(),
                        0.0,
                        line_left,
                        cursor_y,
                        break_height,
                        fonts,
                    ));
                    cursor_y += break_height;
                } else {
                    let line_height = line_height_for(&current_runs);
                    lines.push(finish_line(
                        std::mem::take(&mut current_runs),
                        std::mem::take(&mut current_atomics),
                        current_width,
                        line_left,
                        cursor_y,
                        line_height,
                        fonts,
                    ));
                    // A line ending in a forced break is treated like the last line and is not
                    // stretched by `justify`.
                    apply_text_align(
                        lines.last_mut().expect("just pushed"),
                        text_align,
                        true,
                        line_available_width,
                        &word_boundaries,
                    );
                    word_boundaries.clear();
                    cursor_y += line_height;
                    current_width = 0.0;
                }
                // `<br clear="left|right|all">` (the legacy presentational attribute having
                // been converted into the `clear` property). Writing `br { clear: both }` in
                // CSS takes the same path.
                if let (Some(ctx), Some(clear)) =
                    (float_ctx, span_styles.get(style_index).map(|s| s.clear))
                {
                    cursor_y = ctx.clearance(clear, cursor_y);
                }
                trailing_break_height = Some(break_height);
                continue;
            }
        };
        let word_runs = split_word_into_runs(word, &span_styles, &span_links, fonts, word_break);

        // Even within a word, group into chunks ("the smallest unit judged as a whole for
        // fitting on a line") at every breakable boundary involving a CJK character. A word
        // separation by whitespace is always breakable (handled by `is_first_chunk_of_word`
        // in the next stage). Each element is `(chunk, is it the word's first chunk, may
        // overflow-wrap try to split it by character)`. The third guards against an infinite
        // loop when a chunk that "gave up splitting because not even one character fits" is fed back in.
        let mut chunk_queue: VecDeque<(Vec<TextRun>, bool, bool)> =
            group_into_chunks(word_runs, word_break)
                .into_iter()
                .enumerate()
                .map(|(chunk_index, chunk)| (chunk, chunk_index == 0, true))
                .collect();

        while let Some((chunk, is_first_chunk_of_word, allow_break_fallback)) =
            chunk_queue.pop_front()
        {
            let chunk_width: f32 = chunk.iter().map(|r| r.width).sum();
            let starting_new_line = current_runs.is_empty() && current_atomics.is_empty();

            if starting_new_line {
                // The start of a new line: query the band for the float, using the line height
                // of this chunk's first run (the same computation as `line_height_for`)
                // (a known simplification: with wildly mixed font sizes on one line the band
                // decision can be slightly inaccurate, but that is rare in business documents).
                let hint = line_height_hint_for_chunk(&chunk);
                (line_left, line_available_width) =
                    line_band(float_ctx, cursor_y, hint, origin_x, available_width);
                // `text-indent` applies only to the first physical line (CSS2.1 section 16.1).
                if lines.is_empty() {
                    line_left += text_indent;
                    line_available_width -= text_indent;
                }
            }

            // Only a word's first chunk gets an inter-word space before it. Later chunks
            // split at a CJK boundary within the word continue directly, with no gap.
            let gap_width = if is_first_chunk_of_word && word_space_before {
                current_runs
                    .last()
                    .map(|last| {
                        measure_space_width(fonts, last.font_index, last.font_size)
                            + last.word_spacing
                    })
                    .unwrap_or(0.0)
            } else {
                0.0
            };

            // `overflow-wrap: break-word`: a chunk that still does not fit even at the
            // start of a line is cut character by character to whatever fits and then
            // broken. When not even one character fits (an extremely narrow band) it is
            // placed as-is and allowed to overflow, to avoid an infinite loop.
            if starting_new_line
                && allow_break_fallback
                && overflow_wrap == OverflowWrap::BreakWord
                && white_space != WhiteSpace::Nowrap
                && chunk_width > line_available_width
            {
                let (head, rest) = split_chunk_to_fit(chunk, line_available_width);
                if !head.is_empty() && !rest.is_empty() {
                    // The first half is processed as a chunk that "will not be split further",
                    // and the rest goes back through the splitting decision on the next line.
                    chunk_queue.push_front((rest, false, true));
                    chunk_queue.push_front((head, is_first_chunk_of_word, false));
                } else {
                    // Not even one character fits (an extremely narrow band). Give up splitting and place it as-is.
                    let restored: Vec<TextRun> = head.into_iter().chain(rest).collect();
                    chunk_queue.push_front((restored, is_first_chunk_of_word, false));
                }
                continue;
            }

            if !starting_new_line
                && white_space != WhiteSpace::Nowrap
                && current_width + gap_width + chunk_width > line_available_width
            {
                // When breaking at a soft hyphen, show a hyphen at the end of the line being settled.
                //
                if chunk.first().is_some_and(|run| run.hyphen_before) {
                    push_hyphen(&mut current_runs, &mut current_width, &span_styles, fonts);
                }
                let line_height = line_height_for(&current_runs);
                lines.push(finish_line(
                    std::mem::take(&mut current_runs),
                    std::mem::take(&mut current_atomics),
                    current_width,
                    line_left,
                    cursor_y,
                    line_height,
                    fonts,
                ));
                // Apply text-align to the line settled by the wrap. It is not the last line,
                // so it stretches under `justify` too (per the CSS spec, the last line does not).
                apply_text_align(
                    lines.last_mut().expect("just pushed"),
                    text_align,
                    false,
                    line_available_width,
                    &word_boundaries,
                );
                word_boundaries.clear();
                cursor_y += line_height;
                current_width = 0.0;

                let hint = line_height_hint_for_chunk(&chunk);
                (line_left, line_available_width) =
                    line_band(float_ctx, cursor_y, hint, origin_x, available_width);

                // We broke, so re-evaluate this chunk as the "placed at the start of a line"
                // case. Without that, `overflow-wrap: break-word`'s character splitting would
                // not work from the second line on (it would be placed without ever passing the start-of-line check).
                chunk_queue.push_front((chunk, is_first_chunk_of_word, allow_break_fallback));
                continue;
            } else if !starting_new_line {
                // Only positions where whitespace really is present stretch. Counting a word
                // boundary with no whitespace (`gap_width == 0`), as in `aaa<input>bbb`, would
                // open a gap only between the box and the word.
                if is_first_chunk_of_word && word_space_before {
                    word_boundaries.push(current_width);
                }
                current_width += gap_width;
            }

            for mut run in chunk {
                run.x_offset = current_width;
                current_width += run.width;
                current_runs.push(run);
            }
        }
    }

    // A line carrying no text at all, with only `display: inline-block` boxes on it, is also
    // settled as a line (looking only at `current_runs` would throw away a whole line such as `<p><input></p>`).

    if !current_runs.is_empty() || !current_atomics.is_empty() {
        let line_height = line_height_for(&current_runs);
        lines.push(finish_line(
            current_runs,
            current_atomics,
            current_width,
            line_left,
            cursor_y,
            line_height,
            fonts,
        ));
        // The last line does not stretch under `justify` (per the CSS spec).
        apply_text_align(
            lines.last_mut().expect("just pushed"),
            text_align,
            true,
            line_available_width,
            &word_boundaries,
        );
    } else if let Some(break_height) = trailing_break_height {
        // A trailing `<br>` leaves one empty line (the same behaviour as the major browsers).
        let (left, _) = line_band(float_ctx, cursor_y, break_height, origin_x, available_width);
        lines.push(finish_line(
            Vec::new(),
            Vec::new(),
            0.0,
            left,
            cursor_y,
            break_height,
            fonts,
        ));
    }

    // Merge runs of identical appearance only after `text-align` has been applied.
    for line in &mut lines {
        merge_adjacent_runs(line, fonts);
        // A `Vec` reserves room for at least 4 elements on its first push, so even a box with
        // one run per line holds space for four. There are hundreds of thousands of lines and
        // runs in a document, and that slack is around a fifth of layout's memory, so it is trimmed.
        line.runs.shrink_to_fit();
        line.atomics.shrink_to_fit();
    }
    lines.shrink_to_fit();

    lines
}

/// Merge horizontally adjacent runs of identical appearance into one.
///
/// Line layout creates a run per word, splitting one paragraph into around seven runs. Each
/// run costs a 192-byte struct plus allocations for its text and glyph list, so in a
/// document of tens of thousands of paragraphs that splitting dominates layout's memory.
///
/// Inter-word whitespace is not part of a run but expressed as a "gap", so merging restores
/// it by inserting a space glyph whose advance is the gap. That leaves the drawing positions
/// exactly as they were, and makes PDF text extraction separate the words with a space.
///
/// The gaps `text-align: justify` widened can be handled the same way (the measured gap
/// simply becomes the space's advance), so this must be called after `apply_text_align`.
fn merge_adjacent_runs(line: &mut LineBox, fonts: &FontCollection) {
    if line.runs.len() < 2 {
        return;
    }
    let mut merged: Vec<TextRun> = Vec::with_capacity(line.runs.len());
    for run in std::mem::take(&mut line.runs) {
        let Some(prev) = merged.last_mut() else {
            merged.push(run);
            continue;
        };
        match gap_if_mergeable(prev, &run, fonts) {
            Some(gap) => append_run(prev, run, gap, fonts),
            None => merged.push(run),
        }
    }
    line.runs = merged;
}

/// If `next` can be merged onto the end of `prev`, return the gap (px) between the two.
fn gap_if_mergeable(prev: &TextRun, next: &TextRun, fonts: &FontCollection) -> Option<f32> {
    // Any difference in appearance leaves them as separate runs.
    let same_style = prev.font_index == next.font_index
        && prev.font_size == next.font_size
        && prev.color == next.color
        && prev.background_color == next.background_color
        && prev.bold == next.bold
        && prev.italic == next.italic
        && prev.underline == next.underline
        && prev.line_through == next.line_through
        && prev.line_height == next.line_height
        && prev.word_spacing == next.word_spacing
        && prev.ascent == next.ascent
        && prev.descent == next.descent
        && prev.baseline_shift == next.baseline_shift
        && prev.vertical_align == next.vertical_align
        && prev.style_index == next.style_index
        && prev.link == next.link
        && prev.text_shadow == next.text_shadow;
    if !same_style {
        return None;
    }
    // `letter-spacing` applies to every glyph as PDF's `Tc`, so it would be added to the
    // inserted space too and shift the positions. Lines that set it are not merged.
    if prev.letter_spacing != 0.0 || next.letter_spacing != 0.0 {
        return None;
    }
    // A `text-emphasis` mark is struck per character, so adding a space could change the count.
    if prev.emphasis.is_some() || next.emphasis.is_some() {
        return None;
    }
    // Right after an end-of-line hyphen, merging would lose whether the hyphen was there.
    if next.hyphen_before {
        return None;
    }

    let gap = next.x_offset - (prev.x_offset + prev.width);
    // If they overlap (a negative gap), simply give up.
    if gap < -0.01 {
        return None;
    }
    let gap = gap.max(0.0);
    // Do not merge in a font that cannot fill the gap with a space glyph.
    if gap > 0.01 && space_glyph(prev.font_index, fonts).is_none() {
        return None;
    }
    Some(gap)
}

/// The glyph ID of the space (U+0020) in font `font_index`.
fn space_glyph(font_index: usize, fonts: &FontCollection) -> Option<u16> {
    fonts.get(font_index).and_then(|font| font.glyph_id(' '))
}

/// Append `next` onto the end of `prev`, filling a positive `gap` with a space glyph.
fn append_run(prev: &mut TextRun, next: TextRun, gap: f32, fonts: &FontCollection) {
    if gap > 0.01 {
        let glyph_id =
            space_glyph(prev.font_index, fonts).expect("its presence was just confirmed");
        prev.glyphs.push(ShapedGlyph {
            glyph_id,
            cluster: prev.text.len() as u32,
            x_advance: gap,
            x_offset: 0.0,
            y_offset: 0.0,
        });
        prev.text.push(' ');
    }
    // A cluster is a byte position from the start of its own run's text, so it is shifted to its position after concatenation.
    let base = prev.text.len() as u32;
    prev.glyphs.extend(next.glyphs.into_iter().map(|mut glyph| {
        glyph.cluster += base;
        glyph
    }));
    prev.text.push_str(&next.text);
    prev.width = next.x_offset + next.width - prev.x_offset;
}

/// Apply `text-align` to a settled line. `is_last_line` decides whether `justify` skips the
/// last line (per the CSS spec). `word_boundaries` are the positions of the word boundaries
/// in the line (an x from the left edge of the line box, before the inter-word space), where
/// `justify` distributes its extra space.
///
/// A line's contents are held separately as text runs (`line.runs`) and as `<img>`/
/// `display: inline-block` boxes (`line.atomics`), so every branch moves both.
fn apply_text_align(
    line: &mut LineBox,
    text_align: TextAlign,
    is_last_line: bool,
    line_available_width: f32,
    word_boundaries: &[f32],
) {
    let leftover = line_available_width - line.rect.width;
    match text_align {
        TextAlign::Left => {}
        TextAlign::Right => shift_all_runs(line, leftover),
        TextAlign::Center => shift_all_runs(line, leftover / 2.0),
        TextAlign::Justify if !is_last_line && !word_boundaries.is_empty() && leftover > 0.0 => {
            let extra = leftover / word_boundaries.len() as f32;
            // Both runs and boxes move right by the number of boundaries to their left
            // (a boundary sits just before each word's preceding space, so a word's first run
            // counts its own boundary).
            let shift_at =
                |x: f32| extra * word_boundaries.iter().filter(|&&b| b <= x).count() as f32;
            for run in &mut line.runs {
                run.x_offset += shift_at(run.x_offset);
            }
            for atomic in &mut line.atomics {
                atomic.x_offset += shift_at(atomic.x_offset);
            }
            line.rect.width = line_available_width;
        }
        TextAlign::Justify => {}
    }
}

/// Move a line's contents (text runs and atomic inline boxes) right by `shift` px together.
/// `<img>` and `display: inline-block` boxes are on `line.atomics` rather than `line.runs`,
/// so moving only the runs would leave the boxes stranded at the left edge
/// (issue #19).
fn shift_all_runs(line: &mut LineBox, shift: f32) {
    if shift <= 0.0 {
        return;
    }
    for run in &mut line.runs {
        run.x_offset += shift;
    }
    for atomic in &mut line.atomics {
        atomic.x_offset += shift;
    }
}

/// Approximate the line height by the computed `line_height` of `chunk`'s first run (the
/// runs of the whole line are not yet settled at the point the band is queried).
fn line_height_hint_for_chunk(chunk: &[TextRun]) -> f32 {
    chunk.first().map(|r| r.line_height).unwrap_or(0.0)
}

/// Query the band from `y` to `y+height` if there is a `float_ctx`, and otherwise return the
/// fixed `(origin_x, available_width)`.
fn line_band(
    float_ctx: Option<&FloatContext>,
    y: f32,
    height: f32,
    origin_x: f32,
    available_width: f32,
) -> (f32, f32) {
    match float_ctx {
        Some(ctx) => ctx.available_band(y, height, origin_x, origin_x + available_width),
        None => (origin_x, available_width),
    }
}

/// Expand `spans` character by character, tagging each character with the index of the
/// original [`ComputedStyle`] it belongs to. `span_styles` holds the actual styles paired
/// with the characters. `text-transform` is applied here (finished in one pass before word splitting).
fn flatten_spans(
    spans: &[InlineSpan],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
) -> (Vec<StyledChar>, Vec<ComputedStyle>, Vec<Option<Rc<str>>>) {
    let mut chars = Vec::new();
    let mut span_styles = Vec::with_capacity(spans.len());
    // The `<a href>`, looked up by the same index as the spans. It is not a CSS property, so
    // it is not on `ComputedStyle`.
    let mut span_links: Vec<Option<Rc<str>>> = Vec::with_capacity(spans.len());
    // Word-start tracking continues across spans (the very start counts as a word start).
    let mut prev_is_boundary = true;
    // Whether a soft hyphen preceded.
    //
    // A break opportunity carries over to "the next character", so it has to live outside the
    // span. A `<wbr>` is an element and therefore always its own span, so a variable scoped
    // inside a span would not reach the next one (the same for `<span>foo&shy;</span><span>bar</span>`).
    let mut hyphen_pending = false;
    // Whether a break opportunity (a soft hyphen or ZWSP) preceded.
    let mut break_pending = false;

    for span in spans {
        // The background colour and so on are swapped per span, so an owned style is made from the shared one.
        let mut style = styles
            .get(&span.node)
            .map(|shared| (**shared).clone())
            .unwrap_or_default();
        // The inline background uses the value the span carries (that is, what the nearest
        // inline element specified). A text node's computed style clones even the parent's
        // non-inherited properties, so using it directly would paint the block's background too
        // (see the comment in `box_tree::collect_spans_with_background`).
        style.background_color = span.background_color;
        if span.is_first_letter {
            apply_first_letter_style(&mut style);
        }
        let style_index = span_styles.len();
        let transform = style.text_transform;
        span_styles.push(style);
        span_links.push(span.link.clone());

        if span.atomic.is_some() {
            // `display: inline-block` carries no string, so a single placeholder is put in to hold its position.
            //
            chars.push(StyledChar {
                ch: ATOMIC_PLACEHOLDER,
                style_index,
                atomic_span: Some(style_index),
                is_forced_break: false,
                hyphen_before: hyphen_pending,
                break_before: break_pending,
            });
            hyphen_pending = false;
            break_pending = false;
            prev_is_boundary = false;
            continue;
        }

        let hyphens = span_styles[style_index].hyphens;
        for ch in span.text.chars() {
            // A soft hyphen (U+00AD) is not drawn itself. Under `hyphens: manual` (the initial
            // value) it is carried to the next character as a break opportunity; under `none` it is simply dropped.
            if ch == SOFT_HYPHEN {
                hyphen_pending = hyphens == Hyphens::Manual;
                break_pending |= hyphen_pending;
                continue;
            }
            // A ZWSP (what a `<wbr>` amounts to) is not drawn either. Being nothing but a
            // zero-width break opportunity, it is folded into a flag without emitting a glyph (see `StyledChar::break_before`).
            if ch == white_space::ZERO_WIDTH_SPACE {
                break_pending = true;
                continue;
            }
            let is_word_start = prev_is_boundary;
            let transformed = apply_text_transform(ch, transform, is_word_start);
            chars.push(StyledChar {
                ch: transformed,
                style_index,
                atomic_span: None,
                is_forced_break: span.is_forced_break,
                hyphen_before: hyphen_pending,
                break_before: break_pending,
            });
            hyphen_pending = false;
            break_pending = false;
            prev_is_boundary = ch.is_whitespace();
        }
    }

    (chars, span_styles, span_links)
}

/// Override only the corresponding properties from `style.first_letter_style`, if any.
fn apply_first_letter_style(style: &mut ComputedStyle) {
    let Some(first_letter) = style.first_letter_style.clone() else {
        return;
    };
    if let Some(v) = first_letter.font_size {
        style.font_size = v;
    }
    if let Some(v) = first_letter.font_family {
        style.font_family = v;
    }
    if let Some(v) = first_letter.font_weight {
        style.font_weight = v;
    }
    if let Some(v) = first_letter.font_style {
        style.font_style = v;
    }
    if let Some(v) = first_letter.color {
        style.color = v;
    }
    if let Some(v) = first_letter.text_decoration_line {
        style.text_decoration_line = v;
    }
    if let Some(v) = first_letter.text_transform {
        style.text_transform = v;
    }
}

/// Apply `text-transform` to one character. `uppercase`/`lowercase` take only the first
/// character of `char::to_uppercase()` and friends (multi-character expansions such as
/// German sharp s are not supported). `capitalize` converts only word-initial characters.
fn apply_text_transform(ch: char, transform: TextTransform, is_word_start: bool) -> char {
    match transform {
        TextTransform::None => ch,
        TextTransform::Uppercase => ch.to_uppercase().next().unwrap_or(ch),
        TextTransform::Lowercase => ch.to_lowercase().next().unwrap_or(ch),
        TextTransform::Capitalize if is_word_start && !ch.is_whitespace() => {
            ch.to_uppercase().next().unwrap_or(ch)
        }
        TextTransform::Capitalize => ch,
    }
}

/// Split into words at collapsible whitespace ([`white_space::is_collapsible`]), inserting
/// forced breaks from `<br>` as [`InlineItem::ForcedBreak`] in the order they appear.
/// Consecutive whitespace is collapsed and leading and trailing whitespace is ignored (a
/// forced break is whitespace but does not collapse and always survives as one item).
///
/// `&nbsp;` and thin space are not word separators and are passed on to
/// [`split_word_into_runs`] as part of the word (they do not collapse and are drawn at the
/// font's own advance; whether a break is allowed is decided by [`is_break_boundary`]).
fn split_into_items(chars: &[StyledChar]) -> Vec<InlineItem<'_>> {
    let mut items = Vec::new();
    let mut word_start = 0usize;
    // Whether whitespace preceded (deciding whether to insert an inter-word space).
    let mut space_pending = false;

    for (i, sc) in chars.iter().enumerate() {
        if let Some(span_index) = sc.atomic_span {
            if word_start < i {
                items.push(InlineItem::Word {
                    chars: &chars[word_start..i],
                    space_before: space_pending,
                });
                space_pending = false;
            }
            items.push(InlineItem::Atomic {
                span_index,
                space_before: space_pending,
            });
            space_pending = false;
            word_start = i + 1;
            continue;
        }
        if !white_space::is_collapsible(sc.ch) {
            continue;
        }
        if word_start < i {
            items.push(InlineItem::Word {
                chars: &chars[word_start..i],
                space_before: space_pending,
            });
        }
        space_pending = true;
        if sc.is_forced_break {
            items.push(InlineItem::ForcedBreak {
                style_index: sc.style_index,
            });
            space_pending = false;
        }
        word_start = i + 1;
    }
    if word_start < chars.len() {
        items.push(InlineItem::Word {
            chars: &chars[word_start..],
            space_before: space_pending,
        });
    }

    items
}

/// Split a word into [`TextRun`]s, one per run of consistent (style, font).
/// At a character boundary involving a CJK character ([`is_break_boundary`]) it splits into
/// separate runs even where the style and font are unchanged (to make it a breakable
/// boundary). That means shaping character by character, but there is normally no
/// context-dependent shaping between CJK characters, so the appearance is unaffected.
fn split_word_into_runs(
    word: &[StyledChar],
    span_styles: &[ComputedStyle],
    span_links: &[Option<Rc<str>>],
    fonts: &FontCollection,
    word_break: WordBreak,
) -> Vec<TextRun> {
    let mut runs = Vec::new();
    let mut current: Option<(usize, usize)> = None;
    let mut current_text = String::new();
    let mut last_char: Option<char> = None;
    // Whether what precedes the run being built is a break opportunity from a soft hyphen.
    let mut current_hyphen_before = false;
    // Likewise, whether what precedes is a break opportunity at all (a soft hyphen or ZWSP).
    let mut current_break_before = false;

    let flush = |runs: &mut Vec<TextRun>,
                 current: Option<(usize, usize)>,
                 text: &str,
                 hyphen_before: bool,
                 break_before: bool| {
        if let Some((style_index, fi)) = current {
            let mut run = shape_run(text, fi, fonts, &span_styles[style_index]);
            run.link = span_links.get(style_index).cloned().flatten();
            run.style_index = style_index;
            run.hyphen_before = hyphen_before;
            run.break_before = break_before;
            runs.push(run);
        }
    };

    for sc in word {
        let style = &span_styles[sc.style_index];
        let font_index = fonts
            .select_for_char(
                &style.font_family,
                style.font_weight,
                style.font_style,
                sc.ch,
            )
            .unwrap_or(0);

        let continues_current = match (current, last_char) {
            (Some((style_index, fi)), Some(prev_ch)) => {
                style_index == sc.style_index
                    && fi == font_index
                    && !sc.break_before
                    && !is_break_boundary(prev_ch, sc.ch, word_break)
            }
            _ => false,
        };

        if continues_current {
            current_text.push(sc.ch);
        } else {
            flush(
                &mut runs,
                current,
                &current_text,
                current_hyphen_before,
                current_break_before,
            );
            current_text = sc.ch.to_string();
            current = Some((sc.style_index, font_index));
            current_hyphen_before = sc.hyphen_before;
            current_break_before = sc.break_before;
        }
        last_char = Some(sc.ch);
    }
    flush(
        &mut runs,
        current,
        &current_text,
        current_hyphen_before,
        current_break_before,
    );

    runs
}

/// Group `runs` into unbreakable chunks at each breakable boundary (the start, or a run
/// boundary involving a CJK character, [`is_break_boundary`]).
/// Every internal boundary of a chunk is unbreakable (a style or font change only), so the
/// caller can decide "does it fit on a line as a whole" per chunk.
fn group_into_chunks(runs: Vec<TextRun>, word_break: WordBreak) -> Vec<Vec<TextRun>> {
    let mut chunks: Vec<Vec<TextRun>> = Vec::new();
    for run in runs {
        let starts_new_chunk = match chunks.last().and_then(|chunk| chunk.last()) {
            None => true,
            // A soft hyphen or a ZWSP (`<wbr>`) is a break opportunity too.
            Some(_) if run.break_before => true,
            Some(prev) => is_break_boundary(
                prev.text.chars().last().unwrap_or(' '),
                run.text.chars().next().unwrap_or(' '),
                word_break,
            ),
        };
        if starts_new_chunk {
            chunks.push(vec![run]);
        } else {
            chunks.last_mut().expect("just checked non-empty").push(run);
        }
    }
    chunks
}

/// `text-overflow: ellipsis`. After line layout, trim any line overflowing `content_width`
/// with an ellipsis. It does nothing when `overflow` is `visible`, or when `text-overflow`
/// is `clip` (`clip` is left to the existing `overflow` clipping).
///
/// A known simplification: only lines overflowing horizontally are covered (overflow of the
/// whole block is not handled). In a font without an ellipsis glyph it falls back to a
/// hyphen.
pub(super) fn apply_text_overflow(
    lines: &mut [LineBox],
    style: &ComputedStyle,
    content_width: f32,
    fonts: &FontCollection,
) {
    if !style.overflow.clips() || style.text_overflow != TextOverflow::Ellipsis {
        return;
    }

    for line in lines.iter_mut() {
        let line_width = line
            .runs
            .last()
            .map(|run| run.x_offset + run.width)
            .unwrap_or(0.0);
        if line_width <= content_width {
            continue;
        }
        let Some(last) = line.runs.last() else {
            continue;
        };
        let Some(mut ellipsis) = shape_ellipsis(last.font_index, last.style_index, style, fonts)
        else {
            continue;
        };

        // Reserve room for the ellipsis and keep as many runs as fit.
        // Each run's `x_offset` is its settled position within the line (inter-word spaces
        // included), so it is left alone: recomputing from a running total would shift by the spaces.
        let budget = (content_width - ellipsis.width).max(0.0);
        let mut kept: Vec<TextRun> = Vec::with_capacity(line.runs.len());
        let mut end_x = 0.0f32;
        for run in std::mem::take(&mut line.runs) {
            if run.x_offset + run.width <= budget {
                end_x = run.x_offset + run.width;
                kept.push(run);
                continue;
            }
            if let (Some(fitting), _) = split_run_at_width(&run, budget - run.x_offset) {
                end_x = run.x_offset + fitting.width;
                kept.push(fitting);
            }
            break;
        }

        ellipsis.x_offset = end_x;
        // The trimmed line's height and baseline are unchanged (the ellipsis is shaped in the
        // same style, so it does not affect the line's `ascent`/`descent`).
        kept.push(ellipsis);
        line.runs = kept;
    }
}

/// Build the run for the ellipsis (`...`). In a font without that glyph (`.notdef`) it falls
/// back to a hyphen, and `None` if there is no hyphen either.
fn shape_ellipsis(
    font_index: usize,
    style_index: usize,
    style: &ComputedStyle,
    fonts: &FontCollection,
) -> Option<TextRun> {
    for text in [ELLIPSIS, HYPHEN] {
        let mut run = shape_run(text, font_index, fonts, style);
        if run.glyphs.is_empty() || run.glyphs.iter().any(|g| g.glyph_id == 0) {
            continue;
        }
        run.style_index = style_index;
        return Some(run);
    }
    None
}

/// Add a hyphen at the end of a line (when broken at a soft hyphen). It is shaped in the
/// same style and font as the preceding run.
///
/// A known simplification: the hyphen's width is not counted in the "does it fit" decision
/// (it is added afterwards, so a line can overflow by one hyphen).
fn push_hyphen(
    current_runs: &mut Vec<TextRun>,
    current_width: &mut f32,
    span_styles: &[ComputedStyle],
    fonts: &FontCollection,
) {
    let Some(last) = current_runs.last() else {
        return;
    };
    let (font_index, style_index) = (last.font_index, last.style_index);
    let Some(style) = span_styles.get(style_index) else {
        return;
    };
    let mut hyphen = shape_run(HYPHEN, font_index, fonts, style);
    if hyphen.glyphs.is_empty() {
        return;
    }
    hyphen.style_index = style_index;
    hyphen.x_offset = *current_width;
    *current_width += hyphen.width;
    current_runs.push(hyphen);
}

/// Split a chunk into a first half fitting `max_width` and a remainder
/// (for the `overflow-wrap: break-word` fallback).
///
/// Cutting by glyph means no reshaping is needed (`ShapedGlyph::cluster` carries the byte
/// offset into the original text). It may cut in the middle of a ligature or a combining
/// sequence, but this path is only reached when a single word exceeds the line width (a known simplification).
fn split_chunk_to_fit(chunk: Vec<TextRun>, max_width: f32) -> (Vec<TextRun>, Vec<TextRun>) {
    let mut head = Vec::new();
    let mut rest = Vec::new();
    let mut used = 0.0f32;

    for run in chunk {
        if !rest.is_empty() {
            rest.push(run);
            continue;
        }
        if used + run.width <= max_width {
            used += run.width;
            head.push(run);
            continue;
        }
        let (fitting, remainder) = split_run_at_width(&run, max_width - used);
        if let Some(fitting) = fitting {
            used += fitting.width;
            head.push(fitting);
        }
        // `None` means every glyph fitted (a rounding difference against `run.width`). It should not normally happen.
        if let Some(remainder) = remainder {
            rest.push(remainder);
        }
    }

    (head, rest)
}

/// Split one run, by glyph, into the part fitting `max_width` and the remainder.
/// Either side may come out empty (expressed as `None`).
fn split_run_at_width(run: &TextRun, max_width: f32) -> (Option<TextRun>, Option<TextRun>) {
    let mut used = 0.0f32;
    let mut glyph_count = 0usize;
    for glyph in &run.glyphs {
        let advance = glyph.x_advance + run.letter_spacing;
        if used + advance > max_width {
            break;
        }
        used += advance;
        glyph_count += 1;
    }

    if glyph_count == 0 {
        return (None, Some(run.clone()));
    }
    if glyph_count == run.glyphs.len() {
        return (Some(run.clone()), None);
    }

    // The string is cut at the byte position into the original text that the split point's first trailing glyph points at.
    let split_byte = (run.glyphs[glyph_count].cluster as usize).min(run.text.len());
    let mut head = run.clone();
    head.glyphs = run.glyphs[..glyph_count].to_vec();
    head.text = run.text[..split_byte].to_string();
    head.width = used;

    let mut tail = run.clone();
    // A `cluster` is "a byte offset within that run's `text`", so in the second half it is
    // shifted back by however much was cut. Forgetting that would make the places that
    // recover a character via `text[cluster..]` (`/ToUnicode` generation, emphasis mark drawing) panic out of range.
    tail.glyphs = run.glyphs[glyph_count..]
        .iter()
        .map(|glyph| ShapedGlyph {
            cluster: glyph.cluster.saturating_sub(split_byte as u32),
            ..*glyph
        })
        .collect();
    tail.text = run.text[split_byte..].to_string();
    tail.width = (run.width - used).max(0.0);
    tail.x_offset = 0.0;
    // The second half was merely cut mid-word, so no hyphen is shown.
    tail.hyphen_before = false;
    tail.break_before = false;

    (Some(head), Some(tail))
}

/// Whether a break is allowed between `prev` and `next` (even with no whitespace).
/// Under `word-break: normal` it is a simplified decision, treating a break as allowed when
/// either side is a CJK character ([`is_cjk`]) (not a full implementation of UAX #14).
/// `break-all` allows one at every character boundary; `keep-all` allows none even at a CJK boundary.
fn is_break_boundary(prev: char, next: char, word_break: WordBreak) -> bool {
    // `&nbsp;` and the like (the GL and WJ classes of UAX #14) forbid a break either side.
    // That wins over `word-break`: such a character is placed to keep "10&nbsp;kg" together,
    // so honouring it even under `break-all` matches the user's intent (browsers do the same).
    if white_space::is_non_breaking(prev) || white_space::is_non_breaking(next) {
        return false;
    }
    // A break is allowed right after a thin space and the like (BA) and after a ZWSP (ZW).
    if white_space::allows_break_after(prev) {
        return true;
    }
    match word_break {
        WordBreak::BreakAll => true,
        WordBreak::KeepAll => false,
        WordBreak::Normal => is_cjk(prev) || is_cjk(next),
    }
}

/// Whether the character belongs to a script written without spaces between words, such as
/// hiragana, katakana, kanji (unified CJK ideographs, extension A, compatibility ideographs) or hangul.
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F   // CJK symbols and punctuation
        | 0x3040..=0x30FF // Hiragana and katakana
        | 0x31F0..=0x31FF // Katakana phonetic extensions
        | 0x3400..=0x4DBF // CJK unified ideographs extension A
        | 0x4E00..=0x9FFF // CJK unified ideographs
        | 0xAC00..=0xD7A3 // Hangul syllables
        | 0xF900..=0xFAFF // CJK compatibility ideographs
        | 0xFF00..=0xFFEF // Halfwidth and fullwidth forms
    )
}

/// Resolve a `LengthPercentage` to px using `basis` (the containing width)
/// (the same logic as `block.rs::resolve_lp`, duplicated here for `text-indent`).
fn resolve_length_percentage(lp: LengthPercentage, basis: f32) -> f32 {
    match lp {
        LengthPercentage::Length(px) => px,
        LengthPercentage::Percentage(fraction) => fraction * basis,
        LengthPercentage::Calc { px, percent } => px + percent * basis,
    }
}

/// The multiplier used when `line-height: normal` cannot be derived from font metrics
/// (the collection is empty and no font can be chosen). In practice only the font-less test
/// path reaches it; an ordinary document uses [`Font::normal_line_height`].
/// path reaches it; ordinary documents use [`Font::normal_line_height`].
const NORMAL_LINE_HEIGHT_FALLBACK: f32 = 1.2;

/// Find the px value of a computed `line-height` using this element's own `font_size`
/// (`Number`/`Normal` are multiplied by the element's font-size here, at the point of use).
///
/// `normal` differs per font (ascent plus descent plus line gap), so the font actually used
/// by that run has to be passed in. With `font` as `None` it is approximated by a fixed multiplier.
fn resolve_line_height(style: &ComputedStyle, font: Option<&Font>) -> f32 {
    let font_size = style.font_size.0;
    match style.line_height {
        LineHeight::Normal => match font {
            Some(font) => font.normal_line_height(font_size),
            None => font_size * NORMAL_LINE_HEIGHT_FALLBACK,
        },
        LineHeight::Number(n) => n * font_size,
        LineHeight::Length(px) => px,
    }
}

/// The "first available font" (CSS's first available font) for `style`'s `font-family`.
/// A space is used as the representative character so that `line-height: normal` can be
/// resolved even for a line with no text (a line holding only a `<br>`, or an empty `white-space: pre` line).
fn first_available_font<'a>(style: &ComputedStyle, fonts: &'a FontCollection) -> Option<&'a Font> {
    fonts
        .select_for_char(&style.font_family, style.font_weight, style.font_style, ' ')
        .and_then(|index| fonts.get(index))
}

/// The height (the used value of `line-height`) of a line with no text.
fn empty_line_height(style: &ComputedStyle, fonts: &FontCollection) -> f32 {
    resolve_line_height(style, first_available_font(style, fonts))
}

/// Layout for `white-space: pre`. Lines are split explicitly at newline characters (`\n`)
/// and runs of whitespace are preserved as-is (never collapsed, and not routed through
/// `split_into_words`). There is no wrapping (as with `nowrap`; keeping it a separate path
/// from the body of `layout_inline_content` avoids any regression risk to the Normal/Nowrap side).
/// `split_word_into_runs`/`group_into_chunks` can be reused unchanged, but
/// `group_into_chunks` exists to decide breakability at CJK boundaries and pre never wraps,
/// so it is not used: the result of `split_word_into_runs` is concatenated straight into one line.
#[allow(clippy::too_many_arguments)]
fn layout_pre_content(
    chars: &[StyledChar],
    span_styles: &[ComputedStyle],
    span_links: &[Option<Rc<str>>],
    fonts: &FontCollection,
    available_width: f32,
    origin_x: f32,
    origin_y: f32,
    float_ctx: Option<&FloatContext>,
) -> Vec<LineBox> {
    let text_indent = span_styles
        .first()
        .map(|s| resolve_length_percentage(s.text_indent, available_width))
        .unwrap_or(0.0);

    let mut lines = Vec::new();
    let mut cursor_y = origin_y;

    for segment in chars.split(|sc| sc.ch == '\n') {
        // The line height is approximated from the style of that line's first character
        // (or the IFC's first span if there is none) (a known simplification).
        let hint = segment
            .first()
            .and_then(|sc| span_styles.get(sc.style_index))
            .or_else(|| span_styles.first())
            .map(|style| empty_line_height(style, fonts))
            .unwrap_or(0.0);
        let (mut line_left, _) = line_band(float_ctx, cursor_y, hint, origin_x, available_width);
        // `text-indent` applies only to the first physical line (CSS2.1 section 16.1).
        if lines.is_empty() {
            line_left += text_indent;
        }

        if segment.is_empty() {
            // An empty line from consecutive newlines. A dummy line that consumes only height.
            lines.push(finish_line(
                Vec::new(),
                Vec::new(),
                0.0,
                line_left,
                cursor_y,
                hint,
                fonts,
            ));
            cursor_y += hint;
            continue;
        }

        // `white-space: pre` never wraps, so the break-opportunity decision (`word-break`)
        // does not affect the result (it is always called with `Normal`).
        let runs = split_word_into_runs(segment, span_styles, span_links, fonts, WordBreak::Normal);
        let mut current_width = 0.0;
        let mut placed_runs = Vec::with_capacity(runs.len());
        for mut run in runs {
            run.x_offset = current_width;
            current_width += run.width;
            placed_runs.push(run);
        }
        let line_height = line_height_for(&placed_runs);
        lines.push(finish_line(
            placed_runs,
            Vec::new(),
            current_width,
            line_left,
            cursor_y,
            line_height,
            fonts,
        ));
        cursor_y += line_height;
    }

    lines
}

/// Also used to shape the `list-style-type` marker text
/// (`block.rs::layout_list_marker`), hence `pub(super)`.
pub(super) fn shape_run(
    text: &str,
    font_index: usize,
    fonts: &FontCollection,
    style: &ComputedStyle,
) -> TextRun {
    let font = fonts
        .get(font_index)
        .expect("font_index is always in range");
    let font_size = style.font_size.0;
    let shaped = shape_text(font, text, font_size);
    // If the chosen font really is Bold/Italic, no synthesis is needed
    // (`fonts::FontCollection::select_for_char` prefers a real Bold/Italic face, so where one
    // exists among `--font`/`@font-face`/the system fonts, the synthesis is skipped here).
    // the synthesis is skipped here).
    let needs_synthetic_bold = style.font_weight == FontWeight::Bold && !fonts.is_bold(font_index);
    let needs_synthetic_italic =
        style.font_style == FontStyle::Italic && !fonts.is_italic(font_index);
    let mut line_height = resolve_line_height(style, Some(font));
    // `letter-spacing` adds to the width once per glyph (a known simplification that also
    // adds it at the end of the line). The PDF drawing layer uses `run.letter_spacing` as
    // `Tc`, so the width computed here matches what is rendered.
    let width = shaped.width + style.letter_spacing * shaped.glyphs.len() as f32;
    let units_per_em = font.units_per_em() as f32;
    let mut ascent = font.ascender() as f32 / units_per_em * font_size;
    let mut descent = -(font.descender() as f32) / units_per_em * font_size;

    // `text-emphasis` marks increase the line box's height. With `over` the `0.5em` is added
    // to the ascent side, with `under` to the descent side.
    let emphasis = (style.text_emphasis_style != EmphasisStyle::None).then(|| {
        let size = font_size * EMPHASIS_SIZE_RATIO;
        match style.text_emphasis_position {
            EmphasisPosition::Over => ascent += size,
            EmphasisPosition::Under => descent += size,
        }
        // The line box's height is floored by the value derived from `line-height`
        // (`line_height_for` into `finish_line`), so the marks are added there too.
        // Without that, the marks would overlap the lines above and below.
        line_height += size;
        EmphasisMark {
            style: style.text_emphasis_style.clone(),
            color: style.text_emphasis_color,
            position: style.text_emphasis_position,
            size,
        }
    });

    TextRun {
        font_index,
        font_size,
        color: style.color,
        link: None,
        background_color: style.background_color,
        bold: needs_synthetic_bold,
        italic: needs_synthetic_italic,
        underline: style.text_decoration_line.underline,
        line_through: style.text_decoration_line.line_through,
        text: text.to_string(),
        glyphs: shaped.glyphs,
        x_offset: 0.0,
        width,
        line_height,
        letter_spacing: style.letter_spacing,
        word_spacing: style.word_spacing,
        ascent,
        descent,
        // `baseline_shift` is filled in by `resolve_baseline_shifts` once the line is settled.
        baseline_shift: 0.0,
        vertical_align: style.vertical_align,
        text_shadow: (!style.text_shadow.is_empty())
            .then(|| Rc::from(style.text_shadow.as_slice())),
        emphasis,
        // `style_index`/`hyphen_before`/`break_before` are set by the caller
        // (`split_word_into_runs`). On paths that use this on its own
        // (`shape_standalone_line` and the like) the defaults are fine.
        style_index: 0,
        hyphen_before: false,
        break_before: false,
    }
}

/// Shape an arbitrary string as a single unwrapped line, starting at `(origin_x, origin_y)`.
/// For uses that do not go through an ordinary DOM text node (`@page` margin boxes).
/// The font is re-chosen per character with `fonts.select_for_char`
pub fn shape_standalone_line(
    text: &str,
    style: &ComputedStyle,
    fonts: &FontCollection,
    origin_x: f32,
    origin_y: f32,
) -> LineBox {
    let mut runs: Vec<TextRun> = Vec::new();
    let mut current_font: Option<usize> = None;
    let mut current_text = String::new();

    for ch in text.chars() {
        let font_index = fonts
            .select_for_char(&style.font_family, style.font_weight, style.font_style, ch)
            .unwrap_or(0);
        if current_font == Some(font_index) {
            current_text.push(ch);
        } else {
            if let Some(fi) = current_font {
                runs.push(shape_run(&current_text, fi, fonts, style));
            }
            current_text.clear();
            current_text.push(ch);
            current_font = Some(font_index);
        }
    }
    if let Some(fi) = current_font {
        runs.push(shape_run(&current_text, fi, fonts, style));
    }

    let mut x_cursor = 0.0;
    let mut max_height: f32 = 0.0;
    for run in &mut runs {
        run.x_offset = x_cursor;
        x_cursor += run.width;
        max_height = max_height.max(run.line_height);
    }

    // A single line for a margin box has its baseline settled through the same path as an
    // ordinary line (not because `vertical-align` applies, but to keep the responsibility for
    // giving a `LineBox` its `baseline` in one place).
    finish_line(
        runs,
        Vec::new(),
        x_cursor,
        origin_x,
        origin_y,
        max_height,
        fonts,
    )
}

fn measure_space_width(fonts: &FontCollection, font_index: usize, font_size: f32) -> f32 {
    let Some(font) = fonts.get(font_index) else {
        return 0.0;
    };
    measure_text(font, " ", font_size)
}

/// The line's height is based on the largest computed `line_height` among its runs.
fn line_height_for(runs: &[TextRun]) -> f32 {
    runs.iter().map(|r| r.line_height).fold(0.0f32, f32::max)
}

/// Resolve the `vertical-align` of a settled line, find the line box's height and baseline
/// position, and assemble the [`LineBox`].
///
/// `height` is the height derived from the `line-height` property (the result of
/// `line_height_for`), used as the floor on the line box's height. In a document that does
/// not use `vertical-align`, the height and baseline position are exactly as before.
pub(super) fn finish_line(
    mut runs: Vec<TextRun>,
    mut atomics: Vec<AtomicInline>,
    width: f32,
    x: f32,
    y: f32,
    height: f32,
    fonts: &FontCollection,
) -> LineBox {
    resolve_baseline_shifts(&mut runs, fonts);
    // An atomic box aligns the bottom of its margin box to the baseline. That is, it takes
    // part in the line with ascent = the margin box height and descent = 0.
    for atomic in atomics.iter_mut() {
        atomic.baseline_shift = match atomic.vertical_align {
            VerticalAlign::LengthPercentage(LengthPercentage::Length(px)) => px,
            VerticalAlign::LengthPercentage(LengthPercentage::Percentage(fraction)) => {
                height * fraction
            }
            // `sub`/`super`/`text-*`/`middle` have strict definitions for a box that do not
            // fit this engine's simplifications, so they are treated as `baseline`.
            _ => 0.0,
        };
    }

    // The baseline position as determined by `line-height` alone (that is, with no `vertical-align`).
    // On a single-font line it matches `Font::baseline_offset`.
    let mut baseline = 0.0f32;
    for run in &runs {
        let half_leading = (height - (run.ascent + run.descent)) / 2.0;
        baseline = baseline.max(half_leading + run.ascent);
    }
    let mut above = baseline;
    let mut below = height - baseline;

    // An atomic box always takes part in the line's height (its bottom being the baseline).
    // Unlike a text run it is not excluded even under `top`/`bottom`: on a line holding
    // nothing but boxes (`<p><input></p>`, or a row of cards) the line height would be 0 and
    // overlap what follows. For `top`/`bottom` it is enough that "the line is at least as
    // tall as the box"; the real position is decided once the line's dimensions are settled.
    for atomic in atomics.iter() {
        if matches!(
            atomic.vertical_align,
            VerticalAlign::Top | VerticalAlign::Bottom
        ) {
            above = above.max(atomic.margin_box_height);
        } else {
            above = above.max(atomic.margin_box_height + atomic.baseline_shift);
            below = below.max(-atomic.baseline_shift);
        }
    }

    // Only shifted runs are considered, and only for how far they stick out of the line box.
    // A run with a `baseline_shift` of 0 does not affect the line's height, so in a document
    // that does not use `vertical-align` the line heights and baselines are exactly as before.
    for run in runs.iter().filter(|r| {
        r.baseline_shift != 0.0
            && !matches!(r.vertical_align, VerticalAlign::Top | VerticalAlign::Bottom)
    }) {
        above = above.max(run.ascent + run.baseline_shift);
        below = below.max(run.descent - run.baseline_shift);
    }

    let line_height = if runs.is_empty() && atomics.is_empty() {
        height
    } else {
        above + below
    };
    let baseline = if runs.is_empty() && atomics.is_empty() {
        0.0
    } else {
        above
    };

    // Values that can only be resolved once the line box's dimensions are known.
    for run in &mut runs {
        match run.vertical_align {
            VerticalAlign::Top => run.baseline_shift = baseline - run.ascent,
            VerticalAlign::Bottom => run.baseline_shift = -(line_height - baseline - run.descent),
            _ => {}
        }
    }
    // The same for `top`/`bottom` on an atomic box (a box counts as ascent = the margin box
    // height and descent = 0).
    for atomic in &mut atomics {
        match atomic.vertical_align {
            VerticalAlign::Top => {
                atomic.baseline_shift = baseline - atomic.margin_box_height;
            }
            VerticalAlign::Bottom => atomic.baseline_shift = -(line_height - baseline),
            _ => {}
        }
    }

    LineBox {
        rect: Rect {
            x,
            y,
            width,
            height: line_height,
        },
        runs,
        baseline,
        atomics,
    }
}

/// Lay out the contents of a `display: inline-block`. It establishes a new Block Formatting
/// Context, so an empty `FloatContext` is passed. The width is the explicit setting if there
/// is one, and otherwise the content's natural width clamped by the available width.
///
/// The box is built at the origin `(0, 0)` and moved to its final coordinates by
/// `place_atomic_inlines` once the line's position is settled. The `absolute`s collected
/// need no moving (the same reason as a table cell: an absolute position is determined solely by its containing block).
fn layout_atomic_inline(
    b: &LayoutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    available_width: f32,
    pos: &mut PosCtx,
) -> LaidOutBox {
    let mut style = b
        .node
        .and_then(|n| styles.get(&n))
        .map(|shared| (**shared).clone())
        .unwrap_or_default();
    // A replaced element (`<img>`) gets its dimensions from its `width`/`height` attributes
    // and the image's intrinsic size. The same processing as block placement
    // (`resolve_box_geometry`) runs here too, sharing the sizing logic.
    if let BoxContent::Image(image_content) = &b.content {
        super::block::apply_replaced_element_auto_size(&mut style, image_content, available_width);
    }
    let padding = super::block::resolve_padding(&style, available_width);
    let border = super::block::resolve_border(&style);

    // The normal-flow `resolve_width_and_horizontal_margins` is not used. Its
    // over-constrained rule (CSS2.1 section 10.3.3, recomputing margin-right to fill the
    // remaining width when both width and margins are non-auto) does not apply to an
    // inline-level box, and passing it through would let a huge margin-right leak into the
    // line advance width (the same reason floats bypass it). Instead the used content width
    // is decided here and passed as `forced_content_width`.
    let content_width = match style.width {
        LengthPercentageOrAuto::LengthPercentage(lp) => {
            let width = super::block::resolve_lp(lp, available_width);
            if style.box_sizing == BoxSizing::BorderBox {
                (width - padding.left - padding.right - border.left - border.right).max(0.0)
            } else {
                width
            }
        }
        // The shrink-to-fit equivalent: the content's natural width clamped by the available
        // width. It shares `shrink_to_fit_content_width` with `width: auto` on a float.
        LengthPercentageOrAuto::Auto => {
            let outer = padding.left + padding.right + border.left + border.right;
            // With a settled height, the width follows from `aspect-ratio`.
            super::block::aspect_ratio_width(&style, &padding, &border).unwrap_or_else(|| {
                super::block::shrink_to_fit_content_width(
                    b,
                    styles,
                    fonts,
                    &style,
                    (available_width - outer).max(0.0),
                )
            })
        }
    };
    // `min-width`/`max-width`.
    let content_width = super::block::clamp_used_width(
        &style,
        available_width,
        padding.left + padding.right,
        border.left + border.right,
        content_width,
    );

    let mut float_ctx = FloatContext::new();
    super::block::layout_box_with_forced_width(
        b,
        styles,
        fonts,
        available_width,
        content_width,
        &mut float_ctx,
        0.0,
        0.0,
        pos,
    )
}

/// The margin box width (used for the line advance).
fn margin_box_width_of(b: &LaidOutBox) -> f32 {
    let border_box = b.layout.border_box();
    b.layout.margin.left + border_box.width + b.layout.margin.right
}

/// Find each run's `baseline_shift` (px, positive being up) from its `vertical-align`.
/// `top`/`bottom` need the line box's dimensions, so they stay 0 here and are resolved
/// afterwards by [`finish_line`].
fn resolve_baseline_shifts(runs: &mut [TextRun], fonts: &FontCollection) {
    // The reference for `text-top`/`text-bottom`/`middle` is the line's first run.
    let Some(first) = runs.first() else {
        return;
    };
    let base_ascent = first.ascent;
    let base_descent = first.descent;
    let base_x_height = fonts
        .get(first.font_index)
        .map(|f| f.x_height(first.font_size))
        .unwrap_or(first.font_size * 0.5);

    for run in runs.iter_mut() {
        run.baseline_shift = match run.vertical_align {
            VerticalAlign::Baseline | VerticalAlign::Top | VerticalAlign::Bottom => 0.0,
            VerticalAlign::Sub => -fonts
                .get(run.font_index)
                .map(|f| f.subscript_offset(run.font_size))
                .unwrap_or(run.font_size * 0.2),
            VerticalAlign::Super => fonts
                .get(run.font_index)
                .map(|f| f.superscript_offset(run.font_size))
                .unwrap_or(run.font_size * 0.33),
            VerticalAlign::TextTop => base_ascent - run.ascent,
            VerticalAlign::TextBottom => run.descent - base_descent,
            VerticalAlign::Middle => base_x_height / 2.0 - (run.ascent - run.descent) / 2.0,
            VerticalAlign::LengthPercentage(lp) => resolve_length_percentage(lp, run.line_height),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Font;
    use crate::html::{self, Dom};
    use crate::layout::box_tree::{build_box_tree, BoxContent, LayoutBox};
    use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

    /// Shadow the real function under the same name, wrapping it so the tests need not
    /// provide somewhere to collect absolute positioning each time. Only the line layout
    #[allow(clippy::too_many_arguments)]
    fn layout_inline_content(
        spans: &[InlineSpan],
        styles: &HashMap<NodeId, Rc<ComputedStyle>>,
        fonts: &FontCollection,
        available_width: f32,
        origin_x: f32,
        origin_y: f32,
        float_ctx: Option<&FloatContext>,
    ) -> Vec<LineBox> {
        let mut discarded = Vec::new();
        let mut pos = PosCtx::new(&mut discarded, (0.0, 0.0));
        super::layout_inline_content(
            spans,
            styles,
            fonts,
            available_width,
            origin_x,
            origin_y,
            float_ctx,
            None,
            &mut pos,
        )
    }

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
    const DEJAVU_BOLD_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/DejaVuSans-Bold.ttf"
    );
    const CJK_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/NotoSansCJK-Regular.ttc"
    );

    fn dejavu_only() -> FontCollection {
        FontCollection::new(vec![Font::load(DEJAVU_PATH).unwrap()])
    }

    /// The height of one line under the default style (`line-height: normal`). `normal` comes
    /// from the font metrics, so it is derived from the test font.
    fn default_line_height(fonts: &FontCollection) -> f32 {
        fonts
            .get(0)
            .expect("there is always at least one test font")
            .normal_line_height(ComputedStyle::default().font_size.0)
    }

    fn dejavu_regular_and_bold() -> FontCollection {
        FontCollection::new(vec![
            Font::load(DEJAVU_PATH).unwrap(),
            Font::load(DEJAVU_BOLD_PATH).unwrap(),
        ])
    }

    fn dejavu_and_cjk() -> FontCollection {
        FontCollection::new(vec![
            Font::load(DEJAVU_PATH).unwrap(),
            Font::load_indexed(CJK_PATH, 0).unwrap(),
        ])
    }

    fn find_inline_spans(b: &LayoutBox) -> Option<&Vec<InlineSpan>> {
        match &b.content {
            BoxContent::Inline(spans) => Some(spans),
            BoxContent::Blocks(children) => children.iter().find_map(find_inline_spans),
            BoxContent::Grid(grid) => grid.items.iter().find_map(find_inline_spans),
            BoxContent::Table(table) => table
                .caption
                .as_deref()
                .and_then(find_inline_spans)
                .or_else(|| {
                    table
                        .rows
                        .iter()
                        .flat_map(|row| &row.cells)
                        .find_map(|cell| find_inline_spans(&cell.content))
                }),
            BoxContent::Flex(flex) => flex.items.iter().find_map(find_inline_spans),
            BoxContent::Image(_) => None,
        }
    }

    /// Parse `<p>{inner_html}</p>` and return the first inline box's span list and computed
    /// styles (for tests that go through the real DOM-to-box-tree path).
    fn spans_for(
        inner_html: &str,
        css: &str,
    ) -> (Dom, Vec<InlineSpan>, HashMap<NodeId, Rc<ComputedStyle>>) {
        let html_src = format!("<p>{inner_html}</p>");
        let dom = html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(css);
        let styles = compute_styles(&dom, &ua, &author);
        let tree = build_box_tree(&dom, &styles);
        let spans = find_inline_spans(&tree)
            .expect("expected inline content")
            .clone();
        (dom, spans, styles)
    }

    #[test]
    fn empty_or_whitespace_only_text_produces_no_lines() {
        let (_, spans, styles) = spans_for("", "");
        let fonts = dejavu_only();
        assert!(layout_inline_content(&spans, &styles, &fonts, 200.0, 0.0, 0.0, None).is_empty());

        let (_, spans, styles) = spans_for("   \n\t  ", "");
        assert!(layout_inline_content(&spans, &styles, &fonts, 200.0, 0.0, 0.0, None).is_empty());
    }

    #[test]
    fn empty_font_collection_produces_no_lines() {
        let (_, spans, styles) = spans_for("hello", "");
        let fonts = FontCollection::new(vec![]);
        assert!(layout_inline_content(&spans, &styles, &fonts, 200.0, 0.0, 0.0, None).is_empty());
    }

    #[test]
    fn text_that_fits_stays_on_a_single_line() {
        let (_, spans, styles) = spans_for("hello world", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 10.0, 20.0, None);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].rect.x, 10.0);
        assert_eq!(lines[0].rect.y, 20.0);
        assert!(lines[0].rect.width > 0.0);
        assert_eq!(lines[0].rect.height, default_line_height(&fonts));
        // They are consecutive and identical in appearance, so they merge into one run and the inter-word space is restored.
        assert_eq!(lines[0].runs.len(), 1);
        assert_eq!(lines[0].runs[0].text, "hello world");
        assert!(lines[0].runs.iter().all(|r| r.font_index == 0));
    }

    #[test]
    fn first_letter_style_overrides_are_applied_only_to_the_split_off_run() {
        let (_, spans, styles) = spans_for(
            "Hello world",
            "p::first-letter { font-size: 2em; color: rgb(200, 0, 0); font-weight: bold; }",
        );
        // Use a font set with no real bold face and check, via the synthetic bold flag, that
        // the first-letter's font-weight reached the run.
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        let runs = &lines[0].runs;
        assert!(runs.len() >= 2, "first-letter run + remainder run(s)");

        let base_font_size = ComputedStyle::default().font_size.0;
        assert_eq!(runs[0].text, "H");
        assert_eq!(runs[0].font_size, base_font_size * 2.0);
        assert_eq!(
            runs[0].color,
            RgbaColor {
                red: 200,
                green: 0,
                blue: 0,
                alpha: 1.0
            }
        );
        assert!(runs[0].bold);

        // The rest is identical in appearance, so it merges into one run, inter-word space included.
        let remainder: String = runs[1..].iter().map(|r| r.text.as_str()).collect();
        assert_eq!(remainder, "ello world");
        assert_eq!(runs[1].font_size, base_font_size);
        assert_eq!(runs[1].color, ComputedStyle::default().color);
        assert!(!runs[1].bold);
    }

    #[test]
    fn wraps_to_a_new_line_when_available_width_is_too_narrow() {
        let fonts = dejavu_only();

        let (_, spans, styles) = spans_for("hello world foo bar", "");
        let one_line = layout_inline_content(&spans, &styles, &fonts, 1000.0, 0.0, 0.0, None);
        assert_eq!(one_line.len(), 1);

        let wrapped = layout_inline_content(&spans, &styles, &fonts, 60.0, 0.0, 0.0, None);
        assert!(wrapped.len() > 1);

        let line_height = default_line_height(&fonts);
        assert_eq!(wrapped[1].rect.y, wrapped[0].rect.y + line_height);
    }

    #[test]
    fn float_narrows_the_band_for_lines_overlapping_it() {
        use crate::style::Float;

        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for("hello world foo bar", "");

        // Place a 400px-wide float of ample height on the left and check that every line is
        // pushed to its right (from x=400, width 100).
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 400.0, 1000.0);

        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, Some(&ctx));
        assert!(!lines.is_empty());
        for line in &lines {
            assert_eq!(line.rect.x, 400.0);
            assert!(
                line.rect.width <= 100.0,
                "line width {} should not exceed the 100px band beside the float",
                line.rect.width
            );
        }
    }

    #[test]
    fn line_widens_back_after_passing_the_bottom_of_the_float() {
        use crate::style::Float;

        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for("hello world foo bar baz", "");
        let line_height = default_line_height(&fonts);

        // The float is only one line tall: line 1 is pushed to its right, and from line 2 on
        // it clears the float and returns to the original width and left edge.
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 400.0, line_height);

        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, Some(&ctx));
        assert!(lines.len() >= 2, "expected wrapping to at least 2 lines");
        assert_eq!(lines[0].rect.x, 400.0);
        assert_eq!(
            lines[1].rect.x, 0.0,
            "second line should return to the full width once below the float"
        );
    }

    #[test]
    fn no_float_context_behaves_like_the_unconstrained_case() {
        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for("hello world", "");

        let with_none = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let empty_ctx = FloatContext::new();
        let with_empty_ctx =
            layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, Some(&empty_ctx));

        // A `LineBox` now contains `display: inline-block` boxes and has no PartialEq, so
        // lines are compared by their geometry and text.
        assert_eq!(with_none.len(), with_empty_ctx.len());
        for (a, b) in with_none.iter().zip(with_empty_ctx.iter()) {
            assert_eq!(a.rect, b.rect);
            assert_eq!(a.baseline, b.baseline);
            assert_eq!(
                line_texts(std::slice::from_ref(a)),
                line_texts(std::slice::from_ref(b))
            );
        }
    }

    #[test]
    fn overlong_single_word_is_not_split_and_still_placed() {
        let (_, spans, styles) = spans_for("supercalifragilisticexpialidocious", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 10.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].rect.width > 10.0,
            "overflowing word should not be dropped or split"
        );
    }

    #[test]
    fn collapses_runs_of_whitespace_between_words() {
        let (_, spans, styles) = spans_for("a    b\n\tc", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        // Consecutive spaces, newlines and tabs collapse into a single space.
        assert_eq!(lines[0].runs.len(), 1);
        assert_eq!(lines[0].runs[0].text, "a b c");
    }

    #[test]
    fn mixed_script_word_splits_into_separate_font_runs() {
        // One token mixing Latin and CJK with no whitespace. CJK characters (Japanese) are
        // breakable boundaries, so they split into one run per character even with the same
        // style and font ("café" + three kanji/kana = 4 runs).
        let (_, spans, styles) = spans_for("café日本語", "");
        let fonts = dejavu_and_cjk();

        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        // Only the font boundary splits runs (the CJK is shaped character by character and
        // then merges again, continuing in the same font with no gap).
        assert_eq!(lines[0].runs.len(), 2, "two runs: café and the Japanese");
        assert_eq!(
            lines[0].runs[0].font_index, 0,
            "café should use DejaVu Sans"
        );
        assert_eq!(lines[0].runs[0].text, "café");
        assert_eq!(
            lines[0].runs[1].font_index, 1,
            "日本語 should use the CJK fallback font"
        );
        assert_eq!(lines[0].runs[1].text, "日本語");
        // The runs continue left to right with no gap (being within a word, no space is inserted).
        let mut prev_end = lines[0].runs[0].x_offset + lines[0].runs[0].width;
        for run in &lines[0].runs[1..] {
            assert_eq!(run.x_offset, prev_end);
            prev_end = run.x_offset + run.width;
        }
    }

    #[test]
    fn separate_cjk_and_latin_words_can_land_on_the_same_line() {
        let (_, spans, styles) = spans_for("Invoice 請求書", "");
        let fonts = dejavu_and_cjk();

        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        // Two runs, the fonts differing. The Japanese is one font and merges.
        assert_eq!(lines[0].runs.len(), 2);
        assert_eq!(lines[0].runs[0].font_index, 0);
        assert_eq!(lines[0].runs[0].text, "Invoice");
        assert_eq!(lines[0].runs[1].font_index, 1);
        assert_eq!(lines[0].runs[1].text, "請求書");
    }

    #[test]
    fn long_cjk_sequence_wraps_between_characters_without_whitespace() {
        // Even a long CJK string with no whitespace can break between characters when it does
        // not fit the line width (the language being written without spaces).
        let (_, spans, styles) = spans_for("日本語のテスト文章です", "");
        let fonts = dejavu_and_cjk();

        let narrow = layout_inline_content(&spans, &styles, &fonts, 60.0, 0.0, 0.0, None);
        assert!(
            narrow.len() > 1,
            "a narrow line width should force wrapping within the CJK sequence"
        );
        for line in &narrow {
            assert!(
                !line.runs.is_empty(),
                "every wrapped line should contain at least one run"
            );
        }

        let wide = layout_inline_content(&spans, &styles, &fonts, 2000.0, 0.0, 0.0, None);
        assert_eq!(
            wide.len(),
            1,
            "a wide enough line should keep the whole sequence on one line"
        );
    }

    #[test]
    fn cafe_nihongo_wraps_between_the_script_boundary_when_narrow() {
        let (_, spans, styles) = spans_for("café日本語", "");
        let fonts = dejavu_and_cjk();

        // At a line width just fitting "café", the Japanese that follows should not fit.
        let single_line = layout_inline_content(&spans, &styles, &fonts, 10000.0, 0.0, 0.0, None);
        let cafe_width = single_line[0].runs[0].width;

        let lines =
            layout_inline_content(&spans, &styles, &fonts, cafe_width + 1.0, 0.0, 0.0, None);
        assert!(
            lines.len() > 1,
            "should wrap at the café/日 boundary instead of overflowing as one unbreakable word"
        );
        assert_eq!(lines[0].runs.len(), 1);
        assert_eq!(lines[0].runs[0].text, "café");
    }

    #[test]
    fn bold_span_in_the_middle_of_a_word_splits_into_separate_runs() {
        // "bo" is regular and "ld" is <b> (bold): a style boundary mid-word.
        let (_, spans, styles) = spans_for("bo<b>ld</b>", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs.len(), 2, "should split at the <b> boundary");
        assert!(!lines[0].runs[0].bold);
        assert!(lines[0].runs[1].bold);
        assert_eq!(lines[0].runs[0].text, "bo");
        assert_eq!(lines[0].runs[1].text, "ld");
    }

    #[test]
    fn bold_span_uses_the_real_bold_face_and_skips_synthetic_bold_when_available() {
        // "bo" is regular, "ld" is <b> (bold). With DejaVu Sans's Bold included in the font
        // collection, a real Bold face should be chosen rather than faux bold
        // (without an explicit family name, the default "sans-serif" matches neither font's
        // name and falls back to the first font regardless of weight/style, missing the branch
        // under test, so the family is given explicitly).
        let (_, spans, styles) = spans_for("bo<b>ld</b>", "p { font-family: 'DejaVu Sans'; }");
        let fonts = dejavu_regular_and_bold();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs.len(), 2);
        assert_eq!(
            lines[0].runs[0].font_index, 0,
            "\"bo\" (normal weight) should use the regular face"
        );
        assert!(!lines[0].runs[0].bold);
        assert_eq!(
            lines[0].runs[1].font_index, 1,
            "\"ld\" (bold) should use the real bold face, not the regular one"
        );
        assert!(
            !lines[0].runs[1].bold,
            "no synthetic bold should be applied when a real bold face was selected"
        );
    }

    #[test]
    fn bold_span_prefers_the_real_bold_face_even_without_a_matching_font_family() {
        // Even with no font-family given at all (the default "sans-serif"), the global
        // fallback that ignores family matching should still prefer a weight/style match and
        // choose the real Bold face.
        let (_, spans, styles) = spans_for("bo<b>ld</b>", "");
        let fonts = dejavu_regular_and_bold();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs.len(), 2);
        assert_eq!(lines[0].runs[0].font_index, 0);
        assert_eq!(
            lines[0].runs[1].font_index, 1,
            "bold text should still find the real bold face via the family-agnostic fallback"
        );
        assert!(!lines[0].runs[1].bold);
    }

    #[test]
    fn text_transform_uppercase_and_lowercase_apply_to_every_character() {
        let (_, spans, styles) = spans_for("Hello World", "p { text-transform: uppercase; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "HELLO WORLD");

        let (_, spans, styles) = spans_for("Hello World", "p { text-transform: lowercase; }");
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn text_transform_capitalize_affects_only_the_first_letter_of_each_word() {
        let (_, spans, styles) = spans_for("hello world", "p { text-transform: capitalize; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "Hello World");
    }

    #[test]
    fn text_transform_capitalize_treats_a_span_boundary_as_a_word_start() {
        // Even where a word's start crosses a span boundary, as in "hello <b>world</b>",
        // capitalize should still capitalise correctly.
        let (_, spans, styles) =
            spans_for("hello <b>world</b>", "p { text-transform: capitalize; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "HelloWorld");
    }

    #[test]
    fn word_spacing_widens_the_gap_between_words() {
        let (_, spans, styles) = spans_for("hello world", "");
        let fonts = dejavu_only();
        let without = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        let (_, spans, styles) = spans_for("hello world", "p { word-spacing: 20px; }");
        let with = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        // Inter-word spaces are merged into the runs, so the comparison uses the whole line's width.
        let width_without = without[0].rect.width;
        let width_with = with[0].rect.width;
        assert!(
            width_with > width_without,
            "word-spacing should widen the gap: without={width_without}, with={width_with}"
        );
    }

    #[test]
    fn letter_spacing_widens_run_width_by_glyph_count() {
        let (_, spans, styles) = spans_for("hello", "");
        let fonts = dejavu_only();
        let without = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        let (_, spans, styles) = spans_for("hello", "p { letter-spacing: 2px; }");
        let with = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        let glyph_count = with[0].runs[0].glyphs.len() as f32;
        assert_eq!(
            with[0].runs[0].width,
            without[0].runs[0].width + 2.0 * glyph_count
        );
        assert_eq!(with[0].runs[0].letter_spacing, 2.0);
    }

    #[test]
    fn white_space_nowrap_does_not_wrap_even_when_overflowing() {
        let (_, spans, styles) = spans_for("hello world foo bar", "p { white-space: nowrap; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 60.0, 0.0, 0.0, None);

        assert_eq!(
            lines.len(),
            1,
            "nowrap should keep everything on a single line even when it overflows"
        );
        assert!(lines[0].rect.width > 60.0);
    }

    #[test]
    fn white_space_pre_preserves_explicit_newlines_and_does_not_wrap() {
        let (_, spans, styles) = spans_for(
            "hello&#10;world this is a long line",
            "p { white-space: pre; }",
        );
        let fonts = dejavu_only();
        // Even at a narrow width it should not wrap anywhere but an explicit newline (\n).
        let lines = layout_inline_content(&spans, &styles, &fonts, 60.0, 0.0, 10.0, None);

        assert_eq!(lines.len(), 2, "should split only at the explicit newline");
        assert_eq!(lines[0].runs.len(), 1);
        assert_eq!(lines[0].runs[0].text, "hello");
        assert!(
            lines[1].rect.width > 60.0,
            "the second physical line should not wrap despite overflowing"
        );
        let line_height = default_line_height(&fonts);
        assert_eq!(lines[1].rect.y, lines[0].rect.y + line_height);
    }

    #[test]
    fn white_space_pre_preserves_runs_of_whitespace() {
        let (_, spans, styles) = spans_for("a   b", "p { white-space: pre; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "a   b", "runs of whitespace should not be collapsed");
    }

    #[test]
    fn white_space_pre_preserves_leading_whitespace_before_an_inline_element() {
        // Leading whitespace survives as indentation. The content starts with a
        // whitespace-only text node, which used to be discarded in `box_tree`, giving `xy`.
        let (_, spans, styles) = spans_for("   <b>x</b>y", "p { white-space: pre; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        let text: String = lines[0].runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text, "   xy", "the indentation should be preserved");
    }

    #[test]
    fn white_space_pre_consecutive_newlines_produce_an_empty_line() {
        let (_, spans, styles) = spans_for("a&#10;&#10;b", "p { white-space: pre; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(
            lines.len(),
            3,
            "two newlines should produce 3 physical lines"
        );
        assert!(lines[1].runs.is_empty(), "the middle line should be empty");
        assert!(
            lines[1].rect.height > 0.0,
            "an empty line still consumes height"
        );
    }

    #[test]
    fn text_align_left_is_the_default_and_does_not_shift_runs() {
        let (_, spans, styles) = spans_for("hi", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        assert_eq!(lines[0].runs[0].x_offset, 0.0);
    }

    #[test]
    fn text_align_right_pushes_the_line_to_the_right_edge() {
        let (_, spans, styles) = spans_for("hi", "p { text-align: right; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let content_width = lines[0].rect.width;
        assert_eq!(lines[0].runs[0].x_offset, 500.0 - content_width);
    }

    #[test]
    fn text_align_center_splits_the_leftover_space_evenly() {
        let (_, spans, styles) = spans_for("hi", "p { text-align: center; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let content_width = lines[0].rect.width;
        assert_eq!(lines[0].runs[0].x_offset, (500.0 - content_width) / 2.0);
    }

    #[test]
    fn text_align_justify_spreads_extra_space_across_word_gaps_but_not_on_the_last_line() {
        let (_, spans, styles) = spans_for("hello world foo bar baz", "p { text-align: justify; }");
        let fonts = dejavu_only();
        // Narrow the width to force wrapping onto several lines.
        let lines = layout_inline_content(&spans, &styles, &fonts, 150.0, 0.0, 0.0, None);
        assert!(lines.len() >= 2, "expected wrapping to at least 2 lines");

        // Every line but the last should be stretched to exactly the line width (available_width).
        for line in &lines[..lines.len() - 1] {
            let text: String = line.runs.iter().map(|r| r.text.as_str()).collect();
            assert!(
                text.contains(' '),
                "a justified non-last line needs at least one word gap to stretch"
            );
            assert_eq!(
                line.rect.width, 150.0,
                "non-last justified lines should stretch to fill the available width"
            );
        }

        // The last line does not stretch (rect.width stays the width actually used, short of 150).
        let last = lines.last().unwrap();
        assert!(
            last.rect.width < 150.0,
            "the last line should not be stretched by justify"
        );
    }

    #[test]
    fn text_align_justify_does_not_push_text_over_an_inline_block_on_the_same_line() {
        // When "aa bb [box] cc" is followed by a long word that wraps, the first line is
        // stretched by justify. Distributing the slack only at the word boundaries (bb, cc)
        // left the box behind, and the bb shifted right overlapped it.
        let (_, spans, styles) = spans_for(
            r#"aa bb <input style="width: 40px;"> cc dddddddddddddddddddddd"#,
            "p { text-align: justify; }",
        );
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 180.0, 0.0, 0.0, None);
        assert!(lines.len() >= 2, "expected the long word to wrap");
        let line = &lines[0];
        assert_eq!(line.atomics.len(), 1, "the box should be on the first line");
        assert_eq!(line.rect.width, 180.0, "the first line should be justified");

        let atomic = &line.atomics[0];
        let box_left = atomic.x_offset;
        let box_right = atomic.x_offset + atomic.margin_box_width;
        for run in &line.runs {
            let run_right = run.x_offset + run.width;
            assert!(
                run_right <= box_left + 0.01 || run.x_offset >= box_right - 0.01,
                "run {:?} at [{}, {}] overlaps the box at [{}, {}]",
                run.text,
                run.x_offset,
                run_right,
                box_left,
                box_right
            );
        }
        // The gap between the run just before the box ("aa bb", adjacent runs already merged)
        // and the one just after ("cc") widens like any other inter-word gap (it is not just
        // one side of the box that widens).
        let before = line
            .runs
            .iter()
            .filter(|r| r.x_offset < box_left)
            .max_by(|a, b| a.x_offset.total_cmp(&b.x_offset))
            .expect("a run before the box");
        let after = line
            .runs
            .iter()
            .filter(|r| r.x_offset >= box_right - 0.01)
            .min_by(|a, b| a.x_offset.total_cmp(&b.x_offset))
            .expect("a run after the box");
        let gap_before = box_left - (before.x_offset + before.width);
        let gap_after = after.x_offset - box_right;
        assert!(
            (gap_before - gap_after).abs() < 0.01,
            "expected equal gaps around the box, got before={gap_before} after={gap_after}"
        );
    }

    #[test]
    fn text_align_justify_does_not_open_a_gap_around_a_box_written_without_spaces() {
        // `aaa<input>bbb` has no whitespace and so is not a word boundary either. Counting it
        // as stretchable would visibly open a gap only between the box and its neighbours.
        let (_, spans, styles) = spans_for(
            r#"aaa<input style="width: 40px;">bbb ccc dddddddddddddddddddddd"#,
            "p { text-align: justify; }",
        );
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 180.0, 0.0, 0.0, None);
        assert!(lines.len() >= 2, "expected the long word to wrap");
        let line = &lines[0];
        assert_eq!(line.rect.width, 180.0, "the first line should be justified");
        let atomic = &line.atomics[0];
        let box_left = atomic.x_offset;
        let box_right = atomic.x_offset + atomic.margin_box_width;

        let before = line
            .runs
            .iter()
            .filter(|r| r.x_offset < box_left)
            .max_by(|a, b| a.x_offset.total_cmp(&b.x_offset))
            .expect("a run before the box");
        let after = line
            .runs
            .iter()
            .filter(|r| r.x_offset >= box_right - 0.01)
            .min_by(|a, b| a.x_offset.total_cmp(&b.x_offset))
            .expect("a run after the box");
        assert!(
            (box_left - (before.x_offset + before.width)).abs() < 0.01,
            "expected `aaa` to touch the box, got a gap of {}",
            box_left - (before.x_offset + before.width)
        );
        assert!(
            (after.x_offset - box_right).abs() < 0.01,
            "expected `bbb` to touch the box, got a gap of {}",
            after.x_offset - box_right
        );
    }

    #[test]
    fn text_align_justify_with_a_single_word_line_does_not_panic_or_shift() {
        // A line with no word boundary (a single word) does not stretch under justify
        let (_, spans, styles) = spans_for(
            "supercalifragilisticexpialidocious",
            "p { text-align: justify; }",
        );
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 10.0, 0.0, 0.0, None);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].runs[0].x_offset, 0.0);
    }

    #[test]
    fn text_indent_px_shifts_only_the_first_line() {
        let (_, spans, styles) = spans_for("hello world foo bar", "p { text-indent: 30px; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 60.0, 0.0, 0.0, None);

        assert!(lines.len() >= 2, "expected wrapping to at least 2 lines");
        assert_eq!(lines[0].rect.x, 30.0);
        assert_eq!(lines[1].rect.x, 0.0, "second line should not be indented");
    }

    #[test]
    fn text_indent_percentage_resolves_against_available_width() {
        let (_, spans, styles) = spans_for("hi", "p { text-indent: 10%; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        assert_eq!(lines[0].rect.x, 50.0);
    }

    #[test]
    fn text_indent_applies_to_the_first_physical_line_of_pre_content() {
        let (_, spans, styles) = spans_for(
            "hello&#10;world",
            "p { white-space: pre; text-indent: 15px; }",
        );
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].rect.x, 15.0);
        assert_eq!(lines[1].rect.x, 0.0);
    }

    #[test]
    fn inline_span_color_and_style_are_carried_onto_the_text_run() {
        let (_, spans, styles) = spans_for(
            r#"plain <em style="color: rgb(200, 0, 0);">urgent</em>"#,
            "",
        );
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        let plain_run = lines[0]
            .runs
            .iter()
            .find(|r| r.text == "plain")
            .expect("plain run not found");
        assert!(!plain_run.italic);
        assert_eq!(plain_run.color, ComputedStyle::default().color);

        let urgent_run = lines[0]
            .runs
            .iter()
            .find(|r| r.text == "urgent")
            .expect("urgent run not found");
        assert!(urgent_run.italic, "<em> should render in italic");
        assert_eq!(
            urgent_run.color,
            RgbaColor {
                red: 200,
                green: 0,
                blue: 0,
                alpha: 1.0
            }
        );
    }

    /// Concatenate and return each line's text (for the forced break tests; an empty line gives an empty string).
    fn line_texts(lines: &[LineBox]) -> Vec<String> {
        lines
            .iter()
            .map(|line| line.runs.iter().map(|r| r.text.as_str()).collect())
            .collect()
    }

    #[test]
    fn br_breaks_the_line_even_when_the_text_would_fit() {
        let (_, spans, styles) = spans_for("hello<br>world", "");
        let fonts = dejavu_only();
        // It breaks even at an ample line width.
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);

        assert_eq!(line_texts(&lines), vec!["hello", "world"]);
        assert!(
            lines[1].rect.y > lines[0].rect.y,
            "the second line must be placed below the first"
        );
    }

    #[test]
    fn br_breaks_even_with_white_space_nowrap() {
        let (_, spans, styles) = spans_for("hello<br>world", "p { white-space: nowrap; }");
        let fonts = dejavu_only();
        // `nowrap` only stops wrapping by width; a forced break still applies.
        let lines = layout_inline_content(&spans, &styles, &fonts, 10.0, 0.0, 0.0, None);
        assert_eq!(line_texts(&lines), vec!["hello", "world"]);
    }

    #[test]
    fn consecutive_brs_produce_an_empty_line() {
        let (_, spans, styles) = spans_for("a<br><br>b", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);

        assert_eq!(line_texts(&lines), vec!["a", "", "b"]);
        assert!(
            lines[1].rect.height > 0.0,
            "the blank line must still take vertical space"
        );
        assert_eq!(lines[1].rect.y, lines[0].rect.y + lines[0].rect.height);
        assert_eq!(lines[2].rect.y, lines[1].rect.y + lines[1].rect.height);
    }

    #[test]
    fn a_trailing_br_leaves_one_empty_line() {
        // The same behaviour as the major browsers.
        let (_, spans, styles) = spans_for("a<br>", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);

        assert_eq!(line_texts(&lines), vec!["a", ""]);
        assert!(lines[1].rect.height > 0.0);
    }

    #[test]
    fn a_leading_br_pushes_the_text_down_by_one_line() {
        let (_, spans, styles) = spans_for("<br>a", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);

        assert_eq!(line_texts(&lines), vec!["", "a"]);
        assert_eq!(lines[1].rect.y, lines[0].rect.height);
    }

    #[test]
    fn br_does_not_swallow_the_surrounding_words() {
        // A newline also acts as a word separator, so the words either side are not joined.
        let (_, spans, styles) = spans_for("one two<br>three four", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);
        assert_eq!(line_texts(&lines), vec!["one two", "three four"]);
    }

    #[test]
    fn br_inside_pre_also_breaks_the_line() {
        // `white-space: pre` takes a separate path (`layout_pre_content`), but a `<br>` rides
        // on the span as `'\n'` and becomes a break with no changes.
        let (_, spans, styles) = spans_for("a<br>b", "p { white-space: pre; }");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);
        assert_eq!(line_texts(&lines), vec!["a", "b"]);
    }

    #[test]
    fn the_empty_line_of_a_br_uses_its_own_line_height() {
        let (_, spans, styles) = spans_for(
            "a<br><br>b",
            "p { font-size: 10px; } br { font-size: 40px; line-height: 2; }",
        );
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);

        assert_eq!(line_texts(&lines), vec!["a", "", "b"]);
        assert_eq!(
            lines[1].rect.height, 80.0,
            "the blank line takes the <br>'s own line-height (40px * 2)"
        );
    }

    #[test]
    fn br_clear_pushes_the_next_line_below_a_float() {
        // `<br clear="left">` has the legacy presentational attribute converted to
        // `clear: left`, pushing the line after the forced break down past the float's bottom.
        use crate::layout::float_ctx::FloatContext;
        use crate::style::Float;

        let (_, spans, styles) = spans_for("a<br clear=\"left\">b", "");
        let fonts = dejavu_only();
        let mut ctx = FloatContext::new();
        ctx.register(Float::Left, 0.0, 0.0, 50.0, 100.0);

        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, Some(&ctx));
        assert_eq!(line_texts(&lines), vec!["a", "b"]);
        assert!(
            lines[1].rect.y >= 100.0,
            "the line after <br clear=left> must clear the float, got y={}",
            lines[1].rect.y
        );
    }

    // ===== `vertical-align` (inline context) =====

    /// Return each run in the line as `(text, offset from the baseline)`.
    fn run_shifts(line: &LineBox) -> Vec<(String, f32)> {
        line.runs
            .iter()
            .map(|r| (r.text.clone(), r.baseline_shift))
            .collect()
    }

    #[test]
    fn baseline_is_the_default_and_shifts_nothing() {
        let (_, spans, styles) = spans_for("plain <span>text</span>", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert!(lines[0].runs.iter().all(|r| r.baseline_shift == 0.0));
        assert!(lines[0].baseline > 0.0 && lines[0].baseline < lines[0].rect.height);
    }

    #[test]
    fn a_line_without_vertical_align_keeps_its_previous_height_and_baseline() {
        // Regression check: the same values as before `finish_line` was rewritten (the floor rule).
        let (_, spans, styles) = spans_for("text", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let run = &lines[0].runs[0];
        let font = fonts.get(run.font_index).unwrap();

        assert_eq!(lines[0].rect.height, run.line_height);
        let expected = font.baseline_offset(run.font_size, run.line_height);
        assert!(
            (lines[0].baseline - expected).abs() < 0.01,
            "baseline {} should match Font::baseline_offset {}",
            lines[0].baseline,
            expected
        );
    }

    #[test]
    fn sup_raises_and_sub_lowers_the_run() {
        let (_, spans, styles) = spans_for("H<sub>2</sub>O<sup>3</sup>", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let shifts = run_shifts(&lines[0]);

        let sub = shifts.iter().find(|(t, _)| t == "2").expect("sub run");
        let sup = shifts.iter().find(|(t, _)| t == "3").expect("sup run");
        assert!(sub.1 < 0.0, "sub should be lowered: {sub:?}");
        assert!(sup.1 > 0.0, "super should be raised: {sup:?}");
        assert!(shifts.iter().find(|(t, _)| t == "H").unwrap().1 == 0.0);
    }

    #[test]
    fn a_raised_run_grows_the_line_box() {
        let fonts = dejavu_only();
        let (_, plain_spans, plain_styles) = spans_for("Hx", "");
        let plain =
            layout_inline_content(&plain_spans, &plain_styles, &fonts, 500.0, 0.0, 0.0, None);

        // Raising it far enough grows the line's height (`content_height`).
        let (_, spans, styles) = spans_for("H<span>x</span>", "span { vertical-align: 30px; }");
        let raised = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert!(
            raised[0].rect.height > plain[0].rect.height,
            "{} should exceed {}",
            raised[0].rect.height,
            plain[0].rect.height
        );
        assert!(raised[0].baseline > plain[0].baseline);
    }

    #[test]
    fn length_and_percentage_values_shift_by_the_specified_amount() {
        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for(
            "a<span class=\"px\">b</span><span class=\"pct\">c</span>",
            ".px { vertical-align: 5px; } .pct { vertical-align: 50%; line-height: 20px; }",
        );
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let shifts = run_shifts(&lines[0]);

        assert_eq!(shifts.iter().find(|(t, _)| t == "b").unwrap().1, 5.0);
        // A percentage is relative to that run's `line-height` (20px).
        assert_eq!(shifts.iter().find(|(t, _)| t == "c").unwrap().1, 10.0);
    }

    #[test]
    fn negative_length_lowers_the_run() {
        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for("a<span>b</span>", "span { vertical-align: -4px; }");
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let shifts = run_shifts(&lines[0]);
        assert_eq!(shifts.iter().find(|(t, _)| t == "b").unwrap().1, -4.0);
    }

    #[test]
    fn text_top_and_text_bottom_align_with_the_first_run() {
        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for(
            "big<span class=\"t\">t</span><span class=\"b\">b</span>",
            "p { font-size: 30px; } .t, .b { font-size: 10px; } \
             .t { vertical-align: text-top; } .b { vertical-align: text-bottom; }",
        );
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let shifts = run_shifts(&lines[0]);
        let small_run = lines[0].runs.iter().find(|r| r.text == "t").unwrap();
        let base_run = lines[0].runs.iter().find(|r| r.text == "big").unwrap();

        // The top of the smaller font's text coincides with the top of the reference run's text.
        let t_shift = shifts.iter().find(|(t, _)| t == "t").unwrap().1;
        assert!((t_shift - (base_run.ascent - small_run.ascent)).abs() < 0.01);
        // The bottoms of the text coincide (a shallower descent than the reference means it sits lower).
        let b_shift = shifts.iter().find(|(t, _)| t == "b").unwrap().1;
        assert!(
            b_shift < 0.0,
            "text-bottom should lower a smaller run: {b_shift}"
        );
    }

    #[test]
    fn top_and_bottom_align_with_the_line_box_edges() {
        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for(
            "big<span class=\"t\">t</span><span class=\"b\">b</span>",
            "p { font-size: 40px; } .t, .b { font-size: 10px; } \
             .t { vertical-align: top; } .b { vertical-align: bottom; }",
        );
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let line = &lines[0];
        let top_run = line.runs.iter().find(|r| r.text == "t").unwrap();
        let bottom_run = line.runs.iter().find(|r| r.text == "b").unwrap();

        // In line box coordinates (distance from the top, positive downwards) a run's baseline
        // is `line.baseline - baseline_shift` (`baseline_shift` being positive upwards).
        // A top-aligned run has the top of its text at the top of the line.
        let top_of_run = line.baseline - top_run.baseline_shift - top_run.ascent;
        assert!(top_of_run.abs() < 0.01, "expected 0, got {top_of_run}");
        // A bottom-aligned run has the bottom of its text at the bottom of the line.
        let bottom_of_run = line.baseline - bottom_run.baseline_shift + bottom_run.descent;
        assert!(
            (bottom_of_run - line.rect.height).abs() < 0.01,
            "expected {}, got {bottom_of_run}",
            line.rect.height
        );
    }

    #[test]
    fn middle_centers_the_run_around_the_x_height() {
        let fonts = dejavu_only();
        let (_, spans, styles) = spans_for(
            "big<span>m</span>",
            "p { font-size: 40px; } span { font-size: 10px; vertical-align: middle; }",
        );
        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);
        let run = lines[0].runs.iter().find(|r| r.text == "m").unwrap();
        let base = lines[0].runs.iter().find(|r| r.text == "big").unwrap();
        let x_height = fonts.get(base.font_index).unwrap().x_height(base.font_size);

        let center_of_run = run.baseline_shift + (run.ascent - run.descent) / 2.0;
        assert!(
            (center_of_run - x_height / 2.0).abs() < 0.01,
            "run center {center_of_run} should sit at half the x-height {}",
            x_height / 2.0
        );
    }
}
