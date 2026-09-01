//! Converting image bytes (JPEG/PNG/WebP) into data for embedding as a PDF Image XObject.
//!
//! It takes the raw bytes returned by `core/src/img/` (URL resolution, fetching and
//! caching), identifies the format from its magic bytes and decodes it. A JPEG is not
//! decoded: only the width, height and component count are read from the SOF marker and it
//! is embedded as-is with the DCTDecode filter (the approach validated in
//! `core/examples/spike_image_jpeg_passthrough.rs`). PNG and WebP are fully decoded by the
//! `png` crate and the `image` crate (webp feature only) respectively, and any alpha channel
//! is split off from the colour data into a separate XObject used as its `/SMask`

use std::io::Cursor;

use pdf_writer::{Filter, Finish};
use png::Transformations;

use super::font::deflate;

/// A failure anywhere from format identification through to conversion into embeddable data.
#[derive(Debug)]
pub struct ImageDecodeError(String);

impl std::fmt::Display for ImageDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to decode the image: {}", self.0)
    }
}

impl std::error::Error for ImageDecodeError {}

/// The cap on the raw pixel buffer after decoding (128MiB).
///
/// PNG and WebP allocate the buffer from the dimensions in the header alone, so without this
/// a file of a few dozen bytes could force a gigabyte-scale allocation (a decompression
/// bomb). Rust aborts on a failed allocation, so it has to be rejected before we try.
///
/// 128MiB is 32 megapixels of RGBA, over three times a photograph filling A4 at 300dpi
/// (about 8.7 megapixels), so a realistic image never hits it.
const MAX_DECODED_IMAGE_BYTES: u64 = 128 * 1024 * 1024;

/// Check that the decoded byte count fits [`MAX_DECODED_IMAGE_BYTES`] before allocating.
fn ensure_decoded_size_within_limit(
    decoded_bytes: u64,
    width: u32,
    height: u32,
) -> Result<(), ImageDecodeError> {
    if decoded_bytes > MAX_DECODED_IMAGE_BYTES {
        return Err(ImageDecodeError(format!(
            "the image is too large ({width}x{height}: {decoded_bytes} bytes decoded exceeds the limit of {MAX_DECODED_IMAGE_BYTES} bytes)"
        )));
    }
    Ok(())
}

/// The data for one stream, ready to embed as a PDF Image XObject.
#[derive(Debug, Clone)]
pub struct ImagePlane {
    pub data: Vec<u8>,
    pub filter: Filter,
    pub color_space: PlaneColorSpace,
    pub bits_per_component: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneColorSpace {
    Gray,
    Rgb,
    Cmyk,
}

/// The decode result. Raster and vector look the same to layout.
///
/// `width`/`height` are the CSS intrinsic size (px). For a raster image they are always
/// integers (a pixel count), but an SVG **can be fractional** through `width="40.6"` or a
/// fractional `viewBox`, so they are held as `f32`. Rounding to integers would skew the aspect
/// ratio and visibly shift `object-fit: contain` (40.6x10.4 to 41x10 changes the ratio by 5%).
#[derive(Debug, Clone)]
pub struct PreparedImage {
    pub width: f32,
    pub height: f32,
    pub content: PreparedContent,
}

/// The two kinds of content, which are embedded in the PDF differently.
#[derive(Debug, Clone)]
pub enum PreparedContent {
    /// A raster image (JPEG/PNG/WebP). It becomes one Image XObject.
    /// If `alpha` is present it is written as a separate XObject used as `color`'s `/SMask`.
    Raster {
        color: ImagePlane,
        alpha: Option<ImagePlane>,
    },
    /// An SVG. It becomes a Form XObject plus the objects it references ([`pdf::svg`](super::svg)).
    #[cfg(feature = "svg")]
    Vector(super::svg::VectorGraphic),
}

impl PreparedImage {
    /// The raster content. `None` for an SVG.
    fn raster(&self) -> Option<(&ImagePlane, Option<&ImagePlane>)> {
        match &self.content {
            PreparedContent::Raster { color, alpha } => Some((color, alpha.as_ref())),
            #[cfg(feature = "svg")]
            PreparedContent::Vector(_) => None,
        }
    }
}

/// The pixel count written to an Image XObject's `/Width` and `/Height`.
///
/// A raster image's intrinsic size is the integer the decoder returned, converted to `f32`,
/// so rounding it back changes nothing (this function is only called on the raster path).
fn pixels(value: f32) -> u32 {
    value.round().max(0.0) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFormat {
    Jpeg,
    Png,
    WebP,
    Svg,
}

/// Identify the format from the magic bytes. The declared `Content-Type` and the mime type
/// of a `data:` URI are not trusted; only the actual bytes decide.
/// SVG alone has no magic bytes, so anything matching none of the raster formats is sniffed
/// as XML ([`svg::looks_like_svg`](super::svg::looks_like_svg)).
fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFormat::Jpeg)
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(ImageFormat::Png)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(ImageFormat::WebP)
    } else if super::svg::looks_like_svg(bytes) {
        Some(ImageFormat::Svg)
    } else {
        None
    }
}

/// Identify the format of the image bytes, decode them and convert them into data ready for
/// PDF embedding.
///
/// `svg_fonts` are the fonts for `<text>` inside an SVG ([`SvgFontDb`]). They are not used
/// for decoding raster images.
pub fn decode_image(
    bytes: &[u8],
    svg_fonts: &SvgFontDb,
) -> Result<PreparedImage, ImageDecodeError> {
    match sniff_format(bytes) {
        Some(ImageFormat::Jpeg) => decode_jpeg(bytes),
        Some(ImageFormat::Png) => decode_png(bytes),
        Some(ImageFormat::WebP) => decode_webp(bytes),
        Some(ImageFormat::Svg) => decode_svg(bytes, svg_fonts),
        None => Err(ImageDecodeError(
            "unsupported image format (it is none of JPEG, PNG, WebP or SVG)".to_string(),
        )),
    }
}

#[cfg(feature = "svg")]
fn decode_svg(bytes: &[u8], fonts: &SvgFontDb) -> Result<PreparedImage, ImageDecodeError> {
    let (width, height, graphic) =
        super::svg::convert_svg(bytes, fonts).map_err(|e| ImageDecodeError(e.to_string()))?;
    Ok(PreparedImage {
        width,
        height,
        content: PreparedContent::Vector(graphic),
    })
}

/// Warn once per document about an SVG containing `<text>` when we have no fonts.
///
/// Without the `svg-text` feature (the default), text inside an SVG is **not drawn at all**
/// (not even converted to paths). The warnings from usvg/svg2pdf go through the `log` crate,
/// which this crate never configures, so they vanish. Text disappearing silently is hard to
/// diagnose, hence this warning.
///
/// It stays quiet when `fonts` is non-empty (that is, `svg-text` is on and fonts exist).
fn warn_if_svg_text_will_be_dropped(bytes: &[u8], fonts: &SvgFontDb, warned: &Cell<bool>) {
    if warned.get() || !fonts.is_empty() {
        return;
    }
    // Confirm it is an SVG first, to avoid pointlessly scanning raster image bytes.
    if !super::svg::looks_like_svg(bytes) || !super::svg::looks_like_it_has_text(bytes) {
        return;
    }
    warned.set(true);
    eprintln!(
        "warning: <text> inside an SVG is not drawn (not even as paths).\n  \
         To draw it, build with the svg-text feature enabled\n  \
         (the document's fonts are then usable inside the SVG too)"
    );
}

#[cfg(not(feature = "svg"))]
fn decode_svg(_bytes: &[u8], _fonts: &SvgFontDb) -> Result<PreparedImage, ImageDecodeError> {
    Err(ImageDecodeError(
        "SVG cannot be drawn because the `svg` feature is disabled".to_string(),
    ))
}

/// Read only the SOF0 (baseline) / SOF2 (progressive) marker to extract the width, height
/// and component count. No pixel data is decoded at all
fn parse_jpeg_dimensions(data: &[u8]) -> Option<(u16, u16, u8)> {
    if data.len() < 4 || data[0..2] != [0xFF, 0xD8] {
        return None;
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xC0 || marker == 0xC2 {
            // An SOF segment is "2 length + 1 precision + 2 height + 2 width + 1 component
            // count", needing at least 10 bytes including the 2-byte marker. A truncated JPEG
            // may not reach that, so it is taken as a slice before being read
            // (indexing would panic out of bounds).
            let fields = data.get(i + 5..i + 10)?;
            let height = u16::from_be_bytes([fields[0], fields[1]]);
            let width = u16::from_be_bytes([fields[2], fields[3]]);
            let components = fields[4];
            return Some((width, height, components));
        }
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let segment_len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        i += 2 + segment_len;
    }
    None
}

fn decode_jpeg(bytes: &[u8]) -> Result<PreparedImage, ImageDecodeError> {
    let (width, height, components) = parse_jpeg_dimensions(bytes)
        .ok_or_else(|| ImageDecodeError("no SOF marker found".to_string()))?;
    let color_space = match components {
        1 => PlaneColorSpace::Gray,
        3 => PlaneColorSpace::Rgb,
        4 => PlaneColorSpace::Cmyk,
        other => {
            return Err(ImageDecodeError(format!(
                "unsupported JPEG component count: {other}"
            )))
        }
    };
    Ok(PreparedImage {
        width: width as f32,
        height: height as f32,
        content: PreparedContent::Raster {
            color: ImagePlane {
                data: bytes.to_vec(),
                filter: Filter::DctDecode,
                color_space,
                bits_per_component: 8,
            },
            alpha: None,
        },
    })
}

fn decode_png(bytes: &[u8]) -> Result<PreparedImage, ImageDecodeError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| ImageDecodeError(e.to_string()))?;

    let buffer_size = reader
        .output_buffer_size()
        .ok_or_else(|| ImageDecodeError("cannot obtain the frame information".to_string()))?;
    // The `png` crate's `Limits` does not cover this buffer of ours, so the cap is checked
    // here before allocating.
    let declared = reader.info();
    ensure_decoded_size_within_limit(buffer_size as u64, declared.width, declared.height)?;
    let mut buf = vec![0u8; buffer_size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| ImageDecodeError(e.to_string()))?;
    let (width, height) = (info.width, info.height);

    // normalize_to_color8() has already expanded Indexed to RGB(A), so what comes out here is
    // one of Grayscale, GrayscaleAlpha, Rgb or Rgba. There is no need to inflate a grayscale
    // image to RGB (that would triple the byte count), so the original channel layout is kept
    // and only color_space changes.
    let (color_bytes, color_space, alpha) = match info.color_type {
        png::ColorType::Rgb => (buf, PlaneColorSpace::Rgb, None),
        png::ColorType::Grayscale => (buf, PlaneColorSpace::Gray, None),
        png::ColorType::Rgba => {
            let (color, alpha) = split_interleaved_alpha(&buf, 4);
            (color, PlaneColorSpace::Rgb, Some(alpha))
        }
        png::ColorType::GrayscaleAlpha => {
            let (color, alpha) = split_interleaved_alpha(&buf, 2);
            (color, PlaneColorSpace::Gray, Some(alpha))
        }
        png::ColorType::Indexed => {
            return Err(ImageDecodeError(
                "Indexed surviving normalize_to_color8() is unexpected".to_string(),
            ))
        }
    };

    Ok(PreparedImage {
        width: width as f32,
        height: height as f32,
        content: PreparedContent::Raster {
            color: ImagePlane {
                data: deflate(&color_bytes),
                filter: Filter::FlateDecode,
                color_space,
                bits_per_component: 8,
            },
            alpha: alpha.map(|a| ImagePlane {
                data: deflate(&a),
                filter: Filter::FlateDecode,
                color_space: PlaneColorSpace::Gray,
                bits_per_component: 8,
            }),
        },
    })
}

fn decode_webp(bytes: &[u8]) -> Result<PreparedImage, ImageDecodeError> {
    use image::{ColorType, ImageDecoder};

    let decoder = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
        .map_err(|e| ImageDecodeError(e.to_string()))?;
    let (width, height) = decoder.dimensions();
    let color_type = decoder.color_type();
    // The value follows from the header dimensions alone, so the cap is checked before truncating with `as usize`.
    let total_bytes = decoder.total_bytes();
    ensure_decoded_size_within_limit(total_bytes, width, height)?;
    let mut buf = vec![0u8; total_bytes as usize];
    decoder
        .read_image(&mut buf)
        .map_err(|e| ImageDecodeError(e.to_string()))?;

    let (color_bytes, alpha) = match color_type {
        ColorType::Rgb8 => (buf, None),
        ColorType::Rgba8 => {
            let (color, alpha) = split_interleaved_alpha(&buf, 4);
            (color, Some(alpha))
        }
        other => {
            return Err(ImageDecodeError(format!(
                "unsupported WebP color_type: {other:?}"
            )))
        }
    };

    Ok(PreparedImage {
        width: width as f32,
        height: height as f32,
        content: PreparedContent::Raster {
            color: ImagePlane {
                data: deflate(&color_bytes),
                filter: Filter::FlateDecode,
                color_space: PlaneColorSpace::Rgb,
                bits_per_component: 8,
            },
            alpha: alpha.map(|a| ImagePlane {
                data: deflate(&a),
                filter: Filter::FlateDecode,
                color_space: PlaneColorSpace::Gray,
                bits_per_component: 8,
            }),
        },
    })
}

/// Split off the last channel (alpha) from a buffer interleaved every `stride`
/// (3+1=4, or 1+1=2). What is left is the colour data.
fn split_interleaved_alpha(buf: &[u8], stride: usize) -> (Vec<u8>, Vec<u8>) {
    let pixel_count = buf.len() / stride;
    let mut color = Vec::with_capacity(pixel_count * (stride - 1));
    let mut alpha = Vec::with_capacity(pixel_count);
    for px in buf.chunks_exact(stride) {
        color.extend_from_slice(&px[..stride - 1]);
        alpha.push(px[stride - 1]);
    }
    (color, alpha)
}

// Sharing the "fetch then decode" result within a document, and writing it out as a PDF Image XObject
//
// The approach chosen: share the decode result (`PreparedImage`) itself behind an `Rc`, and
// defer Ref allocation and the actual XObject writing until PDF encoding (after layout, at
// the same point as fonts' `embed_font`/`embed_font_streaming_chunks`).
//
// The reason: allocating Refs and writing to the PDF at box tree construction
// (`layout::box_tree`) would require box tree construction to hold write access to the
// `Sink`, breaking the existing design where box tree construction is determined purely by
// the DOM plus the styles (and is shared by both batch and streaming modes).
// Sharing an `Rc<PreparedImage>` through a per-document cache (this module's
// `ImageAssetCache`) still fetches and decodes once per src (satisfying 0014's core
// requirement that memory scale with the number of distinct images rather than the number of
// elements), while Ref allocation and writing stay at the existing point, as for fonts.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use pdf_writer::{Chunk, Ref};

use crate::img::{DocumentImageCache, ImageFetcher};

use super::document::RefAllocator;
use super::svg::SvgFontDb;

/// One [`ImageAssetCache`] result (the decoded image on success, or the reason for failure).
type CachedDecodedImage = Result<Rc<PreparedImage>, Rc<str>>;

/// The cache memoising `<img>` fetch and decode within a document.
///
/// It layers memoisation of the decode result (`PreparedImage`) on top of
/// `img::DocumentImageCache` (which memoises the raw bytes). However many times the same
/// `src` is referenced within one document, both the fetch and the decode happen once.
pub struct ImageAssetCache {
    fetcher: ImageFetcher,
    fetch_cache: DocumentImageCache,
    decoded: RefCell<HashMap<String, CachedDecodedImage>>,
    /// The fonts used for `<text>` inside an SVG. Built from the document's `FontCollection`
    /// and passed in via [`Self::with_svg_fonts`]. Empty by default (text inside an SVG is not drawn).
    svg_fonts: SvgFontDb,
    /// Whether the "`<text>` inside an SVG is not drawn" warning has already been issued (once per document).
    warned_svg_text: Cell<bool>,
}

impl ImageAssetCache {
    pub fn new(base_dir: PathBuf, allow_remote: bool) -> Self {
        Self::with_base_href(base_dir, allow_remote, None)
    }

    /// Construct with a `<base href>` (the base for relative references).
    pub fn with_base_href(
        base_dir: PathBuf,
        allow_remote: bool,
        base_href: Option<String>,
    ) -> Self {
        Self::with_fetcher(ImageFetcher::new(base_dir, allow_remote).with_base_href(base_href))
    }

    /// Build from an already-configured fetcher (one reflecting local access control and so on).
    pub fn with_fetcher(fetcher: ImageFetcher) -> Self {
        Self {
            fetcher,
            fetch_cache: DocumentImageCache::new(),
            decoded: RefCell::new(HashMap::new()),
            svg_fonts: SvgFontDb::empty(),
            warned_svg_text: Cell::new(false),
        }
    }

    /// Set the fonts used for `<text>` inside an SVG (used builder-style).
    ///
    /// Call it after the document's fonts are decided and before image resolution begins.
    /// Without it, text inside an SVG is not drawn.
    pub fn with_svg_fonts(mut self, fonts: SvgFontDb) -> Self {
        self.svg_fonts = fonts;
        self
    }

    /// Whether at least one reference failed to fetch or decode
    /// (for the `--load-media-error-handling abort` decision).
    pub fn had_errors(&self) -> Option<String> {
        self.decoded
            .borrow()
            .iter()
            .find_map(|(src, result)| result.as_ref().err().map(|e| format!("{src}: {e}")))
    }

    /// Return the decoded image for `raw_src` (the raw value of the `<img src>` attribute).
    pub fn get_or_decode(&self, raw_src: &str) -> CachedDecodedImage {
        if let Some(cached) = self.decoded.borrow().get(raw_src) {
            return cached.clone();
        }

        let result = self
            .fetch_cache
            .get_or_fetch(&self.fetcher, raw_src)
            .and_then(|bytes| {
                warn_if_svg_text_will_be_dropped(&bytes, &self.svg_fonts, &self.warned_svg_text);
                decode_image(&bytes, &self.svg_fonts)
                    .map(Rc::new)
                    .map_err(|e| Rc::from(e.to_string()))
            });

        self.decoded
            .borrow_mut()
            .insert(raw_src.to_string(), result.clone());
        result
    }
}

/// The `Ref`s allocated to write one image ([`PreparedImage`]) into the PDF.
///
/// An SVG holds as many `Ref`s as there are objects in its chunk rather than just one, so
/// this cannot be `Copy` (it is passed by reference).
#[derive(Debug, Clone)]
pub struct ImageIds {
    /// The `Ref` of the XObject referenced with `Do` from the content stream.
    /// An Image XObject for a raster, a Form XObject for an SVG.
    /// This is also what goes into the page's `/Resources /XObject` dictionary.
    pub root: Ref,
    kind: ImageIdsKind,
}

#[derive(Debug, Clone)]
enum ImageIdsKind {
    /// Only `Some` when `PreparedContent::Raster`'s `alpha` is `Some`.
    Raster { alpha: Option<Ref> },
    /// The svg2pdf chunk, already renumbered into the document's `Ref` space. It lives here
    /// because what will be written is already settled.
    ///
    /// Writing happens once per `src`, so [`embed_image`]/[`embed_image_streaming_chunks`]
    /// `take` it rather than cloning, leaving it `None` once written. From then on only
    /// `root` (that is, [`ImageIds::root`]) remains, for the `Do` references on later pages.
    /// This mirrors a raster image not keeping the XObject bytes separately from the decode
    /// result.
    #[cfg(feature = "svg")]
    Vector {
        graphic: Option<Box<super::svg::RenumberedVectorGraphic>>,
    },
}

/// Allocate a `Ref` for `image` if `image_ids` does not already have one
/// (it only registers; the caller writes the XObject with
/// [`embed_image`]/[`embed_image_streaming_chunks`]).
///
/// `image_ids` is a document-wide map keyed on `Rc::as_ptr` (the identity of the decode
/// result, assuming `ImageAssetCache` returns the same `Rc` for the same `src`). If it is
/// already registered, that Ref is returned and no new one is allocated
/// (so the same image is never written to the PDF twice).
///
/// Returns `None` when an SVG's `Ref` renumbering failed (that image is then not drawn).
/// `failed` is the set recording those failures, used to **keep the warning to one even when
/// the same SVG is used many times** (a raster image fails at the decode stage before
/// reaching here and is not covered).
pub fn ids_for_image<'a>(
    alloc: &mut RefAllocator,
    image_ids: &'a mut HashMap<usize, ImageIds>,
    failed: &mut HashSet<usize>,
    image: &Rc<PreparedImage>,
) -> Option<(&'a mut ImageIds, bool)> {
    let key = Rc::as_ptr(image) as usize;
    if failed.contains(&key) {
        return None;
    }
    let is_new = !image_ids.contains_key(&key);
    if is_new {
        let ids = match &image.content {
            PreparedContent::Raster { alpha, .. } => {
                // Allocate the colour data before the alpha (`/SMask`) so `root` stays stable
                // (the order itself carries no meaning).
                let root = alloc.next();
                ImageIds {
                    root,
                    kind: ImageIdsKind::Raster {
                        alpha: alpha.as_ref().map(|_| alloc.next()),
                    },
                }
            }
            #[cfg(feature = "svg")]
            PreparedContent::Vector(graphic) => {
                match super::svg::renumber_into_document(graphic, alloc) {
                    Ok(renumbered) => ImageIds {
                        root: renumbered.root,
                        kind: ImageIdsKind::Vector {
                            graphic: Some(Box::new(renumbered)),
                        },
                    },
                    Err(e) => {
                        eprintln!("warning: {e}");
                        failed.insert(key);
                        return None;
                    }
                }
            }
        };
        image_ids.insert(key, ids);
    }
    Some((image_ids.get_mut(&key)?, is_new))
}

/// For batch mode: write `image`'s XObject directly into `pdf`
/// (a `DerefMut<Target = Chunk>`).
pub fn embed_image(
    pdf: &mut impl std::ops::DerefMut<Target = Chunk>,
    image: &PreparedImage,
    ids: &mut ImageIds,
    grayscale: bool,
) {
    match &mut ids.kind {
        ImageIdsKind::Raster { alpha } => {
            let alpha_id = *alpha;
            let (color, alpha) = raster_planes(image, grayscale);
            if let (Some(alpha), Some(alpha_id)) = (&alpha, alpha_id) {
                write_plane(
                    pdf,
                    alpha_id,
                    pixels(image.width),
                    pixels(image.height),
                    alpha,
                    None,
                );
            }
            write_plane(
                pdf,
                ids.root,
                pixels(image.width),
                pixels(image.height),
                &color,
                alpha_id,
            );
        }
        #[cfg(feature = "svg")]
        ImageIdsKind::Vector { graphic } => {
            // Writing happens once per `src`, so it is taken and released
            // (which frees the `Chunk` here).
            if let Some(graphic) = graphic.take() {
                warn_if_grayscale_svg(grayscale);
                // `Chunk::extend` takes care of fixing up the offsets too.
                pdf.extend(&graphic.chunk);
            }
        }
    }
}

/// The streaming version of [`embed_image`]. Returns the sequence of units written to the
/// `Sink` one at a time (the same shape as fonts' `embed_font_streaming_chunks`).
pub fn embed_image_streaming_chunks(
    image: &PreparedImage,
    ids: &mut ImageIds,
    grayscale: bool,
) -> Vec<EmbedChunk> {
    match &mut ids.kind {
        ImageIdsKind::Raster { alpha } => {
            let alpha_id = *alpha;
            let (color, alpha) = raster_planes(image, grayscale);
            // The alpha and the colour data go in separate chunks and are written one at a
            // time (so both are never in memory at once).
            let mut chunks = Vec::with_capacity(2);
            if let (Some(alpha), Some(alpha_id)) = (&alpha, alpha_id) {
                let mut chunk = Chunk::new();
                write_plane(
                    &mut chunk,
                    alpha_id,
                    pixels(image.width),
                    pixels(image.height),
                    alpha,
                    None,
                );
                chunks.push(EmbedChunk::single(alpha_id, chunk));
            }
            let mut chunk = Chunk::new();
            write_plane(
                &mut chunk,
                ids.root,
                pixels(image.width),
                pixels(image.height),
                &color,
                alpha_id,
            );
            chunks.push(EmbedChunk::single(ids.root, chunk));
            chunks
        }
        #[cfg(feature = "svg")]
        ImageIdsKind::Vector { graphic } => match graphic.take() {
            // Writing happens once per `src`, so it is taken and handed over rather than
            // cloned (the `Chunk` is freed once it has been streamed to the `Sink`).
            Some(graphic) => {
                warn_if_grayscale_svg(grayscale);
                vec![EmbedChunk {
                    chunk: graphic.chunk,
                    offsets: graphic.offsets,
                }]
            }
            None => Vec::new(),
        },
    }
}

/// One unit streamed to the `Sink` in a single write.
///
/// The bytes of `chunk` are written as-is, and the "`Ref` plus starting position within the
/// chunk" pairs in `offsets` are registered in the xref. A raster image is one chunk per
/// object, but an SVG puts several objects in one chunk, hence this shape.
pub struct EmbedChunk {
    pub chunk: Chunk,
    pub offsets: Vec<(Ref, usize)>,
}

impl EmbedChunk {
    fn single(id: Ref, chunk: Chunk) -> Self {
        Self {
            chunk,
            offsets: vec![(id, 0)],
        }
    }
}

/// A raster plane just before it is written (with `--grayscale` already applied).
///
/// `ImageIdsKind` is built from `PreparedContent` by `ids_for_image`, and the two are paired
/// by the same `Rc<PreparedImage>`. So an `image` reaching the `ImageIdsKind::Raster` branch
/// is always a raster, and an SVG turning up here is a bug in the caller.
fn raster_planes(image: &PreparedImage, grayscale: bool) -> (ImagePlane, Option<ImagePlane>) {
    let Some((color, alpha)) = image.raster() else {
        unreachable!("only PreparedContent::Raster corresponds to ImageIdsKind::Raster");
    };
    let color = if grayscale {
        let (plane, converted) = to_grayscale_plane(color);
        if !converted {
            eprintln!(
                "warning: this image cannot be converted to grayscale (we have no decoder for JPEG/CMYK)"
            );
        }
        plane
    } else {
        color.clone()
    };
    (color, alpha.cloned())
}

/// An SVG embeds its converted content stream as-is, so `--grayscale` cannot be applied
/// afterwards (the colours live inside the individual drawing operators).
#[cfg(feature = "svg")]
fn warn_if_grayscale_svg(grayscale: bool) {
    if grayscale {
        eprintln!("warning: an SVG cannot be converted to grayscale (it is embedded in colour)");
    }
}

/// Convert an image's colour plane to grayscale.
///
/// Only an `Rgb` plane that can hold pixel data (uncompressed or `/FlateDecode`) can be
/// converted. JPEG passthrough (`/DCTDecode`) and CMYK have no decoder and cannot be
/// converted, so they are returned unchanged.
/// It returns `false` when nothing was converted, so the caller can warn.
pub fn to_grayscale_plane(plane: &ImagePlane) -> (ImagePlane, bool) {
    if plane.color_space != PlaneColorSpace::Rgb || plane.bits_per_component != 8 {
        // Gray needs no conversion; Cmyk and DctDecode cannot be converted.
        let converted = plane.color_space == PlaneColorSpace::Gray;
        return (plane.clone(), converted);
    }

    let raw = match plane.filter {
        Filter::FlateDecode => match inflate(&plane.data) {
            Some(bytes) => bytes,
            None => return (plane.clone(), false),
        },
        Filter::DctDecode => return (plane.clone(), false),
        _ => plane.data.clone(),
    };

    let mut gray = Vec::with_capacity(raw.len() / 3);
    for px in raw.as_chunks::<3>().0 {
        let y = 0.2126 * px[0] as f32 + 0.7152 * px[1] as f32 + 0.0722 * px[2] as f32;
        gray.push(y.round().clamp(0.0, 255.0) as u8);
    }

    let (data, filter) = match plane.filter {
        Filter::FlateDecode => (deflate(&gray), Filter::FlateDecode),
        other => (gray, other),
    };
    (
        ImagePlane {
            data,
            filter,
            color_space: PlaneColorSpace::Gray,
            bits_per_component: 8,
        },
        true,
    )
}

/// zlib inflate. `None` if the data was corrupt (the caller then gives up on converting).
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(data)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn write_plane(
    chunk: &mut Chunk,
    id: Ref,
    width: u32,
    height: u32,
    plane: &ImagePlane,
    smask: Option<Ref>,
) {
    let mut xobject = chunk.image_xobject(id, &plane.data);
    xobject.width(width as i32);
    xobject.height(height as i32);
    match plane.color_space {
        PlaneColorSpace::Gray => {
            xobject.color_space().device_gray();
        }
        PlaneColorSpace::Rgb => {
            xobject.color_space().device_rgb();
        }
        PlaneColorSpace::Cmyk => {
            xobject.color_space().device_cmyk();
        }
    }
    xobject.bits_per_component(plane.bits_per_component);
    xobject.filter(plane.filter);
    if let Some(smask_id) = smask {
        xobject.s_mask(smask_id);
    }
    xobject.finish();
}

/// The resource name registered in the PDF's `/Resources /XObject` dictionary. Deriving it
/// mechanically from the `Ref` number avoids keeping separate per-page numbering.
/// What is passed in is [`ImageIds::root`] (an Image XObject for an image, a Form XObject for an SVG).
pub fn image_resource_name(root_ref: Ref) -> String {
    format!("Im{}", root_ref.get())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::read::ZlibDecoder;
    use std::io::Read;

    const JPEG_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_gradient.jpg"
    );
    const PNG_ALPHA_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_gradient_alpha.png"
    );
    const PNG_OPAQUE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_opaque.png"
    );
    const PNG_GRAY_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_gray.png"
    );
    const WEBP_ALPHA_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_gradient_alpha.webp"
    );
    const WEBP_OPAQUE_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/images/spike_opaque.webp"
    );

    fn inflate(data: &[u8]) -> Vec<u8> {
        let mut decoder = ZlibDecoder::new(data);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        out
    }

    /// Extract the planes from a raster decode result.
    fn planes(prepared: &PreparedImage) -> (&ImagePlane, Option<&ImagePlane>) {
        prepared.raster().expect("expected a raster image")
    }

    #[test]
    fn jpeg_is_embedded_as_a_dctdecode_passthrough() {
        let bytes = std::fs::read(JPEG_PATH).unwrap();
        let original_len = bytes.len();
        let prepared =
            decode_image(&bytes, &SvgFontDb::empty()).expect("jpeg decode should succeed");
        let (color, alpha) = planes(&prepared);

        assert_eq!(prepared.width, 32.0);
        assert_eq!(prepared.height, 24.0);
        assert_eq!(color.filter, Filter::DctDecode);
        assert_eq!(color.color_space, PlaneColorSpace::Rgb);
        assert!(alpha.is_none(), "JPEG has no alpha channel");
        assert_eq!(
            color.data.len(),
            original_len,
            "passthrough must not re-encode the JPEG bytes"
        );
        assert_eq!(color.data, bytes);
    }

    #[test]
    fn png_with_alpha_splits_color_and_smask() {
        let bytes = std::fs::read(PNG_ALPHA_PATH).unwrap();
        let prepared =
            decode_image(&bytes, &SvgFontDb::empty()).expect("png decode should succeed");
        let (color, alpha) = planes(&prepared);

        assert_eq!(prepared.width, 16.0);
        assert_eq!(prepared.height, 16.0);
        assert_eq!(color.filter, Filter::FlateDecode);
        assert_eq!(color.color_space, PlaneColorSpace::Rgb);

        let alpha = alpha.expect("expected an alpha plane");
        assert_eq!(alpha.color_space, PlaneColorSpace::Gray);

        let color_bytes = inflate(&color.data);
        assert_eq!(color_bytes.len(), 16 * 16 * 3);
        let alpha_bytes = inflate(&alpha.data);
        assert_eq!(alpha_bytes.len(), 16 * 16);

        // A fixture generated with the left half (x<8) opaque (255) and the right half semi-transparent (80).
        assert_eq!(alpha_bytes[0], 255, "left half should be opaque");
        assert_eq!(alpha_bytes[8], 80, "right half should be semi-transparent");
    }

    #[test]
    fn opaque_png_has_no_alpha_plane() {
        let bytes = std::fs::read(PNG_OPAQUE_PATH).unwrap();
        let prepared =
            decode_image(&bytes, &SvgFontDb::empty()).expect("png decode should succeed");
        let (color, alpha) = planes(&prepared);

        assert!(alpha.is_none());
        assert_eq!(color.color_space, PlaneColorSpace::Rgb);
        let color_bytes = inflate(&color.data);
        assert_eq!(
            color_bytes.len(),
            (prepared.width * prepared.height * 3.0) as usize
        );
    }

    #[test]
    fn grayscale_png_stays_devicegray_without_tripling_bytes() {
        let bytes = std::fs::read(PNG_GRAY_PATH).unwrap();
        let prepared =
            decode_image(&bytes, &SvgFontDb::empty()).expect("png decode should succeed");
        let (color, alpha) = planes(&prepared);

        assert!(alpha.is_none());
        assert_eq!(color.color_space, PlaneColorSpace::Gray);
        let color_bytes = inflate(&color.data);
        assert_eq!(
            color_bytes.len(),
            (prepared.width * prepared.height) as usize,
            "grayscale should stay 1 byte/pixel, not be expanded to RGB"
        );
    }

    #[test]
    fn webp_with_alpha_splits_color_and_smask() {
        let bytes = std::fs::read(WEBP_ALPHA_PATH).unwrap();
        let prepared =
            decode_image(&bytes, &SvgFontDb::empty()).expect("webp decode should succeed");
        let (color, alpha) = planes(&prepared);

        assert_eq!(prepared.width, 16.0);
        assert_eq!(prepared.height, 16.0);
        assert_eq!(color.color_space, PlaneColorSpace::Rgb);
        let alpha = alpha.expect("expected an alpha plane");

        let alpha_bytes = inflate(&alpha.data);
        assert_eq!(alpha_bytes[0], 255, "left half should be opaque");
        assert_eq!(alpha_bytes[8], 80, "right half should be semi-transparent");
    }

    #[test]
    fn opaque_webp_has_no_alpha_plane() {
        let bytes = std::fs::read(WEBP_OPAQUE_PATH).unwrap();
        let prepared =
            decode_image(&bytes, &SvgFontDb::empty()).expect("webp decode should succeed");
        let (color, alpha) = planes(&prepared);

        assert!(alpha.is_none());
        assert_eq!(color.color_space, PlaneColorSpace::Rgb);
    }

    /// An SVG rides on the same `PreparedImage` as a raster and its intrinsic size reads the same way.
    #[cfg(feature = "svg")]
    #[test]
    fn an_svg_is_decoded_as_vector_content_with_its_intrinsic_size() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20"><rect width="40" height="20"/></svg>"#;
        let prepared =
            decode_image(svg, &SvgFontDb::empty()).expect("svg conversion should succeed");

        assert_eq!((prepared.width, prepared.height), (40.0, 20.0));
        assert!(matches!(prepared.content, PreparedContent::Vector(_)));
        assert!(
            prepared.raster().is_none(),
            "an SVG has no raster planes to embed"
        );
    }

    #[test]
    fn unrecognized_bytes_are_rejected() {
        let result = decode_image(b"not an image", &SvgFontDb::empty());
        assert!(result.is_err());
    }

    #[test]
    fn truncated_jpeg_header_is_rejected() {
        let result = decode_image(&[0xFF, 0xD8, 0xFF], &SvgFontDb::empty());
        assert!(result.is_err());
    }

    /// A JPEG whose bytes run out right after the SOF marker.
    #[test]
    fn a_jpeg_truncated_inside_the_sof_segment_is_rejected_without_panicking() {
        // Six bytes, ending at FFD8 (SOI) FFC0 (SOF0) 0011 (segment length).
        let result = decode_image(&[0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11], &SvgFontDb::empty());
        assert!(result.is_err(), "a truncated SOF should be rejected");
    }

    /// An SOF segment "one byte short" is rejected the same way
    /// (catching an off-by-one regression at the boundary).
    #[test]
    fn a_jpeg_one_byte_short_of_a_complete_sof_is_rejected() {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        bytes.extend_from_slice(&[0x00, 0x10, 0x00]); // 2 bytes of height plus the first byte of width
        let result = decode_image(&bytes, &SvgFontDb::empty());
        assert!(
            result.is_err(),
            "an SOF not reaching the component count is rejected"
        );
    }

    /// A small PNG declaring huge dimensions in its header (a decompression bomb). Without the
    /// cap, `vec![0u8; w*h*4]` fails to allocate and aborts the whole process.
    #[test]
    fn a_png_declaring_huge_dimensions_is_rejected_before_allocating() {
        fn chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut crc_input = kind.to_vec();
            crc_input.extend_from_slice(data);
            out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
            out
        }

        // A 68-byte PNG declaring 20000x20000 RGBA = 1.6GB.
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&20000u32.to_be_bytes());
        ihdr.extend_from_slice(&20000u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth 8 / color type 6(RGBA)

        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&chunk(b"IDAT", &deflate(&[0u8; 10])));
        png.extend_from_slice(&chunk(b"IEND", b""));

        assert!(
            png.len() < 200,
            "the bomb's own file is small: {}",
            png.len()
        );
        let err = decode_image(&png, &SvgFontDb::empty())
            .expect_err("a PNG whose decoded size exceeds the cap should be rejected");
        assert!(
            err.to_string().contains("too large"),
            "it must be refused by the size cap: {err}"
        );
    }

    /// CRC32 for PNG (implemented only to build headers in the tests).
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
}
