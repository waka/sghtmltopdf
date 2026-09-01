//! Building the HTML for the table of contents (`--toc`).
//!
//! The structure and default styles match the output of wkhtmltopdf's default TOC XSL
//! (`src/lib/tocstylesheet.cc`).
//! Nesting is expressed with nested `<ul>`, and each item is
//! `<li><div><a>heading</a><span>page number</span></div><ul>children</ul></li>`.

use std::fmt::Write as _;

/// One table-of-contents entry.
#[derive(Debug, Clone, PartialEq)]
pub struct TocEntry {
    /// Heading level (`h1` = 1 ... `h6` = 6).
    pub level: u8,
    pub title: String,
    /// Page number to display.
    pub page: usize,
    /// Named destination to link to.
    pub anchor: String,
    /// Destination name attached to this entry itself, for `--enable-toc-back-links`.
    pub back_anchor: Option<String>,
}

/// Options affecting how the table of contents looks (wkhtmltopdf compatible).
#[derive(Debug, Clone)]
pub struct TocOptions {
    pub header_text: String,
    /// Indentation of `ul` (written out verbatim as a CSS length).
    pub level_indentation: String,
    /// Font-size ratio per nesting level (default 0.8).
    pub text_size_shrink: f32,
    /// Whether to draw a dashed underline on the `div`.
    pub dotted_lines: bool,
    /// Whether to link entries to their headings.
    pub links: bool,
}

impl Default for TocOptions {
    fn default() -> Self {
        Self {
            header_text: "Table of Contents".to_string(),
            level_indentation: "1em".to_string(),
            text_size_shrink: 0.8,
            dotted_lines: true,
            links: true,
        }
    }
}

/// Build the table-of-contents HTML document.
pub fn build_toc_html(entries: &[TocEntry], options: &TocOptions) -> String {
    let mut html = String::from("<html><head><style>\n");
    let _ = write!(
        html,
        "h1 {{ text-align: center; font-size: 20px; }}\n\
         span {{ float: right; }}\n\
         li {{ list-style: none; }}\n\
         ul {{ font-size: 20px; padding-left: {}; }}\n\
         ul ul {{ font-size: {}%; }}\n\
         a {{ text-decoration: none; color: black; }}\n",
        options.level_indentation,
        (options.text_size_shrink * 100.0).round() as i32,
    );
    if options.dotted_lines {
        html.push_str("div { border-bottom: 1px dashed rgb(200,200,200); }\n");
    }
    html.push_str("</style></head><body>\n");
    let _ = writeln!(html, "<h1>{}</h1>", escape_html(&options.header_text));

    write_entries(&mut html, entries, options);

    html.push_str("</body></html>");
    html
}

/// Build nested `<ul>` from the relative heading levels.
fn write_entries(html: &mut String, entries: &[TocEntry], options: &TocOptions) {
    if entries.is_empty() {
        html.push_str("<ul></ul>\n");
        return;
    }

    html.push_str("<ul>\n");
    // Stack of levels for the `<li>` elements currently open.
    let mut open_levels: Vec<u8> = Vec::new();

    for entry in entries {
        while let Some(&top) = open_levels.last() {
            if entry.level > top {
                // Going deeper: open a child list.
                html.push_str("<ul>\n");
                break;
            }
            // Same level or shallower: close the open entry.
            html.push_str("</li>\n");
            open_levels.pop();
            if let Some(&next_top) = open_levels.last() {
                if entry.level > next_top {
                    break;
                }
                html.push_str("</ul>\n");
            }
        }

        write_entry(html, entry, options);
        open_levels.push(entry.level);
    }

    // Close what is left.
    while open_levels.pop().is_some() {
        html.push_str("</li>\n");
        if !open_levels.is_empty() {
            html.push_str("</ul>\n");
        }
    }
    html.push_str("</ul>\n");
}

fn write_entry(html: &mut String, entry: &TocEntry, options: &TocOptions) {
    html.push_str("<li><div>");
    let title = escape_html(&entry.title);
    if options.links {
        let mut attrs = format!(" href=\"#{}\"", escape_html(&entry.anchor));
        if let Some(back) = &entry.back_anchor {
            let _ = write!(attrs, " id=\"{}\"", escape_html(back));
        }
        let _ = write!(html, "<a{attrs}>{title}</a>");
    } else if let Some(back) = &entry.back_anchor {
        let _ = write!(html, "<a id=\"{}\">{title}</a>", escape_html(back));
    } else {
        html.push_str(&title);
    }
    let _ = write!(html, "<span>{}</span></div>", entry.page);
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(level: u8, title: &str, page: usize) -> TocEntry {
        TocEntry {
            level,
            title: title.to_string(),
            page,
            anchor: format!("a{page}"),
            back_anchor: None,
        }
    }

    #[test]
    fn the_default_style_matches_the_wkhtmltopdf_defaults() {
        let html = build_toc_html(&[entry(1, "x", 1)], &TocOptions::default());
        assert!(html.contains("h1 { text-align: center; font-size: 20px; }"));
        assert!(html.contains("span { float: right; }"));
        assert!(html.contains("li { list-style: none; }"));
        assert!(html.contains("ul { font-size: 20px; padding-left: 1em; }"));
        assert!(html.contains("ul ul { font-size: 80%; }"));
        assert!(html.contains("a { text-decoration: none; color: black; }"));
        assert!(html.contains("div { border-bottom: 1px dashed rgb(200,200,200); }"));
        assert!(html.contains("<h1>Table of Contents</h1>"));
    }

    #[test]
    fn an_entry_uses_the_div_a_span_structure() {
        let html = build_toc_html(&[entry(1, "Introduction", 3)], &TocOptions::default());
        assert!(
            // Contains `"#`, so the raw string needs `r##` delimiters.
            html.contains(r##"<li><div><a href="#a3">Introduction</a><span>3</span></div>"##),
            "got: {html}"
        );
    }

    #[test]
    fn deeper_levels_are_nested_in_child_uls() {
        let html = build_toc_html(
            &[entry(1, "A", 1), entry(2, "A-1", 2), entry(1, "B", 3)],
            &TocOptions::default(),
        );
        // A child <ul> opens under A and closes before B.
        let a = html.find("A</a>").unwrap();
        let child_ul = html[a..].find("<ul>").unwrap() + a;
        let a1 = html.find("A-1</a>").unwrap();
        let b = html.find("B</a>").unwrap();
        assert!(
            child_ul < a1,
            "child <ul> must open before the nested entry"
        );
        assert!(a1 < b);
        // Opening and closing tags balance.
        assert_eq!(html.matches("<ul>").count(), html.matches("</ul>").count());
        assert_eq!(html.matches("<li>").count(), html.matches("</li>").count());
    }

    #[test]
    fn a_level_jump_counts_as_one_nesting_step() {
        // A jump from h1 to h3 still only nests one level.
        let html = build_toc_html(
            &[entry(1, "A", 1), entry(3, "A-x", 2)],
            &TocOptions::default(),
        );
        assert_eq!(html.matches("<ul>").count(), 2);
        assert_eq!(html.matches("<ul>").count(), html.matches("</ul>").count());
    }

    #[test]
    fn options_change_the_generated_css_and_links() {
        let options = TocOptions {
            header_text: "Contents".to_string(),
            level_indentation: "2em".to_string(),
            text_size_shrink: 0.5,
            dotted_lines: false,
            links: false,
        };
        let html = build_toc_html(&[entry(1, "A", 1)], &options);
        assert!(html.contains("<h1>Contents</h1>"));
        assert!(html.contains("padding-left: 2em;"));
        assert!(html.contains("ul ul { font-size: 50%; }"));
        assert!(!html.contains("border-bottom"));
        assert!(!html.contains("<a href"), "links must be disabled: {html}");
        assert!(
            html.contains("<li><div>A<span>1</span></div>"),
            "got: {html}"
        );
    }

    #[test]
    fn back_links_put_an_id_on_the_toc_entry() {
        let mut e = entry(1, "A", 1);
        e.back_anchor = Some("__sgtocback_0".to_string());
        let html = build_toc_html(&[e], &TocOptions::default());
        assert!(html.contains(r#"id="__sgtocback_0""#), "got: {html}");
    }

    #[test]
    fn html_special_characters_are_escaped() {
        let html = build_toc_html(&[entry(1, "a<b>&\"c\"", 1)], &TocOptions::default());
        assert!(html.contains("a&lt;b&gt;&amp;&quot;c&quot;"), "got: {html}");
    }

    #[test]
    fn no_entries_still_produces_a_valid_document() {
        let html = build_toc_html(&[], &TocOptions::default());
        assert!(html.contains("<ul></ul>"));
        assert!(html.ends_with("</body></html>"));
    }
}
