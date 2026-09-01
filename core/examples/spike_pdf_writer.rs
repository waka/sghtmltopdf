//! T1 spike: a PoC generating a minimal PDF (a rectangle plus one line of text) with pdf-writer.
//!
//! Unlike the krilla spike (`spike_krilla.rs`), this one does not use the `Pdf` type: it
//! assembles the bytes directly a `Chunk` at a time and writes each page to a fake Sink
//! (`output: Vec<u8>`, standing in for `Sink::write`) the moment it is settled.
//! Each Chunk's bytes can then be discarded straight away, and all that is kept afterwards
//! is the lightweight metadata `(Ref, the offset it was written at)`.
//! This confirms that the raw data of pages already written does not pile up in memory as
//! the page count grows. The xref and trailer are assembled by hand, pdf-writer's
//! implementation of them being private.
//!
//! Run with: `cargo run --example spike_pdf_writer` (no font embedding needed; it uses the base14 Helvetica)

use pdf_writer::writers::Catalog;
use pdf_writer::{Chunk, Content, Name, Rect, Ref, Str};

/// The fake Sink. A minimal implementation that merely records the offset on each write.
struct FakeSink {
    output: Vec<u8>,
    offsets: Vec<(Ref, usize)>,
}

impl FakeSink {
    fn new() -> Self {
        // The file header (identical to what `Pdf::new()` writes internally).
        // A `Chunk` on its own carries no header, so it is written at the front by hand.
        let output = b"%PDF-1.7\n%\x80\x80\x80\x80\n\n".to_vec();
        Self {
            output,
            offsets: Vec::new(),
        }
    }

    /// Assuming the Chunk holds a single indirect object, write its bytes while recording
    /// that object's starting offset.
    fn write_chunk(&mut self, id: Ref, chunk: &Chunk) {
        self.offsets.push((id, self.output.len()));
        self.output.extend_from_slice(chunk.as_bytes());
    }

    fn finish(mut self, root: Ref) -> Vec<u8> {
        let xref_offset = self.output.len();
        let size = self
            .offsets
            .iter()
            .map(|(id, _)| id.get())
            .max()
            .unwrap_or(0)
            + 1;

        self.offsets.sort_by_key(|(id, _)| id.get());
        self.output
            .extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        self.output.extend_from_slice(b"0000000000 65535 f \n");
        for (_, offset) in &self.offsets {
            self.output
                .extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }

        self.output.extend_from_slice(
            format!("trailer\n<< /Size {size} /Root {} 0 R >>\n", root.get()).as_bytes(),
        );
        self.output
            .extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());
        self.output
    }
}

fn main() {
    let mut ids = 0..;
    let mut next_id = || Ref::new(ids.next().unwrap() + 1);

    let catalog_id = next_id();
    let pages_tree_id = next_id();
    let font_id = next_id();

    let mut sink = FakeSink::new();

    // The font definition (the base14 Helvetica; no embedding needed, so minimal for a PoC)
    let mut chunk = Chunk::new();
    chunk
        .type1_font(font_id)
        .base_font(Name(b"Helvetica"))
        .encoding_predefined(Name(b"WinAnsiEncoding"));
    sink.write_chunk(font_id, &chunk);

    let mut page_ids = Vec::new();
    for page_no in 1..=2 {
        let page_id = next_id();
        let content_id = next_id();
        page_ids.push(page_id);

        // The page's content stream (a rectangle plus one line of text).
        // This Chunk is assembled the moment layout is settled and written to the Sink
        // immediately -- the point being to keep the bytes of settled pages out of memory.
        let mut content = Content::new();
        content.set_fill_rgb(0.8, 0.8, 0.8);
        content.rect(20.0, 20.0, 100.0, 50.0);
        content.fill_nonzero();
        content.set_fill_rgb(0.0, 0.0, 0.0);
        content.begin_text();
        content.set_font(Name(b"F1"), 14.0);
        content.next_line(20.0, 100.0);
        content.show(Str(
            format!("Hello from pdf-writer, page {page_no}").as_bytes()
        ));
        content.end_text();
        let content_bytes = content.finish();

        let mut chunk = Chunk::new();
        chunk.stream(content_id, &content_bytes);
        sink.write_chunk(content_id, &chunk);

        let mut chunk = Chunk::new();
        chunk
            .page(page_id)
            .parent(pages_tree_id)
            .media_box(Rect::new(0.0, 0.0, 300.0, 200.0))
            .contents(content_id)
            .resources()
            .fonts()
            .pair(Name(b"F1"), font_id);
        sink.write_chunk(page_id, &chunk);
    }

    // The small document-wide part that can only be built once every page's Ref is known
    // (the page tree and the catalog). Up to here, only Refs and offsets were held.
    let mut chunk = Chunk::new();
    chunk
        .pages(pages_tree_id)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);
    sink.write_chunk(pages_tree_id, &chunk);

    let mut chunk = Chunk::new();
    chunk
        .indirect(catalog_id)
        .start::<Catalog>()
        .pages(pages_tree_id);
    sink.write_chunk(catalog_id, &chunk);

    let pdf = sink.finish(catalog_id);

    let out =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/spike_pdf_writer.pdf");
    std::fs::write(&out, &pdf).unwrap();
    eprintln!("wrote {} bytes to {}", pdf.len(), out.display());
}
