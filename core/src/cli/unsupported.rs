//! Reject options that wkhtmltopdf has but sghtmltopdf does not implement.

/// Why an option is unsupported (options sharing a reason are grouped).
struct Reason {
    message: &'static str,
    options: &'static [&'static str],
}

const REASONS: &[Reason] = &[
    Reason {
        message: "sghtmltopdf does not execute JavaScript (a deliberate non-goal).\n  \
                  Build dynamically generated content into the HTML before passing it in",
        options: &[
            "--enable-javascript",
            "--disable-javascript",
            "--javascript-delay",
            "--run-script",
            "--window-status",
            "--debug-javascript",
            "--no-debug-javascript",
            "--stop-slow-scripts",
            "--no-stop-slow-scripts",
            "--enable-plugins",
            "--disable-plugins",
        ],
    },
    Reason {
        message: "PDF outlines (bookmarks) are not supported.\n  \
                  If you need a list of the headings in a document, --toc builds a table of contents page",
        options: &[
            "--outline",
            "--no-outline",
            "--outline-depth",
            "--dump-outline",
            "--exclude-from-outline",
            "--include-in-outline",
        ],
    },
    Reason {
        message: "XSLT is not supported.\n  \
                  The look of the table of contents can be changed with options such as\n  \
                  --toc-header-text and with CSS passed via --user-style-sheet",
        options: &["--xsl-style-sheet", "--dump-default-toc-xsl"],
    },
    Reason {
        message: "Re-encoding or downscaling images is not supported\n  \
                  (JPEGs are embedded as-is, without being decoded).\n  \
                  Resize the images yourself before passing them in",
        options: &["--image-quality", "--image-dpi"],
    },
    Reason {
        message: "Fetching with authentication or through a proxy is not supported.\n  \
                  Fetch such resources yourself and pass them in as a local path\n  \
                  or a data: URI",
        options: &[
            "--proxy",
            "--proxy-hostname-lookup",
            "--bypass-proxy-for",
            "--cookie",
            "--cookie-jar",
            "--custom-header",
            "--custom-header-propagation",
            "--no-custom-header-propagation",
            "--username",
            "--password",
            "--ssl-crt-path",
            "--ssl-key-path",
            "--ssl-key-password",
            "--post",
            "--post-file",
        ],
    },
    Reason {
        message: "WebKit-specific rendering settings are not supported\n  \
                  (sghtmltopdf always renders for print media and has no\n  \
                  concept of a viewport)",
        options: &[
            "--disable-smart-shrinking",
            "--enable-smart-shrinking",
            "--viewport-size",
            "--lowquality",
            "--print-media-type",
            "--no-print-media-type",
            "--use-xserver",
        ],
    },
    Reason {
        message: "Generating PDF forms (AcroForm) is not supported.\n  \
                  Form elements are drawn as static appearance only",
        options: &["--enable-forms", "--disable-forms"],
    },
    Reason {
        message: "Replacing the look of checkboxes and similar controls with SVG is not\n  \
                  supported (the built-in rendering is always used)",
        options: &[
            "--checkbox-svg",
            "--checkbox-checked-svg",
            "--radiobutton-svg",
            "--radiobutton-checked-svg",
        ],
    },
    Reason {
        message: "A copy count has no meaning when generating a PDF, so it is not supported",
        options: &["--copies", "--collate", "--no-collate"],
    },
    Reason {
        message: "Standard input is used for the HTML, so it cannot be used to read arguments",
        options: &["--read-args-from-stdin"],
    },
    Reason {
        message: "No cache of fetched resources is kept",
        options: &["--cache-dir"],
    },
    Reason {
        message: "See the documentation and the README",
        options: &[
            "--extended-help",
            "--htmldoc",
            "--manpage",
            "--readme",
            "--license",
        ],
    },
];

/// If `name` (a long option name, including the leading `--`) is unsupported, return why.
pub fn unsupported_reason(name: &str) -> Option<&'static str> {
    REASONS
        .iter()
        .find(|reason| reason.options.contains(&name))
        .map(|reason| reason.message)
}

/// Return an error message if the command line contains an unsupported option.
///
/// The `--foo=bar` form is handled too. Anything after `--` is treated as a value and not matched.
pub fn check_arguments(args: &[String]) -> Option<String> {
    for arg in args {
        if arg == "--" {
            break;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        if let Some(reason) = unsupported_reason(name) {
            return Some(format!("{name} is not supported.\n  {reason}"));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn javascript_options_are_rejected_with_a_reason() {
        let message = check_arguments(&["--enable-javascript".to_string()]).unwrap();
        assert!(message.contains("--enable-javascript is not supported"));
        assert!(message.contains("JavaScript"));
    }

    #[test]
    fn the_value_form_is_also_detected() {
        assert!(check_arguments(&["--outline-depth=3".to_string()]).is_some());
    }

    #[test]
    fn supported_options_pass_through() {
        assert!(check_arguments(&[
            "input.html".to_string(),
            "--page-size".to_string(),
            "A4".to_string(),
            "--toc".to_string(),
        ])
        .is_none());
    }

    #[test]
    fn arguments_after_a_double_dash_are_values() {
        assert!(check_arguments(&["--".to_string(), "--outline".to_string()]).is_none());
    }

    #[test]
    fn each_reason_mentions_an_alternative_or_the_cause() {
        // Every reason must say why it is unsupported (guards against an empty message).
        for reason in REASONS {
            assert!(reason.message.len() > 10);
            assert!(!reason.options.is_empty());
        }
    }

    #[test]
    fn the_query_side_can_reuse_the_same_table() {
        // The HTTP server matches query keys (without `--`), so it prepends `--` before looking up.
        assert!(unsupported_reason("--xsl-style-sheet").is_some());
        assert!(unsupported_reason("--page-size").is_none());
    }
}
