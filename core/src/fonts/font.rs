//! フォントファイルの読み込み。

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

/// バイト列から作った借用ビュー一式。
///
/// harfrustの`Shaper`とskrifaの`Charmap`/`GlyphMetrics`は、いずれも
/// フォントのバイト列を借用する。個別に作り直すとその都度テーブルを
/// 引き直すことになるので、まとめて1度だけ構築して保持する。
struct FaceView<'a> {
    shaper: Shaper<'a>,
    charmap: Charmap<'a>,
    glyph_metrics: GlyphMetrics<'a>,
    /// For reading colour glyphs (embedded bitmaps, `COLR`/`CPAL`). Those
    /// tables are consulted only once per glyph actually used, so there is no
    /// dedicated view — just a reference to the whole font.
    font: FontRef<'a>,
}

/// `FaceView`の借用元。
///
/// `ShaperData`はシェイピングで使うテーブルのキャッシュで、バイト列を
/// 借用せず自前で持つ。`Shaper`はこの2つ(バイト列と`ShaperData`)を
/// 借用するため、両方を`self_cell`のownerに入れる。
struct FaceOwner {
    bytes: Vec<u8>,
    index: u32,
    shaper_data: ShaperData,
}

self_cell!(
    /// フォントのバイト列と、そこから作った借用ビューを一緒に持つ。
    ///
    /// ビューはバイト列を借用するため、素直に構造体へ入れると自己参照になる。
    /// 構築はフォントのテーブル走査を伴うので、呼び出しのたびに作り直すと
    /// レイアウトが処理時間の大半を占めてしまう。
    struct OwnedFace {
        owner: FaceOwner,
        #[covariant]
        dependent: FaceView,
    }
);

/// シェイピング計画のキャッシュキー。harfrustがバッファの内容から推測した
/// 書字方向・スクリプト・言語で、計画の中身はこの3つとフェイスだけで決まる。
type PlanKey = (
    harfrust::Direction,
    harfrust::Script,
    Option<harfrust::Language>,
);

/// フォント全体のグリフを囲む矩形(フォントユニット)。
///
/// `head`テーブルが持つ値をそのまま指す。PDFのFontDescriptorの`/FontBBox`に使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BoundingBox {
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

/// 読み込み済みのフォントデータ。
///
/// ファイルの生バイト列と、そこから構築したシェイピング/メトリクス用の
/// ビューを保持する。値の変わらないメトリクスは[`Metrics`]として構築時に
/// 1度だけ読み、グリフ検索は[`Font::glyph_id`]でメモ化する。
pub struct Font {
    face: OwnedFace,
    index: u32,
    metrics: Metrics,
    /// 文字 → グリフID(cmapに無ければ`None`)のメモ。
    ///
    /// 文書に現れる異なり文字数は多くないので、素直な`HashMap`で十分に効く。
    /// 内容はフォントから決まるためキャッシュとして透過的で、外から観測できる
    /// 振る舞いは変わらない。
    glyphs: RefCell<HashMap<char, Option<u16>>>,
    /// シェイピング計画のメモ([`Font::shape_plan`])。
    plans: RefCell<HashMap<PlanKey, Rc<harfrust::ShapePlan>>>,
}

impl Clone for Font {
    /// バイト列を複製してビューを作り直す(ビューは複製元のバイト列を
    /// 借用しているため、そのままは持ち出せない)。
    fn clone(&self) -> Self {
        Self::from_bytes(self.data().to_vec(), self.index)
            .expect("複製元が有効なフォントなので失敗しない")
    }
}

impl fmt::Debug for Font {
    /// ビューが`Debug`を実装しないため、識別に足る情報だけ出す。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Font")
            .field("family_name", &self.metrics.family_name)
            .field("index", &self.index)
            .field("bytes", &self.data().len())
            .finish()
    }
}

/// フォントから1度だけ読めば足りるメトリクス。
///
/// 各値はフォントユニットで持つ。`head`/`hhea`/`OS/2`/`post`の各テーブルが
/// これらを整数で格納しているため、skrifaが`f32`で返すものも整数へ戻して
/// 保持している(丸めは発生しない)。
#[derive(Debug, Clone)]
struct Metrics {
    units_per_em: u16,
    ascender: i16,
    descender: i16,
    /// 行間(`hhea`の`lineGap`。`OS/2`がUSE_TYPO_METRICSを立てていれば
    /// `sTypoLineGap`)。`line-height: normal`の算出に使う。
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
    /// Some fonts carry a `cmap` but no outlines, such as bitmap-only colour
    /// emoji fonts (`CBDT`/`CBLC`). Their font program is never embedded:
    /// subsetting has nothing to strip and viewers reject the result.
    has_outlines: bool,
    /// Whether the font has colour glyphs (embedded bitmaps, or `COLR`/`CPAL`).
    ///
    /// A font with this set can be drawn even without outlines; those glyphs
    /// are written to the PDF as a Type 3 font.
    has_color_glyphs: bool,
}

impl Metrics {
    fn read(font: &FontRef<'_>) -> Self {
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let attributes = font.attributes();

        // subscript/superscriptのYオフセットはskrifaの`Metrics`が持たないため、
        // `OS/2`テーブルを直接読む。
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
            // `OS/2`がItalicを立てていなくても、`post`のitalic angleが非ゼロなら
            // 傾いた面として扱う(斜体指定に対する疑似斜体の要否判定に使うため、
            // 実際に傾いているかどうかで判断する)。
            is_italic: matches!(attributes.style, skrifa::attribute::Style::Italic)
                || italic_angle != 0.0,
            underline: m.underline.map(|d| (d.offset as i16, d.thickness as i16)),
            strikeout: m.strikeout.map(|d| (d.offset as i16, d.thickness as i16)),
            is_monospaced: m.is_monospace,
            weight: attributes.weight.value() as u16,
            has_outlines: font.outline_glyphs().format().is_some(),
            has_color_glyphs: super::color::has_color_glyphs(font),
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

/// Whether an already-parsed face can draw anything, i.e. has outlines or
/// colour glyphs.
///
/// The same test as [`Font::can_render`], but without building a `Font` (which
/// copies the byte slice). Used by the full scan over system fonts.
pub(super) fn face_can_render(font: &FontRef<'_>) -> bool {
    font.outline_glyphs().format().is_some() || super::color::has_color_glyphs(font)
}

/// Warn that a font was declined because it cannot draw anything.
///
/// That means a font with neither outlines (`glyf`/CFF) nor colour glyphs
/// (bitmaps, `COLR`). `source` is whatever the user named it by: a `--font`
/// path, an `@font-face` family. Fonts dropped by the automatic search are not
/// reported here; they surface through the "no font can draw this" warning
/// instead.
pub fn warn_font_without_outlines(source: &str) {
    eprintln!(
        "警告: {source} は輪郭もカラーグリフも持たないため使用しません。\n  \
         対応しているのは輪郭(glyf/CFF)、埋め込みビットマップ(CBDT/CBLC・sbix)、\n  \
         COLR/CPAL v0です(COLRv1のグラデーションとOpenType SVGは未対応)"
    );
}

#[derive(Debug)]
pub struct FontLoadError(String);

impl fmt::Display for FontLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "フォントの読み込みに失敗しました: {}", self.0)
    }
}

impl std::error::Error for FontLoadError {}

impl Font {
    /// ローカルファイルパスからフォントを読み込む。
    pub fn load(path: impl AsRef<Path>) -> Result<Self, FontLoadError> {
        Self::load_indexed(path, 0)
    }

    /// ローカルファイルパスからフォントを読み込む。TrueType Collection(`.ttc`)等、
    /// 複数フェイスを含むファイルの場合は`index`でフェイスを選択する。
    pub fn load_indexed(path: impl AsRef<Path>, index: u32) -> Result<Self, FontLoadError> {
        let path = path.as_ref();
        let data =
            std::fs::read(path).map_err(|e| FontLoadError(format!("{}: {e}", path.display())))?;
        Self::from_bytes(data, index)
    }

    /// 読み込み済みのバイト列からフォントを構築する(TrueType Collection等、
    /// 複数フェイスを含む場合は`index`でフェイスを選択する)。
    pub fn from_bytes(data: Vec<u8>, index: u32) -> Result<Self, FontLoadError> {
        // `ShaperData`とメトリクスはバイト列を借用しないので、ここで一度
        // パースして作ってしまう。バイト列を借用する`Shaper`等の構築だけを
        // この後の`self_cell`のクロージャで行う。
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
                font,
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

    /// `key`(方向・スクリプト・言語)に対応するシェイピング計画。
    ///
    /// シェイピングは呼び出しのたびに計画を要求するが、その構築は
    /// シェイピング本体より重い。レイアウトは単語(スタイル/フォントが連続
    /// する区間)ごとにシェイピングを呼ぶため、計画をフェイス単位で使い回さ
    /// ないと処理時間の大半を計画構築が占める。計画の中身はフェイスとキー
    /// だけで決まるため、キャッシュとして透過的で結果は変わらない。
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

    /// フォントファイルの生バイト列(PDFへのフォント埋め込み等で必要)。
    pub fn data(&self) -> &[u8] {
        &self.face.borrow_owner().bytes
    }

    /// TrueType Collection(`.ttc`)等、複数フェイスを含むファイル内でのフェイス番号。
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

    /// `line-height: normal`の使用値(px)。
    ///
    /// CSSの`normal`は「フォントが推奨する行送り」で、その実体は
    /// アセント+ディセント+行間(`lineGap`)。固定倍率(1.2em等)で近似すると、
    /// アセント+ディセントがそれを超えるフォント(CJKのフォントは1.4em前後の
    /// ものが珍しくない)でグリフが行ボックスからはみ出し、隣接する行や
    /// 枠線と重なる。
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

    /// アセント/ディセントから、行ボックス上端からベースラインまでの距離を
    /// 求める(フォントのem矩形を行ボックス内で上下中央に配置する)。
    /// テーブルセルの`vertical-align: baseline`(セル内容の最初の行の
    /// ベースライン位置を求める)とテキスト描画(`render_line`)の両方で使う。
    pub fn baseline_offset(&self, font_size: f32, line_height: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        let ascent = self.ascender() as f32 / units_per_em * font_size;
        let descent = -(self.descender() as f32) / units_per_em * font_size;
        let half_leading = (line_height - (ascent + descent)) / 2.0;
        ascent + half_leading
    }

    /// x-height(px)。`OS/2`テーブルが持たない場合はアセントの半分で近似する
    /// (`vertical-align: middle`の基準)。
    pub fn x_height(&self, font_size: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        match self.metrics.x_height {
            Some(x) => x as f32 / units_per_em * font_size,
            None => self.ascender() as f32 / units_per_em * font_size * 0.5,
        }
    }

    /// `vertical-align: sub`の下げ幅(px、正の値)。フォントの`OS/2`が
    /// subscriptのYオフセットを持たない場合は`0.2em`で近似する。
    pub fn subscript_offset(&self, font_size: f32) -> f32 {
        let units_per_em = self.units_per_em() as f32;
        match self.metrics.subscript_y_offset {
            Some(y_offset) => y_offset as f32 / units_per_em * font_size,
            None => font_size * 0.2,
        }
    }

    /// `vertical-align: super`の上げ幅(px、正の値)。持たない場合は`0.33em`。
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

    /// 下線の中心位置(ベースラインからの符号付きオフセット、フォントユニット。
    /// 上方向が正)と太さ。フォントが`post`テーブルを持たない場合は`None`。
    pub fn underline_metrics(&self) -> Option<(i16, i16)> {
        self.metrics.underline
    }

    /// 取り消し線の中心位置(ベースラインからの符号付きオフセット、フォントユニット。
    /// 上方向が正)と太さ。フォントが`OS/2`テーブルを持たない場合は`None`。
    pub fn strikeout_metrics(&self) -> Option<(i16, i16)> {
        self.metrics.strikeout
    }

    pub fn is_monospaced(&self) -> bool {
        self.metrics.is_monospaced
    }

    /// OS/2テーブルのウェイト値(400=標準, 700=太字)。
    pub fn weight(&self) -> u16 {
        self.metrics.weight
    }

    pub fn bounding_box(&self) -> BoundingBox {
        self.metrics.bounding_box
    }

    /// `glyph_id`の水平アドバンス幅(フォントユニット)。
    pub fn glyph_hor_advance(&self, glyph_id: u16) -> Option<u16> {
        self.view()
            .glyph_metrics
            .advance_width(skrifa::GlyphId::from(glyph_id))
            .map(|advance| advance as u16)
    }

    /// `c`に対応するグリフをこのフォントが持っているか。
    /// font-familyフォールバック(どのフォントでこの文字を描画できるか)の判定に使う。
    /// 文字に対応するグリフID(cmapに無ければ`None`)。
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
    /// Being in the `cmap` is not enough: the font also has to have something
    /// to draw with ([`Self::can_render`]). A font with neither outlines nor
    /// colour glyphs can still carry a `cmap`, and without this check it would
    /// be taken for capable and silently emit invisible text.
    pub fn has_glyph(&self, c: char) -> bool {
        self.can_render() && self.glyph_id(c).is_some()
    }

    /// Whether the font has glyph outlines (`glyf`/CFF/CFF2).
    ///
    /// This is `false` for a bitmap-only colour emoji font. A font without
    /// outlines is never embedded as a font program: subsetting has nothing to
    /// strip and viewers cannot read the result.
    pub fn has_outlines(&self) -> bool {
        self.metrics.has_outlines
    }

    /// Whether the font has colour glyphs (embedded bitmaps, or `COLR`/`CPAL`).
    pub fn has_color_glyphs(&self) -> bool {
        self.metrics.has_color_glyphs
    }

    /// Whether this font can draw anything at all, i.e. has outlines or
    /// colour glyphs.
    ///
    /// The single test font selection, `@font-face` loading and the system
    /// font search use to decide whether a font is usable.
    pub fn can_render(&self) -> bool {
        self.has_outlines() || self.has_color_glyphs()
    }

    /// The colour representation of `glyph_id`: an embedded bitmap, or a
    /// `COLR` v0 layer list. `None` when it has none, in which case it is an
    /// ordinary outline glyph.
    pub fn color_glyph(&self, glyph_id: u16) -> Option<super::color::ColorGlyph> {
        if !self.has_color_glyphs() {
            return None;
        }
        super::color::read(&self.view().font, glyph_id)
    }

    /// Whether `glyph_id` can be drawn as a colour glyph.
    pub fn has_color_glyph(&self, glyph_id: u16) -> bool {
        self.color_glyph(glyph_id).is_some()
    }

    /// Feed `glyph_id`'s outline to `pen`, used to write `COLR` v0 layers out
    /// as PDF paths. A font without outlines does nothing.
    pub fn draw_outline(&self, glyph_id: u16, pen: &mut impl skrifa::outline::OutlinePen) -> bool {
        use skrifa::outline::DrawSettings;

        let Some(glyph) = self
            .view()
            .font
            .outline_glyphs()
            .get(skrifa::GlyphId::from(glyph_id))
        else {
            return false;
        };
        glyph
            .draw(
                DrawSettings::unhinted(Size::unscaled(), LocationRef::default()),
                pen,
            )
            .is_ok()
    }

    /// フォント名(`name`テーブルの Typographic Family、無ければ Family)。
    /// 英語名があればそれを、無ければ最初に見つかった名前を返す。
    pub fn family_name(&self) -> Option<String> {
        self.metrics.family_name.clone()
    }
}

/// バイト列の`index`番目のフェイスを読む。TrueType Collectionでない場合、
/// `index`が0なら単一フェイスとして扱われる。
fn parse_font(data: &[u8], index: u32) -> Result<FontRef<'_>, FontLoadError> {
    FontRef::from_index(data, index)
        .map_err(|e| FontLoadError(format!("不正なフォントデータです: {e}")))
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
        // DejaVu SansはCJK文字を含まない。
        assert!(!font.has_glyph('日'));
    }

    #[test]
    fn from_bytes_rejects_invalid_font_data() {
        let result = Font::from_bytes(b"not a font file".to_vec(), 0);
        assert!(result.is_err());
    }

    #[test]
    fn reads_the_metrics_the_pdf_font_descriptor_needs() {
        // FontDescriptorへ書く値が揃っていることを、DejaVu Sansの既知の値で確認する。
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

        let gid = font.glyph_id('A').expect("DejaVu SansはAを持つ");
        let advance = font
            .glyph_hor_advance(gid)
            .expect("グリフが存在すればアドバンス幅も引ける");
        assert!(advance > 0);
        // 空白の方がAより狭い。
        let space = font.glyph_id(' ').expect("DejaVu Sansは空白を持つ");
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
        // アセント分だけ行の上端から下がった位置が概ねベースラインになるはず
        // (行の高さがフォント自身のメトリクス通りなら半行送りはゼロに近い)。
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

        // DejaVu Sansは`lineGap`が0なので、アセント+ディセントがそのまま
        // `normal`の行送りになる。
        let normal = font.normal_line_height(16.0);
        assert!(
            (normal - expected).abs() < 0.01,
            "normal line height should be ascent + descent (+ line gap): {normal} vs {expected}"
        );
    }

    #[test]
    fn normal_line_height_always_covers_the_glyphs_content_area() {
        // `normal`が「アセント+ディセント」を下回るフォントがあると、
        // 半行送りが負になってグリフが行ボックスからはみ出す。
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
mod colour_tests {
    use super::*;
    use crate::fonts::color::ColorGlyph;

    const TEST_FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
    /// ビットマップのみ(CBDT/CBLC)で、グリフの輪郭を一切持たないフォント。
    const COLOR_EMOJI_FONT_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/NotoColorEmoji.ttf"
    );

    #[test]
    fn a_normal_font_has_outlines_and_no_colour_glyphs() {
        let font = Font::load(TEST_FONT_PATH).expect("should load bundled test font");
        assert!(font.has_outlines());
        assert!(font.can_render());
        assert!(!font.has_color_glyphs());
        assert!(font.has_glyph('A'));
        assert!(font.color_glyph(font.glyph_id('A').unwrap()).is_none());
    }

    /// A bitmap-only font with no outlines at all can still be drawn from its
    /// embedded bitmaps. While this was `false`, font selection kept ruling an
    /// emoji font out as "cannot draw this character" — the behaviour before
    /// #12.
    #[test]
    fn a_bitmap_colour_font_can_render_the_characters_it_covers() {
        let font = Font::load(COLOR_EMOJI_FONT_PATH).expect("should load bundled colour font");

        assert!(!font.has_outlines(), "premise: this font has no outlines");
        assert!(font.has_color_glyphs());
        assert!(font.can_render());
        assert!(font.has_glyph('\u{1F389}'));
        // A character missing from the cmap cannot be drawn by a colour font
        // either.
        assert!(!font.has_glyph('日'));
    }

    /// A bitmap glyph comes out as a PNG, and its placement rectangle
    /// straddles the baseline in font units: the top sits near the ascent and
    /// the bottom below the baseline.
    #[test]
    fn a_bitmap_colour_glyph_is_a_png_placed_across_the_baseline() {
        let font = Font::load(COLOR_EMOJI_FONT_PATH).expect("should load bundled colour font");
        let gid = font.glyph_id('\u{1F389}').expect("cmapは絵文字を持つ");

        let Some(ColorGlyph::Bitmap(bitmap)) = font.color_glyph(gid) else {
            panic!("CBDT/CBLCのグリフはビットマップとして読めるはず");
        };

        assert_eq!(
            &bitmap.png[..8],
            b"\x89PNG\r\n\x1a\n",
            "a 32-bit CBDT glyph is stored as PNG"
        );

        let em = font.units_per_em() as f32;
        // A Noto Color Emoji bitmap is wider than 1em: about 1.25em, matching
        // its advance width.
        let width = bitmap.x_max - bitmap.x_min;
        assert!(
            width > em && width < em * 2.0,
            "placement width is not plausible against the em: {width} (em={em})"
        );
        assert!(
            (bitmap.y_max - font.ascender() as f32).abs() < em * 0.1,
            "the top is not near the ascent: {} (ascender={})",
            bitmap.y_max,
            font.ascender()
        );
        assert!(
            bitmap.y_min < 0.0,
            "the bottom should sit below the baseline: {}",
            bitmap.y_min
        );
    }
}
