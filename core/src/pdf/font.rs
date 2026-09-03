//! フォントのPDF埋め込み(CIDFontType2 + Type0 `/Encoding /Identity-H`)。
//!
//! `core/examples/spike_pdf_font_embedding.rs`で検証した方式をベースに、
//! 実際に使用したグリフだけへのサブセット化(`subsetter`クレート)と、
//! `/ToUnicode` CMapによるテキスト抽出対応を追加している。
//!
//! `subsetter::subset`はサブセット後のフォントから`cmap`テーブルを取り除く仕様
//! (PDF埋め込み専用の割り切った設計)のため、サブセット後のグリフIDは元の
//! グリフIDとは異なる(コンパクトに詰め直された)ものになる。埋め込み方式は
//! 2通りある:
//!
//! - [`embed_font`](一括処理・`pdf::document::encode_pdf`向け): コンテンツ
//!   ストリームを書く前に全ページを見てグリフ使用状況を確定できるため、
//!   CIDそのものをサブセット後のグリフIDに詰め替える(`/CIDToGIDMap
//!   /Identity`)。「元のグリフID→サブセット後のグリフID(=CID)」の対応表を
//!   返し、呼び出し側がコンテンツストリームを書く際にこの対応表でグリフIDを
//!   変換する
//! - [`embed_font_streaming_chunks`](ストリーミング処理向け):
//!   コンテンツストリームは各ページ確定時点で即座に書き出すため、
//!   CIDは常に元のグリフIDのまま(詰め替えない)。フォント埋め込み側だけ
//!   全ページ処理後にサブセット化し、`/CIDToGIDMap`をCID(=元GID)→
//!   サブセット後GIDの対応表を持つ明示的なストリームにすることで整合させる
//!
//! `pdf-writer`は圧縮を自前で行わないため、サブセット後のフォントバイト列は
//! `flate2`でzlib(`/FlateDecode`)圧縮してから埋め込む。

use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::io::Write;

use flate2::write::ZlibEncoder;
use flate2::Compression;
use pdf_writer::types::{CidFontType, FontFlags, SystemInfo, UnicodeCmap};
use pdf_writer::{Chunk, Filter, Finish, Name, Pdf, Rect as PdfRect, Ref, Str};
use subsetter::GlyphRemapper;

use crate::fonts::Font;

use super::color_font::{CODES_PER_COLOR_FONT, MAX_COLOR_GLYPHS_PER_FACE};

/// `/ToUnicode` CMapの`/CMapName`と埋め込みプログラム側の`CMapName`双方に
/// 使用する名前。両者が一致しないとPDF仕様上ill-formedになるので、
/// 単一の定数からすべての利用箇所に配る。
const TO_UNICODE_CMAP_NAME: Name<'static> = Name(b"Custom");

/// `/ToUnicode` CMapの`/CIDSystemInfo`と埋め込みプログラム側の`CIDSystemInfo`
/// 双方に使用する値(Adobe-UCS-0固定)。同上の理由で単一の定数から配る。
const TO_UNICODE_SYSTEM_INFO: SystemInfo<'static> = SystemInfo {
    registry: Str(b"Adobe"),
    ordering: Str(b"UCS"),
    supplement: 0,
};

/// 埋め込むフォント一式のオブジェクトID。
///
/// `cid_to_gid_map`は[`embed_font_streaming_chunks`]専用(`embed_font`は
/// `/CIDToGIDMap /Identity`を使うため参照しない)。
#[derive(Debug, Clone, Copy)]
pub struct FontIds {
    pub font_file: Ref,
    pub descriptor: Ref,
    pub cid_font: Ref,
    pub type0_font: Ref,
    pub to_unicode: Ref,
    pub cid_to_gid_map: Ref,
}

/// What one font is used for, collected by walking the whole document first.
///
/// Glyphs are sorted into two groups. Ones drawn from outlines are subsetted
/// into a CIDFontType2; ones drawn in colour (embedded bitmaps, `COLR` v0) go
/// to a Type 3 font in [`super::color_font`]. That split is decided here once,
/// and the same table is consulted again when the content stream is written.
#[derive(Debug, Default)]
pub struct FontUsage {
    /// Original glyph ID -> (width in 1000-unit glyph space, the source text
    /// this glyph stands for).
    glyphs: BTreeMap<u16, (f32, String)>,
    /// Colour glyphs: original glyph ID -> its assignment.
    color: BTreeMap<u16, ColorGlyphUse>,
    /// The number the next colour glyph will be assigned.
    next_color: usize,
}

/// What one colour glyph was assigned.
#[derive(Debug)]
pub(super) struct ColorGlyphUse {
    /// The source text this glyph stands for, for `/ToUnicode`.
    pub text: String,
    /// Its running number within the document, from which both the Type 3
    /// font and the character code follow: code `index % 256` of font
    /// `index / 256`.
    pub index: usize,
}

impl FontUsage {
    /// Record a use of `glyph_id`. `text` is the source text it stands for,
    /// for building `/ToUnicode`: the cluster string looked back up from
    /// `ShapedGlyph::cluster`.
    ///
    /// It is not necessarily a single character, because of ligatures (`fl`
    /// becoming one glyph). Recording only one character would make text
    /// extraction and search read "float" as "foat".
    ///
    /// Several characters can also share one glyph. When a font has no
    /// `&nbsp;` glyph the shaper substitutes the space glyph (HarfBuzz's space
    /// fallback), so that glyph stands for both U+0020 and U+00A0 in the
    /// document. First-write-wins would mean that a single early `&nbsp;`
    /// makes every space in the document extract as U+00A0, breaking search
    /// and copy. On a collision the plain space wins.
    pub fn record(&mut self, font: &Font, glyph_id: u16, text: &str) {
        if let Some(existing) = self.color.get_mut(&glyph_id) {
            prefer_plain_space(&mut existing.text, text);
            return;
        }
        match self.glyphs.entry(glyph_id) {
            Entry::Occupied(mut slot) => prefer_plain_space(&mut slot.get_mut().1, text),
            Entry::Vacant(slot) => {
                // A glyph that can be drawn in colour goes to the Type 3
                // side. Type 3 is a simple font, so codes are one byte and
                // the number of fonts we reserve is capped; anything past the
                // cap falls back to the outline side.
                if self.next_color < MAX_COLOR_GLYPHS_PER_FACE && font.has_color_glyph(glyph_id) {
                    self.color.insert(
                        glyph_id,
                        ColorGlyphUse {
                            text: text.to_string(),
                            index: self.next_color,
                        },
                    );
                    self.next_color += 1;
                    return;
                }
                // A glyph from a font with no outlines and no colour
                // representation cannot be drawn at all. Recording it would
                // mean embedding a font program no viewer can read, so drop
                // it here.
                if !font.has_outlines() {
                    return;
                }
                let advance = font.glyph_hor_advance(glyph_id).unwrap_or(0) as f32;
                let width_1000 = advance * 1000.0 / font.units_per_em() as f32;
                slot.insert((width_1000, text.to_string()));
            }
        }
    }

    /// If `glyph_id` was recorded as a colour glyph, which Type 3 font it
    /// landed in and under which one-byte character code.
    pub(super) fn color_code(&self, glyph_id: u16) -> Option<(usize, u8)> {
        let index = self.color.get(&glyph_id)?.index;
        Some((
            index / CODES_PER_COLOR_FONT,
            (index % CODES_PER_COLOR_FONT) as u8,
        ))
    }

    /// The colour glyphs held by Type 3 font `ordinal`, in code order.
    pub(super) fn color_glyphs_of(&self, ordinal: usize) -> Vec<(u8, u16, &str)> {
        let mut out: Vec<(u8, u16, &str)> = self
            .color
            .iter()
            .filter(|(_, use_)| use_.index / CODES_PER_COLOR_FONT == ordinal)
            .map(|(&gid, use_)| {
                (
                    (use_.index % CODES_PER_COLOR_FONT) as u8,
                    gid,
                    use_.text.as_str(),
                )
            })
            .collect();
        out.sort_by_key(|(code, ..)| *code);
        out
    }

    /// How many Type 3 fonts the recorded colour glyphs need.
    pub(super) fn color_font_count(&self) -> usize {
        self.next_color.div_ceil(CODES_PER_COLOR_FONT)
    }
}

/// When a plain space and something like `&nbsp;` share one glyph, make
/// `/ToUnicode` point at the plain space; otherwise every space in the
/// document extracts as U+00A0 and search and copy break.
fn prefer_plain_space(recorded: &mut String, text: &str) {
    if text == " " && recorded != " " {
        *recorded = text.to_string();
    }
}

/// `font`をPDFへ埋め込む(`usage`に記録されたグリフだけにサブセット化する)。
///
/// 返り値は「元のグリフID→サブセット後のグリフID(CID)」の対応表。
pub fn embed_font(
    pdf: &mut Pdf,
    font: &Font,
    ids: FontIds,
    usage: &FontUsage,
    compress: bool,
) -> BTreeMap<u16, u16> {
    let mut remapper = GlyphRemapper::new();
    remapper.remap(0); // .notdef
    for &old_gid in usage.glyphs.keys() {
        remapper.remap(old_gid);
    }

    let subset_data = subsetter::subset(font.data(), font.face_index(), &remapper)
        .unwrap_or_else(|_| font.data().to_vec());
    let compressed = maybe_deflate(&subset_data, compress);

    let mut font_file = pdf.stream(ids.font_file, &compressed);
    if compress {
        font_file.filter(Filter::FlateDecode);
    }
    // Length1はフォントプログラム本体の「圧縮前」の長さ(PDF仕様上の規定)。
    font_file.pair(Name(b"Length1"), subset_data.len() as i32);
    font_file.finish();

    let units_per_em = font.units_per_em() as f32;
    let to_1000 = |font_units: f32| font_units * 1000.0 / units_per_em;
    let bbox = font.bounding_box();

    pdf.font_descriptor(ids.descriptor)
        .name(Name(b"EmbeddedFont"))
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(PdfRect::new(
            to_1000(bbox.x_min as f32),
            to_1000(bbox.y_min as f32),
            to_1000(bbox.x_max as f32),
            to_1000(bbox.y_max as f32),
        ))
        .italic_angle(font.italic_angle())
        .ascent(to_1000(font.ascender() as f32))
        .descent(to_1000(font.descender() as f32))
        .cap_height(to_1000(
            font.capital_height().unwrap_or(font.ascender()) as f32
        ))
        .stem_v(if font.weight() >= 700 { 120.0 } else { 80.0 })
        .font_file2(ids.font_file);

    let old_to_new: BTreeMap<u16, u16> = usage
        .glyphs
        .keys()
        .map(|&old_gid| {
            let new_gid = remapper
                .get(old_gid)
                .expect("usageに記録済みのグリフは必ずremapされている");
            (old_gid, new_gid)
        })
        .collect();

    let mut cid_font = pdf.cid_font(ids.cid_font);
    cid_font.subtype(CidFontType::Type2);
    cid_font.base_font(Name(b"EmbeddedFont"));
    cid_font.system_info(SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    });
    cid_font.font_descriptor(ids.descriptor);
    cid_font.default_width(0.0);
    {
        let mut w = cid_font.widths();
        for (&old_gid, (width, _)) in &usage.glyphs {
            let new_gid = old_to_new[&old_gid];
            w.same(new_gid, new_gid, *width);
        }
        w.finish();
    }
    cid_font.cid_to_gid_map_predefined(Name(b"Identity"));
    cid_font.finish();

    let mut cmap = UnicodeCmap::<u16>::new(TO_UNICODE_CMAP_NAME, TO_UNICODE_SYSTEM_INFO);
    for (&old_gid, (_, text)) in &usage.glyphs {
        cmap.pair_with_multiple(old_to_new[&old_gid], text.chars());
    }
    let cmap_bytes = maybe_deflate(&cmap.finish(), compress);
    let mut to_unicode = pdf.cmap(ids.to_unicode, &cmap_bytes);
    to_unicode.name(TO_UNICODE_CMAP_NAME);
    to_unicode.system_info(TO_UNICODE_SYSTEM_INFO);
    if compress {
        to_unicode.filter(Filter::FlateDecode);
    }
    to_unicode.finish();

    pdf.type0_font(ids.type0_font)
        .base_font(Name(b"EmbeddedFont"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(ids.cid_font)
        .to_unicode(ids.to_unicode);

    old_to_new
}

/// [`embed_font`]のストリーミング版。CIDをサブセット後のグリフIDに詰め替えず、
/// 常に元のグリフIDのまま扱う。かわりに`/CIDToGIDMap`を、CID(元GID)から
/// サブセット後GIDへの対応を持つ明示的なストリームにする。
///
/// 返り値は、各オブジェクトを`(Ref, Chunk)`の列として返す。1つの`Chunk`には
/// 1つの間接オブジェクトのみを含める(呼び出し側がオブジェクトごとに
/// `Sink`へ書き出し、その開始オフセットをxrefのために記録できるようにする
/// ため)。呼び出し順に書き出せば十分で、並べ替えは不要。
pub fn embed_font_streaming_chunks(
    font: &Font,
    ids: FontIds,
    usage: &FontUsage,
    compress: bool,
) -> Vec<(Ref, Chunk)> {
    let mut chunks = Vec::with_capacity(6);

    let mut remapper = GlyphRemapper::new();
    remapper.remap(0); // .notdef
    for &old_gid in usage.glyphs.keys() {
        remapper.remap(old_gid);
    }
    let subset_data = subsetter::subset(font.data(), font.face_index(), &remapper)
        .unwrap_or_else(|_| font.data().to_vec());

    let compressed_font = maybe_deflate(&subset_data, compress);
    let mut chunk = Chunk::new();
    let mut font_file = chunk.stream(ids.font_file, &compressed_font);
    if compress {
        font_file.filter(Filter::FlateDecode);
    }
    font_file.pair(Name(b"Length1"), subset_data.len() as i32);
    font_file.finish();
    chunks.push((ids.font_file, chunk));

    let units_per_em = font.units_per_em() as f32;
    let to_1000 = |font_units: f32| font_units * 1000.0 / units_per_em;
    let bbox = font.bounding_box();

    let mut chunk = Chunk::new();
    chunk
        .font_descriptor(ids.descriptor)
        .name(Name(b"EmbeddedFont"))
        .flags(FontFlags::NON_SYMBOLIC)
        .bbox(PdfRect::new(
            to_1000(bbox.x_min as f32),
            to_1000(bbox.y_min as f32),
            to_1000(bbox.x_max as f32),
            to_1000(bbox.y_max as f32),
        ))
        .italic_angle(font.italic_angle())
        .ascent(to_1000(font.ascender() as f32))
        .descent(to_1000(font.descender() as f32))
        .cap_height(to_1000(
            font.capital_height().unwrap_or(font.ascender()) as f32
        ))
        .stem_v(if font.weight() >= 700 { 120.0 } else { 80.0 })
        .font_file2(ids.font_file);
    chunks.push((ids.descriptor, chunk));

    // CIDToGIDMap: CID(=元GID)でインデックスした2バイトのGID値のテーブル。
    // 未使用のCIDは0(.notdef)のままにする。
    let max_gid = usage.glyphs.keys().copied().max().unwrap_or(0);
    let mut cid_to_gid_bytes = vec![0u8; (max_gid as usize + 1) * 2];
    for &old_gid in usage.glyphs.keys() {
        let new_gid = remapper
            .get(old_gid)
            .expect("usageに記録済みのグリフは必ずremapされている");
        let idx = old_gid as usize * 2;
        cid_to_gid_bytes[idx..idx + 2].copy_from_slice(&new_gid.to_be_bytes());
    }
    let compressed_cid_to_gid = maybe_deflate(&cid_to_gid_bytes, compress);
    let mut chunk = Chunk::new();
    let mut cid_to_gid_stream = chunk.stream(ids.cid_to_gid_map, &compressed_cid_to_gid);
    if compress {
        cid_to_gid_stream.filter(Filter::FlateDecode);
    }
    cid_to_gid_stream.finish();
    chunks.push((ids.cid_to_gid_map, chunk));

    let mut chunk = Chunk::new();
    let mut cid_font = chunk.cid_font(ids.cid_font);
    cid_font.subtype(CidFontType::Type2);
    cid_font.base_font(Name(b"EmbeddedFont"));
    cid_font.system_info(SystemInfo {
        registry: Str(b"Adobe"),
        ordering: Str(b"Identity"),
        supplement: 0,
    });
    cid_font.font_descriptor(ids.descriptor);
    cid_font.default_width(0.0);
    {
        // /Wは元のグリフID(=CID)をキーに、サブセット前と同じ値をそのまま
        // 書ける(幅はusage収集時点で元GIDベースに記録済みのため変換不要)。
        let mut w = cid_font.widths();
        for (&old_gid, (width, _)) in &usage.glyphs {
            w.same(old_gid, old_gid, *width);
        }
        w.finish();
    }
    // Identityではなく、サブセット後の実グリフ位置への明示マップを使う。
    cid_font.cid_to_gid_map_stream(ids.cid_to_gid_map);
    cid_font.finish();
    chunks.push((ids.cid_font, chunk));

    let mut cmap = UnicodeCmap::<u16>::new(TO_UNICODE_CMAP_NAME, TO_UNICODE_SYSTEM_INFO);
    for (&old_gid, (_, text)) in &usage.glyphs {
        cmap.pair_with_multiple(old_gid, text.chars());
    }
    let cmap_bytes = maybe_deflate(&cmap.finish(), compress);
    let mut chunk = Chunk::new();
    let mut to_unicode = chunk.cmap(ids.to_unicode, &cmap_bytes);
    to_unicode.name(TO_UNICODE_CMAP_NAME);
    to_unicode.system_info(TO_UNICODE_SYSTEM_INFO);
    if compress {
        to_unicode.filter(Filter::FlateDecode);
    }
    to_unicode.finish();
    chunks.push((ids.to_unicode, chunk));

    let mut chunk = Chunk::new();
    chunk
        .type0_font(ids.type0_font)
        .base_font(Name(b"EmbeddedFont"))
        .encoding_predefined(Name(b"Identity-H"))
        .descendant_font(ids.cid_font)
        .to_unicode(ids.to_unicode);
    chunks.push((ids.type0_font, chunk));

    chunks
}

/// zlib(`/FlateDecode`)圧縮する。`compress`がfalseなら無圧縮のまま返す
/// (`--no-pdf-compression`)。呼び出し側は同じ
/// 条件で`/Filter`を書くかどうかを決める。
pub(super) fn maybe_deflate(data: &[u8], compress: bool) -> Vec<u8> {
    if compress {
        deflate(data)
    } else {
        data.to_vec()
    }
}

pub(super) fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(data)
        .expect("インメモリバッファへの書き込みは失敗しない");
    encoder
        .finish()
        .expect("インメモリバッファへの書き込みは失敗しない")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deflate_shrinks_compressible_data() {
        let data = vec![b'A'; 10_000];
        let compressed = deflate(&data);
        assert!(
            compressed.len() < data.len() / 10,
            "highly repetitive data should compress well: {} -> {}",
            data.len(),
            compressed.len()
        );
    }

    const TEST_FONT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    /// `&nbsp;`のグリフを持たないフォントでは、シェイパーがspaceのグリフで
    /// 代替するため1つのグリフが両方を表す。その場合でも`/ToUnicode`は
    /// 普通のspaceを指すようにする(そうしないと文書中の空白すべてが
    /// U+00A0として抽出され、テキスト検索やコピーが壊れる)。
    #[test]
    fn a_glyph_shared_by_a_space_and_a_no_break_space_maps_to_the_space() {
        let font = Font::load(TEST_FONT).expect("should load bundled test font");
        let space_glyph = 3;

        // `&nbsp;`が先に現れた場合でもspaceが勝つ。
        let mut nbsp_first = FontUsage::default();
        nbsp_first.record(&font, space_glyph, "\u{a0}");
        nbsp_first.record(&font, space_glyph, " ");
        assert_eq!(nbsp_first.glyphs[&space_glyph].1, " ");

        // 逆順でもspaceのまま(`&nbsp;`で上書きしない)。
        let mut space_first = FontUsage::default();
        space_first.record(&font, space_glyph, " ");
        space_first.record(&font, space_glyph, "\u{a0}");
        assert_eq!(space_first.glyphs[&space_glyph].1, " ");
    }

    #[test]
    fn a_ligature_cluster_keeps_the_text_it_was_first_recorded_with() {
        // 合字は複数文字を1グリフで表す。空白の優先は合字の記録を壊さない。
        let font = Font::load(TEST_FONT).expect("should load bundled test font");
        let mut usage = FontUsage::default();
        usage.record(&font, 100, "fl");
        usage.record(&font, 100, "fl");

        assert_eq!(usage.glyphs[&100].1, "fl");
    }

    #[test]
    fn deflate_output_round_trips_via_zlib_decoder() {
        let data = b"the quick brown fox jumps over the lazy dog".repeat(50);
        let compressed = deflate(&data);

        let mut decoder = flate2::read::ZlibDecoder::new(&compressed[..]);
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed).unwrap();

        assert_eq!(decompressed, data);
    }
}
