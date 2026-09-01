//! Converting SVG bytes into a PDF Form XObject (no rasterisation).
//!
//! Parsing and SVG normalisation (expanding `use`, resolving styles, folding coordinate
//! systems) are done by usvg, and the translation from there into a PDF content stream by
//! svg2pdf. Both come from typst, and svg2pdf returns a `pdf_writer::Chunk` (the same type
//! this crate uses to write the whole document), so the conversion result can be spliced
//! straight into the document without going through a byte string.
//!
//! # How it differs from a raster
//!
//! A raster image becomes one Image XObject, but an SVG becomes **a cluster of several
//! objects**: one Form XObject plus the gradients, `ExtGState`s and nested XObjects it
//! references. So Ref allocation needs "as many as there are objects in the chunk" rather
//! than one or two, and [`renumber_into_document`] takes care of that.
//!
//! From the drawing side it is used exactly like a raster: the Form XObject svg2pdf produces
//! is normalised by its `/Matrix` to the 1x1 unit square (as an Image XObject is), so
//! `document::render_image`'s `cm` (`[w, 0, 0, h, x, y]`) applies unchanged.
//!
//! Format sniffing ([`looks_like_svg`]) works even without the `svg` feature, because
//! without it "we recognised an SVG but cannot draw it" is more helpful than
//! "unsupported format".
//!
//! # Fonts
//!
//! Which fonts are available to `<text>` inside an SVG is decided by [`SvgFontDb`]. It is
//! **built from the same `FontCollection` the document uses**, so a font passed with
//! `--font` and a font loaded through `@font-face` are both reachable from inside the SVG.
//! usvg is never asked to search for system fonts itself (so this engine's font resolution
//! does not run twice).

/// The font database used to draw `<text>` inside an SVG.
///
/// Without the `svg-text` feature it is an empty value, and text inside an SVG is not drawn
/// (not even converted to paths).
///
/// With `svg-text` on, it holds the bytes of the fonts in the document's
/// [`FontCollection`](crate::fonts::FontCollection) as-is. usvg's `fontdb` is a different
/// instance of a different version from the one the engine uses, but **the fonts inside are
/// the same**. It is held behind an `Arc`, so cloning is cheap.
#[derive(Clone, Default)]
pub struct SvgFontDb {
    #[cfg(feature = "svg-text")]
    db: std::sync::Arc<svg2pdf::usvg::fontdb::Database>,
    /// The default family name for `<text>` with no `font-family`. It uses the name of the
    /// document's first font (that is, the first one passed with `--font`).
    #[cfg(feature = "svg-text")]
    default_family: Option<String>,
}

impl SvgFontDb {
    /// A database with no fonts. Text inside an SVG is not drawn.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from the document's font collection.
    #[cfg(feature = "svg-text")]
    pub fn from_collection(fonts: &crate::fonts::FontCollection) -> Self {
        use svg2pdf::usvg::fontdb;

        let mut db = fontdb::Database::new();
        let mut default_family = None;
        for (index, font) in fonts.fonts().iter().enumerate() {
            // The font bytes are passed straight through (the file is not re-read, since some
            // paths have no file at all, such as a `data:` URI or an HTTP fetch in
            // `@font-face`). For a multi-face file such as a TTC, `load_font_source`
            // registers every face, so faces other than the one the document uses
            // (`Font::face_index`) are included too. Matching on the SVG side goes by family
            // name, so that causes no trouble.
            let ids = db.load_font_source(fontdb::Source::Binary(std::sync::Arc::new(
                font.data().to_vec(),
            )));

            // Where the name declared in CSS (`@font-face`'s `font-family`) differs from the
            // font's internal `name` table, it cannot be looked up from the SVG as-is.
            // The declared name is added as an alias.
            if let Some(declared) = fonts.declared_family(index) {
                for id in ids {
                    let Some(mut info) = db.face(id).cloned() else {
                        continue;
                    };
                    if info
                        .families
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(declared))
                    {
                        continue;
                    }
                    info.families
                        .push((declared.to_string(), fontdb::Language::English_UnitedStates));
                    db.remove_face(id);
                    db.push_face_info(info);
                }
            }

            if default_family.is_none() {
                default_family = fonts
                    .declared_family(index)
                    .map(str::to_string)
                    .or_else(|| font.family_name());
            }
        }

        // Point the generic families (`serif`/`sans-serif`/`monospace`/`cursive`/`fantasy`)
        // at what the document resolved them to. That has two effects:
        //
        // 1. `font-family: serif` inside an SVG gets the same font as `serif` on the HTML
        //    side (from `--serif-font` or system font resolution)
        // 2. It becomes **the last resort for an unknown family**. usvg's default selection
        //    function appends `Family::Serif` to the candidates, so without this it would look
        //    up fontdb's default ("Times New Roman"), which is not present locally, and the
        //    text would silently disappear
        //
        // If the document has no such generic name, the default font is used instead. The
        // name is guaranteed to be findable in `db`, having been registered as an alias above.
        if let Some(fallback) = &default_family {
            let resolve = |css_name: &str| -> String {
                if fonts.has_family(css_name) {
                    css_name.to_string()
                } else {
                    fallback.clone()
                }
            };
            db.set_serif_family(resolve("serif"));
            db.set_sans_serif_family(resolve("sans-serif"));
            db.set_monospace_family(resolve("monospace"));
            db.set_cursive_family(resolve("cursive"));
            db.set_fantasy_family(resolve("fantasy"));
        }

        Self {
            db: std::sync::Arc::new(db),
            default_family,
        }
    }

    /// With `svg-text` disabled it returns empty without consulting the collection
    /// (text inside an SVG is not drawn).
    #[cfg(not(feature = "svg-text"))]
    pub fn from_collection(_fonts: &crate::fonts::FontCollection) -> Self {
        Self::default()
    }

    /// The number of registered faces (for tests and diagnostics). Always 0 with `svg-text` disabled.
    pub fn len(&self) -> usize {
        #[cfg(feature = "svg-text")]
        {
            self.db.len()
        }
        #[cfg(not(feature = "svg-text"))]
        {
            0
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for SvgFontDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SvgFontDb")
            .field("faces", &self.len())
            .finish()
    }
}

/// Whether the bytes look like SVG (or gzip-compressed svgz).
///
/// Unlike PNG/JPEG/WebP there are no magic bytes, so the decision rests on two things: that
/// it begins as XML, and that `<svg` appears near the start. It assumes the raster magic-byte
/// checks have already been tried first.
pub fn looks_like_svg(bytes: &[u8]) -> bool {
    // svgz (gzip). Deciding what is inside is left to usvg's decoding.
    if bytes.starts_with(&[0x1F, 0x8B]) {
        return true;
    }

    // Skip a UTF-8 BOM and any leading whitespace.
    let rest = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let rest = match rest.iter().position(|b| !b.is_ascii_whitespace()) {
        Some(i) => &rest[i..],
        None => return false,
    };
    if !rest.starts_with(b"<") {
        return false;
    }

    // A `<?xml ...?>`, comments and a DOCTYPE can come first, so the root element is not
    // necessarily at the very start. Only a fixed amount of the beginning is searched for
    // `<svg` (the whole document is not scanned, so other XML is not mistaken for SVG).
    const SNIFF_WINDOW: usize = 4096;
    let window = &rest[..rest.len().min(SNIFF_WINDOW)];
    window.windows(4).any(|w| w.eq_ignore_ascii_case(b"<svg"))
}

/// Whether the SVG bytes look like they contain a `<text>` element.
///
/// Without the `svg-text` feature, text inside an SVG is **not drawn at all** (not even
/// converted to paths: svg2pdf discards the text nodes). usvg and svg2pdf emit warnings to
/// the `log` crate, but this crate configures no logger, so they never reach the user.
/// Text disappearing silently is hard to diagnose, so this lets the caller warn by
/// inspecting the bytes before conversion.
///
/// It looks for `<text` and `<tspan` so it picks up elements rather than attributes
/// (an attribute such as `textLength` carries no `<`, so it is not a false positive).
pub fn looks_like_it_has_text(bytes: &[u8]) -> bool {
    fn contains_tag(haystack: &[u8], tag: &[u8]) -> bool {
        haystack
            .windows(tag.len())
            .any(|w| w.eq_ignore_ascii_case(tag))
    }
    contains_tag(bytes, b"<text") || contains_tag(bytes, b"<tspan")
}

/// Count the inline `<svg>` elements written directly in the HTML.
///
/// Inline SVG is not drawn (the UA stylesheet's `svg { display: none }` removes the whole
/// subtree). `<img src="*.svg">` and `background-image` can now be drawn, so this exists to
/// warn once per document, sparing anyone who read "SVG supported", wrote one inline and saw
/// nothing appear.
///
/// Supporting it would mean rebuilding the SVG XML from the HTML DOM and handing that to
/// usvg (attribute name casing, `viewBox` and the like, CSS inheritance, `currentColor`),
/// which is a different job from referencing an external file, so this only counts them.
/// Only the subtree under `root` is inspected (streaming calls it per top-level element;
/// scanning the whole document each time would be quadratic in the element count).
pub fn count_inline_svg_elements(dom: &crate::html::Dom, root: crate::html::NodeId) -> usize {
    fn walk(dom: &crate::html::Dom, node: crate::html::NodeId, count: &mut usize) {
        if let crate::html::NodeData::Element { name, .. } = &dom.node(node).data {
            // Namespaces are not considered (matching the UA stylesheet's decision). A nested
            // `<svg>` should not be counted, so once one is found its interior is not walked.
            if &*name.local == "svg" {
                *count += 1;
                return;
            }
        }
        for child in dom.children(node) {
            walk(dom, child, count);
        }
    }
    let mut count = 0;
    walk(dom, root, &mut count);
    count
}

/// Warn once per document when [`count_inline_svg_elements`] found at least one.
///
/// `warned` is per-document state. Several documents are converted in one process (the gem
/// and server mode), so making it once per process would silence every document after the first.
pub fn warn_about_inline_svg(dom: &crate::html::Dom, root: crate::html::NodeId, warned: &mut bool) {
    if *warned {
        return;
    }
    let count = count_inline_svg_elements(dom, root);
    if count == 0 {
        return;
    }
    *warned = true;
    eprintln!(
        "warning: the HTML contains {count} inline <svg> element(s), which are not drawn.\n  \
         SVG can only be drawn when referenced from <img src=\"...svg\"> or\n  \
         background-image: url(...svg) (inline SVG is not supported)"
    );
}

#[cfg(feature = "svg")]
mod convert {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use pdf_writer::{Chunk, Ref};

    use super::SvgFontDb;
    use crate::pdf::document::RefAllocator;

    /// A converted SVG, corresponding to one `<img src="*.svg">` (or `background-image`).
    ///
    /// The `Ref`s at this point are chunk-local numbers svg2pdf allocated from 1 and are
    /// unrelated to the document's `Ref` space. They are renumbered by
    /// [`renumber_into_document`] at embedding time.
    #[derive(Debug, Clone)]
    pub struct VectorGraphic {
        /// Renumbering happens once per image, so [`renumber_into_document`] takes it from
        /// here and releases it. The decode result is held by a `src`-keyed cache until the
        /// end of the document, so keeping the pre-renumbering chunk would mean holding the
        /// same content twice.
        chunk: RefCell<Option<Chunk>>,
        /// The Ref of the Form XObject within `chunk` (what the content stream does `Do` on).
        root: Ref,
    }

    /// An SVG renumbered into the document's `Ref` space.
    #[derive(Debug, Clone)]
    pub struct RenumberedVectorGraphic {
        pub chunk: Chunk,
        pub root: Ref,
        /// The starting offset of each object within `chunk`. The streaming writer uses it to
        /// build the xref (the batch writer does not need it, using the offsets `Chunk::extend`
        /// keeps itself).
        pub offsets: Vec<(Ref, usize)>,
    }

    /// Why the SVG conversion failed.
    #[derive(Debug)]
    pub struct SvgError(String);

    impl std::fmt::Display for SvgError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "failed to convert the SVG: {}", self.0)
        }
    }

    impl std::error::Error for SvgError {}

    /// Convert SVG bytes into a PDF Form XObject. The returned `width`/`height` are the SVG's
    /// intrinsic size (px), meaning the same as on a raster `PreparedImage`.
    ///
    /// `fonts` are the fonts used for `<text>` inside the SVG ([`SvgFontDb`]). Passing the
    /// same fonts the document uses makes the HTML-side fonts available inside the SVG too.
    pub fn convert_svg(
        bytes: &[u8],
        fonts: &SvgFontDb,
    ) -> Result<(f32, f32, VectorGraphic), SvgError> {
        let options = svg_options(fonts);
        let tree =
            svg2pdf::usvg::Tree::from_data(bytes, &options).map_err(|e| SvgError(e.to_string()))?;

        let size = tree.size();
        if !size.width().is_finite() || !size.height().is_finite() {
            return Err(SvgError(format!(
                "the intrinsic size is not numeric ({}x{})",
                size.width(),
                size.height()
            )));
        }

        let conversion = svg2pdf::ConversionOptions {
            // An SVG's content stream is a run of path data (text), so it compresses well.
            // The document-wide `--no-compression` exists for reading the PDF structure by
            // eye, not for making an embedded SVG's interior readable, so this always
            // compresses.
            compress: true,
            ..Default::default()
        };
        let (chunk, root) =
            svg2pdf::to_chunk(&tree, conversion).map_err(|e| SvgError(e.to_string()))?;

        // The intrinsic size is returned unrounded. Rounding a `width="40.6"` or a fractional
        // `viewBox` to an integer changes the aspect ratio and visibly shifts
        // `object-fit: contain`/`cover` and the height derived from a `width`-only setting
        // (40.6x10.4 to 41x10 changes the ratio by 5%).
        Ok((
            size.width(),
            size.height(),
            VectorGraphic {
                chunk: RefCell::new(Some(chunk)),
                root,
            },
        ))
    }

    /// The usvg parse options.
    ///
    /// # Blocking file reads from inside an SVG
    ///
    /// usvg's default `ImageHrefResolver::resolve_string` **calls `std::fs::read` directly**
    /// on an `<image href="...">` inside an SVG (`usvg::parser::image`, the one place usvg
    /// touches a file as a library). That bypasses the whole containment `img::fetch`
    /// provides (refusing references outside the base directory, `--allow`,
    /// `--disable-local-file-access`).
    ///
    /// svg2pdf's `image` feature is currently off, so what is read never reaches the PDF
    /// (an `<image>` node is discarded undrawn). We block it anyway because:
    ///
    /// * the read itself happens (probing for a file's existence, an unbounded `fs::read`)
    /// * the moment the `image` feature is added it becomes "any file can be poured into the
    ///   PDF". A nested SVG is drawn as vectors, so its text and shapes would come out as-is
    ///
    /// Replacing `resolve_string` with a function that resolves nothing removes the route to
    /// external resources from inside an SVG. `resolve_data`, which handles `data:` URIs, is
    /// left at its default: it is self-contained within bytes already fetched and crosses no
    /// new trust boundary.
    ///
    /// # Fonts
    ///
    /// `resources_dir` stays at its default `None` (no base for resolving relative paths).
    /// Only the fonts received in `fonts` are used, and usvg is never asked to
    /// `load_system_fonts()`. This engine has its own system font discovery
    /// (`fonts::system`), so running it twice would be pointless and would break the
    /// guarantee that "a font usable in HTML is usable in SVG".
    fn svg_options(fonts: &SvgFontDb) -> svg2pdf::usvg::Options<'static> {
        #[allow(unused_mut)]
        let mut options = svg2pdf::usvg::Options {
            image_href_resolver: svg2pdf::usvg::ImageHrefResolver {
                resolve_string: Box::new(|href, _| {
                    eprintln!(
                        "warning: external references inside an SVG are not loaded (files\n  \
                         cannot be opened from inside an SVG): {href}"
                    );
                    None
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        #[cfg(feature = "svg-text")]
        {
            options.fontdb = fonts.db.clone();
            // `<text>` with no `font-family` is drawn in the document's default font rather
            // than usvg's default ("Times New Roman"). Making a locally absent font name the
            // default would guarantee that unstyled text could never be drawn.
            if let Some(family) = &fonts.default_family {
                options.font_family = family.clone();
            }
        }
        #[cfg(not(feature = "svg-text"))]
        let _ = fonts;
        options
    }

    /// Renumber svg2pdf's chunk-local `Ref`s into document `Ref`s allocated from `alloc`.
    ///
    /// It consumes as many `Ref`s as there are objects in the chunk. The streaming writer's
    /// xref assumes "every object from 1 upwards is written", so it confirms that **the Refs
    /// allocated and the objects actually written correspond one to one** and errors out if
    /// not (dropping one SVG beats writing a broken PDF).
    ///
    /// On failure `alloc` is not advanced. Leaving objects whose numbers were consumed but
    /// which are never written would break the whole document while meaning only to drop an SVG.
    pub fn renumber_into_document(
        graphic: &VectorGraphic,
        alloc: &mut RefAllocator,
    ) -> Result<RenumberedVectorGraphic, SvgError> {
        // Once renumbered the original chunk is no longer needed, so it is taken and released.
        // A second call stops here (it never actually happens, `ids_for_image` calling this
        // once per image).
        let Some(source) = graphic.chunk.borrow_mut().take() else {
            return Err(SvgError(
                "tried to embed an already-renumbered SVG a second time".to_string(),
            ));
        };
        let mut next = alloc.peek().get();
        let mut mapping: HashMap<Ref, Ref> = HashMap::new();
        let chunk = source.renumber(|old| {
            *mapping.entry(old).or_insert_with(|| {
                let assigned = Ref::new(next);
                next += 1;
                assigned
            })
        });

        drop(source);
        let root = *mapping
            .get(&graphic.root)
            .ok_or_else(|| SvgError("the Form XObject's Ref was not renumbered".to_string()))?;

        let refs: Vec<Ref> = chunk.refs().collect();
        if refs.len() != mapping.len() {
            // There is an object referenced within the chunk but not defined
            // (`Chunk::renumber` calls the mapping for such references too). That would leave
            // objects whose numbers were consumed but which are never written, so it is refused.
            return Err(SvgError(format!(
                "the chunk contains undefined references ({} definitions against {} references)",
                refs.len(),
                mapping.len()
            )));
        }

        let offsets = object_offsets(&chunk, &refs)?;
        alloc.commit(mapping.len());
        Ok(RenumberedVectorGraphic {
            chunk,
            root,
            offsets,
        })
    }

    /// Find the starting position of each object within `chunk`'s bytes.
    ///
    /// A `Chunk` keeps the offsets internally but does not expose them, so we scan on the
    /// assumption of the layout `Chunk::renumber` writes (`{id} {gen} obj\n...\nendobj\n\n`
    /// running back to back in `refs()` order). An object's header is searched for anchored to
    /// the preceding `endobj`, so a stream's contents that happen to look like a header are
    /// not picked up. If even one is not found it returns an error
    /// (guessing at the xref would break the whole PDF).
    fn object_offsets(chunk: &Chunk, refs: &[Ref]) -> Result<Vec<(Ref, usize)>, SvgError> {
        const ENDOBJ: &[u8] = b"\nendobj\n\n";
        let bytes = chunk.as_bytes();
        let mut offsets = Vec::with_capacity(refs.len());
        let mut pos = 0usize;

        for (i, &id) in refs.iter().enumerate() {
            let header = format!("{} 0 obj\n", id.get()).into_bytes();
            let start = if i == 0 {
                // The first object is at the start of the chunk.
                bytes.starts_with(&header).then_some(0)
            } else {
                // Every later one follows the previous object's `endobj`.
                let anchored = [ENDOBJ, &header].concat();
                find(&bytes[pos..], &anchored).map(|at| pos + at + ENDOBJ.len())
            };
            let start = start.ok_or_else(|| {
                SvgError(format!(
                    "cannot find the start of object {} (number {})",
                    id.get(),
                    i + 1
                ))
            })?;
            offsets.push((id, start));
            pos = start + header.len();
        }

        Ok(offsets)
    }

    /// The first position at which `needle` appears.
    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || haystack.len() < needle.len() {
            return None;
        }
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const CIRCLE: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10">
            <circle cx="10" cy="5" r="4" fill="#f00"/>
        </svg>"##;

        #[test]
        fn converts_an_svg_and_reports_its_intrinsic_size() {
            let (width, height, graphic) =
                convert_svg(CIRCLE.as_bytes(), &SvgFontDb::empty()).expect("should convert");
            assert_eq!((width, height), (20.0, 10.0));
            assert!(
                graphic.chunk.borrow().as_ref().unwrap().refs().len() >= 1,
                "the chunk should hold at least the form XObject"
            );
        }

        #[test]
        fn renumbering_hands_off_the_original_chunk() {
            // The pre-renumbering chunk is held by the cache until the end of the document, so
            // it is released once renumbered (never held twice).
            let (.., graphic) = convert_svg(CIRCLE.as_bytes(), &SvgFontDb::empty()).unwrap();
            let mut alloc = RefAllocator::default();
            renumber_into_document(&graphic, &mut alloc).expect("should renumber");
            assert!(graphic.chunk.borrow().is_none());
            // A second call errors out without consuming any numbers.
            let before = alloc.peek();
            assert!(renumber_into_document(&graphic, &mut alloc).is_err());
            assert_eq!(alloc.peek(), before);
        }

        #[test]
        fn rejects_bytes_that_are_not_svg() {
            assert!(convert_svg(b"not an svg at all", &SvgFontDb::empty()).is_err());
        }

        #[test]
        fn renumbering_maps_every_object_into_the_documents_ref_space() {
            let (.., graphic) = convert_svg(CIRCLE.as_bytes(), &SvgFontDb::empty()).unwrap();
            let mut alloc = RefAllocator::default();
            // Allocate something before the SVG and confirm it does not restart from 1.
            let first = alloc.next();
            let renumbered = renumber_into_document(&graphic, &mut alloc).expect("should renumber");

            let mut got: Vec<i32> = renumbered.chunk.refs().map(|r| r.get()).collect();
            assert!(!got.is_empty());
            assert!(
                renumbered.chunk.refs().any(|r| r == renumbered.root),
                "the form XObject must be one of the chunk's objects"
            );
            // The allocated `Ref`s and the chunk's objects correspond one to one (no holes in the xref).
            let expected: Vec<i32> = (first.get() + 1..=first.get() + got.len() as i32).collect();
            got.sort_unstable();
            assert_eq!(got, expected);
        }

        #[test]
        fn object_offsets_point_at_every_object_header() {
            let (.., graphic) = convert_svg(CIRCLE.as_bytes(), &SvgFontDb::empty()).unwrap();
            let mut alloc = RefAllocator::default();
            let renumbered = renumber_into_document(&graphic, &mut alloc).unwrap();

            let bytes = renumbered.chunk.as_bytes();
            assert_eq!(renumbered.offsets.len(), renumbered.chunk.refs().len());
            for &(id, offset) in &renumbered.offsets {
                let header = format!("{} 0 obj\n", id.get());
                assert!(
                    bytes[offset..].starts_with(header.as_bytes()),
                    "offset {offset} for object {} should point at its header",
                    id.get()
                );
            }
        }
    }
}

#[cfg(feature = "svg")]
pub use convert::{convert_svg, renumber_into_document, RenumberedVectorGraphic, VectorGraphic};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_a_plain_svg() {
        let src = r#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"></svg>"#;
        assert!(looks_like_svg(src.as_bytes()));
    }

    #[test]
    fn sniffs_an_svg_behind_a_prolog_a_comment_and_a_doctype() {
        let src = concat!(
            "\n  <?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
            "<!-- exported by some drawing tool -->\n",
            "<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\" \"x.dtd\">\n",
            "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"
        );
        assert!(looks_like_svg(src.as_bytes()));
    }

    #[test]
    fn sniffs_an_svg_with_a_utf8_bom_and_an_uppercase_tag() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"<SVG xmlns=\"http://www.w3.org/2000/svg\"></SVG>");
        assert!(looks_like_svg(&bytes));
    }

    #[test]
    fn does_not_sniff_other_xml_or_binary_as_svg() {
        assert!(!looks_like_svg(
            b"<?xml version=\"1.0\"?><rss><channel/></rss>"
        ));
        assert!(!looks_like_svg(b"\x89PNG\r\n\x1a\n"));
        assert!(!looks_like_svg(b""));
        assert!(!looks_like_svg(b"   \n\t  "));
        // XML with `<svg` only far into it is not picked up.
        let mut far = String::from("<other>");
        far.push_str(&"x".repeat(5000));
        far.push_str("<svg/></other>");
        assert!(!looks_like_svg(far.as_bytes()));
    }
}
