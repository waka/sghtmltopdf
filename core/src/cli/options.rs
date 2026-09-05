//! CLIオプションの定義。

use std::path::PathBuf;

#[cfg(feature = "server")]
use clap::Subcommand;
use clap::{ArgAction, ArgMatches, Args, Parser, ValueEnum};

use crate::engine::{ContentOptions, GenericFamily, LocalAccess, Mode};
use crate::layout::{PageSettings, PageSize};
use crate::pdf::{DocumentMetadata, PdfOutputOptions};

use super::header_footer::{MarginBoxText, SimpleHeaderFooter};
use super::toc::TocOptions;
use super::units::parse_length_px;

/// 入力・出力に`-`を指定したときの意味(stdin/stdout)。
pub const STD_STREAM: &str = "-";

#[derive(Debug, Parser)]
#[command(
    name = "sghtmltopdf",
    version,
    about = "Chromium/WebKit/Geckoに依存しないHTML→PDFレンダラー",
    // 変換をサブコマンドにせず、位置引数のまま扱う。
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[cfg(feature = "server")]
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub convert: ConvertArgs,
}

#[cfg(feature = "server")]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// HTTPサーバとして待ち受け、POST /pdf でHTMLをPDFへ変換する
    Server(ServerArgs),
}

#[cfg(feature = "server")]
#[derive(Debug, Clone, Args)]
pub struct ServerArgs {
    /// 待ち受けアドレス(既定はループバック。外部公開はリバースプロキシ経由で)
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub listen: String,

    /// 同時に変換するワーカースレッド数(既定=CPUコア数)
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,

    /// 受理待ちキューの上限(既定=ワーカー数×4)。超えると503を返す
    #[arg(long, value_name = "N")]
    pub max_queue: Option<usize>,

    /// リクエストボディの上限バイト数
    ///
    /// テキスト量に比例するメモリは`MAX_NODES`では抑えられないため、ここが
    /// その担当になる。実測では入力1MiBあたり約185MiBを使う(CJKテキストを
    /// 敷き詰めた最悪ケース)ので、4MiBで約750MiBが上限の目安。
    /// ワーカー数を掛けた値がプロセス全体の必要メモリになる。
    #[arg(long, value_name = "BYTES", default_value_t = 4 * 1024 * 1024)]
    pub max_body_size: usize,

    /// キュー待ちの上限秒数(超えると504)
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    pub timeout: u64,

    /// 使用するフォントファイル(複数指定可。リクエストからは変更できない)。
    /// 省略時はシステムフォントを使うが、出力を安定させるため明示を推奨する
    #[arg(long, value_name = "PATH")]
    pub font: Vec<PathBuf>,

    /// `font-family: sans-serif`の実体
    #[arg(long, value_name = "PATH")]
    pub gothic_font: Option<PathBuf>,

    /// `font-family: serif`の実体
    #[arg(long, value_name = "PATH")]
    pub serif_font: Option<PathBuf>,

    /// `font-family: monospace`の実体
    #[arg(long, value_name = "PATH")]
    pub mono_font: Option<PathBuf>,

    /// ローカルファイルの参照を許可する(サーバモードの既定は禁止)
    #[arg(long, action = ArgAction::SetTrue)]
    pub enable_local_file_access: bool,

    /// ローカル参照を許可するディレクトリ(複数指定可)
    ///
    /// `--allow` is the wkhtmltopdf spelling, kept as an alias.
    #[arg(long = "allow-path", visible_alias = "allow", value_name = "PATH")]
    pub allow: Vec<PathBuf>,

    /// http(s)のリモート取得を許可する(既定は禁止)
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_remote_assets: bool,
}

#[cfg(feature = "server")]
impl ServerArgs {
    /// サーバ起動時に固定するフォント指定。
    pub fn font_specs(&self) -> Vec<FontArg> {
        self.font
            .iter()
            .map(|path| FontArg {
                path: path.clone(),
                index: 0,
            })
            .collect()
    }

    /// 汎用family名へ割り当てるフォント。
    pub fn generic_font_args(&self) -> Vec<(GenericFamily, FontArg)> {
        [
            (GenericFamily::SansSerif, self.gothic_font.as_ref()),
            (GenericFamily::Serif, self.serif_font.as_ref()),
            (GenericFamily::Monospace, self.mono_font.as_ref()),
        ]
        .into_iter()
        .filter_map(|(family, path)| {
            path.map(|path| {
                (
                    family,
                    FontArg {
                        path: path.clone(),
                        index: 0,
                    },
                )
            })
        })
        .collect()
    }
}

/// HTML→PDF変換のオプション。
#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// 入力HTMLファイル(`-`で標準入力)
    #[arg(value_name = "INPUT.HTML", required = true)]
    pub input: Option<String>,

    /// 出力先PDF(既定は入力の拡張子を.pdfにしたもの。`-`で標準出力)
    #[arg(short, long, value_name = "OUTPUT.PDF")]
    pub output: Option<String>,

    /// 用紙サイズ
    #[arg(short = 's', long, value_enum, ignore_case = true, value_name = "SIZE")]
    pub page_size: Option<PageSizeName>,

    /// 用紙の幅(--page-sizeより優先。単位はmm/cm/in/pt/px、省略時はmm)
    #[arg(long, value_name = "LENGTH")]
    pub page_width: Option<String>,

    /// 用紙の高さ(--page-sizeより優先)
    #[arg(long, value_name = "LENGTH")]
    pub page_height: Option<String>,

    /// 用紙の向き(Landscapeは最終的な幅と高さを入れ替える)
    #[arg(short = 'O', long, value_enum, ignore_case = true)]
    pub orientation: Option<Orientation>,

    /// 上マージン(既定1in)
    #[arg(short = 'T', long, value_name = "LENGTH")]
    pub margin_top: Option<String>,

    /// 下マージン(既定1in)
    #[arg(short = 'B', long, value_name = "LENGTH")]
    pub margin_bottom: Option<String>,

    /// 左マージン(既定1in)
    #[arg(short = 'L', long, value_name = "LENGTH")]
    pub margin_left: Option<String>,

    /// 右マージン(既定1in)
    #[arg(short = 'R', long, value_name = "LENGTH")]
    pub margin_right: Option<String>,

    /// 使用するフォントファイル(複数指定可。省略時はシステムフォントを使う)
    #[arg(long, value_name = "PATH")]
    pub font: Vec<PathBuf>,

    /// 直前の--fontに対する、TrueType Collection内のフェイス番号
    #[arg(long, value_name = "N")]
    pub font_index: Vec<u32>,

    /// `font-family: sans-serif`の実体として使うフォント
    #[arg(long, value_name = "PATH")]
    pub gothic_font: Option<PathBuf>,

    /// --gothic-fontのフェイス番号
    #[arg(long, value_name = "N", requires = "gothic_font")]
    pub gothic_font_index: Option<u32>,

    /// `font-family: serif`の実体として使うフォント
    #[arg(long, value_name = "PATH")]
    pub serif_font: Option<PathBuf>,

    /// --serif-fontのフェイス番号
    #[arg(long, value_name = "N", requires = "serif_font")]
    pub serif_font_index: Option<u32>,

    /// `font-family: monospace`の実体として使うフォント
    #[arg(long, value_name = "PATH")]
    pub mono_font: Option<PathBuf>,

    /// --mono-fontのフェイス番号
    #[arg(long, value_name = "N", requires = "mono_font")]
    pub mono_font_index: Option<u32>,

    /// PDFのタイトル(未指定ならHTMLの<title>を使う)
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,

    /// PDFの著者(Info辞書の/Author)
    #[arg(long, value_name = "TEXT")]
    pub author: Option<String>,

    /// PDFの主題(Info辞書の/Subject)
    #[arg(long, value_name = "TEXT")]
    pub subject: Option<String>,

    /// PDFのキーワード(Info辞書の/Keywords)
    #[arg(long, value_name = "TEXT")]
    pub keywords: Option<String>,

    /// CSS pxを何dpiとして解釈するか(既定96。72にすると1px=1pt)
    #[arg(short = 'd', long, value_name = "DPI", default_value_t = 96.0)]
    pub dpi: f32,

    /// 拡大率(既定1.0)
    #[arg(long, value_name = "FACTOR", default_value_t = 1.0)]
    pub zoom: f32,

    /// 塗り・線の色をグレースケールにする
    #[arg(short = 'g', long, action = ArgAction::SetTrue)]
    pub grayscale: bool,

    /// PDFオブジェクトのFlate圧縮を行わない(画像データは対象外)
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_pdf_compression: bool,

    /// 相対参照の解決基準(ディレクトリかhttp(s)のURL。標準入力から読む場合に使う)
    #[arg(long, value_name = "URL|DIR")]
    pub base_url: Option<String>,

    /// 画像(<img>とbackground-image)を読み込まない
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_images: bool,

    /// 要素の背景(色・画像)を描かない
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_background: bool,

    /// ユーザーオリジンのCSSファイル(複数指定可)
    #[arg(long, value_name = "PATH")]
    pub user_style_sheet: Vec<PathBuf>,

    /// 算出font-sizeの下限(px)
    #[arg(long, value_name = "PX")]
    pub minimum_font_size: Option<f32>,

    /// 外部リンク(http(s))のPDF注釈を作らない
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_external_links: bool,

    /// 内部リンク(#id)のPDF注釈を作らない
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_internal_links: bool,

    /// 相対URLの外部リンクを絶対URLへ解決せずそのまま書く
    #[arg(long, action = ArgAction::SetTrue)]
    pub keep_relative_links: bool,

    /// ヘッダー左のテキスト([page]等のプレースホルダが使える)
    #[arg(long, value_name = "TEXT")]
    pub header_left: Option<String>,

    /// ヘッダー中央のテキスト
    #[arg(long, value_name = "TEXT")]
    pub header_center: Option<String>,

    /// ヘッダー右のテキスト
    #[arg(long, value_name = "TEXT")]
    pub header_right: Option<String>,

    /// フッター左のテキスト
    #[arg(long, value_name = "TEXT")]
    pub footer_left: Option<String>,

    /// フッター中央のテキスト
    #[arg(long, value_name = "TEXT")]
    pub footer_center: Option<String>,

    /// フッター右のテキスト
    #[arg(long, value_name = "TEXT")]
    pub footer_right: Option<String>,

    /// ヘッダーのフォント名
    #[arg(long, value_name = "NAME")]
    pub header_font_name: Option<String>,

    /// ヘッダーのフォントサイズ(px)
    #[arg(long, value_name = "SIZE")]
    pub header_font_size: Option<f32>,

    /// フッターのフォント名
    #[arg(long, value_name = "NAME")]
    pub footer_font_name: Option<String>,

    /// フッターのフォントサイズ(px)
    #[arg(long, value_name = "SIZE")]
    pub footer_font_size: Option<f32>,

    /// ヘッダーの下に罫線を引く
    #[arg(long, action = ArgAction::SetTrue)]
    pub header_line: bool,

    /// フッターの上に罫線を引く
    #[arg(long, action = ArgAction::SetTrue)]
    pub footer_line: bool,

    /// ヘッダーと本文の間隔(mm)。その分だけ上マージンが増える
    #[arg(long, value_name = "MM")]
    pub header_spacing: Option<f32>,

    /// フッターと本文の間隔(mm)
    #[arg(long, value_name = "MM")]
    pub footer_spacing: Option<f32>,

    /// タイトルとページ番号の既定ヘッダーを付ける
    #[arg(long, action = ArgAction::SetTrue)]
    pub default_header: bool,

    /// ヘッダー/フッター内の[name]を値へ置換する(name=value、複数指定可)
    #[arg(long, value_name = "NAME=VALUE")]
    pub replace: Vec<String>,

    /// 表紙にするHTML(ページ番号に数えず、ヘッダー/フッターも出さない)
    #[arg(long, value_name = "PATH")]
    pub cover: Option<PathBuf>,

    /// 目次を本文の前に挿入する
    #[arg(long, action = ArgAction::SetTrue)]
    pub toc: bool,

    /// 目次の見出し文字列
    #[arg(long, value_name = "TEXT", default_value = "Table of Contents")]
    pub toc_header_text: String,

    /// 目次の階層1段ごとのインデント(CSSの長さ)
    #[arg(long, value_name = "WIDTH", default_value = "1em")]
    pub toc_level_indentation: String,

    /// 目次の階層1段ごとの文字サイズ比
    #[arg(long, value_name = "REAL", default_value_t = 0.8)]
    pub toc_text_size_shrink: f32,

    /// 目次の点線(破線の下線)を引かない
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_dotted_lines: bool,

    /// 目次から見出しへのリンクを張らない
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_toc_links: bool,

    /// 見出しから目次へ戻るリンクを張る
    #[arg(long, action = ArgAction::SetTrue)]
    pub enable_toc_back_links: bool,

    /// ページ番号の起点をずらす
    #[arg(long, value_name = "OFFSET", default_value_t = 0)]
    pub page_offset: usize,

    /// 各ページ上部へ合成するHTML(プレースホルダ展開後にレンダリングする)
    #[arg(long, value_name = "PATH")]
    pub header_html: Option<PathBuf>,

    /// 各ページ下部へ合成するHTML
    #[arg(long, value_name = "PATH")]
    pub footer_html: Option<PathBuf>,

    /// 入力の文字エンコーディング(未指定ならBOM/<meta charset>/UTF-8の順で判定)
    #[arg(long, value_name = "NAME")]
    pub encoding: Option<String>,

    /// 画像・CSS・フォントの取得に失敗したときの挙動
    #[arg(long, value_enum, default_value_t = LoadErrorHandling::Ignore, value_name = "MODE")]
    pub load_media_error_handling: LoadErrorHandling,

    /// 入力そのものの読み込みに失敗したときの挙動(常にabort相当)
    #[arg(long, value_enum, default_value_t = LoadErrorHandling::Abort, value_name = "MODE")]
    pub load_error_handling: LoadErrorHandling,

    /// ローカルファイルの参照を禁止する(既定は許可)
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "enable_local_file_access")]
    pub disable_local_file_access: bool,

    /// ローカルファイルの参照を許可する(既定。サーバモードで明示するためのもの)
    #[arg(long, action = ArgAction::SetTrue)]
    pub enable_local_file_access: bool,

    /// ローカル参照を許可するディレクトリ(複数指定可。指定するとその配下だけ読める)
    ///
    /// `--allow` is the wkhtmltopdf spelling, kept as an alias. `--allow-path`
    /// is the primary name because `--allow` on its own says nothing about
    /// what is allowed, and sits right next to `--allow-remote-assets`.
    #[arg(long = "allow-path", visible_alias = "allow", value_name = "PATH")]
    pub allow: Vec<PathBuf>,

    /// ストリーミングモードで処理する(一部のオプション・CSSは使えず、その場合はエラーになる)
    #[arg(long, action = ArgAction::SetTrue)]
    pub streaming: bool,

    /// <img src>/<link rel=stylesheet href>/@font-faceのurl()のhttp(s)フェッチを許可する
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_remote_assets: bool,

    /// ログの詳細度
    #[arg(long, value_enum, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,

    /// --log-level noneと同じ
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub quiet: bool,

    /// 変換を打ち切る時刻。CLIのオプションではなく、HTTPサーバモードが
    /// `--timeout`から算出して差し込む(`#[arg(skip)]`)。
    ///
    /// ここに置くのは、`render`/`render_to_memory`のシグネチャを変えずに
    /// エンジンまで運ぶため。CLIとRuby拡張では`None`のまま。
    #[arg(skip)]
    pub deadline: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    None,
    Error,
    Warn,
    Info,
}

/// `--page-size`で選べる用紙。CSSの`@page { size: ... }`が受け付ける
/// キーワードと同じ集合にしてある。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PageSizeName {
    #[value(name = "A3")]
    A3,
    #[value(name = "A4")]
    A4,
    #[value(name = "A5")]
    A5,
    #[value(name = "Letter")]
    Letter,
    #[value(name = "Legal")]
    Legal,
}

impl PageSizeName {
    fn to_page_size(self) -> PageSize {
        match self {
            Self::A3 => PageSize::A3,
            Self::A4 => PageSize::A4,
            Self::A5 => PageSize::A5,
            Self::Letter => PageSize::LETTER,
            Self::Legal => PageSize::LEGAL,
        }
    }
}

/// 取得失敗時の挙動(wkhtmltopdf互換。`skip`は入力が1つなので持たない)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoadErrorHandling {
    /// 失敗を無視して続行する(既定)
    Ignore,
    /// 失敗したら中断する
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Orientation {
    #[value(name = "Portrait")]
    Portrait,
    #[value(name = "Landscape")]
    Landscape,
}

/// フォントファイルとフェイス番号の組。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontArg {
    pub path: PathBuf,
    pub index: u32,
}

impl ConvertArgs {
    /// 実効的なログ出力可否(`--quiet`は`--log-level none`と同義)。
    pub fn is_quiet(&self) -> bool {
        self.quiet || self.log_level == LogLevel::None
    }

    /// ページサイズ・マージンのCLI指定を[`PageSettings`]へまとめる。
    ///
    /// ここで返すのは初期値であり、CSSに`@page`の宣言があれば
    /// プロパティ単位でそちらが優先される(合成は
    /// `engine::apply_page_rule_settings_override`が行う)。
    ///
    /// `--page-width`/`--page-height`は`--page-size`より優先し、
    /// `--orientation Landscape`は最後に幅と高さを入れ替える。
    pub fn page_settings(&self) -> Result<PageSettings, String> {
        let defaults = PageSettings::default();

        let mut size = self
            .page_size
            .map(PageSizeName::to_page_size)
            .unwrap_or(defaults.size);
        if let Some(value) = self.page_width.as_deref() {
            size.width = parse_length_px(value)?;
        }
        if let Some(value) = self.page_height.as_deref() {
            size.height = parse_length_px(value)?;
        }
        if self.orientation == Some(Orientation::Landscape) {
            size = size.landscape();
        }
        if size.width <= 0.0 || size.height <= 0.0 {
            return Err("用紙の幅と高さには正の値を指定してください".to_string());
        }

        let mut margin = defaults.margin;
        for (value, edge) in [
            (self.margin_top.as_deref(), &mut margin.top),
            (self.margin_bottom.as_deref(), &mut margin.bottom),
            (self.margin_left.as_deref(), &mut margin.left),
            (self.margin_right.as_deref(), &mut margin.right),
        ] {
            if let Some(value) = value {
                *edge = parse_length_px(value)?;
            }
        }

        // `--header-spacing`/`--footer-spacing`はヘッダー/フッターと本文の
        // 間隔で、その分だけ上下マージンを増やす。
        const MM_TO_PX: f32 = 96.0 / 25.4;
        if let Some(mm) = self.header_spacing {
            margin.top += mm * MM_TO_PX;
        }
        if let Some(mm) = self.footer_spacing {
            margin.bottom += mm * MM_TO_PX;
        }

        let settings = PageSettings { size, margin };
        if settings.content_width() <= 0.0 {
            return Err("左右マージンの合計が用紙の幅以上です".to_string());
        }
        if settings.content_height() <= 0.0 {
            return Err("上下マージンの合計が用紙の高さ以上です".to_string());
        }
        Ok(settings)
    }

    /// PDF書き出しオプションへまとめる。
    ///
    /// `--title`が未指定の場合の`<title>`フォールバックはエンジン側で行う。
    pub fn pdf_output_options(&self) -> PdfOutputOptions {
        PdfOutputOptions {
            metadata: DocumentMetadata {
                title: self.title.clone(),
                author: self.author.clone(),
                subject: self.subject.clone(),
                keywords: self.keywords.clone(),
            },
            compress: !self.no_pdf_compression,
            scale: PdfOutputOptions::scale_from_dpi_and_zoom(self.dpi, self.zoom),
            grayscale: self.grayscale,
            header_line: self.header_line,
            footer_line: self.footer_line,
        }
    }

    /// 描画内容のオプション([`ContentOptions`])へまとめる。
    /// `--user-style-sheet`のファイル読み込みもここで行う。
    pub fn content_options(&self) -> Result<ContentOptions, String> {
        let mut user_stylesheets = Vec::with_capacity(self.user_style_sheet.len());
        for path in &self.user_style_sheet {
            let css = std::fs::read_to_string(path)
                .map_err(|e| format!("{}の読み込みに失敗しました: {e}", path.display()))?;
            user_stylesheets.push(css);
        }

        Ok(ContentOptions {
            load_images: !self.no_images,
            draw_backgrounds: !self.no_background,
            user_stylesheets,
            minimum_font_size: self.minimum_font_size,
            external_links: !self.disable_external_links,
            internal_links: !self.disable_internal_links,
            keep_relative_links: self.keep_relative_links,
            abort_on_media_error: self.load_media_error_handling == LoadErrorHandling::Abort,
        })
    }

    /// `--replace name=value`をパースする。
    pub fn replacements(&self) -> Result<Vec<(String, String)>, String> {
        self.replace
            .iter()
            .map(|item| {
                item.split_once('=')
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .ok_or_else(|| format!("--replaceはname=valueの形で指定してください: {item}"))
            })
            .collect()
    }

    /// ヘッダー/フッターの簡易オプションをまとめる。
    pub fn simple_header_footer(&self) -> SimpleHeaderFooter {
        let mut boxes = Vec::new();
        if self.default_header {
            // wkhtmltopdfの`--default-header`相当(タイトルとページ番号)。
            boxes.push(MarginBoxText {
                area: "top-left",
                text: "[title]".to_string(),
            });
            boxes.push(MarginBoxText {
                area: "top-right",
                text: "[page]".to_string(),
            });
        }
        for (area, text) in [
            ("top-left", &self.header_left),
            ("top-center", &self.header_center),
            ("top-right", &self.header_right),
            ("bottom-left", &self.footer_left),
            ("bottom-center", &self.footer_center),
            ("bottom-right", &self.footer_right),
        ] {
            if let Some(text) = text {
                // 明示指定は`--default-header`より後に置いて上書きする。
                boxes.retain(|b: &MarginBoxText| b.area != area);
                boxes.push(MarginBoxText {
                    area,
                    text: text.clone(),
                });
            }
        }

        // 同じ側にHTMLが指定されていれば、そちらが優先(二重描画を避ける)。
        if self.header_html.is_some() {
            boxes.retain(|b| !b.area.starts_with("top"));
        }
        if self.footer_html.is_some() {
            boxes.retain(|b| !b.area.starts_with("bottom"));
        }

        SimpleHeaderFooter {
            boxes,
            header_font_name: self.header_font_name.clone(),
            header_font_size: self.header_font_size,
            footer_font_name: self.footer_font_name.clone(),
            footer_font_size: self.footer_font_size,
        }
    }

    /// 目次の見た目のオプション(wkhtmltopdf互換)。
    pub fn toc_options(&self) -> TocOptions {
        TocOptions {
            header_text: self.toc_header_text.clone(),
            level_indentation: self.toc_level_indentation.clone(),
            text_size_shrink: self.toc_text_size_shrink,
            dotted_lines: !self.disable_dotted_lines,
            links: !self.disable_toc_links,
        }
    }

    /// ローカルファイル参照の許可設定。
    ///
    /// The `--allow-path` directories are resolved to real paths here. Resolving
    /// them at each reference would fall back to comparing the raw paths when
    /// resolution fails, leaving `..` in the comparison. A directory that cannot
    /// be resolved is an error at startup rather than a silent skip.
    pub fn local_access(&self) -> Result<LocalAccess, String> {
        let mut allowed_dirs = Vec::with_capacity(self.allow.len());
        for dir in &self.allow {
            let canonical = dir.canonicalize().map_err(|e| {
                format!(
                    "--allow-pathに指定したディレクトリを解決できません: {} ({e})",
                    dir.display()
                )
            })?;
            if !canonical.is_dir() {
                return Err(format!(
                    "--allow-pathにはディレクトリを指定してください: {}",
                    dir.display()
                ));
            }
            allowed_dirs.push(canonical);
        }
        Ok(LocalAccess {
            allow: !self.disable_local_file_access,
            allowed_dirs,
        })
    }

    /// 処理モード(`--streaming`)。
    pub fn mode(&self) -> Mode {
        if self.streaming {
            Mode::Streaming
        } else {
            Mode::Batch
        }
    }

    /// `--dpi`/`--zoom`の値の妥当性(正の有限値であること)。
    pub fn validate_scaling(&self) -> Result<(), String> {
        if !(self.dpi.is_finite() && self.dpi > 0.0) {
            return Err(format!("--dpiには正の値を指定してください: {}", self.dpi));
        }
        if !(self.zoom.is_finite() && self.zoom > 0.0) {
            return Err(format!("--zoomには正の値を指定してください: {}", self.zoom));
        }
        Ok(())
    }

    /// `--font`と`--font-index`をコマンドラインでの出現順に基づいて
    /// 組にする。
    ///
    /// `--font-index`は「直前の`--font`に対する指定」という位置依存の意味を
    /// 持つ(手書きパーサ時代からの互換)。clapは値をオプションごとにまとめて
    /// しまうため、`ArgMatches::indices_of`で元の位置を取り直して対応付ける。
    pub fn font_specs(&self, matches: &ArgMatches) -> Result<Vec<FontArg>, String> {
        let font_positions: Vec<usize> = matches
            .indices_of("font")
            .map(|it| it.collect())
            .unwrap_or_default();
        let index_positions: Vec<usize> = matches
            .indices_of("font_index")
            .map(|it| it.collect())
            .unwrap_or_default();

        let mut specs: Vec<FontArg> = self
            .font
            .iter()
            .map(|path| FontArg {
                path: path.clone(),
                index: 0,
            })
            .collect();

        for (nth, position) in index_positions.iter().enumerate() {
            // その`--font-index`より手前にある`--font`のうち最後のもの。
            let target = font_positions.iter().rposition(|p| p < position);
            match target {
                Some(i) => specs[i].index = self.font_index[nth],
                None => {
                    return Err("--font-indexは直前の--fontに対して指定してください".to_string())
                }
            }
        }

        Ok(specs)
    }

    /// 汎用family名(`sans-serif`/`serif`/`monospace`)へ明示指定された
    /// フォントの組。指定が無い汎用名は含めない(システムフォントで解決する)。
    pub fn generic_font_specs(&self) -> Vec<(GenericFamily, FontArg)> {
        [
            (
                GenericFamily::SansSerif,
                self.gothic_font.as_ref(),
                self.gothic_font_index,
            ),
            (
                GenericFamily::Serif,
                self.serif_font.as_ref(),
                self.serif_font_index,
            ),
            (
                GenericFamily::Monospace,
                self.mono_font.as_ref(),
                self.mono_font_index,
            ),
        ]
        .into_iter()
        .filter_map(|(family, path, index)| {
            path.map(|path| {
                (
                    family,
                    FontArg {
                        path: path.clone(),
                        index: index.unwrap_or(0),
                    },
                )
            })
        })
        .collect()
    }

    /// 入力が標準入力か。
    pub fn reads_stdin(&self) -> bool {
        self.input.as_deref() == Some(STD_STREAM)
    }

    /// 出力先。`-o`省略時は入力の拡張子を`.pdf`に置き換える。
    /// 標準出力の場合は`None`を返す。
    pub fn output_path(&self) -> Result<Option<PathBuf>, String> {
        match self.output.as_deref() {
            Some(STD_STREAM) => Ok(None),
            Some(path) => Ok(Some(PathBuf::from(path))),
            None => {
                if self.reads_stdin() {
                    return Err(
                        "標準入力から読む場合は-o/--outputで出力先を指定してください(標準出力は`-o -`)"
                            .to_string(),
                    );
                }
                let input = PathBuf::from(self.input.as_deref().unwrap_or_default());
                Ok(Some(input.with_extension("pdf")))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> (Cli, ArgMatches) {
        let matches = Cli::command().get_matches_from(args);
        let cli = <Cli as clap::FromArgMatches>::from_arg_matches(&matches).unwrap();
        (cli, matches)
    }

    #[test]
    fn font_index_applies_to_the_preceding_font() {
        let (cli, matches) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--font",
            "b.ttc",
            "--font-index",
            "2",
            "--font",
            "c.ttf",
        ]);
        let specs = cli.convert.font_specs(&matches).unwrap();
        assert_eq!(
            specs,
            vec![
                FontArg {
                    path: PathBuf::from("a.ttf"),
                    index: 0
                },
                FontArg {
                    path: PathBuf::from("b.ttc"),
                    index: 2
                },
                FontArg {
                    path: PathBuf::from("c.ttf"),
                    index: 0
                },
            ]
        );
    }

    #[test]
    fn font_index_before_any_font_is_an_error() {
        let (cli, matches) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font-index",
            "1",
            "--font",
            "a.ttf",
        ]);
        assert!(cli.convert.font_specs(&matches).is_err());
    }

    #[test]
    fn output_defaults_to_the_input_with_pdf_extension() {
        let (cli, _) = parse(&["sghtmltopdf", "docs/in.html", "--font", "a.ttf"]);
        assert_eq!(
            cli.convert.output_path().unwrap(),
            Some(PathBuf::from("docs/in.pdf"))
        );
    }

    #[test]
    fn dash_selects_std_streams() {
        let (cli, _) = parse(&["sghtmltopdf", "-", "--font", "a.ttf", "-o", "-"]);
        assert!(cli.convert.reads_stdin());
        assert_eq!(cli.convert.output_path().unwrap(), None);
    }

    #[test]
    fn stdin_input_requires_an_explicit_output() {
        let (cli, _) = parse(&["sghtmltopdf", "-", "--font", "a.ttf"]);
        assert!(cli.convert.output_path().is_err());
    }

    #[test]
    fn quiet_is_equivalent_to_log_level_none() {
        let (cli, _) = parse(&["sghtmltopdf", "in.html", "--font", "a.ttf", "-q"]);
        assert!(cli.convert.is_quiet());
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--log-level",
            "none",
        ]);
        assert!(cli.convert.is_quiet());
        let (cli, _) = parse(&["sghtmltopdf", "in.html", "--font", "a.ttf"]);
        assert!(!cli.convert.is_quiet());
    }

    #[cfg(feature = "server")]
    #[test]
    fn server_subcommand_does_not_require_convert_args() {
        // `server`は`--font`が必須(リクエストからは変えられないため)。
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "server",
            "--listen",
            "0.0.0.0:9000",
            "--font",
            "a.ttf",
        ]);
        match cli.command {
            Some(Command::Server(ref args)) => assert_eq!(args.listen, "0.0.0.0:9000"),
            _ => panic!("server subcommand should be parsed"),
        }
    }

    #[test]
    fn page_size_name_is_case_insensitive_and_maps_to_the_layout_constants() {
        let (cli, _) = parse(&["sghtmltopdf", "in.html", "--font", "a.ttf", "-s", "a5"]);
        let settings = cli.convert.page_settings().unwrap();
        assert_eq!(settings.size, PageSize::A5);
    }

    #[test]
    fn explicit_width_and_height_win_over_page_size() {
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--page-size",
            "A4",
            "--page-width",
            "400px",
            "--page-height",
            "500px",
        ]);
        let settings = cli.convert.page_settings().unwrap();
        assert_eq!(settings.size.width, 400.0);
        assert_eq!(settings.size.height, 500.0);
    }

    #[test]
    fn landscape_swaps_width_and_height_last() {
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--page-width",
            "400px",
            "--page-height",
            "500px",
            "-O",
            "Landscape",
        ]);
        let settings = cli.convert.page_settings().unwrap();
        assert_eq!(settings.size.width, 500.0);
        assert_eq!(settings.size.height, 400.0);
    }

    #[test]
    fn margins_default_to_one_inch_and_are_overridden_individually() {
        let (cli, _) = parse(&["sghtmltopdf", "in.html", "--font", "a.ttf"]);
        let settings = cli.convert.page_settings().unwrap();
        assert_eq!(settings.margin.top, 96.0);
        assert_eq!(settings.margin.left, 96.0);

        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "-T",
            "25.4mm",
            "--margin-left",
            "0",
        ]);
        let settings = cli.convert.page_settings().unwrap();
        assert!((settings.margin.top - 96.0).abs() < 0.01);
        assert_eq!(settings.margin.left, 0.0);
        // 指定しなかった辺は既定のまま。
        assert_eq!(settings.margin.right, 96.0);
    }

    #[test]
    fn margins_larger_than_the_page_are_rejected() {
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--page-width",
            "100px",
            "--margin-left",
            "60px",
            "--margin-right",
            "60px",
        ]);
        assert!(cli.convert.page_settings().is_err());
    }

    #[test]
    fn a_bad_length_is_reported_as_an_error() {
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--margin-top",
            "10em",
        ]);
        assert!(cli.convert.page_settings().is_err());
    }

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// The `--allow-path` directories are resolved to real paths at startup.
    #[test]
    fn allow_dirs_are_resolved_to_real_paths() {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-allow-test-{}-resolved",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("assets")).unwrap();

        // `<dir>/assets/..` を渡しても `<dir>` に畳まれる。
        let dotted = dir.join("assets").join("..");
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--allow",
            dotted.to_str().unwrap(),
        ]);
        let access = cli.convert.local_access().expect("実在するので解決できる");
        assert_eq!(access.allowed_dirs, vec![dir.canonicalize().unwrap()]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An unresolvable `--allow-path` is an error, not a silent skip.
    /// (無視すると許可範囲が意図せず変わる)。
    #[test]
    fn an_allow_dir_that_does_not_exist_is_an_error() {
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--allow",
            "/definitely/not/a/real/directory",
        ]);
        let err = cli.convert.local_access().unwrap_err();
        assert!(err.contains("--allow"), "got: {err}");
    }

    /// Passing a file to `--allow-path` is an error too.
    #[test]
    fn an_allow_path_that_is_not_a_directory_is_an_error() {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-allow-test-{}-not-a-dir",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        std::fs::write(&file, b"x").unwrap();

        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--allow",
            file.to_str().unwrap(),
        ]);
        let err = cli.convert.local_access().unwrap_err();
        assert!(err.contains("ディレクトリ"), "got: {err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
