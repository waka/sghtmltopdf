//! Compare the peak memory and running time of sghtmltopdf, wkhtmltopdf and headless
//! Chrome under the same conditions. The performance comparison table on the documentation site comes from this.
//!
//! Run with:
//!
//! ```text
//! cargo build --release                      # build the CLI under comparison first
//! cargo run --release --example compare_engines
//! ```
//!
//! An engine that cannot be found is skipped. Its location can also be given by an environment variable.
//!
//! * `WKHTMLTOPDF` - the official distribution is packages only, with no standalone binary,
//!   so unpacking the deb is the easy way to use it without installing:
//!   `dpkg-deb -x wkhtmltox_0.12.6.1-3.jammy_amd64.deb /tmp/wk`
//!   (running it needs `xfonts-base` and `xfonts-75dpi`)
//! * `CHROME` - with none given, `google-chrome` is looked up on `PATH`
//!
//! # What is held equal to make the comparison meaningful
//!
//! * The paper size and margins are set with `@page`, so all three get the same geometry
//!   from the same CSS (wkhtmltopdf alone ignores `@page`'s `size`, so the same values are
//!   passed on its CLI too)
//! * All three reference the same font file through `@font-face`
//! * wkhtmltopdf treats a CSS px as 1/72 inch, so [`ZOOM`] brings it to 1px = 1/96 inch.
//!   sghtmltopdf and Chrome use 1/96 inch already and need no correction
//!
//! The page counts still do not match exactly. The table shows the page counts too, so check
//! each time whether the comparison is within a meaningful range.
//!
//! # How the memory is measured
//!
//! Chrome splits into several processes (browser, renderer, GPU), so looking only at the
//! process we started shows less than half the reality (246MB against 567MB, measured).
//! So [`tree_pss_kib`] samples the total `Pss` of the whole process tree and takes the
//! maximum. All three are measured the same way.
//!
//! Being sampled, a spike shorter than [`SAMPLE_INTERVAL`] can be missed. What is measured
//! is also one conversion including browser startup, not the numbers for keeping a browser
//! resident and reusing it (pooling over CDP, say).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The size of the documents measured.
const PARAGRAPH_COUNTS: &[usize] = &[5_000, 20_000, 60_000];
const TABLE_COUNTS: &[usize] = &[5_000, 20_000];

/// The number of trials per condition. The better value is taken.
const RUNS: usize = 2;

/// The memory sampling interval.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(10);

/// The factor bringing wkhtmltopdf's px interpretation (1px = 1/72 inch) to 1px = 1/96 inch.
const ZOOM: &str = "1.3333333";

/// The paper size and margins. The same values are written both in `@page` and on wkhtmltopdf's CLI.
const PAGE_SIZE: &str = "A4";
const MARGIN: &str = "10mm";

fn main() {
    let sghtmltopdf = release_binary();
    if !sghtmltopdf.exists() {
        eprintln!(
            "{} does not exist. Run cargo build --release first",
            sghtmltopdf.display()
        );
        std::process::exit(1);
    }
    let wkhtmltopdf = find_binary("WKHTMLTOPDF", "wkhtmltopdf");
    let chrome = find_binary("CHROME", "google-chrome");
    for (name, found) in [("wkhtmltopdf", &wkhtmltopdf), ("chrome", &chrome)] {
        if found.is_none() {
            eprintln!("{name} was not found, so its column is skipped");
        }
    }

    let work = env::temp_dir().join("sghtmltopdf-compare");
    std::fs::create_dir_all(&work).expect("cannot create the working directory");
    std::fs::copy(font_path(), work.join("font.ttf")).expect("cannot copy the font");

    for (title, header, counts, table_mode) in [
        (
            "### A document that is mostly paragraphs",
            "elements",
            PARAGRAPH_COUNTS,
            false,
        ),
        (
            "### A business form that is mostly tables",
            "rows",
            TABLE_COUNTS,
            true,
        ),
    ] {
        let mut columns = vec!["sghtmltopdf", "sghtmltopdf (streaming)"];
        if wkhtmltopdf.is_some() {
            columns.push("wkhtmltopdf");
        }
        if chrome.is_some() {
            columns.push("headless Chrome");
        }
        println!("\n{title}\n");
        println!("| {header} | {} | pages |", columns.join(" | "));
        println!("|{}|", "---|".repeat(columns.len() + 2));

        for &count in counts {
            let name = if table_mode { "table" } else { "para" };
            let html = work.join(format!("{name}{count}.html"));
            std::fs::write(&html, build_html(count, table_mode)).expect("cannot write the HTML");

            let mut cells = Vec::new();
            let mut pages = Vec::new();
            // An engine that could not convert just gets a `-` in its cell and we carry on
            // (a document using CSS unavailable in streaming, say).
            let mut measure =
                |label: &str, command: &dyn Fn(&Path) -> Command, count_it: bool| match best_of(
                    &work, command,
                ) {
                    Some((cell, page_count)) => {
                        cells.push(cell);
                        if count_it {
                            pages.push(page_count.to_string());
                        }
                    }
                    None => {
                        eprintln!(
                            "{name}{count}: {label} failed to convert, so its cell becomes `-`"
                        );
                        cells.push("-".to_string());
                        if count_it {
                            pages.push("-".to_string());
                        }
                    }
                };

            measure(
                "sghtmltopdf",
                &|out| sg_command(&sghtmltopdf, &html, out, false),
                true,
            );
            measure(
                "sghtmltopdf (streaming)",
                &|out| sg_command(&sghtmltopdf, &html, out, true),
                false,
            );
            if let Some(wk) = &wkhtmltopdf {
                measure("wkhtmltopdf", &|out| wk_command(wk, &html, out), true);
            }
            if let Some(chrome) = &chrome {
                measure(
                    "headless Chrome",
                    &|out| chrome_command(chrome, &html, out),
                    true,
                );
            }
            println!(
                "| {} | {} | {} |",
                group_digits(count),
                cells.join(" | "),
                pages.join(" / ")
            );
        }
    }
}

/// Run the command `build` returns [`RUNS`] times and return "peak memory / running time"
/// plus the page count of the PDF produced.
///
/// `None` if even one conversion failed. The numbers from a failed run are unreliable, so we
/// never average over just the successful ones.
fn best_of(work: &Path, build: &dyn Fn(&Path) -> Command) -> Option<(String, usize)> {
    let out = work.join("out.pdf");
    let mut best_kib = f64::MAX;
    let mut best_secs = f64::MAX;
    for _ in 0..RUNS {
        let _ = std::fs::remove_file(&out);
        let (kib, secs) = run_and_measure(build(&out))?;
        best_kib = best_kib.min(kib);
        best_secs = best_secs.min(secs);
    }
    Some((
        format!("{:.0}MB / {:.2}s", best_kib / 1024.0, best_secs),
        count_pages(&out),
    ))
}

/// Run the command and return `(peak PSS in KiB, seconds)`. `None` if the conversion failed.
fn run_and_measure(mut command: Command) -> Option<(f64, f64)> {
    let started = Instant::now();
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("cannot start the conversion command");

    let mut peak = 0;
    loop {
        match child
            .try_wait()
            .expect("cannot obtain the child process's status")
        {
            Some(status) => {
                if !status.success() {
                    return None;
                }
                break;
            }
            None => {
                peak = peak.max(tree_pss_kib(child.id()));
                std::thread::sleep(SAMPLE_INTERVAL);
            }
        }
    }
    Some((peak as f64, started.elapsed().as_secs_f64()))
}

/// The total `Pss` (KiB) of `root` and its descendant processes.
///
/// `Pss` (proportional set size) divides shared pages by the number of processes sharing
/// them, so a multi-process browser is not double-counted.
fn tree_pss_kib(root: u32) -> u64 {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return 0;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            continue;
        };
        if let Some(ppid) = status
            .lines()
            .find_map(|line| line.strip_prefix("PPid:"))
            .and_then(|value| value.trim().parse::<u32>().ok())
        {
            children.entry(ppid).or_default().push(pid);
        }
    }

    let mut total = 0;
    let mut stack = vec![root];
    let mut seen = HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Ok(rollup) = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup")) {
            total += rollup
                .lines()
                .find_map(|line| line.strip_prefix("Pss:"))
                .and_then(|value| value.split_whitespace().next())
                .and_then(|kib| kib.parse::<u64>().ok())
                .unwrap_or(0);
        }
        stack.extend(children.get(&pid).into_iter().flatten());
    }
    total
}

fn sg_command(sghtmltopdf: &Path, html: &Path, out: &Path, streaming: bool) -> Command {
    let mut command = Command::new(sghtmltopdf);
    command
        .arg(html)
        .arg("-o")
        .arg(out)
        .arg("--enable-local-file-access")
        .arg("-q");
    if streaming {
        command.arg("--streaming");
    }
    command
}

fn wk_command(wkhtmltopdf: &Path, html: &Path, out: &Path) -> Command {
    let mut command = Command::new(wkhtmltopdf);
    command
        .arg("-q")
        .arg("--disable-javascript")
        // Needed so the local font in `@font-face` can be read.
        .arg("--enable-local-file-access")
        .arg("--zoom")
        .arg(ZOOM)
        // wkhtmltopdf ignores `@page`'s `size`/`margin`, so they are passed on the CLI too.
        .arg("--page-size")
        .arg(PAGE_SIZE)
        .args(["-T", MARGIN, "-B", MARGIN, "-L", MARGIN, "-R", MARGIN])
        .arg(html)
        .arg(out);
    command
}

fn chrome_command(chrome: &Path, html: &Path, out: &Path) -> Command {
    let mut command = Command::new(chrome);
    command
        .arg("--headless")
        // Needed to run it in a container or under WSL (the process layout changes).
        .arg("--no-sandbox")
        .arg("--disable-gpu")
        .arg("--disable-extensions")
        .arg("--no-first-run")
        .arg(format!("--print-to-pdf={}", out.display()))
        .arg(html);
    command
}

/// The number of `/Type /Page` occurrences in the PDF. Used to confirm all three processed the same amount.
fn count_pages(pdf: &Path) -> usize {
    let bytes = std::fs::read(pdf).unwrap_or_default();
    let mut count = 0;
    for (i, _) in bytes.windows(5).enumerate().filter(|(_, w)| *w == b"/Type") {
        // Whether there is whitespace between `/Type` and `/Page` differs by writer.
        let mut j = i + 5;
        while matches!(bytes.get(j), Some(b' ' | b'\n' | b'\r')) {
            j += 1;
        }
        let rest = &bytes[j..];
        if rest.starts_with(b"/Page") && !rest.starts_with(b"/Pages") {
            count += 1;
        }
    }
    count
}

/// The HTML used for the comparison. The paper is set with `@page` and the dimensions in px (a unit all three understand).
fn build_html(count: usize, table_mode: bool) -> String {
    let mut html = String::with_capacity(count * 120);
    let _ = write!(
        html,
        "<html><head><meta charset=\"utf-8\"><style>\
         @page {{ size: {PAGE_SIZE}; margin: {MARGIN}; }}\
         @font-face {{ font-family: 'BenchSans'; src: url('font.ttf') format('truetype'); }}\
         html, body {{ margin: 0; padding: 0; }}\
         body {{ font-family: 'BenchSans'; font-size: 12px; line-height: 1.4; }}"
    );
    if table_mode {
        html.push_str(
            "table { width: 100%; border-collapse: collapse; }\
             th, td { border: 1px solid #999999; padding: 4px 6px; }",
        );
        html.push_str("</style></head><body><table>");
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
        html.push_str("p { height: 60px; margin: 0; }</style></head><body>");
        for i in 0..count {
            let _ = write!(html, "<p>paragraph {i} lorem ipsum dolor sit amet</p>");
        }
    }
    html.push_str("</body></html>");
    html
}

/// Look in the location given by `env_var`, and on `PATH` if there is none.
fn find_binary(env_var: &str, name: &str) -> Option<PathBuf> {
    if let Ok(path) = env::var(env_var) {
        let path = PathBuf::from(path);
        return path.exists().then_some(path);
    }
    let found = Command::new("which").arg(name).output().ok()?;
    found
        .status
        .success()
        .then(|| PathBuf::from(String::from_utf8_lossy(&found.stdout).trim()))
}

/// The CLI binary `cargo build --release` produces.
fn release_binary() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/release")
        .join("sghtmltopdf")
}

fn font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fonts/DejaVuSans.ttf")
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
