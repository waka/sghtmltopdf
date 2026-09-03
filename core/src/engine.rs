//! `Engine`: HTMLチャンク投入からPDFバイト列書き出しまでを1つのAPIとして
//! 統合するコアのエントリポイント。
//!
//! Sinkベースの`new`/`feed`/`finish`という粗粒度APIを実装する。Ruby側のFFI
//! 境界(`Engine.new(options)` / `feed(html_chunk)`
//! /`each_pdf_chunk { |bytes| ... }` / `finish`)にほぼ1:1で対応する。
//!
//! ## `Mode::Batch`と`Mode::Streaming`でパイプラインが異なる
//!
//! `Mode::Batch`は、`finish`が呼ばれた時点でDOM全体を一括して
//! (`compute_styles`/`build_box_tree`/`layout_document`/
//! `paginate_document_streaming`で)処理する、一括APIの薄いラッパー。
//!
//! `Mode::Streaming`は、`<body>`直下のトップレベルブロック要素が確定する
//! たびに、そのサブツリーだけをスタイル計算・レイアウト・ページ分割・
//! PDF書き出し・DOM解放まで処理する「真のストリーミング処理」を行う。
//! `<html>`/`<body>`自身のスタイルは、最初のトップレベル要素が確定する
//! までに一度だけ計算し、以後の各トップレベル要素のスタイル計算の起点
//! (継承元)として使う。
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::fonts::{
    ensure_cjk_fallback_font, load_font_faces, load_fonts_for_uncovered_chars,
    load_missing_system_fonts, warn_font_without_outlines, warn_uncovered_chars, Font,
    FontCollection, SystemFonts,
};
use crate::html::{
    collect_anchor_targets, find_base_href, find_document_title, Dom, NodeData, NodeId,
    StreamingParser,
};
use crate::img::{DocumentImageCache, ImageFetcher};
use crate::layout::{
    build_box_for_element, collect_completed_subtree_roots, has_visible_decoration,
    layout_document_from, paginate_document, paginate_document_with_absolutes,
    resolve_background_images, resolve_border, resolve_images, resolve_lpa_or_zero,
    resolve_padding, resolve_width_and_horizontal_margins, EdgeSizes, LaidOutBox, LaidOutContent,
    PageSettings, Rect, StreamingPaginator,
};
use crate::pdf::{
    anchor_destination_name, warn_about_inline_svg, ImageAssetCache, LinkSettings, PageOverlay,
    PdfOutputOptions, PreparedImage, StreamingPdfWriter, SvgFontDb,
};
use crate::sink::Sink;
use crate::style::{
    compute_single_element_style, compute_styles, compute_styles_with_parent,
    extract_author_stylesheet, needs_preceding_siblings, resolve_page_rules, rules_use_page_count,
    streaming_unsafe_selectors, user_agent_stylesheet, ComputedStyle, LengthPercentageOrAuto,
    PageRule, RgbaColor, Stylesheet,
};
use crate::style::{FontStyle, FontWeight};

/// 一括処理かストリーミング処理かを選択する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Batch,
    Streaming,
}

/// CSSの汎用family名のうち、実体を
/// 明示指定できるもの。`cursive`/`fantasy`は対象外。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericFamily {
    SansSerif,
    Serif,
    Monospace,
}

impl GenericFamily {
    /// CSSで書かれる名前。この名前でフォントコレクションへ登録する。
    pub fn css_name(self) -> &'static str {
        match self {
            Self::SansSerif => "sans-serif",
            Self::Serif => "serif",
            Self::Monospace => "monospace",
        }
    }
}

/// `--font`相当の明示的なフォント指定。
pub struct FontSpec {
    pub path: PathBuf,
    /// TrueType Collection(`.ttc`)等、複数フェイスを含むファイルのフェイス番号。
    pub index: u32,
}

/// レンダリング内容の挙動を変えるオプション
///
/// PDFの書き出し方だけを変える[`crate::pdf::PdfOutputOptions`]と対になる、
/// 「何を描くか」側の設定。
#[derive(Debug, Clone)]
pub struct ContentOptions {
    /// `<img>`とCSS`background-image`を読み込むか(`--no-images`でfalse)。
    pub load_images: bool,
    /// 要素の背景(色・画像)を描くか(`--no-background`でfalse)。
    pub draw_backgrounds: bool,
    /// ユーザーオリジンのCSS(`--user-style-sheet`)。UAスタイルシートの
    /// 後ろへ連結する(UAより強く、著者CSSより弱い位置)。
    pub user_stylesheets: Vec<String>,
    /// 算出`font-size`の下限(`--minimum-font-size`)。
    pub minimum_font_size: Option<f32>,
    /// 外部リンクの注釈を出すか(`--disable-external-links`でfalse)。
    pub external_links: bool,
    /// 内部リンク(`#id`)の注釈を出すか(`--disable-internal-links`でfalse)。
    pub internal_links: bool,
    /// 相対URLの外部リンクを`<base href>`で絶対化せずそのまま書くか
    /// (`--keep-relative-links`でtrue)。
    pub keep_relative_links: bool,
    /// 画像・CSS・フォントの取得に失敗したら中断するか
    /// (`--load-media-error-handling abort`)。
    pub abort_on_media_error: bool,
}

impl Default for ContentOptions {
    fn default() -> Self {
        Self {
            load_images: true,
            draw_backgrounds: true,
            user_stylesheets: Vec::new(),
            minimum_font_size: None,
            external_links: true,
            internal_links: true,
            keep_relative_links: false,
            abort_on_media_error: false,
        }
    }
}

/// `Engine`の初期化オプション。
#[derive(Default)]
pub struct EngineOptions {
    pub mode: Mode,
    pub settings: PageSettings,
    /// `--font`相当の明示的なフォント指定(複数指定可)。
    pub fonts: Vec<FontSpec>,
    /// CSSの汎用family名(`sans-serif`/`serif`/`monospace`)の実体を明示指定する
    /// (`--gothic-font`/`--serif-font`/`--mono-font`相当)。指定した汎用名は
    /// そのフォントで最優先に解決され、未指定の汎用名はシステムフォントの
    /// 候補リスト([`crate::fonts`])で解決する。既定`font-family`(未指定)は
    /// これに関わらず`--font`のフォントへフォールバックする。
    pub generic_fonts: Vec<(GenericFamily, FontSpec)>,
    /// `@font-face`の`src: url(...)`を相対解決する基準ディレクトリ。
    /// 入力がファイルに対応しない場合(Rackボディ等)は`None`でよく、
    /// その場合はカレントディレクトリを基準にする。`<img src>`のローカル
    /// 相対パス解決にも同じ基準ディレクトリを使う。
    pub base_dir: Option<PathBuf>,
    /// 相対参照の解決基準URL(`--base-url`相当)。HTMLに`<base href>`が
    /// あればそちらが優先される(この値はその既定を外から与えるもの)。
    /// http(s)のURLを想定し、ローカルディレクトリを基準にしたい場合は
    /// `base_dir`を使う。
    pub base_href: Option<String>,
    /// `<img src>`・`<link rel=stylesheet href>`のhttp(s)絶対URLフェッチを
    /// 許可するか。既定`false`(「既定無効・明示オプトイン」方針。画像・外部
    /// スタイルシート双方をこの1つのフラグで統括する)。ローカル相対パス・
    /// `data:`URIはこの値に関わらず常に許可する。
    pub allow_remote_assets: bool,
    /// PDF書き出しオプション(メタデータ・圧縮・スケール・グレースケール)。
    pub output: PdfOutputOptions,
    /// 描画内容の挙動([`ContentOptions`])。
    pub content: ContentOptions,
    /// ローカルファイル参照の可否と許可ディレクトリ
    /// (`--enable/disable-local-file-access`・`--allow`)。
    /// 既定はCLIの従来挙動どおり「許可・ディレクトリ制限なし」。
    pub local_access: LocalAccess,
    /// `--header-html`/`--footer-html`のテンプレート。
    pub header_footer_html: HeaderFooterHtml,
    /// `--cover`のHTML(プレースホルダ展開済み)。
    pub cover_html: Option<String>,
    /// 目次の設定。
    pub toc: TocSettings,
    /// `--page-offset`。TOC・本文のページ番号の起点をずらす。
    pub page_offset: usize,
    /// CLIのヘッダー/フッター簡易オプションから合成した`@page`ルール。著者
    /// CSSのページルールより前に置かれるため、同じmargin boxを著者が
    /// 宣言していればそちらが勝つ。
    pub extra_page_rules: Vec<PageRule>,
    /// 変換を打ち切る時刻。`None`なら無制限(CLIの既定)。
    ///
    /// HTTPサーバモードが`--timeout`から与える。1リクエストが際限なく
    /// ワーカーを占有するのを防ぐためのもの。
    ///
    /// 判定はチャンク投入ごと・トップレベル要素ごと・ページ書き出しごとに
    /// 行う。レイアウトの1回の呼び出しの内側までは見ないので、超過に
    /// 気づくのは最大でその1区間ぶん遅れる。
    pub deadline: Option<std::time::Instant>,
}

/// ローカルファイル参照の許可設定。
#[derive(Debug, Clone)]
pub struct LocalAccess {
    pub allow: bool,
    /// 空でなければ、この配下のファイルだけを読める。
    pub allowed_dirs: Vec<PathBuf>,
}

impl Default for LocalAccess {
    fn default() -> Self {
        Self {
            allow: true,
            allowed_dirs: Vec::new(),
        }
    }
}

/// `--header-html`/`--footer-html`のテンプレート。
///
/// 中身はプレースホルダ展開前のHTMLテキスト。ページ番号を含む場合は
/// ページごとに展開してレイアウトし直す。
#[derive(Debug, Clone, Default)]
pub struct HeaderFooterHtml {
    pub header: Option<String>,
    pub footer: Option<String>,
    /// ページごとに値が変わるプレースホルダ(`[page]`/`[topage]`)の展開値を
    /// 埋めるための、文書単位で決まる値。
    pub placeholders: HeaderFooterPlaceholders,
}

/// プレースホルダの展開値(CLI層の`PlaceholderValues`から詰め替えたもの)。
/// コアがCLI層に依存しないよう、必要な値だけを持つ素朴な型にしている。
#[derive(Debug, Clone, Default)]
pub struct HeaderFooterPlaceholders {
    /// `[page]`/`[topage]`以外を展開済みにしたテキストを作る関数の代わりに、
    /// 展開済みのテンプレートをそのまま受け取る運用にする。
    /// ここにはページ番号だけを差し込むための素材を持つ。
    pub page_token: String,
    pub total_pages_token: String,
}

impl HeaderFooterHtml {
    pub fn is_empty(&self) -> bool {
        self.header.is_none() && self.footer.is_none()
    }

    /// ページ番号のプレースホルダを含むか(含まなければレイアウト結果を
    /// ページ間で使い回せる)。
    pub fn depends_on_page(&self) -> bool {
        [self.header.as_deref(), self.footer.as_deref()]
            .into_iter()
            .flatten()
            .any(|html| {
                html.contains(&self.placeholders.page_token)
                    || html.contains(&self.placeholders.total_pages_token)
            })
    }

    /// `[topage]`(総ページ数)を使っているか。`Mode::Streaming`では値が
    /// 定まらないためエラーにする。
    pub fn uses_total_pages(&self) -> bool {
        [self.header.as_deref(), self.footer.as_deref()]
            .into_iter()
            .flatten()
            .any(|html| html.contains(&self.placeholders.total_pages_token))
    }

    fn expand(&self, template: &str, page: usize, total_pages: Option<usize>) -> String {
        let total = total_pages.map(|t| t.to_string()).unwrap_or_default();
        template
            .replace(&self.placeholders.page_token, &page.to_string())
            .replace(&self.placeholders.total_pages_token, &total)
    }
}

/// ヘッダー(`top = true`)またはフッター用に、余白領域を基準とした
/// `PageSettings`とクリップ矩形を作る。
fn overlay_area(settings: &PageSettings, top: bool) -> (PageSettings, Rect) {
    let size = settings.size;
    let (margin, clip) = if top {
        (
            EdgeSizes {
                top: 0.0,
                right: settings.margin.right,
                bottom: size.height - settings.margin.top,
                left: settings.margin.left,
            },
            Rect {
                x: settings.margin.left,
                y: 0.0,
                width: settings.content_width(),
                height: settings.margin.top,
            },
        )
    } else {
        (
            EdgeSizes {
                top: size.height - settings.margin.bottom,
                right: settings.margin.right,
                bottom: 0.0,
                left: settings.margin.left,
            },
            Rect {
                x: settings.margin.left,
                y: size.height - settings.margin.bottom,
                width: settings.content_width(),
                height: settings.margin.bottom,
            },
        )
    };
    (PageSettings { size, margin }, clip)
}

/// ヘッダー/フッターHTMLを1つ、余白領域向けにレイアウトして
/// [`PageOverlay`]にする。
///
/// 画像は非対応(`ImageAssetCache`を渡していないため
/// `<img>`は空のボックスになる)。テキスト・枠線・背景色は本文と同じ
/// パイプラインで描かれる。
fn layout_overlay(
    html: &str,
    fonts: &FontCollection,
    settings: &PageSettings,
    top: bool,
    fetcher: &ImageFetcher,
    cache: &DocumentImageCache,
) -> Option<PageOverlay> {
    let (area_settings, clip) = overlay_area(settings, top);
    if area_settings.content_height() <= 0.0 || area_settings.content_width() <= 0.0 {
        return None;
    }

    let dom = crate::html::parse(html.as_bytes());
    let ua = user_agent_stylesheet();
    let author = extract_author_stylesheet(&dom, fetcher, cache);
    let styles = compute_styles(&dom, &ua, &author);
    let pages = paginate_document(&dom, &styles, fonts, &area_settings);
    let boxes = pages.into_iter().next().map(|page| page.boxes)?;
    if boxes.is_empty() {
        return None;
    }

    Some(PageOverlay {
        boxes,
        styles,
        settings: area_settings,
        clip,
    })
}

/// ヘッダー/フッターHTML用のフェッチャ。外部リソースは取得しない
/// (インラインの`<style>`とテキストだけを対象にする。既知の限界)。
fn overlay_fetcher() -> ImageFetcher {
    ImageFetcher::new(PathBuf::from("."), false).with_local_access(false, Vec::new())
}

/// このページに重ねるヘッダー/フッターのオーバーレイを作る。
#[allow(clippy::too_many_arguments)]
fn build_page_overlays(
    html: &HeaderFooterHtml,
    fonts: &FontCollection,
    settings: &PageSettings,
    page_number: usize,
    total_pages: Option<usize>,
    fetcher: &ImageFetcher,
    cache: &DocumentImageCache,
    cached: &mut Option<Vec<PageOverlay>>,
) -> Vec<PageOverlay> {
    // ページ番号を含まないなら初回のレイアウトを使い回す。
    if !html.depends_on_page() {
        if let Some(overlays) = cached.as_ref() {
            return overlays.clone();
        }
    }

    let mut overlays = Vec::new();
    for (template, top) in [(&html.header, true), (&html.footer, false)] {
        let Some(template) = template else { continue };
        let text = html.expand(template, page_number, total_pages);
        if let Some(overlay) = layout_overlay(&text, fonts, settings, top, fetcher, cache) {
            overlays.push(overlay);
        }
    }
    if !html.depends_on_page() {
        *cached = Some(overlays.clone());
    }
    overlays
}

/// 見出しの一覧から目次のHTMLを組み立てる関数(CLI層(`cli::toc`)が
/// 実装して渡す)。
pub type TocHtmlBuilder = Rc<dyn Fn(&[TocHeading]) -> String>;

/// 目次(`--toc`)の設定。
///
/// 見た目に関わる値はCLI層(`cli::toc::TocOptions`)が組み立てたCSS/HTMLへ
/// 反映されるため、コア側は「有効かどうか」と「HTML組み立て関数」だけを持つ。
#[derive(Clone)]
pub struct TocSettings {
    pub enabled: bool,
    /// 見出しの一覧からTOCのHTMLを組み立てる関数。CLI層が実装したものを渡す。
    pub build_html: TocHtmlBuilder,
    /// 見出しから目次へ戻るリンクを張るか(`--enable-toc-back-links`)。
    pub back_links: bool,
}

impl Default for TocSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            build_html: Rc::new(|_| String::new()),
            back_links: false,
        }
    }
}

impl std::fmt::Debug for TocSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TocSettings")
            .field("enabled", &self.enabled)
            .field("back_links", &self.back_links)
            .finish_non_exhaustive()
    }
}

/// 目次に載せる見出し1件。
#[derive(Debug, Clone, PartialEq)]
pub struct TocHeading {
    /// `h1`=1 … `h6`=6。
    pub level: u8,
    pub title: String,
    /// 本文内での0始まりのページ番号。表示番号は
    /// `body_page + 1 + TOCページ数 + page_offset`。
    pub body_page: usize,
    /// リンク先の名前付き宛先。
    pub anchor: String,
}

/// 本文のページ列から`h1`〜`h6`を拾い、そのページ番号とアンカー名を集める。
///
/// `id`を持たない見出しには`__sgtoc_<連番>`を
/// 自動で振り、`anchor_names`へ追加する。
fn collect_headings(
    dom: &Dom,
    pages: &[crate::layout::Page],
    anchor_names: &mut HashMap<NodeId, String>,
) -> Vec<TocHeading> {
    fn heading_level(dom: &Dom, node: NodeId) -> Option<u8> {
        let NodeData::Element { name, .. } = &dom.node(node).data else {
            return None;
        };
        match &*name.local {
            "h1" => Some(1),
            "h2" => Some(2),
            "h3" => Some(3),
            "h4" => Some(4),
            "h5" => Some(5),
            "h6" => Some(6),
            _ => None,
        }
    }

    fn text_of(dom: &Dom, node: NodeId, out: &mut String) {
        match &dom.node(node).data {
            NodeData::Text { contents } => out.push_str(contents),
            NodeData::Element { .. } => {
                for child in dom.children(node) {
                    text_of(dom, child, out);
                }
            }
            _ => {}
        }
    }

    fn walk(
        dom: &Dom,
        b: &LaidOutBox,
        page_index: usize,
        seen: &mut Vec<NodeId>,
        out: &mut Vec<(NodeId, u8, usize)>,
    ) {
        if let Some(node) = b.node {
            if let Some(level) = heading_level(dom, node) {
                if !seen.contains(&node) {
                    seen.push(node);
                    out.push((node, level, page_index));
                }
            }
        }
        // 子の辿り方は`pdf::document::collect_link_areas`と同じ構造。
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for child in children {
                    walk(dom, child, page_index, seen, out);
                }
            }
            LaidOutContent::Grid(grid) => {
                for child in grid.rows.iter().flat_map(|row| &row.items) {
                    walk(dom, child, page_index, seen, out);
                }
            }
            LaidOutContent::Table(table) => {
                if let Some(caption) = &table.caption {
                    walk(dom, caption, page_index, seen, out);
                }
                for row in &table.rows {
                    for cell in &row.cells {
                        walk(dom, cell, page_index, seen, out);
                    }
                }
            }
            LaidOutContent::Inline(lines) => {
                for line in lines {
                    for atomic in &line.atomics {
                        walk(dom, &atomic.content, page_index, seen, out);
                    }
                }
            }
            _ => {}
        }
    }

    let mut found: Vec<(NodeId, u8, usize)> = Vec::new();
    let mut seen: Vec<NodeId> = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        for b in &page.boxes {
            walk(dom, b, index, &mut seen, &mut found);
        }
    }

    found
        .into_iter()
        .enumerate()
        .map(|(i, (node, level, body_page))| {
            let anchor = match anchor_names.get(&node) {
                Some(existing) => existing.clone(),
                None => {
                    // `id`が無い見出しには自動で宛先名を振る。
                    let name = anchor_destination_name(&format!("__sgtoc_{i}"));
                    anchor_names.insert(node, name.clone());
                    name
                }
            };
            let mut title = String::new();
            text_of(dom, node, &mut title);
            TocHeading {
                level,
                title: title.split_whitespace().collect::<Vec<_>>().join(" "),
                body_page,
                anchor,
            }
        })
        .collect()
}

/// 独立したHTMLドキュメント(cover/TOC)をレイアウトしてページ列にする。
/// 外部リソースは取得しない(ヘッダー/フッターと同じ制約)。
fn render_standalone_document(
    html: &str,
    fonts: &FontCollection,
    settings: &PageSettings,
) -> Vec<crate::layout::Page> {
    let dom = crate::html::parse(html.as_bytes());
    let ua = user_agent_stylesheet();
    let author = extract_author_stylesheet(&dom, &overlay_fetcher(), &DocumentImageCache::new());
    let styles = compute_styles(&dom, &ua, &author);
    paginate_document(&dom, &styles, fonts, settings)
}

/// 目次のページ列を、ページ数が収束するまで組み立て直す。
///
/// 戻り値は(TOCのページ列, TOCドキュメントのスタイル)。TOCは独立ドキュメント
/// なので、描画にはそのスタイルマップが要る。
fn build_toc_pages(
    headings: &[TocHeading],
    toc: &TocSettings,
    page_offset: usize,
    fonts: &FontCollection,
    settings: &PageSettings,
) -> (Vec<crate::layout::Page>, HashMap<NodeId, Rc<ComputedStyle>>) {
    const MAX_ROUNDS: usize = 3;

    let mut toc_page_count = 1;
    let mut result = (Vec::new(), HashMap::new());

    for round in 0..MAX_ROUNDS {
        let numbered: Vec<TocHeading> = headings
            .iter()
            .map(|h| TocHeading {
                body_page: h.body_page + 1 + toc_page_count + page_offset,
                ..h.clone()
            })
            .collect();
        let html = (toc.build_html)(&numbered);

        let dom = crate::html::parse(html.as_bytes());
        let ua = user_agent_stylesheet();
        let author =
            extract_author_stylesheet(&dom, &overlay_fetcher(), &DocumentImageCache::new());
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, fonts, settings);

        let converged = pages.len() == toc_page_count;
        toc_page_count = pages.len().max(1);
        result = (pages, styles);
        if converged {
            return result;
        }
        if round + 1 == MAX_ROUNDS {
            eprintln!(
                "警告: 目次のページ数が収束しませんでした(最後の結果を使います)。\n  \
                 目次のページ番号が1ページ分ずれる可能性があります"
            );
        }
    }
    result
}

/// `--font`で明示されたフォントを読む。
fn load_explicit_fonts<E>(specs: &[FontSpec]) -> Result<Vec<Font>, EngineError<E>> {
    let mut loaded = Vec::with_capacity(specs.len());
    for spec in specs {
        let font = Font::load_indexed(&spec.path, spec.index)
            .map_err(|e| EngineError::Font(format!("フォントの読み込みに失敗しました: {e}")))?;
        // 明示指定でも、輪郭を持たないフォントは採らない。埋め込んでも
        // 何も描かれないうえ、サブセット化が効かずPDFだけが膨らむため。
        if !font.can_render() {
            warn_font_without_outlines(&spec.path.display().to_string());
            continue;
        }
        loaded.push(font);
    }
    Ok(loaded)
}

/// `--font`・`@font-face`・システムフォント探索をすべて終えてもフォントが
/// 1つも無い場合に、システムの`sans-serif`候補を既定フォントとして補う。
///
/// フォントが1つも無いと、`font-family`未指定のテキスト(既定`font-family`は
/// 空)の描画先が無くなる。`--font`を必須にせずシステムフォントで埋める
/// ことで、wkhtmltopdfと同じ使い心地にしている(その代わり、何も
/// 指定しなかった場合の出力は実行環境に依存する)。
///
/// `@font-face`でフォントが供給されている場合は何もしない。ここで
/// 足してしまうとフェイスの並び順が変わってしまうため。
fn ensure_default_font<E>(
    fonts: &mut FontCollection,
    system: &SystemFonts,
) -> Result<(), EngineError<E>> {
    if !fonts.is_empty() {
        return Ok(());
    }
    match system.load_generic("sans-serif", FontWeight::Normal, FontStyle::Normal) {
        Some(font) => {
            fonts.push_font_face("sans-serif".to_string(), None, None, Vec::new(), font);
            Ok(())
        }
        None => Err(EngineError::Font(
            "使用できるフォントがありません(システムフォントが見つかりませんでした)。\n  \
             --fontでフォントファイルを指定してください"
                .to_string(),
        )),
    }
}

/// `Mode::Streaming`で`font-family`が解決できなかった場合に警告する。
///
/// ストリーミング処理では[`crate::pdf::StreamingPdfWriter`]が`new`の時点で
/// フォント数を固定するため、後から`font-family`名でシステムフォントを
/// 探して足すことができない(`load_missing_system_fonts`を呼べない)。
/// 該当する指定は黙って既定フォントで描画されるので、一度だけ警告する。
fn warn_unresolved_font_families(
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    warned: &mut Vec<String>,
) {
    for style in styles.values() {
        for family in &style.font_family {
            if fonts.has_matching_face(family, style.font_weight, style.font_style) {
                continue;
            }
            if warned.iter().any(|f| f == family) {
                continue;
            }
            warned.push(family.clone());
            eprintln!(
                "警告: font-family \"{family}\" はストリーミングモードでは解決できません\n  \
                 (フォントは処理開始時に確定させる必要があるため)。既定のフォントで描画します。\n  \
                 --font/--gothic-font/--serif-font/--mono-font か @font-face で明示してください"
            );
        }
    }
}

/// CLI由来の`@page`ルールを著者ルールの前に並べたものを返す。
fn page_rules_with_cli(extra: &[PageRule], author: &[PageRule]) -> Vec<PageRule> {
    let mut rules = extra.to_vec();
    rules.extend_from_slice(author);
    rules
}

/// ユーザーオリジンのCSSをUAスタイルシートの後ろへ連結する。
///
/// CSSのカスケードではユーザーオリジンは「UAより強く著者CSSより弱い」。
/// UAシートの末尾に置けば同オリジン内のソース順で後勝ちになり、著者CSSには
/// 負けるため、この近似で意図した強さになる(`!important`は未対応のため
/// 逆転の問題も起きない)。
fn append_user_stylesheets(ua: &mut Stylesheet, user_css: &[String]) {
    for css in user_css {
        let sheet = crate::style::parse_stylesheet(css);
        ua.rules.extend(sheet.rules);
    }
}

/// スタイル計算後の一括後処理(`--no-background`・`--minimum-font-size`)。
fn apply_content_options(
    styles: &mut HashMap<NodeId, Rc<ComputedStyle>>,
    content: &ContentOptions,
) {
    for shared in styles.values_mut() {
        // 共有されているスタイルを書き換えるので、必要なときだけ複製する。
        if !content.draw_backgrounds {
            let style = Rc::make_mut(shared);
            style.background_color = RgbaColor::TRANSPARENT;
            style.background_image = None;
        }
        if let Some(min) = content.minimum_font_size {
            if shared.font_size.0 < min {
                Rc::make_mut(shared).font_size.0 = min;
            }
        }
    }
}

/// `Engine`が返すエラー。`Sink`からのエラー(`Io`)、コア自身が判定する
/// 構造エラー(`UnsupportedInStreamingMode`)、フォント読み込みエラー
/// (`Font`)を区別する。
#[derive(Debug)]
pub enum EngineError<E> {
    Io(E),
    UnsupportedInStreamingMode(&'static str),
    Font(String),
    /// DOMのネストが[`crate::html::MAX_ELEMENT_DEPTH`]を超えた。
    ///
    /// 以降のスタイル計算・レイアウト・描画はいずれも深さぶん再帰するため、
    /// ここで止めないとスタックオーバーフローでプロセスごと落ちる。
    DepthLimitExceeded {
        depth: u32,
        limit: u32,
    },
    /// 保持しているノード数が[`crate::html::MAX_NODES`]を超えた。
    ///
    /// スタイル・ボックスツリー・レイアウト結果がノード数に比例して積み上がる
    /// ため、ここで止めないとメモリを食い潰す。
    NodeLimitExceeded {
        nodes: usize,
        limit: usize,
    },
    /// [`EngineOptions::deadline`]を過ぎたため打ち切った。
    TimedOut,
    /// `--load-media-error-handling abort`のときに、画像・外部CSS等の取得に失敗した。
    MediaLoad(String),
}

impl<E> From<E> for EngineError<E> {
    fn from(e: E) -> Self {
        Self::Io(e)
    }
}

impl<E: std::fmt::Display> std::fmt::Display for EngineError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::UnsupportedInStreamingMode(msg) => write!(f, "{msg}"),
            Self::Font(msg) => write!(f, "{msg}"),
            Self::DepthLimitExceeded { depth, limit } => write!(
                f,
                "HTMLのネストが深すぎます(深さ{depth}、上限{limit})。\n  \
                 入れ子を浅くするか、閉じタグの抜けがないか確認してください"
            ),
            Self::NodeLimitExceeded { nodes, limit } => write!(
                f,
                "HTMLの要素数が多すぎます(ノード数{nodes}、上限{limit})。\n  \
                 文書を分割するか、--streamingで逐次処理してください"
            ),
            Self::TimedOut => write!(f, "変換が制限時間を超えました"),
            Self::MediaLoad(msg) => write!(f, "リソースの取得に失敗しました: {msg}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for EngineError<E> {}

/// 深さとノード数の上限をまとめて確認する。
///
/// `Engine`のメソッドからも、`self`を持てない`finish_batch`の途中からも
/// 呼べるように独立した関数にしてある。
fn check_document_limits<E>(depth: u32, nodes: usize) -> Result<(), EngineError<E>> {
    if depth > crate::html::MAX_ELEMENT_DEPTH {
        return Err(EngineError::DepthLimitExceeded {
            depth,
            limit: crate::html::MAX_ELEMENT_DEPTH,
        });
    }
    if nodes > crate::html::MAX_NODES {
        return Err(EngineError::NodeLimitExceeded {
            nodes,
            limit: crate::html::MAX_NODES,
        });
    }
    Ok(())
}

/// `deadline`を過ぎていれば[`EngineError::TimedOut`]を返す。
fn check_deadline<E>(deadline: Option<std::time::Instant>) -> Result<(), EngineError<E>> {
    match deadline {
        Some(deadline) if std::time::Instant::now() >= deadline => Err(EngineError::TimedOut),
        _ => Ok(()),
    }
}

/// `Mode::Streaming`でのトップレベル要素処理に必要な、`<head>`閉じ時点
/// (`<body>`検出時点)で一度だけ確定する状態。
struct StreamingState<S: Sink> {
    ua: Stylesheet,
    author: Stylesheet,
    fonts: FontCollection,
    /// 処理済みの全トップレベル要素のスタイルを蓄積する、永続的なマップ。
    /// 1ページに複数のトップレベル要素のボックスが混在しうるため、
    /// `StreamingPdfWriter::write_page`はこの全体を必要とする。
    styles: HashMap<NodeId, Rc<ComputedStyle>>,
    /// `background-image`を持つ要素の、デコード済み画像を`NodeId`キーで
    /// 引けるようにする側マップ。`styles`と同じく処理済みトップレベル要素
    /// ぶんを蓄積する。
    background_images: HashMap<NodeId, Rc<PreparedImage>>,
    root_font_size: f32,
    /// CSSカウンタの状態。ドキュメント順に依存するため、トップレベル
    /// 要素をまたいで永続させる必要があり
    /// `root_font_size`と同じ位置づけで持つ。
    counters: HashMap<String, Vec<i32>>,
    /// `quotes`のネスト深度(木構造とは無関係な単一のカウンタ)。
    quote_depth: i32,
    /// `<body>`要素自身の計算スタイル。各トップレベル要素のスタイル計算の
    /// 親スタイルとして使う。
    body_style: ComputedStyle,
    /// `<body>`の`padding`/`border`/`margin`を反映した、トップレベル要素の
    /// containing width。
    content_width: f32,
    /// `<body>`の`margin-left`+`border-left`+`padding-left`。
    start_x: f32,
    /// 次に配置するトップレベル要素の開始Y座標(前の要素までの累積高さ)。
    cursor_y: f32,
    /// ページのジオメトリ(オーバーレイの領域計算に使う)。
    page_settings: PageSettings,
    /// ページ番号に依存しないヘッダー/フッターHTMLのレイアウト結果。
    overlay_cache: Option<Vec<PageOverlay>>,
    /// 解決できない`font-family`について警告済みの名前(同じ警告を
    /// 何度も出さないため)。
    warned_font_families: Vec<String>,
    /// どのフォントでも描画できず警告済みの文字。ストリーミングでは
    /// トップレベル要素ごとに判定するため、
    /// 既に警告した文字を持ち回って重複を防ぐ。
    warned_uncovered_chars: HashSet<char>,
    /// インラインの`<svg>`について既に警告したか(1文書につき1回だけ出す)。
    warned_inline_svg: bool,
    /// 処理済みトップレベル要素を、サブツリーごと解放してよいか。
    ///
    /// `+`/`~`や`:first-child`のように直前の兄弟が要るセレクタを使う文書では、
    /// 解放を子孫だけに絞って要素そのものは残す。残さないと、後続の要素から
    /// 見て「自分が最初の子」になってしまう。
    release_whole_subtree: bool,
    paginator: StreamingPaginator,
    writer: StreamingPdfWriter<S>,
    /// `<img>`のフェッチ・デコード結果を文書内でメモ化するキャッシュ。
    image_cache: ImageAssetCache,
}

/// 処理済みのトップレベル要素を解放する。
///
/// `whole`が`false`のときは子孫だけを解放し、要素そのものは残す。残した要素は
/// タグ名・クラス・idを保つので、後続の兄弟から「直前の兄弟」として見え続ける。
fn release_processed(mut dom: std::cell::RefMut<'_, Dom>, node: NodeId, whole: bool) {
    if whole {
        dom.release_subtree(node);
    } else {
        dom.release_descendants(node);
    }
}

/// HTMLチャンク投入からPDFバイト列書き出しまでを1つのAPIとして統合するコアの
/// エントリポイント。`--gothic-font`を`font-family: sans-serif`の実体として
/// 登録する。`push_font_face`で宣言family名`"sans-serif"`として追加するので、
/// `select_for_char`の通常のfamily一致でそのまま拾える。
/// `has_matching_face("sans-serif", ...)`が真になるため、後段の
/// `load_missing_system_fonts`はシステムのゴシック探索をスキップする。CSSの
/// 汎用family名として明示指定されたフォントを、
/// その汎用名で引けるように登録する。
fn register_generic_fonts<E>(
    fonts: &mut FontCollection,
    generic_fonts: &[(GenericFamily, FontSpec)],
) -> Result<(), EngineError<E>> {
    for (family, spec) in generic_fonts {
        let font = Font::load_indexed(&spec.path, spec.index).map_err(|e| {
            EngineError::Font(format!(
                "{}のフォントの読み込みに失敗しました: {e}",
                family.css_name()
            ))
        })?;
        if !font.can_render() {
            warn_font_without_outlines(&spec.path.display().to_string());
            continue;
        }
        fonts.push_font_face(family.css_name().to_string(), None, None, Vec::new(), font);
    }
    Ok(())
}

pub struct Engine<S: Sink> {
    options: EngineOptions,
    parser: StreamingParser,
    /// `Mode::Batch`では`finish`まで保持し続ける。`Mode::Streaming`では
    /// 最初のトップレベル要素処理の直前に`StreamingState::writer`へ
    /// 移動するため`None`になる。
    sink: Option<S>,
    streaming: Option<StreamingState<S>>,
}

impl<S: Sink> Engine<S> {
    pub fn new(options: EngineOptions, sink: S) -> Self {
        Self {
            options,
            parser: StreamingParser::new(),
            sink: Some(sink),
            streaming: None,
        }
    }

    /// パース済みの範囲のネストが上限に収まっているか確認する。
    ///
    /// DOMを再帰的に辿る処理より前に必ず通す。`feed`のたびに呼ぶが、深さは
    /// 木を組み立てながら更新済みの値を読むだけなのでコストはかからない。
    /// [`EngineOptions::deadline`]を過ぎていないか確認する。
    ///
    /// レイアウトの内側までは辿らないので、置けるのは「区切りのよい場所」
    /// だけになる。チャンク投入ごと・トップレベル要素ごと・ページ書き出し
    /// ごとに呼ぶ。
    fn check_deadline(&self) -> Result<(), EngineError<S::Error>> {
        check_deadline(self.options.deadline)
    }

    fn ensure_depth_within_limit(&self) -> Result<(), EngineError<S::Error>> {
        let dom = self.parser.dom();
        check_document_limits(dom.max_depth(), dom.node_count())
    }

    /// HTMLバイト列のチャンクを1つ投入する。何度でも呼べる。
    ///
    /// `Mode::Streaming`では、投入後に`<body>`より後の`<style>`タグが
    /// 検出された場合エラーを返す(モジュールdoc参照)。`Mode::Batch`では
    /// このチェックを行わず、DOMを蓄積するのみで実際の処理は`finish`まで
    /// 行わない。`Mode::Streaming`では、確定した`<body>`直下のトップレベル
    /// 要素をこの中で処理する。
    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), EngineError<S::Error>> {
        self.parser.feed(chunk);
        // DOMを辿る処理(この後の`find_base_href`等も含めて再帰する)より前に、
        // 積み上がった深さを確認する。パース自体はアリーナなので深くても安全。
        self.ensure_depth_within_limit()?;
        self.check_deadline()?;
        if self.options.mode == Mode::Streaming && self.parser.has_late_css_source() {
            return Err(EngineError::UnsupportedInStreamingMode(
                "<body>より後の<style>/<link rel=stylesheet>はストリーミングモードでは使えません\n  \
                 (既に書き出したページへ遡って適用できないため)。\n  \
                 これらを使う場合は --streaming を外してください",
            ));
        }

        if self.options.mode != Mode::Streaming {
            return Ok(());
        }

        self.ensure_streaming_state_initialized()?;
        if self.streaming.is_some() {
            let completed = self.parser.take_completed_top_level_children();
            for node in completed {
                self.process_top_level_element(node)?;
            }
        }
        Ok(())
    }

    /// `<body>`が検出されていて、まだ`StreamingState`を作っていなければ
    /// 作る。`sink`をここで`StreamingState::writer`へ移動する(以後
    /// `self.sink`は`None`になる)。
    fn ensure_streaming_state_initialized(&mut self) -> Result<(), EngineError<S::Error>> {
        if self.streaming.is_some() {
            return Ok(());
        }
        let Some(body) = self.parser.body_node() else {
            return Ok(());
        };
        let sink = self
            .sink
            .take()
            .expect("sinkはstreaming state初期化時に一度だけ取り出される");
        let state = self.init_streaming_state(body, sink)?;
        self.streaming = Some(state);
        Ok(())
    }

    /// `<head>`閉じ時点(`<body>`検出時点)で一度だけ行う初期化:
    /// フォント解決・`<html>`/`<body>`のスタイル計算・`<body>`の装飾
    /// チェック・`StreamingPdfWriter`の構築。
    fn init_streaming_state(
        &self,
        body: NodeId,
        sink: S,
    ) -> Result<StreamingState<S>, EngineError<S::Error>> {
        let mut ua = user_agent_stylesheet();
        append_user_stylesheets(&mut ua, &self.options.content.user_stylesheets);
        let base_dir = self
            .options
            .base_dir
            .as_deref()
            .unwrap_or_else(|| Path::new("."));
        // 外部スタイルシート(`<link>`)取得用のフェッチャー/キャッシュ。
        // 画像用の`ImageAssetCache`(下の`image_cache`)とは別インスタンスを
        // 持つ。`<base href>`は`<head>`に現れるため、この時点(最初の
        // トップレベル要素が確定した時点)で既にパース済み。
        let base_href =
            find_base_href(&self.parser.dom()).or_else(|| self.options.base_href.clone());
        let css_fetcher =
            ImageFetcher::new(base_dir.to_path_buf(), self.options.allow_remote_assets)
                .with_base_href(base_href.clone())
                .with_local_access(
                    self.options.local_access.allow,
                    self.options.local_access.allowed_dirs.clone(),
                );
        let css_cache = DocumentImageCache::new();
        let author = {
            let dom = self.parser.dom();
            extract_author_stylesheet(&dom, &css_fetcher, &css_cache)
        };
        let page_rules = page_rules_with_cli(&self.options.extra_page_rules, &author.page_rules);
        let page_settings = apply_page_rule_settings_override(self.options.settings, &page_rules);
        if rules_use_page_count(&page_rules) {
            return Err(EngineError::UnsupportedInStreamingMode(
                "@pageのマージンボックスの counter(pages) はストリーミングモードでは使えません\n  \
                 (総ページ数は1パスでは決まらないため)。\n  \
                 これを使う場合は --streaming を外してください",
            ));
        }
        // `--header-html`/`--footer-html`の`[topage]`も同じ理由で使えない。
        if self.options.header_footer_html.uses_total_pages() {
            return Err(EngineError::UnsupportedInStreamingMode(
                "--header-html/--footer-html の [topage] はストリーミングモードでは使えません\n  \
                 (総ページ数は1パスでは決まらないため)。\n  \
                 これを使う場合は --streaming を外してください",
            ));
        }
        // 目次は本文全体のページ分割が終わらないと作れない。
        if self.options.toc.enabled {
            return Err(EngineError::UnsupportedInStreamingMode(
                "--toc はストリーミングモードでは使えません\n  \
                 (目次には本文のページ番号が要るため)。\n  \
                 これを使う場合は --streaming を外してください",
            ));
        }
        // 後方参照セレクタは常に非マッチになる。エラーにはしないが、黙って
        // 結果が変わるのは避けたいので警告する。
        let unsafe_selectors = streaming_unsafe_selectors(&author);
        if !unsafe_selectors.is_empty() {
            eprintln!(
                "警告: {} はストリーミングモードでは結果が変わります\n  \
                 (<body>直下の要素は、前後の兄弟が揃う前に確定するため)。\n  \
                 これらを使う場合は --streaming を外してください",
                unsafe_selectors.join(", ")
            );
        }

        let system_fonts = SystemFonts::scan();
        let mut fonts = FontCollection::new(load_explicit_fonts(&self.options.fonts)?);

        register_generic_fonts(&mut fonts, &self.options.generic_fonts)?;
        for loaded in load_font_faces(&author.font_faces, &css_fetcher, &system_fonts) {
            fonts.push_font_face(
                loaded.family,
                Some(loaded.weight),
                Some(loaded.style),
                loaded.unicode_range,
                loaded.font,
            );
        }
        // `load_missing_system_fonts`・`load_fonts_for_uncovered_chars`は
        // 文書全体のスタイル(や文字)を必要とするが、真のストリーミング処理では
        // 文書全体を一度に持たないため、ここでは呼ばない。
        // 代わりに、フォントが何も与えられていない場合は
        // 既定フォント(ラテン)に加えてCJKカバー用のフォントを先回りで足す。
        // `--font`/`@font-face`でフォントが供給されている場合に勝手に足さない
        // のは、フェースの並び順(`unicode-range`先勝ち)と「`--font`で渡した
        // フォントが既定になる」原則への影響を避けるため。
        let had_no_fonts = fonts.is_empty();
        ensure_default_font(&mut fonts, &system_fonts)?;
        if had_no_fonts {
            ensure_cjk_fallback_font(&mut fonts, &system_fonts);
        }

        // CSSカウンタ・quote深度はドキュメント順に依存する状態なので、
        // <html>から<body>直下の各トップレベル要素まで一貫して同じ状態を引き
        // 継ぐ(以後`StreamingState`が永続させる)。
        let mut counters = HashMap::new();
        let mut quote_depth = 0;
        let (html_style, body_style, root_font_size) = {
            let dom = self.parser.dom();
            let html_id = dom
                .parent(body)
                .expect("<body>には親要素(<html>)があるはず");
            let default_root_font_size = ComputedStyle::default().font_size.0;
            let html_style = compute_single_element_style(
                &dom,
                html_id,
                None,
                default_root_font_size,
                &ua,
                &author,
                &mut counters,
                &mut quote_depth,
            );
            let root_font_size = html_style.font_size.0;
            let body_style = compute_single_element_style(
                &dom,
                body,
                Some(&html_style),
                root_font_size,
                &ua,
                &author,
                &mut counters,
                &mut quote_depth,
            );
            (html_style, body_style, root_font_size)
        };
        let _ = html_style;

        let body_border = resolve_border(&body_style);
        if has_visible_decoration(&body_style, &body_border) {
            return Err(EngineError::UnsupportedInStreamingMode(
                "背景色・枠線を持つ<body>はストリーミングモードでは使えません\n  \
                 (複数ページにまたがる装飾を再現できないため)。\n  \
                 これらを使う場合は --streaming を外してください",
            ));
        }

        // `<a href="#id">`の宛先候補。`Mode::Streaming`ではこの時点(最初の
        // トップレベル要素が確定した時点)までにパースできた範囲しか
        // 見えないが、宛先は「そのページを書き出す時に見つかったボックス」
        // から記録されるため、後からパースされる要素も対象になる(ここで
        // 集めるのは`id`の一覧ではなく、「どの
        // ノードがどの名前か」の対応表であるため)。
        let anchor_names: HashMap<NodeId, String> = collect_anchor_targets(&self.parser.dom())
            .into_iter()
            .map(|(node, id)| (node, anchor_destination_name(&id)))
            .collect();

        let page_width = page_settings.content_width();
        let body_padding = resolve_padding(&body_style, page_width);
        let (body_content_width, body_margin_left, _) = resolve_width_and_horizontal_margins(
            &body_style,
            page_width,
            body_padding.left + body_padding.right,
            body_border.left + body_border.right,
        );
        let start_x = body_margin_left + body_border.left + body_padding.left;
        let start_y = resolve_lpa_or_zero(body_style.margin_top, page_width)
            + body_border.top
            + body_padding.top;

        // `--title`未指定なら`<title>`をPDFの`/Title`に使う。
        let mut output = self.options.output.clone();
        output
            .metadata
            .fill_title_from_document(find_document_title(&self.parser.dom()));

        let writer = StreamingPdfWriter::with_options(
            &fonts,
            page_settings,
            sink,
            page_rules.clone(),
            LinkSettings {
                anchor_names: anchor_names.clone(),
                base_href: base_href.clone(),
                external: self.options.content.external_links,
                internal: self.options.content.internal_links,
                keep_relative: self.options.content.keep_relative_links,
            },
            output,
        )
        .map_err(EngineError::Io)?;
        let image_cache = ImageAssetCache::with_fetcher(
            ImageFetcher::new(base_dir.to_path_buf(), self.options.allow_remote_assets)
                .with_base_href(base_href)
                .with_local_access(
                    self.options.local_access.allow,
                    self.options.local_access.allowed_dirs.clone(),
                ),
        )
        // SVG内の`<text>`は文書と同じフォントで描く。`fonts`はここまでで
        // 出揃っている(以降は変更しない)ので、この時点で組める。
        .with_svg_fonts(SvgFontDb::from_collection(&fonts));

        // 直前の兄弟が要るセレクタを使っていない文書では、従来どおり
        // サブツリーごと解放する(要素を残すとトップレベル要素1個につき
        // 1ノードが積み上がるため、要らないなら残さない)。
        let release_whole_subtree = !needs_preceding_siblings(&author);

        Ok(StreamingState {
            ua,
            author,
            fonts,
            styles: HashMap::new(),
            background_images: HashMap::new(),
            root_font_size,
            counters,
            quote_depth,
            body_style,
            content_width: body_content_width,
            start_x,
            cursor_y: start_y,
            page_settings,
            overlay_cache: None,
            warned_font_families: Vec::new(),
            warned_uncovered_chars: HashSet::new(),
            warned_inline_svg: false,
            release_whole_subtree,
            paginator: StreamingPaginator::new(page_settings.content_height()),
            writer,
            image_cache,
        })
    }

    /// 確定した1つのトップレベル要素(`<body>`直下の子)を、スタイル計算・
    /// レイアウト・ページ分割・PDF書き出し・DOM解放まで処理する。
    fn process_top_level_element(&mut self, node: NodeId) -> Result<(), EngineError<S::Error>> {
        // 1要素ぶんのレイアウトと書き出しに入る前に確認する。
        self.check_deadline()?;
        let Engine {
            parser,
            streaming,
            options,
            ..
        } = self;
        let options_content = &options.content;
        let state = streaming
            .as_mut()
            .expect("process_top_level_elementはstreaming state初期化後にのみ呼ばれる");

        let (sub_styles, item_box) = {
            let dom = parser.dom();
            let sub_styles = compute_styles_with_parent(
                &dom,
                node,
                &state.body_style,
                state.root_font_size,
                &state.ua,
                &state.author,
                &mut state.counters,
                &mut state.quote_depth,
            );
            let mut sub_styles = sub_styles;
            apply_content_options(&mut sub_styles, options_content);
            warn_unresolved_font_families(
                &sub_styles,
                &state.fonts,
                &mut state.warned_font_families,
            );
            // ストリーミングでは文字ベースのフォント補完ができないので、
            // 描画できない文字が出たら都度警告する。
            warn_uncovered_chars(
                &state.fonts,
                &dom,
                &sub_styles,
                &mut state.warned_uncovered_chars,
            );
            // このトップレベル要素の中だけを見る(文書全体を毎回走査すると
            // 要素数の2乗になる)。
            warn_about_inline_svg(&dom, node, &mut state.warned_inline_svg);
            let mut item_box = build_box_for_element(&dom, &sub_styles, node);
            if let (Some(item_box), true) = (&mut item_box, options_content.load_images) {
                resolve_images(item_box, &dom, &state.image_cache);
            }
            (sub_styles, item_box)
        };
        if options_content.load_images {
            state
                .background_images
                .extend(resolve_background_images(&sub_styles, &state.image_cache));
        }
        state.styles.extend(sub_styles);

        let Some(item_box) = item_box else {
            // `display: none`などでボックスを生成しない要素。
            release_processed(parser.dom_mut(), node, state.release_whole_subtree);
            return Ok(());
        };

        let laid_out = layout_document_from(
            &item_box,
            &state.styles,
            &state.fonts,
            state.content_width,
            state.start_x,
            state.cursor_y,
        );
        state.cursor_y += laid_out.layout.margin_box_height();

        // レイアウトはすでに完了しており、これ以降このDOMサブツリー
        // (テキスト内容・属性等)が再度読まれることはないため、ページの
        // flushを待たずに即座に解放してよい(`ComputedStyle`は`state.styles`
        // に別途保持済み)。
        release_processed(parser.dom_mut(), node, state.release_whole_subtree);

        // このトップレベル要素自体が装飾(背景・枠線・background-image。
        // `has_visible_decoration`はbackground-imageも見る)を持たない場合、
        // `place_split`は装飾フラグメントを生成しないため、このノード自体が
        // `page.boxes`に現れることはない。つまり`node`自身の`ComputedStyle`/
        // 背景画像はこの後`write_page`から一切参照されないため、ここで即座に
        // 削除してよい(装飾を持つ場合は、装飾フラグメントが実際に配置された
        // ページのflush時に、下の
        // `collect_completed_subtree_roots`経由で削除される)。
        if !laid_out.has_visible_decoration {
            state.styles.remove(&node);
            state.background_images.remove(&node);
        }

        let mut laid_out = laid_out;
        let pages = state.paginator.push_item(&mut laid_out);
        for page in &pages {
            if !options.header_footer_html.is_empty() {
                let page_number = state.writer.page_count() + 1;
                // `Mode::Streaming`では総ページ数が
                // 不明なので`[topage]`は空になる。
                let overlays = build_page_overlays(
                    &options.header_footer_html,
                    &state.fonts,
                    &state.page_settings,
                    page_number,
                    None,
                    &overlay_fetcher(),
                    &DocumentImageCache::new(),
                    &mut state.overlay_cache,
                );
                state.writer.set_page_overlays(overlays);
            }
            state
                .writer
                // `Mode::Streaming`は総ページ数を原理的に知りえないため常に`None`
                // (`counter(pages)`使用時は`init_streaming_state`で事前に
                // エラーを返している)。
                .write_page(
                    page,
                    &state.styles,
                    &state.background_images,
                    &state.fonts,
                    None,
                )
                .map_err(EngineError::Io)?;
        }

        // 各ページに実際に配置され、これ以上分割されない
        // (`FragmentPosition::Whole`/`Last`)子孫ノードの`ComputedStyle`/
        // 背景画像を解放する。DOM自体は上ですでにタブストーン化済みだが、
        // 木構造のリンクは保持されているため`Dom::children`で辿れる。
        let dom = parser.dom();
        for page in &pages {
            for root in collect_completed_subtree_roots(page) {
                remove_subtree_styles(&dom, root, &mut state.styles, &mut state.background_images);
            }
        }
        drop(dom);

        Ok(())
    }

    /// 残りの処理をすべて行い、`sink`へ書き出す。
    ///
    /// `Mode::Batch`ではDOM確定後に一括処理する。`Mode::Streaming`では、
    /// まだ処理していない(保留中だった最後の要素を含む)トップレベル要素を
    /// すべて処理してから、`StreamingPdfWriter::finish`でフォント埋め込み・
    /// xref/trailerを書き出す。
    pub fn finish(mut self) -> Result<S::Output, EngineError<S::Error>> {
        if self.options.mode != Mode::Streaming {
            return self.finish_batch();
        }

        self.ensure_depth_within_limit()?;
        self.check_deadline()?;
        self.ensure_streaming_state_initialized()?;
        let remaining = self.parser.take_all_remaining_top_level_children();
        for node in remaining {
            self.process_top_level_element(node)?;
        }

        match self.streaming {
            Some(state) => {
                let StreamingState {
                    styles,
                    background_images,
                    fonts,
                    mut writer,
                    paginator,
                    image_cache,
                    page_settings,
                    mut overlay_cache,
                    ..
                } = state;
                if self.options.content.abort_on_media_error {
                    if let Some(err) = image_cache.had_errors() {
                        return Err(EngineError::MediaLoad(err));
                    }
                }
                for page in paginator.finish() {
                    if !self.options.header_footer_html.is_empty() {
                        let page_number = writer.page_count() + 1;
                        let overlays = build_page_overlays(
                            &self.options.header_footer_html,
                            &fonts,
                            &page_settings,
                            page_number,
                            None,
                            &overlay_fetcher(),
                            &DocumentImageCache::new(),
                            &mut overlay_cache,
                        );
                        writer.set_page_overlays(overlays);
                    }
                    writer
                        .write_page(&page, &styles, &background_images, &fonts, None)
                        .map_err(EngineError::Io)?;
                }
                writer.finish(&fonts).map_err(EngineError::Io)
            }
            None => {
                // <body>が一度も現れなかった(空文書・不正な入力等)。
                // 空のsink(0ページのPDFにはならないが、書き込みなしで
                // finishする)扱いにする。
                let sink = self
                    .sink
                    .take()
                    .expect("streaming未初期化ならsinkはまだ保持しているはず");
                sink.finish().map_err(EngineError::Io)
            }
        }
    }

    fn finish_batch(self) -> Result<S::Output, EngineError<S::Error>> {
        let Self {
            options,
            parser,
            sink,
            ..
        } = self;
        let mut dom = parser.finish();
        // `parser.finish()`は未閉のタグを閉じる過程でノードを足すことがあるため、
        // `feed`時の確認とは別にここでも見る(この直後からDOMの再帰走査が始まる)。
        check_deadline(options.deadline)?;
        check_document_limits(dom.max_depth(), dom.node_count())?;
        let sink = sink.expect("Mode::Batchではsinkがfinishまでそのまま保持される");

        let system_fonts = SystemFonts::scan();
        let mut fonts = FontCollection::new(load_explicit_fonts(&options.fonts)?);

        let mut ua = user_agent_stylesheet();
        append_user_stylesheets(&mut ua, &options.content.user_stylesheets);
        let base_dir = options
            .base_dir
            .as_deref()
            .unwrap_or_else(|| Path::new("."));
        let base_href = find_base_href(&dom).or_else(|| options.base_href.clone());
        let css_fetcher = ImageFetcher::new(base_dir.to_path_buf(), options.allow_remote_assets)
            .with_base_href(base_href.clone())
            .with_local_access(
                options.local_access.allow,
                options.local_access.allowed_dirs.clone(),
            );
        let css_cache = DocumentImageCache::new();
        let author = extract_author_stylesheet(&dom, &css_fetcher, &css_cache);
        let mut styles = compute_styles(&dom, &ua, &author);
        apply_content_options(&mut styles, &options.content);
        // `<a href="#id">`の宛先候補。
        let mut anchor_names: HashMap<NodeId, String> = collect_anchor_targets(&dom)
            .into_iter()
            .map(|(node, id)| (node, anchor_destination_name(&id)))
            .collect();
        let page_rules = page_rules_with_cli(&options.extra_page_rules, &author.page_rules);
        let page_settings = apply_page_rule_settings_override(options.settings, &page_rules);

        register_generic_fonts(&mut fonts, &options.generic_fonts)?;
        for loaded in load_font_faces(&author.font_faces, &css_fetcher, &system_fonts) {
            fonts.push_font_face(
                loaded.family,
                Some(loaded.weight),
                Some(loaded.style),
                loaded.unicode_range,
                loaded.font,
            );
        }
        load_missing_system_fonts(&mut fonts, &styles, &system_fonts);
        // family名では手掛かりにならない文字(`font-family`未指定の日本語など)
        // を文字カバレッジから補う。`ensure_default_font`より先に呼ぶ
        // 必要はないが、既定フォントを足す前に文書由来のフォントを揃えておく
        // 方がフェースの並びが読みやすい。
        load_fonts_for_uncovered_chars(&mut fonts, &dom, &styles, &system_fonts);
        ensure_default_font(&mut fonts, &system_fonts)?;
        // 補ってもなお描画できない文字が残っていれば警告する。
        warn_uncovered_chars(&fonts, &dom, &styles, &mut HashSet::new());
        // インラインの`<svg>`は描画しない。`<img src="*.svg">`は描けるように
        // なったので、黙って消えると紛らわしい。
        warn_about_inline_svg(&dom, dom.document(), &mut false);

        let mut output = options.output.clone();
        output
            .metadata
            .fill_title_from_document(find_document_title(&dom));

        let image_cache = ImageAssetCache::with_fetcher(
            ImageFetcher::new(base_dir.to_path_buf(), options.allow_remote_assets)
                .with_base_href(base_href.clone())
                .with_local_access(
                    options.local_access.allow,
                    options.local_access.allowed_dirs.clone(),
                ),
        )
        // SVG内の`<text>`は文書と同じフォントで描く。フォントの補完
        // (`load_missing_system_fonts`等)はここより前に済んでいる。
        .with_svg_fonts(SvgFontDb::from_collection(&fonts));
        // `background-image`はレイアウトのサイズ計算に影響しない描画専用の
        // 情報なので、`resolve_images`(box tree構築)とは独立に、文書全体の
        // `styles`から一度だけ構築できる。
        let background_images = if options.content.load_images {
            resolve_background_images(&styles, &image_cache)
        } else {
            HashMap::new()
        };

        // `Mode::Batch`は全ページを確定させてから絶対配置をオーバーレイし、
        // 順に書き出す。`fixed`の全ページ複製・`absolute`の祖先ページ解決が全
        // ページ確定後でないとできないため、`paginate_document_streaming`(逐
        // 次解放)ではなくこちらを使う。
        check_deadline(options.deadline)?;

        // cover/TOCのために、writerを作る前に本文のページを確定させる。
        // 見出しへ自動で振るアンカー名を`LinkSettings`へ載せる必要があるため。
        let pages = paginate_document_with_absolutes(
            &mut dom,
            &styles,
            &fonts,
            &page_settings,
            &image_cache,
        );

        // 目次用の見出し収集。`id`が無い見出しには
        // 自動で宛先名を振り、`anchor_names`へ足す。
        let headings = if options.toc.enabled {
            collect_headings(&dom, &pages, &mut anchor_names)
        } else {
            Vec::new()
        };

        // 表紙は独立したドキュメントとして先に組み立てる。
        let cover_pages = match &options.cover_html {
            Some(html) => render_standalone_document(html, &fonts, &page_settings),
            None => Vec::new(),
        };

        // 目次は「自身のページ数が本文のページ番号をずらす」ため、ページ数が
        // 収束するまで最大3回組み立て直す。
        let (toc_pages, toc_styles) = if options.toc.enabled {
            build_toc_pages(
                &headings,
                &options.toc,
                options.page_offset,
                &fonts,
                &page_settings,
            )
        } else {
            (Vec::new(), HashMap::new())
        };

        // `counter(pages)`の総ページ数はcoverを除いた「TOC + 本文」。
        let total_pages = if rules_use_page_count(&page_rules) {
            Some(toc_pages.len() + pages.len())
        } else {
            None
        };

        let mut writer = StreamingPdfWriter::with_options(
            &fonts,
            page_settings,
            sink,
            page_rules.clone(),
            LinkSettings {
                anchor_names: anchor_names.clone(),
                base_href: base_href.clone(),
                external: options.content.external_links,
                internal: options.content.internal_links,
                keep_relative: options.content.keep_relative_links,
            },
            output,
        )
        .map_err(EngineError::Io)?;

        // 書き出し順は cover → TOC → 本文。ページ番号はcoverを数えず、
        // TOCから`1 + --page-offset`で始める。
        let empty_styles: HashMap<NodeId, Rc<ComputedStyle>> = HashMap::new();
        let empty_images: HashMap<NodeId, Rc<PreparedImage>> = HashMap::new();

        for page in &cover_pages {
            // 番号を持たないページ: margin box・ヘッダー/フッターを出さない。
            writer.set_next_page_number(None);
            writer
                .write_page(page, &empty_styles, &empty_images, &fonts, total_pages)
                .map_err(EngineError::Io)?;
        }

        let mut page_number = 1 + options.page_offset;
        for page in &toc_pages {
            writer.set_next_page_number(Some(page_number));
            writer
                .write_page(page, &toc_styles, &empty_images, &fonts, total_pages)
                .map_err(EngineError::Io)?;
            page_number += 1;
        }

        let mut overlay_cache: Option<Vec<PageOverlay>> = None;
        for page in pages.iter() {
            check_deadline(options.deadline)?;
            if !options.header_footer_html.is_empty() {
                let overlays = build_page_overlays(
                    &options.header_footer_html,
                    &fonts,
                    &page_settings,
                    page_number,
                    total_pages,
                    &overlay_fetcher(),
                    &DocumentImageCache::new(),
                    &mut overlay_cache,
                );
                writer.set_page_overlays(overlays);
            }
            writer.set_next_page_number(Some(page_number));
            writer
                .write_page(page, &styles, &background_images, &fonts, total_pages)
                .map_err(EngineError::Io)?;
            page_number += 1;
        }

        if options.content.abort_on_media_error {
            if let Some(err) = image_cache.had_errors().or_else(|| css_cache.had_errors()) {
                return Err(EngineError::MediaLoad(err));
            }
        }

        writer.finish(&fonts).map_err(EngineError::Io)
    }
}

/// `@page`ルールの`size`/`margin`宣言(無条件`@page{}`ルールのみ)を`base`(CLI
/// オプション/既定値)へ適用した`PageSettings`を返す。
/// `:first`/`:left`/`:right`はmargin box(ヘッダー/フッター内容)の出し
/// 分けにのみ使うため、ここでは無条件ルールだけを見ればよい
/// (`is_first`/`is_left`はどちらの値でも`resolve_page_rules`が返す
/// `size_px`/`margin_*`には影響しない)。
fn apply_page_rule_settings_override(base: PageSettings, page_rules: &[PageRule]) -> PageSettings {
    let resolved = resolve_page_rules(page_rules, false, false);
    let mut settings = base;
    if let Some((width, height)) = resolved.size_px {
        settings.size.width = width;
        settings.size.height = height;
    }
    let resolve_edge = |value: Option<LengthPercentageOrAuto>, base: f32, basis: f32| match value {
        None | Some(LengthPercentageOrAuto::Auto) => base,
        Some(LengthPercentageOrAuto::LengthPercentage(lp)) => match lp {
            crate::style::LengthPercentage::Length(px) => px,
            crate::style::LengthPercentage::Percentage(p) => basis * p,
            crate::style::LengthPercentage::Calc { px, percent } => px + basis * percent,
        },
    };
    settings.margin.top = resolve_edge(
        resolved.margin_top,
        settings.margin.top,
        settings.size.height,
    );
    settings.margin.bottom = resolve_edge(
        resolved.margin_bottom,
        settings.margin.bottom,
        settings.size.height,
    );
    settings.margin.left = resolve_edge(
        resolved.margin_left,
        settings.margin.left,
        settings.size.width,
    );
    settings.margin.right = resolve_edge(
        resolved.margin_right,
        settings.margin.right,
        settings.size.width,
    );
    settings
}

/// `root`以下のサブツリーに属するノードの`ComputedStyle`を`styles`から
/// 取り除く。`dom`は`root`以下がすでに[`Dom::release_subtree`]で解放済み
/// (タブストーン化済み)でもよい(木構造のリンク自体は保持されるため)。
fn remove_subtree_styles(
    dom: &Dom,
    root: NodeId,
    styles: &mut HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &mut HashMap<NodeId, Rc<PreparedImage>>,
) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        stack.extend(dom.children(id));
        styles.remove(&id);
        background_images.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::paginate_document;
    use crate::pdf::write_document;
    use crate::sink::MemorySink;

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    fn font_spec() -> FontSpec {
        FontSpec {
            path: PathBuf::from(DEJAVU_PATH),
            index: 0,
        }
    }

    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    /// `/MediaBox`の期待値をCSS pxで書けるようにするヘルパ。
    fn media_box(width_px: f32, height_px: f32) -> String {
        format!(
            "/MediaBox [0 0 {} {}]",
            width_px * crate::pdf::DEFAULT_SCALE,
            height_px * crate::pdf::DEFAULT_SCALE
        )
    }

    /// PDFバイト列中の全`stream`〜`endstream`区間を展開して連結したものを
    /// 返す。各ストリームの`/Length N`をパースし、`stream\n`直後から正確に
    /// `N`バイトを切り出す(`core/src/pdf/document.rs`の同名ヘルパーは
    /// `\nendstream`という文字列を素朴に探すだけで、フォント埋め込み
    /// バイナリ中に偶然そのバイト列が出現すると誤って区切ってしまい
    /// 後続のストリームを取りこぼす。それを踏んで`sanity check: batched
    /// output should draw strokes`が誤って失敗することを実際に確認した
    /// ため、ここでは`/Length`を使う正確な実装にしている)。
    fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
        fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
            haystack.windows(needle.len()).position(|w| w == needle)
        }

        let mut out = Vec::new();
        let mut i = 0;
        // 末尾の空白で`/Length1`(フォントの元サイズ)と区別する。
        while let Some(pos) = find_subslice(&pdf_bytes[i..], b"/Length ") {
            let len_start = i + pos + b"/Length ".len();
            let mut len_end = len_start;
            while len_end < pdf_bytes.len() && pdf_bytes[len_end].is_ascii_digit() {
                len_end += 1;
            }
            let Some(length) = std::str::from_utf8(&pdf_bytes[len_start..len_end])
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
            else {
                i = len_end.max(i + pos + 1);
                continue;
            };
            let Some(stream_rel) = find_subslice(&pdf_bytes[len_end..], b"stream\n") else {
                break;
            };
            let data_start = len_end + stream_rel + b"stream\n".len();
            let data_end = data_start + length;
            if data_end > pdf_bytes.len() {
                i = len_end;
                continue;
            }
            let raw = &pdf_bytes[data_start..data_end];

            let mut decoder = flate2::read::ZlibDecoder::new(raw);
            let mut decompressed = Vec::new();
            if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
                out.extend_from_slice(&decompressed);
            } else {
                out.extend_from_slice(raw);
            }
            out.push(b'\n');

            i = data_end;
        }
        out
    }

    #[test]
    fn streaming_mode_releases_computed_styles_for_flushed_pages() {
        // 装飾のない200個の<p>。全要素分の`ComputedStyle`を`finish`まで
        // 保持し続けるなら、200要素分(400エントリ超)が`styles`に残るはず。
        // ページがflushされるたびに解放されていれば、直近の未flushページ
        // 分程度(数十エントリ)に収まる。
        let mut html_src = String::from("<style>.item { height: 100px; margin: 0; }</style><body>");
        for i in 0..200 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</body>");

        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            settings: PageSettings::default(),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html_src.as_bytes()).unwrap();

        let styles_len = engine
            .streaming
            .as_ref()
            .expect("<body> should have been detected by now")
            .styles
            .len();
        assert!(
            styles_len < 50,
            "expected the styles map to stay small while streaming (pages should \
             release their entries once flushed), but it grew to {styles_len} entries"
        );

        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn engine_produces_a_valid_pdf_from_a_single_feed() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(b"<p>Hello, world!</p>").unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        assert!(count_occurrences(&bytes, b"%%EOF") > 0);
        assert!(count_occurrences(&bytes, b"/MediaBox") > 0);
    }

    #[test]
    fn engine_produces_a_valid_pdf_from_multiple_feeds() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(b"<p>Hello").unwrap();
        engine.feed(b", ").unwrap();
        engine.feed(b"world!</p>").unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn streaming_mode_matches_batch_mode_for_a_decorated_wrapper_spanning_pages() {
        // 単一のトップレベル要素(背景色・枠線を持つwrapper)が複数ページに
        // またがるケース。`process_top_level_element`は1回しか呼ばれない
        // ため、`push_item`の1回の呼び出し内で複数ページがflushされる。
        // `styles`解放ロジック(`collect_completed_subtree_roots`)が、
        // wrapper自身の`ComputedStyle`をまだ必要な間に誤って消していないか
        // どうかは、`render_box`が`styles.get`の失敗をサイレントに
        // `ComputedStyle::default()`へフォールバックしてしまう
        // (`core/src/pdf/document.rs`)ため、ページ数の一致だけでは検出
        // できない可能性がある。出力バイト列そのものを一括APIと比較する。
        let mut html_src = String::from(r#"<div class="wrapper">"#);
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");

        let author_css = ".wrapper { border: 2px solid black; padding: 5px; margin: 0; } \
             .item { height: 100px; margin: 0; }";
        let settings = PageSettings::default();

        let dom = crate::html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let author = crate::style::parse_stylesheet(author_css);
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = FontCollection::new(vec![Font::load(DEJAVU_PATH).unwrap()]);
        let batched_pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(batched_pages.len() > 1, "expected multiple pages");
        let batched_bytes = write_document(
            &batched_pages,
            &styles,
            &HashMap::new(),
            &fonts,
            &settings,
            MemorySink::new(),
        )
        .unwrap();

        let html_with_style = format!("<style>{author_css}</style>{html_src}");
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            settings,
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html_with_style.as_bytes()).unwrap();
        let streamed_bytes = engine.finish().unwrap();

        assert_eq!(
            count_occurrences(&streamed_bytes, b"/MediaBox"),
            count_occurrences(&batched_bytes, b"/MediaBox"),
        );
        // 描画コンテンツ(枠線描画で使われる`closepath`+`fill`の出現数)も
        // 一致するはず。`styles`から早すぎるタイミングでwrapperの
        // `ComputedStyle`が失われていれば、装飾(枠線)の描画コマンドが欠落
        // しこの数が変わる。コンテンツストリームは`/FlateDecode`で圧縮
        // されているため、圧縮後の`bytes`を直接文字列検索しても意味が
        // なく、展開してから比較する必要がある(`solid_border_fills_a_
        // mitered_quad_per_side`が示す通り、単色borderは`stroke`ではなく
        // 辺ごとの塗りつぶしパスとして描画される実装のため`h\nf\n`を数える)。
        let streamed_stream = decompressed_stream_bytes(&streamed_bytes);
        let batched_stream = decompressed_stream_bytes(&batched_bytes);
        let streamed_fills = count_occurrences(&streamed_stream, b"h\nf\n");
        let batched_fills = count_occurrences(&batched_stream, b"h\nf\n");
        assert!(
            batched_fills > 0,
            "sanity check: batched output should draw border fill paths"
        );
        assert_eq!(
            streamed_fills, batched_fills,
            "border fill path count should match (border rendering should be identical)"
        );
    }

    #[test]
    fn engine_output_matches_the_batch_api_page_count() {
        let mut html_src = String::from("<style>.item { height: 100px; margin: 0; }</style><div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");

        let settings = PageSettings::default();

        // 既存の一括API経由。
        let dom = crate::html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let css_fetcher = ImageFetcher::new(std::path::PathBuf::from("."), false);
        let css_cache = DocumentImageCache::new();
        let author = crate::style::extract_author_stylesheet(&dom, &css_fetcher, &css_cache);
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = FontCollection::new(vec![Font::load(DEJAVU_PATH).unwrap()]);
        let batched_pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(batched_pages.len() > 1, "expected multiple pages");
        let batched_bytes = write_document(
            &batched_pages,
            &styles,
            &HashMap::new(),
            &fonts,
            &settings,
            MemorySink::new(),
        )
        .unwrap();

        // Engine経由(Mode::Batch)。
        let options = EngineOptions {
            fonts: vec![font_spec()],
            settings,
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html_src.as_bytes()).unwrap();
        let engine_bytes = engine.finish().unwrap();

        assert_eq!(
            count_occurrences(&engine_bytes, b"/MediaBox"),
            count_occurrences(&batched_bytes, b"/MediaBox"),
            "Engine (batch mode) and the batch API should produce the same page count"
        );
    }

    #[test]
    fn streaming_mode_produces_the_same_page_count_as_batch_mode() {
        let mut html_src = String::from("<style>.item { height: 100px; margin: 0; }</style><div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");

        let settings = PageSettings::default();

        let batch_options = EngineOptions {
            mode: Mode::Batch,
            fonts: vec![font_spec()],
            settings,
            ..EngineOptions::default()
        };
        let mut batch_engine = Engine::new(batch_options, MemorySink::new());
        batch_engine.feed(html_src.as_bytes()).unwrap();
        let batch_bytes = batch_engine.finish().unwrap();
        let batch_pages = count_occurrences(&batch_bytes, b"/MediaBox");
        assert!(batch_pages > 1, "expected multiple pages");

        let streaming_options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            settings,
            ..EngineOptions::default()
        };
        let mut streaming_engine = Engine::new(streaming_options, MemorySink::new());
        streaming_engine.feed(html_src.as_bytes()).unwrap();
        let streaming_bytes = streaming_engine.finish().unwrap();

        assert_eq!(
            count_occurrences(&streaming_bytes, b"/MediaBox"),
            batch_pages,
            "Mode::Streaming should produce the same page count as Mode::Batch"
        );
    }

    #[test]
    fn streaming_mode_works_when_fed_one_byte_at_a_time() {
        let mut html_src =
            String::from("<style>.item { height: 100px; margin: 0; }</style><body><div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div></body>");

        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            settings: PageSettings::default(),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        for byte in html_src.as_bytes() {
            engine.feed(std::slice::from_ref(byte)).unwrap();
        }
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        assert!(count_occurrences(&bytes, b"/MediaBox") > 1);
    }

    #[test]
    fn streaming_mode_rejects_a_style_tag_after_body_starts() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(b"<body><p>x</p>").unwrap();

        match engine.feed(b"<style>p{color:red}</style>") {
            Err(EngineError::UnsupportedInStreamingMode(_)) => {}
            other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
        }
    }

    #[test]
    fn batch_mode_allows_a_style_tag_after_body_starts() {
        let options = EngineOptions {
            mode: Mode::Batch,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(b"<body><p>x</p>").unwrap();
        engine
            .feed(b"<style>p{color:red}</style>")
            .expect("Mode::Batch should not reject a late <style> tag");
        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn apply_page_rule_settings_override_uses_only_unconditional_rules() {
        let base = PageSettings::default();
        let sheet = crate::style::parse_stylesheet(
            "@page { size: 300px 400px; margin: 20px; } \
             @page :first { size: 999px 999px; margin: 999px; }",
        );
        let overridden = apply_page_rule_settings_override(base, &sheet.page_rules);
        assert_eq!(overridden.size.width, 300.0);
        assert_eq!(overridden.size.height, 400.0);
        assert_eq!(overridden.margin.top, 20.0);
        assert_eq!(overridden.margin.left, 20.0);
    }

    #[test]
    fn apply_page_rule_settings_override_leaves_settings_unchanged_without_at_page() {
        let base = PageSettings::default();
        let overridden = apply_page_rule_settings_override(base, &[]);
        assert_eq!(overridden, base);
    }

    #[test]
    fn at_page_size_overrides_the_pdf_media_box_in_batch_mode() {
        let options = EngineOptions {
            mode: Mode::Batch,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine
            .feed(b"<html><head><style>@page { size: 300px 400px; }</style></head><body><p>x</p></body></html>")
            .unwrap();
        let bytes = engine.finish().unwrap();
        assert!(
            count_occurrences(&bytes, media_box(300.0, 400.0).as_bytes()) > 0,
            "@page size should override the PDF MediaBox"
        );
    }

    #[test]
    fn at_page_size_overrides_the_pdf_media_box_in_streaming_mode() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine
            .feed(b"<html><head><style>@page { size: 300px 400px; }</style></head><body><p>x</p></body></html>")
            .unwrap();
        let bytes = engine.finish().unwrap();
        assert!(
            count_occurrences(&bytes, media_box(300.0, 400.0).as_bytes()) > 0,
            "@page size should override the PDF MediaBox in streaming mode too"
        );
    }

    #[test]
    fn margin_box_content_glyphs_are_embedded_in_the_font_subset_in_batch_mode() {
        // margin boxのcontentは通常のBoxContent::Inline経路(collect_usage)を
        // 通らない独立した経路(collect_margin_box_usage)なので、専用の収集漏れが
        // 起きていないかを回帰確認する(リストのマーカーグリフ収集漏れと同種の
        // バグクラス)。本文には登場しない数字を`@bottom-right`のページ番号として
        // 表示させ、そのグリフが実際にToUnicode CMapへ埋め込まれることを確認する。
        let options = EngineOptions {
            mode: Mode::Batch,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine
            .feed(
                b"<html><head><style>\
                    @page { @bottom-right { content: counter(page); } }\
                  </style></head><body><p>no digits here</p></body></html>",
            )
            .unwrap();
        let bytes = engine.finish().unwrap();
        let decompressed = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&decompressed, b"<0031>") > 0,
            "the margin box counter(page) glyph ('1') should be embedded in the ToUnicode CMap"
        );
    }

    #[test]
    fn counter_pages_in_a_margin_box_is_rejected_in_streaming_mode() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        match engine.feed(
            b"<html><head><style>\
                @page { @bottom-center { content: counter(pages); } }\
              </style></head><body><p>x</p></body></html>",
        ) {
            Err(EngineError::UnsupportedInStreamingMode(_)) => {}
            other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
        }
    }

    #[test]
    fn counter_page_alone_is_allowed_in_streaming_mode() {
        // `counter(page)`単体(`counter(pages)`を伴わない)は、ページ確定時点で
        // 値が決まるためストリーミングでも問題なく動作するはず。
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine
            .feed(
                b"<html><head><style>\
                    @page { @bottom-right { content: counter(page); } }\
                  </style></head><body><p>x</p></body></html>",
            )
            .expect("counter(page) alone should be allowed in streaming mode");
        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn counter_pages_resolves_to_the_actual_total_page_count_in_batch_mode() {
        // `@page`の`size`/`margin`を明示指定してページ数を決定論的にする:
        // ページ内容領域の高さ=300px(margin 0)、300px高さのdivを2個並べれば
        // ちょうど2ページに分かれるはず。
        let options = EngineOptions {
            mode: Mode::Batch,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        let html = b"<html><head><style>\
               @page { size: 200px 300px; margin: 0; @bottom-right { content: counter(pages); } }\
               body { margin: 0; } div { height: 300px; }\
             </style></head><body><div></div><div></div></body></html>";
        engine.feed(html).unwrap();
        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(
            count_occurrences(&bytes, media_box(200.0, 300.0).as_bytes()),
            2,
            "expected exactly 2 pages"
        );
        let decompressed = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&decompressed, b"<0032>") > 0,
            "counter(pages) should resolve to the actual total page count ('2') in the ToUnicode CMap"
        );
    }

    #[test]
    fn streaming_mode_rejects_a_decorated_body() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        match engine.feed(
            b"<html><head><style>body { background-color: red; }</style></head><body><p>x</p>",
        ) {
            Err(EngineError::UnsupportedInStreamingMode(_)) => {}
            other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
        }
    }

    /// 深くネストしたHTMLを組み立てる。
    fn deeply_nested_html(depth: usize) -> String {
        format!(
            "<html><body>{}x{}</body></html>",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        )
    }

    /// 上限を超えるネストは、DOMを再帰的に辿る処理へ進む前にエラーで拒否
    /// されること。これが無いとスタイル計算・レイアウト・描画・`LayoutBox`の
    /// 再帰Dropのいずれかでスタックオーバーフローし、プロセスごとabortする。
    #[test]
    fn html_nested_beyond_the_depth_limit_is_rejected_in_batch_mode() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        let html = deeply_nested_html(crate::html::MAX_ELEMENT_DEPTH as usize + 10);

        let result = engine.feed(html.as_bytes()).and_then(|()| {
            engine.finish()?;
            Ok(())
        });
        match result {
            Err(EngineError::DepthLimitExceeded { depth, limit }) => {
                assert!(depth > limit, "深さ{depth}は上限{limit}を超えているはず");
            }
            other => panic!("expected DepthLimitExceeded, got {other:?}"),
        }
    }

    /// ストリーミングモードでも同じく拒否されること(こちらは`feed`の途中で
    /// 部分木の処理が始まるため、`finish`を待たずに止める必要がある)。
    #[test]
    fn html_nested_beyond_the_depth_limit_is_rejected_in_streaming_mode() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        let html = deeply_nested_html(crate::html::MAX_ELEMENT_DEPTH as usize + 10);

        let result = engine.feed(html.as_bytes()).and_then(|()| {
            engine.finish()?;
            Ok(())
        });
        assert!(
            matches!(result, Err(EngineError::DepthLimitExceeded { .. })),
            "ストリーミングでも深さ超過は拒否されるべき: {result:?}"
        );
    }

    /// ノード数が上限を超える入力は拒否すること。
    ///
    /// スタイル・ボックスツリー・レイアウト結果がノード数に比例して積み
    /// 上がるため、ここで止めないとメモリを食い潰す(実測で1ノードあたり
    /// 最悪1210B)。
    #[test]
    fn html_with_too_many_nodes_is_rejected() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        // `<p>a</p>`は要素+テキストで2ノード。
        let body = "<p>a</p>".repeat(crate::html::MAX_NODES);
        let html = format!("<html><body>{body}</body></html>");

        let result = engine.feed(html.as_bytes()).and_then(|()| {
            engine.finish()?;
            Ok(())
        });
        match result {
            Err(EngineError::NodeLimitExceeded { nodes, limit }) => {
                assert!(
                    nodes > limit,
                    "ノード数{nodes}は上限{limit}を超えているはず"
                );
            }
            other => panic!("expected NodeLimitExceeded, got {other:?}"),
        }
    }

    /// 上限内の文書はこれまでどおり通ること。
    #[test]
    fn html_within_the_node_limit_still_renders() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        let body = "<p>a</p>".repeat(1000);
        let html = format!("<html><body>{body}</body></html>");

        engine.feed(html.as_bytes()).expect("上限内なので通る");
        let pdf = engine.finish().expect("上限内なので書き出せる");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// ストリーミングモードでは解放したノードが数に戻ること。
    ///
    /// 逐次解放しながら進む限り、総ノード数が上限を超えていても変換できる
    /// (ストリーミングの低メモリという利点を上限で潰さないため)。
    ///
    /// 解放はトップレベル要素の処理時に起きるので、確認にはCLIと同じく
    /// チャンクに分けて投入する必要がある。一度に全部投入すると、解放が
    /// 走る前にDOMが積み上がってしまい、実際にメモリも使う。
    #[test]
    fn released_nodes_do_not_count_towards_the_node_limit() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        // 総ノード数は上限の2倍を超えるが、逐次解放されるので当たらない。
        let body = "<p>a</p>".repeat(crate::html::MAX_NODES);
        let html = format!("<html><body>{body}</body></html>");

        // `cli::convert`のFEED_CHUNKと同じ64KiB刻み。
        for chunk in html.as_bytes().chunks(64 * 1024) {
            engine.feed(chunk).expect("解放が効くので上限に当たらない");
        }
        let pdf = engine.finish().expect("ストリーミングなら書き出せる");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// 期限を過ぎていれば変換を打ち切ること(バッチ)。
    #[test]
    fn a_deadline_that_has_already_passed_stops_the_conversion() {
        // `check_deadline`は`>=`で見るので、今の時刻をそのまま期限にすれば
        // 判定時点では必ず過ぎている。
        let options = EngineOptions {
            fonts: vec![font_spec()],
            deadline: Some(std::time::Instant::now()),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());

        let result = engine
            .feed(b"<html><body><p>x</p></body></html>")
            .and_then(|()| {
                engine.finish()?;
                Ok(())
            });
        assert!(
            matches!(result, Err(EngineError::TimedOut)),
            "期限切れはTimedOutで返るべき: {result:?}"
        );
    }

    /// ストリーミングモードでも同じく打ち切ること。
    #[test]
    fn a_passed_deadline_stops_the_conversion_in_streaming_mode() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            deadline: Some(std::time::Instant::now()),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());

        let result = engine
            .feed(b"<html><body><p>x</p></body></html>")
            .and_then(|()| {
                engine.finish()?;
                Ok(())
            });
        assert!(
            matches!(result, Err(EngineError::TimedOut)),
            "ストリーミングでも期限切れはTimedOut: {result:?}"
        );
    }

    /// 期限が先ならこれまでどおり最後まで走ること。
    #[test]
    fn a_deadline_in_the_future_does_not_interfere() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(300)),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());

        engine
            .feed(b"<html><body><p>x</p></body></html>")
            .expect("期限内なので通る");
        let pdf = engine.finish().expect("期限内なので書き出せる");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// 期限を指定しなければ無制限(CLIの既定)。
    #[test]
    fn no_deadline_means_no_limit() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        assert!(options.deadline.is_none());
    }

    /// 上限のすぐ内側は通ること(上限が実用的な文書を巻き込まない確認)。
    ///
    /// テストスレッドの既定スタックは2MiBで、デバッグビルドの1段あたり約11KiB
    /// では上限ぶんの再帰に足りない。CLI・サーバと同じく
    /// [`crate::render_stack::with_render_stack`]で確保してから走らせる
    /// (上限とスタックが対で意味を持つことの確認でもある)。
    #[test]
    fn html_just_within_the_depth_limit_still_renders() {
        let pdf = crate::render_stack::with_render_stack(|| {
            let options = EngineOptions {
                fonts: vec![font_spec()],
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            // <html>/<body>ぶんの数段を見込んで少し余裕を取る。
            let html = deeply_nested_html(crate::html::MAX_ELEMENT_DEPTH as usize - 10);

            engine.feed(html.as_bytes()).expect("上限内なら通るはず");
            engine.finish().expect("上限内なら書き出せるはず")
        });
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn engine_resolves_at_font_face_relative_to_base_dir() {
        // 既存のCLI E2Eテスト(cli.rs)と同じ@font-face+base_dir解決の
        // シナリオをEngine経由で検証する。
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-engine-test-{}-{}",
            std::process::id(),
            "font_face"
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let font_dest = dir.join("embedded.ttf");
        std::fs::copy(DEJAVU_PATH, &font_dest).unwrap();

        let html = r#"<html><head><style>
            @font-face { font-family: "Embedded"; src: url("embedded.ttf"); }
            p { font-family: "Embedded"; }
        </style></head><body><p>hello</p></body></html>"#;

        let options = EngineOptions {
            base_dir: Some(dir.clone()),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        assert!(count_occurrences(&bytes, b"/FontFile2") > 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unicode_range_hard_filter_excludes_a_face_end_to_end_through_the_engine() {
        // 1つ目の`@font-face`(index 0)はDejaVu Sansだが`unicode-range:
        // U+0-7F`(Basic Latinのみ)を宣言する。'é'(U+00E9)はDejaVu Sans
        // 自身が実際に描画できるグリフだが、宣言レンジ外なのでハード
        // フィルタで除外されるはず。2つ目の`@font-face`(index 1)は同じ
        // DejaVu Sansをrange指定なしで再登録したもので、こちらが
        // 選ばれるはず。CSSパース→`Engine`→`FontCollection`の実際の
        // パイプラインを通した回帰検知。
        let base_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts"));
        let html = r#"<html><head><style>
            @font-face { font-family: "Brand"; src: url("DejaVuSans.ttf"); unicode-range: U+0-7F; }
            @font-face { font-family: "Brand"; src: url("DejaVuSans.ttf"); }
            p { font-family: "Brand"; }
        </style></head><body><p>ééé</p></body></html>"#;

        let options = EngineOptions {
            base_dir: Some(base_dir.to_path_buf()),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        let stream = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&stream, b"/F1 ") > 0,
            "should select the unrestricted second face (index 1) for U+00E9"
        );
        assert_eq!(
            count_occurrences(&stream, b"/F0 "),
            0,
            "the range-restricted first face (index 0) should never be selected for U+00E9, \
             even though it physically has the glyph"
        );
    }

    #[test]
    fn unicode_range_split_between_latin_and_cjk_faces_matches_in_batch_and_streaming_mode() {
        // 典型的な「英数字用+CJK用を同一family名でunicode-range分けして併用」パターン。
        // `Mode::Batch`/`Mode::Streaming`両方で同じ結果になることも確認する。
        let base_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts"));
        let html_src = r#"<style>
            @font-face { font-family: "Brand"; src: url("DejaVuSans.ttf"); unicode-range: U+0-24F; }
            @font-face { font-family: "Brand"; src: url("NotoSansCJK-Regular.ttc"); unicode-range: U+4E00-9FFF; }
            p { font-family: "Brand"; }
        </style><body><p>A&#26085;</p></body>"#;

        let run = |mode: Mode| {
            let options = EngineOptions {
                mode,
                base_dir: Some(base_dir.to_path_buf()),
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            engine.feed(html_src.as_bytes()).unwrap();
            engine.finish().unwrap()
        };

        let batch_bytes = run(Mode::Batch);
        let streaming_bytes = run(Mode::Streaming);

        for (label, bytes) in [("batch", &batch_bytes), ("streaming", &streaming_bytes)] {
            assert!(
                bytes.starts_with(b"%PDF-"),
                "{label} output should be a valid PDF"
            );
            let stream = decompressed_stream_bytes(bytes);
            assert!(
                count_occurrences(&stream, b"/F0 ") > 0,
                "{label}: the Latin-range face (index 0) should be used for 'A'"
            );
            assert!(
                count_occurrences(&stream, b"/F1 ") > 0,
                "{label}: the CJK-range face (index 1) should be used for U+65E5"
            );
        }
    }

    const JPEG_FIXTURE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_gradient.jpg"
    );
    const PNG_ALPHA_FIXTURE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_gradient_alpha.png"
    );

    fn data_uri(path: &str, mime_type: &str) -> String {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let bytes = std::fs::read(path).unwrap();
        format!("data:{mime_type};base64,{}", STANDARD.encode(bytes))
    }

    #[test]
    fn image_data_uri_is_embedded_as_a_dctdecode_xobject_end_to_end() {
        // 画像埋め込みのパイプライン全体(DOM属性抽出→data:URI分類→
        // デコード→box tree→レイアウト→PDF XObject書き出し)を、
        // fetchを一切経由しないdata:URI経由で検証する。
        let html = format!(
            r#"<html><body><img src="{}" width="32" height="24"></body></html>"#,
            data_uri(JPEG_FIXTURE_PATH, "image/jpeg")
        );

        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        // JPEGはデコードせずそのままDCTDecodeフィルタで埋め込むため、生のJPEG
        // バイト列そのものが出現するはず。
        let jpeg_bytes = std::fs::read(JPEG_FIXTURE_PATH).unwrap();
        assert!(count_occurrences(&bytes, b"/DCTDecode") > 0);
        assert!(
            count_occurrences(&bytes, &jpeg_bytes) > 0,
            "the original JPEG bytes should be embedded verbatim (no re-encode)"
        );
        assert!(count_occurrences(&bytes, b"/Width 32") > 0);
        assert!(count_occurrences(&bytes, b"/Height 24") > 0);
    }

    #[test]
    fn png_with_alpha_data_uri_produces_an_smask_xobject_end_to_end() {
        let html = format!(
            r#"<html><body><img src="{}"></body></html>"#,
            data_uri(PNG_ALPHA_FIXTURE_PATH, "image/png")
        );

        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        assert!(
            count_occurrences(&bytes, b"/SMask") > 0,
            "a PNG with an alpha channel should produce an SMask-linked XObject"
        );
        // 内在サイズ(16x16、フィクスチャの実寸)がwidth/height属性なしで
        // そのまま使われているはず。
        assert!(count_occurrences(&bytes, b"/Width 16") > 0);
        assert!(count_occurrences(&bytes, b"/Height 16") > 0);
    }

    /// `object-fit`/`object-position`のE2Eテスト。
    /// `object_fit_rect`自体の幾何計算は`pdf/document.rs`の単体テストで
    /// 網羅済みのため、ここでは実際のパイプライン(data:URIデコード→box tree
    /// →レイアウト→PDFエンコード)を通した疎通・クリップ発行の確認に絞る。
    fn build_object_fit_pdf(object_fit_css: &str) -> Vec<u8> {
        let html = format!(
            r#"<html><body><img src="{}" style="width: 150px; height: 80px; {}"></body></html>"#,
            data_uri(JPEG_FIXTURE_PATH, "image/jpeg"),
            object_fit_css
        );
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
        bytes
    }

    #[test]
    fn object_fit_cover_and_none_render_valid_pdfs_with_a_single_image_draw_each() {
        for object_fit in ["cover", "contain", "none", "scale-down", "fill"] {
            let bytes = build_object_fit_pdf(&format!("object-fit: {object_fit};"));
            let decompressed = decompressed_stream_bytes(&bytes);
            assert_eq!(
                count_occurrences(&decompressed, b" Do\n"),
                1,
                "object-fit: {object_fit} should draw the image exactly once (no tiling)"
            );
        }
    }

    #[test]
    fn object_fit_always_clips_to_the_content_box_even_for_the_default_fill() {
        // `object-fit`の値によらず常にcontent-boxへクリップする(`Fill`は
        // 元々ぴったり収まるがno-opとして同じ経路を通る)。クリップパスの構築
        // (`re` → `W n`)が実際に発行されていることを確認する。
        let bytes = build_object_fit_pdf("");
        let decompressed = decompressed_stream_bytes(&bytes);
        assert_eq!(count_occurrences(&decompressed, b" re\n"), 1);
        assert!(count_occurrences(&decompressed, b"W\n") > 0);
    }

    #[test]
    fn object_fit_cover_and_fill_produce_different_geometry_end_to_end() {
        // intrinsic 32x24 を 150x80 のボックスへ描画する場合、`fill`
        // (非一様に引き伸ばす)と`cover`(アスペクト比を保って拡大・はみ出し分は
        // クリップ)は描画される画像の変換行列(`cm`)が異なるはずなので、
        // コンテンツストリーム全体としてもバイト列が一致しないはず。
        let fill_bytes = decompressed_stream_bytes(&build_object_fit_pdf("object-fit: fill;"));
        let cover_bytes = decompressed_stream_bytes(&build_object_fit_pdf("object-fit: cover;"));
        assert_ne!(fill_bytes, cover_bytes);
    }

    #[test]
    fn object_position_moves_the_image_within_the_content_box_end_to_end() {
        let center_bytes = decompressed_stream_bytes(&build_object_fit_pdf("object-fit: contain;"));
        let right_bottom_bytes = decompressed_stream_bytes(&build_object_fit_pdf(
            "object-fit: contain; object-position: right bottom;",
        ));
        assert_ne!(center_bytes, right_bottom_bytes);
    }

    #[test]
    fn image_rendering_matches_between_batch_and_streaming_mode() {
        let html = format!(
            r#"<html><body><p>before</p><img src="{}" width="32" height="24"><p>after</p></body></html>"#,
            data_uri(JPEG_FIXTURE_PATH, "image/jpeg")
        );

        let run = |mode: Mode| {
            let options = EngineOptions {
                mode,
                fonts: vec![font_spec()],
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            engine.feed(html.as_bytes()).unwrap();
            engine.finish().unwrap()
        };

        let batch_bytes = run(Mode::Batch);
        let streaming_bytes = run(Mode::Streaming);

        for (label, bytes) in [("batch", &batch_bytes), ("streaming", &streaming_bytes)] {
            assert!(
                bytes.starts_with(b"%PDF-"),
                "{label} output should be a valid PDF"
            );
            assert!(
                count_occurrences(bytes, b"/DCTDecode") > 0,
                "{label}: image should be embedded"
            );
        }
    }

    #[test]
    fn background_image_on_a_plain_div_is_embedded_as_a_dctdecode_xobject_end_to_end() {
        // パイプライン全体(パース→カスケード→
        // `resolve_background_images`→PDF XObject書き出し)を検証する。
        // `<div>`は`background-color`も枠線も持たない。
        let html = format!(
            r#"<html><body><div style="background-image: url('{}'); width: 32px; height: 24px;"></div></body></html>"#,
            data_uri(JPEG_FIXTURE_PATH, "image/jpeg")
        );

        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        let jpeg_bytes = std::fs::read(JPEG_FIXTURE_PATH).unwrap();
        assert!(count_occurrences(&bytes, b"/DCTDecode") > 0);
        assert!(
            count_occurrences(&bytes, &jpeg_bytes) > 0,
            "the background-image's original JPEG bytes should be embedded verbatim"
        );
    }

    #[test]
    fn background_image_rendering_matches_between_batch_and_streaming_mode() {
        let html = format!(
            r#"<html><body><p>before</p><div style="background-image: url('{}'); width: 32px; height: 24px;"></div><p>after</p></body></html>"#,
            data_uri(JPEG_FIXTURE_PATH, "image/jpeg")
        );

        let run = |mode: Mode| {
            let options = EngineOptions {
                mode,
                fonts: vec![font_spec()],
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            engine.feed(html.as_bytes()).unwrap();
            engine.finish().unwrap()
        };

        let batch_bytes = run(Mode::Batch);
        let streaming_bytes = run(Mode::Streaming);

        for (label, bytes) in [("batch", &batch_bytes), ("streaming", &streaming_bytes)] {
            assert!(
                bytes.starts_with(b"%PDF-"),
                "{label} output should be a valid PDF"
            );
            assert!(
                count_occurrences(bytes, b"/DCTDecode") > 0,
                "{label}: background image should be embedded"
            );
        }
    }

    #[test]
    fn a_broken_background_image_url_degrades_gracefully_instead_of_failing_the_whole_document() {
        // 取得・デコード失敗はその要素の背景画像だけ空扱いにして、
        // 文書生成全体は止めない。
        let html = r#"<html><body><p>before</p>
            <div style="background-image: url('does-not-exist-anywhere.png'); width: 50px; height: 50px;"></div>
            <p>after</p></body></html>"#;

        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine
            .finish()
            .expect("a broken background-image url must not fail the whole document");

        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(
            count_occurrences(&bytes, b"/DCTDecode"),
            0,
            "no image XObject should have been written for the failed fetch"
        );
    }

    #[test]
    fn a_broken_image_src_degrades_to_an_empty_box_instead_of_failing_the_whole_document() {
        // 取得・デコード失敗はその要素だけ空扱いにして、文書生成全体は
        // 止めない(壊れたURLがDoSベクタにならないように)。
        let html = r#"<html><body><p>before</p>
            <img src="does-not-exist-anywhere.png" width="50" height="50">
            <p>after</p></body></html>"#;

        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine
            .finish()
            .expect("a broken image src must not fail the whole document");

        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(
            count_occurrences(&bytes, b"/DCTDecode"),
            0,
            "no image XObject should have been written for the failed fetch"
        );
    }

    #[test]
    fn external_stylesheet_via_link_is_applied_end_to_end() {
        // 外部スタイルシートのパイプライン全体(<link>検出→fetch→parse→cascade)を、
        // 実際にfont-sizeの違いとしてPDFコンテンツストリームに現れるかで
        // 検証する。
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-engine-test-{}-external_stylesheet",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.css"), b"p { font-size: 40px; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="main.css"></head>
            <body><p>hello</p></body></html>"#;
        let options = EngineOptions {
            fonts: vec![font_spec()],
            base_dir: Some(dir.clone()),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        let stream = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&stream, b"/F0 40 Tf") > 0,
            "the font-size from the fetched external stylesheet should apply"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn external_stylesheet_matches_between_batch_and_streaming_mode() {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-engine-test-{}-external_stylesheet_parity",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.css"), b"p { font-size: 40px; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="main.css"></head>
            <body><p>hello</p></body></html>"#;

        let run = |mode: Mode| {
            let options = EngineOptions {
                mode,
                fonts: vec![font_spec()],
                base_dir: Some(dir.clone()),
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            engine.feed(html.as_bytes()).unwrap();
            engine.finish().unwrap()
        };

        let batch_bytes = run(Mode::Batch);
        let streaming_bytes = run(Mode::Streaming);

        for (label, bytes) in [("batch", &batch_bytes), ("streaming", &streaming_bytes)] {
            assert!(
                bytes.starts_with(b"%PDF-"),
                "{label} output should be a valid PDF"
            );
            let stream = decompressed_stream_bytes(bytes);
            assert!(
                count_occurrences(&stream, b"/F0 40 Tf") > 0,
                "{label}: the fetched external stylesheet's font-size should apply"
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn at_import_inside_an_external_stylesheet_is_applied_end_to_end() {
        // @importのパイプライン全体(<link>のfetch→@importの検出・再帰フェッチ→
        // 展開→parse→cascade)を、実際にfont-sizeの違いとしてPDFコンテンツ
        // ストリームに現れるかで検証する。
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-engine-test-{}-at_import",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.css"), br#"@import url("base.css");"#).unwrap();
        std::fs::write(dir.join("base.css"), b"p { font-size: 40px; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="main.css"></head>
            <body><p>hello</p></body></html>"#;
        let options = EngineOptions {
            fonts: vec![font_spec()],
            base_dir: Some(dir.clone()),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine.finish().unwrap();

        assert!(bytes.starts_with(b"%PDF-"));
        let stream = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&stream, b"/F0 40 Tf") > 0,
            "the font-size from the @import-ed stylesheet should apply"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn at_import_matches_between_batch_and_streaming_mode() {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-engine-test-{}-at_import_parity",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.css"), br#"@import url("base.css");"#).unwrap();
        std::fs::write(dir.join("base.css"), b"p { font-size: 40px; }").unwrap();

        let html = r#"<html><head><link rel="stylesheet" href="main.css"></head>
            <body><p>hello</p></body></html>"#;

        let run = |mode: Mode| {
            let options = EngineOptions {
                mode,
                fonts: vec![font_spec()],
                base_dir: Some(dir.clone()),
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            engine.feed(html.as_bytes()).unwrap();
            engine.finish().unwrap()
        };

        let batch_bytes = run(Mode::Batch);
        let streaming_bytes = run(Mode::Streaming);

        for (label, bytes) in [("batch", &batch_bytes), ("streaming", &streaming_bytes)] {
            assert!(
                bytes.starts_with(b"%PDF-"),
                "{label} output should be a valid PDF"
            );
            let stream = decompressed_stream_bytes(bytes);
            assert!(
                count_occurrences(&stream, b"/F0 40 Tf") > 0,
                "{label}: the @import-ed stylesheet's font-size should apply"
            );
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn streaming_mode_rejects_a_late_link_stylesheet_after_body_starts() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(b"<body><p>x</p>").unwrap();

        match engine.feed(br#"<link rel="stylesheet" href="late.css">"#) {
            Err(EngineError::UnsupportedInStreamingMode(_)) => {}
            other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
        }
    }

    #[test]
    fn streaming_mode_allows_a_late_link_that_is_not_a_stylesheet() {
        // rel="stylesheet"以外のlink(favicon等)は、<body>より後に
        // 出現してもストリーミングモードの制約対象外のはず。
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(b"<body><p>x</p>").unwrap();
        engine
            .feed(br#"<link rel="icon" href="favicon.ico">"#)
            .expect("a non-stylesheet <link> after <body> should not be rejected");
    }

    #[test]
    fn a_failed_external_stylesheet_does_not_fail_the_whole_document() {
        // 外部スタイルシートの取得失敗はそのスタイルシートだけを無視し、
        // 文書生成全体は止めない(画像と同じ方針)。
        let html = r#"<html><head><link rel="stylesheet" href="does-not-exist.css"></head>
            <body><p>hello</p></body></html>"#;
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html.as_bytes()).unwrap();
        let bytes = engine
            .finish()
            .expect("a broken external stylesheet must not fail the whole document");

        assert!(bytes.starts_with(b"%PDF-"));
    }
}
