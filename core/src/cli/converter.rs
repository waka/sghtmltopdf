//! [`Converter`]: conversion driven by the CLI's option list, for language bindings.

use std::io::{self, Read};

use clap::{CommandFactory, FromArgMatches};

use crate::sink::{MemorySink, Sink};

use super::options::{Cli, ConvertArgs, FontArg};
use super::{convert, unsupported, ConvertError};

/// A conversion configured by the same options as the `sghtmltopdf` command.
///
/// This is the entry point for language bindings: they pass the options through as a list of
/// strings instead of assembling [`EngineOptions`](crate::EngineOptions) field by field, and
/// every option the command accepts is available with the same meaning and the same errors.
///
/// ```no_run
/// use sghtmltopdf::Converter;
///
/// let converter = Converter::from_args(["--page-size", "Letter", "--title", "Report"])?;
/// let pdf = converter.render_to_vec(&b"<!DOCTYPE html><p>Hello</p>"[..])?;
/// # Ok::<(), sghtmltopdf::ConvertError>(())
/// ```
///
/// The options are validated once by [`from_args`](Self::from_args), and the same
/// `Converter` can then render any number of documents. It is `Send`, so it can be built on
/// one thread and moved to a rendering thread.
///
/// Rendering recurses as deep as the document, so run it on a thread with enough stack,
/// for example through [`with_render_stack`](crate::with_render_stack).
///
/// Available with the `cli` feature (on by default).
#[derive(Debug, Clone)]
pub struct Converter {
    args: ConvertArgs,
    fonts: Vec<FontArg>,
}

impl Converter {
    /// Validate the options, written as they would be on the command line without the
    /// program name (for example `["--page-size", "A4", "--grayscale"]`).
    ///
    /// The HTML comes from the reader given to [`render`](Self::render) and the PDF goes to
    /// its sink, so an input path, `--output` and the `server` subcommand are rejected as
    /// [`ConvertError::Usage`].
    pub fn from_args<I, S>(args: I) -> Result<Self, ConvertError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let args: Vec<String> = args.into_iter().map(Into::into).collect();
        // For options wkhtmltopdf has but we do not support, report the reason rather than
        // clap's "unknown argument", as the command does.
        if let Some(message) = unsupported::check_arguments(&args) {
            return Err(ConvertError::Usage(message));
        }

        // The command requires an input path. `-` (standard input) satisfies it and is never
        // read, since `render` takes the HTML from its reader. A second positional argument,
        // meaning an input path from the caller, is then rejected by clap.
        let argv = [env!("CARGO_PKG_NAME").to_string(), "-".to_string()]
            .into_iter()
            .chain(args);
        let matches = Cli::command()
            .try_get_matches_from(argv)
            .map_err(|e| ConvertError::Usage(e.to_string()))?;
        let cli =
            Cli::from_arg_matches(&matches).map_err(|e| ConvertError::Usage(e.to_string()))?;

        #[cfg(feature = "server")]
        if cli.command.is_some() {
            return Err(ConvertError::Usage(
                "subcommands cannot be used here; pass only conversion options".to_string(),
            ));
        }
        if cli.convert.output.is_some() {
            return Err(ConvertError::Usage(
                "--output cannot be used here: the PDF is written to the sink passed to render"
                    .to_string(),
            ));
        }

        let fonts = cli
            .convert
            .font_specs(&matches)
            .map_err(ConvertError::Usage)?;
        Ok(Self {
            args: cli.convert,
            fonts,
        })
    }

    /// Convert the HTML read from `html` and write the PDF into `sink`, returning what the
    /// sink's [`Sink::finish`] returns.
    ///
    /// The input is fed to the engine in chunks, so a large document is never held in memory
    /// whole. The character encoding is detected the way a browser does (BOM, `<meta charset>`,
    /// then UTF-8), unless `--encoding` was given.
    pub fn render<S: Sink<Error = io::Error>>(
        &self,
        html: impl Read,
        sink: S,
    ) -> Result<S::Output, ConvertError> {
        convert::render_with(&self.args, &self.fonts, html, sink)
    }

    /// [`render`](Self::render) into memory, returning the PDF bytes.
    pub fn render_to_vec(&self, html: impl Read) -> Result<Vec<u8>, ConvertError> {
        self.render(html, MemorySink::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage_message(result: Result<Converter, ConvertError>) -> String {
        match result {
            Err(ConvertError::Usage(message)) => message,
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn accepts_options_without_a_program_name() {
        let converter = Converter::from_args(["--page-size", "Letter", "--grayscale"]).unwrap();
        assert!(converter.args.grayscale);
    }

    #[test]
    fn an_empty_list_uses_the_defaults() {
        Converter::from_args(Vec::<String>::new()).unwrap();
    }

    #[test]
    fn rejects_an_input_path() {
        let message = usage_message(Converter::from_args(["page.html"]));
        assert!(message.contains("page.html"), "{message}");
    }

    #[test]
    fn rejects_output() {
        let message = usage_message(Converter::from_args(["--output", "out.pdf"]));
        assert!(message.contains("--output"), "{message}");
    }

    #[cfg(feature = "server")]
    #[test]
    fn rejects_the_server_subcommand() {
        usage_message(Converter::from_args(["server"]));
    }

    #[test]
    fn binds_each_font_index_to_the_preceding_font() {
        let converter =
            Converter::from_args(["--font", "a.ttf", "--font", "b.ttc", "--font-index", "2"])
                .unwrap();

        assert_eq!(converter.fonts.len(), 2);
        assert_eq!(converter.fonts[0].index, 0);
        assert_eq!(converter.fonts[1].index, 2);
    }

    #[test]
    fn rejects_unsupported_wkhtmltopdf_options_with_a_reason() {
        let message = usage_message(Converter::from_args(["--enable-javascript"]));
        assert!(message.contains("is not supported"), "{message}");
    }

    #[test]
    fn rejects_unknown_options() {
        usage_message(Converter::from_args(["--no-such-option"]));
    }

    #[test]
    fn is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Converter>();
    }
}
