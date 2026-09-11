//! Deterministic performance metrics with a baseline gate.
//!
//! Wall-clock time on a shared CI runner is noisy, so this binary records the
//! numbers that are not: allocation count and bytes, peak live heap, page
//! count and PDF size, plus peak RSS and best-of-N time. Every case runs in
//! its own child process so RSS and the allocation counters start from zero.
//!
//! ```text
//! cargo bench --bench metrics                       # compare with benches/baseline.json
//! cargo bench --bench metrics -- --save-baseline    # (re)write the baseline
//! cargo bench --bench metrics -- --filter scale/    # subset of cases
//! cargo bench --bench metrics -- --json out.json    # also dump this run
//! cargo bench --bench metrics -- --no-time          # keep time out of the verdict
//! cargo bench --bench metrics -- --markdown         # pipe table, for pasting in a PR
//! cargo bench --bench metrics -- --list
//! ```
//!
//! Exit status is 1 when any gate fails (see [`Thresholds`]). Time only warns
//! unless it moves by more than the fail threshold, and peak RSS is compared
//! only when the baseline was recorded on the same OS and architecture.
//! `--no-time` drops time from the verdict entirely (it is still measured and
//! printed); CI uses it because a shared runner's clock is not comparable.

mod support;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sghtmltopdf_core::engine::Mode;

use support::alloc::{self, CountingAlloc};
use support::{
    cli_font_args, fixture, fixtures_dir, page_count, peak_rss_bytes, render, Fixture, Generator,
    FIXTURES, MANIFEST_DIR, SCALE_KINDS,
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");
const CASE_ENV: &str = "SGHTMLTOPDF_BENCH_CASE";

// ---------------------------------------------------------------------------
// Cases
// ---------------------------------------------------------------------------

enum Kind {
    Engine {
        fixture: &'static Fixture,
        mode: Mode,
    },
    Scale {
        generate: Generator,
        count: usize,
        mode: Mode,
    },
    Cli {
        input: PathBuf,
        args: Vec<String>,
    },
}

struct Case {
    name: String,
    kind: Kind,
}

fn mode_name(mode: Mode) -> &'static str {
    match mode {
        Mode::Batch => "batch",
        Mode::Streaming => "streaming",
    }
}

fn cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for mode in [Mode::Batch, Mode::Streaming] {
        for fixture in FIXTURES {
            if mode == Mode::Streaming && !fixture.streaming {
                continue;
            }
            cases.push(Case {
                name: format!("engine/{}/{}", mode_name(mode), fixture.name),
                kind: Kind::Engine { fixture, mode },
            });
        }
    }
    for kind in SCALE_KINDS {
        for mode in [Mode::Batch, Mode::Streaming] {
            for &count in kind.sizes {
                cases.push(Case {
                    name: format!("scale/{}/{}/{count}", kind.name, mode_name(mode)),
                    kind: Kind::Scale {
                        generate: kind.generate,
                        count,
                        mode,
                    },
                });
            }
        }
    }
    let assets = fixtures_dir().join("assets");
    let arg = |p: &Path| p.display().to_string();
    let report = fixture("real_report").path();
    let cli: [(&str, PathBuf, Vec<String>); 6] = [
        ("report", report.clone(), vec![]),
        (
            "report_streaming",
            report.clone(),
            vec!["--streaming".into()],
        ),
        ("report_toc", report.clone(), vec!["--toc".into()]),
        (
            "report_header_footer_html",
            report.clone(),
            vec![
                "--header-html".into(),
                arg(&assets.join("header.html")),
                "--footer-html".into(),
                arg(&assets.join("footer.html")),
            ],
        ),
        (
            "report_cover",
            report,
            vec!["--cover".into(), arg(&assets.join("cover.html"))],
        ),
        ("invoice", fixture("real_invoice").path(), vec![]),
    ];
    for (name, input, args) in cli {
        cases.push(Case {
            name: format!("cli/{name}"),
            kind: Kind::Cli { input, args },
        });
    }
    cases
}

// ---------------------------------------------------------------------------
// Measurement (child process side)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Measurement {
    name: String,
    /// Best of N runs, seconds.
    seconds: f64,
    peak_rss_bytes: u64,
    /// Zero for CLI cases: the allocator counters live in this process.
    alloc_count: u64,
    alloc_bytes: u64,
    peak_live_bytes: u64,
    pages: u64,
    pdf_bytes: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Report {
    os: String,
    arch: String,
    runs: usize,
    cases: Vec<Measurement>,
}

fn measure_in_process(name: &str, html: &[u8], mode: Mode) -> Measurement {
    alloc::reset();
    let started = Instant::now();
    let pdf = render(html, mode);
    let seconds = started.elapsed().as_secs_f64();
    let stats = alloc::snapshot();
    Measurement {
        name: name.to_string(),
        seconds,
        peak_rss_bytes: peak_rss_bytes(false),
        alloc_count: stats.count as u64,
        alloc_bytes: stats.bytes as u64,
        peak_live_bytes: stats.peak_live as u64,
        pages: page_count(&pdf) as u64,
        pdf_bytes: pdf.len() as u64,
    }
}

fn measure_cli(name: &str, input: &Path, args: &[String]) -> Measurement {
    let output =
        std::env::temp_dir().join(format!("sghtmltopdf-metrics-{}.pdf", std::process::id()));
    let started = Instant::now();
    let status = Command::new(BIN)
        .arg(input)
        .args(cli_font_args())
        .args(["--allow", MANIFEST_DIR, "--quiet"])
        .args(args)
        .arg("-o")
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run sghtmltopdf");
    let seconds = started.elapsed().as_secs_f64();
    assert!(status.success(), "sghtmltopdf failed for {name}");
    let pdf = std::fs::read(&output).expect("read output PDF");
    let _ = std::fs::remove_file(&output);
    Measurement {
        name: name.to_string(),
        seconds,
        peak_rss_bytes: peak_rss_bytes(true),
        alloc_count: 0,
        alloc_bytes: 0,
        peak_live_bytes: 0,
        pages: page_count(&pdf) as u64,
        pdf_bytes: pdf.len() as u64,
    }
}

/// Child entry point: measure one case and print it as a JSON line.
fn run_child(name: &str) {
    let case = cases()
        .into_iter()
        .find(|c| c.name == name)
        .unwrap_or_else(|| panic!("unknown case {name}"));
    let measurement = match case.kind {
        Kind::Engine { fixture, mode } => {
            let html = fixture.html();
            measure_in_process(name, &html, mode)
        }
        Kind::Scale {
            generate,
            count,
            mode,
        } => {
            let html = generate(count);
            measure_in_process(name, html.as_bytes(), mode)
        }
        Kind::Cli { input, args } => measure_cli(name, &input, &args),
    };
    println!(
        "{}",
        serde_json::to_string(&measurement).expect("serialise")
    );
}

/// Parent side: spawn ourselves once per run and keep the best time (other
/// metrics are identical across runs, or nearly so for RSS).
fn measure(name: &str, runs: usize) -> Measurement {
    let exe = std::env::current_exe().expect("current exe");
    let mut best: Option<Measurement> = None;
    for _ in 0..runs {
        // The engine's warnings (unresolved font-family in streaming mode and
        // the like) are expected for some fixtures; show stderr only on failure.
        let output = Command::new(&exe)
            .env(CASE_ENV, name)
            .output()
            .expect("spawn child");
        assert!(
            output.status.success(),
            "child failed for {name}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let line = String::from_utf8_lossy(&output.stdout);
        let m: Measurement = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("bad child output for {name}: {e}: {line:?}"));
        best = Some(match best {
            Some(b) if b.seconds <= m.seconds => Measurement {
                peak_rss_bytes: b.peak_rss_bytes.min(m.peak_rss_bytes),
                ..b
            },
            Some(b) => Measurement {
                peak_rss_bytes: b.peak_rss_bytes.min(m.peak_rss_bytes),
                ..m
            },
            None => m,
        });
    }
    best.expect("at least one run")
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

/// Relative growth allowed before a metric counts as a regression.
struct Thresholds {
    alloc: f64,
    rss: f64,
    time_warn: f64,
    time_fail: f64,
}

const THRESHOLDS: Thresholds = Thresholds {
    alloc: 0.05,
    rss: 0.10,
    time_warn: 0.15,
    time_fail: 0.30,
};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Verdict {
    Ok,
    New,
    Warn,
    Fail,
}

struct Comparison {
    verdict: Verdict,
    notes: Vec<String>,
}

fn growth(current: u64, baseline: u64) -> f64 {
    if baseline == 0 {
        return 0.0;
    }
    current as f64 / baseline as f64 - 1.0
}

fn pct(x: f64) -> String {
    format!("{:+.1}%", x * 100.0)
}

fn compare(
    current: &Measurement,
    baseline: Option<&Measurement>,
    same_platform: bool,
    gate_time: bool,
) -> Comparison {
    let Some(base) = baseline else {
        return Comparison {
            verdict: Verdict::New,
            notes: vec!["no baseline".into()],
        };
    };
    let mut verdict = Verdict::Ok;
    let mut notes = Vec::new();
    let mut raise = |v: Verdict, note: String| {
        if v > verdict {
            verdict = v;
        }
        notes.push(note);
    };

    if current.pages != base.pages {
        raise(
            Verdict::Fail,
            format!("pages {} -> {}", base.pages, current.pages),
        );
    }
    if current.pdf_bytes != base.pdf_bytes {
        raise(
            Verdict::Fail,
            format!(
                "pdf size {} -> {} ({})",
                base.pdf_bytes,
                current.pdf_bytes,
                pct(growth(current.pdf_bytes, base.pdf_bytes))
            ),
        );
    }
    for (label, cur, old) in [
        ("allocs", current.alloc_count, base.alloc_count),
        ("alloc bytes", current.alloc_bytes, base.alloc_bytes),
        ("peak live", current.peak_live_bytes, base.peak_live_bytes),
    ] {
        if old == 0 {
            continue;
        }
        let g = growth(cur, old);
        if g > THRESHOLDS.alloc {
            raise(Verdict::Fail, format!("{label} {}", pct(g)));
        }
    }
    if same_platform && base.peak_rss_bytes > 0 {
        let g = growth(current.peak_rss_bytes, base.peak_rss_bytes);
        if g > THRESHOLDS.rss {
            raise(Verdict::Fail, format!("peak rss {}", pct(g)));
        }
    }
    if gate_time && base.seconds > 0.0 {
        let g = current.seconds / base.seconds - 1.0;
        if g > THRESHOLDS.time_fail {
            raise(Verdict::Fail, format!("time {}", pct(g)));
        } else if g > THRESHOLDS.time_warn {
            raise(Verdict::Warn, format!("time {}", pct(g)));
        }
    }
    Comparison { verdict, notes }
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

struct Options {
    baseline: PathBuf,
    save_baseline: bool,
    json: Option<PathBuf>,
    runs: usize,
    filter: Option<String>,
    list: bool,
    fail: bool,
    gate_time: bool,
    markdown: bool,
}

fn default_baseline() -> PathBuf {
    Path::new(MANIFEST_DIR).join("benches/baseline.json")
}

fn parse_args() -> Options {
    let mut opts = Options {
        baseline: default_baseline(),
        save_baseline: false,
        json: None,
        runs: 3,
        filter: None,
        list: false,
        fail: true,
        gate_time: true,
        markdown: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            // Passed by `cargo bench`; irrelevant here.
            "--bench" => {}
            "--save-baseline" => opts.save_baseline = true,
            "--baseline" => opts.baseline = PathBuf::from(args.next().expect("--baseline <path>")),
            "--json" => opts.json = Some(PathBuf::from(args.next().expect("--json <path>"))),
            "--runs" => {
                opts.runs = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .expect("--runs <n>")
            }
            "--filter" => opts.filter = Some(args.next().expect("--filter <substring>")),
            "--list" => opts.list = true,
            "--markdown" => opts.markdown = true,
            "--no-fail" => opts.fail = false,
            "--no-time" => opts.gate_time = false,
            other if other.starts_with('-') => panic!("unknown option {other}"),
            other => opts.filter = Some(other.to_string()),
        }
    }
    opts
}

fn format_bytes(bytes: u64) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 1.0 {
        format!("{mib:.1}MB")
    } else {
        format!("{:.0}KB", bytes as f64 / 1024.0)
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

const HEADERS: [&str; 9] = [
    "time",
    "\u{394}",
    "peak RSS",
    "allocs",
    "alloc bytes",
    "peak live",
    "pages",
    "PDF",
    "status",
];

/// One measured case, split into the group it belongs to (`engine/batch`) and
/// the leaf name shown in the table (`text_inline`).
struct Row {
    group: String,
    leaf: String,
    cells: [String; 9],
    verdict: Verdict,
    delta: Option<f64>,
}

impl Row {
    fn new(m: &Measurement, verdict: Verdict, delta: Option<f64>) -> Row {
        let (group, leaf) = match m.name.rsplit_once('/') {
            Some((group, leaf)) => (group.to_string(), leaf.to_string()),
            None => (String::new(), m.name.clone()),
        };
        Row {
            group,
            leaf,
            cells: [
                format!("{:.3}s", m.seconds),
                delta.map(pct).unwrap_or_else(|| "-".into()),
                format_bytes(m.peak_rss_bytes),
                format_count(m.alloc_count),
                format_bytes(m.alloc_bytes),
                format_bytes(m.peak_live_bytes),
                m.pages.to_string(),
                format_bytes(m.pdf_bytes),
                verdict_name(verdict).to_string(),
            ],
            verdict,
            delta,
        }
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Ok => "ok",
        Verdict::New => "new",
        Verdict::Warn => "WARN",
        Verdict::Fail => "FAIL",
    }
}

fn text_width(s: &str) -> usize {
    s.chars().count()
}

/// ANSI styling, disabled when stdout is not a terminal or `NO_COLOR` is set.
struct Style {
    on: bool,
}

impl Style {
    fn detect() -> Style {
        Style {
            on: std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn paint(&self, text: &str, code: &str) -> String {
        if self.on {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }
}

fn verdict_color(v: Verdict) -> &'static str {
    match v {
        Verdict::Ok => "2",
        Verdict::New => "36",
        Verdict::Warn => "33",
        Verdict::Fail => "1;31",
    }
}

/// Green once a case is meaningfully faster, yellow and red at the time gates.
fn delta_color(d: f64) -> &'static str {
    if d >= THRESHOLDS.time_fail {
        "31"
    } else if d >= THRESHOLDS.time_warn {
        "33"
    } else if d <= -0.05 {
        "32"
    } else {
        "2"
    }
}

const INDENT: usize = 2;
const GAP: usize = 2;

/// Aligned columns, one section per case group, for reading in a terminal.
fn print_table(rows: &[Row]) {
    let style = Style::detect();
    let case_w = rows
        .iter()
        .map(|r| text_width(&r.leaf) + INDENT)
        .chain(rows.iter().map(|r| text_width(&r.group)))
        .chain(std::iter::once(text_width("case")))
        .max()
        .unwrap_or(4);
    let widths: Vec<usize> = HEADERS
        .iter()
        .enumerate()
        .map(|(i, h)| {
            rows.iter()
                .map(|r| text_width(&r.cells[i]))
                .chain(std::iter::once(text_width(h)))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut header = format!("{:<case_w$}", "case");
    for (i, h) in HEADERS.iter().enumerate() {
        header.push_str(&" ".repeat(GAP));
        // The status column is the last one and reads better flush left.
        if i == HEADERS.len() - 1 {
            header.push_str(h);
        } else {
            let pad = widths[i] - text_width(h);
            header.push_str(&" ".repeat(pad));
            header.push_str(h);
        }
    }
    let rule_w = case_w + widths.iter().map(|w| w + GAP).sum::<usize>();
    println!("{}", style.paint(&header, "1"));
    println!("{}", style.paint(&"\u{2500}".repeat(rule_w), "2"));

    let mut group: Option<&str> = None;
    for row in rows {
        if group != Some(row.group.as_str()) {
            if group.is_some() {
                println!();
            }
            if !row.group.is_empty() {
                println!("{}", style.paint(&row.group, "1;36"));
            }
            group = Some(&row.group);
        }
        let mut line = format!(
            "{}{:<w$}",
            " ".repeat(INDENT),
            row.leaf,
            w = case_w - INDENT
        );
        for (i, cell) in row.cells.iter().enumerate() {
            line.push_str(&" ".repeat(GAP));
            let last = i == row.cells.len() - 1;
            let padded = if last {
                cell.clone()
            } else {
                format!("{:>w$}", cell, w = widths[i])
            };
            let color = match i {
                1 => row.delta.map(delta_color).unwrap_or("2"),
                _ if last => verdict_color(row.verdict),
                _ => "",
            };
            if color.is_empty() {
                line.push_str(&padded);
            } else {
                line.push_str(&style.paint(&padded, color));
            }
        }
        println!("{}", line.trim_end());
    }
}

/// The pipe table, for pasting into a PR or an issue.
fn print_markdown(rows: &[Row]) {
    print!("| case |");
    for h in HEADERS {
        print!(" {h} |");
    }
    println!();
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---|");
    for row in rows {
        let name = if row.group.is_empty() {
            row.leaf.clone()
        } else {
            format!("{}/{}", row.group, row.leaf)
        };
        println!("| {} | {} |", name, row.cells.join(" | "));
    }
}

fn main() {
    if let Ok(name) = std::env::var(CASE_ENV) {
        run_child(&name);
        return;
    }

    let opts = parse_args();
    let selected: Vec<Case> = cases()
        .into_iter()
        .filter(|c| opts.filter.as_ref().is_none_or(|f| c.name.contains(f)))
        .collect();
    if opts.list {
        for case in &selected {
            println!("{}", case.name);
        }
        return;
    }

    let os = std::env::consts::OS.to_string();
    let arch = std::env::consts::ARCH.to_string();
    let baseline: Option<Report> = match std::fs::read(&opts.baseline) {
        Ok(bytes) => Some(serde_json::from_slice(&bytes).expect("parse baseline")),
        Err(_) => None,
    };
    let same_platform = baseline
        .as_ref()
        .is_some_and(|b| b.os == os && b.arch == arch);
    if let Some(b) = &baseline {
        eprintln!(
            "baseline: {} ({} {}{})",
            opts.baseline.display(),
            b.os,
            b.arch,
            if same_platform {
                ""
            } else {
                ", different platform: peak RSS not compared"
            }
        );
    } else if !opts.save_baseline {
        eprintln!(
            "no baseline at {} (run with --save-baseline to create one)",
            opts.baseline.display()
        );
    }

    let mut results = Vec::with_capacity(selected.len());
    let mut rows = Vec::with_capacity(selected.len());
    let mut worst = Verdict::Ok;
    let mut problems = Vec::new();
    // Nothing is printed until every case is measured, so the columns can be
    // sized; a run takes a while, hence the progress line on an interactive
    // stderr.
    let progress = std::io::stderr().is_terminal();
    let started = Instant::now();
    for (i, case) in selected.iter().enumerate() {
        if progress {
            eprint!("\r\x1b[2K[{}/{}] {}", i + 1, selected.len(), case.name);
            let _ = std::io::stderr().flush();
        }
        let m = measure(&case.name, opts.runs);
        let base = baseline
            .as_ref()
            .and_then(|b| b.cases.iter().find(|c| c.name == m.name));
        let cmp = compare(&m, base, same_platform, opts.gate_time);
        let delta = base.map(|b| m.seconds / b.seconds - 1.0);
        if cmp.verdict >= Verdict::Warn {
            problems.push(format!(
                "{} {}: {}",
                verdict_name(cmp.verdict),
                m.name,
                cmp.notes.join(", ")
            ));
        }
        worst = worst.max(cmp.verdict);
        rows.push(Row::new(&m, cmp.verdict, delta));
        results.push(m);
    }
    if progress {
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }

    if opts.markdown {
        print_markdown(&rows);
    } else {
        print_table(&rows);
        let counts = |v: Verdict| rows.iter().filter(|r| r.verdict == v).count();
        let mut summary = vec![format!(
            "{} case{}",
            rows.len(),
            if rows.len() == 1 { "" } else { "s" }
        )];
        for v in [Verdict::New, Verdict::Warn, Verdict::Fail] {
            if counts(v) > 0 {
                summary.push(format!("{} {}", counts(v), verdict_name(v)));
            }
        }
        summary.push(format!("{:.1}s", started.elapsed().as_secs_f64()));
        let style = Style::detect();
        println!();
        println!("{}", style.paint(&summary.join("  \u{b7}  "), "2"));
    }

    let report = Report {
        os,
        arch,
        runs: opts.runs,
        cases: results,
    };
    if let Some(path) = &opts.json {
        std::fs::write(path, serde_json::to_string_pretty(&report).unwrap()).expect("write json");
        eprintln!("wrote {}", path.display());
    }
    if opts.save_baseline {
        // A filtered run only replaces the cases it measured.
        let mut merged = baseline
            .filter(|b| b.os == report.os && b.arch == report.arch)
            .map(|b| b.cases)
            .unwrap_or_default();
        for m in &report.cases {
            match merged.iter_mut().find(|c| c.name == m.name) {
                Some(slot) => *slot = m.clone(),
                None => merged.push(m.clone()),
            }
        }
        let saved = Report {
            cases: merged,
            ..report
        };
        std::fs::write(
            &opts.baseline,
            serde_json::to_string_pretty(&saved).unwrap(),
        )
        .expect("write baseline");
        eprintln!("saved baseline to {}", opts.baseline.display());
        return;
    }

    if !problems.is_empty() {
        eprintln!();
        for p in &problems {
            eprintln!("{p}");
        }
    }
    if worst == Verdict::Fail && opts.fail {
        eprintln!("\nperformance regression detected");
        std::process::exit(1);
    }
}
