//! OS標準のフォントディレクトリを走査してシステムフォントを解決する(`fontdb`を使用)。
//!
//! CSSの汎用family名(`monospace`/`serif`/`sans-serif`)は、`fontdb`(=Linuxでは
//! fontconfig)の汎用名解決には任せず、自前の候補リストで解決する。
//! `fontdb`はfontconfig未設置の最小環境ではOS間で一貫性のないハードコードの
//! 既定名(`Arial`等)にフォールバックし、インストール環境に実在するとは
//! 限らないため。候補リストが全て外れた場合、`monospace`のみ`fontdb`の
//! フェース単位のメタデータ(`FaceInfo::monospaced`。fontconfig非依存)を使って
//! 等幅フェースを探す。それでも見つからなければ解決を諦め、
//! [`crate::fonts::FontCollection`]が既に持つグリフカバレッジ・フォールバック
//! に任せる。`sans-serif`は当初「既定`font-family`と同値なので解決しない」
//! としていたが、既定`font-family`を空(未指定)に切り離したため、
//! `sans-serif`を明示した場合のみゴシック体を解決するよう改めた。未指定要素は
//! 空`font-family`で`select_for_char`のフォールバック(=`--font`のフォント)へ
//! 行くため、`--font`が既定という挙動は保たれる。`--gothic-font`が渡された
//! 場合はそちらが`sans-serif`として最優先で使われる。
//!
//! family名による解決とは別に、文書中の文字を描画できるフォントが1本も無い
//! 場合に、その文字を描画できるシステムフォントを探す経路も持つ
//! ([`SystemFonts::load_covering`]・[`load_fonts_for_uncovered_chars`])。
//! `font-family`未指定の日本語文書のように、どのfamily名も
//! 手掛かりにならないケースでは[`FontCollection`]のグリフカバレッジ・フォー
//! ルバックが選ぶ候補自体が存在しないため、ここで補う必要がある。

use std::collections::HashMap;
use std::collections::HashSet;
use std::rc::Rc;

use skrifa::MetadataProvider;

use crate::html::{Dom, NodeData, NodeId};
use crate::style::{ComputedStyle, Display, FontStyle, FontWeight};

use super::collection::FontCollection;
use super::font::Font;

/// CSSの汎用family名(大文字小文字を区別せず判定する)。
const GENERIC_FAMILIES: &[&str] = &["serif", "sans-serif", "monospace", "cursive", "fantasy"];

/// 汎用family名ごとの、実在しやすい具体フォント名の候補(優先順)。
///
/// `cursive`/`fantasy`は環境差が大きく実務上の需要も薄いため候補を持たない(=
/// 解決しない)。`sans-serif`は既定`font-family`を
/// 空に切り離したため明示時のみ解決する。
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
            // 公式Dockerイメージが同梱する明朝体。ラテン系の候補が全て外れる
            // 最小環境(=そのイメージの中)で拾わせるため末尾に置く。
            "BIZ UDPMincho",
            "BIZ UDMincho",
        ],
    ),
    (
        // `sans-serif`は明示指定時のみゴシック体を探す。英字ゴシックの候補
        // (実運用では`--gothic-font`で決定的に上書きできる)。
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
            // 公式Dockerイメージが同梱するゴシック体(上の`serif`と同じ理由)。
            "BIZ UDPGothic",
            "BIZ UDGothic",
        ],
    ),
];

/// CJK(漢字・かな・ハングル)を描画できるフォントの候補(優先順)。
///
/// CJK統合漢字は日本語・韓国語・中国語で共有されるため、言語ごとに分けず
/// 1つの候補列にまとめ、実際に描画できるか([`Font::has_glyph`])で
/// 確認しながら順に試す(例えばHiragino Sansはハングルを持たないので、
/// ハングルを探しているときは自然に次の候補へ進む)。
///
/// 日本語 → 韓国語 → 中国語簡体 → 中国語繁体の順に並べるのは、帳票用途で
/// 日本語が支配的なため。漢字の字体には国別の差があるが、`lang`属性を見ないと
/// 決められないので初期スコープでは踏み込まない(外れる場合は`--gothic-font`
/// 等で決定的に上書きできる)。
const CJK_FAMILY_CANDIDATES: &[&str] = &[
    // 日本語
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
    // 公式Dockerイメージが同梱するゴシック体。
    "BIZ UDPGothic",
    "BIZ UDGothic",
    // 韓国語
    "Noto Sans CJK KR",
    "Noto Sans KR",
    "Apple SD Gothic Neo",
    "Malgun Gothic",
    // 中国語簡体
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "PingFang SC",
    "Microsoft YaHei",
    "SimSun",
    // 中国語繁体
    "Noto Sans CJK TC",
    "Noto Sans TC",
    "PingFang TC",
    "Microsoft JhengHei",
    "PMingLiU",
];

/// `c`がCJK系の文字か([`CJK_FAMILY_CANDIDATES`]を引くかどうかの判定に使う)。
///
/// 全走査([`SystemFonts::load_any_covering`])より先に候補リストを試すための
/// 絞り込みでしかないので、境界の厳密さは求めない(ここで漏れても
/// 全走査が拾う)。
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x303F      // CJKの記号と句読点
        | 0x3040..=0x309F    // ひらがな
        | 0x30A0..=0x30FF    // カタカナ
        | 0x3130..=0x318F    // ハングル互換字母
        | 0x3400..=0x4DBF    // CJK統合漢字 拡張A
        | 0x4E00..=0x9FFF    // CJK統合漢字
        | 0xAC00..=0xD7AF    // ハングル音節
        | 0xF900..=0xFAFF    // CJK互換漢字
        | 0xFF00..=0xFFEF    // 半角・全角形
        | 0x20000..=0x2FA1F  // CJK統合漢字 拡張B以降
    )
}

pub struct SystemFonts {
    db: fontdb::Database,
}

impl SystemFonts {
    /// OSのフォントディレクトリを走査してデータベースを構築する
    /// (メタデータのスキャンのみ。フォントファイルの実体はまだ読み込まない)。
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

    /// `family`という名前のシステムフォントを読み込む(大文字小文字を区別しない)。
    /// `weight`/`style`は`fontdb`のCSSライクなマッチングにそのまま渡すため、
    /// 例えば`weight: Bold`で該当familyに本物のBold面が存在すればそれが選ばれる
    /// (存在しなければ`fontdb`が代わりに最も近い面を返し、その場合は呼び出し側が
    /// `FontCollection::is_bold`等で実体を確認した上で疑似太字を補うことになる)。
    /// 一致するフォントが無ければ`None`。
    pub fn load(&self, family: &str, weight: FontWeight, style: FontStyle) -> Option<Font> {
        // `fontdb::Database::query`はfamily名の完全一致(大文字小文字を区別する)
        // でしか照合しないため、まず大文字小文字を無視して実際の登録名を探し、
        // その名前で改めてクエリする。
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
            // A font with neither outlines nor colour glyphs draws nothing,
            // however well its name matched. Every system font lookup goes
            // through here, so this is the one place that test lives.
            .filter(|font| font.can_render())
    }

    /// CSSの汎用family名(`monospace`/`serif`)を、自前の候補リスト
    /// ([`GENERIC_FAMILY_CANDIDATES`])を優先順に試して具体フォントへ
    /// 解決する。候補が全て外れた場合、`monospace`のみ`fontdb`の
    /// `FaceInfo::monospaced`フラグで等幅フェースを探す。`sans-serif`(既定
    /// `font-family`と同値のため意図的に対象外)・`cursive`/`fantasy`・
    /// 汎用名でない名前を渡した場合、および何も見つからない場合は`None`。
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

    /// `c`を描画できるシステムフォントを1つ探し、見つかった実family名と
    /// ともに返す。
    ///
    /// 解決は2段階(fontconfigの汎用名解決には任せない):
    ///
    /// 1. CJKなら[`CJK_FAMILY_CANDIDATES`]を順に`load`し、実際に`c`を
    ///    描画できるものを採る
    /// 2. 候補が外れたら[`Self::load_any_covering`]で全フェースを走査する
    ///    (最終手段)
    ///
    /// family名を一緒に返すのは、追加したフォントを
    /// [`FontCollection::push_font_face`]へ実family名で登録するため。
    /// 空名や擬似的な名前だと`matches_family`で意図しない一致を起こす。
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

    /// DB内の全フェースを走査して`c`を描画できるものを探す(最終手段)。
    ///
    /// グリフの有無の判定はcmapをその場で読むだけにして
    /// [`Font`]への変換(データのコピー)を避け、当たったフェースの
    /// family名で改めて`load`する(weight/styleの面選択を`load`に任せるため)。
    /// それでもフェースの実体読み込みは全件に及ぶので、候補リストが全て
    /// 外れた場合にしか通らない位置に置いている。
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
            // Being in the `cmap` is not enough; the face also needs
            // something to draw with. Same test as `Font::has_glyph`, done in
            // place without building a `Font`.
            let covered = self
                .db
                .with_face_data(info.id, |data, index| {
                    skrifa::FontRef::from_index(data, index)
                        .map(|font| {
                            font.charmap().map(c).is_some() && super::font::face_can_render(&font)
                        })
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if !covered {
                continue;
            }
            // 同じfamilyの別フェース(Bold等)が該当する場合もあるので、
            // family名で引き直してweight/styleに合う面を選ぶ。引き直しに
            // 失敗した場合だけ、判定に使ったフェースをそのまま採る。
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

    /// フォント自身のメタデータ上「等幅」とされているフェースを1つ選び、その
    /// family名で改めて`load`する(weight/styleの面選択を`load`に任せるため)。
    fn load_any_monospaced(&self, weight: FontWeight, style: FontStyle) -> Option<Font> {
        // `load` can decline a face even when the monospace flag is set, so
        // try them in order rather than stopping at the first hit.
        //
        // Only faces with outlines qualify. A colour emoji font has the same
        // advance for every glyph and so is registered as monospaced, and does
        // reach this point — but `font-family: monospace` resolving to Noto
        // Color Emoji would mean a document that draws emoji and not one
        // character of body text.
        self.db
            .faces()
            .filter(|info| info.monospaced)
            .filter_map(|info| info.families.first().map(|(name, _)| name.clone()))
            .find_map(|family| {
                self.load(&family, weight, style)
                    .filter(|font| font.has_outlines())
            })
    }

    /// `@font-face`の`src: local(...)`用。`name`(フルネームまたはPostScript名、
    /// 大文字小文字を区別しない)に一致する特定の面を1つ直接読み込む。
    /// `load`(family名+weight/styleによるCSS的なフォールバック検索)とは異なり、
    /// weight/styleによる曖昧なマッチングは行わない(名前で一意に指定された
    /// 1つの面を指すのが`local()`の意味のため)。
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

/// `styles`中で使われているfont-family/weight/styleの組のうち、`fonts`に
/// まだ実体が無いものだけを`system`から読み込み、`fonts`へ追加する。
///
/// `family`単位ではなく(family, weight, style)単位で判定するため、例えば
/// `--font`でRegularのみ読み込んだfamilyに対して文書内で太字が使われていれば、
/// そのfamilyのBold面だけを追加でシステムから探しに行く。
///
/// CSSの汎用family名(`monospace`等)は[`SystemFonts::load_generic`]で解決し、
/// 汎用名そのものを宣言family名として`fonts`へ登録する。こうすることで
/// `font-family: monospace`の照合が[`FontCollection::select_for_char`]の
/// 通常のfamily一致でそのまま機能する。
///
/// 走査順は[`document_chars`]と同じく文書順([`NodeId`]順)に固定する。
/// `styles`は`HashMap`なので反復順が実行ごとに変わり、そのまま回すと
/// ここで追加されるフォントの順序=PDFのフォント番号がぶれて、同じHTMLから
/// 別のバイト列が出てしまう。
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

/// 文書中の文字のうち`fonts`のどのフォントでも描画できないものについて、
/// 描画できるシステムフォントを探して`fonts`へ追加する。
///
/// [`load_missing_system_fonts`]がfamily名を手掛かりにするのに対し、
/// こちらは実際に使われている文字を手掛かりにする。`font-family`未指定の
/// 日本語文書のようにfamily名が何の手掛かりにもならないケースを救うための
/// 経路で、`load_missing_system_fonts`の直後に呼ぶ。
///
/// 対象はテキストノードの文字と、`::before`/`::after`の生成文字列(counter()や
/// quotesを解決済みの[`ComputedStyle::pseudo_before_content`]等)。リスト
/// マーカーの記号までは追わない(初期スコープ)。
///
/// フォントを1本追加するたびに残りの文字を再判定するため、日本語文書なら
/// CJKフォント1本の追加で以降の文字は全てカバー済みになる。走査順は
/// 文書順([`NodeId`]順)に固定して、追加されるフォントの順序=PDFの
/// フォント番号が実行ごとにぶれないようにする。
pub fn load_fonts_for_uncovered_chars(
    fonts: &mut FontCollection,
    dom: &Dom,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    system: &SystemFonts,
) {
    let mut seen = HashSet::new();
    // 借用の都合で一旦集める(`fonts`を可変で触るため、走査中に`styles`と
    // `fonts`を同時に借りられない)。
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

/// 文書中で実際に描画される文字を、文書順([`NodeId`]順)に列挙する。
///
/// 対象はテキストノードの文字と、`::before`/`::after`の生成文字列
/// (`counter()`やquotesを解決済みの[`ComputedStyle::pseudo_before_content`]等)。
/// リストマーカーの記号までは追わない(初期スコープ)。空白・制御文字は
/// グリフを引く前にレイアウト側で処理されるので除く。
///
/// 順序を文書順に固定するのは、ここで追加されるフォントの順序=PDFのフォント
/// 番号が実行ごとにぶれないようにするため(`styles`は`HashMap`なので
/// キーの反復順は不定)。
fn document_chars<'a>(
    dom: &'a Dom,
    styles: &'a HashMap<NodeId, Rc<ComputedStyle>>,
) -> impl Iterator<Item = (char, &'a ComputedStyle)> {
    let mut out = Vec::new();
    collect_rendered_chars(dom, dom.document(), styles, &mut out);
    out.into_iter()
}

/// `node`以下のテキストと生成内容を文書順に集める。
///
/// `display: none`の要素はサブツリーごと飛ばす([`crate::layout::box_tree`]が
/// 同じ位置で再帰を止めるのに合わせる)。描かれない文字までフォント選択の
/// 対象にすると、`<script>`や`<style>`に日本語が1文字あるだけで使われない
/// CJKフォントが埋め込まれ、さらに本文に使うフォントの選択まで変わってしまう。
///
/// テキストノードは親の計算スタイルをそのまま引き継ぐため`display`も見えるが、
/// `display: none`の要素と対象のテキストの間に要素が挟まると(`<div
/// style="display:none"><span>…`)そちらは`inline`に計算されるので、
/// ノード単位のフィルタでは足りず木を辿る必要がある。
///
/// `visibility: hidden`は対象外。描画されないだけで領域は占めるため、
/// 行の高さを決めるのにフォントが要る。
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

/// `c`を`weight`/`style`で描画できるフォントが`fonts`に無ければ、システムから
/// 探して追加する。判定済みの組は`seen`で覚えて再探索を避ける。
///
/// 「描画できる」の判定にウェイト/スタイルの一致まで含める理由は
/// [`FontCollection::can_render_with_matching_face`]を参照。
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

/// ストリーミングモードのように文書全体の文字を事前に集められない場合に、
/// CJKの代表文字を描画できるフォントを先回りで足す。
///
/// [`crate::pdf::StreamingPdfWriter`]は`new`の時点でフォント数を固定するので、
/// 読み進めながら[`load_fonts_for_uncovered_chars`]で補うことができない。
/// 代わりに、既定フォント(ラテン)だけでは確実に豆腐になるCJKを先回りで
/// カバーしておく。CJK以外のスクリプトは依然カバーできないため、呼び出し側は
/// 描画できない文字が残った場合の警告と併用する。
///
/// 代表文字を2つ試すのは、かな(`あ`)と漢字(`漢`)の両方を確認するため。
/// 通常は1本目のフォントが両方を持つので、2文字目は既にカバー済みとして
/// 何も追加されない。
pub fn ensure_cjk_fallback_font(fonts: &mut FontCollection, system: &SystemFonts) {
    const REPRESENTATIVE_CHARS: &[char] = &['漢', 'あ'];

    // 既定の`ComputedStyle`と同じ「family未指定・Regular・Normal」で探す。
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

/// 文書中にどのフォントでも描画できない文字が残っていれば、文字ごとに
/// 一度だけ警告する。
///
/// 黙って豆腐を出力しないための最後の網。`warned`は既に警告した文字の集合で、
/// ストリーミングモードのようにこの関数が何度も呼ばれる場合に、同じ文字を
/// 繰り返し警告しないために呼び出し側が持ち回る。
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
            "警告: 文字 \"{c}\" を描画できるフォントがありません(豆腐になります)。\n  \
             --font/--gothic-font か @font-face でフォントを明示してください"
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
        // `styles`は`HashMap`なので反復順が実行ごとに変わる。文書順に
        // 固定していないと、追加されるフォントの順序=PDFのフォント
        // 番号がぶれて、同じHTMLから別のバイト列が出てしまう(「同じ
        // HTMLなら同じ出力」が成り立たなくなる)。
        //
        // `HashMap`のハッシュキーはインスタンスごとに変わるため、順序が
        // 崩れているかどうかは1回の実行では見えないことがある。毎回
        // `styles`を作り直して繰り返す。
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
        // `--font`でRegularのDejaVu Sansのみ読み込み済みの状態で、文書は同じ
        // familyのBold(<b>)も使う。family自体は既に存在するが、Bold面は
        // まだ無いので、そのweightだけを追加でシステムから探しに行くはず。
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
        // `sans-serif`を明示した場合はゴシック体を候補リストで解決する。
        // fixtureの"DejaVu Sans"が候補にあるので拾える。既定`font-family`は空
        // (未指定)に切り離したため、この解決が
        // `--font`の既定挙動を壊すことはない。
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
        // 既定`font-family`は空(未指定)なので、`--font`のフォントへ
        // フォールバックする。システムフォント探索は起きない。
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
        // `cursive`/`fantasy`は候補を持たない。`Helvetica`は汎用名でない。
        // (`sans-serif`は解決対象になったのでここには含めない。)
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
        // serifの候補("DejaVu Serif"等)はfixtureに存在しない。monospaceと
        // 違い、フラグによるフォールバック探索も行わないのでNoneになる。
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        assert!(system
            .load_generic("serif", FontWeight::Normal, FontStyle::Normal)
            .is_none());
    }

    #[test]
    fn load_any_monospaced_finds_a_monospaced_face_by_its_metadata_flag() {
        // 候補リストが全て外れた場合のフォールバック経路(fontdbの
        // `FaceInfo::monospaced`フラグ)。fixtureのDejaVu Sans Monoが
        // 等幅フラグを持つことを利用して、経路そのものを直接検証する。
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
        // キリル文字は候補リストを持たないので、全フェース走査の経路
        // (`load_any_covering`)でしか見つからない。
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        let (_, font) = system
            .load_covering('Д', FontWeight::Normal, FontStyle::Normal)
            .expect("the fixture fonts cover Cyrillic");
        assert!(font.has_glyph('Д'));
    }

    #[test]
    fn load_covering_gives_up_on_a_character_no_font_can_render() {
        let system = SystemFonts::from_dir(std::path::Path::new(FONTS_DIR));
        // 私用領域の文字はどのfixtureフォントも持たない。
        assert!(system
            .load_covering('\u{E000}', FontWeight::Normal, FontStyle::Normal)
            .is_none());
    }

    #[test]
    fn load_fonts_for_uncovered_chars_ignores_text_that_is_not_rendered() {
        // 描かれない文字までカバレッジの対象にすると、`<script>`に日本語が
        // 1文字あるだけで使われないCJKフォントが埋め込まれ、さらに本文に
        // 使うフォントの選択まで変わってしまう。
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
            // `display: none`とテキストの間に要素を挟むと、その要素自身の
            // `display`は`inline`に計算される。ノード単位のフィルタでは
            // 漏れるので、木を辿って落とせているかをここで見る。
            r#"<div style="display: none"><span>領収書</span></div>"#,
        ] {
            assert_eq!(
                count_for(hidden),
                baseline,
                "描画されない日本語でフォントが増えた: {hidden}"
            );
        }

        // 実際に描画される日本語なら、従来どおりカバレッジから補完する。
        assert_eq!(count_for("<p>領収書</p>"), baseline + 1);
    }

    #[test]
    fn load_fonts_for_uncovered_chars_adds_a_cjk_face_for_japanese_text() {
        // `font-family`未指定の日本語文書。family名は何の手掛かりにもならないので、
        // 文字カバレッジからCJKフォントを引き当てる必要がある。
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
        // `::before`の生成文字列も描画されるので対象にする。
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
        // ストリーミング用の先回り。代表文字2つを試すが、1本のCJKフォントが
        // 両方を持つので追加は1本で済む。
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

    /// A bitmap colour emoji font is picked up by the coverage search (#12).
    /// It has no outlines, but it can draw the character from its embedded
    /// bitmaps, so it genuinely is "a font that can draw this".
    #[test]
    fn a_bitmap_colour_font_is_picked_up_by_the_coverage_search() {
        // Build a directory holding only the colour font: passing `FONTS_DIR`
        // as-is would mix in fonts that do have outlines.
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
        let (family, font) = system
            .load_covering('\u{1F389}', FontWeight::Normal, FontStyle::Normal)
            .expect("a font that can draw the emoji from bitmaps should be accepted");
        assert!(family.contains("Emoji"), "family name: {family}");
        assert!(!font.has_outlines());
        assert!(font.has_color_glyphs());

        // A character the font cannot draw at all is still not found.
        assert!(system
            .load_covering('日', FontWeight::Normal, FontStyle::Normal)
            .is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
