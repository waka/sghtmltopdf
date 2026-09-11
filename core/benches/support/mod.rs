//! Shared harness for the benchmark suite (`core/benches/*.rs`).
//!
//! Everything that more than one bench binary needs lives here: the fixture
//! corpus manifest, the bundled font set, engine construction, the individual
//! pipeline stages, synthetic document generators for the scale series, and
//! the deterministic counters (allocations, peak RSS, page count) used by
//! `metrics.rs`.
//!
//! Fonts are always the ones bundled under `tests/fonts`, and every case turns
//! the engine's system font search off, so a run never depends on what is
//! installed on the machine.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, GenericFamily, Mode};
use sghtmltopdf_core::fonts::{load_font_faces, Font, FontCollection, SystemFonts};
use sghtmltopdf_core::html::{self, Dom, NodeId};
use sghtmltopdf_core::img::{DocumentImageCache, ImageFetcher};
use sghtmltopdf_core::layout::PageSettings;
use sghtmltopdf_core::sink::MemorySink;
use sghtmltopdf_core::style::{
    compute_styles, extract_author_stylesheet, user_agent_stylesheet, ComputedStyle, Stylesheet,
};

pub const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Size of the chunks fed to the engine, mirroring how the CLI reads a file.
pub const FEED_CHUNK: usize = 64 * 1024;

pub fn fonts_dir() -> PathBuf {
    Path::new(MANIFEST_DIR).join("tests/fonts")
}

pub fn fixtures_dir() -> PathBuf {
    Path::new(MANIFEST_DIR).join("benches/fixtures")
}

// ---------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------

/// A font from `tests/fonts`, with the face index for collections.
pub struct BundledFont {
    pub file: &'static str,
    pub index: u32,
}

/// Fonts passed as `--font`. The first one is the document default.
/// `NotoColorEmoji.ttf` is deliberately absent: it has no outlines and the
/// engine rejects it with a warning.
pub const EXPLICIT_FONTS: &[BundledFont] = &[
    BundledFont {
        file: "DejaVuSans.ttf",
        index: 0,
    },
    BundledFont {
        file: "DejaVuSans-Bold.ttf",
        index: 0,
    },
    BundledFont {
        file: "NotoSansCJK-Regular.ttc",
        index: 0,
    },
];

/// Fonts bound to the CSS generic families (`--gothic-font` etc.), so that a
/// fixture can say `font-family: monospace` without touching system fonts.
pub const GENERIC_FONTS: &[(GenericFamily, &str)] = &[
    (GenericFamily::SansSerif, "DejaVuSans.ttf"),
    (GenericFamily::Serif, "DejaVuSans.ttf"),
    (GenericFamily::Monospace, "DejaVuSansMono.ttf"),
];

fn font_spec(file: &str, index: u32) -> FontSpec {
    FontSpec {
        path: fonts_dir().join(file),
        index,
    }
}

/// Arguments equivalent to [`EXPLICIT_FONTS`] and [`GENERIC_FONTS`] for the
/// CLI and the HTTP server.
pub fn cli_font_args() -> Vec<String> {
    let mut args = vec!["--disable-system-fonts".to_string()];
    for font in EXPLICIT_FONTS {
        args.push("--font".to_string());
        args.push(fonts_dir().join(font.file).display().to_string());
        if font.index != 0 {
            args.push("--font-index".to_string());
            args.push(font.index.to_string());
        }
    }
    for (family, file) in GENERIC_FONTS {
        let flag = match family {
            GenericFamily::SansSerif => "--gothic-font",
            GenericFamily::Serif => "--serif-font",
            GenericFamily::Monospace => "--mono-font",
        };
        args.push(flag.to_string());
        args.push(fonts_dir().join(file).display().to_string());
    }
    args
}

/// The same font set as a [`FontCollection`], for benches that call the
/// pipeline stages directly instead of going through the engine.
/// `@font-face` rules from `author` are loaded the way the engine does it.
pub fn font_collection(author: &Stylesheet) -> FontCollection {
    let fonts = EXPLICIT_FONTS
        .iter()
        .map(|f| Font::load_indexed(fonts_dir().join(f.file), f.index).expect("bundled font"))
        .collect();
    let mut collection = FontCollection::new(fonts);
    for (family, file) in GENERIC_FONTS {
        let font = Font::load(fonts_dir().join(file)).expect("bundled font");
        collection.push_font_face(family.css_name().to_string(), None, None, Vec::new(), font);
    }
    if !author.font_faces.is_empty() {
        let fetcher = ImageFetcher::new(fixtures_dir(), false);
        // Same reason as `disable_system_fonts` in `engine_options`.
        let system = SystemFonts::none();
        for loaded in load_font_faces(&author.font_faces, &fetcher, &system) {
            collection.push_font_face(
                loaded.family,
                Some(loaded.weight),
                Some(loaded.style),
                loaded.unicode_range,
                loaded.font,
            );
        }
    }
    collection
}

// ---------------------------------------------------------------------------
// Fixture corpus
// ---------------------------------------------------------------------------

/// One HTML document from `benches/fixtures`.
pub struct Fixture {
    pub name: &'static str,
    pub file: &'static str,
    /// Can be rendered in `Mode::Streaming` without hitting a hard error
    /// (`counter(pages)`, `<body>` decorations, trailing `<style>`, ...).
    pub streaming: bool,
    /// Loads images, stylesheets or fonts from disk. Such fixtures cannot be
    /// posted to the HTTP server, which disables local file access.
    pub local_assets: bool,
}

impl Fixture {
    pub fn path(&self) -> PathBuf {
        fixtures_dir().join(self.file)
    }

    pub fn html(&self) -> Vec<u8> {
        std::fs::read(self.path()).unwrap_or_else(|e| panic!("read {}: {e}", self.file))
    }
}

macro_rules! fixture {
    ($name:literal, streaming: $s:literal, local_assets: $l:literal) => {
        Fixture {
            name: $name,
            file: concat!($name, ".html"),
            streaming: $s,
            local_assets: $l,
        }
    };
}

/// The corpus. One document per feature area, plus three realistic ones.
pub const FIXTURES: &[Fixture] = &[
    fixture!("text_inline", streaming: true, local_assets: false),
    fixture!("text_cjk", streaming: true, local_assets: false),
    fixture!("block_layout", streaming: true, local_assets: false),
    fixture!("flexbox", streaming: true, local_assets: false),
    fixture!("grid", streaming: true, local_assets: false),
    fixture!("table_basic", streaming: true, local_assets: false),
    fixture!("table_pagination", streaming: true, local_assets: false),
    fixture!("paged_media", streaming: true, local_assets: false),
    fixture!("paged_media_total", streaming: false, local_assets: false),
    fixture!("generated_content", streaming: true, local_assets: false),
    fixture!("style_engine", streaming: true, local_assets: false),
    fixture!("images", streaming: true, local_assets: true),
    fixture!("fonts", streaming: true, local_assets: true),
    fixture!("real_receipt", streaming: false, local_assets: true),
    fixture!("real_invoice", streaming: true, local_assets: false),
    fixture!("real_report", streaming: true, local_assets: true),
];

pub fn fixture(name: &str) -> &'static Fixture {
    FIXTURES
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no fixture named {name}"))
}

// ---------------------------------------------------------------------------
// Engine (end-to-end)
// ---------------------------------------------------------------------------

pub fn engine_options(mode: Mode) -> EngineOptions {
    EngineOptions {
        mode,
        fonts: EXPLICIT_FONTS
            .iter()
            .map(|f| font_spec(f.file, f.index))
            .collect(),
        generic_fonts: GENERIC_FONTS
            .iter()
            .map(|(family, file)| (*family, font_spec(file, 0)))
            .collect(),
        base_dir: Some(fixtures_dir()),
        // Without this the engine fills gaps in the bundled set (there is no
        // italic face, and `serif` is bound to a regular one) from whatever is
        // installed on the machine, so the same fixture embeds Times New Roman
        // here and DejaVu Serif on a Linux runner. Numbers are only comparable
        // across machines with the search switched off.
        disable_system_fonts: true,
        ..EngineOptions::default()
    }
}

/// Renders `html` to PDF bytes through the public engine API, feeding it in
/// [`FEED_CHUNK`] pieces like the CLI does.
pub fn render(html: &[u8], mode: Mode) -> Vec<u8> {
    let mut engine = Engine::new(engine_options(mode), MemorySink::new());
    for chunk in html.chunks(FEED_CHUNK) {
        engine.feed(chunk).unwrap_or_else(|e| panic!("feed: {e}"));
    }
    engine.finish().unwrap_or_else(|e| panic!("finish: {e}"))
}

// ---------------------------------------------------------------------------
// Pipeline stages (for the per-phase benches)
// ---------------------------------------------------------------------------

/// Parses the `<style>`/`<link rel=stylesheet>` sources of `dom` into one
/// author stylesheet, resolving local `<link>`s relative to the fixtures dir.
pub fn author_stylesheet(dom: &Dom) -> Stylesheet {
    let fetcher = ImageFetcher::new(fixtures_dir(), false);
    extract_author_stylesheet(dom, &fetcher, &DocumentImageCache::new())
}

pub type Styles = HashMap<NodeId, Rc<ComputedStyle>>;

/// The inputs every stage after parsing needs, computed once up front.
pub struct Prepared {
    pub dom: Dom,
    pub ua: Stylesheet,
    pub author: Stylesheet,
    pub styles: Styles,
    pub fonts: FontCollection,
    pub settings: PageSettings,
}

pub fn prepare(html: &[u8]) -> Prepared {
    let dom = html::parse(html);
    let ua = user_agent_stylesheet();
    let author = author_stylesheet(&dom);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = font_collection(&author);
    Prepared {
        dom,
        ua,
        author,
        styles,
        fonts,
        settings: PageSettings::default(),
    }
}

// ---------------------------------------------------------------------------
// Synthetic documents for the scale series
// ---------------------------------------------------------------------------

/// Element counts for the paragraph series. Big enough to expose anything
/// worse than linear, small enough to finish in seconds.
pub const SCALE_SIZES: &[usize] = &[1_000, 5_000, 20_000, 60_000];

/// A synthetic document generator for the scale series.
pub type Generator = fn(usize) -> String;

/// One document shape in the scale series.
pub struct ScaleKind {
    pub name: &'static str,
    pub generate: Generator,
    /// Element counts to run. The engine caps a document at 500,000 DOM
    /// nodes, in streaming mode too when everything sits inside one
    /// top-level element such as a table, so each shape has its own list.
    pub sizes: &'static [usize],
}

/// The document shapes in the scale series.
pub const SCALE_KINDS: &[ScaleKind] = &[
    ScaleKind {
        name: "paragraphs",
        generate: paragraphs_html,
        sizes: SCALE_SIZES,
    },
    ScaleKind {
        // 11 nodes per row (tr, 5 td, 5 text), so 40,000 rows is the
        // largest round size under the cap.
        name: "table",
        generate: table_html,
        sizes: &[1_000, 5_000, 20_000, 40_000],
    },
];

/// `count` fixed-height paragraphs. The same shape as the documentation's
/// memory table so the numbers stay comparable.
pub fn paragraphs_html(count: usize) -> String {
    let mut html = String::with_capacity(count * 56 + 128);
    html.push_str("<html><head><style>p { height: 60px; margin: 0; }</style></head><body>");
    for i in 0..count {
        let _ = write!(html, "<p>paragraph {i} lorem ipsum dolor sit amet</p>");
    }
    html.push_str("</body></html>");
    html
}

/// One table with `count` rows and a repeating header.
pub fn table_html(count: usize) -> String {
    let mut html = String::with_capacity(count * 110 + 256);
    html.push_str(
        "<html><head><style>\
         table { border-collapse: collapse; width: 100%; }\
         th, td { border: 1px solid #999; padding: 4px 6px; }\
         thead th { background: #eee; }\
         </style></head><body><table><thead><tr>\
         <th>#</th><th>Item</th><th>Qty</th><th>Unit</th><th>Total</th>\
         </tr></thead><tbody>",
    );
    for i in 0..count {
        let qty = i % 97 + 1;
        let _ = write!(
            html,
            "<tr><td>{i}</td><td>Item {i} description text</td><td>{qty}</td><td>{}</td><td>{}</td></tr>",
            qty * 120,
            qty * 360
        );
    }
    html.push_str("</tbody></table></body></html>");
    html
}

// ---------------------------------------------------------------------------
// Deterministic measurements
// ---------------------------------------------------------------------------

/// Number of pages in a PDF produced by this engine: one `/MediaBox` per
/// page object (page dictionaries are never compressed).
pub fn page_count(pdf: &[u8]) -> usize {
    pdf.windows(b"/MediaBox".len())
        .filter(|w| *w == b"/MediaBox")
        .count()
}

/// Allocation counters. Install [`CountingAlloc`] as the global allocator to
/// make them live; otherwise every value stays zero.
pub mod alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    static COUNT: AtomicUsize = AtomicUsize::new(0);
    static BYTES: AtomicUsize = AtomicUsize::new(0);
    static LIVE: AtomicUsize = AtomicUsize::new(0);
    static PEAK_LIVE: AtomicUsize = AtomicUsize::new(0);

    pub struct CountingAlloc;

    unsafe impl GlobalAlloc for CountingAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                record(layout.size());
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) };
            LIVE.fetch_sub(layout.size(), Relaxed);
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
            if !new_ptr.is_null() {
                LIVE.fetch_sub(layout.size(), Relaxed);
                record(new_size);
            }
            new_ptr
        }
    }

    fn record(size: usize) {
        COUNT.fetch_add(1, Relaxed);
        BYTES.fetch_add(size, Relaxed);
        let live = LIVE.fetch_add(size, Relaxed) + size;
        PEAK_LIVE.fetch_max(live, Relaxed);
    }

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct AllocStats {
        /// Number of allocations (including reallocations).
        pub count: usize,
        /// Bytes requested over the whole run.
        pub bytes: usize,
        /// High-water mark of live heap bytes.
        pub peak_live: usize,
    }

    /// Zeroes the cumulative counters and restarts the peak from the current
    /// live size.
    pub fn reset() {
        COUNT.store(0, Relaxed);
        BYTES.store(0, Relaxed);
        PEAK_LIVE.store(LIVE.load(Relaxed), Relaxed);
    }

    pub fn snapshot() -> AllocStats {
        AllocStats {
            count: COUNT.load(Relaxed),
            bytes: BYTES.load(Relaxed),
            peak_live: PEAK_LIVE.load(Relaxed),
        }
    }
}

/// Peak resident set size of this process (`RUSAGE_SELF`) or of its waited
/// children (`RUSAGE_CHILDREN`), in bytes.
pub fn peak_rss_bytes(children: bool) -> u64 {
    let who = if children {
        libc::RUSAGE_CHILDREN
    } else {
        libc::RUSAGE_SELF
    };
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes a fully initialised rusage on success.
    let rc = unsafe { libc::getrusage(who, usage.as_mut_ptr()) };
    if rc != 0 {
        return 0;
    }
    let usage = unsafe { usage.assume_init() };
    let maxrss = usage.ru_maxrss as u64;
    // macOS reports bytes, Linux and the BSDs report kibibytes.
    if cfg!(target_os = "macos") {
        maxrss
    } else {
        maxrss * 1024
    }
}
