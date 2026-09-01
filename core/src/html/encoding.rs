//! Detecting and decoding the character encoding of the input HTML.
//!
//! sghtmltopdf works in UTF-8 internally, but the input is not always UTF-8.
//! The order of precedence is
//! BOM > an explicit `--encoding` > `<meta charset>` > UTF-8.
//! The BOM comes first to match the HTML Standard's sniffing algorithm.
//!
//! Use [`decode_html`] when the whole input is available, and [`StreamingDecoder`] when
//! processing as it is read. The latter holds an `encoding_rs` incremental decoder and
//! recovers correctly even when a chunk boundary splits a multi-byte character (the path
//! the HTTP server takes when passing the body to `Engine::feed` as it reads).

use encoding_rs::Encoding;

/// How far to look for `<meta charset>`. Equivalent to the HTML Standard's prescan.
const PRESCAN_LIMIT: usize = 1024;

/// Decode the input bytes into a UTF-8 string.
///
/// `declared` is the name given explicitly with `--encoding` (`Shift_JIS`, say).
/// An unknown label is an error (never silently treated as UTF-8).
pub fn decode_html(bytes: &[u8], declared: Option<&str>) -> Result<String, String> {
    if let Some((encoding, bom_len)) = Encoding::for_bom(bytes) {
        let (text, _, _) = encoding.decode(&bytes[bom_len..]);
        return Ok(text.into_owned());
    }

    let encoding = match declared {
        Some(label) => Encoding::for_label(label.as_bytes())
            .ok_or_else(|| format!("unknown encoding: {label}"))?,
        None => match sniff_meta_charset(bytes) {
            Some(encoding) => encoding,
            None => encoding_rs::UTF_8,
        },
    };

    let (text, _, _) = encoding.decode(bytes);
    Ok(text.into_owned())
}

/// Look in the first 1KB for `<meta charset=...>` or
/// `<meta http-equiv="Content-Type" content="...; charset=...">`.
fn sniff_meta_charset(bytes: &[u8]) -> Option<&'static Encoding> {
    let limit = bytes.len().min(PRESCAN_LIMIT);
    let head = String::from_utf8_lossy(&bytes[..limit]).to_ascii_lowercase();

    let mut search_from = 0;
    while let Some(pos) = head[search_from..].find("charset") {
        let after = &head[search_from + pos + "charset".len()..];
        let value = after
            .trim_start()
            .strip_prefix('=')
            .map(|rest| rest.trim_start())
            .unwrap_or("");
        let value: String = value
            .trim_start_matches(['"', '\''])
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        if !value.is_empty() {
            if let Some(encoding) = Encoding::for_label(value.as_bytes()) {
                return Some(encoding);
            }
        }
        search_from += pos + "charset".len();
    }
    None
}

/// A decoder that converts to UTF-8 as the input is read.
///
/// Detecting the encoding needs a fixed number of leading bytes ([`PRESCAN_LIMIT`]), so
/// input is buffered internally until it is settled, and then passed to `encoding_rs`'s
/// incremental decoder. A chunk boundary in the middle of a multi-byte character is fine,
/// because the decoder carries the remainder over.
pub struct StreamingDecoder {
    /// The value given explicitly with `--encoding` (already settled).
    declared: Option<&'static Encoding>,
    state: State,
}

enum State {
    /// Waiting for the encoding to be settled. Holds the bytes accumulated so far.
    Buffering(Vec<u8>),
    Decoding(encoding_rs::Decoder),
}

impl StreamingDecoder {
    /// `declared` is the `--encoding` value. An unknown label is an error here.
    pub fn new(declared: Option<&str>) -> Result<Self, String> {
        let declared = match declared {
            Some(label) => Some(
                Encoding::for_label(label.as_bytes())
                    .ok_or_else(|| format!("unknown encoding: {label}"))?,
            ),
            None => None,
        };
        Ok(Self {
            declared,
            state: State::Buffering(Vec::new()),
        })
    }

    /// Take a chunk and return however much UTF-8 could be settled.
    pub fn push(&mut self, chunk: &[u8]) -> String {
        match &mut self.state {
            State::Buffering(buffer) => {
                buffer.extend_from_slice(chunk);
                if buffer.len() < PRESCAN_LIMIT {
                    return String::new();
                }
                self.settle()
            }
            State::Decoding(decoder) => decode_chunk(decoder, chunk, false),
        }
    }

    /// End of input. Flush whatever is left.
    pub fn finish(&mut self) -> String {
        // If the input never reached [`PRESCAN_LIMIT`], this is where it settles.
        let mut out = if matches!(self.state, State::Buffering(_)) {
            self.settle()
        } else {
            String::new()
        };
        if let State::Decoding(decoder) = &mut self.state {
            out.push_str(&decode_chunk(decoder, &[], true));
        }
        out
    }

    /// Decide the encoding from the accumulated buffer and switch to the decoder.
    fn settle(&mut self) -> String {
        let State::Buffering(buffer) = &mut self.state else {
            return String::new();
        };
        let buffer = std::mem::take(buffer);

        // A BOM wins outright (`new_decoder`'s BOM sniffing handles it).
        // Then `--encoding`, then `<meta charset>`, and finally UTF-8.
        let encoding = self
            .declared
            .or_else(|| sniff_meta_charset(&buffer))
            .unwrap_or(encoding_rs::UTF_8);
        let mut decoder = encoding.new_decoder();
        let out = decode_chunk(&mut decoder, &buffer, false);
        self.state = State::Decoding(decoder);
        out
    }
}

/// Convert one chunk to UTF-8. The output buffer is sized up front with
/// `max_utf8_buffer_length`, so no `OutputFull` loop is needed.
fn decode_chunk(decoder: &mut encoding_rs::Decoder, input: &[u8], last: bool) -> String {
    let capacity = decoder
        .max_utf8_buffer_length(input.len())
        .unwrap_or(input.len().saturating_mul(3) + 4);
    let mut out = String::with_capacity(capacity);
    let (_result, _read, _had_errors) = decoder.decode_to_string(input, &mut out, last);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_input_passes_through() {
        let html = "<html><body><p>日本語</p></body></html>";
        assert_eq!(decode_html(html.as_bytes(), None).unwrap(), html);
    }

    #[test]
    fn an_explicit_encoding_is_used() {
        // "nihongo" in Shift_JIS.
        let bytes = b"\x93\xfa\x96\x7b\x8c\xea";
        assert_eq!(decode_html(bytes, Some("Shift_JIS")).unwrap(), "日本語");
        assert_eq!(decode_html(bytes, Some("sjis")).unwrap(), "日本語");
    }

    #[test]
    fn meta_charset_is_detected_when_no_encoding_is_given() {
        let mut bytes = b"<html><head><meta charset=\"shift_jis\"></head><body><p>".to_vec();
        bytes.extend_from_slice(b"\x93\xfa\x96\x7b\x8c\xea");
        bytes.extend_from_slice(b"</p></body></html>");
        let text = decode_html(&bytes, None).unwrap();
        assert!(text.contains("日本語"), "got: {text}");
    }

    #[test]
    fn http_equiv_content_type_is_also_detected() {
        let mut bytes =
            b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=euc-jp\">"
                .to_vec();
        // "nihongo" in EUC-JP.
        bytes.extend_from_slice(b"</head><body><p>\xc6\xfc\xcb\xdc\xb8\xec</p></body></html>");
        let text = decode_html(&bytes, None).unwrap();
        assert!(text.contains("日本語"), "got: {text}");
    }

    #[test]
    fn an_explicit_encoding_wins_over_meta_charset() {
        let mut bytes = b"<html><head><meta charset=\"utf-8\"></head><body><p>".to_vec();
        bytes.extend_from_slice(b"\x93\xfa\x96\x7b\x8c\xea");
        bytes.extend_from_slice(b"</p></body></html>");
        let text = decode_html(&bytes, Some("Shift_JIS")).unwrap();
        assert!(text.contains("日本語"), "got: {text}");
    }

    #[test]
    fn a_bom_wins_over_everything() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("日本語".as_bytes());
        // The BOM says UTF-8, so it wins over the --encoding setting.
        assert_eq!(decode_html(&bytes, Some("Shift_JIS")).unwrap(), "日本語");
    }

    /// Feed the input in chunks and return the concatenated result.
    fn stream(chunks: &[&[u8]], declared: Option<&str>) -> String {
        let mut decoder = StreamingDecoder::new(declared).unwrap();
        let mut out = String::new();
        for chunk in chunks {
            out.push_str(&decoder.push(chunk));
        }
        out.push_str(&decoder.finish());
        out
    }

    #[test]
    fn streaming_decoder_handles_a_split_multibyte_character() {
        // Split the UTF-8 of "nihongo" mid-character.
        let bytes = "日本語".as_bytes();
        let out = stream(&[&bytes[..4], &bytes[4..]], None);
        assert_eq!(out, "日本語");
    }

    #[test]
    fn streaming_decoder_handles_a_split_shift_jis_character() {
        // Split the Shift_JIS "nihongo" (three 2-byte characters) between its first and second bytes.
        let bytes: &[u8] = b"\x93\xfa\x96\x7b\x8c\xea";
        let out = stream(&[&bytes[..1], &bytes[1..3], &bytes[3..]], Some("Shift_JIS"));
        assert_eq!(out, "日本語");
    }

    #[test]
    fn streaming_decoder_detects_meta_charset_after_buffering() {
        // Make it longer than the prescan window so the settling path is exercised.
        let mut html = b"<html><head><meta charset=\"shift_jis\"></head><body>".to_vec();
        html.extend(std::iter::repeat_n(b'x', PRESCAN_LIMIT));
        html.extend_from_slice(b"<p>\x93\xfa\x96\x7b\x8c\xea</p></body></html>");

        let chunks: Vec<&[u8]> = html.chunks(97).collect();
        let out = stream(&chunks, None);
        assert!(out.contains("日本語"), "got: {out}");
    }

    #[test]
    fn streaming_decoder_flushes_short_input_on_finish() {
        // Input shorter than PRESCAN_LIMIT only settles, and appears, at finish.
        let mut decoder = StreamingDecoder::new(None).unwrap();
        assert_eq!(decoder.push(b"<p>short</p>"), "");
        assert_eq!(decoder.finish(), "<p>short</p>");
    }

    #[test]
    fn streaming_decoder_lets_a_bom_win() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("日本語".as_bytes());
        assert_eq!(stream(&[&bytes], Some("Shift_JIS")), "日本語");
    }

    #[test]
    fn streaming_decoder_rejects_an_unknown_label() {
        assert!(StreamingDecoder::new(Some("no-such-encoding")).is_err());
    }

    #[test]
    fn an_unknown_label_is_an_error() {
        assert!(decode_html(b"x", Some("no-such-encoding")).is_err());
    }
}
