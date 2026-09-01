//! The `server` subcommand (HTTP server mode).
//!
//! * `POST /pdf?<the same option names as the CLI>` with raw HTML as the body
//! * Only the keys listed in [`ALLOWED_QUERY_KEYS`] may appear in the query

use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{CommandFactory, FromArgMatches};
use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::sink::{MemorySink, Sink};

use super::options::{Cli, ConvertArgs, ServerArgs};
use super::CliError;

/// Options allowed in the query string.
const ALLOWED_QUERY_KEYS: &[&str] = &[
    // Page setup
    "page-size",
    "page-width",
    "page-height",
    "orientation",
    "margin-top",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "zoom",
    "dpi",
    "page-offset",
    "minimum-font-size",
    // Output appearance
    "grayscale",
    "no-background",
    "no-images",
    "no-pdf-compression",
    // PDF metadata
    "title",
    "author",
    "subject",
    "keywords",
    // Header/footer (text and its styling only; HTML files cannot be specified)
    "default-header",
    "header-left",
    "header-center",
    "header-right",
    "header-font-name",
    "header-font-size",
    "header-spacing",
    "header-line",
    "footer-left",
    "footer-center",
    "footer-right",
    "footer-font-name",
    "footer-font-size",
    "footer-spacing",
    "footer-line",
    "replace",
    // Table of contents
    "toc",
    "toc-header-text",
    "toc-level-indentation",
    "toc-text-size-shrink",
    "disable-dotted-lines",
    "disable-toc-links",
    "enable-toc-back-links",
    // Links
    "disable-external-links",
    "disable-internal-links",
    "keep-relative-links",
    // How input is interpreted, and what to do on failure
    "encoding",
    "load-error-handling",
    "load-media-error-handling",
    "streaming",
];

/// Options that can only be set when the server starts.
const SERVER_ONLY_KEYS: &[&str] = &[
    "font",
    "font-index",
    "gothic-font",
    "gothic-font-index",
    "serif-font",
    "serif-font-index",
    "mono-font",
    "mono-font-index",
    "output",
    "cover",
    "header-html",
    "footer-html",
    "user-style-sheet",
    "base-url",
    "allow",
    "enable-local-file-access",
    "disable-local-file-access",
    "allow-remote-assets",
    "log-level",
    "quiet",
];

pub fn run(args: &ServerArgs) -> Result<(), CliError> {
    let workers = args
        .workers
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()))
        .max(1);
    let max_queue = args.max_queue.unwrap_or(workers * 4).max(1);

    let server = Server::http(&args.listen)
        .map_err(|e| CliError::Input(format!("cannot listen on {}: {e}", args.listen)))?;
    let addr = server
        .server_addr()
        .to_ip()
        .map(|a| a.to_string())
        .unwrap_or_else(|| args.listen.clone());
    println!("listening on {addr}");

    let server = Arc::new(server);
    let shared = Arc::new(ServerContext {
        args: args.clone(),
        max_body_size: args.max_body_size,
    });

    // Count pending requests as the channel length, to decide when the queue overflows.
    let (tx, rx) = mpsc::sync_channel::<(Request, Instant)>(max_queue);
    let rx = Arc::new(std::sync::Mutex::new(rx));

    let mut handles = Vec::with_capacity(workers);
    for index in 0..workers {
        let rx = Arc::clone(&rx);
        let shared = Arc::clone(&shared);
        let timeout = Duration::from_secs(args.timeout);
        // The default 2MiB is not enough for the recursion in layout and drawing, so allocate
        // explicitly (see the docs on `crate::render_stack::STACK_SIZE`).
        let worker = std::thread::Builder::new()
            .name(format!("render-{index}"))
            .stack_size(crate::render_stack::STACK_SIZE);
        let handle = worker.spawn(move || loop {
            let next = {
                let guard = rx.lock().expect("failed to lock the receive queue");
                guard.recv()
            };
            let Ok((request, queued_at)) = next else {
                break; // the sending side closed: shut down
            };
            if queued_at.elapsed() > timeout {
                let _ = respond_text(
                    request,
                    504,
                    "waited in the queue for longer than --timeout",
                );
                continue;
            }
            // Whatever time is left becomes the rendering deadline
            handle_request(request, &shared, queued_at + timeout);
        });
        handles.push(
            handle.map_err(|e| CliError::Input(format!("cannot create a worker thread: {e}")))?,
        );
    }

    for request in server.incoming_requests() {
        match tx.try_send((request, Instant::now())) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full((request, _))) => {
                let _ = respond_text(request, 503, "busy (--max-queue exceeded)");
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break,
        }
    }

    drop(tx);
    for handle in handles {
        let _ = handle.join();
    }
    Ok(())
}

struct ServerContext {
    args: ServerArgs,
    max_body_size: usize,
}

fn handle_request(mut request: Request, ctx: &ServerContext, deadline: Instant) {
    let url = request.url().to_string();
    let (path, query) = match url.split_once('?') {
        Some((path, query)) => (path.to_string(), query.to_string()),
        None => (url, String::new()),
    };
    let method = request.method().as_str().to_string();

    match (method.as_str(), path.as_str()) {
        ("GET", "/healthz") => {
            let _ = respond_text(request, 200, "ok");
        }
        ("GET", "/version") => {
            let _ = respond_text(
                request,
                200,
                &format!("sghtmltopdf {}", env!("CARGO_PKG_VERSION")),
            );
        }
        ("POST", "/pdf") if wants_chunked(&query) => {
            if let Err((status, message)) = respond_chunked(request, &query, ctx, deadline) {
                eprintln!("error: {status} {message}");
            }
        }
        ("POST", "/pdf") => match render_request(&mut request, &query, ctx, deadline) {
            Ok(pdf) => {
                let header = Header::from_bytes(&b"Content-Type"[..], &b"application/pdf"[..])
                    .expect("a fixed header value always builds");
                let response = Response::from_data(pdf).with_header(header);
                let _ = request.respond(response);
            }
            Err((status, message)) => {
                let _ = respond_text(request, status, &message);
            }
        },
        (_, "/pdf") | (_, "/healthz") | (_, "/version") => {
            let _ = respond_text(request, 405, "method not allowed on this path");
        }
        _ => {
            let _ = respond_text(request, 404, "not found");
        }
    }
}

/// Whether `?stream=1` (streaming the response with chunked encoding) was requested.
fn wants_chunked(query: &str) -> bool {
    parse_query(query)
        .map(|pairs| {
            pairs.iter().any(|(key, value)| {
                key == "stream" && value.as_deref().map(is_true).unwrap_or(true)
            })
        })
        .unwrap_or(false)
}

/// A Sink writing into a `std::io::PipeWriter`, joining the rendering side (push) to the HTTP response side (pull).
struct PipeSink(std::io::PipeWriter);

impl Sink for PipeSink {
    type Output = ();
    type Error = std::io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.0.write_all(bytes)
    }

    fn finish(mut self) -> Result<Self::Output, Self::Error> {
        self.0.flush()
    }
}

/// The response for `?stream=1`: stream each page out with chunked encoding as soon as it is final.
fn respond_chunked(
    mut request: Request,
    query: &str,
    ctx: &ServerContext,
    deadline: Instant,
) -> Result<(), (u16, String)> {
    let too_large = || {
        (
            413,
            format!("the body exceeds the limit of {} bytes", ctx.max_body_size),
        )
    };
    if let Some(len) = request.body_length() {
        if len > ctx.max_body_size {
            let _ = respond_text(request, 413, &too_large().1);
            return Ok(());
        }
    }

    let stripped = strip_stream_key(query);
    let args = match build_convert_args(&stripped, &ctx.args).map(|mut a| {
        a.deadline = Some(deadline);
        a
    }) {
        Ok(args) => args,
        Err(message) => {
            let _ = respond_text(request, 400, &message);
            return Ok(());
        }
    };
    let fonts = ctx.args.font_specs();

    let mut html = Vec::new();
    let read = request
        .as_reader()
        .take(ctx.max_body_size as u64 + 1)
        .read_to_end(&mut html);
    match read {
        Ok(_) if html.len() > ctx.max_body_size => {
            let _ = respond_text(request, 413, &too_large().1);
            return Ok(());
        }
        Ok(_) if html.is_empty() => {
            let _ = respond_text(request, 400, "put the HTML in the request body");
            return Ok(());
        }
        Ok(_) => {}
        Err(e) => {
            let _ = respond_text(request, 400, &format!("failed to read the body: {e}"));
            return Ok(());
        }
    }

    let (pipe_reader, pipe_writer) = match std::io::pipe() {
        Ok(pair) => pair,
        Err(e) => {
            let _ = respond_text(request, 500, &format!("failed to create a pipe: {e}"));
            return Ok(());
        }
    };

    // Rendering runs on its own thread and writes into the pipe as it goes.
    // This thread returns the read end of the pipe as the response body.
    // As with the workers, allocate a stack explicitly so the recursion fits.
    let spawned = std::thread::Builder::new()
        .name("render-stream".to_string())
        .stack_size(crate::render_stack::STACK_SIZE)
        .spawn(move || {
            if let Err(e) = super::convert::render(
                &args,
                &fonts,
                std::io::Cursor::new(html),
                PipeSink(pipe_writer),
            ) {
                // The headers have already been sent, so all we can do here is log.
                eprintln!("error: the streamed response failed part-way through: {e}");
            }
        });
    if let Err(e) = spawned {
        let _ = respond_text(
            request,
            500,
            &format!("failed to create the rendering thread: {e}"),
        );
        return Ok(());
    }

    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/pdf"[..])
        .expect("a fixed header value always builds");
    // Setting `data_length` to `None` selects chunked transfer encoding.
    let response = Response::new(StatusCode(200), vec![header], pipe_reader, None, None);
    let _ = request.respond(response);
    Ok(())
}

/// The query string with only the `stream` key removed (it is not passed on to option parsing).
fn strip_stream_key(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| {
            let key = pair.split('=').next().unwrap_or(pair);
            key != "stream"
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// A `Read` wrapper that counts the body against a size limit as it is read.
///
/// It returns an error the moment the limit is passed and sets `exceeded`. The caller
/// checks that flag to return 413 (rather than depending on the engine's error text).
struct LimitedReader<R> {
    inner: R,
    remaining: usize,
    read_any: bool,
    exceeded: bool,
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            // Reading even one more byte means the limit was exceeded.
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe)? > 0 {
                self.exceeded = true;
                return Err(std::io::Error::other("body too large"));
            }
            return Ok(0);
        }
        let limit = buf.len().min(self.remaining);
        let read = self.inner.read(&mut buf[..limit])?;
        self.remaining -= read;
        if read > 0 {
            self.read_any = true;
        }
        Ok(read)
    }
}

/// Conversion of a single request. Errors come back as (status, message).
///
/// The body is streamed into `Engine::feed` rather than read to the end.
fn render_request(
    request: &mut Request,
    query: &str,
    ctx: &ServerContext,
    deadline: Instant,
) -> Result<Vec<u8>, (u16, String)> {
    let too_large = || {
        (
            413,
            format!("the body exceeds the limit of {} bytes", ctx.max_body_size),
        )
    };

    // If the body length is known, reject before reading anything.
    if let Some(len) = request.body_length() {
        if len > ctx.max_body_size {
            return Err(too_large());
        }
    }

    // Parse the query before reading the body (a bad query is a 400 with nothing read).
    let mut args = build_convert_args(query, &ctx.args).map_err(|e| (400, e))?;
    args.deadline = Some(deadline);
    let fonts = ctx.args.font_specs();

    let mut reader = LimitedReader {
        inner: request.as_reader(),
        remaining: ctx.max_body_size,
        read_any: false,
        exceeded: false,
    };

    let sink = MemorySink::new();
    let result = super::convert::render_to_memory(&args, &fonts, &mut reader, sink);

    if reader.exceeded {
        return Err(too_large());
    }
    if !reader.read_any {
        return Err((400, "put the HTML in the request body".to_string()));
    }

    result.map_err(|e| match e {
        CliError::Usage(msg) => (400, msg),
        CliError::Input(msg) => (400, msg),
        CliError::Render(msg) => (500, msg),
        CliError::Timeout(msg) => (504, msg),
    })
}

/// Turn the query string into a CLI argument list and run it through the same clap parser.
fn build_convert_args(query: &str, server: &ServerArgs) -> Result<ConvertArgs, String> {
    let mut argv: Vec<String> = vec!["sghtmltopdf".to_string()];
    // The input is the body, so the positional argument is `-` for stdin (never actually read).
    argv.push("-".to_string());
    argv.push("--output".to_string());
    argv.push("-".to_string());

    // Values fixed at server startup (a request cannot change these).
    for path in &server.font {
        argv.push("--font".to_string());
        argv.push(path.display().to_string());
    }
    for (flag, path) in [
        ("--gothic-font", server.gothic_font.as_ref()),
        ("--serif-font", server.serif_font.as_ref()),
        ("--mono-font", server.mono_font.as_ref()),
    ] {
        if let Some(path) = path {
            argv.push(flag.to_string());
            argv.push(path.display().to_string());
        }
    }
    if !server.enable_local_file_access {
        argv.push("--disable-local-file-access".to_string());
    }
    for dir in &server.allow {
        argv.push("--allow".to_string());
        argv.push(dir.display().to_string());
    }
    if server.allow_remote_assets {
        argv.push("--allow-remote-assets".to_string());
    }
    argv.push("--quiet".to_string());

    for (key, value) in parse_query(query)? {
        // Unsupported options come back with the same reason as in the CLI.
        if let Some(reason) = super::unsupported::unsupported_reason(&format!("--{key}")) {
            return Err(format!("{key} is not supported. {reason}"));
        }
        if SERVER_ONLY_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "{key} cannot be set per request (set it with a server startup option)"
            ));
        }
        if !ALLOWED_QUERY_KEYS.contains(&key.as_str()) {
            return Err(format!("{key} cannot be set in a request"));
        }
        match value {
            // No value, or a value meaning true, is passed as a flag.
            None => argv.push(format!("--{key}")),
            Some(v) if is_true(&v) => argv.push(format!("--{key}")),
            // A value meaning false is the same as not specifying the option.
            Some(v) if is_false(&v) => {}
            // A value is always folded into a single `--key=value` token. Pushing `--key`
            // and the value as separate tokens would let a value such as
            // `--allow-remote-assets` be read by clap as an independent flag, slipping
            // past DENIED_QUERY_KEYS (which only inspects the key).
            Some(v) => argv.push(format!("--{key}={v}")),
        }
    }

    let matches = Cli::command()
        .try_get_matches_from(&argv)
        .map_err(|e| e.to_string())?;
    let cli = Cli::from_arg_matches(&matches).map_err(|e| e.to_string())?;
    Ok(cli.convert)
}

fn is_true(value: &str) -> bool {
    matches!(value, "" | "1" | "true" | "yes" | "on")
}

fn is_false(value: &str) -> bool {
    matches!(value, "0" | "false" | "no" | "off")
}

/// Split `a=1&b&c=%E6%97%A5` into keys and values (percent-decoding included).
fn parse_query(query: &str) -> Result<Vec<(String, Option<String>)>, String> {
    let mut out = Vec::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, Some(value)),
            None => (pair, None),
        };
        let key = percent_decode(key)?;
        if key.is_empty() {
            continue;
        }
        let value = match value {
            Some(value) => Some(percent_decode(value)?),
            None => None,
        };
        out.push((key, value));
    }
    Ok(out)
}

/// Decode `%XX` and `+`.
fn percent_decode(text: &str) -> Result<String, String> {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .ok_or_else(|| format!("broken URL encoding: {text}"))?;
                let hex =
                    std::str::from_utf8(hex).map_err(|_| format!("broken URL encoding: {text}"))?;
                let byte = u8::from_str_radix(hex, 16)
                    .map_err(|_| format!("broken URL encoding: {text}"))?;
                out.push(byte);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| format!("cannot be read as UTF-8: {text}"))
}

fn respond_text(request: Request, status: u16, message: &str) -> std::io::Result<()> {
    let header = Header::from_bytes(&b"Content-Type"[..], &b"text/plain; charset=utf-8"[..])
        .expect("a fixed header value always builds");
    let response = Response::from_string(message)
        .with_status_code(StatusCode(status))
        .with_header(header);
    request.respond(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_args() -> ServerArgs {
        ServerArgs {
            listen: "127.0.0.1:0".to_string(),
            workers: Some(1),
            max_queue: Some(1),
            max_body_size: 1024,
            timeout: 30,
            font: vec![std::path::PathBuf::from("/tmp/a.ttf")],
            gothic_font: None,
            serif_font: None,
            mono_font: None,
            enable_local_file_access: false,
            allow: Vec::new(),
            allow_remote_assets: false,
        }
    }

    #[test]
    fn query_pairs_are_percent_decoded() {
        let pairs = parse_query("a=1&b&c=%E6%97%A5%E6%9C%AC&d=x+y").unwrap();
        assert_eq!(
            pairs,
            vec![
                ("a".to_string(), Some("1".to_string())),
                ("b".to_string(), None),
                ("c".to_string(), Some("日本".to_string())),
                ("d".to_string(), Some("x y".to_string())),
            ]
        );
    }

    #[test]
    fn a_broken_escape_is_an_error() {
        assert!(parse_query("a=%zz").is_err());
        assert!(parse_query("a=%4").is_err());
    }

    #[test]
    fn query_options_reach_the_same_parser_as_the_cli() {
        let args = build_convert_args("page-size=A5&margin-top=20mm&toc", &server_args()).unwrap();
        let settings = args.page_settings().unwrap();
        assert_eq!(settings.size, crate::layout::PageSize::A5);
        assert!((settings.margin.top - 75.59).abs() < 0.1);
        assert!(args.toc);
    }

    #[test]
    fn boolean_values_are_understood() {
        let truthy = build_convert_args("grayscale=1&no-images=true", &server_args()).unwrap();
        assert!(truthy.grayscale);
        assert!(truthy.no_images);

        let falsy = build_convert_args("grayscale=0&no-images=false", &server_args()).unwrap();
        assert!(!falsy.grayscale);
        assert!(!falsy.no_images);
    }

    #[test]
    fn local_access_is_disabled_unless_the_server_enabled_it() {
        let args = build_convert_args("", &server_args()).unwrap();
        assert!(args.disable_local_file_access);
        assert!(!args.allow_remote_assets);

        let mut server = server_args();
        server.enable_local_file_access = true;
        server.allow_remote_assets = true;
        let args = build_convert_args("", &server).unwrap();
        assert!(!args.disable_local_file_access);
        assert!(args.allow_remote_assets);
    }

    #[test]
    fn denied_keys_are_rejected() {
        for key in [
            "font=/etc/passwd",
            "cover=/etc/passwd",
            "base-url=/etc",
            "output=/tmp/x.pdf",
        ] {
            let err = build_convert_args(key, &server_args()).unwrap_err();
            assert!(err.contains("cannot be set"), "got: {err}");
        }
    }

    #[test]
    fn an_unknown_option_is_an_error() {
        assert!(build_convert_args("no-such-option=1", &server_args()).is_err());
    }

    /// Every option must be classified either as "allowed in the query" or
    /// "server startup only".
    #[test]
    fn every_option_is_classified_as_allowed_or_server_only() {
        let command = Cli::command();
        let unclassified: Vec<&str> = command
            .get_arguments()
            .filter_map(|arg| arg.get_long())
            // Options clap adds automatically are outside the classification.
            .filter(|long| !matches!(*long, "help" | "version"))
            .filter(|long| !ALLOWED_QUERY_KEYS.contains(long) && !SERVER_ONLY_KEYS.contains(long))
            .collect();

        assert!(
            unclassified.is_empty(),
            "these options are unclassified: {unclassified:?}\n\
             add each to either ALLOWED_QUERY_KEYS (changeable per request)\n\
             or SERVER_ONLY_KEYS (server startup only)"
        );
    }

    /// The classification lists must not name options that no longer exist
    /// (this catches renamed or removed options).
    #[test]
    fn the_classification_lists_only_name_real_options() {
        let command = Cli::command();
        let known: Vec<&str> = command
            .get_arguments()
            .filter_map(|a| a.get_long())
            .collect();

        let stale: Vec<&&str> = ALLOWED_QUERY_KEYS
            .iter()
            .chain(SERVER_ONLY_KEYS)
            .filter(|key| !known.contains(key))
            .collect();

        assert!(
            stale.is_empty(),
            "these option names no longer exist: {stale:?}"
        );
    }

    /// The two lists must be disjoint (appearing in both makes the intent unreadable).
    #[test]
    fn the_two_classification_lists_do_not_overlap() {
        let both: Vec<&&str> = ALLOWED_QUERY_KEYS
            .iter()
            .filter(|key| SERVER_ONLY_KEYS.contains(key))
            .collect();
        assert!(both.is_empty(), "listed in both lists: {both:?}");
    }

    /// An option outside the allowlist is refused, however harmless it looks.
    #[test]
    fn an_option_outside_the_allowlist_is_refused() {
        // `--base-url` is in SERVER_ONLY_KEYS, so it gets its own reason.
        let err = build_convert_args("base-url=/etc", &server_args()).unwrap_err();
        assert!(err.contains("server startup"), "got: {err}");
    }

    /// Writing another flag into an option's value must not make it a separate argument.
    /// This used to be pushed as the two tokens `--toc` and `--allow-remote-assets`,
    /// which slipped past DENIED_QUERY_KEYS because that only inspects the key.
    #[test]
    fn a_flag_smuggled_through_a_value_does_not_reach_the_parser_as_a_flag() {
        let args = build_convert_args("toc=--allow-remote-assets", &server_args());
        match args {
            // `--toc=...` is a flag that takes no value, so clap is right to reject it.
            Err(message) => assert!(
                !message.contains("unexpected argument"),
                "a value must not be read as a flag: {message}"
            ),
            Ok(args) => assert!(
                !args.allow_remote_assets,
                "remote fetching must not be enabled through a query value"
            ),
        }
    }

    /// Injection through a value must not enable local file access either.
    #[test]
    fn local_file_access_cannot_be_smuggled_through_a_value_either() {
        for query in [
            "toc=--enable-local-file-access",
            "grayscale=--enable-local-file-access",
        ] {
            match build_convert_args(query, &server_args()) {
                Err(_) => {}
                Ok(args) => assert!(
                    args.disable_local_file_access,
                    "{query} must not enable local file access"
                ),
            }
        }
    }

    /// A legitimate value containing `=` or whitespace must survive being folded into one token.
    #[test]
    fn a_value_containing_an_equals_sign_still_reaches_the_option_intact() {
        let args = build_convert_args("title=a%3Db+c", &server_args()).unwrap();
        assert_eq!(args.title.as_deref(), Some("a=b c"));
    }
}
