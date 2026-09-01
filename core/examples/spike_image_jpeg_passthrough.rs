//! Spike: a PoC fetching a JPEG over HTTP and embedding it in a PDF as-is with the DCTDecode
//! filter, without decoding it.
//!
//! What we want to check:
//! - Whether `ureq` can complete an HTTP(S) fetch through a synchronous (non-async) API.
//!   Confirmed with a real TCP round trip against a local loopback HTTP server (std::net
//!   only; no external network connection is used)
//! - Whether the width, height and component count can be extracted from the SOF0/SOF2
//!   marker alone, without decoding the JPEG bytes at all (the main point being whether we
//!   can avoid adding an image decoding crate)
//! - Whether `pdf-writer`'s `ImageXObject` accepts `Filter::DctDecode` and lets the raw JPEG
//!   bytes be embedded directly as the stream
//!
//! Run with: `cargo run --example spike_image_jpeg_passthrough`
//! (it uses `tests/fixtures/images/spike_gradient.jpg`, a baseline JPEG)

use std::io::{Read, Write};
use std::net::TcpListener;

use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect as PdfRect, Ref};

const JPEG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_gradient.jpg"
);

/// Read only the SOF0 (baseline) / SOF2 (progressive) marker to extract the width, height
/// and component count. No pixel data is decoded at all.
fn parse_jpeg_dimensions(data: &[u8]) -> Option<(u16, u16, u8)> {
    if data.len() < 4 || data[0..2] != [0xFF, 0xD8] {
        return None; // no SOI marker
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        // SOF0..SOF3, SOF5..SOF7, SOF9..SOF11 and SOF13..SOF15 are the SOF family.
        // Only baseline (0xC0) and progressive (0xC2) are handled here.
        if marker == 0xC0 || marker == 0xC2 {
            let height = u16::from_be_bytes([data[i + 5], data[i + 6]]);
            let width = u16::from_be_bytes([data[i + 7], data[i + 8]]);
            let components = data[i + 9];
            return Some((width, height, components));
        }
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2; // a marker with no length field
            continue;
        }
        let segment_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 2 + segment_len;
    }
    None
}

/// Start an HTTP server on loopback that answers exactly once, with no dependency on the
/// external network. A real fetch is expected to be the same synchronous `ureq` API call.
fn spawn_single_response_server(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind to loopback");
    let addr = listener.local_addr().unwrap();

    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("failed to accept the connection");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf); // the request body is discarded (this is a spike and does not check it)

        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(header.as_bytes()).unwrap();
        stream.write_all(&body).unwrap();
    });

    format!("http://{addr}/spike.jpg")
}

fn main() {
    let jpeg_bytes = std::fs::read(JPEG_PATH).expect("failed to read the JPEG test fixture");
    let url = spawn_single_response_server(jpeg_bytes.clone());

    // As in the real fetch abstraction, this is all done through ureq's synchronous API
    // (no separate async runtime or thread pool has to be brought in).
    let fetched: Vec<u8> = ureq::get(&url)
        .call()
        .expect("the HTTP fetch failed")
        .body_mut()
        .read_to_vec()
        .expect("failed to read the response body");

    assert_eq!(
        fetched, jpeg_bytes,
        "the fetched bytes do not match the original JPEG"
    );

    let (width, height, components) =
        parse_jpeg_dimensions(&fetched).expect("failed to parse the size from the SOF marker");
    eprintln!("JPEG: {width}x{height}, components={components}");

    let mut ids = 0..;
    let mut next_id = || Ref::new(ids.next().unwrap() + 1);

    let catalog_id = next_id();
    let pages_tree_id = next_id();
    let page_id = next_id();
    let content_id = next_id();
    let image_id = next_id();

    let mut pdf = Pdf::new();
    pdf.catalog(catalog_id).pages(pages_tree_id);
    pdf.pages(pages_tree_id).kids([page_id]).count(1);

    let mut page = pdf.page(page_id);
    page.parent(pages_tree_id);
    page.media_box(PdfRect::new(0.0, 0.0, width as f32, height as f32));
    page.contents(content_id);
    page.resources().x_objects().pair(Name(b"Im0"), image_id);
    page.finish();

    let mut content = Content::new();
    // One image tiled over the whole page (scaled to width/height by the cm matrix).
    content.save_state();
    content.transform([width as f32, 0.0, 0.0, height as f32, 0.0, 0.0]);
    content.x_object(Name(b"Im0"));
    content.restore_state();
    pdf.stream(content_id, &content.finish());

    // The heart of the check: the JPEG bytes are embedded directly as a DCTDecode filter
    // stream, with no decoding at all.
    let mut image = pdf.image_xobject(image_id, &fetched);
    image.width(width as i32);
    image.height(height as i32);
    match components {
        1 => image.color_space().device_gray(),
        3 => image.color_space().device_rgb(),
        4 => image.color_space().device_cmyk(),
        other => panic!("unsupported component count: {other}"),
    }
    image.bits_per_component(8);
    image.filter(Filter::DctDecode);
    image.finish();

    let bytes = pdf.finish();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/spike_image_jpeg_passthrough.pdf");
    std::fs::write(&out, &bytes).unwrap();
    eprintln!(
        "wrote {} bytes to {} (embedded JPEG stayed {} bytes raw, no re-encode)",
        bytes.len(),
        out.display(),
        fetched.len()
    );
}
