//! The Ruby extension's entry point.
//!
//! This layer is kept thin. Assembling the option argument list (argv) is done on the Ruby
//! side, and this merely runs the argv it receives through the same parser as the CLI and
//! the HTTP server, and renders.

mod callback_sink;
mod errors;
mod gvl;

use std::io::Cursor;
use std::path::PathBuf;

use magnus::rb_sys::AsRawValue;
use magnus::{block::Proc, function, prelude::*, Error, RString, Ruby};
use sghtmltopdf_core::cli::{self, convert};
use sghtmltopdf_core::render_stack;
use sghtmltopdf_core::sink::{FileSink, MemorySink};

use callback_sink::{pump_to_block, BlockSlot, PendingUnwind, ValueSlot};

/// Convert HTML and return the PDF bytes.
fn render(html: RString, argv: Vec<String>) -> Result<RString, Error> {
    let ruby = Ruby::get().expect("it should be called while holding the GVL");
    // Copy to the Rust side before releasing the GVL. Ruby objects cannot be touched while
    // it is released, so an `RString` cannot be carried in.
    let html = unsafe { html.as_slice() }.to_vec();
    errors::catch_panic(&ruby, move || render_inner(html, argv))
}

fn render_inner(html: Vec<u8>, argv: Vec<String>) -> Result<RString, Error> {
    let ruby = Ruby::get().expect("it should be called while holding the GVL");
    let (args, fonts) = cli::parse_convert_argv(&argv).map_err(|e| errors::to_ruby(&ruby, e))?;

    // Release the GVL and then move onto a thread with a stack allocated specifically for
    // rendering. A Ruby thread's machine stack is only 1MiB by default and cannot survive the
    // recursion of layout and drawing (see the module docs of `callback_sink`).
    // This path never calls back into Ruby, so it can be moved as-is.
    let pdf = gvl::without_gvl(move || {
        render_stack::with_render_stack(move || {
            convert::render_to_memory(&args, &fonts, Cursor::new(html), MemorySink::new())
        })
    })
    .map_err(|e| errors::to_ruby(&ruby, e))?;

    Ok(ruby.str_from_slice(&pdf))
}

/// Convert HTML and write it to `path`.
///
/// The destination is decided by [`FileSink`], so argv's `--output` is unused
/// (it writes to a temporary file and renames only on success, so a failure part-way through
/// leaves no broken PDF).
fn render_to_file(html: RString, argv: Vec<String>, path: String) -> Result<(), Error> {
    let ruby = Ruby::get().expect("it should be called while holding the GVL");
    let html = unsafe { html.as_slice() }.to_vec();
    errors::catch_panic(&ruby, move || render_to_file_inner(html, argv, path))
}

fn render_to_file_inner(html: Vec<u8>, argv: Vec<String>, path: String) -> Result<(), Error> {
    let ruby = Ruby::get().expect("it should be called while holding the GVL");
    let (args, fonts) = cli::parse_convert_argv(&argv).map_err(|e| errors::to_ruby(&ruby, e))?;

    let path = PathBuf::from(path);
    let sink = FileSink::create(&path).map_err(|e| {
        errors::to_ruby(
            &ruby,
            cli::CliError::Input(format!("failed to create {}: {e}", path.display())),
        )
    })?;

    gvl::without_gvl(move || {
        render_stack::with_render_stack(move || {
            convert::render(&args, &fonts, Cursor::new(html), sink)
        })
    })
    .map_err(|e| errors::to_ruby(&ruby, e))?;
    Ok(())
}

/// Convert HTML and hand the settled PDF bytes to `block` in `chunk_size` pieces.
///
/// The GVL is released during rendering and reacquired only for the moment the block is
/// called. If the block throws an exception, that exception is propagated to the caller
/// unchanged (the engine unwinds through its ordinary error path).
fn render_each(
    html: RString,
    argv: Vec<String>,
    block: Proc,
    chunk_size: usize,
) -> Result<(), Error> {
    let ruby = Ruby::get().expect("it should be called while holding the GVL");
    let html = unsafe { html.as_slice() }.to_vec();
    errors::catch_panic(&ruby, move || {
        render_each_inner(html, argv, block, chunk_size)
    })
}

fn render_each_inner(
    html: Vec<u8>,
    argv: Vec<String>,
    block: Proc,
    chunk_size: usize,
) -> Result<(), Error> {
    let ruby = Ruby::get().expect("it should be called while holding the GVL");
    let (args, fonts) = cli::parse_convert_argv(&argv).map_err(|e| errors::to_ruby(&ruby, e))?;

    // The block has to survive across the GVL-released region, so it is registered with the
    // GC (a value pushed onto the stack after the release is outside the conservative GC's scan).
    let block = ValueSlot::new(block.as_raw());
    let mut pending = PendingUnwind::default();

    let result = {
        let slot = BlockSlot::new(&block);
        let pending = &mut pending;
        gvl::without_gvl(move || {
            // Rendering runs on a thread with a dedicated stack, and only the settled chunks
            // come back here. Calling the block (that is, reacquiring the GVL) has to happen
            // on this thread, the one that released it, so it is left to `pump_to_block`.
            pump_to_block(slot, pending, chunk_size, move |sink| {
                convert::render(&args, &fonts, Cursor::new(html), sink)
            })
        })
    };
    drop(block);

    // An interruption from the block is propagated in preference to whatever error the engine
    // returns (`Sink::Error` is fixed to `io::Error`, so the reason rides on this instead).
    // A non-local exit such as `break` calls `rb_jump_tag` inside `into_error` and never
    // returns, so the Rust-side values are all dropped before it is called.
    if pending.is_pending() {
        drop(result);
        return Err(pending
            .into_error()
            .expect("with is_pending true, an interruption is present"));
    }
    result.map_err(|e| errors::to_ruby(&ruby, e))
}

/// For confirming we can link against the core (a connectivity check).
fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Really call one of the core's symbols to confirm the link.
fn default_page_size() -> String {
    let settings = sghtmltopdf_core::layout::PageSettings::default();
    format!("{}x{}", settings.size.width, settings.size.height)
}

/// For confirming it can run with the GVL released. That other Ruby threads make progress
/// during the release is checked by a test on the Ruby side.
fn sleep_without_gvl(ms: u64) {
    gvl::without_gvl(|| std::thread::sleep(std::time::Duration::from_millis(ms)));
}

#[magnus::init]
fn init(ruby: &Ruby) -> Result<(), Error> {
    let module = ruby.define_module("Sghtmltopdf")?;
    errors::define(ruby, module)?;

    let native = module.define_module("Native")?;
    native.define_singleton_method("render", function!(render, 2))?;
    native.define_singleton_method("render_to_file", function!(render_to_file, 3))?;
    native.define_singleton_method("render_each", function!(render_each, 4))?;
    native.define_singleton_method("core_version", function!(core_version, 0))?;
    native.define_singleton_method("default_page_size", function!(default_page_size, 0))?;
    native.define_singleton_method("sleep_without_gvl", function!(sleep_without_gvl, 1))?;
    Ok(())
}
