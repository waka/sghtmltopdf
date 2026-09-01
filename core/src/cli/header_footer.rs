//! Translation of the simple header/footer options into `@page` rules.
//!
//! A setting like `--header-center "Page [page]"` is assembled into the CSS text
//! `@page { @top-center { content: "Page " counter(page); } }` and fed through the
//! existing `parse_stylesheet`. That reuses the whole margin-box resolution, shaping
//! and drawing path as is.

use std::fmt::Write as _;

/// Document-level values needed to expand the placeholders.
#[derive(Debug, Clone, Default)]
pub struct PlaceholderValues {
    /// `[title]`/`[doctitle]`.
    pub title: Option<String>,
    /// `[date]` (`YYYY-MM-DD`).
    pub date: String,
    /// `[time]` (`HH:MM:SS`).
    pub time: String,
    /// Any `[name]` added via `--replace <name> <value>`.
    pub replacements: Vec<(String, String)>,
}

impl PlaceholderValues {
    /// Build the values with `[date]`/`[time]` filled in from the current time.
    pub fn new(title: Option<String>, replacements: Vec<(String, String)>) -> Self {
        let (year, month, day, hour, minute, second) = crate::pdf::current_datetime();
        Self {
            title,
            date: format!("{year:04}-{month:02}-{day:02}"),
            time: format!("{hour:02}:{minute:02}:{second:02}"),
            replacements,
        }
    }

    /// Expand every placeholder in the string except the page numbers.
    /// `[page]`/`[topage]` are handled differently depending on the caller's context
    /// (a CSS counter() versus a literal page number), so they are left alone here.
    fn expand_document_level(&self, text: &str) -> String {
        let mut out = text.to_string();
        if let Some(title) = &self.title {
            out = out.replace("[title]", title).replace("[doctitle]", title);
        } else {
            out = out.replace("[title]", "").replace("[doctitle]", "");
        }
        out = out
            .replace("[date]", &self.date)
            .replace("[time]", &self.time);
        out = out.replace("[frompage]", "1");
        for (name, value) in &self.replacements {
            out = out.replace(&format!("[{name}]"), value);
        }
        out
    }

    /// For `--header-html`: expand everything but leave `[page]`/`[topage]` in place.
    /// The engine fills those in per page.
    pub fn expand_keeping_page_tokens(&self, text: &str) -> String {
        self.expand_document_level(text)
    }

    /// For `--header-html`: expand everything, page numbers included.
    pub fn expand_all(&self, text: &str, page: usize, total_pages: Option<usize>) -> String {
        let mut out = self.expand_document_level(text);
        out = out.replace("[page]", &page.to_string());
        let total = total_pages.map(|t| t.to_string()).unwrap_or_default();
        out.replace("[topage]", &total)
    }
}

/// One piece of text to place in a margin box.
#[derive(Debug, Clone)]
pub struct MarginBoxText {
    /// The at-rule name such as `@top-left` (without the leading `@`).
    pub area: &'static str,
    pub text: String,
}

/// The full set of simple header/footer options.
#[derive(Debug, Clone, Default)]
pub struct SimpleHeaderFooter {
    pub boxes: Vec<MarginBoxText>,
    pub header_font_name: Option<String>,
    pub header_font_size: Option<f32>,
    pub footer_font_name: Option<String>,
    pub footer_font_size: Option<f32>,
}

impl SimpleHeaderFooter {
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    /// Build the CSS text of the `@page` rule, or `None` if nothing was specified.
    ///
    /// `[page]`/`[topage]` become `counter(page)`/`counter(pages)`; other placeholders
    /// expand to strings.
    pub fn to_page_css(&self, values: &PlaceholderValues) -> Option<String> {
        if self.is_empty() {
            return None;
        }

        let mut css = String::from("@page {\n");
        for item in &self.boxes {
            let expanded = values.expand_document_level(&item.text);
            let content = content_value(&expanded);
            let is_header = item.area.starts_with("top");
            let (font_name, font_size) = if is_header {
                (self.header_font_name.as_deref(), self.header_font_size)
            } else {
                (self.footer_font_name.as_deref(), self.footer_font_size)
            };

            let _ = write!(css, "  @{} {{ content: {content};", item.area);
            if let Some(name) = font_name {
                let _ = write!(css, " font-family: \"{}\";", escape_css_string(name));
            }
            if let Some(size) = font_size {
                let _ = write!(css, " font-size: {size}px;");
            }
            css.push_str(" }\n");
        }
        css.push_str("}\n");
        Some(css)
    }
}

/// Convert text containing placeholders into a CSS `content` value.
/// `[page]`/`[topage]` become `counter()`, so text fragments and counters alternate.
fn content_value(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut buffer = String::new();

    let mut rest = text;
    while let Some(pos) = rest.find('[') {
        let (before, from_bracket) = rest.split_at(pos);
        buffer.push_str(before);

        let counter = if from_bracket.starts_with("[page]") {
            Some(("counter(page)", "[page]".len()))
        } else if from_bracket.starts_with("[topage]") {
            Some(("counter(pages)", "[topage]".len()))
        } else {
            None
        };

        match counter {
            Some((expr, len)) => {
                if !buffer.is_empty() {
                    parts.push(format!("\"{}\"", escape_css_string(&buffer)));
                    buffer.clear();
                }
                parts.push(expr.to_string());
                rest = &from_bracket[len..];
            }
            None => {
                // An unknown `[...]` is kept as literal text.
                buffer.push('[');
                rest = &from_bracket[1..];
            }
        }
    }
    buffer.push_str(rest);
    if !buffer.is_empty() {
        parts.push(format!("\"{}\"", escape_css_string(&buffer)));
    }

    if parts.is_empty() {
        "\"\"".to_string()
    } else {
        parts.join(" ")
    }
}

/// Escape the contents of a CSS string literal.
fn escape_css_string(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> PlaceholderValues {
        PlaceholderValues {
            title: Some("Invoice".to_string()),
            date: "2026-07-25".to_string(),
            time: "12:34:56".to_string(),
            replacements: vec![("customer".to_string(), "Waka Store".to_string())],
        }
    }

    fn simple(area: &'static str, text: &str) -> SimpleHeaderFooter {
        SimpleHeaderFooter {
            boxes: vec![MarginBoxText {
                area,
                text: text.to_string(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn plain_text_becomes_a_content_string() {
        let css = simple("top-center", "hello")
            .to_page_css(&values())
            .unwrap();
        assert!(
            css.contains("@top-center { content: \"hello\";"),
            "got: {css}"
        );
    }

    #[test]
    fn page_placeholders_become_counters() {
        let css = simple("bottom-center", "Page [page] of [topage]")
            .to_page_css(&values())
            .unwrap();
        assert!(
            css.contains(r#"content: "Page " counter(page) " of " counter(pages);"#),
            "got: {css}"
        );
    }

    #[test]
    fn document_level_placeholders_are_expanded_as_text() {
        let css = simple("top-left", "[title] / [date] / [time] / [customer]")
            .to_page_css(&values())
            .unwrap();
        assert!(
            css.contains(r#"content: "Invoice / 2026-07-25 / 12:34:56 / Waka Store";"#),
            "got: {css}"
        );
    }

    #[test]
    fn quotes_and_backslashes_are_escaped() {
        let css = simple("top-left", r#"a"b\c"#)
            .to_page_css(&values())
            .unwrap();
        assert!(css.contains(r#"content: "a\"b\\c";"#), "got: {css}");
    }

    #[test]
    fn an_unknown_placeholder_stays_as_literal_text() {
        let css = simple("top-left", "[unknown] x")
            .to_page_css(&values())
            .unwrap();
        assert!(css.contains(r#"content: "[unknown] x";"#), "got: {css}");
    }

    #[test]
    fn font_options_apply_to_the_matching_side() {
        let hf = SimpleHeaderFooter {
            boxes: vec![
                MarginBoxText {
                    area: "top-center",
                    text: "H".to_string(),
                },
                MarginBoxText {
                    area: "bottom-center",
                    text: "F".to_string(),
                },
            ],
            header_font_size: Some(8.0),
            footer_font_name: Some("Mincho".to_string()),
            ..Default::default()
        };
        let css = hf.to_page_css(&values()).unwrap();
        assert!(
            css.contains("@top-center { content: \"H\"; font-size: 8px; }"),
            "got: {css}"
        );
        assert!(
            css.contains("@bottom-center { content: \"F\"; font-family: \"Mincho\"; }"),
            "got: {css}"
        );
    }

    #[test]
    fn nothing_specified_produces_no_css() {
        assert!(SimpleHeaderFooter::default()
            .to_page_css(&values())
            .is_none());
    }

    #[test]
    fn expand_all_fills_page_numbers_for_header_html() {
        let text = "[title] [page]/[topage] [date]";
        let out = values().expand_all(text, 3, Some(10));
        assert_eq!(out, "Invoice 3/10 2026-07-25");
    }
}
