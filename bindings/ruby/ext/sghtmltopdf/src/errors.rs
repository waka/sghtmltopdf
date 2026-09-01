//! The Ruby exception classes, and the mapping from the core's [`CliError`].

use std::panic::AssertUnwindSafe;

use magnus::{prelude::*, ExceptionClass, RModule, Ruby};
use sghtmltopdf_core::cli::CliError;

pub fn define(ruby: &Ruby, module: RModule) -> Result<(), magnus::Error> {
    let base = module.define_error("Error", ruby.exception_standard_error())?;
    module.define_error("UsageError", base)?;
    module.define_error("InputError", base)?;
    module.define_error("RenderError", base)?;
    module.define_error("TimeoutError", base)?;
    module.define_error("InternalError", base)?;
    Ok(())
}

/// Run `f`, converting a Rust panic into `Sghtmltopdf::InternalError`.
///
/// # Why we catch it ourselves
///
/// magnus also guards method calls against panics, so the process does not abort. But what
/// magnus converts to is Ruby's `fatal`, which not even `rescue Exception` catches and which
/// terminates the process. Losing a whole worker inside a web application over a bug in one
/// request is unacceptable, so it is converted to a descendant of `StandardError` here,
/// before it reaches magnus.
///
/// A panic means a bug in the core, so the message is kept rather than swallowed.
pub fn catch_panic<F, R>(ruby: &Ruby, f: F) -> Result<R, magnus::Error>
where
    F: FnOnce() -> Result<R, magnus::Error>,
{
    // AssertUnwindSafe: after unwinding from a panic, all that is touched is building the
    // Ruby-side exception; no half-broken Rust state is read back.
    match std::panic::catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => result,
        Err(payload) => Err(magnus::Error::new(
            class(ruby, "InternalError"),
            format!("internal error (panic): {}", panic_message(&payload)),
        )),
    }
}

/// Extract a human-readable message from a panic payload.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "no details".to_string()
    }
}

/// Convert a core error into the corresponding Ruby exception.
///
/// The message is the wording the core returns, verbatim (the same wording as the CLI).
pub fn to_ruby(ruby: &Ruby, error: CliError) -> magnus::Error {
    let (class_name, message) = match error {
        CliError::Usage(message) => ("UsageError", message),
        CliError::Input(message) => ("InputError", message),
        CliError::Render(message) => ("RenderError", message),
        CliError::Timeout(message) => ("TimeoutError", message),
    };
    magnus::Error::new(class(ruby, class_name), message)
}

/// Look up the `Sghtmltopdf::<name>` exception class.
///
/// It is defined when the `.so` is loaded ([`define`]). On the off chance the lookup fails,
/// the error is raised as a `RuntimeError` rather than swallowed.
pub fn class(ruby: &Ruby, name: &str) -> ExceptionClass {
    ruby.class_object()
        .const_get::<_, RModule>("Sghtmltopdf")
        .and_then(|module| module.const_get::<_, ExceptionClass>(name))
        .unwrap_or_else(|_| ruby.exception_runtime_error())
}
