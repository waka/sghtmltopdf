//! Options changing how the PDF is written.
//!
//! A type grouping the settings that change only how the PDF is written, never the layout
//! result, so they can be carried around together. The CLI's `--title`,
//! `--no-pdf-compression`, `--grayscale`, `--dpi` and `--zoom` all end up here.

/// The document metadata written to the PDF Info dictionary and the XMP metadata stream, and
/// the language to the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DocumentMetadata {
    /// `--title`. With none given, the HTML `<title>` goes here (resolved by the caller).
    pub title: Option<String>,
    /// `/Author`.
    pub author: Option<String>,
    /// `/Subject`.
    pub subject: Option<String>,
    /// `/Keywords`.
    pub keywords: Option<String>,
    /// `/Creator`, the application that made the document (`xmp:CreatorTool`).
    pub creator: Option<String>,
    /// `--creation-date` as UNIX seconds. Written as `/CreationDate` and `/ModDate` instead
    /// of the wall clock, so the same input renders to the same bytes.
    pub creation_date: Option<i64>,
    /// The catalog's `/Lang`. With none given, `<html lang>` goes here (resolved by the
    /// caller); an explicit value wins and is written as given.
    pub language: Option<String>,
}

impl DocumentMetadata {
    /// The creation date as UTC year, month, day, hour, minute and second: `creation_date`
    /// when given, otherwise the current time.
    ///
    /// A writer calls this once, so `/CreationDate`, `/ModDate`, the XMP dates and the
    /// `/ID` all carry the same instant.
    pub fn datetime(&self) -> (i64, u32, u32, u32, u32, u32) {
        match self.creation_date {
            Some(secs) => datetime_from_unix(secs),
            None => current_datetime(),
        }
    }

    /// Adopt the HTML `<title>` only when `title` is unset.
    pub fn fill_title_from_document(&mut self, document_title: Option<String>) {
        if self.title.is_none() {
            self.title = document_title.filter(|t| !t.trim().is_empty());
        }
    }

    /// Adopt the `<html lang>` as the document language only when `language` is unset. It is
    /// kept as written (`en-GB`) minus surrounding whitespace; an empty value writes nothing,
    /// and one that cannot be a language tag writes nothing with a warning.
    pub fn fill_language_from_document(&mut self, document_language: Option<String>) {
        if self.language.is_none() {
            self.language = document_language
                .map(|lang| lang.trim().to_string())
                .filter(|lang| !lang.is_empty())
                .filter(|lang| {
                    let valid = is_language_tag(lang);
                    if !valid {
                        eprintln!(
                            "warning: <html lang=\"{lang}\"> is not a language tag; the PDF has no /Lang"
                        );
                    }
                    valid
                });
        }
    }
}

/// Whether `tag` has the shape of a language tag: `-`-separated subtags of 1–8 ASCII
/// letters or digits, the first one letters only.
fn is_language_tag(tag: &str) -> bool {
    let mut subtags = tag.split('-');
    let primary = subtags.next().unwrap_or_default();
    let valid = |subtag: &str| (1..=8).contains(&subtag.len());
    valid(primary)
        && primary.chars().all(|c| c.is_ascii_alphabetic())
        && subtags.all(|subtag| valid(subtag) && subtag.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// The default factor converting CSS px to PDF pt (at 96dpi, `72 / 96`).
pub const DEFAULT_SCALE: f32 = 72.0 / 96.0;

/// PDF output options.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct PdfOutputOptions {
    /// The document information written to the PDF.
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
    /// Write a tagged PDF (`--tagged`): a structure tree for accessibility, with page furniture
    /// marked as artifacts. Not supported with `Mode::Streaming`.
    pub tagged: bool,
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
            tagged: false,
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

/// Parse an ISO 8601 date or date-time into UNIX seconds (UTC).
///
/// Accepts `YYYY-MM-DD`, or a date followed by `T` (or a space) and `HH:MM[:SS[.fraction]]`,
/// optionally ending in `Z` or an offset (`+02:00`, `+0200`, `+02`). A time without an offset
/// is read as UTC. The fraction is dropped.
pub fn unix_from_iso8601(text: &str) -> Result<i64, String> {
    let bad = || format!("not an ISO 8601 date or date-time: {text:?}");
    let text = text.trim();
    let (date, rest) = match text.find(['T', 't', ' ']) {
        Some(i) => (&text[..i], Some(&text[i + 1..])),
        None => (text, None),
    };

    let mut parts = date.split('-');
    let mut number = |digits: usize| -> Result<i64, String> {
        let part = parts.next().ok_or_else(bad)?;
        if part.len() != digits || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        part.parse().map_err(|_| bad())
    };
    let (year, month, day) = (number(4)?, number(2)?, number(2)?);
    if parts.next().is_some() || year < 1 || !(1..=12).contains(&month) {
        return Err(bad());
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days_in_month).contains(&day) {
        return Err(bad());
    }

    let (mut seconds_of_day, mut offset) = (0, 0);
    if let Some(rest) = rest {
        let (clock, zone) = match rest.find(['Z', 'z', '+', '-']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        let (clock, fraction) = match clock.split_once('.') {
            Some((clock, fraction)) => (clock, Some(fraction)),
            None => (clock, None),
        };
        let mut fields = clock.split(':');
        let mut field = |max: i64| -> Result<i64, String> {
            let part = fields.next().ok_or_else(bad)?;
            if part.len() != 2 || !part.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            part.parse().ok().filter(|v| *v <= max).ok_or_else(bad)
        };
        let (hour, minute) = (field(23)?, field(59)?);
        let second = if clock.matches(':').count() == 2 {
            field(59)?
        } else {
            0
        };
        // Exactly HH:MM or HH:MM:SS, and a fraction only after the seconds.
        if fields.next().is_some() {
            return Err(bad());
        }
        if let Some(fraction) = fraction {
            if clock.matches(':').count() != 2
                || fraction.is_empty()
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(bad());
            }
        }
        seconds_of_day = hour * 3600 + minute * 60 + second;

        offset = match zone {
            "" | "Z" | "z" => 0,
            _ => {
                let sign = match zone.as_bytes()[0] {
                    b'+' => 1,
                    b'-' => -1,
                    _ => return Err(bad()),
                };
                // `HH`, `HHMM` or `HH:MM`.
                let digits = match zone[1..].split_once(':') {
                    Some((hours, minutes)) if hours.len() == 2 && minutes.len() == 2 => {
                        format!("{hours}{minutes}")
                    }
                    Some(_) => return Err(bad()),
                    None => zone[1..].to_string(),
                };
                if !matches!(digits.len(), 2 | 4) || !digits.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(bad());
                }
                let hours: i64 = digits[..2].parse().map_err(|_| bad())?;
                let minutes: i64 = digits
                    .get(2..)
                    .map_or(Ok(0), str::parse)
                    .map_err(|_| bad())?;
                if hours > 23 || minutes > 59 {
                    return Err(bad());
                }
                sign * (hours * 3600 + minutes * 60)
            }
        };
    }
    Ok(days_from_civil(year, month as u32, day as u32) * 86_400 + seconds_of_day - offset)
}

/// Convert a Gregorian year, month and day into days since the epoch (1970-01-01 = 0).
/// The inverse of [`civil_from_days`].
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
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
    fn iso_8601_dates_become_unix_seconds() {
        assert_eq!(unix_from_iso8601("1970-01-01"), Ok(0));
        assert_eq!(unix_from_iso8601("2026-07-25T00:34:56Z"), Ok(1_784_939_696));
        assert_eq!(
            unix_from_iso8601("2026-07-25 02:34:56+02:00"),
            Ok(1_784_939_696)
        );
        assert_eq!(
            unix_from_iso8601("2026-07-24T19:04:56.5-0530"),
            Ok(1_784_939_696)
        );
        assert_eq!(unix_from_iso8601("2024-02-29"), Ok(1_709_164_800));
        // A time without an offset is UTC.
        assert_eq!(unix_from_iso8601("2026-07-25T00:34"), Ok(1_784_939_640));
        for bad in [
            "",
            "2026",
            "2026-02-29",
            "2026-1-01",
            "2026-01-01T24:00",
            "2026-01-01T10",
            "2026-01-01T10:00+2",
            "2026-01-01T10:00:00+24:00",
            "x2026-01-01",
        ] {
            assert!(unix_from_iso8601(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn days_from_civil_inverts_civil_from_days() {
        for z in [-800_000, -1, 0, 1, 59, 60, 365, 11_017, 2_932_896] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
    }

    #[test]
    fn a_creation_date_wins_over_the_wall_clock() {
        let meta = DocumentMetadata {
            creation_date: Some(1_784_939_696),
            ..Default::default()
        };
        assert_eq!(meta.datetime(), (2026, 7, 25, 0, 34, 56));
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

    #[test]
    fn the_document_language_is_kept_as_written_when_it_can_be_a_language_tag() {
        let language = |lang: &str| {
            let mut meta = DocumentMetadata::default();
            meta.fill_language_from_document(Some(lang.to_string()));
            meta.language
        };
        assert_eq!(language("pl").as_deref(), Some("pl"));
        assert_eq!(language(" en-GB\n").as_deref(), Some("en-GB"));
        assert_eq!(language(""), None);
        assert_eq!(language("   "), None);
        assert_eq!(language("p(l"), None);
        assert_eq!(language("język"), None);
        assert_eq!(language("-"), None);
        assert_eq!(language("-en"), None);
        assert_eq!(language("en-"), None);
        assert_eq!(language("en--US"), None);
        assert_eq!(language("123"), None);
        assert_eq!(language("en-abcdefghi"), None);
        assert_eq!(language("en_US"), None);
        assert_eq!(language("zh-Hant-TW").as_deref(), Some("zh-Hant-TW"));
        assert_eq!(language("es-419").as_deref(), Some("es-419"));

        let mut meta = DocumentMetadata::default();
        meta.fill_language_from_document(None);
        assert_eq!(meta.language, None);
    }

    #[test]
    fn an_explicit_language_wins_over_the_html_lang() {
        for document_language in [Some("pl".to_string()), None] {
            let mut meta = DocumentMetadata {
                language: Some("ja".into()),
                ..Default::default()
            };
            meta.fill_language_from_document(document_language);
            assert_eq!(meta.language.as_deref(), Some("ja"));
        }
    }
}
