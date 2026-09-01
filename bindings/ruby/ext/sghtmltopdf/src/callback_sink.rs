//! Handing the settled PDF bytes to a Ruby block, chunk by chunk.
//!
//! # How the threads are split
//!
//! Rendering recurses as deep as the DOM (style computation, layout, drawing).
//! A Ruby thread's machine stack is 1MiB by default (`thread_machine_stack_size` in
//! `RubyVM::DEFAULT_PARAMS`), so running it directly on a Puma worker thread overflows the
//! stack at a depth just under 200. Worse, it touches the guard page with the GVL released,
//! so rather than the process dying the thread hangs.
//!
//! So rendering runs on a dedicated thread with a
//! [`sghtmltopdf_core::render_stack::STACK_SIZE`] stack allocated explicitly, and the settled
//! chunks are passed back to the original thread over a channel. Only the original thread ever touches Ruby.
//!
//! ```text
//! original thread (Ruby's, GVL released)      rendering thread (16MiB)
//!   recv(chunk)  <---------- chunk ----------  Sink::write
//!   with_gvl { block.call(chunk) }
//!   send(ack)    ------------ ack ---------->  (on to the next chunk)
//! ```
//!
//! It only works this way round: [`crate::gvl::with_gvl`]'s `rb_thread_call_with_gvl`
//! assumes "this thread released the GVL through `rb_thread_call_without_gvl`" and cannot
//! be called from a thread Ruby does not know about. So the rendering thread never touches
//! Ruby at all.

use std::io;
use std::sync::mpsc::{Receiver, SyncSender};

use magnus::rb_sys::{AsRawValue, FromRawValue};
use magnus::{block::Proc, Error, ExceptionClass, RString, Ruby, Value};
use rb_sys::VALUE;
use sghtmltopdf_core::sink::Sink;

use crate::gvl;

/// The error `Sink::write` returns when the block was interrupted.
///
/// `convert::render` requires `Sink<Output = (), Error = io::Error>`, so no Ruby-derived
/// information can ride on the error type. The real reason is put in [`PendingUnwind`] and
/// this is used purely as "the signal to unwind".
fn interrupted() -> io::Error {
    io::Error::other("the Ruby block was interrupted")
}

/// Somewhere to keep a `VALUE` protected from the GC with `rb_gc_register_address`.
///
/// Ruby's conservative GC scans the machine stack to find VALUEs, but only as far as the
/// stack position at the moment the GVL was released (the machine context being saved then).
/// A value pushed onto the stack inside `without_gvl` lies beyond that and is not scanned.
/// Any VALUE that must survive across the released region has to be registered here.
///
/// The registered address is pinned with a `Box`. Even when GC compaction moves the object,
/// the contents at the registered address are updated, so no stale reference is held.
pub struct ValueSlot {
    slot: Box<VALUE>,
}

impl ValueSlot {
    pub fn new(value: VALUE) -> Self {
        let mut slot = Box::new(value);
        unsafe { rb_sys::rb_gc_register_address(&mut *slot) };
        Self { slot }
    }

    /// The address of the GC-registered slot.
    pub fn addr(&self) -> *mut VALUE {
        &*self.slot as *const VALUE as *mut VALUE
    }

    /// The current `VALUE`. Call only while holding the GVL.
    pub fn get(&self) -> VALUE {
        *self.slot
    }
}

impl Drop for ValueSlot {
    fn drop(&mut self) {
        unsafe { rb_sys::rb_gc_unregister_address(&mut *self.slot) };
    }
}

/// The address of a [`ValueSlot`], for carrying into a GVL-released region.
#[derive(Clone, Copy)]
pub struct BlockSlot(*mut VALUE);

// SAFETY: in the released region the address is only carried around as a number; it is read
// as a `VALUE` only inside `with_gvl` (that is, while holding the GVL). What it points at is
// a `ValueSlot` registered with the GC and alive until `without_gvl` returns.
unsafe impl Send for BlockSlot {}

impl BlockSlot {
    pub fn new(slot: &ValueSlot) -> Self {
        Self(slot.addr())
    }

    /// Convert back to a `Proc`, assuming the GVL is held.
    fn proc(self) -> Option<Proc> {
        let value = unsafe { Value::from_raw(*self.0) };
        Proc::from_value(value)
    }
}

/// The receptacle for carrying an exception or non-local exit thrown by the block out of the GVL-released region.
#[derive(Default)]
pub struct PendingUnwind {
    unwind: Option<Unwind>,
}

enum Unwind {
    /// A Ruby exception object.
    Exception(ValueSlot),
    /// An error the Rust side (magnus) built. The class and the message are carried separately.
    Raise { class: ValueSlot, message: String },
    /// A `break`, `return`, `throw` and so on. The value is the tag passed to `rb_jump_tag`.
    Jump(i32),
}

impl PendingUnwind {
    /// Whether the block was interrupted.
    pub fn is_pending(&self) -> bool {
        self.unwind.is_some()
    }

    /// Convert a magnus `Error` into a form that can cross the released region and store it.
    /// Call it while holding the GVL (it registers with the GC).
    fn store(&mut self, error: Error) {
        use magnus::error::ErrorType;

        let unwind = match error.error_type() {
            ErrorType::Jump(tag) => Unwind::Jump(*tag as i32),
            ErrorType::Error(class, message) => Unwind::Raise {
                class: ValueSlot::new(class.as_raw()),
                message: message.to_string(),
            },
            ErrorType::Exception(exception) => {
                Unwind::Exception(ValueSlot::new(exception.as_raw()))
            }
        };
        self.unwind = Some(unwind);
    }

    /// Return the stored interruption to Ruby. Call it while holding the GVL.
    ///
    /// A non-local exit such as `break` is propagated faithfully with `rb_jump_tag`. This
    /// function does not return, so call it once the Rust-side cleanup is done
    /// (deregistering the `ValueSlot` from the GC is already handled inside it).
    pub fn into_error(self) -> Option<Error> {
        match self.unwind? {
            Unwind::Exception(slot) => {
                let value = unsafe { Value::from_raw(slot.get()) };
                // Deregistration happens after the `Error` is built. From here on the caller
                // returns to Ruby still holding the GVL, so it is within the conservative
                // GC's scanning range.
                let error = magnus::Exception::from_value(value).map(Error::from);
                drop(slot);
                Some(error.unwrap_or_else(|| {
                    Error::new(
                        Ruby::get()
                            .expect("it should be called while holding the GVL")
                            .exception_runtime_error(),
                        "could not restore the exception the block threw",
                    )
                }))
            }
            Unwind::Raise { class, message } => {
                let value = unsafe { Value::from_raw(class.get()) };
                let error = ExceptionClass::from_value(value).map(|c| Error::new(c, message));
                drop(class);
                Some(error.unwrap_or_else(|| {
                    Error::new(
                        Ruby::get()
                            .expect("it should be called while holding the GVL")
                            .exception_runtime_error(),
                        "could not restore the block's interruption",
                    )
                }))
            }
            // `rb_jump_tag` does not return (`-> !`). Every other field of `self` has already
            // been dropped by this point.
            Unwind::Jump(tag) => unsafe { rb_sys::rb_jump_tag(tag) },
        }
    }
}

/// A Sink streaming the settled bytes to a channel in `chunk_size` pieces.
///
/// Used on the rendering thread. It never touches Ruby, so it is `Send` and free of the
/// GVL's constraints. It waits for the receiving side's acknowledgement after every chunk
/// (a rendezvous), so it cannot run ahead of the block's processing and pile up memory.
pub struct ChannelSink {
    chunks: SyncSender<Vec<u8>>,
    ack: Receiver<bool>,
    buf: Vec<u8>,
    chunk_size: usize,
}

impl ChannelSink {
    fn new(chunks: SyncSender<Vec<u8>>, ack: Receiver<bool>, chunk_size: usize) -> Self {
        Self {
            chunks,
            ack,
            buf: Vec::new(),
            // At 0 the GVL would be reacquired for every byte, hence the lower bound.
            chunk_size: chunk_size.max(1),
        }
    }

    /// Hand over one chunk and wait until the block has finished receiving it.
    ///
    /// Both being unable to send (the receiver went away) and the block returning an
    /// interruption unwind through [`interrupted`]. The real reason for the interruption is
    /// in the receiver's [`PendingUnwind`].
    fn hand_off(&mut self, chunk: Vec<u8>) -> Result<(), io::Error> {
        if self.chunks.send(chunk).is_err() {
            return Err(interrupted());
        }
        match self.ack.recv() {
            Ok(true) => Ok(()),
            _ => Err(interrupted()),
        }
    }
}

impl Sink for ChannelSink {
    type Output = ();
    type Error = io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), io::Error> {
        self.buf.extend_from_slice(bytes);
        while self.buf.len() >= self.chunk_size {
            let chunk: Vec<u8> = self.buf.drain(..self.chunk_size).collect();
            self.hand_off(chunk)?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), io::Error> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let rest = std::mem::take(&mut self.buf);
        self.hand_off(rest)
    }
}

/// Hand one chunk to the Ruby block. Returns `false` on an interruption.
///
/// This is the only place the GVL is reacquired. The block call goes through magnus's
/// `Proc::call`, which wraps it in `rb_protect` internally, so an exception's longjmp cannot
/// jump over a Rust frame.
///
/// Storing the error also happens inside this region. Carrying a Ruby object (an exception)
/// out of `with_gvl` would put it outside the GC's scope the moment the GVL is released
/// (see the documentation of `gvl::with_gvl`). Only a boolean is carried out.
fn call_block(block: BlockSlot, pending: &mut PendingUnwind, bytes: Vec<u8>) -> bool {
    gvl::with_gvl(move || {
        let ruby = Ruby::get().expect("inside with_gvl, so the GVL is held");
        let result = match block.proc() {
            Some(proc) => {
                let chunk: RString = ruby.str_from_slice(&bytes);
                proc.call::<_, Value>((chunk,)).map(|_| ())
            }
            None => Err(Error::new(
                ruby.exception_runtime_error(),
                "the block was lost",
            )),
        };
        match result {
            Ok(()) => true,
            Err(error) => {
                // Registering with the GC also happens here, while the GVL is held.
                pending.store(error);
                false
            }
        }
    })
}

/// Run `render` on a dedicated rendering thread and keep handing the chunks it produces to
/// the Ruby block from this thread.
///
/// Call it from inside a GVL-released region (inside `without_gvl`), on the thread that
/// released it. It is the left-hand side of the diagram in the module docs.
pub fn pump_to_block<F>(
    block: BlockSlot,
    pending: &mut PendingUnwind,
    chunk_size: usize,
    render: F,
) -> Result<(), sghtmltopdf_core::cli::CliError>
where
    F: FnOnce(ChannelSink) -> Result<(), sghtmltopdf_core::cli::CliError> + Send + 'static,
{
    use sghtmltopdf_core::cli::CliError;
    use sghtmltopdf_core::render_stack::STACK_SIZE;

    // Both are zero-capacity rendezvous channels. The rendering side waits for the block to
    // finish after every chunk.
    let (chunk_tx, chunk_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(0);
    let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel::<bool>(0);

    let worker = std::thread::Builder::new()
        .name("sghtmltopdf-render".to_string())
        .stack_size(STACK_SIZE)
        .spawn(move || render(ChannelSink::new(chunk_tx, ack_rx, chunk_size)))
        .map_err(|e| CliError::Input(format!("cannot create the rendering thread: {e}")))?;

    while let Ok(chunk) = chunk_rx.recv() {
        let ok = call_block(block, pending, chunk);
        // Also break out when the acknowledgement cannot be sent (the rendering side already went away).
        if ack_tx.send(ok).is_err() || !ok {
            break;
        }
    }
    // On an interruption, drop the receiving end first so the rendering side errors on its
    // next send and can unwind.
    drop(chunk_rx);
    drop(ack_tx);

    worker
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}
