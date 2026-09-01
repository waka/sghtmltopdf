//! The Sink trait, an abstraction over where the PDF bytes are written.
//!
//! The engine only needs to know that it is "writing bytes somewhere"; what the
//! destination actually is (memory, a file, or one day a Rack response or a
//! multipart upload) is deliberately none of its business.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub trait Sink {
    type Output;
    type Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error>;
    fn finish(self) -> Result<Self::Output, Self::Error>;
}

/// In-memory buffer Sink, for tests and the synchronous-return mode.
#[derive(Debug, Default)]
pub struct MemorySink {
    buf: Vec<u8>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Sink for MemorySink {
    type Output = Vec<u8>;
    type Error = io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn finish(self) -> Result<Self::Output, Self::Error> {
        Ok(self.buf)
    }
}

/// Sink that writes to a file (for the CLI).
///
/// It writes to a temporary file (`<output>.tmp-<pid>`) and only renames it onto the
/// final output when [`Sink::finish`] succeeds, so a failure part-way through rendering
/// never leaves a broken PDF at the output path. If it is dropped without `finish`,
/// `Drop` removes the temporary file.
pub struct FileSink {
    /// `take`n by `finish`. `None` means "already finished", which doubles as the check
    /// for whether `Drop` still has a temporary file to remove.
    file: Option<File>,
    temp_path: PathBuf,
    final_path: PathBuf,
}

impl FileSink {
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        let final_path = path.as_ref().to_path_buf();
        let mut temp_name = final_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("output.pdf"))
            .to_os_string();
        temp_name.push(format!(".tmp-{}", std::process::id()));
        let temp_path = final_path.with_file_name(temp_name);

        let file = File::create(&temp_path)?;
        Ok(Self {
            file: Some(file),
            temp_path,
            final_path,
        })
    }
}

impl Sink for FileSink {
    type Output = ();
    type Error = io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        match self.file.as_mut() {
            Some(file) => file.write_all(bytes),
            None => Err(io::Error::other(
                "tried to write to a FileSink that was already finished",
            )),
        }
    }

    fn finish(mut self) -> Result<Self::Output, Self::Error> {
        let Some(mut file) = self.file.take() else {
            return Err(io::Error::other("FileSink::finish was called twice"));
        };
        file.flush()?;
        drop(file);
        if let Err(e) = std::fs::rename(&self.temp_path, &self.final_path) {
            // If the rename fails, do not leave the temporary file behind.
            let _ = std::fs::remove_file(&self.temp_path);
            return Err(e);
        }
        Ok(())
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        // Only clean up if `finish` was never reached (i.e. we aborted with an error).
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.temp_path);
        }
    }
}

/// Sink that writes to standard output (for `-o -`).
///
/// Bytes already written cannot be taken back, so on a mid-way failure the caller
/// reports it through stderr and the exit code.
#[derive(Debug)]
pub struct StdoutSink {
    out: io::Stdout,
}

impl StdoutSink {
    pub fn new() -> Self {
        Self { out: io::stdout() }
    }
}

impl Default for StdoutSink {
    fn default() -> Self {
        Self::new()
    }
}

impl Sink for StdoutSink {
    type Output = ();
    type Error = io::Error;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.out.write_all(bytes)
    }

    fn finish(mut self) -> Result<Self::Output, Self::Error> {
        self.out.flush()
    }
}

/// Minimum part size for a multipart upload (all but the last part).
pub const MULTIPART_MIN_PART_SIZE: usize = 5 * 1024 * 1024;

/// A Sink that calls `on_part` every time `threshold` bytes have accumulated.
///
/// A generic implementation for something like "a buffering Sink that PUTs a multipart
/// chunk every 5MB" (the intended `threshold` being [`MULTIPART_MIN_PART_SIZE`]).
/// Multipart uploads impose a minimum size on every part but the last, so rather than
/// PUTting each small PDF chunk that a flush boundary produces, we accumulate up to the
/// threshold and hand that over as one part.
///
/// The core deliberately depends on neither Ruby nor the AWS SDK, so no actual upload
/// (HTTP PUT or otherwise) happens here. This type's responsibility ends at handing one
/// part's bytes to the `on_part` callback; the real PUT against the storage service is
/// expected to happen inside `on_part`, in the FFI layer (the Ruby bindings).
///
/// The last part (whatever remains in the buffer when `finish` is called) is passed to
/// `on_part` even if it is under `threshold` (the last part is allowed to be smaller).
/// If the buffer happens to be empty at `finish`, no empty part is sent.
pub struct BufferedSink<T, E, F: FnMut(Vec<u8>) -> Result<T, E>> {
    buf: Vec<u8>,
    threshold: usize,
    on_part: F,
    parts: Vec<T>,
}

impl<T, E, F: FnMut(Vec<u8>) -> Result<T, E>> BufferedSink<T, E, F> {
    /// Create a `BufferedSink` that calls `on_part` every `threshold` bytes.
    /// A `threshold` of 0 would call `on_part` for every single byte on every `write`,
    /// which is wasteful, so callers must pass a positive value.
    pub fn new(threshold: usize, on_part: F) -> Self {
        Self {
            buf: Vec::new(),
            threshold,
            on_part,
            parts: Vec::new(),
        }
    }
}

impl<T, E, F: FnMut(Vec<u8>) -> Result<T, E>> Sink for BufferedSink<T, E, F> {
    /// The values returned by each `on_part` call (an ETag, say), in order.
    type Output = Vec<T>;
    type Error = E;

    fn write(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        self.buf.extend_from_slice(bytes);
        while self.threshold > 0 && self.buf.len() >= self.threshold {
            let part: Vec<u8> = self.buf.drain(..self.threshold).collect();
            self.parts.push((self.on_part)(part)?);
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Self::Output, Self::Error> {
        if !self.buf.is_empty() {
            self.parts.push((self.on_part)(self.buf)?);
        }
        Ok(self.parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_sink_accumulates_written_bytes() {
        let mut sink = MemorySink::new();
        sink.write(b"hello, ").unwrap();
        sink.write(b"world").unwrap();
        assert_eq!(sink.finish().unwrap(), b"hello, world");
    }

    #[test]
    fn file_sink_writes_to_disk() {
        let dir =
            std::env::temp_dir().join(format!("sghtmltopdf-sink-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.bin");

        let mut sink = FileSink::create(&path).unwrap();
        sink.write(b"pdf bytes").unwrap();
        sink.finish().unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"pdf bytes");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn buffered_sink_does_not_flush_a_part_before_the_threshold_is_reached() {
        let mut parts_seen: Vec<Vec<u8>> = Vec::new();
        let mut sink: BufferedSink<(), io::Error, _> = BufferedSink::new(10, |part| {
            parts_seen.push(part);
            Ok(())
        });
        sink.write(b"12345").unwrap();
        sink.write(b"6789").unwrap();

        assert!(
            parts_seen.is_empty(),
            "9 bytes written should not cross the 10-byte threshold yet"
        );
    }

    #[test]
    fn buffered_sink_flushes_a_part_once_the_threshold_is_crossed() {
        let mut parts_seen: Vec<Vec<u8>> = Vec::new();
        let mut sink: BufferedSink<(), io::Error, _> = BufferedSink::new(10, |part| {
            parts_seen.push(part);
            Ok(())
        });
        sink.write(b"12345").unwrap();
        sink.write(b"67890").unwrap();

        assert_eq!(parts_seen, vec![b"1234567890".to_vec()]);
    }

    #[test]
    fn buffered_sink_flushes_multiple_parts_from_a_single_large_write() {
        let mut parts_seen: Vec<Vec<u8>> = Vec::new();
        let mut sink: BufferedSink<(), io::Error, _> = BufferedSink::new(3, |part| {
            parts_seen.push(part);
            Ok(())
        });
        sink.write(b"abcdefghi").unwrap();

        assert_eq!(
            parts_seen,
            vec![b"abc".to_vec(), b"def".to_vec(), b"ghi".to_vec()]
        );
    }

    #[test]
    fn buffered_sink_flushes_the_remaining_partial_data_on_finish() {
        let mut parts_seen: Vec<Vec<u8>> = Vec::new();
        let mut sink: BufferedSink<(), io::Error, _> = BufferedSink::new(10, |part| {
            parts_seen.push(part);
            Ok(())
        });
        sink.write(b"hello").unwrap();
        sink.finish().unwrap();

        assert_eq!(
            parts_seen,
            vec![b"hello".to_vec()],
            "the final short part (5 bytes, below the 10-byte threshold) should still \
             be flushed on finish, as the last part is allowed to be smaller"
        );
    }

    #[test]
    fn buffered_sink_does_not_send_an_empty_final_part() {
        let mut parts_seen: Vec<Vec<u8>> = Vec::new();
        let mut sink: BufferedSink<(), io::Error, _> = BufferedSink::new(5, |part| {
            parts_seen.push(part);
            Ok(())
        });
        sink.write(b"12345").unwrap();
        sink.finish().unwrap();

        assert_eq!(
            parts_seen,
            vec![b"12345".to_vec()],
            "exactly one full part should be sent, with no trailing empty part on finish"
        );
    }

    #[test]
    fn buffered_sink_preserves_byte_order_across_parts() {
        let mut parts_seen: Vec<Vec<u8>> = Vec::new();
        let mut sink: BufferedSink<(), io::Error, _> = BufferedSink::new(4, |part| {
            parts_seen.push(part);
            Ok(())
        });
        for chunk in [b"ab".as_slice(), b"cdef".as_slice(), b"gh".as_slice(), b"i"] {
            sink.write(chunk).unwrap();
        }
        sink.finish().unwrap();

        let reassembled: Vec<u8> = parts_seen.concat();
        assert_eq!(reassembled, b"abcdefghi");
    }

    #[test]
    fn buffered_sink_propagates_errors_from_on_part() {
        let mut sink: BufferedSink<(), io::Error, _> =
            BufferedSink::new(4, |_part| Err(io::Error::other("upload failed")));
        let result = sink.write(b"abcd");
        assert!(result.is_err());
    }

    #[test]
    fn buffered_sink_output_collects_each_parts_return_value() {
        // The values `on_part` returns (an ETag, say) are collected into Output in order.
        let mut next_etag = 0u32;
        let mut sink: BufferedSink<u32, io::Error, _> = BufferedSink::new(4, |_part| {
            next_etag += 1;
            Ok(next_etag)
        });
        sink.write(b"abcdefgh").unwrap();
        let etags = sink.finish().unwrap();

        assert_eq!(etags, vec![1, 2]);
    }
}
