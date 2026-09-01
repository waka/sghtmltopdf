//! Running the convert subcommand (the default when no subcommand is given).

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

/// Wraps the output target (file or stdout) in a single type.
/// [`Engine`] is generic over `S: Sink`, so the branching is absorbed here.
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

    let sink =
        match output_path.as_ref() {
            Some(path) => OutputSink::File(FileSink::create(path).map_err(|e| {
                CliError::Input(format!("failed to create {}: {e}", path.display()))
            })?),
            None => OutputSink::Stdout(StdoutSink::new()),
        };

    // The input stays a Read too, so a large HTML file is never held in memory whole.
    match open_input(args)? {
        InputSource::Stdin => render(args, &fonts, io::stdin().lock(), sink)?,
        InputSource::File(file) => render(args, &fonts, file, sink)?,
    }

    if !args.is_quiet() {
        match output_path.as_ref() {
            Some(path) => eprintln!("wrote the PDF to {}", path.display()),
            None => eprintln!("wrote the PDF to standard output"),
        }
    }
    Ok(())
}

/// Variant of [`render`] that returns the bytes in memory (for the HTTP server). Takes a
/// Sink with `Output = Vec<u8>`, such as `MemorySink`, and returns the PDF bytes.
pub fn render_to_memory<S: Sink<Output = Vec<u8>, Error = io::Error>>(
    args: &ConvertArgs,
    fonts: &[FontArg],
    reader: impl Read,
    sink: S,
) -> Result<Vec<u8>, CliError> {
    render_from_reader(args, fonts, reader, sink)
}

/// Convert the HTML bytes and write the result to `sink`.
///
/// The shared execution path for the CLI (`run`) and the HTTP server ([`super::server`]).
/// Fonts are resolved by the caller and passed in (the CLI uses the order the `--font`
/// options appear in; the server builds them from its startup options).
pub fn render<S: Sink<Output = (), Error = io::Error>>(
    args: &ConvertArgs,
    fonts: &[FontArg],
    reader: impl Read,
    sink: S,
) -> Result<(), CliError> {
    render_from_reader(args, fonts, reader, sink)
}

/// How much of a single `read` is handed to `Engine::feed` at a time.
const FEED_CHUNK: usize = 64 * 1024;

/// The body of [`render`]/[`render_to_memory`].
///
/// The input is passed to `Engine::feed` in chunks rather than read to the end.
/// Only the prefix needed to detect the encoding is buffered internally by
/// [`crate::html::StreamingDecoder`].
fn render_from_reader<S: Sink<Error = io::Error>>(
    args: &ConvertArgs,
    fonts: &[FontArg],
    mut reader: impl Read,
    sink: S,
) -> Result<S::Output, CliError> {
    let (base_dir, base_href) = resolve_base(args)?;

    // The CLI's page settings are the *initial* values: an author `@page` declaration wins
    // per property. `engine::apply_page_rule_settings_override` does the merging.
    let settings = args.page_settings().map_err(CliError::Usage)?;
    args.validate_scaling().map_err(CliError::Usage)?;
    let content_options = args.content_options().map_err(CliError::Input)?;

    // Fold the simple header/footer options into `@page` rules. Resolving `[title]` needs
    // the PDF title, so `--title` wins and, if it is unset, the value is empty here
    // (the engine only fills `/Title` from `<title>`).
    let replacements = args.replacements().map_err(CliError::Usage)?;
    let placeholders =
        crate::cli::header_footer::PlaceholderValues::new(args.title.clone(), replacements);
    let extra_page_rules = match args.simple_header_footer().to_page_css(&placeholders) {
        Some(css) => crate::style::parse_stylesheet(&css).page_rules,
        None => Vec::new(),
    };

    // For `--header-html`/`--footer-html`, expand every placeholder except the page
    // numbers at read time; the engine fills the remaining `[page]`/`[topage]` in per page.
    // Same for the cover page.
    let cover_html = read_optional_html(args.cover.as_deref(), &placeholders)?
        .map(|html| placeholders.expand_all(&html, 1, None));

    // Table of contents. Building the HTML lives in the CLI layer (`cli::toc`); the engine
    // just receives it as a "heading list -> HTML" function.
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

    // Normalise the input to UTF-8 (BOM > --encoding > <meta charset> > UTF-8) and
    // `feed` it as it is read.
    let mut decoder =
        crate::html::StreamingDecoder::new(args.encoding.as_deref()).map_err(CliError::Usage)?;
    let mut buffer = vec![0u8; FEED_CHUNK];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| CliError::Input(format!("failed to read the input: {e}")))?;
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

/// Map an `EngineError` to an exit code. Write failures and font-loading failures are
/// resource errors (2); the engine's own limits are rendering errors (3).
fn engine_error(e: EngineError<io::Error>) -> CliError {
    match e {
        EngineError::Io(e) => CliError::Input(format!("failed to write the PDF: {e}")),
        EngineError::Font(msg) => CliError::Input(msg),
        EngineError::UnsupportedInStreamingMode(msg) => CliError::Render(msg.to_string()),
        // This is a problem with the input HTML, so treat it as an input error (server mode
        // then returns 400: "the HTML you sent is invalid", not "the server broke").
        e @ EngineError::DepthLimitExceeded { .. } => CliError::Input(e.to_string()),
        e @ EngineError::NodeLimitExceeded { .. } => CliError::Input(e.to_string()),
        e @ EngineError::TimedOut => CliError::Timeout(e.to_string()),
        EngineError::MediaLoad(msg) => {
            CliError::Input(format!("failed to fetch a resource: {msg}"))
        }
    }
}

/// Read the `--header-html`/`--footer-html` file and expand every placeholder except
/// the page numbers.
fn read_optional_html(
    path: Option<&std::path::Path>,
    placeholders: &PlaceholderValues,
) -> Result<Option<String>, CliError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let bytes = std::fs::read(path)
        .map_err(|e| CliError::Input(format!("failed to read {}: {e}", path.display())))?;
    let text = crate::html::decode_html(&bytes, None).map_err(CliError::Usage)?;
    // Keep `[page]`/`[topage]`, expand everything else first.
    Ok(Some(placeholders.expand_keeping_page_tokens(&text)))
}

/// Where the input comes from. Stdin and files are kept separate so both stay a `Read`.
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
        .map_err(|e| CliError::Input(format!("failed to read {}: {e}", path.display())))?;
    Ok(InputSource::File(file))
}

/// Decide what relative references resolve against.
///
/// * If `--base-url` is an http(s) URL, pass it as the default for `<base href>`
///   (a `<base href>` in the HTML still wins)
/// * If `--base-url` is a directory, use it as the base directory for local resolution
/// * If unset, use the directory holding the input HTML (the current directory for stdin)
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
            "--base-url must be a directory or an http(s) URL: {base_url}"
        )));
    }
    Ok((Some(dir), None))
}
