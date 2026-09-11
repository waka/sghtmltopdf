//! 変換サブコマンド(サブコマンド省略時の既定)の実行。

use std::io::{self, Read};
use std::path::PathBuf;

use clap::ArgMatches;

use crate::engine::{
    Engine, EngineError, EngineOptions, FontSpec as EngineFontSpec, HeaderFooterHtml,
    HeaderFooterPlaceholders, TocHeading, TocSettings,
};
use crate::sink::{FileSink, Sink, StdoutSink};

use super::header_footer::PlaceholderValues;
use super::options::{ConvertArgs, FontArg};
use super::toc::{build_toc_html, TocEntry};
use super::CliError;

/// 出力先(ファイル/標準出力)を1つの型にまとめる。
/// [`Engine`]は`S: Sink`のジェネリックなので、分岐をここで吸収する。
enum OutputSink {
    File(FileSink),
    Stdout(StdoutSink),
}

impl Sink for OutputSink {
    type Output = ();
    type Error = io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        match self {
            Self::File(sink) => sink.write(bytes),
            Self::Stdout(sink) => sink.write(bytes),
        }
    }

    fn finish(self) -> Result<Self::Output, Self::Error> {
        match self {
            Self::File(sink) => sink.finish(),
            Self::Stdout(sink) => sink.finish(),
        }
    }
}

pub fn run(args: &ConvertArgs, matches: &ArgMatches) -> Result<(), CliError> {
    let fonts = args.font_specs(matches).map_err(CliError::Usage)?;
    let output_path = args.output_path().map_err(CliError::Usage)?;

    let sink = match output_path.as_ref() {
        Some(path) => OutputSink::File(FileSink::create(path).map_err(|e| {
            CliError::Input(format!("{}の作成に失敗しました: {e}", path.display()))
        })?),
        None => OutputSink::Stdout(StdoutSink::new()),
    };

    // 入力もReadのまま渡す(大きなHTMLを丸ごとメモリに載せない)。
    match open_input(args)? {
        InputSource::Stdin => render(args, &fonts, io::stdin().lock(), sink)?,
        InputSource::File(file) => render(args, &fonts, file, sink)?,
    }

    if !args.is_quiet() {
        match output_path.as_ref() {
            Some(path) => eprintln!("PDFを書き出しました: {}", path.display()),
            None => eprintln!("PDFを標準出力へ書き出しました"),
        }
    }
    Ok(())
}

/// [`render`]のメモリ返却版(HTTPサーバ用)。`MemorySink`のように
/// `Output = Vec<u8>`のSinkを受け取り、PDFバイト列を返す。
pub fn render_to_memory<S: Sink<Output = Vec<u8>, Error = io::Error>>(
    args: &ConvertArgs,
    fonts: &[FontArg],
    reader: impl Read,
    sink: S,
) -> Result<Vec<u8>, CliError> {
    render_from_reader(args, fonts, reader, sink)
}

/// HTMLバイト列を変換して`sink`へ書き出す。
///
/// CLI(`run`)とHTTPサーバ([`super::server`])の共通の実行経路。フォントは
/// 呼び出し側が解決して渡す(CLIは`--font`の
/// 出現順、サーバは起動時オプションから作る)。
pub fn render<S: Sink<Output = (), Error = io::Error>>(
    args: &ConvertArgs,
    fonts: &[FontArg],
    reader: impl Read,
    sink: S,
) -> Result<(), CliError> {
    render_from_reader(args, fonts, reader, sink)
}

/// 1回の`read`で`Engine::feed`へ渡す量。
const FEED_CHUNK: usize = 64 * 1024;

/// [`render`]/[`render_to_memory`]の実体。
///
/// 入力を読み切らずにチャンク単位で`Engine::feed`へ渡す。
/// エンコーディングの判定に必要な先頭だけは
/// [`crate::html::StreamingDecoder`]が内部でバッファする。
fn render_from_reader<S: Sink<Error = io::Error>>(
    args: &ConvertArgs,
    fonts: &[FontArg],
    mut reader: impl Read,
    sink: S,
) -> Result<S::Output, CliError> {
    let (base_dir, base_href) = resolve_base(args)?;

    // CLIのページ設定は「初期値」であり、著者CSSの`@page`宣言があれば
    // プロパティ単位でそちらが優先される。合成は
    // `engine::apply_page_rule_settings_override`が行う。
    let settings = args.page_settings().map_err(CliError::Usage)?;
    args.validate_scaling().map_err(CliError::Usage)?;
    let content_options = args.content_options().map_err(CliError::Input)?;

    // ヘッダー/フッターの簡易オプションを`@page`ルールへ合成する。`[title]`の
    // 解決にはPDFタイトルが要るので、`--title`優先・未指定ならここでは空
    // (エンジンが`<title>`で埋めるのは`/Title`だけ)。
    let replacements = args.replacements().map_err(CliError::Usage)?;
    let placeholders =
        crate::cli::header_footer::PlaceholderValues::new(args.title.clone(), replacements);
    let extra_page_rules = match args.simple_header_footer().to_page_css(&placeholders) {
        Some(css) => crate::style::parse_stylesheet(&css).page_rules,
        None => Vec::new(),
    };

    // `--header-html`/`--footer-html`は読み込み時点でページ番号以外の
    // プレースホルダを展開しておく。残った`[page]`/`[topage]`はエンジンが
    // ページごとに差し込む。表紙。ページ
    // 番号以外のプレースホルダは展開しておく。
    let cover_html = read_optional_html(args.cover.as_deref(), &placeholders)?
        .map(|html| placeholders.expand_all(&html, 1, None));

    // 目次。HTMLの組み立てはCLI層(`cli::toc`)が持ち、エンジンは「見出し一覧
    // → HTML」の関数として受け取る。
    let toc_options = args.toc_options();
    let back_links = args.enable_toc_back_links;
    let toc_settings = TocSettings {
        enabled: args.toc,
        build_html: std::rc::Rc::new(move |headings: &[TocHeading]| {
            let entries: Vec<TocEntry> = headings
                .iter()
                .enumerate()
                .map(|(i, h)| TocEntry {
                    level: h.level,
                    title: h.title.clone(),
                    page: h.body_page,
                    anchor: h.anchor.clone(),
                    back_anchor: back_links.then(|| format!("__sgtocback_{i}")),
                })
                .collect();
            build_toc_html(&entries, &toc_options)
        }),
        back_links,
    };

    let header_footer_html = HeaderFooterHtml {
        header: read_optional_html(args.header_html.as_deref(), &placeholders)?,
        footer: read_optional_html(args.footer_html.as_deref(), &placeholders)?,
        placeholders: HeaderFooterPlaceholders {
            page_token: "[page]".to_string(),
            total_pages_token: "[topage]".to_string(),
        },
    };

    let engine_options = EngineOptions {
        mode: args.mode(),
        settings,
        fonts: fonts
            .iter()
            .map(|spec| EngineFontSpec {
                path: spec.path.clone(),
                index: spec.index,
            })
            .collect(),
        generic_fonts: args
            .generic_font_specs()
            .into_iter()
            .map(|(family, spec)| {
                (
                    family,
                    EngineFontSpec {
                        path: spec.path,
                        index: spec.index,
                    },
                )
            })
            .collect(),
        base_dir,
        base_href,
        allow_remote_assets: args.allow_remote_assets,
        disable_system_fonts: args.disable_system_fonts,
        output: args.pdf_output_options(),
        content: content_options,
        local_access: args.local_access().map_err(CliError::Input)?,
        extra_page_rules,
        deadline: args.deadline,
        header_footer_html,
        cover_html,
        toc: toc_settings,
        page_offset: args.page_offset,
    };

    let mut engine = Engine::new(engine_options, sink);

    // 入力をUTF-8へ揃えながら(BOM > --encoding > <meta charset> > UTF-8)、
    // 読んだそばから`feed`する。
    let mut decoder =
        crate::html::StreamingDecoder::new(args.encoding.as_deref()).map_err(CliError::Usage)?;
    let mut buffer = vec![0u8; FEED_CHUNK];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| CliError::Input(format!("入力の読み込みに失敗しました: {e}")))?;
        if read == 0 {
            break;
        }
        let text = decoder.push(&buffer[..read]);
        if !text.is_empty() {
            engine.feed(text.as_bytes()).map_err(engine_error)?;
        }
    }
    let tail = decoder.finish();
    if !tail.is_empty() {
        engine.feed(tail.as_bytes()).map_err(engine_error)?;
    }

    engine.finish().map_err(engine_error)
}

/// `EngineError`をexit codeへ対応付ける。書き込み失敗・フォント読み込み失敗は
/// リソースエラー(2)、エンジン自身の制約違反はレンダリングエラー(3)。
fn engine_error(e: EngineError<io::Error>) -> CliError {
    match e {
        EngineError::Io(e) => CliError::Input(format!("PDFの書き込みに失敗しました: {e}")),
        EngineError::Font(msg) => CliError::Input(msg),
        EngineError::UnsupportedInStreamingMode(msg) => CliError::Render(msg.to_string()),
        // 入力HTML側の問題なので入力エラー扱いにする(サーバモードでは400になり、
        // 「サーバが壊れた」ではなく「送られたHTMLが不正」として返る)。
        e @ EngineError::DepthLimitExceeded { .. } => CliError::Input(e.to_string()),
        e @ EngineError::NodeLimitExceeded { .. } => CliError::Input(e.to_string()),
        e @ EngineError::TimedOut => CliError::Timeout(e.to_string()),
        EngineError::MediaLoad(msg) => {
            CliError::Input(format!("リソースの取得に失敗しました: {msg}"))
        }
    }
}

/// `--header-html`/`--footer-html`のファイルを読み、ページ番号以外の
/// プレースホルダを展開する。
fn read_optional_html(
    path: Option<&std::path::Path>,
    placeholders: &PlaceholderValues,
) -> Result<Option<String>, CliError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let bytes = std::fs::read(path)
        .map_err(|e| CliError::Input(format!("{}の読み込みに失敗しました: {e}", path.display())))?;
    let text = crate::html::decode_html(&bytes, None).map_err(CliError::Usage)?;
    // `[page]`/`[topage]`は残し、それ以外を先に展開する。
    Ok(Some(placeholders.expand_keeping_page_tokens(&text)))
}

/// 入力の取得元。`Read`のまま扱うため、標準入力とファイルを分けて持つ。
enum InputSource {
    Stdin,
    File(std::fs::File),
}

fn open_input(args: &ConvertArgs) -> Result<InputSource, CliError> {
    if args.reads_stdin() {
        return Ok(InputSource::Stdin);
    }
    let path = PathBuf::from(args.input.as_deref().unwrap_or_default());
    let file = std::fs::File::open(&path)
        .map_err(|e| CliError::Input(format!("{}の読み込みに失敗しました: {e}", path.display())))?;
    Ok(InputSource::File(file))
}

/// 相対参照の解決基準を決める。
///
/// * `--base-url`がhttp(s)のURLなら`<base href>`の既定値として渡す
///   (HTML内に`<base href>`があればそちらが優先される)
/// * `--base-url`がディレクトリならローカル解決の基準ディレクトリにする
/// * 未指定なら入力HTMLのあるディレクトリ(標準入力の場合はカレント)
fn resolve_base(args: &ConvertArgs) -> Result<(Option<PathBuf>, Option<String>), CliError> {
    let input_dir = if args.reads_stdin() {
        None
    } else {
        PathBuf::from(args.input.as_deref().unwrap_or_default())
            .parent()
            .map(|p| p.to_path_buf())
    };

    let Some(base_url) = args.base_url.as_deref() else {
        return Ok((input_dir, None));
    };

    let lower = base_url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Ok((input_dir, Some(base_url.to_string())));
    }

    let dir = PathBuf::from(base_url);
    if !dir.is_dir() {
        return Err(CliError::Input(format!(
            "--base-urlにはディレクトリかhttp(s)のURLを指定してください: {base_url}"
        )));
    }
    Ok((Some(dir), None))
}
