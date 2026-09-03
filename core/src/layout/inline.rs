//! Inline Formatting Context: 単純な貪欲法によるテキストの行分割と行ボックスの配置。

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

/// `display: inline-block`のプレースホルダ文字(U+FFFC OBJECT REPLACEMENT
/// CHARACTER)。実際には描画されず、行組みが箱の位置を保つためだけに使う。
const ATOMIC_PLACEHOLDER: char = '\u{FFFC}';

/// `text-emphasis`のマーク1つ分の描画情報。マークの
/// サイズは`font-size`の半分(仕様の推奨値)。
#[derive(Debug, Clone, PartialEq)]
pub struct EmphasisMark {
    pub style: EmphasisStyle,
    pub color: RgbaColor,
    pub position: EmphasisPosition,
    /// マークの外接サイズ(px)。`font_size * 0.5`。
    pub size: f32,
}

/// `text-emphasis`のマークが行に要求する高さの、font-sizeに対する比率
/// (仕様の推奨値`0.5em`)。
const EMPHASIS_SIZE_RATIO: f32 = 0.5;

/// soft hyphen(U+00AD)。描画はせず、改行機会としてのみ扱う。
const SOFT_HYPHEN: char = '\u{00AD}';

/// 行末に表示するハイフン(soft hyphenで分割したとき)。
const HYPHEN: &str = "-";

/// `text-overflow: ellipsis`の省略記号(U+2026)。グリフを持たないフォントでは
/// ハイフンにフォールバックする。
const ELLIPSIS: &str = "…";

/// 同一スタイル・同一フォントで連続する区間(1単語の一部、または1単語全体)。
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    /// この区間の描画に使う、[`FontCollection`]内でのフォントのインデックス。
    pub font_index: usize,
    pub font_size: f32,
    pub color: RgbaColor,
    /// このランを囲む`<a href>`のhref値。PDF
    /// 層がこの値ごとに`/Link`注釈を作る。
    pub link: Option<Rc<str>>,
    /// このランの`background-color`。インライン要素の背景は、そのランの
    /// (ascent〜descentの)矩形として描画層が塗る(`<mark>`等)。
    pub background_color: RgbaColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub line_through: bool,
    /// この区間の元テキスト(`ShapedGlyph::cluster`から文字を逆引きするために保持する。
    /// PDF出力の`/ToUnicode`CMap生成で使う)。
    pub text: String,
    pub glyphs: Vec<ShapedGlyph>,
    /// 行ボックス(`LineBox::rect`)の左端からの相対x座標。
    pub x_offset: f32,
    pub width: f32,
    /// このランの計算済み行高さ(px)。`line-height: normal`はこのランのフォントの
    /// メトリクス由来、`<number>`はこのランの`font_size`で乗算済み。
    pub line_height: f32,
    /// `letter-spacing`の解決済みpx。PDF描画層(`pdf::document::render_line`)
    /// が`Tc`(character spacing)としてそのまま
    /// 使う(レイアウト側の幅計算にも反映済み)。
    pub letter_spacing: f32,
    /// `word-spacing`の解決済みpx。単語間gap計算専用(描画には使わない、
    /// PDFの`Tw`は複合フォントに効かないためgap幅への加算だけで実現)。
    pub word_spacing: f32,
    /// このランのフォント・サイズにおけるアセント(px、ベースラインより上)。
    /// 行ボックスの高さ・ベースライン位置の算出に使う。
    pub ascent: f32,
    /// 同じくディセント(px、ベースラインより下、正の値)。
    pub descent: f32,
    /// `vertical-align`によるベースラインからのずれ(px、正=上方向)。
    /// 描画層は`line.baseline`にこの値を加減して各ランのベースラインを求める。
    pub baseline_shift: f32,
    /// `text-shadow`(継承済み・色解決済み)。描画層がテキスト本体の前に
    /// 重ね描きする。レイアウトには影響しない。空なら影なし。
    /// `text-shadow`。指定が無いのが普通なので`Option`にして、
    /// 空でも`Rc`を確保しないようにする(ランは文書全体で数十万個になる)。
    pub text_shadow: Option<Rc<[ComputedTextShadow]>>,
    /// `text-emphasis`のマーク。`None`ならマークなし。マーク分の高さは
    /// `ascent`/`descent`に加算済み。
    pub emphasis: Option<EmphasisMark>,
    /// このランのスタイルの、IFC内での位置(`span_styles`のインデックス)。
    /// 行組み中にハイフンや省略記号を同じスタイルで生成し直すために持つ。
    pub(super) style_index: usize,
    /// このランの直前がsoft hyphen由来の改行機会かどうか。ここで行が
    /// 分かれたら、前の行の末尾にハイフンを表示する。
    pub(super) hyphen_before: bool,
    /// このランの直前が改行機会かどうか(soft hyphen・ZWSP・`<wbr>`)。
    /// ハイフンを出すかどうかは`hyphen_before`が別に持つ。
    pub(super) break_before: bool,
    /// このランに適用された`vertical-align`の計算値(行の高さ確定後に
    /// `top`/`bottom`を後追いで解決するため、レイアウト中だけ使う)。
    pub(super) vertical_align: VerticalAlign,
}

#[derive(Debug, Clone)]
pub struct LineBox {
    pub rect: Rect,
    pub runs: Vec<TextRun>,
    /// 行ボックス上端からベースラインまでの距離(px)。`vertical-align`が絡むと
    /// ランごとにベースラインがずれるため、レイアウト時に確定させて保持する。
    pub baseline: f32,
    /// この行に置かれた`display: inline-block`のボックス。
    pub atomics: Vec<AtomicInline>,
}

/// 行に置かれたアトミックインラインボックス(`display: inline-block`)。
#[derive(Debug, Clone)]
pub struct AtomicInline {
    /// レイアウト済みの中身。座標は`layout::block`が行の位置確定後に補正する。
    pub content: LaidOutBox,
    /// 行ボックス左端からのx方向オフセット。
    pub x_offset: f32,
    /// マージンボックスの寸法(行送り・折り返し判定に使う)。
    pub margin_box_width: f32,
    pub margin_box_height: f32,
    /// `vertical-align`によるベースラインからのずれ(px、正=上)。
    pub baseline_shift: f32,
    /// このボックスの`vertical-align`(行の高さ確定後に`top`/`bottom`を
    /// 解決するために保持する)。
    pub(super) vertical_align: VerticalAlign,
}

/// 1文字とその文字が属する[`InlineSpan`](=計算スタイル)への参照。
#[derive(Debug, Clone, Copy)]
struct StyledChar {
    ch: char,
    style_index: usize,
    /// `display: inline-block`のプレースホルダなら、その`InlineSpan`の
    /// インデックス。`ch`はU+FFFC(OBJECT REPLACEMENT CHARACTER)。
    atomic_span: Option<usize>,
    /// `<br>`由来の強制改行文字かどうか。`ch`は`'\n'`。`white-space: pre`の
    /// 経路はこのフラグを見ずに`'\n'`だけで行を分割するため、`<pre>`内の
    /// `<br>`も自然に改行になる。
    is_forced_break: bool,
    /// この文字の直前にsoft hyphen(U+00AD)があったか。soft hyphen自身は
    /// 描画されないため文字列からは取り除き、改行機会としてこのフラグに
    /// 変換する。ここで行が分かれた場合は行末にハイフンを表示する。
    hyphen_before: bool,
    /// この文字の直前が改行機会かどうか(soft hyphenとZWSP・`<wbr>`)。
    ///
    /// `hyphen_before`と分けているのは、ハイフンを表示するかどうかが違うため。
    /// ZWSPは「幅ゼロの改行機会」でしかないので、グリフを1つも出さずにこの
    /// フラグへ畳む。文字として残すと、フォントがZWSPのグリフを持たない場合に
    /// spaceのグリフで代替され、`/ToUnicode`で普通の空白と衝突してPDFの
    /// テキスト抽出が壊れる(空白がU+200Bとして取り出される)。
    break_before: bool,
}

/// 通常フロー(`white-space: normal`/`nowrap`)の行組みの入力単位。
enum InlineItem<'a> {
    Word {
        chars: &'a [StyledChar],
        /// 直前に空白があったか(単語間スペースを入れるかの判定)。
        space_before: bool,
    },
    /// `display: inline-block`の箱。
    /// `span_index`は`InlineSpan`のインデックス。
    Atomic {
        span_index: usize,
        space_before: bool,
    },
    /// `<br>`由来の強制改行。`style_index`は`<br>`要素自身のスタイル
    /// (空行の高さ算出に使う)。
    ForcedBreak { style_index: usize },
}

/// `spans`(テキストノード単位の区間列)を`available_width`に収まるよう行分割し、
/// `(origin_x, origin_y)`を起点に縦に積んだ行ボックス列を返す。単語の途中で
/// スタイル(`<b>`等)やフォント(CSSの`font-family`フォールバック)が切り替わる
/// 場合は、その単語を複数の[`TextRun`]に分けてシェイピングする。
///
/// `float_ctx`が`Some`の場合、各行の開始時点でその行のY位置におけるfloat
/// 占有帯を問い合わせ、`available_width`/`origin_x`を動的に狭める(float周りの
/// テキスト回り込み)。`None`(floatが無い、またはテーブル列幅の事前測定など
/// 無関係な呼び出し)なら固定の`available_width`/`origin_x`のまま(既存動作)。
///
/// `container_style`はこのIFCを確立するブロックコンテナの計算スタイル
/// (無名ボックスや採寸パスなら`None`)。`text-align`はブロックコンテナに
/// 適用されるプロパティなので、行内のインラインボックスの値ではなく
/// ここから読む。
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
    // `text-align`/`text-indent`/`white-space`はIFC内の先頭spanの計算値で
    // 代表する(無名ボックスのbox_style欠陥を回避する設計)。ただし
    // `display: inline-block`のスパンは除外する:箱は独自のIFCを内側に
    // 持つため、その`white-space`等が親の行組みを支配してはいけない(UA
    // スタイルシートが`input`に付ける`white-space: pre`が段落全体をpre
    // 扱いにしてしまう)。箱しか無いIFC(`<p><input></p>`等)ではテキスト由来の
    // 代表値が存在しないため、初期値
    // (`white-space: normal`/`text-indent: 0`)を使う
    let representative = spans
        .iter()
        .position(|span| span.atomic.is_none())
        .and_then(|i| span_styles.get(i));
    let white_space = representative.map(|s| s.white_space).unwrap_or_default();
    // `text-align`はブロックコンテナに適用されるプロパティなので、
    // インラインの代表値ではなくコンテナの計算値を優先する。
    // `<div style="text-align: right"><img></div>`のロゴが右に寄り(issue #19)、
    // `<div style="text-align: right"><span style="text-align: left">WORD</span></div>`
    // で先頭spanの値が勝ってしまう既存の不具合も直る。コンテナが無い
    // (無名ボックスや採寸パス)場合だけ代表値にフォールバックする。
    let text_align = container_style
        .map(|s| s.text_align)
        .or_else(|| representative.map(|s| s.text_align))
        .unwrap_or_default();
    // `word-break`/`overflow-wrap`も`white-space`と同じくIFCの代表値で扱う。
    let word_break = representative.map(|s| s.word_break).unwrap_or_default();
    let overflow_wrap = representative.map(|s| s.overflow_wrap).unwrap_or_default();
    // パーセンテージはこのIFCのcontaining width(`available_width`)基準で解決する
    // (`width`/`margin`と同じ「使用値は使う側で解決」パターン)。
    let text_indent = representative
        .map(|s| resolve_length_percentage(s.text_indent, available_width))
        .unwrap_or(0.0);
    if white_space == WhiteSpace::Pre {
        // `white-space: pre`の経路は`display: inline-block`の箱を扱えない
        // (既知の限界)。プレースホルダ文字がそのままグリフとして描かれるのを
        // 防ぐため、ここで取り除く。
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
    // 現在組み立て中の行における、単語境界の位置(行ボックス左端からのx、
    // 単語間スペースを入れる前の`current_width`)。`text-align: justify`が
    // ここに追加スペースを配分する(行頭に来た単語は境界として記録しない、
    // 既存の行の左端そのものだから)。テキストランと`<img>`等の箱はそれぞれ
    // `current_runs`/`current_atomics`に分かれて積まれるため、両方に同じ
    // 規則で適用できるようインデックスではなくxで持つ。
    let mut word_boundaries: Vec<f32> = Vec::new();
    // 直前のアイテムが強制改行だった場合の、その`<br>`が要求する行高さ。
    // 末尾の`<br>`に対して空行を1つ足すために使う。
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

                // 空行の場合は先に帯を引く(通常の単語と同じ手順)。
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

                // 行に収まらなければ先に改行する(箱自体は分割しない)。
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
                    // 箱の前の空白も`justify`の伸縮対象(テキストランと同じ)。
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
                // 強制改行は行幅の残りに関係なく行を確定させる
                // (`white-space: nowrap`でも効く)。
                let break_height = span_styles
                    .get(style_index)
                    .map(|style| empty_line_height(style, fonts))
                    .unwrap_or(0.0);
                if current_runs.is_empty() && current_atomics.is_empty() {
                    // 行に何も無い状態での強制改行(連続する`<br>`や段落先頭の
                    // `<br>`)は、高さだけを持つ空行になる。
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
                    // 強制改行で終わる行は最終行と同じ扱いで、`justify`の
                    // 伸縮対象にしない。
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
                // `<br clear="left|right|all">`(レガシー表示属性が`clear`
                // プロパティに変換されている)。CSSで
                // `br { clear: both }`と書いた場合も同じ経路。
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

        // 単語内であっても、CJK文字が絡む改行可能な境界ごとに「まとめて
        // 1行に収まるか判定する最小単位」(chunk)へグループ化する。空白による
        // 単語区切りは常に改行可能(次段の`is_first_chunk_of_word`で扱う)。
        // 要素は`(chunk, 単語の先頭chunkか, overflow-wrapの文字分割を試みてよいか)`。
        // 3つ目は「1文字も入らないので分割を諦めた」chunkを再投入したときに
        // 無限ループへ入らないためのフラグ。
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
                // 新しい行の先頭: floatに応じた帯を、このchunkの先頭ランの
                // 行高さ(`line_height_for`と同じ計算値)で問い合わせる
                // (既知の簡略化: 行内でフォントサイズが極端に混在する場合は
                // 帯判定がわずかに不正確になり得るが、帳票用途では稀)。
                let hint = line_height_hint_for_chunk(&chunk);
                (line_left, line_available_width) =
                    line_band(float_ctx, cursor_y, hint, origin_x, available_width);
                // `text-indent`は最初の物理行のみに適用する(CSS2.1 §16.1)。
                if lines.is_empty() {
                    line_left += text_indent;
                    line_available_width -= text_indent;
                }
            }

            // 単語の先頭のchunkにのみ、直前のランとの間に単語間スペースを
            // 挟む。単語内のCJK境界で分かれた後続chunkは隙間0で直接続ける。
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

            // `overflow-wrap: break-word`: 行頭に置いてもなお収まらない
            // chunkは、収まるところまで文字単位で切って改行する。1文字も
            // 入らない場合(極端に狭い帯)は無限ループを
            // 避けるためそのまま置いてはみ出させる。
            if starting_new_line
                && allow_break_fallback
                && overflow_wrap == OverflowWrap::BreakWord
                && white_space != WhiteSpace::Nowrap
                && chunk_width > line_available_width
            {
                let (head, rest) = split_chunk_to_fit(chunk, line_available_width);
                if !head.is_empty() && !rest.is_empty() {
                    // 前半を「これ以上分割しない」chunkとして先に処理し、
                    // 残りは次の行で改めて分割判定にかける。
                    chunk_queue.push_front((rest, false, true));
                    chunk_queue.push_front((head, is_first_chunk_of_word, false));
                } else {
                    // 1文字も入らない(極端に狭い帯)。分割を諦めてそのまま置く。
                    let restored: Vec<TextRun> = head.into_iter().chain(rest).collect();
                    chunk_queue.push_front((restored, is_first_chunk_of_word, false));
                }
                continue;
            }

            if !starting_new_line
                && white_space != WhiteSpace::Nowrap
                && current_width + gap_width + chunk_width > line_available_width
            {
                // soft hyphenの位置で改行する場合、
                // 確定する行の末尾にハイフンを表示する。
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
                // 折返しで確定した行にtext-alignを適用する。最後の行ではない
                // ため`justify`も伸縮対象になる(CSS仕様: 最後の行は伸縮しない)。
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

                // 改行したので、このchunkを「行頭に置く」ケースとして評価し直す。
                // こうしないと`overflow-wrap: break-word`の文字分割が2行目以降で
                // 効かない(行頭判定を通らないまま置かれてしまう)。
                chunk_queue.push_front((chunk, is_first_chunk_of_word, allow_break_fallback));
                continue;
            } else if !starting_new_line {
                // 実際に空白がある位置だけが伸縮対象。`aaa<input>bbb`のように
                // 空白が無い単語の切れ目(`gap_width == 0`)を境界に数えると、
                // 箱と単語の間にだけ隙間が空いてしまう。
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

    // 行にテキストが1つも無く`display: inline-block`の箱だけが載っている場合も
    // 行として確定させる(`current_runs`だけを見ていると`<p><input></p>`のよう
    // な行がまるごと捨てられる)。
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
        // 最後の行は`justify`で伸縮しない(CSS仕様)。
        apply_text_align(
            lines.last_mut().expect("just pushed"),
            text_align,
            true,
            line_available_width,
            &word_boundaries,
        );
    } else if let Some(break_height) = trailing_break_height {
        // 末尾の`<br>`は1行分の空行を残す(主要ブラウザと同じ挙動)。
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

    // `text-align`の適用まで終わってから、同じ体裁のランをまとめる。
    for line in &mut lines {
        merge_adjacent_runs(line, fonts);
        // `Vec`は最初のpushで最小4要素分を確保するため、1行1ランの箱でも
        // 4つ分を抱えたままになる。行やランは文書全体で数十万個になり、
        // この余剰がレイアウトのメモリの2割前後を占めるので切り詰める。
        line.runs.shrink_to_fit();
        line.atomics.shrink_to_fit();
    }
    lines.shrink_to_fit();

    lines
}

/// 同じ体裁で横に連続するランを1つにまとめる。
///
/// 行組みは単語ごとにランを作るので、1段落が7ラン前後に分かれる。ランは
/// 1つあたり構造体192バイトに加えてテキストとグリフ列の確保を伴うため、
/// 数万段落の文書ではこの分割がレイアウトのメモリの大半を占める。
///
/// 単語間の空白はランに含まれず「隙間」として表現されているので、まとめる際は
/// 隙間ぶんのアドバンスを持つ空白グリフを差し込んで復元する。こうすると描画位置は
/// 元のまま変わらず、PDFのテキスト抽出では単語が空白で区切られるようになる。
///
/// `text-align: justify`が広げた隙間も同じ扱いでよい(隙間の実測値をそのまま
/// 空白のアドバンスにする)ため、この処理は`apply_text_align`の後に呼ぶこと。
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

/// `prev`の直後に`next`をまとめられるなら、2つの間の隙間(px)を返す。
fn gap_if_mergeable(prev: &TextRun, next: &TextRun, fonts: &FontCollection) -> Option<f32> {
    // 体裁が1つでも違えば別のランのままにする。
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
    // `letter-spacing`はPDFの`Tc`として全グリフに効くため、差し込んだ空白にも
    // 加算されて位置がずれる。指定がある行はまとめない。
    if prev.letter_spacing != 0.0 || next.letter_spacing != 0.0 {
        return None;
    }
    // マーク(`text-emphasis`)は文字ごとに打つので、空白の追加で数が変わりうる。
    if prev.emphasis.is_some() || next.emphasis.is_some() {
        return None;
    }
    // 行末ハイフンの直後は、ハイフンの有無が失われるためまとめない。
    if next.hyphen_before {
        return None;
    }

    let gap = next.x_offset - (prev.x_offset + prev.width);
    // 重なっている(負の隙間)場合は素直に諦める。
    if gap < -0.01 {
        return None;
    }
    let gap = gap.max(0.0);
    // 隙間を空白グリフで埋められないフォントではまとめない。
    if gap > 0.01 && space_glyph(prev.font_index, fonts).is_none() {
        return None;
    }
    Some(gap)
}

/// `font_index`のフォントが持つ空白(U+0020)のグリフID。
fn space_glyph(font_index: usize, fonts: &FontCollection) -> Option<u16> {
    fonts.get(font_index).and_then(|font| font.glyph_id(' '))
}

/// `prev`の末尾へ`next`を連結する。`gap`が正なら空白グリフで埋める。
fn append_run(prev: &mut TextRun, next: TextRun, gap: f32, fonts: &FontCollection) {
    if gap > 0.01 {
        let glyph_id = space_glyph(prev.font_index, fonts).expect("直前に存在を確認済み");
        prev.glyphs.push(ShapedGlyph {
            glyph_id,
            cluster: prev.text.len() as u32,
            x_advance: gap,
            x_offset: 0.0,
            y_offset: 0.0,
        });
        prev.text.push(' ');
    }
    // クラスタは各ランのテキスト先頭からのバイト位置なので、連結後の位置へずらす。
    let base = prev.text.len() as u32;
    prev.glyphs.extend(next.glyphs.into_iter().map(|mut glyph| {
        glyph.cluster += base;
        glyph
    }));
    prev.text.push_str(&next.text);
    prev.width = next.x_offset + next.width - prev.x_offset;
}

/// 確定した行に`text-align`を適用する。`is_last_line`は`justify`が最後の行を
/// 伸縮しない(CSS仕様)ための判定。`word_boundaries`は行内の単語境界の位置
/// (行ボックス左端からのx、単語間スペースの手前)で、`justify`がそこに追加
/// スペースを配分する。
///
/// 行の中身はテキストラン(`line.runs`)と`<img>`/`display: inline-block`の箱
/// (`line.atomics`)に分かれて持たれているので、どの分岐も両方をずらす。
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
            // ランも箱も、自分より左にある境界の数ぶんだけ右へずれる
            // (境界は各単語の直前の空白の手前にあるので、単語の先頭ランは
            // 自分の境界を含めて数える)。
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

/// 行の中身(テキストランとアトミックインラインボックス)をまとめて右へ`shift`px
/// ずらす。`<img>`や`display: inline-block`の箱は`line.runs`ではなく
/// `line.atomics`に載っているので、ランだけをずらすと箱が左端に取り残される
/// (issue #19)。
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

/// `chunk`最初のランの計算済み`line_height`で行高さを近似する(帯を問い合わせる
/// 時点ではまだ行全体のランが確定していないため)。
fn line_height_hint_for_chunk(chunk: &[TextRun]) -> f32 {
    chunk.first().map(|r| r.line_height).unwrap_or(0.0)
}

/// `float_ctx`があれば`y`〜`y+height`の帯を問い合わせ、無ければ固定の
/// `(origin_x, available_width)`を返す。
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

/// `spans`を1文字単位に展開し、各文字が元のどの[`ComputedStyle`]に属するかの
/// インデックスを付与する。`span_styles`は文字と対になるスタイルの実体。
/// `text-transform`はここで適用する(単語分割前の1パスで完結させる)。
fn flatten_spans(
    spans: &[InlineSpan],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
) -> (Vec<StyledChar>, Vec<ComputedStyle>, Vec<Option<Rc<str>>>) {
    let mut chars = Vec::new();
    let mut span_styles = Vec::with_capacity(spans.len());
    // スパンと同じインデックスで引く`<a href>`。CSSプロパティではないため
    // `ComputedStyle`には載せない。
    let mut span_links: Vec<Option<Rc<str>>> = Vec::with_capacity(spans.len());
    // spanを跨いでも語頭判定を継続する(先頭は語頭扱い)。
    let mut prev_is_boundary = true;
    // 直前にsoft hyphenがあったか。
    //
    // 改行機会は「次に来る文字」へ持ち越すため、spanの外で持つ必要がある。
    // `<wbr>`は要素なので必ず単独のspanになり、span内に閉じた変数では
    // 次のspanへ渡らない(`<span>foo&shy;</span><span>bar</span>`も同様)。
    let mut hyphen_pending = false;
    // 直前に改行機会(soft hyphen・ZWSP)があったか。
    let mut break_pending = false;

    for span in spans {
        // スパンごとに背景色などを差し替えるため、共有スタイルから所有スタイルを作る。
        let mut style = styles
            .get(&span.node)
            .map(|shared| (**shared).clone())
            .unwrap_or_default();
        // インライン背景はスパンが持つ値(=直近のインライン要素の指定)を使う。
        // テキストノードの計算スタイルは親の非継承プロパティまでクローンして
        // いるため、そのまま使うとブロックの背景まで塗ってしまう
        // (`box_tree::collect_spans_with_background`のコメント参照)。
        style.background_color = span.background_color;
        if span.is_first_letter {
            apply_first_letter_style(&mut style);
        }
        let style_index = span_styles.len();
        let transform = style.text_transform;
        span_styles.push(style);
        span_links.push(span.link.clone());

        if span.atomic.is_some() {
            // `display: inline-block`は文字列を持たないため、位置を保つための
            // プレースホルダを1つだけ置く。
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
            // soft hyphen(U+00AD)自身は描画しない。`hyphens: manual`(初期値)
            // なら改行機会として次の文字へ引き継ぎ、`none`なら単に捨てる。
            if ch == SOFT_HYPHEN {
                hyphen_pending = hyphens == Hyphens::Manual;
                break_pending |= hyphen_pending;
                continue;
            }
            // ZWSP(`<wbr>`の実体)も描画しない。幅ゼロの改行機会でしかないため、
            // グリフを出さずにフラグへ畳む(詳細は`StyledChar::break_before`)。
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

/// `style.first_letter_style`(あれば)で対応するプロパティのみを上書きする。
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

/// `text-transform`を1文字に適用する。`uppercase`/`lowercase`は
/// `char::to_uppercase()`等の最初の1文字のみ採用する(独語ß等の複数文字展開は
/// 非対応)。`capitalize`は語頭の文字のみ変換する。
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

/// 畳み込み対象の空白([`white_space::is_collapsible`])で単語分割しつつ、
/// `<br>`由来の強制改行を[`InlineItem::ForcedBreak`]として出現順に挟み込む。
/// 連続する空白は畳み込み、先頭・末尾の空白は無視する(強制改行は
/// 空白ではあるが畳み込まれず、常に1つのアイテムとして残る)。
///
/// `&nbsp;`やthin spaceは単語区切りにはならず、単語の一部として
/// [`split_word_into_runs`]へ渡る(畳み込まれず、フォント本来の字幅で描かれ、
/// 改行の可否は[`is_break_boundary`]が決める)。
fn split_into_items(chars: &[StyledChar]) -> Vec<InlineItem<'_>> {
    let mut items = Vec::new();
    let mut word_start = 0usize;
    // 直前に空白があったか(単語間スペースを入れるかの判定)。
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

/// 単語を、(スタイル, フォント)が連続する区間ごとに[`TextRun`]へ分割する。
/// CJK文字が絡む文字境界([`is_break_boundary`])では、スタイル/フォントが
/// 同じであっても別ランに分ける(改行可能な境界にするため。1文字ごとの
/// シェイピングになるが、CJK文字間の文脈依存シェイピングは通常無いため
/// 見た目には影響しない)。
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
    // 現在組み立て中のランの直前がsoft hyphen由来の改行機会かどうか。
    let mut current_hyphen_before = false;
    // 同じく、直前が改行機会(soft hyphen・ZWSP)かどうか。
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

/// `runs`を、改行可能な境界(先頭、またはCJK文字が絡むrun境界
/// [`is_break_boundary`])ごとに分割不可能な塊(chunk)へグループ化する。
/// 各chunkの内部境界はすべて改行不可(スタイル/フォント変更のみ)なので、
/// 呼び出し側はchunk単位で「まとめて1行に収まるか」を判定できる。
fn group_into_chunks(runs: Vec<TextRun>, word_break: WordBreak) -> Vec<Vec<TextRun>> {
    let mut chunks: Vec<Vec<TextRun>> = Vec::new();
    for run in runs {
        let starts_new_chunk = match chunks.last().and_then(|chunk| chunk.last()) {
            None => true,
            // soft hyphen・ZWSP(`<wbr>`)も改行機会。
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

/// `text-overflow: ellipsis`。行組みが終わった後に、`content_width`からはみ
/// 出した行を省略記号で切り詰める。`overflow`が`visible`、または
/// `text-overflow`が`clip`なら何もしない
/// (`clip`は既存の`overflow`クリップに委ねる)。
///
/// 既知の簡略化: 幅方向にはみ出した行のみを対象にする(ブロック全体の
/// オーバーフローは扱わない)。省略記号のグリフを持たないフォントでは
/// ハイフンにフォールバックする。
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

        // 省略記号の幅を確保した上で、収まるところまでランを残す。
        // 各ランの`x_offset`は行内の確定位置(単語間スペースを含む)なので
        // 書き換えない。累積幅で置き直すとスペース分ずれる。
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
        // 切り詰めた行の高さ・ベースラインは変えない(省略記号は同じスタイルで
        // シェイプしているため、行の`ascent`/`descent`に影響しない)。
        kept.push(ellipsis);
        line.runs = kept;
    }
}

/// 省略記号(`…`)のランを作る。フォントがそのグリフを持たない(`.notdef`)場合は
/// ハイフンへフォールバックし、それも無ければ`None`。
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

/// 行末にハイフンを追加する(soft hyphenで分割したとき)。直前のランと同じ
/// スタイル・フォントでシェイプする。
///
/// 既知の簡略化: ハイフン分の幅は「収まるかどうか」の判定には含めない
/// (判定後に足すため、行がハイフン1文字分だけはみ出しうる)。
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

/// chunkを`max_width`に収まる前半と残りに分割する
/// (`overflow-wrap: break-word`のフォールバック用)。
///
/// グリフ単位で切るため再シェイプは不要(`ShapedGlyph::cluster`が元テキストの
/// バイトオフセットを持つ)。合字・結合文字の途中で切れる可能性はあるが、
/// この経路に来るのは「1単語が行幅を超える」場合に限られる(既知の簡略化)。
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
        // `None`は全グリフが収まった場合(`run.width`との誤差)。通常来ない。
        if let Some(remainder) = remainder {
            rest.push(remainder);
        }
    }

    (head, rest)
}

/// 1つのランを、グリフ単位で`max_width`に収まる部分と残りに分割する。
/// どちらの側も空になり得る(`None`で表す)。
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

    // 分割位置の後半先頭グリフが指す元テキストのバイト位置で文字列も切る。
    let split_byte = (run.glyphs[glyph_count].cluster as usize).min(run.text.len());
    let mut head = run.clone();
    head.glyphs = run.glyphs[..glyph_count].to_vec();
    head.text = run.text[..split_byte].to_string();
    head.width = used;

    let mut tail = run.clone();
    // `cluster`は「そのランの`text`内のバイトオフセット」なので、後半では
    // 切った分だけ詰め直す。これを忘れると`text[cluster..]`で文字を逆引きする
    // 箇所(`/ToUnicode`生成・圏点描画)が範囲外アクセスでpanicする。
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
    // 後半は「単語の途中で切られた」だけなので、ハイフンは表示しない。
    tail.hyphen_before = false;
    tail.break_before = false;

    (Some(head), Some(tail))
}

/// `prev`と`next`の間で(空白が無くても)改行してよいかどうか。
/// `word-break: normal`ではどちらか一方がCJK文字([`is_cjk`])であれば
/// 改行可能とみなす簡略判定(UAX#14の全面実装ではない)。`break-all`はすべての
/// 文字境界、`keep-all`はCJK境界でも改行しない。
fn is_break_boundary(prev: char, next: char, word_break: WordBreak) -> bool {
    // `&nbsp;`等(UAX #14のGL・WJ)は前後の改行を禁止する。これは`word-break`
    // より優先する: 「10&nbsp;kg」を分断しないために置かれた文字なので、
    // `break-all`でも守るのが利用者の意図に合う(ブラウザも同様)。
    if white_space::is_non_breaking(prev) || white_space::is_non_breaking(next) {
        return false;
    }
    // thin space等(BA)とZWSP(ZW)の直後は改行してよい。
    if white_space::allows_break_after(prev) {
        return true;
    }
    match word_break {
        WordBreak::BreakAll => true,
        WordBreak::KeepAll => false,
        WordBreak::Normal => is_cjk(prev) || is_cjk(next),
    }
}

/// ひらがな・カタカナ・漢字(CJK統合漢字・拡張A・互換漢字)・ハングルなど、
/// 分かち書きをしない(単語間に空白を置かない)スクリプトの文字かどうか。
fn is_cjk(ch: char) -> bool {
    matches!(ch as u32,
        0x3000..=0x303F   // CJKの記号・句読点
        | 0x3040..=0x30FF // ひらがな・カタカナ
        | 0x31F0..=0x31FF // カタカナ拡張
        | 0x3400..=0x4DBF // CJK統合漢字拡張A
        | 0x4E00..=0x9FFF // CJK統合漢字
        | 0xAC00..=0xD7A3 // ハングル音節
        | 0xF900..=0xFAFF // CJK互換漢字
        | 0xFF00..=0xFFEF // 全角形・半角形
    )
}

/// `LengthPercentage`を`basis`(containing width)を使ってpxへ解決する
/// (`block.rs::resolve_lp`と同じロジック、`text-indent`専用にここへ複製する)。
fn resolve_length_percentage(lp: LengthPercentage, basis: f32) -> f32 {
    match lp {
        LengthPercentage::Length(px) => px,
        LengthPercentage::Percentage(fraction) => fraction * basis,
        LengthPercentage::Calc { px, percent } => px + percent * basis,
    }
}

/// `line-height: normal`をフォントメトリクスから求められない場合
/// (コレクションが空でフォントを1つも選べない)の倍率。実際に使われるのは
/// フォントを持たないテスト用の経路くらいで、通常の文書では
/// [`Font::normal_line_height`]が使われる。
const NORMAL_LINE_HEIGHT_FALLBACK: f32 = 1.2;

/// `line-height`の計算値からこの要素自身の`font_size`を使ってpx値を求める
/// (`Number`/`Normal`は使用側=ここでその要素のfont-sizeを使って乗算する)。
///
/// `normal`はフォントごとに異なる(アセント+ディセント+行間)ため、そのランで
/// 実際に使うフォントを渡す必要がある。`font`が`None`なら固定倍率で近似する。
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

/// `style`の`font-family`に対する「最初に使えるフォント」(CSSのfirst available
/// font)。テキストを持たない行(`<br>`だけの行や`white-space: pre`の空行)でも
/// `line-height: normal`を解決できるように、半角スペースを代表文字として選ぶ。
fn first_available_font<'a>(style: &ComputedStyle, fonts: &'a FontCollection) -> Option<&'a Font> {
    fonts
        .select_for_char(&style.font_family, style.font_weight, style.font_style, ' ')
        .and_then(|index| fonts.get(index))
}

/// テキストを持たない行の高さ(`line-height`の使用値)。
fn empty_line_height(style: &ComputedStyle, fonts: &FontCollection) -> f32 {
    resolve_line_height(style, first_available_font(style, fonts))
}

/// `white-space: pre`用のレイアウト。改行文字(`\n`)で明示的に行を分割し、
/// 連続する空白はそのまま保持する(畳み込まない、`split_into_words`を経由しない)。
/// 折り返しは行わない(`nowrap`と同様、既存の`layout_inline_content`本体とは
/// 別経路にすることでNormal/Nowrap側のリグレッションリスクを避ける)。
/// `split_word_into_runs`/`group_into_chunks`は変更せず再利用できるが、
/// `group_into_chunks`はCJK境界の改行可能判定用でpreでは折り返さないため
/// 使わず、`split_word_into_runs`の結果をそのまま1行に連結する。
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
        // 行高さの近似は、その行最初の文字のスタイル(無ければIFC先頭spanの
        // スタイル)を基準にする(既知の簡略化)。
        let hint = segment
            .first()
            .and_then(|sc| span_styles.get(sc.style_index))
            .or_else(|| span_styles.first())
            .map(|style| empty_line_height(style, fonts))
            .unwrap_or(0.0);
        let (mut line_left, _) = line_band(float_ctx, cursor_y, hint, origin_x, available_width);
        // `text-indent`は最初の物理行のみに適用する(CSS2.1 §16.1)。
        if lines.is_empty() {
            line_left += text_indent;
        }

        if segment.is_empty() {
            // 連続改行による空行。高さだけ消費するダミー行。
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

        // `white-space: pre`は折り返さないので、改行機会の判定(`word-break`)は
        // 結果に影響しない(常に`Normal`で呼ぶ)。
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

/// `list-style-type`のマーカーテキストのシェイピングにも使う
/// (`block.rs::layout_list_marker`)ため`pub(super)`。
pub(super) fn shape_run(
    text: &str,
    font_index: usize,
    fonts: &FontCollection,
    style: &ComputedStyle,
) -> TextRun {
    let font = fonts.get(font_index).expect("font_indexは常に有効な範囲");
    let font_size = style.font_size.0;
    let shaped = shape_text(font, text, font_size);
    // 選択されたフォントが実際にBold/Italicであれば、疑似合成は不要
    // (`fonts::FontCollection::select_for_char`が本物のBold/Italic面を優先して
    // 選ぶため、`--font`/`@font-face`/システムフォントに実体があればここで
    // 疑似合成をスキップできる)。
    let needs_synthetic_bold = style.font_weight == FontWeight::Bold && !fonts.is_bold(font_index);
    let needs_synthetic_italic =
        style.font_style == FontStyle::Italic && !fonts.is_italic(font_index);
    let mut line_height = resolve_line_height(style, Some(font));
    // `letter-spacing`はグリフ数分だけ幅に加算する(行末にも均等加算する
    // 既知の簡略化)。PDF描画層は`run.letter_spacing`を`Tc`として使う
    // ため、ここでの幅計算とレンダリング結果が一致する。
    let width = shaped.width + style.letter_spacing * shaped.glyphs.len() as f32;
    let units_per_em = font.units_per_em() as f32;
    let mut ascent = font.ascender() as f32 / units_per_em * font_size;
    let mut descent = -(font.descender() as f32) / units_per_em * font_size;

    // `text-emphasis`のマークは行ボックスの高さを増やす。`over`ならascent側、
    // `under`ならdescent側に`0.5em`を足す。
    let emphasis = (style.text_emphasis_style != EmphasisStyle::None).then(|| {
        let size = font_size * EMPHASIS_SIZE_RATIO;
        match style.text_emphasis_position {
            EmphasisPosition::Over => ascent += size,
            EmphasisPosition::Under => descent += size,
        }
        // 行ボックスの高さは`line-height`由来の値が下限になる
        // (`line_height_for`→`finish_line`)ため、マーク分はそちらにも足す。
        // こうしないと上下の行とマークが重なる。
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
        // `baseline_shift`は行が確定した時点で`resolve_baseline_shifts`が埋める。
        baseline_shift: 0.0,
        vertical_align: style.vertical_align,
        text_shadow: (!style.text_shadow.is_empty())
            .then(|| Rc::from(style.text_shadow.as_slice())),
        emphasis,
        // `style_index`/`hyphen_before`/`break_before`は呼び出し側
        // (`split_word_into_runs`)が設定する。単体で使う経路
        // (`shape_standalone_line`等)では既定値でよい。
        style_index: 0,
        hyphen_before: false,
        break_before: false,
    }
}

/// 任意の文字列を、折り返しなしの単一行として`(origin_x, origin_y)`起点で
/// シェイピングする。通常のDOMテキストノードを経由しない用途(`@page`のmargin
/// box)向け。文字ごとに`fonts.select_for_char`でフォントを選び直す
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

    // margin box用の単一行も通常の行と同じ経路でベースラインを確定させる
    // (`vertical-align`が効くわけではないが、`LineBox::baseline`を持たせる
    // 責務を1箇所にまとめるため)。
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

/// Width of the gap between two words, measured on the font of the word that
/// precedes it.
///
/// A font with no outlines is a colour-glyph-only font (Noto Color Emoji and
/// friends): it is never chosen as a text font, only for the one emoji that
/// needed it, and its space glyph is sized for emoji rather than for text
/// (Noto Color Emoji is monospaced at about 1.25em). Measuring with it would
/// leave a gap four times too wide after every emoji, so fall back to the
/// first font in the collection that actually draws text.
fn measure_space_width(fonts: &FontCollection, font_index: usize, font_size: f32) -> f32 {
    let font = match fonts.get(font_index) {
        Some(font) if font.has_outlines() => Some(font),
        _ => fonts.fonts().iter().find(|font| font.has_outlines()),
    };
    font.map(|font| measure_text(font, " ", font_size))
        .unwrap_or(0.0)
}

/// 行内の各ランの計算済み`line_height`のうち最大値を基準に行の高さを決める。
fn line_height_for(runs: &[TextRun]) -> f32 {
    runs.iter().map(|r| r.line_height).fold(0.0f32, f32::max)
}

/// 確定した行の`vertical-align`を解決し、行ボックスの高さとベースライン位置を
/// 求めて[`LineBox`]を組み立てる。
///
/// `height`は`line-height`プロパティ由来の高さ(`line_height_for`の結果)で、
/// 行ボックスの高さの下限として使う。`vertical-align`を使わない文書では
/// 高さもベースライン位置も従来と完全に一致する。
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
    // アトミックボックスはマージンボックスの下端をベースラインに合わせる。
    // つまりascent=マージンボックス高さ・descent=0として行に参加する。
    for atomic in atomics.iter_mut() {
        atomic.baseline_shift = match atomic.vertical_align {
            VerticalAlign::LengthPercentage(LengthPercentage::Length(px)) => px,
            VerticalAlign::LengthPercentage(LengthPercentage::Percentage(fraction)) => {
                height * fraction
            }
            // `sub`/`super`/`text-*`/`middle`は箱に対する厳密な定義が本
            // エンジンの簡略化と噛み合わないため、`baseline`扱い。
            _ => 0.0,
        };
    }

    // `line-height`だけで決まる(=`vertical-align`が無いときの)ベースライン位置。
    // 単一フォントの行では`Font::baseline_offset`と一致する。
    let mut baseline = 0.0f32;
    for run in &runs {
        let half_leading = (height - (run.ascent + run.descent)) / 2.0;
        baseline = baseline.max(half_leading + run.ascent);
    }
    let mut above = baseline;
    let mut below = height - baseline;

    // アトミックボックスは必ず行の高さに参加する(下端=ベースライン)。
    // テキストランと違い`top`/`bottom`でも除外しない: 箱しか無い行
    // (`<p><input></p>`や並べたカード)で行の高さが0になり、後続の内容と
    // 重なってしまうため。`top`/`bottom`の場合は「行の高さが箱の高さ以上」
    // でありさえすればよく、実際の位置は行の寸法確定後に決める。
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

    // ずらされたランだけを、行ボックスからはみ出す分について考慮する。
    // `baseline_shift`が0のランは行の高さに影響しないため、`vertical-align`を
    // 使わない文書の行の高さ・ベースラインは従来と完全に一致する。
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

    // 行ボックスの寸法が決まってはじめて解決できる値。
    for run in &mut runs {
        match run.vertical_align {
            VerticalAlign::Top => run.baseline_shift = baseline - run.ascent,
            VerticalAlign::Bottom => run.baseline_shift = -(line_height - baseline - run.descent),
            _ => {}
        }
    }
    // アトミックボックスの`top`/`bottom`も同様(箱はascent=マージンボックス
    // 高さ・descent=0として扱う)。
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

/// `display: inline-block`の中身をレイアウトする。新しいBlock Formatting
/// Contextを確立するため、空の`FloatContext`を渡す。幅は
/// 明示指定があればそれ、無ければ内容の自然幅を使える幅でクランプする。
///
/// 箱は原点`(0, 0)`で組み、行の位置が確定してから`place_atomic_inlines`が
/// 最終座標へ動かす。集めた`absolute`は動かさなくてよい(テーブルセルと
/// 同じ理由: 絶対配置の位置はcontaining blockからしか決まらない)。
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
    // 置換要素(`<img>`)は`width`/`height`属性・画像の固有サイズから寸法が
    // 決まる。ブロック配置時(`resolve_box_geometry`)と同じ処理をここでも
    // 通し、寸法決定ロジックを共有する。
    if let BoxContent::Image(image_content) = &b.content {
        super::block::apply_replaced_element_auto_size(&mut style, image_content, available_width);
    }
    let padding = super::block::resolve_padding(&style, available_width);
    let border = super::block::resolve_border(&style);

    // 通常フロー用の`resolve_width_and_horizontal_margins`は使わない。あの関数の
    // over-constrained規則(CSS2.1 §10.3.3、width/margin両方が非autoのとき
    // margin-rightを残り幅いっぱいに再計算する)はインラインレベルの箱には
    // 適用されず、そのまま通すと巨大なmargin-rightが行送りの幅に混入する
    // (floatが同じ理由で迂回している)。代わりに使用content幅を自分で決めて
    // `forced_content_width`として渡す。
    let content_width = match style.width {
        LengthPercentageOrAuto::LengthPercentage(lp) => {
            let width = super::block::resolve_lp(lp, available_width);
            if style.box_sizing == BoxSizing::BorderBox {
                (width - padding.left - padding.right - border.left - border.right).max(0.0)
            } else {
                width
            }
        }
        // shrink-to-fit相当: 内容の自然幅を使える幅でクランプ。floatの
        // `width: auto`と同じ`shrink_to_fit_content_width`を共有する。
        LengthPercentageOrAuto::Auto => {
            let outer = padding.left + padding.right + border.left + border.right;
            // 高さが確定していれば`aspect-ratio`から幅を導ける。
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
    // `min-width`/`max-width`。
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

/// マージンボックスの幅(行送りに使う)。
fn margin_box_width_of(b: &LaidOutBox) -> f32 {
    let border_box = b.layout.border_box();
    b.layout.margin.left + border_box.width + b.layout.margin.right
}

/// 各ランの`vertical-align`から`baseline_shift`(px、正=上)を求める。
/// `top`/`bottom`は行ボックスの寸法が要るため、ここでは0のままにして
/// [`finish_line`]が後追いで解決する。
fn resolve_baseline_shifts(runs: &mut [TextRun], fonts: &FontCollection) {
    // `text-top`/`text-bottom`/`middle`の基準は行の先頭ラン。
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

    /// 絶対配置の収集先を毎回用意しなくて済むよう、本体を包んで同名で影を作る。
    /// ここでの関心は行組みの結果だけなので、集まった`absolute`は捨てる。
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

    /// 既定スタイル(`line-height: normal`)での1行の高さ。`normal`はフォントの
    /// メトリクス由来なので、テスト用フォントから求める。
    fn default_line_height(fonts: &FontCollection) -> f32 {
        fonts
            .get(0)
            .expect("テスト用フォントは必ず1本ある")
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

    /// `<p>{inner_html}</p>`をパースし、最初のインラインボックスの
    /// スパン列と計算スタイルを返す(実際のDOM→ボックスツリー経由のテスト用)。
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
        // 同じ体裁で連続するので1ランにまとまり、単語間の空白も復元される。
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
        // real boldフェイスがないフォント集合を使い、synthetic boldフラグで
        // first-letterのfont-weightがランに反映されたことを検証する。
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

        // 残りは同じ体裁なので1ランにまとまり、単語間の空白も含む。
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

        // 左に400px幅・十分な高さのfloatを置き、全ての行がその右側
        // (x=400以降、幅100)に押し込まれることを確認する。
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

        // floatの高さは1行分だけ: 1行目はfloatの右に押し込まれ、2行目以降は
        // floatの下に出るため元の幅・左端に戻るはず。
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

        // `LineBox`は`display: inline-block`のボックスを含むようになり
        // PartialEqを持たないため、行の幾何とテキストで比較する。
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
        // 連続する空白・改行・タブは空白1つに畳まれる。
        assert_eq!(lines[0].runs.len(), 1);
        assert_eq!(lines[0].runs[0].text, "a b c");
    }

    #[test]
    fn mixed_script_word_splits_into_separate_font_runs() {
        // 空白なしでLatinとCJKが混在する1トークン。CJK文字(日本語)は
        // 改行可能境界のため、スタイル/フォントが同じでも1文字ずつ別ランに
        // 分かれる("café" + "日" + "本" + "語" = 4ラン)。
        let (_, spans, styles) = spans_for("café日本語", "");
        let fonts = dejavu_and_cjk();

        let lines = layout_inline_content(&spans, &styles, &fonts, 500.0, 0.0, 0.0, None);

        assert_eq!(lines.len(), 1);
        // フォントが変わる境界だけがランを分ける(CJKは1文字ずつ組んだあと、
        // 同じフォントで隙間なく続くのでまとまる)。
        assert_eq!(lines[0].runs.len(), 2, "café / 日本語 の2ラン");
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
        // 各ランは隙間なく(単語内なので空白は挟まず)左から右へ連続する。
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
        // フォントが違うので2ラン。"請求書"は同じフォントなのでまとまる。
        assert_eq!(lines[0].runs.len(), 2);
        assert_eq!(lines[0].runs[0].font_index, 0);
        assert_eq!(lines[0].runs[0].text, "Invoice");
        assert_eq!(lines[0].runs[1].font_index, 1);
        assert_eq!(lines[0].runs[1].text, "請求書");
    }

    #[test]
    fn long_cjk_sequence_wraps_between_characters_without_whitespace() {
        // 空白の無い長いCJK文字列でも、行幅に収まらなければ文字間で改行できる
        // (分かち書きをしない言語のため)。
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

        // "café"の幅ぎりぎりの行幅にすると、続く日本語部分は収まらないはず。
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
        // "bo"は通常、"ld"は<b>(太字)というスタイル境界が単語の途中にある。
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
        // "bo"は通常、"ld"は<b>(太字)。フォントコレクションにDejaVu SansのBold版も
        // 含まれている場合、疑似太字ではなく本物のBold面が選ばれるはず
        // (family名を明示しないと既定の"sans-serif"はどちらのフォント名にも
        // 一致せず、weight/styleを問わない先頭フォントへのフォールバックに
        // 落ちてしまい本来テストしたい分岐を通らないため、明示的に指定する)。
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
        // font-familyを一切指定しない(既定値"sans-serif")場合でも、familyの
        // 一致を問わないグローバルフォールバック側でweight/style一致を優先し、
        // 本物のBold面を選べるはず
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
        // "hello <b>world</b>"のように単語の先頭がspan境界を跨いでいても、
        // capitalizeは正しく大文字化できるはず。
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

        // 単語間の空白はランにまとめられるため、行全体の幅で比べる。
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
        // 幅を狭くしても、明示的な改行(\n)以外では折り返さないはず。
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
        // 行頭の空白がインデントとして残る。空白のみのテキストノードで
        // 始まっているので、以前は`box_tree`の段階で捨てられて`xy`になっていた。
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
        // 幅を狭くして複数行に折り返させる。
        let lines = layout_inline_content(&spans, &styles, &fonts, 150.0, 0.0, 0.0, None);
        assert!(lines.len() >= 2, "expected wrapping to at least 2 lines");

        // 最後の行以外は、行幅ちょうど(available_width)まで引き伸ばされるはず。
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

        // 最後の行は伸縮しない(rect.widthは実際に使った幅のまま、150に届かない)。
        let last = lines.last().unwrap();
        assert!(
            last.rect.width < 150.0,
            "the last line should not be stretched by justify"
        );
    }

    #[test]
    fn text_align_justify_does_not_push_text_over_an_inline_block_on_the_same_line() {
        // 「aa bb [箱] cc」の後に長い単語が来て折り返すと、最初の行は
        // justifyで引き伸ばされる。単語境界(bb・cc)にだけ余りを配ると
        // 箱が置き去りになり、右へずれたbbが箱に重なっていた。
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
        // 箱の直前のラン("aa bb"、隣接ランは結合済み)と直後のラン("cc")の
        // 隙間も、他の単語間と同じく広がっている(箱の前後で一方だけが
        // 広がるのではない)。
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
        // `aaa<input>bbb`には空白が無いので単語境界でもない。伸縮の対象に
        // 数えてしまうと、箱とその両隣の文字の間だけが開いて見える。
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
        // 単語境界が無い行(1単語だけ)はjustifyしても伸縮しない
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

    /// 各行のテキストを連結して返す(強制改行のテスト用。空行は空文字列)。
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
        // 十分に広い行幅でも改行される。
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
        // `nowrap`は「幅による折り返し」を止めるだけで、強制改行は効く。
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
        // 主要ブラウザと同じ挙動。
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
        // 改行文字は単語区切りとしても働くため、前後の単語が連結されない。
        let (_, spans, styles) = spans_for("one two<br>three four", "");
        let fonts = dejavu_only();
        let lines = layout_inline_content(&spans, &styles, &fonts, 5000.0, 0.0, 0.0, None);
        assert_eq!(line_texts(&lines), vec!["one two", "three four"]);
    }

    #[test]
    fn br_inside_pre_also_breaks_the_line() {
        // `white-space: pre`は別経路(`layout_pre_content`)だが、`<br>`は
        // `'\n'`としてスパンに載るため改修なしで改行になる。
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
        // `<br clear="left">`はレガシー表示属性が`clear: left`に変換され、
        // 強制改行の直後の行をfloatの下端まで押し下げる。
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

    // ===== `vertical-align`(インライン文脈) =====

    /// 行内の各ランを`(テキスト, ベースラインからのずれ)`で返す。
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
        // 回帰確認: `finish_line`の書き換え前と同じ値(下限規則)。
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

        // 大きく持ち上げれば行の高さが伸びる(`content_height`)。
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
        // パーセンテージはそのランの`line-height`(20px)基準。
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

        // 小さいフォントの文字上端が、基準ランの文字上端に一致する。
        let t_shift = shifts.iter().find(|(t, _)| t == "t").unwrap().1;
        assert!((t_shift - (base_run.ascent - small_run.ascent)).abs() < 0.01);
        // 文字下端どうしが一致する(基準より浅いディセント = 下にずれる)。
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

        // 行ボックス座標(上端からの距離、下向きが正)では、ランのベースラインは
        // `line.baseline - baseline_shift`(`baseline_shift`は上向きが正)。
        // topのランは文字上端が行の上端に一致する。
        let top_of_run = line.baseline - top_run.baseline_shift - top_run.ascent;
        assert!(top_of_run.abs() < 0.01, "expected 0, got {top_of_run}");
        // bottomのランは文字下端が行の下端に一致する。
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
