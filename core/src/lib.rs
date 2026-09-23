//! A lightweight HTML to PDF renderer that does not depend on Chromium, WebKit or Gecko.
//!
//! Feed HTML to an [`Engine`] in chunks and it writes the PDF bytes into a [`Sink`].
//!
//! ```no_run
//! use sghtmltopdf::{Engine, EngineOptions, MemorySink};
//!
//! let mut engine = Engine::new(EngineOptions::default(), MemorySink::default());
//! engine.feed(b"<!DOCTYPE html><p>Hello</p>").unwrap();
//! let pdf: Vec<u8> = engine.finish().unwrap();
//! ```
//!
//! With the `cli` feature (on by default), [`Converter`] runs a conversion configured by the
//! same options as the `sghtmltopdf` command, which is the simplest entry point for language
//! bindings.
//!
//! Rendering recurses as deep as the document, so run it on a thread with enough stack,
//! for example through [`with_render_stack`].
//!
//! ## Stability
//!
//! The items re-exported at the crate root are the public API and follow semver.
//! The modules are reachable only so that the CLI, the tests and the Ruby binding can use
//! the internals; they are hidden from the documentation and may change in any release.

#[cfg(feature = "cli")]
pub use cli::{ConvertError, Converter};
pub use engine::{
    ContentOptions, Engine, EngineError, EngineOptions, FontSpec, GenericFamily, HeaderFooterHtml,
    HeaderFooterPlaceholders, LocalAccess, Mode, TocHeading, TocHtmlBuilder, TocSettings,
};
pub use layout::{EdgeSizes, PageSettings, PageSize};
pub use pdf::{DocumentMetadata, PdfOutputOptions, DEFAULT_SCALE};
pub use render_stack::{with_render_stack, STACK_SIZE};
pub use sink::{BufferedSink, FileSink, MemorySink, Sink, StdoutSink, MULTIPART_MIN_PART_SIZE};

/// CLI implementation. Only available with the `cli` feature (on by default).
#[doc(hidden)]
#[cfg(feature = "cli")]
pub mod cli;
#[doc(hidden)]
pub mod engine;
#[doc(hidden)]
pub mod fonts;
#[doc(hidden)]
pub mod html;
#[doc(hidden)]
pub mod img;
#[doc(hidden)]
pub mod layout;
mod numbering;
#[doc(hidden)]
pub mod pdf;
#[doc(hidden)]
pub mod render_stack;
#[doc(hidden)]
pub mod sink;
#[doc(hidden)]
pub mod style;

// Compile the Rust examples in the crates.io README along with the other doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
