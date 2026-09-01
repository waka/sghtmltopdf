//! `Engine`: the core entry point unifying everything from feeding HTML chunks to writing
//! the PDF bytes into a single API.
//!
//! It implements the coarse-grained Sink-based `new`/`feed`/`finish` API, corresponding
//! almost one to one with the Ruby FFI boundary (`Engine.new(options)`, `feed(html_chunk)`,
//! `each_pdf_chunk { |bytes| ... }`, `finish`).
//!
//! ## The pipeline differs between `Mode::Batch` and `Mode::Streaming`
//!
//! `Mode::Batch` is a thin wrapper over the batch API, processing the whole DOM at once when
//! (`compute_styles`/`build_box_tree`/`layout_document`/
//! `paginate_document_streaming`).
//!
//! `Mode::Streaming` performs genuine streaming: each time a top-level block element
//! directly under `<body>` becomes final, that subtree alone goes through style computation,
//! layout, pagination, PDF writing and DOM release.
//! The styles of `<html>`/`<body>` themselves are computed once, before the first top-level
//! element is final, and used as the starting point (the inherited source) for computing
//! each later top-level element's styles.
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

/// Selects batch or streaming processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Batch,
    Streaming,
}

/// The CSS generic family names whose concrete font can be given explicitly.
/// `cursive`/`fantasy` are not covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenericFamily {
    SansSerif,
    Serif,
    Monospace,
}

impl GenericFamily {
    /// The name as written in CSS. The font is registered in the collection under this name.
    pub fn css_name(self) -> &'static str {
        match self {
            Self::SansSerif => "sans-serif",
            Self::Serif => "serif",
            Self::Monospace => "monospace",
        }
    }
}

/// An explicit font specification, the equivalent of `--font`.
pub struct FontSpec {
    pub path: PathBuf,
    /// The face index in a file containing several faces, such as a TrueType Collection (`.ttc`).
    pub index: u32,
}

/// Options changing the behaviour of what is rendered.
///
/// The "what is drawn" counterpart of [`crate::pdf::PdfOutputOptions`], which changes only
/// how the PDF is written.
#[derive(Debug, Clone)]
pub struct ContentOptions {
    /// Whether to load `<img>` and CSS `background-image` (false with `--no-images`).
    pub load_images: bool,
    /// Whether to paint element backgrounds (colours and images) (false with `--no-background`).
    pub draw_backgrounds: bool,
    /// User-origin CSS (`--user-style-sheet`). Concatenated after the UA stylesheet
    /// (stronger than the UA, weaker than author CSS).
    pub user_stylesheets: Vec<String>,
    /// The lower bound on the computed `font-size` (`--minimum-font-size`).
    pub minimum_font_size: Option<f32>,
    /// Whether to emit annotations for external links (false with `--disable-external-links`).
    pub external_links: bool,
    /// Whether to emit annotations for internal links (`#id`) (false with `--disable-internal-links`).
    pub internal_links: bool,
    /// Whether to write a relative external link URL as-is rather than making it absolute
    /// with `<base href>` (true with `--keep-relative-links`).
    pub keep_relative_links: bool,
    /// Whether to abort when fetching an image, stylesheet or font fails
    /// (`--load-media-error-handling abort`).
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

/// The initialisation options for `Engine`.
#[derive(Default)]
pub struct EngineOptions {
    pub mode: Mode,
    pub settings: PageSettings,
    /// Explicit font specifications, the equivalent of `--font` (repeatable).
    pub fonts: Vec<FontSpec>,
    /// Give the concrete font for a CSS generic family name (`sans-serif`/`serif`/`monospace`)
    /// explicitly (the equivalent of `--gothic-font`/`--serif-font`/`--mono-font`). A generic
    /// name given here resolves to that font first, and one left unset resolves through the
    /// system font candidate list ([`crate::fonts`]). The default `font-family` (unset) falls
    /// back to the `--font` font regardless.
    pub generic_fonts: Vec<(GenericFamily, FontSpec)>,
    /// The base directory for resolving `src: url(...)` in `@font-face` relatively.
    /// Where the input corresponds to no file (a Rack body, say) it may be `None`, and the
    /// current directory is then the base. The same base directory is used for resolving
    /// local relative paths in `<img src>` too.
    pub base_dir: Option<PathBuf>,
    /// The base URL for resolving relative references (the equivalent of `--base-url`).
    /// A `<base href>` in the HTML wins (this value only supplies the default from outside).
    /// An http(s) URL is expected; to use a local directory as the base, use `base_dir`
    /// instead.
    pub base_href: Option<String>,
    /// Whether to allow http(s) absolute URL fetches for `<img src>` and
    /// `<link rel=stylesheet href>`. `false` by default (the "disabled by default, explicit
    /// opt-in" rule; this one flag governs both images and external stylesheets). Local
    /// relative paths and `data:` URIs are always allowed regardless.
    pub allow_remote_assets: bool,
    /// The PDF output options (metadata, compression, scale and grayscale).
    pub output: PdfOutputOptions,
    /// The behaviour of what is drawn ([`ContentOptions`]).
    pub content: ContentOptions,
    /// Whether local file references are allowed, and which directories are permitted
    /// (`--enable/disable-local-file-access` and `--allow`).
    /// The default is the CLI's traditional behaviour: allowed, with no directory restriction.
    pub local_access: LocalAccess,
    /// The `--header-html`/`--footer-html` templates.
    pub header_footer_html: HeaderFooterHtml,
    /// The `--cover` HTML (with placeholders already expanded).
    pub cover_html: Option<String>,
    /// The table-of-contents settings.
    pub toc: TocSettings,
    /// `--page-offset`. Shifts the starting page number of the TOC and the body.
    pub page_offset: usize,
    /// The `@page` rules composed from the CLI's simple header/footer options. They are
    /// placed before the author CSS's page rules, so an author declaration of the same margin
    /// box wins.
    pub extra_page_rules: Vec<PageRule>,
    /// The time at which the conversion is abandoned. `None` means unlimited (the CLI default).
    ///
    /// HTTP server mode supplies it from `--timeout`, to stop one request occupying a worker
    /// indefinitely.
    ///
    /// It is checked per chunk fed, per top-level element and per page written. It never
    /// looks inside a single layout call, so an overrun is noticed at worst one such interval late.
    pub deadline: Option<std::time::Instant>,
}

/// The permission settings for local file references.
#[derive(Debug, Clone)]
pub struct LocalAccess {
    pub allow: bool,
    /// If non-empty, only files under these directories may be read.
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

/// The `--header-html`/`--footer-html` templates.
///
/// The contents are the HTML text before placeholder expansion. Where it contains a page
/// number, it is expanded and laid out again per page.
#[derive(Debug, Clone, Default)]
pub struct HeaderFooterHtml {
    pub header: Option<String>,
    pub footer: Option<String>,
    /// The document-level values used to fill in the placeholders whose value changes per
    /// page (`[page]`/`[topage]`).
    pub placeholders: HeaderFooterPlaceholders,
}

/// The placeholder expansion values (transferred from the CLI layer's `PlaceholderValues`).
/// It is a plain type holding only what is needed, so the core does not depend on the CLI layer.
#[derive(Debug, Clone, Default)]
pub struct HeaderFooterPlaceholders {
    /// Rather than a function producing text with everything but `[page]`/`[topage]` already
    /// expanded, the already-expanded template is received as-is.
    /// This holds only the material needed to substitute the page numbers.
    pub page_token: String,
    pub total_pages_token: String,
}

impl HeaderFooterHtml {
    pub fn is_empty(&self) -> bool {
        self.header.is_none() && self.footer.is_none()
    }

    /// Whether it contains a page number placeholder (if not, the layout result can be reused
    /// across pages).
    pub fn depends_on_page(&self) -> bool {
        [self.header.as_deref(), self.footer.as_deref()]
            .into_iter()
            .flatten()
            .any(|html| {
                html.contains(&self.placeholders.page_token)
                    || html.contains(&self.placeholders.total_pages_token)
            })
    }

    /// Whether it uses `[topage]` (the total page count). It cannot be determined under
    /// `Mode::Streaming`, so it is an error there.
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

/// Build the `PageSettings` and clip rectangle relative to the margin area, for the header
/// (`top = true`) or the footer.
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

/// Lay one header/footer HTML out against the margin area and turn it into a [`PageOverlay`].
///
///
/// Images are not supported (no `ImageAssetCache` is passed, so an `<img>` becomes an empty
/// box). Text, borders and background colours are drawn through the same pipeline as the
/// body.
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

/// The fetcher for the header/footer HTML. It fetches no external resources
/// (only an inline `<style>` and text are covered; a known limitation).
fn overlay_fetcher() -> ImageFetcher {
    ImageFetcher::new(PathBuf::from("."), false).with_local_access(false, Vec::new())
}

/// Build the header/footer overlays to composite onto this page.
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
    // Where no page number is involved, the first layout is reused.
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

/// The function building the table-of-contents HTML from the list of headings (implemented
/// and supplied by the CLI layer, `cli::toc`).
pub type TocHtmlBuilder = Rc<dyn Fn(&[TocHeading]) -> String>;

/// The table-of-contents (`--toc`) settings.
///
/// Everything affecting its appearance is reflected in the CSS/HTML the CLI layer
/// (`cli::toc::TocOptions`) builds, so the core holds only "is it enabled" and the HTML builder function.
#[derive(Clone)]
pub struct TocSettings {
    pub enabled: bool,
    /// The function building the TOC's HTML from the list of headings. The CLI layer's implementation is passed in.
    pub build_html: TocHtmlBuilder,
    /// Whether to link headings back to the table of contents (`--enable-toc-back-links`).
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

/// One heading listed in the table of contents.
#[derive(Debug, Clone, PartialEq)]
pub struct TocHeading {
    /// `h1` = 1 ... `h6` = 6.
    pub level: u8,
    pub title: String,
    /// The 0-based page number within the body. The displayed number is
    /// `body_page + 1 + the TOC page count + page_offset`.
    pub body_page: usize,
    /// The named destination to link to.
    pub anchor: String,
}

/// Pick out the `h1` to `h6` from the body's pages and collect their page numbers and anchor names.
///
/// A heading with no `id` is given an automatic `__sgtoc_<serial>`, which is added to
/// `anchor_names`.
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
        // The children are walked with the same structure as `pdf::document::collect_link_areas`.
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
                    // A heading with no `id` is given an automatic destination name.
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

/// Lay out an independent HTML document (a cover or TOC) into a list of pages.
/// It fetches no external resources (the same constraint as the header/footer).
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

/// Rebuild the table of contents' pages until the page count converges.
///
/// It returns (the TOC's pages, the TOC document's styles). The TOC is an independent
/// document, so drawing it needs its own style map.
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
                "warning: the table of contents' page count did not converge (using the last result).\n  \
                 The page numbers in the table of contents may be off by one page"
            );
        }
    }
    result
}

/// Load the fonts named explicitly with `--font`.
fn load_explicit_fonts<E>(specs: &[FontSpec]) -> Result<Vec<Font>, EngineError<E>> {
    let mut loaded = Vec::with_capacity(specs.len());
    for spec in specs {
        let font = Font::load_indexed(&spec.path, spec.index)
            .map_err(|e| EngineError::Font(format!("failed to load the font: {e}")))?;
        // Even when named explicitly, a font with no outlines is not taken. Embedding it would
        // draw nothing while defeating subsetting and bloating the PDF.
        if !font.has_outlines() {
            warn_font_without_outlines(&spec.path.display().to_string());
            continue;
        }
        loaded.push(font);
    }
    Ok(loaded)
}

/// Where no font at all remains after `--font`, `@font-face` and system font discovery, fill
/// in the system's `sans-serif` candidate as the default font.
///
/// With no font at all there is nowhere to draw text with no `font-family` (the default
/// `font-family` being empty). Filling it in from the system fonts rather than requiring
/// `--font` gives the same feel as wkhtmltopdf (at the cost of the output depending on the
/// environment when nothing is specified).
///
/// It does nothing when `@font-face` supplied a font, because adding one here would change
/// the order of the faces.
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
            "no usable font (no system font was found).\n  \
             Specify a font file with --font"
                .to_string(),
        )),
    }
}

/// Warn when a `font-family` could not be resolved under `Mode::Streaming`.
///
/// In streaming, [`crate::pdf::StreamingPdfWriter`] fixes the font count at `new`, so a
/// system font cannot be looked up by `font-family` name and added later
/// (`load_missing_system_fonts` cannot be called).
/// Such a setting would silently be drawn in the default font, so it is warned about once.
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
                "warning: font-family \"{family}\" cannot be resolved in streaming mode\n  \
                 (the fonts have to be settled before processing begins). It will be drawn in the default font.\n  \
                 Name it explicitly with --font/--gothic-font/--serif-font/--mono-font or @font-face"
            );
        }
    }
}

/// Return the CLI-derived `@page` rules placed before the author's rules.
fn page_rules_with_cli(extra: &[PageRule], author: &[PageRule]) -> Vec<PageRule> {
    let mut rules = extra.to_vec();
    rules.extend_from_slice(author);
    rules
}

/// Concatenate the user-origin CSS after the UA stylesheet.
///
/// In the CSS cascade the user origin is "stronger than the UA, weaker than author CSS".
/// Placing it at the end of the UA sheet makes it win on source order within that origin
/// while still losing to author CSS, so this approximation gives the intended strength
/// (`!important` is unsupported, so there is no inversion to worry about either).
fn append_user_stylesheets(ua: &mut Stylesheet, user_css: &[String]) {
    for css in user_css {
        let sheet = crate::style::parse_stylesheet(css);
        ua.rules.extend(sheet.rules);
    }
}

/// The bulk post-processing after style computation (`--no-background` and `--minimum-font-size`).
fn apply_content_options(
    styles: &mut HashMap<NodeId, Rc<ComputedStyle>>,
    content: &ContentOptions,
) {
    for shared in styles.values_mut() {
        // It rewrites a shared style, so it is cloned only when it has to be.
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

/// The errors `Engine` returns. It distinguishes an error from the `Sink` (`Io`), a
/// structural error the core decides itself (`UnsupportedInStreamingMode`), and a font
/// loading error (`Font`).
#[derive(Debug)]
pub enum EngineError<E> {
    Io(E),
    UnsupportedInStreamingMode(&'static str),
    Font(String),
    /// The DOM nesting exceeded [`crate::html::MAX_ELEMENT_DEPTH`].
    ///
    /// Style computation, layout and drawing all recurse as deep as that, so without stopping
    /// here a stack overflow would take the whole process down.
    DepthLimitExceeded {
        depth: u32,
        limit: u32,
    },
    /// The number of nodes held exceeded [`crate::html::MAX_NODES`].
    ///
    /// Styles, the box tree and the layout result all pile up in proportion to the node
    /// count, so without stopping here memory would be exhausted.
    NodeLimitExceeded {
        nodes: usize,
        limit: usize,
    },
    /// Abandoned because [`EngineOptions::deadline`] passed.
    TimedOut,
    /// Fetching an image, external CSS or the like failed under `--load-media-error-handling abort`.
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
                "the HTML is nested too deeply (depth {depth}, limit {limit}).\n  \
                 Reduce the nesting, or check for a missing closing tag"
            ),
            Self::NodeLimitExceeded { nodes, limit } => write!(
                f,
                "the HTML has too many elements ({nodes} nodes, limit {limit}).\n  \
                 Split the document, or process it incrementally with --streaming"
            ),
            Self::TimedOut => write!(f, "the conversion exceeded the time limit"),
            Self::MediaLoad(msg) => write!(f, "failed to fetch a resource: {msg}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for EngineError<E> {}

/// Check the depth and node count limits together.
///
/// It is a free function so it can be called both from `Engine`'s methods and from part-way
/// through `finish_batch`, which has no `self`.
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

/// Return [`EngineError::TimedOut`] if `deadline` has passed.
fn check_deadline<E>(deadline: Option<std::time::Instant>) -> Result<(), EngineError<E>> {
    match deadline {
        Some(deadline) if std::time::Instant::now() >= deadline => Err(EngineError::TimedOut),
        _ => Ok(()),
    }
}

/// The state needed to process top-level elements under `Mode::Streaming`, settled once when
/// `<head>` closes (that is, when `<body>` is detected).
struct StreamingState<S: Sink> {
    ua: Stylesheet,
    author: Stylesheet,
    fonts: FontCollection,
    /// The persistent map accumulating the styles of every top-level element processed.
    /// One page can hold boxes from several top-level elements, so
    /// `StreamingPdfWriter::write_page` needs all of it.
    styles: HashMap<NodeId, Rc<ComputedStyle>>,
    /// The side map letting the decoded image of an element with a `background-image` be
    /// looked up by `NodeId`. Like `styles`, it accumulates the top-level elements processed
    /// so far.
    background_images: HashMap<NodeId, Rc<PreparedImage>>,
    root_font_size: f32,
    /// The CSS counter state. It depends on document order, so it has to persist across
    /// top-level elements and is held here alongside `root_font_size`.
    counters: HashMap<String, Vec<i32>>,
    /// The nesting depth of `quotes` (a single counter unrelated to the tree structure).
    quote_depth: i32,
    /// The computed style of the `<body>` element itself. Used as the parent style when
    /// computing each top-level element's styles.
    body_style: ComputedStyle,
    /// The containing width for a top-level element, reflecting `<body>`'s `padding`,
    /// `border` and `margin`.
    content_width: f32,
    /// `<body>`'s `margin-left` + `border-left` + `padding-left`.
    start_x: f32,
    /// The starting Y coordinate of the next top-level element (the accumulated height so far).
    cursor_y: f32,
    /// The page geometry (used to compute the overlay areas).
    page_settings: PageSettings,
    /// The layout result of a header/footer HTML that does not depend on the page number.
    overlay_cache: Option<Vec<PageOverlay>>,
    /// The `font-family` names already warned about as unresolvable (so the same warning is
    /// not repeated).
    warned_font_families: Vec<String>,
    /// The characters already warned about as undrawable by any font. Streaming decides this
    /// per top-level element, so the characters already warned about are carried along to
    /// prevent duplicates.
    warned_uncovered_chars: HashSet<char>,
    /// Whether inline `<svg>` has already been warned about (emitted once per document).
    warned_inline_svg: bool,
    /// Whether a processed top-level element may be freed along with its subtree.
    ///
    /// In a document using selectors that need the preceding sibling, such as `+`/`~` or
    /// `:first-child`, the freeing is limited to the descendants and the element itself is
    /// kept. Without it, a later element would see itself as "the first child".
    release_whole_subtree: bool,
    paginator: StreamingPaginator,
    writer: StreamingPdfWriter<S>,
    /// The cache memoising `<img>` fetch and decode results within the document.
    image_cache: ImageAssetCache,
}

/// Free a processed top-level element.
///
/// With `whole` as `false`, only the descendants are freed and the element itself is kept.
/// A kept element retains its tag name, classes and id, so a later sibling still sees it as "the preceding sibling".
fn release_processed(mut dom: std::cell::RefMut<'_, Dom>, node: NodeId, whole: bool) {
    if whole {
        dom.release_subtree(node);
    } else {
        dom.release_descendants(node);
    }
}

/// Register `--gothic-font` as the concrete font for `font-family: sans-serif`.
/// It is added with `push_font_face` under the declared family name `"sans-serif"`, so
/// `select_for_char`'s ordinary family matching picks it up as-is.
/// `has_matching_face("sans-serif", ...)` then returns true, so the later
/// `load_missing_system_fonts` skips the system gothic search. This registers a font named
/// explicitly for a CSS generic family so it can be looked up by that generic name.
fn register_generic_fonts<E>(
    fonts: &mut FontCollection,
    generic_fonts: &[(GenericFamily, FontSpec)],
) -> Result<(), EngineError<E>> {
    for (family, spec) in generic_fonts {
        let font = Font::load_indexed(&spec.path, spec.index).map_err(|e| {
            EngineError::Font(format!(
                "failed to load the font for {}: {e}",
                family.css_name()
            ))
        })?;
        if !font.has_outlines() {
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
    /// Under `Mode::Batch` it is kept until `finish`. Under `Mode::Streaming` it becomes
    /// `None`, having been moved into `StreamingState::writer` just before the first
    /// top-level element is processed.
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

    /// Check that the nesting of what has been parsed stays within the limit.
    ///
    /// It is always passed before anything walks the DOM recursively. It is called on every
    /// `feed`, but costs nothing: the depth is just a read of a value already updated while
    /// the tree was built.
    ///
    /// It does not descend into layout, so it can only sit at convenient boundaries. It is
    /// called per chunk fed, per top-level element and per page written.
    fn check_deadline(&self) -> Result<(), EngineError<S::Error>> {
        check_deadline(self.options.deadline)
    }

    fn ensure_depth_within_limit(&self) -> Result<(), EngineError<S::Error>> {
        let dom = self.parser.dom();
        check_document_limits(dom.max_depth(), dom.node_count())
    }

    /// Feed one chunk of HTML bytes. May be called any number of times.
    ///
    /// Under `Mode::Streaming` it returns an error if a `<style>` tag after `<body>` is
    /// detected once fed (see the module docs). `Mode::Batch` does no such check, merely
    /// accumulating the DOM and doing no real work until `finish`. Under `Mode::Streaming`,
    /// the top-level elements directly under `<body>` that have become final are processed
    /// here.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), EngineError<S::Error>> {
        self.parser.feed(chunk);
        // Check the accumulated depth before anything walks the DOM (including the
        // `find_base_href` below, which recurses). Parsing itself is arena-based and safe at any depth.
        self.ensure_depth_within_limit()?;
        self.check_deadline()?;
        if self.options.mode == Mode::Streaming && self.parser.has_late_css_source() {
            return Err(EngineError::UnsupportedInStreamingMode(
                "a <style>/<link rel=stylesheet> after <body> cannot be used in streaming mode\n  \
                 (it cannot be applied retroactively to pages already written).\n  \
                 To use them, drop --streaming",
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

    /// Create the `StreamingState` if `<body>` has been detected and one has not been created
    /// yet. `sink` is moved into `StreamingState::writer` here (`self.sink` is `None` from
    /// then on).
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
            .expect("the sink is taken exactly once, when the streaming state is initialised");
        let state = self.init_streaming_state(body, sink)?;
        self.streaming = Some(state);
        Ok(())
    }

    /// The initialisation done once when `<head>` closes (that is, when `<body>` is
    /// detected): resolving the fonts, computing the `<html>`/`<body>` styles, checking
    /// `<body>`'s decoration, and building the `StreamingPdfWriter`.
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
        // The fetcher and cache for retrieving external stylesheets (`<link>`). It is a
        // separate instance from the image `ImageAssetCache` (`image_cache` below).
        // `<base href>` appears in `<head>`, so it is already parsed by this point (when the
        // first top-level element becomes final).
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
                "counter(pages) in an @page margin box cannot be used in streaming mode\n  \
                 (the total page count cannot be known in a single pass).\n  \
                 To use it, drop --streaming",
            ));
        }
        // `[topage]` in `--header-html`/`--footer-html` is unusable for the same reason.
        if self.options.header_footer_html.uses_total_pages() {
            return Err(EngineError::UnsupportedInStreamingMode(
                "[topage] in --header-html/--footer-html cannot be used in streaming mode\n  \
                 (the total page count cannot be known in a single pass).\n  \
                 To use it, drop --streaming",
            ));
        }
        // A table of contents cannot be built until the whole body has been paginated.
        if self.options.toc.enabled {
            return Err(EngineError::UnsupportedInStreamingMode(
                "--toc cannot be used in streaming mode\n  \
                 (a table of contents needs the body's page numbers).\n  \
                 To use it, drop --streaming",
            ));
        }
        // A backward-referencing selector always fails to match. It is not an error, but the
        // result changing silently is worth avoiding, so it is warned about.
        let unsafe_selectors = streaming_unsafe_selectors(&author);
        if !unsafe_selectors.is_empty() {
            eprintln!(
                "warning: {} gives a different result in streaming mode\n  \
                 (an element directly under <body> becomes final before its siblings either side are known).\n  \
                 To use them, drop --streaming",
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
        // `load_missing_system_fonts` and `load_fonts_for_uncovered_chars` need the whole
        // document's styles (and characters), which genuine streaming never holds at once, so
        // they are not called here.
        // Instead, where no font has been supplied at all, a font covering CJK is added up
        // front alongside the default (latin) one.
        // Nothing is added unasked when `--font`/`@font-face` has supplied a font, to avoid
        // affecting the order of the faces (`unicode-range` being first-wins) and the
        // principle that "the font passed with `--font` is the default".
        let had_no_fonts = fonts.is_empty();
        ensure_default_font(&mut fonts, &system_fonts)?;
        if had_no_fonts {
            ensure_cjk_fallback_font(&mut fonts, &system_fonts);
        }

        // The CSS counters and quote depth are state depending on document order, so the same
        // state is carried consistently from <html> through each top-level element directly
        // under <body> (`StreamingState` persists it from then on).
        let mut counters = HashMap::new();
        let mut quote_depth = 0;
        let (html_style, body_style, root_font_size) = {
            let dom = self.parser.dom();
            let html_id = dom
                .parent(body)
                .expect("<body> should have a parent element (<html>)");
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
                "a <body> with a background colour or borders cannot be used in streaming mode\n  \
                 (decoration spanning several pages cannot be reproduced).\n  \
                 To use them, drop --streaming",
            ));
        }

        // The candidate destinations for `<a href="#id">`. Under `Mode::Streaming` only what
        // has been parsed by this point (when the first top-level element became final) is
        // visible, but a destination is recorded from "the box found when that page is
        // written", so elements parsed later are covered too (what is collected here is not a
        // list of `id`s but the mapping of which node has which name).

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

        // With no `--title`, the `<title>` becomes the PDF's `/Title`.
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
        // `<text>` inside an SVG is drawn with the document's fonts. `fonts` is complete by
        // this point (and never changes afterwards), so it can be built here.
        .with_svg_fonts(SvgFontDb::from_collection(&fonts));

        // In a document not using selectors that need the preceding sibling, the whole subtree
        // is freed as before (keeping the element would accumulate one node per top-level
        // element, so it is not kept unless needed).
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

    /// Take one settled top-level element (a child directly under `<body>`) all the way
    /// through style computation, layout, pagination, PDF writing and DOM release.
    fn process_top_level_element(&mut self, node: NodeId) -> Result<(), EngineError<S::Error>> {
        // Checked before entering the layout and writing for this one element.
        self.check_deadline()?;
        let Engine {
            parser,
            streaming,
            options,
            ..
        } = self;
        let options_content = &options.content;
        let state = streaming.as_mut().expect(
            "process_top_level_element is only called after the streaming state is initialised",
        );

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
            // Streaming cannot top up fonts from the characters, so an undrawable character
            // is warned about each time one appears.
            warn_uncovered_chars(
                &state.fonts,
                &dom,
                &sub_styles,
                &mut state.warned_uncovered_chars,
            );
            // Only the inside of this top-level element is inspected (scanning the whole
            // document each time would be quadratic in the element count).
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
            // An element generating no box, through `display: none` and the like.
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

        // Layout is already complete and this DOM subtree (its text content, attributes and
        // so on) is never read again, so it can be freed immediately without waiting for the
        // page to flush (the `ComputedStyle`s are held separately in `state.styles`).

        release_processed(parser.dom_mut(), node, state.release_whole_subtree);

        // Where this top-level element itself carries no decoration (a background, borders or
        // a background-image; `has_visible_decoration` covers background-image too),
        // `place_split` generates no decoration fragment and this node never appears in
        // `page.boxes`. That is, `node`'s own `ComputedStyle` and background image are never
        // referenced again by `write_page`, so they can be removed right here (with
        // decoration, they are removed via `collect_completed_subtree_roots` below when the
        // page the decoration fragment really landed on is flushed).

        if !laid_out.has_visible_decoration {
            state.styles.remove(&node);
            state.background_images.remove(&node);
        }

        let mut laid_out = laid_out;
        let pages = state.paginator.push_item(&mut laid_out);
        for page in &pages {
            if !options.header_footer_html.is_empty() {
                let page_number = state.writer.page_count() + 1;
                // Under `Mode::Streaming` the total page count is unknown, so `[topage]` comes out empty.

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
                // `Mode::Streaming` cannot know the total page count in principle, so it is
                // always `None` (a use of `counter(pages)` has already been rejected in
                // `init_streaming_state`).
                .write_page(
                    page,
                    &state.styles,
                    &state.background_images,
                    &state.fonts,
                    None,
                )
                .map_err(EngineError::Io)?;
        }

        // Free the `ComputedStyle`s and background images of the descendant nodes really
        // placed on each page and not split further (`FragmentPosition::Whole`/`Last`).
        // The DOM itself is already tombstoned above, but the tree links survive, so it can
        // still be walked with `Dom::children`.
        let dom = parser.dom();
        for page in &pages {
            for root in collect_completed_subtree_roots(page) {
                remove_subtree_styles(&dom, root, &mut state.styles, &mut state.background_images);
            }
        }
        drop(dom);

        Ok(())
    }

    /// Do all the remaining work and write to `sink`.
    ///
    /// Under `Mode::Batch` everything is processed at once, after the DOM is final. Under
    /// `Mode::Streaming`, every top-level element not yet processed (including the last one,
    /// which was being held back) is processed, and then `StreamingPdfWriter::finish` writes
    /// the font embedding, xref and trailer.
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
                // `<body>` never appeared (an empty document, invalid input and so on).
                // It is treated as an empty sink (not a zero-page PDF, but finished with
                // nothing written).
                let sink = self
                    .sink
                    .take()
                    .expect("with streaming uninitialised, the sink should still be held");
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
        // `parser.finish()` can add nodes while closing unclosed tags, so it is checked here
        // as well as on `feed` (the recursive DOM walk starts right after this).
        check_deadline(options.deadline)?;
        check_document_limits(dom.max_depth(), dom.node_count())?;
        let sink = sink.expect("under Mode::Batch the sink is held unchanged until finish");

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
        // The candidate destinations for `<a href="#id">`.
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
        // Top up from character coverage the characters a family name gives no clue about
        // (Japanese with no `font-family`, say). It need not come before `ensure_default_font`,
        // but getting the document's own fonts in place before adding the default keeps the
        // order of the faces easier to read.
        load_fonts_for_uncovered_chars(&mut fonts, &dom, &styles, &system_fonts);
        ensure_default_font(&mut fonts, &system_fonts)?;
        // Warn if any character remains undrawable even after topping up.
        warn_uncovered_chars(&fonts, &dom, &styles, &mut HashSet::new());
        // Inline `<svg>` is not drawn. `<img src="*.svg">` can be, so it is confusing for one
        // to vanish silently.
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
        // `<text>` inside an SVG is drawn with the document's fonts. Topping the fonts up
        // (`load_missing_system_fonts` and friends) is already done before this point.
        .with_svg_fonts(SvgFontDb::from_collection(&fonts));
        // A `background-image` is draw-time-only information that does not affect layout
        // sizing, so it can be built once from the whole document's `styles`, independently
        // of `resolve_images` (box tree construction).
        let background_images = if options.content.load_images {
            resolve_background_images(&styles, &image_cache)
        } else {
            HashMap::new()
        };

        // `Mode::Batch` settles every page, overlays the absolute positioning and then writes
        // them in order. Duplicating `fixed` onto every page and resolving an `absolute`'s
        // ancestor page both require every page to be settled, so this is used rather than
        // `paginate_document_streaming` (which frees incrementally).
        check_deadline(options.deadline)?;

        // For the cover and TOC, the body's pages are settled before the writer is created,
        // because the anchor names assigned automatically to headings have to go into `LinkSettings`.
        let pages = paginate_document_with_absolutes(
            &mut dom,
            &styles,
            &fonts,
            &page_settings,
            &image_cache,
        );

        // Collecting the headings for the table of contents. A heading with no `id` is given
        // an automatic destination name, which is added to `anchor_names`.
        let headings = if options.toc.enabled {
            collect_headings(&dom, &pages, &mut anchor_names)
        } else {
            Vec::new()
        };

        // The cover page is assembled first, as an independent document.
        let cover_pages = match &options.cover_html {
            Some(html) => render_standalone_document(html, &fonts, &page_settings),
            None => Vec::new(),
        };

        // The table of contents shifts the body's page numbers by its own page count, so it
        // is rebuilt up to three times until the page count converges.
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

        // The total page count for `counter(pages)` is "TOC + body", excluding the cover.
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

        // The write order is cover, then TOC, then body. Page numbers do not count the cover
        // and start from `1 + --page-offset` at the TOC.
        let empty_styles: HashMap<NodeId, Rc<ComputedStyle>> = HashMap::new();
        let empty_images: HashMap<NodeId, Rc<PreparedImage>> = HashMap::new();

        for page in &cover_pages {
            // A page with no number: no margin boxes and no header/footer.
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

/// Return the `PageSettings` with the `size`/`margin` declarations of the `@page` rules
/// (unconditional `@page{}` rules only) applied to `base` (the CLI options or the defaults).
/// `:first`/`:left`/`:right` are used only to vary the margin boxes (the header/footer
/// content), so only the unconditional rules matter here (the `size_px`/`margin_*` that
/// `resolve_page_rules` returns are unaffected by either value of `is_first`/`is_left`).
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

/// Remove from `styles` the `ComputedStyle`s of the nodes in the subtree under `root`.
/// `dom` may already have had everything under `root` freed by [`Dom::release_subtree`]
/// (tombstoned), since the tree links themselves survive.
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

    /// A helper letting the expected `/MediaBox` be written in CSS px.
    fn media_box(width_px: f32, height_px: f32) -> String {
        format!(
            "/MediaBox [0 0 {} {}]",
            width_px * crate::pdf::DEFAULT_SCALE,
            height_px * crate::pdf::DEFAULT_SCALE
        )
    }

    /// Return every `stream` to `endstream` region in the PDF bytes, inflated and
    /// concatenated. Each stream's `/Length N` is parsed and exactly `N` bytes are taken from
    /// just after `stream\n` (the identically named helper in `core/src/pdf/document.rs`
    /// naively searches for the string `\nendstream`, so a chance occurrence of those bytes
    /// inside an embedded font binary cuts in the wrong place and loses the streams that
    /// follow. That really was observed making `sanity check: batched output should draw
    /// strokes` fail spuriously, hence the exact `/Length`-based implementation here).
    fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
        fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
            haystack.windows(needle.len()).position(|w| w == needle)
        }

        let mut out = Vec::new();
        let mut i = 0;
        // The trailing whitespace distinguishes it from `/Length1` (the font's original size).
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
        // 200 undecorated <p>s. Holding every element's `ComputedStyle` until `finish` would
        // leave 200 elements' worth (over 400 entries) in `styles`. Freeing them as each page
        // is flushed keeps it to roughly the most recent unflushed page's worth (a few dozen entries).

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
        // The case of a single top-level element (a wrapper with a background colour and
        // borders) spanning several pages. `process_top_level_element` is called only once, so
        // several pages are flushed within a single `push_item` call. Whether the `styles`
        // release logic (`collect_completed_subtree_roots`) wrongly removed the wrapper's own
        // `ComputedStyle` while it was still needed may not be detectable from the page count
        // alone, because `render_box` silently falls back to `ComputedStyle::default()` when
        // `styles.get` fails (`core/src/pdf/document.rs`). So the output bytes themselves are
        // compared against the batch API.

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
        // The drawing content (the number of `closepath` plus `fill` used for border drawing)
        // should match too. If the wrapper's `ComputedStyle` were lost from `styles` too
        // early, the decoration (border) drawing commands would be missing and this count
        // would change. The content stream is `/FlateDecode` compressed, so searching the
        // compressed `bytes` as a string is meaningless and it has to be inflated first (as
        // `solid_border_fills_a_mitered_quad_per_side` shows, a single-colour border is drawn
        // as a per-edge fill path rather than a stroke, hence counting `h\nf\n`).

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

        // Through the existing batch API.
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

        // Through the Engine (Mode::Batch).
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
        // A margin box's content goes through its own path (collect_margin_box_usage) rather
        // than the ordinary BoxContent::Inline one (collect_usage), so this is a regression
        // check that it has no collection gap of its own (the same class of bug as the missed
        // list marker glyphs). A digit that never appears in the body is displayed as the page
        // number in `@bottom-right`, and that glyph is confirmed to be embedded in the ToUnicode CMap.
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
        // `counter(page)` on its own (without `counter(pages)`) has a value once a page is
        // settled, so it should work fine in streaming too.
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
        // Give `@page` an explicit `size`/`margin` to make the page count deterministic: with
        // a page content height of 300px (margin 0), two 300px-tall divs should split into
        // exactly two pages.
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

    /// Build deeply nested HTML.
    fn deeply_nested_html(depth: usize) -> String {
        format!(
            "<html><body>{}x{}</body></html>",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        )
    }

    /// Nesting past the limit must be rejected with an error before anything walks the DOM
    /// recursively. Without that, style computation, layout, drawing or the recursive Drop of
    /// `LayoutBox` would overflow the stack and abort the whole process.
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
                assert!(
                    depth > limit,
                    "a depth of {depth} should exceed the limit of {limit}"
                );
            }
            other => panic!("expected DepthLimitExceeded, got {other:?}"),
        }
    }

    /// It must be rejected in streaming mode too (there the subtree processing starts
    /// part-way through `feed`, so it has to stop without waiting for `finish`).
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
            "exceeding the depth should be rejected in streaming too: {result:?}"
        );
    }

    /// Input exceeding the node count limit must be rejected.
    ///
    /// Styles, the box tree and the layout result pile up in proportion to the node count, so
    /// without stopping here memory would be exhausted (measured at worst 1210B per node).
    #[test]
    fn html_with_too_many_nodes_is_rejected() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        // `<p>a</p>` is 2 nodes: the element plus the text.
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
                    "a node count of {nodes} should exceed the limit of {limit}"
                );
            }
            other => panic!("expected NodeLimitExceeded, got {other:?}"),
        }
    }

    /// A document within the limits must still pass as before.
    #[test]
    fn html_within_the_node_limit_still_renders() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        let body = "<p>a</p>".repeat(1000);
        let html = format!("<html><body>{body}</body></html>");

        engine
            .feed(html.as_bytes())
            .expect("within the limit, so it passes");
        let pdf = engine
            .finish()
            .expect("within the limit, so it can be written");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// In streaming mode a freed node must count back towards the limit.
    ///
    /// As long as it frees as it goes, a document can be converted even where the total node
    /// count exceeds the limit (so the limit does not negate streaming's low-memory benefit).
    ///
    /// Freeing happens when a top-level element is processed, so as in the CLI the input has
    /// to be fed in chunks to check this. Feeding it all at once would pile up the DOM before
    /// any freeing ran, and really would use the memory.
    #[test]
    fn released_nodes_do_not_count_towards_the_node_limit() {
        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        // The total node count is over twice the limit, but incremental freeing keeps it clear.
        let body = "<p>a</p>".repeat(crate::html::MAX_NODES);
        let html = format!("<html><body>{body}</body></html>");

        // The same 64KiB steps as `cli::convert`'s FEED_CHUNK.
        for chunk in html.as_bytes().chunks(64 * 1024) {
            engine
                .feed(chunk)
                .expect("freeing works, so the limit is not hit");
        }
        let pdf = engine.finish().expect("streaming can write it out");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// A conversion past its deadline must be abandoned (batch).
    #[test]
    fn a_deadline_that_has_already_passed_stops_the_conversion() {
        // `check_deadline` compares with `>=`, so using the current time as the deadline
        // guarantees it has passed by the time it is checked.
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
            "an expired deadline should return TimedOut: {result:?}"
        );
    }

    /// It must be abandoned the same way in streaming mode.
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
            "an expired deadline returns TimedOut in streaming too: {result:?}"
        );
    }

    /// With the deadline in the future it must run to the end as before.
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
            .expect("within the deadline, so it passes");
        let pdf = engine
            .finish()
            .expect("within the deadline, so it can be written");
        assert!(pdf.starts_with(b"%PDF-"));
    }

    /// With no deadline given it is unlimited (the CLI default).
    #[test]
    fn no_deadline_means_no_limit() {
        let options = EngineOptions {
            fonts: vec![font_spec()],
            ..EngineOptions::default()
        };
        assert!(options.deadline.is_none());
    }

    /// Just inside the limit must pass (checking that the limit does not catch practical documents).
    ///
    /// A test thread's default stack is 2MiB, and at about 11KiB per level in a debug build
    /// that is not enough for the limit's worth of recursion. As with the CLI and the server,
    /// it is run after allocating with [`crate::render_stack::with_render_stack`]
    /// (which also confirms that the limit and the stack only mean anything as a pair).
    #[test]
    fn html_just_within_the_depth_limit_still_renders() {
        let pdf = crate::render_stack::with_render_stack(|| {
            let options = EngineOptions {
                fonts: vec![font_spec()],
                ..EngineOptions::default()
            };
            let mut engine = Engine::new(options, MemorySink::new());
            // A little headroom for the few levels of <html>/<body>.
            let html = deeply_nested_html(crate::html::MAX_ELEMENT_DEPTH as usize - 10);

            engine
                .feed(html.as_bytes())
                .expect("within the limit, so it should pass");
            engine
                .finish()
                .expect("within the limit, so it should be writable")
        });
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn engine_resolves_at_font_face_relative_to_base_dir() {
        // Check the same `@font-face` plus base_dir resolution scenario as the existing CLI
        // E2E test (cli.rs), through the Engine.
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
        // The first `@font-face` (index 0) is DejaVu Sans but declares
        // `unicode-range: U+0-7F` (Basic Latin only). 'e-acute' (U+00E9) is a glyph DejaVu
        // Sans really can draw, but it is outside the declared range and should be excluded
        // by the hard filter. The second `@font-face` (index 1) registers the same DejaVu
        // Sans again with no range, and should be the one chosen. A regression check through
        // the real CSS parse -> `Engine` -> `FontCollection` pipeline.

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
        // The classic "an alphanumeric font and a CJK font used together under one family name, split by unicode-range" pattern.
        // It also confirms that `Mode::Batch` and `Mode::Streaming` give the same result.
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
        // Check the whole image embedding pipeline (DOM attribute extraction, data: URI
        // classification, decoding, box tree, layout, PDF XObject writing) through a data: URI,
        // which involves no fetching at all.
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
        // A JPEG is embedded as-is with the DCTDecode filter rather than decoded, so the raw
        // JPEG bytes themselves should appear.
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
        // The intrinsic size (16x16, the fixture's real dimensions) should be used as-is,
        // with no width/height attributes.
        assert!(count_occurrences(&bytes, b"/Width 16") > 0);
        assert!(count_occurrences(&bytes, b"/Height 16") > 0);
    }

    /// An E2E test for `object-fit`/`object-position`.
    /// The geometry of `object_fit_rect` itself is covered by the unit tests in
    /// `pdf/document.rs`, so this narrows to confirming the real pipeline (data: URI
    /// decoding, box tree, layout, PDF encoding) connects up and emits the clip.
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
        // It always clips to the content box whatever the `object-fit` value (`Fill` fits
        // exactly to begin with, but takes the same path as a no-op). This confirms the clip
        // path construction (`re` then `W n`) really is emitted.
        let bytes = build_object_fit_pdf("");
        let decompressed = decompressed_stream_bytes(&bytes);
        assert_eq!(count_occurrences(&decompressed, b" re\n"), 1);
        assert!(count_occurrences(&decompressed, b"W\n") > 0);
    }

    #[test]
    fn object_fit_cover_and_fill_produce_different_geometry_end_to_end() {
        // Drawing an intrinsic 32x24 into a 150x80 box, `fill` (stretching non-uniformly) and
        // `cover` (scaling with the aspect ratio preserved and clipping the overflow) should
        // give different transformation matrices (`cm`) for the drawn image, so the content
        // streams as a whole should not match byte for byte either.
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
        // Check the whole pipeline (parsing, the cascade, `resolve_background_images` and PDF
        // XObject writing). The `<div>` has neither a `background-color` nor borders.

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
        // A failed fetch or decode leaves only that element's background image empty and does
        // not stop the whole document.
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
        // A failed fetch or decode leaves only that element empty and does not stop the whole
        // document (so a broken URL is not a DoS vector).
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
        // Check the whole external stylesheet pipeline (<link> detection, fetch, parse,
        // cascade) by whether it really shows up in the PDF content stream as a difference in
        // font-size.
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
        // Check the whole @import pipeline (fetching the <link>, detecting and recursively
        // fetching the @import, expanding, parsing, cascading) by whether it really shows up
        // in the PDF content stream as a difference in font-size.
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
        // A link other than rel="stylesheet" (a favicon, say) should be outside streaming
        // mode's restriction even when it appears after <body>.
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
        // A failed fetch of an external stylesheet ignores only that stylesheet and does not
        // stop the whole document (the same policy as images).
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
