//! Releasing the GVL (Global VM Lock).
//!
//! The GVL is released during rendering so Puma's other threads are not blocked.

use std::ffi::c_void;
use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};

/// Release the GVL and run `func`.
///
/// # Why the `Send` bound is needed
///
/// magnus caches the GVL state in a thread local on the assumption that "there is no API
/// that releases the GVL" (*assumed not to change because there's currently no api to
/// unlock* in `magnus::api`), so `Ruby::get()` returns `Ok` even after we release it here.
/// Touching Ruby through the handle it returns would be UB.
///
/// Imposing a `Send` bound makes capturing a magnus value (`NonNull<RBasic>`) or a `Ruby`
/// handle (`*mut ()`) a compile error, both being `!Send`.
/// Calling `Ruby::get()` again inside the closure is not something the types can prevent,
/// but the only things called in the released region are `sghtmltopdf_core` functions, and
/// the core knows nothing about Ruby, so it is unreachable.
///
/// # Interruption
///
/// The UBF (unblock function) is `None`, meaning uninterruptible. Interruption through
/// `Kernel#trap` or Ctrl-C is outside the initial scope.
#[allow(dead_code)] // used by the future GVL-releasing implementation
pub fn without_gvl<F, R>(func: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    struct State<F, R> {
        func: Option<F>,
        result: Option<std::thread::Result<R>>,
    }

    unsafe extern "C" fn call<F, R>(arg: *mut c_void) -> *mut c_void
    where
        F: FnOnce() -> R + Send,
        R: Send,
    {
        let state = unsafe { &mut *(arg as *mut State<F, R>) };
        let func = state.func.take().expect("the callback was called twice");
        // A panic crossing the FFI boundary aborts the process, so it is caught here and
        // resumed on the Rust side after the GVL is reacquired.
        state.result = Some(catch_unwind(AssertUnwindSafe(func)));
        std::ptr::null_mut()
    }

    let mut state = State::<F, R> {
        func: Some(func),
        result: None,
    };
    unsafe {
        rb_sys::rb_thread_call_without_gvl(
            Some(call::<F, R>),
            &mut state as *mut _ as *mut c_void,
            None,
            std::ptr::null_mut(),
        );
    }
    match state.result.expect("the callback was never run") {
        Ok(value) => value,
        Err(panic) => resume_unwind(panic),
    }
}

/// Reacquire the GVL and run `func`. Call only from inside [`without_gvl`].
///
/// Unlike `without_gvl` it imposes no `Send` bound, this being a region where the GVL is
/// held and Ruby may be touched.
///
/// # What the caller must observe (libruby's constraints)
///
/// * Do not return a Ruby object from `func`. Returning one puts it outside the GC's scope
///   once the GVL is released again, and it will not be marked. To carry a value back, put
///   it in a slot registered with `rb_gc_register_address`
///   (`callback_sink::ValueSlot`)
/// * Do not let `func` throw an exception. A longjmp jumping over this function is undefined
///   behaviour. Every Ruby call must be wrapped in the equivalent of `rb_protect`
///   (magnus's `Proc::call` uses `protect` internally, so it can be used directly)
pub fn with_gvl<F, R>(func: F) -> R
where
    F: FnOnce() -> R,
{
    struct State<F, R> {
        func: Option<F>,
        result: Option<std::thread::Result<R>>,
    }

    unsafe extern "C" fn call<F, R>(arg: *mut c_void) -> *mut c_void
    where
        F: FnOnce() -> R,
    {
        let state = unsafe { &mut *(arg as *mut State<F, R>) };
        let func = state.func.take().expect("the callback was called twice");
        // A panic crossing a libruby frame aborts the process.
        state.result = Some(catch_unwind(AssertUnwindSafe(func)));
        std::ptr::null_mut()
    }

    let mut state = State::<F, R> {
        func: Some(func),
        result: None,
    };
    unsafe {
        rb_sys::rb_thread_call_with_gvl(Some(call::<F, R>), &mut state as *mut _ as *mut c_void);
    }
    match state.result.expect("the callback was never run") {
        Ok(value) => value,
        Err(panic) => resume_unwind(panic),
    }
}
