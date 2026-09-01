//! Spike: a PoC decoding a PNG (possibly palettised, interlaced or with transparency) to
//! raw pixels with the `png` crate, splitting the colour data from the alpha channel and embedding both in a PDF.
//!
//! Unlike a JPEG, a PNG generally cannot be passed through as raw bytes the way DCTDecode
//! allows (interlacing, palettes and arbitrary bit depths cannot be reproduced by PDF's
//! Predictor alone). So a full decode with the `png` crate is needed (de-interlacing,
//! palette expansion and normalisation to 8 bits included).
//!
//! What we want to check:
//! - Whether `png::Transformations::normalize_to_color8()` decodes to 8-bit RGB(A)
//!   regardless of palette, low bit depth or interlacing
//! - Whether the alpha channel can be split from the decode result, with the colour data as
//!   DeviceRGB + FlateDecode and the alpha as a separate XObject in DeviceGray + FlateDecode, tied together by `/SMask`
//!
//! Run with: `cargo run --example spike_image_png_decode`
//! (it uses `tests/fixtures/images/spike_gradient_alpha.png`, whose right half is semi-transparent)

use png::{ColorType, Transformations};

use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect as PdfRect, Ref};

const PNG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_gradient_alpha.png"
);

fn main() {
    let file = std::io::BufReader::new(
        std::fs::File::open(PNG_PATH).expect("failed to read the PNG test fixture"),
    );
    let mut decoder = png::Decoder::new(file);
    // Enable palette expansion, 8-bit conversion of low bit depths and tRNS-to-alpha together.
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().expect("failed to read the PNG header");

    let mut buf = vec![
        0u8;
        reader
            .output_buffer_size()
            .expect("the frame count is unknown")
    ];
    let info = reader
        .next_frame(&mut buf)
        .expect("failed to decode the frame");
    let (width, height) = (info.width, info.height);
    eprintln!(
        "PNG: {width}x{height}, color_type={:?}, bit_depth={:?}",
        info.color_type, info.bit_depth
    );

    // After normalisation it is one of Grayscale, GrayscaleAlpha, Rgb or Rgba (a palette is
    // already expanded to RGB(A) by normalize_to_color8(), so Indexed never appears here).
    let (rgb, alpha): (Vec<u8>, Option<Vec<u8>>) = match info.color_type {
        ColorType::Rgb => (buf, None),
        ColorType::Rgba => {
            let pixel_count = (width * height) as usize;
            let mut rgb = Vec::with_capacity(pixel_count * 3);
            let mut alpha = Vec::with_capacity(pixel_count);
            for px in buf.as_chunks::<4>().0 {
                rgb.extend_from_slice(&px[0..3]);
                alpha.push(px[3]);
            }
            (rgb, Some(alpha))
        }
        ColorType::Grayscale => {
            let rgb = buf.iter().flat_map(|&g| [g, g, g]).collect();
            (rgb, None)
        }
        ColorType::GrayscaleAlpha => {
            let pixel_count = (width * height) as usize;
            let mut rgb = Vec::with_capacity(pixel_count * 3);
            let mut alpha = Vec::with_capacity(pixel_count);
            for px in buf.as_chunks::<2>().0 {
                rgb.extend_from_slice(&[px[0], px[0], px[0]]);
                alpha.push(px[1]);
            }
            (rgb, Some(alpha))
        }
        ColorType::Indexed => {
            unreachable!("normalize_to_color8() has already expanded Indexed to RGB(A)")
        }
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

    // If there is an alpha channel, write it out first as an independent DeviceGray SMask
    // (referenced from the colour data by /SMask). It is FlateDecode (zlib) compressed too.
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
        .join("../target/spike_image_png_decode.pdf");
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
