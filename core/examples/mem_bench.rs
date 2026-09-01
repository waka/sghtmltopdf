//! Measure the peak memory and running time of the normal mode (`Mode::Batch`) and the
//! streaming mode (`Mode::Streaming`). It measures the same conditions (HTML shape, explicit
//! fonts, trial count) as the "memory and running time" table on the documentation site and
//! prints them as a Markdown table ready to paste in.
//!
//! Run with: `cargo run --release --example mem_bench`
//!
//! What is measured is the process's peak RSS (`VmHWM` in Linux's `/proc/self/status`), so
//! running both modes in one process would leave the earlier one's peak standing and make
//! the comparison meaningless. So the parent process re-executes itself per condition and
//! the child reports its own peak RSS (it behaves as the child when
//! `SGHTMLTOPDF_BENCH_CASE` is set).
//!
//! The numbers depend heavily on the environment. When putting them in the documentation,
//! record the machine and build type they were measured on too.

use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use sghtmltopdf_core::engine::{Engine, EngineOptions, FontSpec, Mode};
use sghtmltopdf_core::sink::FileSink;

/// The document sizes measured (the number of `<p>` elements).
const ELEMENT_COUNTS: &[usize] = &[1_000, 5_000, 20_000, 60_000];

/// The number of trials per condition. The better value is taken.
const RUNS: usize = 2;

/// The divisor for rounding off the remainder below 1MiB.
const MIB: f64 = 1024.0;

fn main() {
    // Called as a child process, it measures one condition and returns the result on one line.
    if let Ok(case) = env::var("SGHTMLTOPDF_BENCH_CASE") {
        run_one_case(&case);
        return;
    }

    let exe = env::current_exe().expect("cannot obtain the executable's path");
    println!("| elements | HTML size | normal mode | --streaming |");
    println!("|---|---|---|---|");
    for &count in ELEMENT_COUNTS {
        let html_bytes = build_html(count).len();
        let batch = best_of(&exe, count, Mode::Batch);
        let streaming = best_of(&exe, count, Mode::Streaming);
        println!(
            "| {} | {} | {} | {} |",
            group_digits(count),
            format_bytes(html_bytes),
            batch,
            streaming,
        );
    }
}

/// Measure the same condition [`RUNS`] times and return the minimum of the peak RSS and of the running time.
fn best_of(exe: &Path, count: usize, mode: Mode) -> String {
    let mut best_rss = f64::MAX;
    let mut best_secs = f64::MAX;
    for _ in 0..RUNS {
        let (rss_kib, secs) = spawn_case(exe, count, mode);
        best_rss = best_rss.min(rss_kib);
        best_secs = best_secs.min(secs);
    }
    format!("{:.0}MB / {:.2}s", best_rss / MIB, best_secs)
}

/// Start ourselves as a child process and receive `(peak RSS in KiB, seconds)`.
fn spawn_case(exe: &Path, count: usize, mode: Mode) -> (f64, f64) {
    let mode_name = match mode {
        Mode::Batch => "batch",
        Mode::Streaming => "streaming",
    };
    let output = Command::new(exe)
        .env("SGHTMLTOPDF_BENCH_CASE", format!("{count}:{mode_name}"))
        .output()
        .expect("cannot start the child process");
    if !output.status.success() {
        panic!(
            "the child process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let line = String::from_utf8_lossy(&output.stdout);
    let mut parts = line.split_whitespace();
    let rss = parts.next().and_then(|v| v.parse().ok());
    let secs = parts.next().and_then(|v| v.parse().ok());
    match (rss, secs) {
        (Some(rss), Some(secs)) => (rss, secs),
        _ => panic!("cannot interpret the child process's output: {line:?}"),
    }
}

/// The child side. It converts one condition and writes "peak RSS in KiB, seconds" to standard output.
fn run_one_case(case: &str) {
    let (count, mode_name) = case.split_once(':').expect("the condition is malformed");
    let count: usize = count.parse().expect("cannot interpret the element count");
    let mode = match mode_name {
        "batch" => Mode::Batch,
        "streaming" => Mode::Streaming,
        other => panic!("unknown mode: {other}"),
    };

    let html = build_html(count);
    let started = Instant::now();
    convert(&html, mode);
    let secs = started.elapsed().as_secs_f64();

    println!("{} {secs}", peak_rss_kib());
}

/// Run one conversion.
///
/// The output goes to a temporary file ([`FileSink`], as the CLI does). With a [`MemorySink`]
/// the whole PDF would stay in memory and inflate the streaming mode's numbers by the size
/// of the PDF.
fn convert(html: &str, mode: Mode) {
    let options = EngineOptions {
        mode,
        // The font is given explicitly so system font discovery cannot skew the numbers.
        fonts: vec![FontSpec {
            path: font_path(),
            index: 0,
        }],
        ..EngineOptions::default()
    };
    let out_path =
        env::temp_dir().join(format!("sghtmltopdf-mem-bench-{}.pdf", std::process::id()));
    let sink = FileSink::create(&out_path).expect("cannot create the output");
    let mut engine = Engine::new(options, sink);
    // Fed in 64KiB pieces, to stay close to real use.
    for chunk in html.as_bytes().chunks(64 * 1024) {
        engine.feed(chunk).expect("feed failed");
    }
    engine.finish().expect("finish failed");
    let _ = std::fs::remove_file(&out_path);
}

/// HTML that is just `count` `<p>` elements of height 60px.
fn build_html(count: usize) -> String {
    let mut html = String::with_capacity(count * 48);
    html.push_str("<html><head><style>p { height: 60px; margin: 0; }</style></head><body>");
    for i in 0..count {
        let _ = write!(html, "<p>paragraph {i} lorem ipsum dolor sit amet</p>");
    }
    html.push_str("</body></html>");
    html
}

fn font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts/DejaVuSans.ttf")
}

/// The maximum RSS this process has used so far (KiB).
///
/// Returns 0 outside Linux, where there is no `VmHWM` (only the running times in the table then mean anything).
fn peak_rss_kib() -> f64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0.0;
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kib| kib.parse().ok())
        .unwrap_or(0.0)
}

fn format_bytes(bytes: usize) -> String {
    let kib = bytes as f64 / 1024.0;
    if kib < 1024.0 {
        format!("{kib:.0}KB")
    } else {
        format!("{:.1}MB", kib / 1024.0)
    }
}

/// Insert thousands separators (to make the table easier to read).
fn group_digits(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
