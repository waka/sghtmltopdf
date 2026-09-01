//! Implementation of the CLI (the `sghtmltopdf` binary).
//!
//! `main.rs` is a thin entry point that only calls [`run`]; option definitions live in
//! one place, [`options`] (the HTTP server mode uses the same definitions).

pub mod convert;
pub mod header_footer;
pub mod options;
/// HTTP server mode. Only available with the `server` feature (on by default).
#[cfg(feature = "server")]
pub mod server;
pub mod toc;
pub mod units;
pub mod unsupported;

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches};

use crate::render_stack::with_render_stack;
use options::Cli;
#[cfg(feature = "server")]
use options::Command;

/// CLI errors. Each variant maps directly to an exit code.
#[derive(Debug)]
pub enum CliError {
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

impl CliError {
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

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for CliError {}

/// Parse an argument list into [`options::ConvertArgs`] plus the font specifications.
///
/// This lets entry points other than the CLI (the Ruby binding) share the same option handling.
/// Callers must put the program name in `argv[0]`, following clap's convention.
///
/// Pairing up `--font` and `--font-index` needs the occurrence positions from
/// [`clap::ArgMatches`], so that is resolved here and returned as a list of
/// [`options::FontArg`] (which keeps callers free of a clap dependency).
pub fn parse_convert_argv(
    argv: &[String],
) -> Result<(options::ConvertArgs, Vec<options::FontArg>), CliError> {
    // For unsupported options, report the reason rather than clap's "unknown argument".
    // Same handling as the CLI's `run`.
    if let Some(message) = unsupported::check_arguments(&argv[1..]) {
        return Err(CliError::Usage(message));
    }

    let matches = Cli::command()
        .try_get_matches_from(argv)
        .map_err(|e| CliError::Usage(e.to_string()))?;
    let cli = Cli::from_arg_matches(&matches).map_err(|e| CliError::Usage(e.to_string()))?;
    let fonts = cli.convert.font_specs(&matches).map_err(CliError::Usage)?;
    Ok((cli.convert, fonts))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        let mut argv = vec!["sghtmltopdf".to_string(), "-".to_string()];
        argv.extend(["--output".to_string(), "-".to_string()]);
        argv.extend(args.iter().map(|s| s.to_string()));
        argv
    }

    #[test]
    fn parse_convert_argv_binds_each_font_index_to_the_preceding_font() {
        let (_, fonts) = parse_convert_argv(&argv(&[
            "--font",
            "a.ttf",
            "--font",
            "b.ttc",
            "--font-index",
            "2",
        ]))
        .expect("parse should succeed");

        assert_eq!(fonts.len(), 2);
        assert_eq!(fonts[0].index, 0);
        assert_eq!(fonts[1].index, 2);
    }

    #[test]
    fn parse_convert_argv_rejects_unsupported_options_with_a_reason() {
        let error = parse_convert_argv(&argv(&["--enable-javascript"]))
            .expect_err("unsupported option should be rejected");

        match error {
            CliError::Usage(message) => assert!(message.contains("is not supported")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn parse_convert_argv_rejects_unknown_options() {
        assert!(matches!(
            parse_convert_argv(&argv(&["--no-such-option"])),
            Err(CliError::Usage(_))
        ));
    }
}
