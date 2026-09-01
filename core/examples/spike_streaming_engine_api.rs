//! A PoC checking that the type signatures of the core API expressing flush boundaries fit
//! together consistently with the existing `Sink` trait.
//!
//! Only the API's *shape* is checked here. Streaming parsing, making layout flushable and
//! the post-processing of font embedding (the CIDToGIDMap approach) are not wired in yet,
//! and the bodies of `feed`/`finish` are left as dummy implementations.
//!
//! What we want to check:
//! - Whether a shape where `Engine<S: Sink>` owns the Sink and can call `sink.write`
//!   internally on every `feed` meshes with the existing `Sink` trait (`sink/mod.rs`) as-is
//! - Whether the core's Rust API can correspond almost one to one with the Ruby FFI boundary
//!   (`Engine.new(options)`, `feed(html_chunk)`, `each_pdf_chunk { |bytes| ... }`,
//!   `finish`)
//! - Whether Ruby's `each_pdf_chunk { |bytes| ... }` block can be wrapped, without touching
//!   the core, in "a Sink implementation that merely calls the block whenever it is called"
//!   (simulated by `CallbackSink`)
//! - Whether switching between `Mode::Batch` and `Mode::Streaming` sits naturally on the
//!   same `Engine` type. Batch mode imposes no non-locality constraints at all (waiting for
//!   the whole DOM before processing, it handles `nth-last-child` and friends fine), and
//!   only Streaming mode applies them (a `<style>` part-way through the body is refused with
//!   an error)
//!
//! Run with: `cargo run --example spike_streaming_engine_api`

use sghtmltopdf_core::sink::{MemorySink, Sink};

/// Selects batch or streaming processing.
///
/// `Batch` processes only once the whole DOM is present, so it imposes no non-locality
/// constraints (no unsupported `nth-last-child` and friends, no `<style>`-inside-`<head>`-only rule).
/// Only `Streaming` applies them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Batch,
    Streaming,
}

/// A placeholder for the engine's initialisation options (page size, margins and so on).
/// The real fields will be decided in line with the CLI and bindings layers' options.
#[derive(Default)]
struct EngineOptions {
    mode: Mode,
}

/// The errors `Engine` returns. It distinguishes a failed write to the Sink (`Io`) from an
/// unsupported HTML structure detected in Streaming mode (`UnsupportedInStreamingMode`).
/// The latter is an error the core decides itself and unrelated to `Sink::Error`, so it gets
/// its own variant rather than riding on `S::Error`.
#[derive(Debug)]
enum EngineError<E> {
    Io(E),
    UnsupportedInStreamingMode(&'static str),
}

impl<E> From<E> for EngineError<E> {
    fn from(e: E) -> Self {
        Self::Io(e)
    }
}

/// The type signature of a streaming engine that consumes HTML while calling `sink.write` at
/// every flush boundary (each settled page). The body is filled in by the real implementation.
struct Engine<S: Sink> {
    sink: S,
    options: EngineOptions,
    /// For the dummy implementation: it merely accumulates the chunks received by feed.
    /// In the real implementation this is replaced by the streaming parser's and layout's internal state.
    pending: Vec<u8>,
    /// For the dummy implementation: whether `<body` has been seen (the real implementation
    /// decides it through a TreeSink hook).
    seen_body: bool,
}

impl<S: Sink> Engine<S> {
    fn new(options: EngineOptions, sink: S) -> Self {
        Self {
            sink,
            options,
            pending: Vec::new(),
            seen_body: false,
        }
    }

    /// Feed one HTML chunk. Whenever a new flush boundary (a settled page) is reached
    /// internally, `sink.write` is called (this dummy implementation never does; the real
    /// implementation adds it alongside making layout flushable).
    ///
    /// Under `Mode::Streaming` it returns an error if a `<style` appears after `<body`.
    /// The real check is implemented as a TreeSink hook (here a crude byte scan stands in for
    /// the purposes of this check).
    fn feed(&mut self, html_chunk: &[u8]) -> Result<(), EngineError<S::Error>> {
        if self.options.mode == Mode::Streaming {
            if !self.seen_body && contains(html_chunk, b"<body") {
                self.seen_body = true;
            }
            if self.seen_body && contains(html_chunk, b"<style") {
                return Err(EngineError::UnsupportedInStreamingMode(
                    "<style> after <body> is not supported in streaming mode",
                ));
            }
        }
        self.pending.extend_from_slice(html_chunk);
        Ok(())
    }

    /// Settle the remaining content as the last page, do the all-pages post-processing such
    /// as font embedding (the CIDToGIDMap approach), and then call `sink.finish()`.
    fn finish(mut self) -> Result<S::Output, EngineError<S::Error>> {
        // Dummy implementation: it merely writes the accumulated chunks out once.
        // The real implementation assembles and writes the PDF bytes here (the content
        // streams plus the font embedding).
        self.sink.write(&self.pending)?;
        Ok(self.sink.finish()?)
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// A Sink implementation that merely calls a callback, modelling Ruby's
/// `each_pdf_chunk { |bytes| ... }`. It confirms that this conversion can be absorbed
/// entirely in the FFI layer, with no change to the core.
struct CallbackSink<F: FnMut(&[u8])> {
    callback: F,
}

impl<F: FnMut(&[u8])> Sink for CallbackSink<F> {
    type Output = ();
    type Error = std::io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        (self.callback)(bytes);
        Ok(())
    }

    fn finish(self) -> Result<Self::Output, Self::Error> {
        Ok(())
    }
}

fn main() {
    // --- The equivalent of the synchronous-return mode: write to a MemorySink (Batch by default) ---
    let mut engine = Engine::new(EngineOptions::default(), MemorySink::new());
    engine.feed(b"<p>Hello").unwrap();
    engine.feed(b", world!</p>").unwrap();
    let bytes = engine.finish().unwrap();
    eprintln!(
        "via MemorySink (Batch): {} bytes -> {:?}",
        bytes.len(),
        String::from_utf8_lossy(&bytes)
    );

    // --- The equivalent of each_pdf_chunk { |bytes| ... }: write to a callback Sink ---
    let mut chunks_seen = Vec::new();
    let callback_sink = CallbackSink {
        callback: |bytes: &[u8]| chunks_seen.push(bytes.to_vec()),
    };
    let mut engine = Engine::new(EngineOptions::default(), callback_sink);
    engine.feed(b"<p>chunked").unwrap();
    engine.feed(b" input</p>").unwrap();
    engine.finish().unwrap();
    eprintln!(
        "via CallbackSink (Batch): {} writes observed",
        chunks_seen.len()
    );

    // --- Batch mode: a <style> part-way through the body is accepted ---
    let mut engine = Engine::new(EngineOptions { mode: Mode::Batch }, MemorySink::new());
    engine.feed(b"<body><p>x</p>").unwrap();
    engine
        .feed(b"<style>p{color:red}</style>")
        .expect("Batch mode should not error on a <style> part-way through the body");
    engine.finish().unwrap();
    eprintln!("Batch mode: a <style> part-way through the body is accepted");

    // --- Streaming mode: a <style> part-way through the body is an error ---
    let mut engine = Engine::new(
        EngineOptions {
            mode: Mode::Streaming,
        },
        MemorySink::new(),
    );
    engine.feed(b"<body><p>x</p>").unwrap();
    match engine.feed(b"<style>p{color:red}</style>") {
        Err(EngineError::UnsupportedInStreamingMode(msg)) => {
            eprintln!("Streaming mode: the error was detected as expected ({msg})");
        }
        other => panic!("expected UnsupportedInStreamingMode, got {other:?}"),
    }
}
