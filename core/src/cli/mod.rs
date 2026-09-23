//! Implementation of the CLI (the `sghtmltopdf` binary).
//!
//! `main.rs` is a thin entry point that only calls [`run`]; option definitions live in
//! one place, [`options`] (the HTTP server mode uses the same definitions).

pub mod convert;
mod converter;
pub mod header_footer;
pub mod options;
/// HTTP server mode. Only available with the `server` feature (on by default).
#[cfg(feature = "server")]
pub mod server;
pub mod toc;
pub mod units;
pub mod unsupported;

use std::process::ExitCode;

pub use converter::Converter;

use clap::{CommandFactory, FromArgMatches};

use crate::render_stack::with_render_stack;
use options::Cli;
#[cfg(feature = "server")]
use options::Command;

/// Errors from the command and from [`Converter`], classified by cause.
///
/// The command exits with the code noted on each variant. Bindings map the variants to their
/// own error types, so the classification is part of the public API; the messages are not.
///
/// Available with the `cli` feature (on by default).
#[derive(Debug)]
#[non_exhaustive]
pub enum ConvertError {
    /// Usage error (unknown option, malformed value, unsupported option) = 1
    Usage(String),
    /// Input or resource error (missing file, unreadable font, failed write) = 2
    Input(String),
    /// Rendering error (an engine limit was exceeded, etc.) = 3
    Render(String),
    /// Aborted after exceeding the time limit = 4
    ///
    /// Only the HTTP server mode sets a deadline (`--timeout`), so the CLI does not
    /// currently produce this exit code.
    Timeout(String),
}

impl ConvertError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 1,
            Self::Input(_) => 2,
            Self::Render(_) => 3,
            Self::Timeout(_) => 4,
        }
    }

    fn message(&self) -> &str {
        match self {
            Self::Usage(m) | Self::Input(m) | Self::Render(m) | Self::Timeout(m) => m,
        }
    }
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for ConvertError {}

/// CLI entry point.
pub fn run() -> ExitCode {
    // For options wkhtmltopdf has but we do not support, exit with a reason and an
    // alternative rather than clap's "unknown argument".
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(message) = unsupported::check_arguments(&args) {
        eprintln!("error: {message}");
        return ExitCode::from(1);
    }

    // clap uses exit code 2 for argument errors by default, but this CLI assigns 1 to
    // usage errors, so we convert to an ExitCode ourselves.
    let matches = match Cli::command().try_get_matches() {
        Ok(matches) => matches,
        Err(e) => {
            let _ = e.print();
            // --help/--version are the success path (use_stderr() == false).
            return if e.use_stderr() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            };
        }
    };

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => {
            let _ = e.print();
            return ExitCode::from(1);
        }
    };

    // Conversion recurses as deep as the DOM during layout and drawing, so rather than
    // relying on the default stack (which depends on `ulimit -s`) we run it on a thread
    // with [`STACK_SIZE`]. Server mode does the same per worker, so it is not wrapped here.
    #[cfg(feature = "server")]
    let result = match cli.command {
        Some(Command::Server(ref args)) => server::run(args),
        None => with_render_stack(|| convert::run(&cli.convert, &matches)),
    };
    #[cfg(not(feature = "server"))]
    let result = with_render_stack(|| convert::run(&cli.convert, &matches));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(e.exit_code())
        }
    }
}
