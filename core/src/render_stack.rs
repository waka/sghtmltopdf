//! Allocation of the rendering stack.
//!
//! Style computation, box tree construction, layout and PDF drawing all recurse as deep
//! as the DOM, so a deep document crashes if the calling thread's stack is small.
//! Shared by the CLI, the HTTP server, the Ruby binding and the engine tests.

/// Stack size allocated for the thread that runs rendering.
///
/// How much is needed follows from [`crate::html::MAX_ELEMENT_DEPTH`]. Its cap of 256
/// levels is about 2.8MiB in debug-build terms, so this value leaves over 5x headroom.
///
/// We do not rely on the default because a thread's default stack varies widely by
/// environment (2MiB for threads Rust spawns; main can be smaller depending on
/// `ulimit -s`; Ruby threads get about 1MiB). Fixing it here keeps the depth cap in
/// line with the depth we can actually survive.
pub const STACK_SIZE: usize = 16 * 1024 * 1024;

/// Run `f` on a thread with a [`STACK_SIZE`] stack and return its result.
///
/// If `f` panics, the panic is propagated to the calling thread
/// (so interposing a thread does not change behaviour).
pub fn with_render_stack<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_SIZE)
            .spawn_scoped(scope, f)
            .expect("failed to create the rendering thread")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}
