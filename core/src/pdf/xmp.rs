//! The XMP metadata stream (`/Metadata` in the catalog).
//!
//! It mirrors the Info dictionary: the same fields, the same dates, written as an XMP packet
//! in UTF-8. The stream is left uncompressed, as XMP readers expect to find the packet by
//! scanning the raw file. PDF/A identification (`pdfaid`) is not part of it.

use super::options::{producer_string, DocumentMetadata};

/// The date-time a writer resolved with [`DocumentMetadata::datetime`].
type DateTime = (i64, u32, u32, u32, u32, u32);

/// Build the XMP packet for a document.
///
/// Fields that were not given are left out, as in the Info dictionary. `/ModDate` equals the
/// creation date, so `xmp:ModifyDate` does too.
pub(super) fn xmp_packet(metadata: &DocumentMetadata, datetime: DateTime) -> Vec<u8> {
    let (year, month, day, hour, minute, second) = datetime;
    let date = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");

    let mut body = String::new();
    if let Some(title) = metadata.title.as_deref() {
        body.push_str(&format!(
            "   <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n",
            escape(title)
        ));
    }
    if let Some(author) = metadata.author.as_deref() {
        body.push_str(&format!(
            "   <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n",
            escape(author)
        ));
    }
    if let Some(subject) = metadata.subject.as_deref() {
        body.push_str(&format!(
            "   <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>\n",
            escape(subject)
        ));
    }
    if let Some(keywords) = metadata.keywords.as_deref() {
        body.push_str(&format!(
            "   <pdf:Keywords>{}</pdf:Keywords>\n",
            escape(keywords)
        ));
    }
    body.push_str(&format!(
        "   <pdf:Producer>{}</pdf:Producer>\n",
        escape(&producer_string())
    ));
    if let Some(creator) = metadata.creator.as_deref() {
        body.push_str(&format!(
            "   <xmp:CreatorTool>{}</xmp:CreatorTool>\n",
            escape(creator)
        ));
    }
    body.push_str(&format!(
        "   <xmp:CreateDate>{date}</xmp:CreateDate>\n   <xmp:ModifyDate>{date}</xmp:ModifyDate>\n"
    ));

    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
         <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
         <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" \
         xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n\
         {body}  </rdf:Description>\n \
         </rdf:RDF>\n\
         </x:xmpmeta>\n\
         <?xpacket end=\"w\"?>"
    )
    .into_bytes()
}

/// Escape text for an XML element, dropping the characters XML 1.0 cannot carry at all
/// (control characters other than tab, line feed and carriage return, and U+FFFE/U+FFFF).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if c < ' ' || c == '\u{7f}' || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_characters_are_escaped_and_unrepresentable_ones_dropped() {
        assert_eq!(escape("a & b <c> ]]>"), "a &amp; b &lt;c&gt; ]]&gt;");
        assert_eq!(escape("a\u{0}b\u{8}c\u{ffff}d\te"), "abcd\te");
        assert_eq!(escape("Zażółć Владимир Путин"), "Zażółć Владимир Путин");
    }

    #[test]
    fn the_packet_is_framed_for_xmp_readers() {
        let packet = String::from_utf8(xmp_packet(
            &DocumentMetadata::default(),
            (2026, 1, 2, 3, 4, 5),
        ))
        .unwrap();
        assert!(packet.starts_with("<?xpacket begin=\"\u{feff}\""));
        assert!(packet.ends_with("<?xpacket end=\"w\"?>"));
        assert!(packet.contains("<xmp:CreateDate>2026-01-02T03:04:05Z</xmp:CreateDate>"));
    }
}
