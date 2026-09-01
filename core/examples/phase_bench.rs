//! Measure the time each stage of the pipeline takes (parsing, styles, box tree, layout,
//! pagination, PDF encoding) and print the breakdown.
//!
//! Run with: `cargo run --release --example phase_bench [element count]`
//!
//! Where `mem_bench` gives the overall numbers, this shows which stage is expensive.

use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::path::Path;
use std::time::Instant;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{build_box_tree, layout_document, paginate_document, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

/// An allocator that merely counts allocations, to see which size class dominates the memory.
struct CountingAlloc;

static LIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The bytes of currently live allocations per size class (a power of two).
static BUCKETS: [std::sync::atomic::AtomicUsize; 24] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 24];
/// The per-size-class breakdown at the moment a peak was set (to see which structure creates the peak).
static PEAK_BUCKETS: [std::sync::atomic::AtomicUsize; 24] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 24];
/// The count of currently live allocations per size class.
static COUNTS: [std::sync::atomic::AtomicUsize; 24] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 24];
/// The counts at the peak.
static PEAK_COUNTS: [std::sync::atomic::AtomicUsize; 24] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; 24];
/// The live-bytes threshold at which the next breakdown is recorded.
static NEXT_SNAPSHOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn bucket_of(size: usize) -> usize {
    ((usize::BITS - size.leading_zeros()) as usize).min(23)
}

unsafe impl std::alloc::GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        let ptr = unsafe { std::alloc::System.alloc(layout) };
        if !ptr.is_null() {
            let now =
                LIVE.fetch_add(layout.size(), std::sync::atomic::Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, std::sync::atomic::Ordering::Relaxed);
            BUCKETS[bucket_of(layout.size())]
                .fetch_add(layout.size(), std::sync::atomic::Ordering::Relaxed);
            COUNTS[bucket_of(layout.size())].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // Record the per-size-class breakdown every time the live total grows by 8MB.
            if now > NEXT_SNAPSHOT.load(std::sync::atomic::Ordering::Relaxed) {
                NEXT_SNAPSHOT.store(now + 8 * 1024 * 1024, std::sync::atomic::Ordering::Relaxed);
                for (dst, src) in PEAK_BUCKETS.iter().zip(BUCKETS.iter()) {
                    dst.store(
                        src.load(std::sync::atomic::Ordering::Relaxed),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
                for (dst, src) in PEAK_COUNTS.iter().zip(COUNTS.iter()) {
                    dst.store(
                        src.load(std::sync::atomic::Ordering::Relaxed),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        LIVE.fetch_sub(layout.size(), std::sync::atomic::Ordering::Relaxed);
        BUCKETS[bucket_of(layout.size())]
            .fetch_sub(layout.size(), std::sync::atomic::Ordering::Relaxed);
        COUNTS[bucket_of(layout.size())].fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        unsafe { std::alloc::System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Reset the peak to the live total at this point (to see the peak of each stage).
fn reset_peak() {
    use std::sync::atomic::Ordering::Relaxed;
    let live = LIVE.load(Relaxed);
    PEAK.store(live, Relaxed);
    NEXT_SNAPSHOT.store(live, Relaxed);
    for b in PEAK_BUCKETS.iter() {
        b.store(0, Relaxed);
    }
}

/// Print the live allocations per size class.
fn dump_live_allocations(label: &str) {
    use std::sync::atomic::Ordering::Relaxed;
    let live = LIVE.load(Relaxed) as f64 / 1024.0 / 1024.0;
    println!(
        "{label}: live {live:.0}MB (peak {:.0}MB)",
        PEAK.load(Relaxed) as f64 / 1024.0 / 1024.0
    );
    println!("  breakdown at the peak:");
    for (i, b) in PEAK_BUCKETS.iter().enumerate() {
        let mb = b.load(Relaxed) as f64 / 1024.0 / 1024.0;
        let n = PEAK_COUNTS[i].load(Relaxed);
        if mb >= 5.0 {
            println!(
                "    ~{:>8}B  {:>6.0}MB  {:>9} allocs  avg {:>6.0}B",
                1usize << i,
                mb,
                n,
                if n > 0 {
                    b.load(Relaxed) as f64 / n as f64
                } else {
                    0.0
                }
            );
        }
    }
}

fn main() {
    {
        use sghtmltopdf_core::layout::{LaidOutBox, LayoutBox};
        use sghtmltopdf_core::style::ComputedStyle;
        use std::mem::size_of;
        println!(
            "type sizes: ComputedStyle {}B / LayoutBox {}B / LaidOutBox {}B",
            size_of::<ComputedStyle>(),
            size_of::<LayoutBox>(),
            size_of::<LaidOutBox>(),
        );
        use sghtmltopdf_core::fonts::ShapedGlyph;
        use sghtmltopdf_core::layout::{LaidOutContent, Layout};
        use sghtmltopdf_core::layout::{LineBox, TextRun};
        println!(
            "          Layout {}B / LaidOutContent {}B / Option<LineBox> {}B",
            size_of::<Layout>(),
            size_of::<LaidOutContent>(),
            size_of::<Option<LineBox>>(),
        );
        println!(
            "          LineBox {}B / TextRun {}B / ShapedGlyph {}B",
            size_of::<LineBox>(),
            size_of::<TextRun>(),
            size_of::<ShapedGlyph>(),
        );
    }
    println!("RSS at start {:.0}MB", rss_mb());
    let count: usize = env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let html_src = build_html(count);
    println!(
        "elements {count} / HTML {:.1}KB",
        html_src.len() as f64 / 1024.0
    );

    let fonts = load_fonts();
    let settings = PageSettings::default();
    let mut phases: Vec<(&str, f64, f64)> = Vec::new();

    reset_peak();
    let t = Instant::now();
    let dom = html::parse(html_src.as_bytes());
    phases.push(("HTML parse", t.elapsed().as_secs_f64(), rss_mb()));
    dump_live_allocations("HTML parse");
    reset_peak();

    let ua = user_agent_stylesheet();
    let author = parse_stylesheet("p { height: 60px; margin: 0; }");

    let t = Instant::now();
    let styles = compute_styles(&dom, &ua, &author);
    phases.push(("style computation", t.elapsed().as_secs_f64(), rss_mb()));
    dump_live_allocations("style computation");
    reset_peak();

    let t = Instant::now();
    let tree = build_box_tree(&dom, &styles);
    phases.push(("box tree build", t.elapsed().as_secs_f64(), rss_mb()));
    dump_live_allocations("box tree build");
    reset_peak();

    let t = Instant::now();
    let laid = layout_document(&tree, &styles, &fonts, settings.content_width());
    phases.push(("layout", t.elapsed().as_secs_f64(), rss_mb()));
    dump_live_allocations("layout");
    reset_peak();
    let mut c = Counts::default();
    count_boxes(&laid, &mut c);
    println!(
        "layout result: boxes {} / lines {} / runs {} / glyphs {} (per paragraph: runs {:.1}, glyphs {:.1})",
        c.boxes,
        c.lines,
        c.runs,
        c.glyphs,
        c.runs as f64 / count as f64,
        c.glyphs as f64 / count as f64
    );
    {
        use sghtmltopdf_core::fonts::ShapedGlyph;
        use sghtmltopdf_core::layout::{LaidOutBox, LineBox, TextRun};
        use std::mem::size_of;
        let real = c.boxes * size_of::<LaidOutBox>()
            + c.lines * size_of::<LineBox>()
            + c.runs * size_of::<TextRun>()
            + c.glyphs * size_of::<ShapedGlyph>();
        println!(
            "  real data {:.0}MB / Vec spare capacity {:.0}MB",
            real as f64 / 1024.0 / 1024.0,
            c.slack as f64 / 1024.0 / 1024.0
        );
    }
    dump_live_allocations("after layout");
    drop(laid);

    // Pagination redoes layout internally, so it is measured whole, separately from the above.
    let t = Instant::now();
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    phases.push((
        "pagination (layout included)",
        t.elapsed().as_secs_f64(),
        rss_mb(),
    ));
    dump_live_allocations("after pagination");

    let t = Instant::now();
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
    phases.push(("PDF encode", t.elapsed().as_secs_f64(), rss_mb()));

    println!(
        "pages {} / PDF {:.1}KB\n",
        pages.len(),
        bytes.len() as f64 / 1024.0
    );

    // "parse + styles + pagination + encode" is what the real total running time amounts to.
    let total: f64 = phases
        .iter()
        .filter(|(name, _, _)| *name != "layout" && *name != "box tree build")
        .map(|(_, secs, _)| secs)
        .sum();
    println!(
        "{:<28} {:>8}  {:>6}  {:>10}",
        "stage", "secs", "share", "resident RSS"
    );
    for (name, secs, rss) in &phases {
        let share = if *name == "layout" || *name == "box tree build" {
            "(breakdown)".to_string()
        } else {
            format!("{:.0}%", secs / total * 100.0)
        };
        println!("{name:<28} {secs:>8.2}  {share:>6}  {rss:>8.0}MB");
    }
    println!("{:<28} {total:>8.2}", "total (real work)");
}

fn build_html(count: usize) -> String {
    // Passing `empty` as the second argument makes the `<p>` elements carry no text,
    // to separate out the contribution of text processing (shaping and line breaking).
    let mode = env::args().nth(2).unwrap_or_default();
    let empty = mode == "empty";
    if mode == "table" {
        let mut html = String::with_capacity(count * 120);
        html.push_str(
            "<html><head><style>table { border-collapse: collapse; } \
             th, td { border: 1px solid #999999; padding: 4px 6px; }</style></head><body><table>",
        );
        for i in 0..count {
            let _ = write!(
                html,
                "<tr><td>{i}</td><td>Item {i} description text</td><td>{}</td>\
                 <td>{}</td><td>{}</td></tr>",
                i % 97 + 1,
                (i % 97 + 1) * 120,
                (i % 97 + 1) * 360
            );
        }
        html.push_str("</table></body></html>");
        return html;
    }
    let mut html = String::with_capacity(count * 48);
    html.push_str("<html><head><style>p { height: 60px; margin: 0; }</style></head><body>");
    for i in 0..count {
        if empty {
            html.push_str("<p></p>");
        } else {
            let _ = write!(html, "<p>paragraph {i} lorem ipsum dolor sit amet</p>");
        }
    }
    html.push_str("</body></html>");
    html
}

fn load_fonts() -> FontCollection {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts/DejaVuSans.ttf");
    let data = std::fs::read(path).expect("cannot read the font");
    let font = Font::from_bytes(data, 0).expect("cannot interpret the font");
    FontCollection::new(vec![font])
}

/// The current resident memory (MB). 0 outside Linux.
fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next())
                .and_then(|kib| kib.parse::<f64>().ok())
        })
        .unwrap_or(0.0)
        / 1024.0
}

#[derive(Default)]
struct Counts {
    boxes: usize,
    lines: usize,
    runs: usize,
    glyphs: usize,
    slack: usize,
}

/// Walk the layout result and count the elements it holds.
fn count_boxes(b: &sghtmltopdf_core::layout::LaidOutBox, c: &mut Counts) {
    use sghtmltopdf_core::layout::LaidOutContent;
    c.boxes += 1;
    match &b.content {
        LaidOutContent::Blocks(children) => {
            for child in children {
                count_boxes(child, c);
            }
        }
        LaidOutContent::Table(table) => {
            for row in &table.rows {
                for cell in &row.cells {
                    count_boxes(cell, c);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            c.slack += (lines.capacity() - lines.len())
                * std::mem::size_of::<sghtmltopdf_core::layout::LineBox>();
            for line in lines {
                c.lines += 1;
                c.runs += line.runs.len();
                c.slack += (line.runs.capacity() - line.runs.len())
                    * std::mem::size_of::<sghtmltopdf_core::layout::TextRun>();
                for run in &line.runs {
                    c.glyphs += run.glyphs.len();
                    c.slack += (run.glyphs.capacity() - run.glyphs.len())
                        * std::mem::size_of::<sghtmltopdf_core::fonts::ShapedGlyph>();
                    c.slack += run.text.capacity() - run.text.len();
                }
            }
        }
        _ => {}
    }
}
