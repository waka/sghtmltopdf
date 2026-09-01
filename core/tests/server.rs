//! E2E tests for HTTP server mode (`sghtmltopdf server`).
//!
//! They really start the binary as a server, send requests with `ureq` and check the status
//! codes and PDF bytes. To avoid a port clash it starts with `--listen 127.0.0.1:0` and
//! reads the real port from the `listening on <addr>` line on standard output.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");
const HTML: &str = "<html><body><h1>hello</h1></body></html>";

/// The started server. `Drop` reliably shuts it down.
struct TestServer {
    child: Child,
    addr: String,
}

impl TestServer {
    fn start(extra_args: &[&str]) -> Self {
        let mut child = Command::new(BIN)
            .arg("server")
            .args(["--listen", "127.0.0.1:0", "--font", FONT_PATH])
            .args(extra_args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("failed to start the server");

        let stdout = child.stdout.take().expect("stdout should be piped");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .expect("the server should announce its address");
        let addr = line
            .trim()
            .strip_prefix("listening on ")
            .unwrap_or_else(|| panic!("unexpected startup line: {line:?}"))
            .to_string();

        Self { child, addr }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// A helper letting the expected `/MediaBox` be written in CSS px
/// (it is written to the PDF in pt, 0.75x; f32 rounding means the same computation is done on the Rust side).
fn media_box(width_px: f32, height_px: f32) -> String {
    format!(
        "/MediaBox [0 0 {} {}]",
        width_px * sghtmltopdf_core::pdf::DEFAULT_SCALE,
        height_px * sghtmltopdf_core::pdf::DEFAULT_SCALE
    )
}

/// Extract the status code, 4xx and 5xx included.
fn status_of(result: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> u16 {
    match result {
        Ok(response) => response.status().as_u16(),
        Err(ureq::Error::StatusCode(code)) => code,
        Err(e) => panic!("unexpected transport error: {e}"),
    }
}

#[test]
fn healthz_and_version_respond() {
    let server = TestServer::start(&[]);

    let mut response = ureq::get(server.url("/healthz")).call().unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.body_mut().read_to_string().unwrap(), "ok");

    let mut response = ureq::get(server.url("/version")).call().unwrap();
    assert!(response
        .body_mut()
        .read_to_string()
        .unwrap()
        .starts_with("sghtmltopdf "));
}

#[test]
fn post_pdf_returns_a_pdf() {
    let server = TestServer::start(&[]);

    let mut response = ureq::post(server.url("/pdf")).send(HTML).unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap()),
        Some("application/pdf")
    );

    let pdf = response.body_mut().read_to_vec().unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(count_occurrences(&pdf, b"%%EOF") > 0);
}

#[test]
fn query_options_are_interpreted_exactly_like_the_cli() {
    let server = TestServer::start(&[]);

    let mut response = ureq::post(server.url("/pdf?page-size=A5&margin-top=0&margin-bottom=0"))
        .send(HTML)
        .unwrap();
    let pdf = response.body_mut().read_to_vec().unwrap();
    // A5 = 559.4 x 793.7 CSS px.
    assert!(
        count_occurrences(&pdf, media_box(559.4, 793.7).as_bytes()) > 0,
        "page-size=A5 should be applied"
    );
}

#[test]
fn a_boolean_query_flag_works_without_a_value() {
    let server = TestServer::start(&[]);

    let mut response = ureq::post(server.url("/pdf?no-pdf-compression&grayscale"))
        .send(r#"<html><body><p style="color:#ff0000">red</p></body></html>"#)
        .unwrap();
    let pdf = response.body_mut().read_to_vec().unwrap();
    assert_eq!(count_occurrences(&pdf, b"/FlateDecode"), 0);
    assert!(count_occurrences(&pdf, b"0.2126 0.2126 0.2126 rg") > 0);
}

#[test]
fn options_that_take_local_paths_are_rejected() {
    let server = TestServer::start(&[]);

    for query in [
        "/pdf?font=/etc/passwd",
        "/pdf?cover=/etc/passwd",
        "/pdf?user-style-sheet=/etc/passwd",
        "/pdf?base-url=/etc",
        "/pdf?output=/tmp/x.pdf",
    ] {
        let status = status_of(ureq::post(server.url(query)).send(HTML));
        assert_eq!(status, 400, "{query} must be rejected");
    }
}

#[test]
fn local_file_access_is_disabled_by_default() {
    // Local references are forbidden by default, so the image is not fetched and only the
    // body appears. Even attempted, the fetch fails and is ignored.
    let server = TestServer::start(&[]);
    let html = r#"<html><body><img src="/etc/hostname"><p>x</p></body></html>"#;

    let mut response = ureq::post(server.url("/pdf")).send(html).unwrap();
    let pdf = response.body_mut().read_to_vec().unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert_eq!(
        count_occurrences(&pdf, b"/Subtype /Image"),
        0,
        "no local file may be embedded"
    );
}

#[test]
fn an_empty_body_is_a_bad_request() {
    let server = TestServer::start(&[]);
    assert_eq!(status_of(ureq::post(server.url("/pdf")).send("")), 400);
}

#[test]
fn a_too_large_body_is_rejected() {
    let server = TestServer::start(&["--max-body-size", "128"]);
    let big = "x".repeat(1024);
    assert_eq!(status_of(ureq::post(server.url("/pdf")).send(big)), 413);
}

#[test]
fn unknown_paths_and_methods_are_reported() {
    let server = TestServer::start(&[]);
    assert_eq!(status_of(ureq::get(server.url("/nope")).call()), 404);
    assert_eq!(status_of(ureq::get(server.url("/pdf")).call()), 405);
}

#[test]
fn a_render_error_is_reported_as_a_server_error() {
    // Using `counter(pages)` in streaming mode is a rendering error.
    let server = TestServer::start(&[]);
    let html = r#"<html><head><style>
            @page { @bottom-center { content: counter(pages); } }
        </style></head><body><p>x</p></body></html>"#;
    let status = status_of(ureq::post(server.url("/pdf?streaming")).send(html));
    assert_eq!(status, 500);
}

#[test]
fn stream_query_switches_to_chunked_transfer_encoding() {
    let server = TestServer::start(&[]);

    // The default is a buffered response (with a Content-Length).
    let buffered = ureq::post(server.url("/pdf")).send(HTML).unwrap();
    assert!(
        buffered.headers().get("content-length").is_some(),
        "the default response should be buffered"
    );

    // `?stream=1` makes it chunked.
    let mut streamed = ureq::post(server.url("/pdf?stream=1")).send(HTML).unwrap();
    assert_eq!(streamed.status().as_u16(), 200);
    assert_eq!(
        streamed
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap()),
        Some("application/pdf")
    );

    let pdf = streamed.body_mut().read_to_vec().unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(count_occurrences(&pdf, b"%%EOF") > 0);
}

#[test]
fn other_query_options_still_apply_in_stream_mode() {
    let server = TestServer::start(&[]);

    let mut response = ureq::post(server.url("/pdf?stream=1&page-size=A5"))
        .send(HTML)
        .unwrap();
    let pdf = response.body_mut().read_to_vec().unwrap();
    assert!(
        count_occurrences(&pdf, media_box(559.4, 793.7).as_bytes()) > 0,
        "page-size must still be honored when streaming"
    );
}

#[test]
fn stream_mode_reports_errors_before_the_body_starts() {
    let server = TestServer::start(&[]);

    // A malformed query, an empty body and an oversized body are all detectable before the
    // headers are sent, so they come back with their usual status.
    assert_eq!(
        status_of(ureq::post(server.url("/pdf?stream=1&font=/etc/passwd")).send(HTML)),
        400
    );
    assert_eq!(
        status_of(ureq::post(server.url("/pdf?stream=1")).send("")),
        400
    );
}

#[test]
fn a_too_large_body_is_rejected_in_stream_mode_too() {
    let server = TestServer::start(&["--max-body-size", "128"]);
    let big = "x".repeat(1024);
    assert_eq!(
        status_of(ureq::post(server.url("/pdf?stream=1")).send(big)),
        413
    );
}
