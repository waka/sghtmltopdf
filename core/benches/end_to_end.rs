//! End-to-end benchmarks through the three public entry points:
//!
//! * `engine/*`: the `Engine` API in batch and streaming mode, fed in chunks
//!   the way the CLI does, for every fixture in the corpus.
//! * `cli/*`: the compiled binary, including process startup, option
//!   parsing, TOC, cover, header/footer HTML and the file sink.
//! * `server/*`: one HTTP round trip per conversion against a server
//!   started once for the group.
//!
//! Run: `cargo bench --bench end_to_end`
//! Just the CLI: `cargo bench --bench end_to_end -- '^cli/'`

mod support;

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use criterion::{
    criterion_group, criterion_main, BenchmarkId, Criterion, SamplingMode, Throughput,
};
use sghtmltopdf_core::engine::Mode;

use support::{cli_font_args, fixture, fixtures_dir, page_count, render, FIXTURES, MANIFEST_DIR};

const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");

fn engine(c: &mut Criterion) {
    for (mode_name, mode) in [("batch", Mode::Batch), ("streaming", Mode::Streaming)] {
        let mut group = c.benchmark_group(format!("engine/{mode_name}"));
        group.sampling_mode(SamplingMode::Flat);
        for fixture in FIXTURES {
            if mode == Mode::Streaming && !fixture.streaming {
                continue;
            }
            let html = fixture.html();
            group.throughput(Throughput::Bytes(html.len() as u64));
            group.bench_with_input(
                BenchmarkId::from_parameter(fixture.name),
                &html,
                |b, html| b.iter(|| render(html, mode)),
            );
        }
        group.finish();
    }
}

/// One CLI invocation: which fixture, and any extra flags.
struct CliCase {
    name: &'static str,
    input: PathBuf,
    args: Vec<String>,
}

fn cli_cases() -> Vec<CliCase> {
    let assets = fixtures_dir().join("assets");
    let arg = |p: &Path| p.display().to_string();
    let report = fixture("real_report").path();
    let tiny = std::env::temp_dir().join("sghtmltopdf-bench-tiny.html");
    std::fs::write(&tiny, "<html><body><p>hello</p></body></html>").expect("write tiny html");
    vec![
        CliCase {
            name: "startup",
            input: tiny,
            args: vec![],
        },
        CliCase {
            name: "report",
            input: report.clone(),
            args: vec![],
        },
        CliCase {
            name: "report_streaming",
            input: report.clone(),
            args: vec!["--streaming".into()],
        },
        CliCase {
            name: "report_toc",
            input: report.clone(),
            args: vec!["--toc".into(), "--enable-toc-back-links".into()],
        },
        CliCase {
            name: "report_header_footer_html",
            input: report.clone(),
            args: vec![
                "--header-html".into(),
                arg(&assets.join("header.html")),
                "--footer-html".into(),
                arg(&assets.join("footer.html")),
            ],
        },
        CliCase {
            name: "report_header_footer_text",
            input: report.clone(),
            args: vec![
                "--header-left".into(),
                "Annual report".into(),
                "--footer-center".into(),
                "[page] / [topage]".into(),
                "--header-line".into(),
            ],
        },
        CliCase {
            name: "report_cover",
            input: report.clone(),
            args: vec!["--cover".into(), arg(&assets.join("cover.html"))],
        },
        CliCase {
            name: "report_grayscale_uncompressed",
            input: report,
            args: vec!["--grayscale".into(), "--no-pdf-compression".into()],
        },
        CliCase {
            name: "report_letter_landscape_zoom",
            input: fixture("real_report").path(),
            args: vec![
                "--page-size".into(),
                "Letter".into(),
                "--orientation".into(),
                "Landscape".into(),
                "--zoom".into(),
                "0.8".into(),
                "--margin-top".into(),
                "10mm".into(),
            ],
        },
        CliCase {
            name: "invoice",
            input: fixture("real_invoice").path(),
            args: vec![],
        },
        CliCase {
            name: "receipt",
            input: fixture("real_receipt").path(),
            args: vec![],
        },
        CliCase {
            name: "images_no_images",
            input: fixture("images").path(),
            args: vec!["--no-images".into(), "--no-background".into()],
        },
    ]
}

/// Runs the binary once and returns the PDF size. Fonts are always the
/// bundled set; `--allow` widens local access to the whole crate so fixtures
/// can reach `tests/fonts` and `tests/fixtures/images`.
pub fn run_cli(input: &Path, extra: &[String], output: &Path) -> u64 {
    let status = Command::new(BIN)
        .arg(input)
        .args(cli_font_args())
        .args(["--allow", MANIFEST_DIR, "--quiet"])
        .args(extra)
        .arg("-o")
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run sghtmltopdf");
    assert!(
        status.success(),
        "sghtmltopdf failed on {}",
        input.display()
    );
    std::fs::metadata(output).map(|m| m.len()).unwrap_or(0)
}

fn cli(c: &mut Criterion) {
    let mut group = c.benchmark_group("cli");
    group.sampling_mode(SamplingMode::Flat);
    let output = std::env::temp_dir().join(format!("sghtmltopdf-bench-{}.pdf", std::process::id()));
    for case in cli_cases() {
        // Fail loudly before timing if the flags are wrong.
        let bytes = run_cli(&case.input, &case.args, &output);
        assert!(bytes > 0, "{} produced an empty PDF", case.name);
        group.bench_function(case.name, |b| {
            b.iter(|| run_cli(&case.input, &case.args, &output))
        });
    }
    let _ = std::fs::remove_file(&output);
    group.finish();
}

/// A running `sghtmltopdf server`, killed on drop.
struct Server {
    child: Child,
    url: String,
}

impl Server {
    fn start() -> Self {
        let mut child = Command::new(BIN)
            .arg("server")
            .args(["--listen", "127.0.0.1:0"])
            .args(cli_font_args())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start server");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("server announces its address");
        let addr = line
            .trim()
            .strip_prefix("listening on ")
            .unwrap_or_else(|| panic!("unexpected startup line: {line:?}"))
            .to_string();
        Self {
            child,
            url: format!("http://{addr}/pdf"),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn post_pdf(agent: &ureq::Agent, url: &str, html: &[u8]) -> Vec<u8> {
    let mut response = agent.post(url).send(html).expect("POST /pdf");
    assert_eq!(response.status().as_u16(), 200);
    response.body_mut().read_to_vec().expect("read PDF body")
}

fn server(c: &mut Criterion) {
    let server = Server::start();
    let agent = ureq::Agent::new_with_defaults();
    let mut group = c.benchmark_group("server");
    group.sampling_mode(SamplingMode::Flat);
    for fixture in FIXTURES.iter().filter(|f| !f.local_assets) {
        let html = fixture.html();
        assert!(page_count(&post_pdf(&agent, &server.url, &html)) > 0);
        group.throughput(Throughput::Bytes(html.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("post_pdf", fixture.name),
            &html,
            |b, html| b.iter(|| post_pdf(&agent, &server.url, html)),
        );
    }
    group.finish();
    drop(server);
}

fn config() -> Criterion {
    Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_millis(1500))
}

criterion_group! {
    name = benches;
    config = config();
    targets = engine, cli, server
}
criterion_main!(benches);
