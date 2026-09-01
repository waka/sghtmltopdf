//! Options changing how the PDF is written.
//!
//! A type grouping the settings that change only how the PDF is written, never the layout
//! result, so they can be carried around together. The CLI's `--title`,
//! `--no-pdf-compression`, `--grayscale`, `--dpi` and `--zoom` all end up here.

/// The document metadata written to the PDF Info dictionary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentMetadata {
    /// `--title`. With none given, the HTML `<title>` goes here (resolved by the caller).
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
}

impl DocumentMetadata {
    /// Adopt the HTML `<title>` only when `title` is unset.
    pub fn fill_title_from_document(&mut self, document_title: Option<String>) {
        if self.title.is_none() {
            self.title = document_title.filter(|t| !t.trim().is_empty());
        }
    }
}

/// The default factor converting CSS px to PDF pt (at 96dpi, `72 / 96`).
pub const DEFAULT_SCALE: f32 = 72.0 / 96.0;

/// PDF output options.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfOutputOptions {
    pub metadata: DocumentMetadata,
    /// Flate compression of the PDF objects (content streams, fonts, CMaps).
    /// Image data is not covered by this flag.
    pub compress: bool,
    /// The CSS px to PDF pt factor.
    pub scale: f32,
    /// Convert fill and stroke colours to grayscale.
    pub grayscale: bool,
    /// Draw a rule below the header (`--header-line`).
    pub header_line: bool,
    /// Draw a rule above the footer (`--footer-line`).
    pub footer_line: bool,
}

impl Default for PdfOutputOptions {
    fn default() -> Self {
        Self {
            metadata: DocumentMetadata::default(),
            compress: true,
            scale: DEFAULT_SCALE,
            grayscale: false,
            header_line: false,
            footer_line: false,
        }
    }
}

impl PdfOutputOptions {
    /// Derive the conversion factor from `--dpi` and `--zoom`.
    ///
    /// `dpi` is "what dpi a CSS px is read as". The default of 96dpi gives 0.75, and passing
    /// 72 makes 1 CSS px = 1 pt.
    pub fn scale_from_dpi_and_zoom(dpi: f32, zoom: f32) -> f32 {
        72.0 / dpi * zoom
    }

    /// The conversion for values written directly in page coordinates (MediaBox, annotation Rects, Dests coordinates).
    pub fn to_pt(&self, px: f32) -> f32 {
        px * self.scale
    }

    /// The luminance formula. Returns the input unchanged when `grayscale` is off.
    pub fn map_rgb(&self, rgb: (f32, f32, f32)) -> (f32, f32, f32) {
        if !self.grayscale {
            return rgb;
        }
        let (r, g, b) = rgb;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        (y, y, y)
    }
}

/// The value written to `/Producer` in the PDF Info dictionary.
pub fn producer_string() -> String {
    format!("sghtmltopdf {}", env!("CARGO_PKG_VERSION"))
}

/// Return the current time as a PDF date string (`D:YYYYMMDDHHmmSSZ`).
/// Falls back to the UNIX epoch when the system time cannot be read.
pub fn current_pdf_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    pdf_date_from_unix(secs)
}

/// Convert UNIX seconds to a PDF date string (always UTC).
///
/// To avoid another dependency, the date arithmetic uses our own copy of Howard Hinnant's
/// `civil_from_days`.
pub fn pdf_date_from_unix(secs: i64) -> String {
    let (year, month, day, hour, minute, second) = datetime_from_unix(secs);
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z")
}

/// Break UNIX seconds into UTC year, month, day, hour, minute and second (to build a `pdf_writer::Date`).
pub fn datetime_from_unix(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    )
}

/// Return the current time as UTC year, month, day, hour, minute and second.
pub fn current_datetime() -> (i64, u32, u32, u32, u32, u32) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    datetime_from_unix(secs)
}

/// Convert days since the epoch (1970-01-01 = 0) into a Gregorian year, month and day.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_scale_turns_a4_into_the_real_paper_size() {
        let options = PdfOutputOptions::default();
        // A4 is 793.7 x 1122.5 CSS px, which in pt is 595.3 x 841.9 (= 210 x 297mm).
        assert!((options.to_pt(793.7) - 595.275).abs() < 0.1);
        assert!((options.to_pt(1122.5) - 841.875).abs() < 0.1);
    }

    #[test]
    fn dpi_72_keeps_one_css_px_as_one_pt() {
        assert!((PdfOutputOptions::scale_from_dpi_and_zoom(72.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((PdfOutputOptions::scale_from_dpi_and_zoom(96.0, 1.0) - 0.75).abs() < 1e-6);
        // zoom multiplies the factor.
        assert!((PdfOutputOptions::scale_from_dpi_and_zoom(96.0, 2.0) - 1.5).abs() < 1e-6);
    }

    #[test]
    fn grayscale_maps_colors_to_their_luminance() {
        let mut options = PdfOutputOptions::default();
        assert_eq!(options.map_rgb((1.0, 0.0, 0.0)), (1.0, 0.0, 0.0));

        options.grayscale = true;
        let (r, g, b) = options.map_rgb((1.0, 0.0, 0.0));
        assert_eq!((r, g), (b, b));
        assert!((r - 0.2126).abs() < 1e-6);
        // White and black are unchanged.
        assert_eq!(options.map_rgb((0.0, 0.0, 0.0)), (0.0, 0.0, 0.0));
        let (w, _, _) = options.map_rgb((1.0, 1.0, 1.0));
        assert!((w - 1.0).abs() < 1e-6);
    }

    #[test]
    fn pdf_dates_are_formatted_in_utc() {
        assert_eq!(pdf_date_from_unix(0), "D:19700101000000Z");
        // 2026-07-25T00:34:56Z
        assert_eq!(pdf_date_from_unix(1_784_939_696), "D:20260725003456Z");
        // A leap day.
        assert_eq!(pdf_date_from_unix(1_709_164_800), "D:20240229000000Z");
    }

    #[test]
    fn the_document_title_is_only_used_when_the_option_is_absent() {
        let mut meta = DocumentMetadata::default();
        meta.fill_title_from_document(Some("the HTML title".to_string()));
        assert_eq!(meta.title.as_deref(), Some("the HTML title"));

        let mut meta = DocumentMetadata {
            title: Some("given on the CLI".to_string()),
            ..Default::default()
        };
        meta.fill_title_from_document(Some("the HTML title".to_string()));
        assert_eq!(meta.title.as_deref(), Some("given on the CLI"));

        // An empty <title> is not adopted.
        let mut meta = DocumentMetadata::default();
        meta.fill_title_from_document(Some("   ".to_string()));
        assert_eq!(meta.title, None);
    }
}
