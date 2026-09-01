//! An additional spike: a PoC decoding WebP with the `image` crate (with only the webp
//! feature enabled) and embedding it in a PDF by the same RGB-plus-SMask split as the PNG
//! spike (`spike_image_png_decode.rs`).
//!
//! Why the `image` crate: PNG and JPEG are covered by dedicated crates (`png` alone, and
//! DCTDecode passthrough with no decoding for JPEG), but WebP has no comparably lightweight
//! standalone crate (`libwebp-sys` is a C library binding, not pure Rust).
//! With its default features the `image` crate drags in AV1, TIFF, GIF and more, so it was
//! rejected as a whole; but narrowed to `default-features = false, features = ["webp"]` the
//! additions are only nine crates (`image`, `image-webp`, `quick-error`, `moxcms`, `pxfm`
//! and so on), with none of the AV1 encoder stack (`rav1e` and friends) - confirmed.
//! Its dependencies also overlap heavily with the existing `png` crate (the flate2 family),
//! so the real net addition is small
//!
//! Run with: `cargo run --example spike_image_webp_decode`
//! (it uses `tests/fixtures/images/spike_gradient_alpha.webp`, a lossless WebP whose right
//! half is semi-transparent)

use std::io::BufReader;

use image::codecs::webp::WebPDecoder;
use image::{ColorType, ImageDecoder};

use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect as PdfRect, Ref};

const WEBP_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_gradient_alpha.webp"
);

fn main() {
    let file = BufReader::new(
        std::fs::File::open(WEBP_PATH).expect("failed to read the WebP test fixture"),
    );
    let decoder = WebPDecoder::new(file).expect("failed to read the WebP header");

    let (width, height) = decoder.dimensions();
    let color_type = decoder.color_type();
    eprintln!("WebP: {width}x{height}, color_type={color_type:?}");

    let mut buf = vec![0u8; decoder.total_bytes() as usize];
    decoder
        .read_image(&mut buf)
        .expect("failed to decode the frame");

    // As in the PNG spike, the colour data and the alpha channel are split.
    let (rgb, alpha): (Vec<u8>, Option<Vec<u8>>) = match color_type {
        ColorType::Rgb8 => (buf, None),
        ColorType::Rgba8 => {
            let pixel_count = (width * height) as usize;
            let mut rgb = Vec::with_capacity(pixel_count * 3);
            let mut alpha = Vec::with_capacity(pixel_count);
            for px in buf.as_chunks::<4>().0 {
                rgb.extend_from_slice(&px[0..3]);
                alpha.push(px[3]);
            }
            (rgb, Some(alpha))
        }
        other => panic!("a color_type this spike has not checked: {other:?}"),
    };

    let mut ids = 0..;
    let mut next_id = || Ref::new(ids.next().unwrap() + 1);

    let catalog_id = next_id();
    let pages_tree_id = next_id();
    let page_id = next_id();
    let content_id = next_id();
    let image_id = next_id();
    let smask_id = next_id();

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
    content.save_state();
    content.transform([width as f32, 0.0, 0.0, height as f32, 0.0, 0.0]);
    content.x_object(Name(b"Im0"));
    content.restore_state();
    pdf.stream(content_id, &content.finish());

    if let Some(alpha) = &alpha {
        let compressed = deflate(alpha);
        let mut smask = pdf.image_xobject(smask_id, &compressed);
        smask.width(width as i32);
        smask.height(height as i32);
        smask.color_space().device_gray();
        smask.bits_per_component(8);
        smask.filter(Filter::FlateDecode);
        smask.finish();
    }

    let compressed_rgb = deflate(&rgb);
    let mut image = pdf.image_xobject(image_id, &compressed_rgb);
    image.width(width as i32);
    image.height(height as i32);
    image.color_space().device_rgb();
    image.bits_per_component(8);
    image.filter(Filter::FlateDecode);
    if alpha.is_some() {
        image.s_mask(smask_id);
    }
    image.finish();

    let bytes = pdf.finish();
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/spike_image_webp_decode.pdf");
    std::fs::write(&out, &bytes).unwrap();
    eprintln!(
        "wrote {} bytes to {} (alpha channel present: {})",
        bytes.len(),
        out.display(),
        alpha.is_some()
    );
}

fn deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}
