//! Encoding of layout results into PDF objects.

mod document;
mod font;
mod img;
mod options;
mod streaming;
mod svg;

pub use document::{
    anchor_destination_name, encode_pdf, encode_pdf_with_anchors, encode_pdf_with_options,
    write_document, write_document_with_options, LinkSettings, PageOverlay,
};
pub use font::{embed_font, FontIds};
pub use img::{ImageAssetCache, ImagePlane, PlaneColorSpace, PreparedContent, PreparedImage};
pub use options::{
    current_datetime, current_pdf_date, pdf_date_from_unix, producer_string, DocumentMetadata,
    PdfOutputOptions, DEFAULT_SCALE,
};
pub use streaming::StreamingPdfWriter;
pub use svg::{warn_about_inline_svg, SvgFontDb};
