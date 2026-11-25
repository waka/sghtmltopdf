//! Turning a `multipart/form-data` request into a private directory of files.
//!
//! The request carries the document and the files it refers to (style sheets, images,
//! fonts), so that nothing has to be inlined into the HTML and the server does not have to open
//! local file access. The files are written into a directory made for this request alone,
//! which is the base directory for relative references and the only directory the request may
//! read. The engine's existing confinement to the base directory then applies unchanged: a
//! reference that leaves it (`../`, an absolute path) is refused as it is for the CLI.
//!
//! The layout of the form follows Gotenberg's, so that a client written for it needs few changes:
//!
//! * every file is a part named `files`; the folder part of its name is dropped, so the HTML
//!   refers to each file by its name alone (`<img src="logo.png">`)
//! * `index.html` is the document
//! * `header.html` and `footer.html` are the page header and footer (`--header-html`,
//!   `--footer-html`), and `cover.html` the cover (`--cover`)
//! * `user-style-sheet.css` is the user-origin style sheet (`--user-style-sheet`): stronger than
//!   the browser defaults, weaker than the document's own CSS
//! * a part without a file name is an option, with the name and the meaning of the query
//!   key (`page-size`, `margin-top`, ...) and the same allow list

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::multipart;

/// The file that is the document.
pub const INDEX_FILE: &str = "index.html";
/// The file that is the page header (`--header-html`).
pub const HEADER_FILE: &str = "header.html";
/// The file that is the page footer (`--footer-html`).
pub const FOOTER_FILE: &str = "footer.html";
/// The file that is the cover page (`--cover`).
pub const COVER_FILE: &str = "cover.html";
/// The file that is the user-origin style sheet (`--user-style-sheet`).
pub const USER_STYLE_SHEET_FILE: &str = "user-style-sheet.css";
/// The name of the form part every file is sent in.
pub const FILE_FIELD: &str = "files";

/// The most parts (files and options together) a request may have.
const MAX_PARTS: usize = 1024;

/// The longest file name, in bytes, that every common file system accepts.
const MAX_FILE_NAME_LEN: usize = 255;

/// A directory created for one request and removed, with everything in it, when dropped.
struct UploadDir(PathBuf);

impl UploadDir {
    fn create() -> Result<Self, String> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "sghtmltopdf-upload-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        );
        let path = std::env::temp_dir().join(unique);
        // Only this server's user may look into it.
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        // `create` (not `create_all`) fails when the path already exists, so a directory
        // somebody else made, or a link they planted, is never reused.
        builder
            .create(&path)
            .map_err(|e| format!("cannot create a directory for the upload: {e}"))?;
        Ok(Self(path))
    }
}

impl Drop for UploadDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The files and options of one upload request. The files are removed when this is dropped,
/// so it has to outlive the rendering.
pub struct Upload {
    dir: UploadDir,
    /// The option parts, in the order they were sent. A value is always present, as for a
    /// query key written `key=`.
    pub fields: Vec<(String, Option<String>)>,
    /// The document.
    pub index: PathBuf,
    pub header: Option<PathBuf>,
    pub footer: Option<PathBuf>,
    pub cover: Option<PathBuf>,
    pub user_style_sheet: Option<PathBuf>,
}

impl Upload {
    /// Parse `body` (delimited by `boundary`) and write its files out.
    ///
    /// An error is a message for a `400` response.
    pub fn from_body(body: &[u8], boundary: &str) -> Result<Self, String> {
        let parts = multipart::parse(body, boundary, MAX_PARTS)?;

        // Check everything before anything is written.
        let mut names: Vec<String> = Vec::new();
        let mut files: Vec<(String, &[u8])> = Vec::new();
        let mut fields = Vec::new();
        for part in &parts {
            match &part.file_name {
                Some(raw) => {
                    if part.name != FILE_FIELD {
                        return Err(format!(
                            "the file {raw:?} is in a part named {:?}; send files in parts named {FILE_FIELD:?}",
                            part.name
                        ));
                    }
                    let name = file_name(raw)?;
                    // File systems may ignore case, so two names that differ only in case
                    // would be one file there.
                    let folded = name.to_lowercase();
                    if names.contains(&folded) {
                        return Err(format!("the file {name:?} is sent more than once"));
                    }
                    names.push(folded);
                    files.push((name, part.data));
                }
                None => {
                    let value = String::from_utf8(part.data.to_vec())
                        .map_err(|_| format!("the value of {:?} is not valid UTF-8", part.name))?;
                    fields.push((part.name.clone(), Some(value)));
                }
            }
        }
        let document = files
            .iter()
            .find(|(name, _)| name == INDEX_FILE)
            .ok_or_else(|| {
                format!("send the HTML as a file named {INDEX_FILE} in a part named {FILE_FIELD}")
            })?;
        if document.1.is_empty() {
            return Err(format!("{INDEX_FILE} is empty"));
        }

        let dir = UploadDir::create()?;
        for (name, data) in &files {
            write_new_file(&dir.0.join(name), data)
                .map_err(|e| format!("cannot store the file {name:?}: {e}"))?;
        }
        let path_of = |wanted: &str| {
            files
                .iter()
                .any(|(name, _)| name == wanted)
                .then(|| dir.0.join(wanted))
        };
        Ok(Self {
            index: dir.0.join(INDEX_FILE),
            header: path_of(HEADER_FILE),
            footer: path_of(FOOTER_FILE),
            cover: path_of(COVER_FILE),
            user_style_sheet: path_of(USER_STYLE_SHEET_FILE),
            fields,
            dir,
        })
    }

    /// The directory holding the files: the base for relative references and the only place the
    /// request may read.
    pub fn dir(&self) -> &Path {
        &self.dir.0
    }
}

fn write_new_file(path: &Path, data: &[u8]) -> std::io::Result<()> {
    // `create_new` refuses an existing file or a link in its place.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(data)
}

/// The name a file is stored under: what the client sent, without any folders, and checked
/// to be a plain file name on every common operating system.
///
/// The same name is refused on Linux too, so that a request behaves the same wherever the server
/// runs.
fn file_name(raw: &str) -> Result<String, String> {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let refuse = |why: &str| Err(format!("the file name {raw:?} is not allowed: {why}"));
    if name.is_empty() || name == "." || name == ".." {
        return refuse("it names no file");
    }
    if name.len() > MAX_FILE_NAME_LEN {
        return refuse("it is too long");
    }
    if name
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return refuse("it has a control character or one of < > : \" | ? *");
    }
    // Windows drops a trailing dot or space, which would make two names one.
    if name.ends_with(['.', ' ']) {
        return refuse("it ends in a dot or a space");
    }
    // Windows opens these names as devices, with or without an extension.
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let stem = stem.to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        return refuse("it is a reserved device name on Windows");
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(parts: &[(&str, Option<&str>, &str)]) -> (Vec<u8>, &'static str) {
        let mut body = Vec::new();
        for (name, file_name, data) in parts {
            body.extend_from_slice(b"--B\r\n");
            match file_name {
                Some(file_name) => body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{name}\"; filename=\"{file_name}\"\r\n\r\n"
                    )
                    .as_bytes(),
                ),
                None => body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
                ),
            }
            body.extend_from_slice(data.as_bytes());
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(b"--B--\r\n");
        (body, "B")
    }

    fn upload(parts: &[(&str, Option<&str>, &str)]) -> Result<Upload, String> {
        let (body, boundary) = form(parts);
        Upload::from_body(&body, boundary)
    }

    #[test]
    fn the_files_are_written_under_their_names_and_the_options_are_collected() {
        let upload = upload(&[
            ("files", Some("index.html"), "<p>hi</p>"),
            ("files", Some("style.css"), "p{color:red}"),
            ("page-size", None, "A5"),
            ("files", Some("header.html"), "<b>h</b>"),
            ("files", Some("footer.html"), "<b>f</b>"),
            ("files", Some("cover.html"), "<b>c</b>"),
            ("files", Some("user-style-sheet.css"), "p{margin:0}"),
        ])
        .unwrap();
        let read = |name: &str| std::fs::read_to_string(upload.dir().join(name)).unwrap();
        assert_eq!(read("index.html"), "<p>hi</p>");
        assert_eq!(read("style.css"), "p{color:red}");
        assert_eq!(upload.index, upload.dir().join("index.html"));
        assert_eq!(upload.header, Some(upload.dir().join("header.html")));
        assert_eq!(upload.footer, Some(upload.dir().join("footer.html")));
        assert_eq!(upload.cover, Some(upload.dir().join("cover.html")));
        assert_eq!(
            upload.user_style_sheet,
            Some(upload.dir().join("user-style-sheet.css"))
        );
        assert_eq!(
            upload.fields,
            vec![("page-size".to_string(), Some("A5".to_string()))]
        );
    }

    #[test]
    fn header_footer_and_cover_are_optional() {
        let upload = upload(&[("files", Some("index.html"), "x")]).unwrap();
        assert!(upload.header.is_none() && upload.footer.is_none() && upload.cover.is_none());
        assert!(upload.user_style_sheet.is_none());
    }

    #[test]
    fn the_directory_is_removed_when_the_upload_is_dropped() {
        let upload = upload(&[("files", Some("index.html"), "x")]).unwrap();
        let dir = upload.dir().to_path_buf();
        assert!(dir.is_dir());
        drop(upload);
        assert!(!dir.exists());
    }

    #[test]
    fn two_uploads_never_share_a_directory() {
        let a = upload(&[("files", Some("index.html"), "a")]).unwrap();
        let b = upload(&[("files", Some("index.html"), "b")]).unwrap();
        assert_ne!(a.dir(), b.dir());
    }

    #[test]
    fn the_document_is_required_and_must_not_be_empty() {
        let err = upload(&[("files", Some("style.css"), "x")]).err().unwrap();
        assert!(err.contains("index.html"), "{err}");
        let err = upload(&[("files", Some("index.html"), "")]).err().unwrap();
        assert!(err.contains("empty"), "{err}");
        // An option is not a file, so it cannot stand in for the document.
        assert!(upload(&[("index.html", None, "<p>x</p>")]).is_err());
    }

    #[test]
    fn a_file_must_be_sent_in_a_part_named_files() {
        let err = upload(&[("index.html", Some("index.html"), "x")])
            .err()
            .unwrap();
        assert!(err.contains("files"), "{err}");
    }

    #[test]
    fn folders_are_dropped_from_a_file_name() {
        let upload = upload(&[
            ("files", Some("index.html"), "x"),
            ("files", Some("assets/img/logo.png"), "png"),
            ("files", Some("C:\\Users\\me\\font.ttf"), "ttf"),
        ])
        .unwrap();
        assert!(upload.dir().join("logo.png").is_file());
        assert!(upload.dir().join("font.ttf").is_file());
    }

    #[test]
    fn a_name_that_could_leave_the_directory_or_name_a_device_is_refused() {
        for bad in [
            "..",
            ".",
            "",
            "a/..",
            "dir/",
            "evil\u{0}.png",
            "a:b.png",
            "a*.png",
            "con",
            "CON.txt",
            "nul.png",
            "Com1.css",
            "lpt9",
            "name.",
            "name ",
        ] {
            let result = upload(&[
                ("files", Some("index.html"), "x"),
                ("files", Some(bad), "data"),
            ]);
            assert!(result.is_err(), "{bad:?} must be refused");
        }
        // COM0 and a longer name that merely starts with a device name are ordinary names.
        for good in ["com0.css", "console.css", "aux2.css", "a b.png", "日本.png"] {
            let result = upload(&[
                ("files", Some("index.html"), "x"),
                ("files", Some(good), "data"),
            ]);
            assert!(result.is_ok(), "{good:?} must be accepted");
        }
    }

    #[test]
    fn a_file_sent_twice_is_refused_even_when_only_the_case_differs() {
        for second in ["logo.png", "LOGO.PNG"] {
            let result = upload(&[
                ("files", Some("index.html"), "x"),
                ("files", Some("logo.png"), "1"),
                ("files", Some(second), "2"),
            ]);
            assert!(result.is_err(), "{second} duplicates logo.png");
        }
    }

    #[test]
    fn a_name_that_is_too_long_is_refused() {
        let long = "a".repeat(MAX_FILE_NAME_LEN + 1);
        let result = upload(&[
            ("files", Some("index.html"), "x"),
            ("files", Some(&long), "data"),
        ]);
        assert!(result.is_err());
    }
}
