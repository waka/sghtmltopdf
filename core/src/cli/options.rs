//! Definitions of the CLI options.

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

/// What `-` means for the input and the output (stdin/stdout).
pub const STD_STREAM: &str = "-";

#[derive(Debug, Parser)]
#[command(
    name = "sghtmltopdf",
    version,
    about = "An HTML-to-PDF renderer that does not depend on Chromium, WebKit or Gecko",
    // Conversion is not a subcommand; it stays a positional argument.
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
    /// Listen as an HTTP server and convert HTML to PDF via POST /pdf
    Server(ServerArgs),
}

#[cfg(feature = "server")]
#[derive(Debug, Clone, Args)]
pub struct ServerArgs {
    /// Address to listen on (loopback by default; expose it through a reverse proxy)
    #[arg(long, default_value = "127.0.0.1:8080")]
    pub listen: String,

    /// Number of worker threads converting concurrently (default: CPU core count)
    #[arg(long, value_name = "N")]
    pub workers: Option<usize>,

    /// Maximum number of queued requests (default: workers x 4). Returns 503 beyond that
    #[arg(long, value_name = "N")]
    pub max_queue: Option<usize>,

    /// Maximum request body size in bytes
    ///
    /// Memory proportional to the amount of text is not bounded by `MAX_NODES`, so this
    /// is what bounds it. Measurements show about 185MiB per 1MiB of input (worst case,
    /// packed with CJK text), so 4MiB works out to roughly 750MiB. Multiply by the worker
    /// count for the memory the whole process needs.
    #[arg(long, value_name = "BYTES", default_value_t = 4 * 1024 * 1024)]
    pub max_body_size: usize,

    /// Maximum seconds a request may wait in the queue (504 beyond that)
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    pub timeout: u64,

    /// Font files to use (repeatable; cannot be changed per request).
    /// System fonts are used if omitted, but naming them is recommended for stable output
    #[arg(long, value_name = "PATH")]
    pub font: Vec<PathBuf>,

    /// The font behind `font-family: sans-serif`
    #[arg(long, value_name = "PATH")]
    pub gothic_font: Option<PathBuf>,

    /// The font behind `font-family: serif`
    #[arg(long, value_name = "PATH")]
    pub serif_font: Option<PathBuf>,

    /// The font behind `font-family: monospace`
    #[arg(long, value_name = "PATH")]
    pub mono_font: Option<PathBuf>,

    /// Allow references to local files (forbidden by default in server mode)
    #[arg(long, action = ArgAction::SetTrue)]
    pub enable_local_file_access: bool,

    /// Directories local references may read from (repeatable)
    #[arg(long, value_name = "PATH")]
    pub allow: Vec<PathBuf>,

    /// Allow remote http(s) fetches (forbidden by default)
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_remote_assets: bool,
}

#[cfg(feature = "server")]
impl ServerArgs {
    /// Font settings fixed when the server starts.
    pub fn font_specs(&self) -> Vec<FontArg> {
        self.font
            .iter()
            .map(|path| FontArg {
                path: path.clone(),
                index: 0,
            })
            .collect()
    }

    /// Fonts assigned to the generic family names.
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

/// Options for HTML-to-PDF conversion.
#[derive(Debug, Args)]
pub struct ConvertArgs {
    /// Input HTML file (`-` for standard input)
    #[arg(value_name = "INPUT.HTML", required = true)]
    pub input: Option<String>,

    /// Output PDF (defaults to the input path with a .pdf extension; `-` for standard output)
    #[arg(short, long, value_name = "OUTPUT.PDF")]
    pub output: Option<String>,

    /// Paper size
    #[arg(short = 's', long, value_enum, ignore_case = true, value_name = "SIZE")]
    pub page_size: Option<PageSizeName>,

    /// Paper width (wins over --page-size; units mm/cm/in/pt/px, mm if omitted)
    #[arg(long, value_name = "LENGTH")]
    pub page_width: Option<String>,

    /// Paper height (wins over --page-size)
    #[arg(long, value_name = "LENGTH")]
    pub page_height: Option<String>,

    /// Paper orientation (Landscape swaps the final width and height)
    #[arg(short = 'O', long, value_enum, ignore_case = true)]
    pub orientation: Option<Orientation>,

    /// Top margin (default 1in)
    #[arg(short = 'T', long, value_name = "LENGTH")]
    pub margin_top: Option<String>,

    /// Bottom margin (default 1in)
    #[arg(short = 'B', long, value_name = "LENGTH")]
    pub margin_bottom: Option<String>,

    /// Left margin (default 1in)
    #[arg(short = 'L', long, value_name = "LENGTH")]
    pub margin_left: Option<String>,

    /// Right margin (default 1in)
    #[arg(short = 'R', long, value_name = "LENGTH")]
    pub margin_right: Option<String>,

    /// Font files to use (repeatable; system fonts are used if omitted)
    #[arg(long, value_name = "PATH")]
    pub font: Vec<PathBuf>,

    /// Face index within a TrueType Collection, for the preceding --font
    #[arg(long, value_name = "N")]
    pub font_index: Vec<u32>,

    /// Font to use as `font-family: sans-serif`
    #[arg(long, value_name = "PATH")]
    pub gothic_font: Option<PathBuf>,

    /// Face index for --gothic-font
    #[arg(long, value_name = "N", requires = "gothic_font")]
    pub gothic_font_index: Option<u32>,

    /// Font to use as `font-family: serif`
    #[arg(long, value_name = "PATH")]
    pub serif_font: Option<PathBuf>,

    /// Face index for --serif-font
    #[arg(long, value_name = "N", requires = "serif_font")]
    pub serif_font_index: Option<u32>,

    /// Font to use as `font-family: monospace`
    #[arg(long, value_name = "PATH")]
    pub mono_font: Option<PathBuf>,

    /// Face index for --mono-font
    #[arg(long, value_name = "N", requires = "mono_font")]
    pub mono_font_index: Option<u32>,

    /// PDF title (the HTML <title> is used if unset)
    #[arg(long, value_name = "TEXT")]
    pub title: Option<String>,

    /// PDF author (/Author in the Info dictionary)
    #[arg(long, value_name = "TEXT")]
    pub author: Option<String>,

    /// PDF subject (/Subject in the Info dictionary)
    #[arg(long, value_name = "TEXT")]
    pub subject: Option<String>,

    /// PDF keywords (/Keywords in the Info dictionary)
    #[arg(long, value_name = "TEXT")]
    pub keywords: Option<String>,

    /// What dpi a CSS px is read as (default 96; 72 makes 1px = 1pt)
    #[arg(short = 'd', long, value_name = "DPI", default_value_t = 96.0)]
    pub dpi: f32,

    /// Scale factor (default 1.0)
    #[arg(long, value_name = "FACTOR", default_value_t = 1.0)]
    pub zoom: f32,

    /// Convert fill and stroke colours to grayscale
    #[arg(short = 'g', long, action = ArgAction::SetTrue)]
    pub grayscale: bool,

    /// Do not Flate-compress PDF objects (image data is unaffected)
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_pdf_compression: bool,

    /// Base for resolving relative references (a directory or an http(s) URL; used when reading from standard input)
    #[arg(long, value_name = "URL|DIR")]
    pub base_url: Option<String>,

    /// Do not load images (<img> and background-image)
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_images: bool,

    /// Do not paint element backgrounds (colours and images)
    #[arg(long, action = ArgAction::SetTrue)]
    pub no_background: bool,

    /// User-origin CSS files (repeatable)
    #[arg(long, value_name = "PATH")]
    pub user_style_sheet: Vec<PathBuf>,

    /// Lower bound on the computed font-size (px)
    #[arg(long, value_name = "PX")]
    pub minimum_font_size: Option<f32>,

    /// Do not create PDF annotations for external links (http(s))
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_external_links: bool,

    /// Do not create PDF annotations for internal links (#id)
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_internal_links: bool,

    /// Write relative external link URLs as-is instead of resolving them to absolute URLs
    #[arg(long, action = ArgAction::SetTrue)]
    pub keep_relative_links: bool,

    /// Text for the left of the header (placeholders such as [page] may be used)
    #[arg(long, value_name = "TEXT")]
    pub header_left: Option<String>,

    /// Text for the centre of the header
    #[arg(long, value_name = "TEXT")]
    pub header_center: Option<String>,

    /// Text for the right of the header
    #[arg(long, value_name = "TEXT")]
    pub header_right: Option<String>,

    /// Text for the left of the footer
    #[arg(long, value_name = "TEXT")]
    pub footer_left: Option<String>,

    /// Text for the centre of the footer
    #[arg(long, value_name = "TEXT")]
    pub footer_center: Option<String>,

    /// Text for the right of the footer
    #[arg(long, value_name = "TEXT")]
    pub footer_right: Option<String>,

    /// Header font name
    #[arg(long, value_name = "NAME")]
    pub header_font_name: Option<String>,

    /// Header font size (px)
    #[arg(long, value_name = "SIZE")]
    pub header_font_size: Option<f32>,

    /// Footer font name
    #[arg(long, value_name = "NAME")]
    pub footer_font_name: Option<String>,

    /// Footer font size (px)
    #[arg(long, value_name = "SIZE")]
    pub footer_font_size: Option<f32>,

    /// Draw a rule below the header
    #[arg(long, action = ArgAction::SetTrue)]
    pub header_line: bool,

    /// Draw a rule above the footer
    #[arg(long, action = ArgAction::SetTrue)]
    pub footer_line: bool,

    /// Gap between the header and the body (mm). The top margin grows by that much
    #[arg(long, value_name = "MM")]
    pub header_spacing: Option<f32>,

    /// Gap between the footer and the body (mm)
    #[arg(long, value_name = "MM")]
    pub footer_spacing: Option<f32>,

    /// Add a default header with the title and the page number
    #[arg(long, action = ArgAction::SetTrue)]
    pub default_header: bool,

    /// Replace [name] in the header/footer with a value (name=value, repeatable)
    #[arg(long, value_name = "NAME=VALUE")]
    pub replace: Vec<String>,

    /// HTML to use as a cover page (not counted in page numbers, and no header/footer)
    #[arg(long, value_name = "PATH")]
    pub cover: Option<PathBuf>,

    /// Insert a table of contents before the body
    #[arg(long, action = ArgAction::SetTrue)]
    pub toc: bool,

    /// Heading text for the table of contents
    #[arg(long, value_name = "TEXT", default_value = "Table of Contents")]
    pub toc_header_text: String,

    /// Indentation per nesting level in the table of contents (a CSS length)
    #[arg(long, value_name = "WIDTH", default_value = "1em")]
    pub toc_level_indentation: String,

    /// Font-size ratio per nesting level in the table of contents
    #[arg(long, value_name = "REAL", default_value_t = 0.8)]
    pub toc_text_size_shrink: f32,

    /// Do not draw the dotted (dashed underline) leaders in the table of contents
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_dotted_lines: bool,

    /// Do not link table-of-contents entries to their headings
    #[arg(long, action = ArgAction::SetTrue)]
    pub disable_toc_links: bool,

    /// Link headings back to the table of contents
    #[arg(long, action = ArgAction::SetTrue)]
    pub enable_toc_back_links: bool,

    /// Offset the page numbering start
    #[arg(long, value_name = "OFFSET", default_value_t = 0)]
    pub page_offset: usize,

    /// HTML composited onto the top of every page (rendered after placeholder expansion)
    #[arg(long, value_name = "PATH")]
    pub header_html: Option<PathBuf>,

    /// HTML composited onto the bottom of every page
    #[arg(long, value_name = "PATH")]
    pub footer_html: Option<PathBuf>,

    /// Character encoding of the input (BOM, then <meta charset>, then UTF-8, if unset)
    #[arg(long, value_name = "NAME")]
    pub encoding: Option<String>,

    /// What to do when fetching an image, stylesheet or font fails
    #[arg(long, value_enum, default_value_t = LoadErrorHandling::Ignore, value_name = "MODE")]
    pub load_media_error_handling: LoadErrorHandling,

    /// What to do when reading the input itself fails (always equivalent to abort)
    #[arg(long, value_enum, default_value_t = LoadErrorHandling::Abort, value_name = "MODE")]
    pub load_error_handling: LoadErrorHandling,

    /// Forbid references to local files (allowed by default)
    #[arg(long, action = ArgAction::SetTrue, conflicts_with = "enable_local_file_access")]
    pub disable_local_file_access: bool,

    /// Allow references to local files (the default; here so server mode can be explicit)
    #[arg(long, action = ArgAction::SetTrue)]
    pub enable_local_file_access: bool,

    /// Directories local references may read from (repeatable; only these subtrees become readable)
    #[arg(long, value_name = "PATH")]
    pub allow: Vec<PathBuf>,

    /// Process in streaming mode (some options and CSS are unavailable and raise an error)
    #[arg(long, action = ArgAction::SetTrue)]
    pub streaming: bool,

    /// Allow http(s) fetches for <img src>, <link rel=stylesheet href> and url() in @font-face
    #[arg(long, action = ArgAction::SetTrue)]
    pub allow_remote_assets: bool,

    /// Log verbosity
    #[arg(long, value_enum, default_value_t = LogLevel::Info)]
    pub log_level: LogLevel,

    /// Same as --log-level none
    #[arg(short, long, action = ArgAction::SetTrue)]
    pub quiet: bool,

    /// Time at which the conversion is abandoned. Not a CLI option: HTTP server mode
    /// derives it from `--timeout` and injects it (`#[arg(skip)]`).
    ///
    /// It lives here so it reaches the engine without changing the signature of
    /// `render`/`render_to_memory`. It stays `None` for the CLI and the Ruby extension.
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

/// The papers `--page-size` accepts. The same set of keywords that CSS
/// `@page { size: ... }` accepts.
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

/// What to do when a fetch fails (wkhtmltopdf compatible; there is no `skip` because there is only one input).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoadErrorHandling {
    /// Ignore the failure and carry on (default)
    Ignore,
    /// Abort on failure
    Abort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Orientation {
    #[value(name = "Portrait")]
    Portrait,
    #[value(name = "Landscape")]
    Landscape,
}

/// A font file paired with a face index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontArg {
    pub path: PathBuf,
    pub index: u32,
}

impl ConvertArgs {
    /// Whether logging is effectively on (`--quiet` means `--log-level none`).
    pub fn is_quiet(&self) -> bool {
        self.quiet || self.log_level == LogLevel::None
    }

    /// Collect the page size and margin options into [`PageSettings`].
    ///
    /// What is returned here is the initial value: a `@page` declaration in the CSS wins
    /// per property (`engine::apply_page_rule_settings_override` does the merging).
    ///
    /// `--page-width`/`--page-height` win over `--page-size`, and
    /// `--orientation Landscape` swaps width and height last.
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
            return Err("the paper width and height must be positive".to_string());
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

        // `--header-spacing`/`--footer-spacing` are the gaps between the header/footer and
        // the body, and they grow the top and bottom margins by that much.
        const MM_TO_PX: f32 = 96.0 / 25.4;
        if let Some(mm) = self.header_spacing {
            margin.top += mm * MM_TO_PX;
        }
        if let Some(mm) = self.footer_spacing {
            margin.bottom += mm * MM_TO_PX;
        }

        let settings = PageSettings { size, margin };
        if settings.content_width() <= 0.0 {
            return Err(
                "the left and right margins add up to at least the paper width".to_string(),
            );
        }
        if settings.content_height() <= 0.0 {
            return Err(
                "the top and bottom margins add up to at least the paper height".to_string(),
            );
        }
        Ok(settings)
    }

    /// Collect the PDF output options.
    ///
    /// Falling back to `<title>` when `--title` is unset is done by the engine.
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

    /// Collect the drawing options ([`ContentOptions`]).
    /// Reading the `--user-style-sheet` files also happens here.
    pub fn content_options(&self) -> Result<ContentOptions, String> {
        let mut user_stylesheets = Vec::with_capacity(self.user_style_sheet.len());
        for path in &self.user_style_sheet {
            let css = std::fs::read_to_string(path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
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

    /// Parse `--replace name=value`.
    pub fn replacements(&self) -> Result<Vec<(String, String)>, String> {
        self.replace
            .iter()
            .map(|item| {
                item.split_once('=')
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .ok_or_else(|| format!("--replace must be given as name=value: {item}"))
            })
            .collect()
    }

    /// Collect the simple header/footer options.
    pub fn simple_header_footer(&self) -> SimpleHeaderFooter {
        let mut boxes = Vec::new();
        if self.default_header {
            // The equivalent of wkhtmltopdf's `--default-header` (title and page number).
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
                // Explicit settings come after `--default-header` so they override it.
                boxes.retain(|b: &MarginBoxText| b.area != area);
                boxes.push(MarginBoxText {
                    area,
                    text: text.clone(),
                });
            }
        }

        // If HTML was given for the same side, it wins (to avoid drawing twice).
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

    /// Options for the look of the table of contents (wkhtmltopdf compatible).
    pub fn toc_options(&self) -> TocOptions {
        TocOptions {
            header_text: self.toc_header_text.clone(),
            level_indentation: self.toc_level_indentation.clone(),
            text_size_shrink: self.toc_text_size_shrink,
            dotted_lines: !self.disable_dotted_lines,
            links: !self.disable_toc_links,
        }
    }

    /// Permission settings for local file references.
    ///
    /// The `--allow` directories are resolved to real paths here. Resolving them on every
    /// reference would mean falling back to comparing raw paths whenever resolution fails,
    /// leaving `..` in the comparison. A directory that cannot be resolved is an error at
    /// startup rather than being silently ignored.
    pub fn local_access(&self) -> Result<LocalAccess, String> {
        let mut allowed_dirs = Vec::with_capacity(self.allow.len());
        for dir in &self.allow {
            let canonical = dir.canonicalize().map_err(|e| {
                format!(
                    "cannot resolve the directory given to --allow: {} ({e})",
                    dir.display()
                )
            })?;
            if !canonical.is_dir() {
                return Err(format!(
                    "--allow must be given a directory: {}",
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

    /// Processing mode (`--streaming`).
    pub fn mode(&self) -> Mode {
        if self.streaming {
            Mode::Streaming
        } else {
            Mode::Batch
        }
    }

    /// Validity of the `--dpi`/`--zoom` values (they must be positive and finite).
    pub fn validate_scaling(&self) -> Result<(), String> {
        if !(self.dpi.is_finite() && self.dpi > 0.0) {
            return Err(format!("--dpi must be positive: {}", self.dpi));
        }
        if !(self.zoom.is_finite() && self.zoom > 0.0) {
            return Err(format!("--zoom must be positive: {}", self.zoom));
        }
        Ok(())
    }

    /// Pair up `--font` and `--font-index` based on the order they appear on the
    /// command line.
    ///
    /// `--font-index` has the position-dependent meaning "applies to the preceding
    /// `--font`" (kept from the hand-written parser). clap groups values per option, so
    /// we recover the original positions with `ArgMatches::indices_of` to pair them up.
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
            // The last `--font` appearing before that `--font-index`.
            let target = font_positions.iter().rposition(|p| p < position);
            match target {
                Some(i) => specs[i].index = self.font_index[nth],
                None => return Err("--font-index must follow the --font it applies to".to_string()),
            }
        }

        Ok(specs)
    }

    /// Fonts explicitly assigned to the generic family names (`sans-serif`/`serif`/
    /// `monospace`). Generic names with no setting are omitted (system fonts resolve them).
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

    /// Whether the input is standard input.
    pub fn reads_stdin(&self) -> bool {
        self.input.as_deref() == Some(STD_STREAM)
    }

    /// The output target. With no `-o`, the input path's extension is replaced with `.pdf`.
    /// Returns `None` for standard output.
    pub fn output_path(&self) -> Result<Option<PathBuf>, String> {
        match self.output.as_deref() {
            Some(STD_STREAM) => Ok(None),
            Some(path) => Ok(Some(PathBuf::from(path))),
            None => {
                if self.reads_stdin() {
                    return Err(
                        "when reading from standard input, name the output with -o/--output (`-o -` for standard output)"
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
        // `server` requires `--font` (it cannot be changed per request).
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
        // Sides that were not given keep their defaults.
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

    /// `--allow` directories are resolved to real paths at startup.
    #[test]
    fn allow_dirs_are_resolved_to_real_paths() {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-allow-test-{}-resolved",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("assets")).unwrap();

        // Passing `<dir>/assets/..` collapses to `<dir>`.
        let dotted = dir.join("assets").join("..");
        let (cli, _) = parse(&[
            "sghtmltopdf",
            "in.html",
            "--font",
            "a.ttf",
            "--allow",
            dotted.to_str().unwrap(),
        ]);
        let access = cli
            .convert
            .local_access()
            .expect("it exists, so it resolves");
        assert_eq!(access.allowed_dirs, vec![dir.canonicalize().unwrap()]);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An `--allow` that cannot be resolved is an error rather than silently ignored
    /// (ignoring it would change the permitted set unintentionally).
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

    /// Passing a file to `--allow` is an error too.
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
        assert!(err.contains("directory"), "got: {err}");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
