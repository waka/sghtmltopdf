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
        width_px * sghtmltopdf::pdf::DEFAULT_SCALE,
        height_px * sghtmltopdf::pdf::DEFAULT_SCALE
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
    // Body decorations still require batch mode.
    let server = TestServer::start(&[]);
    let html = r#"<html><head><style>
            body { background: red; }
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

#[test]
fn inline_html_headers_and_footers_are_allowed_per_request() {
    let server = TestServer::start(&[]);
    for option in ["header-html-content", "footer-html-content"] {
        for mode in ["", "&streaming", "&stream=1"] {
            let url = server.url(&format!(
                "/pdf?no-pdf-compression&{option}=%3Cdiv%3E%5Btopage%5D%3C%2Fdiv%3E{mode}"
            ));
            let mut response = ureq::post(url)
                .send("<p>First</p><p style=\"break-before:page\">Second</p>")
                .unwrap();
            let bytes = response.body_mut().read_to_vec().unwrap();
            assert_eq!(count_occurrences(&bytes, b"/MediaBox"), 2);
            assert!(count_occurrences(&bytes, b"<0032>") > 0);
        }
    }
}

// ---------------------------------------------------------------------------------------
// `multipart/form-data` requests: the document, the files it refers to, and options.
//
// The form follows Gotenberg's (files in parts named `files`, `index.html` the document,
// `header.html` / `footer.html` the page header and footer, folders dropped from names), as
// described in `core/src/cli/upload.rs`. The multipart syntax itself is RFC 7578.
// ---------------------------------------------------------------------------------------

const IMAGES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/images");
const MONO_FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/DejaVuSansMono.ttf"
);
const BOUNDARY: &str = "----sghtmltopdf-test-boundary";

/// One part of a multipart body: a file (`Some(file_name)`) or a plain option.
struct FormPart<'a> {
    name: &'a str,
    file_name: Option<&'a str>,
    data: Vec<u8>,
}

fn file<'a>(file_name: &'a str, data: impl Into<Vec<u8>>) -> FormPart<'a> {
    FormPart {
        name: "files",
        file_name: Some(file_name),
        data: data.into(),
    }
}

fn option<'a>(name: &'a str, value: &str) -> FormPart<'a> {
    FormPart {
        name,
        file_name: None,
        data: value.as_bytes().to_vec(),
    }
}

/// Encode parts as RFC 7578 §4.1 shows: each part opens with a boundary line and its
/// `Content-Disposition`, and the body ends with the closing boundary.
fn multipart_body(parts: &[FormPart]) -> Vec<u8> {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        let disposition = match part.file_name {
            Some(file_name) => format!(
                "Content-Disposition: form-data; name=\"{}\"; filename=\"{file_name}\"\r\n\r\n",
                part.name
            ),
            None => format!(
                "Content-Disposition: form-data; name=\"{}\"\r\n\r\n",
                part.name
            ),
        };
        body.extend_from_slice(disposition.as_bytes());
        body.extend_from_slice(&part.data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    body
}

fn post_form(
    server: &TestServer,
    path_and_query: &str,
    parts: &[FormPart],
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let body = multipart_body(parts);
    ureq::post(server.url(path_and_query))
        .header(
            "Content-Type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .send(&body[..])
}

fn render_form(server: &TestServer, path_and_query: &str, parts: &[FormPart]) -> Vec<u8> {
    let mut response =
        post_form(server, path_and_query, parts).expect("the request should succeed");
    assert_eq!(response.status().as_u16(), 200);
    response.body_mut().read_to_vec().unwrap()
}

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("{IMAGES}/{name}")).unwrap()
}

/// The directories this server process made for uploads and has not removed.
fn leftover_upload_dirs(server: &TestServer) -> usize {
    let prefix = format!("sghtmltopdf-upload-{}-", server.child.id());
    std::fs::read_dir(std::env::temp_dir())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .count()
}

#[test]
fn an_uploaded_document_renders_like_the_same_html_sent_raw() {
    let server = TestServer::start(&[]);
    let html = "<html><body><h1>hello</h1><p>world</p></body></html>";
    // A fixed creation date makes the same input render to the same bytes.
    let query = "/pdf?creation-date=2026-01-01T00:00:00Z";

    let raw = {
        let mut response = ureq::post(server.url(query)).send(html).unwrap();
        response.body_mut().read_to_vec().unwrap()
    };
    let uploaded = render_form(&server, query, &[file("index.html", html)]);
    assert_eq!(uploaded, raw);
}

#[test]
fn an_uploaded_stylesheet_and_image_are_used_by_their_bare_names() {
    let server = TestServer::start(&["--disable-system-fonts"]);
    let html = r#"<html><head><link rel="stylesheet" href="style.css"></head>
        <body><p>red text</p><img src="logo.png" width="20" height="20"></body></html>"#;

    let pdf = render_form(
        &server,
        "/pdf?no-pdf-compression",
        &[
            file("index.html", html),
            file("style.css", "p { color: #ff0000 }"),
            file("logo.png", fixture("spike_opaque.png")),
        ],
    );
    assert!(
        count_occurrences(&pdf, b"/Subtype /Image") > 0,
        "the uploaded image must be embedded"
    );
    // DeviceRGB red, as the uploaded stylesheet asks (PDF 32000-1 §8.6.8: `r g b rg`).
    assert!(
        count_occurrences(&pdf, b"1 0 0 rg") > 0,
        "the uploaded stylesheet must be applied"
    );

    // Without the files, the same HTML has neither.
    let without = render_form(
        &server,
        "/pdf?no-pdf-compression",
        &[file("index.html", html)],
    );
    assert_eq!(count_occurrences(&without, b"/Subtype /Image"), 0);
    assert_eq!(count_occurrences(&without, b"1 0 0 rg"), 0);
}

#[test]
fn a_folder_in_an_uploaded_name_is_dropped() {
    let server = TestServer::start(&[]);
    let pdf = render_form(
        &server,
        "/pdf",
        &[
            file(
                "index.html",
                r#"<body><img src="logo.png" width="20" height="20">"#,
            ),
            file("assets/img/logo.png", fixture("spike_opaque.png")),
        ],
    );
    assert!(count_occurrences(&pdf, b"/Subtype /Image") > 0);
}

#[test]
fn an_uploaded_font_can_be_used_through_font_face() {
    // The server is started with DejaVuSans; the document asks for an uploaded DejaVuSansMono.
    let server = TestServer::start(&["--disable-system-fonts"]);
    let html = r#"<html><head><style>
        @font-face { font-family: Uploaded; src: url(mono.ttf); }
        p { font-family: Uploaded; }
        </style></head><body><p>uploaded font</p></body></html>"#;
    let font = std::fs::read(MONO_FONT_PATH).unwrap();

    let pdf = render_form(
        &server,
        "/pdf",
        &[file("index.html", html), file("mono.ttf", font)],
    );
    assert!(
        count_occurrences(&pdf, b"DejaVuSansMono") > 0,
        "the uploaded font must be embedded"
    );
}

#[test]
fn options_can_be_sent_as_form_fields_and_in_the_query() {
    let server = TestServer::start(&[]);
    let html = "<p>x</p>";

    // `page-size` as a form field, `margin-top` in the query.
    let pdf = render_form(
        &server,
        "/pdf?margin-top=0&margin-bottom=0",
        &[option("page-size", "A5"), file("index.html", html)],
    );
    // A5 = 559.4 x 793.7 CSS px.
    assert!(count_occurrences(&pdf, media_box(559.4, 793.7).as_bytes()) > 0);

    // A form field is subject to the same allow list as the query.
    for (name, value) in [
        ("font", "/etc/passwd"),
        ("base-url", "/etc"),
        ("output", "x.pdf"),
    ] {
        let status = status_of(post_form(
            &server,
            "/pdf",
            &[option(name, value), file("index.html", html)],
        ));
        assert_eq!(status, 400, "{name} must be rejected as a form field");
    }
}

#[test]
fn an_uploaded_header_and_footer_are_used() {
    let server = TestServer::start(&[]);
    let pages = "<p>First</p><p style=\"break-before:page\">Second</p>";
    let query = "/pdf?creation-date=2026-01-01T00:00:00Z";

    for (upload_name, option_name) in [
        ("header.html", "header-html-content"),
        ("footer.html", "footer-html-content"),
    ] {
        let decoration = "<div>[page] of [topage]</div>";
        let uploaded = render_form(
            &server,
            query,
            &[file("index.html", pages), file(upload_name, decoration)],
        );
        // Uploading `header.html` means the same as `header-html-content` with its text.
        let inline = {
            let encoded = "%3Cdiv%3E%5Bpage%5D+of+%5Btopage%5D%3C%2Fdiv%3E";
            let mut response = ureq::post(server.url(&format!("{query}&{option_name}={encoded}")))
                .send(pages)
                .unwrap();
            response.body_mut().read_to_vec().unwrap()
        };
        assert_eq!(uploaded, inline, "{upload_name}");
    }
}

#[test]
fn an_uploaded_cover_is_used() {
    let server = TestServer::start(&[]);
    let with_cover = render_form(
        &server,
        "/pdf",
        &[
            file("index.html", "<p>body</p>"),
            file("cover.html", "<h1>cover</h1>"),
        ],
    );
    let without = render_form(&server, "/pdf", &[file("index.html", "<p>body</p>")]);
    assert_eq!(count_occurrences(&without, b"/MediaBox"), 1);
    assert_eq!(count_occurrences(&with_cover, b"/MediaBox"), 2);
}

#[test]
fn an_uploaded_user_style_sheet_is_weaker_than_the_documents_own_css() {
    // `--user-style-sheet` is user-origin CSS: stronger than the built-in defaults and weaker
    // than the author's CSS (the CLI reference). So it styles what the document leaves
    // alone, and the document's own rule wins where both speak.
    let server = TestServer::start(&["--disable-system-fonts"]);
    let user_css = "p { color: #ff0000 }";
    let query = "/pdf?no-pdf-compression";

    let silent = render_form(
        &server,
        query,
        &[
            file("index.html", "<p>x</p>"),
            file("user-style-sheet.css", user_css),
        ],
    );
    assert!(
        count_occurrences(&silent, b"1 0 0 rg") > 0,
        "user CSS applies"
    );

    let author = "<style>p { color: #00ff00 }</style><p>x</p>";
    let overridden = render_form(
        &server,
        query,
        &[
            file("index.html", author),
            file("user-style-sheet.css", user_css),
        ],
    );
    assert!(count_occurrences(&overridden, b"0 1 0 rg") > 0);
    assert_eq!(
        count_occurrences(&overridden, b"1 0 0 rg"),
        0,
        "the author's CSS must win over user CSS"
    );

    // Without the file, the document is black.
    let plain = render_form(&server, query, &[file("index.html", "<p>x</p>")]);
    assert_eq!(count_occurrences(&plain, b"1 0 0 rg"), 0);
}

#[test]
fn an_upload_cannot_read_files_outside_its_own_directory() {
    let server = TestServer::start(&[]);
    // A real image next to the upload directories, and another at an absolute path.
    let sibling =
        std::env::temp_dir().join(format!("sghtmltopdf-secret-{}.png", std::process::id()));
    std::fs::write(&sibling, fixture("spike_opaque.png")).unwrap();
    let absolute = format!("{IMAGES}/spike_opaque.png").replace('\\', "/");

    let html = format!(
        r#"<body>
        <img src="../{name}" width="20" height="20">
        <img src="{absolute}" width="20" height="20">
        <img src="file:///{absolute}" width="20" height="20">
        </body>"#,
        name = sibling.file_name().unwrap().to_string_lossy()
    );
    let pdf = render_form(&server, "/pdf", &[file("index.html", html)]);
    let _ = std::fs::remove_file(&sibling);

    assert_eq!(
        count_occurrences(&pdf, b"/Subtype /Image"),
        0,
        "no file outside the uploaded ones may be embedded"
    );
}

#[test]
fn an_upload_does_not_widen_what_the_operator_allowed_remotely() {
    // Remote fetching stays off without --allow-remote-assets, also for uploads.
    let server = TestServer::start(&[]);
    let pdf = render_form(
        &server,
        "/pdf",
        &[file(
            "index.html",
            r#"<body><img src="http://127.0.0.1:1/x.png"><p>x</p></body>"#,
        )],
    );
    assert!(pdf.starts_with(b"%PDF-"));
    assert_eq!(count_occurrences(&pdf, b"/Subtype /Image"), 0);
}

#[test]
fn a_bad_upload_is_a_client_error() {
    let server = TestServer::start(&[]);
    let html = "<p>x</p>";

    // No `index.html`.
    assert_eq!(
        status_of(post_form(&server, "/pdf", &[file("other.html", html)])),
        400
    );
    // `index.html` empty.
    assert_eq!(
        status_of(post_form(&server, "/pdf", &[file("index.html", "")])),
        400
    );
    // A file in a part that is not named `files`.
    let misnamed = FormPart {
        name: "document",
        file_name: Some("index.html"),
        data: html.as_bytes().to_vec(),
    };
    assert_eq!(status_of(post_form(&server, "/pdf", &[misnamed])), 400);
    // A name that names no file, and a name that is a Windows device.
    for bad in ["..", "con.png", "a:b.png"] {
        assert_eq!(
            status_of(post_form(
                &server,
                "/pdf",
                &[file("index.html", html), file(bad, "x")]
            )),
            400,
            "{bad}"
        );
    }
    // The same file twice.
    assert_eq!(
        status_of(post_form(
            &server,
            "/pdf",
            &[
                file("index.html", html),
                file("a.png", "1"),
                file("A.png", "2")
            ]
        )),
        400
    );
    // A body that is not multipart at all, under a multipart Content-Type.
    let status = status_of(
        ureq::post(server.url("/pdf"))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={BOUNDARY}"),
            )
            .send("this is not multipart"),
    );
    assert_eq!(status, 400);
    // No boundary parameter.
    let status = status_of(
        ureq::post(server.url("/pdf"))
            .header("Content-Type", "multipart/form-data")
            .send("x"),
    );
    assert_eq!(status, 400);
}

#[test]
fn another_multipart_type_is_unsupported_media() {
    let server = TestServer::start(&[]);
    let status = status_of(
        ureq::post(server.url("/pdf"))
            .header("Content-Type", "multipart/mixed; boundary=x")
            .send("--x--"),
    );
    assert_eq!(status, 415);
}

#[test]
fn other_content_types_are_still_raw_html() {
    let server = TestServer::start(&[]);
    // `curl --data-binary` sends the first of these by default.
    for content_type in [
        "application/x-www-form-urlencoded",
        "text/html; charset=utf-8",
    ] {
        let mut response = ureq::post(server.url("/pdf"))
            .header("Content-Type", content_type)
            .send(HTML)
            .unwrap();
        assert!(response
            .body_mut()
            .read_to_vec()
            .unwrap()
            .starts_with(b"%PDF-"));
    }
}

#[test]
fn the_body_size_limit_covers_the_whole_upload() {
    let server = TestServer::start(&["--max-body-size", "2048"]);
    let big = "x".repeat(4096);
    let status = status_of(post_form(
        &server,
        "/pdf",
        &[file("index.html", "<p>x</p>"), file("big.bin", big)],
    ));
    assert_eq!(status, 413);
}

#[test]
fn uploads_work_with_a_streamed_response_too() {
    let server = TestServer::start(&[]);
    let html = r#"<body><img src="logo.png" width="20" height="20"></body>"#;
    let parts = [
        file("index.html", html),
        file("logo.png", fixture("spike_opaque.png")),
    ];

    let mut response = post_form(&server, "/pdf?stream=1", &parts).unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(
        response.headers().get("content-length").is_none(),
        "the response must be chunked"
    );
    let pdf = response.body_mut().read_to_vec().unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(count_occurrences(&pdf, b"/Subtype /Image") > 0);

    // A bad upload is still reported with its status before the body starts.
    assert_eq!(
        status_of(post_form(
            &server,
            "/pdf?stream=1",
            &[file("other.html", "x")]
        )),
        400
    );
}

#[test]
fn uploaded_files_are_removed_after_each_request() {
    let server = TestServer::start(&[]);
    let parts = [
        file("index.html", r#"<body><img src="logo.png"></body>"#),
        file("logo.png", fixture("spike_opaque.png")),
    ];

    render_form(&server, "/pdf", &parts);
    // A refused upload leaves nothing either.
    let _ = post_form(&server, "/pdf", &[file("other.html", "x")]);
    // A render that fails after the files were stored.
    let _ = post_form(
        &server,
        "/pdf?streaming",
        &[file(
            "index.html",
            "<html><head><style>body{background:red}</style></head><body>x</body></html>",
        )],
    );
    assert_eq!(leftover_upload_dirs(&server), 0);

    // The streamed response renders on its own thread; its files go when it is done.
    let mut response = post_form(&server, "/pdf?stream=1", &parts).unwrap();
    response.body_mut().read_to_vec().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while leftover_upload_dirs(&server) > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(leftover_upload_dirs(&server), 0);
}

#[test]
fn concurrent_uploads_do_not_see_each_others_files() {
    let server = TestServer::start(&["--workers", "4"]);
    let results: Vec<(usize, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let server = &server;
                scope.spawn(move || {
                    // Half of the requests send a `logo.png`, the others none at all.
                    let html = r#"<body><img src="logo.png" width="20" height="20"></body>"#;
                    let mut parts = vec![file("index.html", html)];
                    if i % 2 == 0 {
                        parts.push(file("logo.png", fixture("spike_opaque.png")));
                    }
                    let pdf = render_form(server, "/pdf", &parts);
                    (i, count_occurrences(&pdf, b"/Subtype /Image"))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    for (i, images) in results {
        if i % 2 == 0 {
            assert!(images > 0, "request {i} sent its image");
        } else {
            assert_eq!(images, 0, "request {i} sent no image");
        }
    }
}
