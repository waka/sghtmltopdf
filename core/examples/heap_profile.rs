//! Aggregate heap allocations by their caller. For hunting down what creates the peak memory.
//!
//! Run with: `cargo run --release --example heap_profile [element count] [table]`
//!
//! `dhat` (a dev-dependency) replaces the global allocator and writes `dhat-heap.json` on
//! exit. Feeding that to the bundled `heap_report.py` lists, largest first, what was holding
//! memory at the peak.
//!
//! Where `phase_bench` shows which stage is expensive, this shows which code is allocating.
//! dhat records every allocation, so it runs several times slower. Do not use it to measure
//! time.

use std::collections::HashMap;
use std::env;
use std::fmt::Write as _;
use std::path::Path;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, PageSettings};
use sghtmltopdf_core::pdf::encode_pdf;
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    let profiler = dhat::Profiler::new_heap();

    let count: usize = env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let table_mode = env::args().nth(2).is_some_and(|v| v == "table");
    let html_src = build_html(count, table_mode);

    let fonts = load_fonts();
    let settings = PageSettings::default();
    let dom = html::parse(html_src.as_bytes());
    let author = parse_stylesheet(if table_mode {
        "table { border-collapse: collapse; } th, td { border: 1px solid #999999; padding: 4px 6px; }"
    } else {
        "p { height: 60px; margin: 0; }"
    });
    let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);
    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

    println!(
        "pages {} / PDF {:.1}KB",
        pages.len(),
        bytes.len() as f64 / 1024.0
    );
    // Dropping here is what writes dhat-heap.json.
    drop(profiler);
    println!("wrote dhat-heap.json");
}

fn build_html(count: usize, table_mode: bool) -> String {
    let mut html = String::with_capacity(count * 120);
    html.push_str("<html><head></head><body>");
    if table_mode {
        html.push_str("<table>");
        for i in 0..count {
            let _ = write!(
                html,
                "<tr><td>{i}</td><td>Item {i} description text</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                i % 97 + 1,
                (i % 97 + 1) * 120,
                (i % 97 + 1) * 360
            );
        }
        html.push_str("</table>");
    } else {
        for i in 0..count {
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
