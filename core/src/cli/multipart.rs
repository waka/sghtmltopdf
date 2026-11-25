//! A parser for `multipart/form-data` request bodies.
//!
//! The format is RFC 7578, which builds on the multipart syntax of RFC 2046 §5.1.1:
//!
//! ```text
//! delimiter      = CRLF "--" boundary
//! boundary line  = delimiter transport-padding CRLF     (opens a part)
//! close line     = delimiter "--"                       (ends the body)
//! part           = headers CRLF CRLF data               (or CRLF data, with no headers)
//! ```
//!
//! The first delimiter may be at the very start of the body (the CRLF belongs to the line
//! before it, which does not exist), and anything before it (the preamble) or after the close
//! line (the epilogue) is ignored. The body is parsed in memory, so the caller bounds its size.
//! The parts borrow from it, which avoids a second copy of every uploaded file.

/// One part of a `multipart/form-data` body.
#[derive(Debug, PartialEq, Eq)]
pub struct Part<'a> {
    /// The `name` of its `Content-Disposition: form-data` header.
    pub name: String,
    /// The `filename` of that header. A part has one when it is a file upload, and none when
    /// it is a plain form field.
    pub file_name: Option<String>,
    /// The bytes of the part, without the CRLF that precedes the next delimiter.
    pub data: &'a [u8],
}

/// Why a `Content-Type` header does not name a `multipart/form-data` body that can be read.
#[derive(Debug, PartialEq, Eq)]
pub enum ContentTypeError {
    /// Another `multipart/*` type, which this does not read.
    Unsupported(String),
    /// `multipart/form-data` without a usable `boundary` parameter.
    Malformed(String),
}

/// The longest boundary RFC 2046 allows.
const MAX_BOUNDARY_LEN: usize = 70;

/// The boundary of a `multipart/form-data` request, or `None` when the `Content-Type` is not
/// one (a raw HTML body, say).
pub fn boundary(content_type: Option<&str>) -> Result<Option<String>, ContentTypeError> {
    let Some(content_type) = content_type else {
        return Ok(None);
    };
    let (media_type, parameters) = match content_type.split_once(';') {
        Some((media_type, parameters)) => (media_type, parameters),
        None => (content_type, ""),
    };
    let media_type = media_type.trim();
    if !media_type.eq_ignore_ascii_case("multipart/form-data") {
        if media_type
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("multipart/"))
        {
            return Err(ContentTypeError::Unsupported(format!(
                "{media_type} is not supported; send multipart/form-data"
            )));
        }
        return Ok(None);
    }
    let boundary = parse_parameters(parameters)
        .into_iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("boundary"))
        .map(|(_, value)| value)
        .ok_or_else(|| {
            ContentTypeError::Malformed(
                "multipart/form-data needs a boundary parameter".to_string(),
            )
        })?;
    if boundary.is_empty() || boundary.len() > MAX_BOUNDARY_LEN {
        return Err(ContentTypeError::Malformed(format!(
            "the multipart boundary must be 1 to {MAX_BOUNDARY_LEN} characters"
        )));
    }
    if boundary.contains(['\r', '\n']) {
        return Err(ContentTypeError::Malformed(
            "the multipart boundary must not contain a line break".to_string(),
        ));
    }
    Ok(Some(boundary))
}

/// Split the parts of `body`, which is delimited by `boundary`.
///
/// More than `max_parts` parts is an error, so that a body of tiny parts cannot make the
/// server do unbounded work.
pub fn parse<'a>(
    body: &'a [u8],
    boundary: &str,
    max_parts: usize,
) -> Result<Vec<Part<'a>>, String> {
    let delimiter = format!("\r\n--{boundary}").into_bytes();
    let mut parts = Vec::new();

    // The first delimiter has no CRLF in front of it when the body starts with it.
    let mut position =
        if body.starts_with(&delimiter[2..]) && is_delimiter_end(body, delimiter.len() - 2) {
            delimiter.len() - 2
        } else {
            find_delimiter(body, &delimiter, 0)?.1
        };

    loop {
        // `position` is just past a delimiter: either the close line or a part follows.
        if body[position..].starts_with(b"--") {
            return Ok(parts);
        }
        position = skip_transport_padding(body, position)?;

        let (header_bytes, data_start) = if body[position..].starts_with(b"\r\n") {
            (&body[position..position], position + 2)
        } else {
            let end = find(&body[position..], b"\r\n\r\n")
                .ok_or_else(|| "a multipart part has no end to its headers".to_string())?;
            (&body[position..position + end], position + end + 4)
        };
        let (data_end, next) = find_delimiter(body, &delimiter, data_start)?;
        if parts.len() == max_parts {
            return Err(format!("more than {max_parts} multipart parts"));
        }
        parts.push(parse_part(header_bytes, &body[data_start..data_end])?);
        position = next;
    }
}

/// The offset of the next delimiter at or after `from`, and the offset just past it.
///
/// A delimiter must be followed by transport padding and a line break, or by `--`
/// (RFC 2046 §5.1.1). Without that check, a boundary of `abc` would end a part at a line
/// of the data that merely starts with `--abcd`.
fn find_delimiter(body: &[u8], delimiter: &[u8], from: usize) -> Result<(usize, usize), String> {
    let mut search_from = from;
    while let Some(offset) = find(&body[search_from..], delimiter) {
        let start = search_from + offset;
        let end = start + delimiter.len();
        if is_delimiter_end(body, end) {
            return Ok((start, end));
        }
        search_from = start + 1;
    }
    Err("the multipart body is not terminated by its closing boundary".to_string())
}

/// Whether what follows a delimiter ending at `end` is valid: `--`, or optional spaces and tabs
/// and then CRLF.
fn is_delimiter_end(body: &[u8], end: usize) -> bool {
    let rest = &body[end..];
    if rest.starts_with(b"--") {
        return true;
    }
    let padding = rest
        .iter()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    rest[padding..].starts_with(b"\r\n")
}

/// Skip the spaces and tabs after a delimiter and the CRLF that ends its line.
fn skip_transport_padding(body: &[u8], mut position: usize) -> Result<usize, String> {
    while matches!(body.get(position), Some(b' ' | b'\t')) {
        position += 1;
    }
    if body[position..].starts_with(b"\r\n") {
        Ok(position + 2)
    } else {
        Err("a multipart boundary line is not followed by a line break".to_string())
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_part<'a>(header_bytes: &[u8], data: &'a [u8]) -> Result<Part<'a>, String> {
    let headers = std::str::from_utf8(header_bytes)
        .map_err(|_| "the headers of a multipart part are not valid UTF-8".to_string())?;

    // A line that starts with a space or a tab continues the header above it.
    let mut lines: Vec<String> = Vec::new();
    for line in headers.split("\r\n").filter(|line| !line.is_empty()) {
        match lines.last_mut() {
            Some(previous) if line.starts_with([' ', '\t']) => {
                previous.push(' ');
                previous.push_str(line.trim_start());
            }
            _ => lines.push(line.to_string()),
        }
    }

    let disposition = lines
        .iter()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-disposition"))
        .map(|(_, value)| value)
        .ok_or_else(|| "a multipart part has no Content-Disposition header".to_string())?;
    let (kind, parameters) = match disposition.split_once(';') {
        Some((kind, parameters)) => (kind, parameters),
        None => (disposition, ""),
    };
    if !kind.trim().eq_ignore_ascii_case("form-data") {
        return Err(format!(
            "a multipart part has Content-Disposition `{}`, not form-data",
            kind.trim()
        ));
    }
    let parameters = parse_parameters(parameters);
    let parameter = |wanted: &str| {
        parameters
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.clone())
    };
    let name = parameter("name")
        .ok_or_else(|| "a multipart part has no name in its Content-Disposition".to_string())?;
    Ok(Part {
        name,
        file_name: parameter("filename"),
        data,
    })
}

/// Parse `a=b; c="d;e"` into name and value pairs.
///
/// A value is a token or a quoted string. In a quoted string, `\"` stands for a quote and
/// `\\` for a backslash. Any other backslash is kept as it is, because an old browser sends a
/// Windows path as the file name (`C:\dir\file.png`) without escaping it.
fn parse_parameters(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace() || *c == ';') {
            chars.next();
        }
        let mut name = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ';' {
                break;
            }
            name.push(c);
            chars.next();
        }
        if chars.peek().is_none() && name.trim().is_empty() {
            return out;
        }
        let mut value = String::new();
        if chars.peek() == Some(&'=') {
            chars.next();
            if chars.peek() == Some(&'"') {
                chars.next();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('"' | '\\')) => {
                            value.push(chars.next().expect("peeked"));
                        }
                        c => value.push(c),
                    }
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c == ';' {
                        break;
                    }
                    value.push(c);
                    chars.next();
                }
                value = value.trim_end().to_string();
            }
        }
        let name = name.trim();
        if !name.is_empty() {
            out.push((name.to_string(), value));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example of RFC 7578 §4.1 (the file contents replaced by plain text).
    const RFC7578_BODY: &[u8] = b"--AaB03x\r\n\
        Content-Disposition: form-data; name=\"submit-name\"\r\n\
        \r\n\
        Larry\r\n\
        --AaB03x\r\n\
        Content-Disposition: form-data; name=\"files\"; filename=\"file1.txt\"\r\n\
        Content-Type: text/plain\r\n\
        \r\n\
        ... contents of file1.txt ...\r\n\
        --AaB03x--\r\n";

    #[test]
    fn the_rfc_7578_example_is_split_into_a_field_and_a_file() {
        let parts = parse(RFC7578_BODY, "AaB03x", 10).unwrap();
        assert_eq!(
            parts,
            vec![
                Part {
                    name: "submit-name".to_string(),
                    file_name: None,
                    data: b"Larry",
                },
                Part {
                    name: "files".to_string(),
                    file_name: Some("file1.txt".to_string()),
                    data: b"... contents of file1.txt ...",
                },
            ]
        );
    }

    #[test]
    fn a_preamble_and_an_epilogue_are_ignored() {
        // RFC 2046 §5.1.1: text before the first delimiter and after the close delimiter is
        // to be ignored. The preamble ends in the CRLF that starts the first delimiter.
        let body = b"this is a preamble\r\n--b\r\n\
            Content-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--b--\r\nepilogue";
        let parts = parse(body, "b", 10).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].data, b"1");
    }

    #[test]
    fn data_may_contain_binary_bytes_and_a_partial_boundary() {
        // The data holds a line that starts with the boundary text but continues
        // (`--bX`), which is not a delimiter (RFC 2046 §5.1.1: it must be followed by
        // padding and a line break, or by `--`), and a bare CR and LF.
        let mut body =
            b"--b\r\nContent-Disposition: form-data; name=\"files\"; filename=\"x.bin\"\r\n\r\n"
                .to_vec();
        let data: &[u8] = b"\x00\xff\r--b\n\r\n--bX still data\r\n--";
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n--b--");
        let parts = parse(&body, "b", 10).unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].data, data);
    }

    #[test]
    fn a_part_may_be_empty() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n\r\n--b--";
        let parts = parse(body, "b", 10).unwrap();
        assert_eq!(parts[0].data, b"");
    }

    #[test]
    fn transport_padding_after_a_boundary_is_allowed() {
        let body = b"--b \t\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n--b  \r\n\
            Content-Disposition: form-data; name=\"c\"\r\n\r\n2\r\n--b--";
        let parts = parse(body, "b", 10).unwrap();
        assert_eq!(parts.len(), 2);
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let body = b"--b\r\ncontent-disposition: FORM-DATA; NAME=\"a\"; FileName=\"f.txt\"\r\n\r\n1\r\n--b--";
        let parts = parse(body, "b", 10).unwrap();
        assert_eq!(parts[0].name, "a");
        assert_eq!(parts[0].file_name.as_deref(), Some("f.txt"));
    }

    #[test]
    fn a_quoted_value_may_contain_a_semicolon_a_quote_and_utf8() {
        let body = "--b\r\nContent-Disposition: form-data; name=\"files\"; \
            filename=\"a;b \\\"c\\\" 日本.png\"\r\n\r\nx\r\n--b--";
        let parts = parse(body.as_bytes(), "b", 10).unwrap();
        assert_eq!(parts[0].file_name.as_deref(), Some("a;b \"c\" 日本.png"));
    }

    #[test]
    fn a_windows_path_keeps_its_backslashes() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"files\"; \
            filename=\"C:\\dir\\file.png\"\r\n\r\nx\r\n--b--";
        let parts = parse(body, "b", 10).unwrap();
        assert_eq!(parts[0].file_name.as_deref(), Some("C:\\dir\\file.png"));
    }

    #[test]
    fn a_folded_header_line_is_joined() {
        let body = b"--b\r\nContent-Disposition: form-data;\r\n name=\"a\"\r\n\r\n1\r\n--b--";
        let parts = parse(body, "b", 10).unwrap();
        assert_eq!(parts[0].name, "a");
    }

    #[test]
    fn a_body_without_its_closing_boundary_is_an_error() {
        let body = b"--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n";
        assert!(parse(body, "b", 10).is_err());
        assert!(parse(b"no boundary here", "b", 10).is_err());
        assert!(parse(b"", "b", 10).is_err());
    }

    #[test]
    fn a_part_that_is_not_form_data_or_has_no_name_is_an_error() {
        let attachment = b"--b\r\nContent-Disposition: attachment; name=\"a\"\r\n\r\n1\r\n--b--";
        assert!(parse(attachment, "b", 10).is_err());
        let nameless = b"--b\r\nContent-Disposition: form-data\r\n\r\n1\r\n--b--";
        assert!(parse(nameless, "b", 10).is_err());
        let headerless = b"--b\r\n\r\n1\r\n--b--";
        assert!(parse(headerless, "b", 10).is_err());
    }

    #[test]
    fn more_parts_than_allowed_is_an_error() {
        let mut body = Vec::new();
        for _ in 0..3 {
            body.extend_from_slice(
                b"--b\r\nContent-Disposition: form-data; name=\"a\"\r\n\r\n1\r\n",
            );
        }
        body.extend_from_slice(b"--b--");
        assert!(parse(&body, "b", 3).is_ok());
        assert!(parse(&body, "b", 2).is_err());
    }

    #[test]
    fn the_boundary_is_read_from_the_content_type() {
        let get = |value| boundary(Some(value));
        assert_eq!(
            get("multipart/form-data; boundary=AaB03x"),
            Ok(Some("AaB03x".to_string()))
        );
        // Media type and parameter names are case-insensitive; the value may be quoted.
        assert_eq!(
            get("Multipart/Form-Data; Boundary=\"a b;c\""),
            Ok(Some("a b;c".to_string()))
        );
        assert_eq!(
            get("multipart/form-data; charset=utf-8; boundary=x"),
            Ok(Some("x".to_string()))
        );
    }

    #[test]
    fn a_content_type_that_is_not_multipart_is_not_an_upload() {
        assert_eq!(boundary(None), Ok(None));
        assert_eq!(boundary(Some("text/html; charset=utf-8")), Ok(None));
        assert_eq!(
            boundary(Some("application/x-www-form-urlencoded")),
            Ok(None)
        );
    }

    #[test]
    fn a_multipart_content_type_that_cannot_be_read_is_an_error() {
        assert!(matches!(
            boundary(Some("multipart/mixed; boundary=x")),
            Err(ContentTypeError::Unsupported(_))
        ));
        for bad in [
            "multipart/form-data",
            "multipart/form-data; boundary=",
            &format!("multipart/form-data; boundary={}", "x".repeat(71)),
        ] {
            assert!(
                matches!(boundary(Some(bad)), Err(ContentTypeError::Malformed(_))),
                "{bad}"
            );
        }
        // 70 characters is the longest RFC 2046 allows.
        let longest = format!("multipart/form-data; boundary={}", "x".repeat(70));
        assert!(boundary(Some(&longest)).unwrap().is_some());
    }
}
