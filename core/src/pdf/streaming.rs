//! A streaming writer that emits the PDF bytes to a [`Sink`] as each page is settled.
//!
//! Each page's content stream is built and written to the `Sink` the moment that page's
//! [`Page`] is settled (CIDs are always the original glyph IDs; `render_box`/`render_line`
//! are passed `remaps: None`). Font embedding (subsetting and building the `/CIDToGIDMap`
//! stream) is done all at once when [`StreamingPdfWriter::finish`] is called,
//! after every page has been processed.
//!
//! `pdf_writer::Pdf` keeps the xref and trailer construction in a private implementation, so
//! we write to the `Sink` a `Chunk` (a self-contained byte string per object) at a time,
//! record `(Ref, the offset it was written at)` ourselves, and assemble the xref and trailer
//! in [`StreamingPdfWriter::finish`].

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use pdf_writer::writers::Catalog;
use pdf_writer::{Chunk, Content, Filter, Finish, Name, Rect as PdfRect, Ref};

use crate::fonts::FontCollection;
use crate::html::NodeId;
use crate::layout::{Page, PageSettings};
use crate::sink::Sink;
use crate::style::{ComputedStyle, PageRule};

use super::document::{
    alpha_gs_resource_name, collect_anchor_positions, collect_image_uses, collect_link_areas,
    collect_margin_box_usage, collect_opacity_uses, collect_usage, file_identifier, render_box,
    render_header_footer_rules, render_margin_boxes, render_page_overlay, write_document_info,
    write_link_annotation, write_resources, LinkSettings, PageOverlay, RefAllocator, RenderTarget,
    ALPHA_STEPS,
};
use super::font::{deflate, embed_font_streaming_chunks, FontIds, FontUsage};
use super::img::{embed_image_streaming_chunks, ids_for_image, ImageIds, PreparedImage};
use super::options::PdfOutputOptions;

const PDF_HEADER: &[u8] = b"%PDF-1.7\n%\x80\x80\x80\x80\n\n";

/// A writer emitting the PDF bytes to a `Sink` as each page is settled.
///
/// `new` writes the file header immediately, `write_page` is called as each page is settled,
/// and finally `finish` writes the font embedding, xref and trailer and closes the `sink`.
pub struct StreamingPdfWriter<S: Sink> {
    sink: S,
    output_len: usize,
    offsets: Vec<(Ref, usize)>,
    alloc: RefAllocator,
    catalog_id: Ref,
    pages_tree_id: Ref,
    font_ids: Vec<FontIds>,
    font_resource_names: Vec<String>,
    usages: Vec<FontUsage>,
    page_ids: Vec<Ref>,
    settings: PageSettings,
    /// The document-wide map of image Refs, keyed on `Rc::as_ptr` (the identity of the decode
    /// result). Unlike fonts, images need no cross-page usage tally (subsetting), so this is
    /// filled in per page on a "write it if this is its first appearance" basis, rather than
    /// waiting for `finish`.
    image_ids: HashMap<usize, ImageIds>,
    /// The keys of SVGs whose Ref renumbering failed in `ids_for_image`. A cache so the same
    /// SVG used many times warns only once (a raster image fails at the decode stage and
    /// never reaches here).
    failed_svg_ids: HashSet<usize>,
    /// The `@page` rules (for drawing margin boxes).
    page_rules: Vec<PageRule>,
    /// The ExtGStates for semi-transparent drawing of `background-color`/`box-shadow`
    /// (21 steps of 0.05). Allocated once for the whole document, as in batch mode (`encode_pdf`).
    alpha_gs_ids: Vec<Ref>,
    alpha_gs_names: Vec<String>,
    /// The settings for generating link annotations.
    links: LinkSettings,
    /// The positions of the anchors found on the pages written so far
    /// (name, page Ref, x, y). Written out as the `/Dests` dictionary in `finish`.
    destinations: Vec<(String, Ref, f32, f32)>,
    /// Metadata, compression, scale and grayscale.
    output: PdfOutputOptions,
    /// The subdocument to composite onto the next page written
    /// (`--header-html`/`--footer-html`). Consumed by `write_page`.
    pending_overlays: Vec<PageOverlay>,
    /// The page number of the next page written.
    ///
    /// * `Some(Some(n))`: treat it as page number `n`
    /// * `Some(None)`: a page with no number (a cover). Neither margin boxes nor
    ///   headers/footers are drawn
    /// * `None`: not specified (the number of pages written so far, plus 1)
    pending_page_number: Option<Option<usize>>,
}

impl<S: Sink> StreamingPdfWriter<S> {
    /// Create a new writer, writing the PDF file header to `sink` immediately.
    /// `links` is the internal anchor table plus `<base href>` ([`LinkSettings`]).
    /// With the default value, only external link annotations are generated.
    pub fn new(
        fonts: &FontCollection,
        settings: PageSettings,
        sink: S,
        page_rules: Vec<PageRule>,
        links: LinkSettings,
    ) -> Result<Self, S::Error> {
        Self::with_options(
            fonts,
            settings,
            sink,
            page_rules,
            links,
            PdfOutputOptions::default(),
        )
    }

    /// The version taking an explicit [`PdfOutputOptions`].
    pub fn with_options(
        fonts: &FontCollection,
        settings: PageSettings,
        mut sink: S,
        page_rules: Vec<PageRule>,
        links: LinkSettings,
        output: PdfOutputOptions,
    ) -> Result<Self, S::Error> {
        sink.write(PDF_HEADER)?;

        let mut alloc = RefAllocator::default();
        let catalog_id = alloc.next();
        let pages_tree_id = alloc.next();
        let font_ids: Vec<FontIds> = (0..fonts.len())
            .map(|_| FontIds {
                font_file: alloc.next(),
                descriptor: alloc.next(),
                cid_font: alloc.next(),
                type0_font: alloc.next(),
                to_unicode: alloc.next(),
                cid_to_gid_map: alloc.next(),
            })
            .collect();
        let font_resource_names = (0..fonts.len()).map(|i| format!("F{i}")).collect();
        let usages = (0..fonts.len()).map(|_| FontUsage::default()).collect();
        let alpha_gs_ids: Vec<Ref> = (0..=ALPHA_STEPS).map(|_| alloc.next()).collect();
        let alpha_gs_names: Vec<String> = (0..=ALPHA_STEPS).map(alpha_gs_resource_name).collect();

        let mut writer = Self {
            sink,
            output_len: PDF_HEADER.len(),
            offsets: Vec::new(),
            alloc,
            catalog_id,
            pages_tree_id,
            font_ids,
            font_resource_names,
            usages,
            page_ids: Vec::new(),
            settings,
            image_ids: HashMap::new(),
            failed_svg_ids: HashSet::new(),
            page_rules,
            alpha_gs_ids: alpha_gs_ids.clone(),
            alpha_gs_names,
            links,
            destinations: Vec::new(),
            output,
            pending_overlays: Vec::new(),
            pending_page_number: None,
        };
        for (step, id) in alpha_gs_ids.into_iter().enumerate() {
            let a = step as f32 / ALPHA_STEPS as f32;
            let mut chunk = Chunk::new();
            chunk
                .ext_graphics(id)
                .non_stroking_alpha(a)
                .stroking_alpha(a);
            writer.write_chunk(id, &chunk)?;
        }
        Ok(writer)
    }

    /// Set the subdocument to composite onto the page `write_page` writes next.
    ///
    /// The entry point for compositing the header/footer HTML without changing `write_page`'s
    /// signature. The content can vary per page (`[page]`), so the caller sets it per page.
    /// The number of pages written so far (the next page number being `+1`).
    pub fn page_count(&self) -> usize {
        self.page_ids.len()
    }

    pub fn set_page_overlays(&mut self, overlays: Vec<PageOverlay>) {
        self.pending_overlays = overlays;
    }

    /// Set the page number of the next page written explicitly.
    ///
    /// `Some(n)` treats it as that number; `None` makes it a page with no number (a cover),
    /// drawing neither margin boxes nor headers/footers.
    pub fn set_next_page_number(&mut self, number: Option<usize>) {
        self.pending_page_number = Some(number);
    }

    /// Encode one settled page into a content stream immediately and write it to `sink`. The
    /// glyphs used are only accumulated internally as a lightweight [`FontUsage`], so `page`
    /// (the layout result) may be discarded after the call.
    ///
    /// `total_pages` is the total page count for `counter(pages)` (always `None` under
    /// `Mode::Streaming`, where it cannot be known in principle; a pre-counted value is
    /// passed only under `Mode::Batch` when `@page` uses `counter(pages)`).
    pub fn write_page(
        &mut self,
        page: &Page,
        styles: &HashMap<NodeId, Rc<ComputedStyle>>,
        background_images: &HashMap<NodeId, Rc<PreparedImage>>,
        fonts: &FontCollection,
        total_pages: Option<usize>,
    ) -> Result<(), S::Error> {
        // By default the page number is "the number of pages written so far, plus 1"
        // (1-based), but it can be given explicitly for a cover or a table of contents.
        // `None` means "a page with no number", drawing no margin boxes and no header/footer.
        let explicit = self.pending_page_number.take();
        let numbered = explicit.map(|n| n.is_some()).unwrap_or(true);
        let page_number = explicit
            .flatten()
            .unwrap_or_else(|| self.page_ids.len() + 1);

        for b in &page.boxes {
            collect_usage(b, fonts, &mut self.usages);
        }
        let overlays = if numbered {
            std::mem::take(&mut self.pending_overlays)
        } else {
            self.pending_overlays.clear();
            Vec::new()
        };
        for overlay in &overlays {
            for b in &overlay.boxes {
                collect_usage(b, fonts, &mut self.usages);
            }
        }
        if numbered {
            collect_margin_box_usage(
                &self.settings,
                fonts,
                &self.page_rules,
                page_number,
                total_pages,
                &mut self.usages,
            );
        }

        // Unlike fonts, images need no cross-page usage tally (subsetting), so anything
        // appearing for the first time on this page is written out as an XObject right here.
        // Both `<img>` itself and `background-image` are collected together at this point.

        let mut used_images = Vec::new();
        for b in &page.boxes {
            collect_image_uses(b, background_images, &mut used_images);
        }
        let mut page_image_refs = Vec::with_capacity(used_images.len());
        for image in &used_images {
            // An SVG whose `Ref` renumbering failed becomes `None` (and is not drawn).
            let Some((ids, is_new)) = ids_for_image(
                &mut self.alloc,
                &mut self.image_ids,
                &mut self.failed_svg_ids,
                image,
            ) else {
                continue;
            };
            let root = ids.root;
            // Writing borrows `self` mutably, so the `ids` borrowed from `self.image_ids` are
            // released here before moving on to `write_objects`.
            let embedded = if is_new {
                embed_image_streaming_chunks(image, ids, self.output.grayscale)
            } else {
                Vec::new()
            };
            for embed in &embedded {
                self.write_objects(&embed.chunk, &embed.offsets)?;
            }
            page_image_refs.push(root);
        }

        // Collect the elements with `opacity < 1` first and allocate their Refs (the same
        // structure as batch mode's `encode_pdf`).
        let mut opacity_nodes = Vec::new();
        for b in &page.boxes {
            collect_opacity_uses(b, styles, &mut opacity_nodes);
        }
        let opacity_form_ids: HashMap<NodeId, Ref> = opacity_nodes
            .iter()
            .map(|&n| (n, self.alloc.next()))
            .collect();
        let mut pending_forms: Vec<(Ref, Vec<u8>)> = Vec::new();

        let page_id = self.alloc.next();
        let content_id = self.alloc.next();
        self.page_ids.push(page_id);

        let mut content = Content::new();
        // The CSS px to PDF pt conversion is done by the page's overall CTM. Every coordinate
        // in the content stream from here on can stay in CSS px.
        let scale = self.output.scale;
        content.transform([scale, 0.0, 0.0, scale, 0.0, 0.0]);
        // A wrapper interposing the colour conversion.
        let mut target = RenderTarget::new(&mut content, self.output.grayscale);
        for b in &page.boxes {
            // `remaps: None` - CIDs are always used as the original glyph IDs.
            render_box(
                &mut target,
                b,
                styles,
                fonts,
                &self.settings,
                None,
                &self.font_resource_names,
                &self.image_ids,
                background_images,
                &self.alpha_gs_names,
                &opacity_form_ids,
                &mut pending_forms,
            );
        }
        for overlay in &overlays {
            render_page_overlay(
                &mut target,
                overlay,
                fonts,
                &self.font_resource_names,
                &self.alpha_gs_names,
            );
        }
        if numbered {
            render_header_footer_rules(
                &mut target,
                &self.settings,
                self.output.header_line,
                self.output.footer_line,
            );
            render_margin_boxes(
                &mut target,
                &self.settings,
                fonts,
                &self.page_rules,
                page_number,
                total_pages,
                None,
                &self.font_resource_names,
            );
        }
        let content_bytes = content.finish();
        let stream_bytes = if self.output.compress {
            deflate(&content_bytes)
        } else {
            content_bytes.to_vec()
        };

        let mut chunk = Chunk::new();
        let mut content_stream = chunk.stream(content_id, &stream_bytes);
        if self.output.compress {
            content_stream.filter(Filter::FlateDecode);
        }
        content_stream.finish();
        self.write_chunk(content_id, &chunk)?;

        // The `<a href>` annotations, and the positions of the anchors landing on this page.
        // An annotation only references a named destination, so even a link pointing at a
        // later page can be written out fully at this point.
        let mut page_links = Vec::new();
        let mut page_anchors = Vec::new();
        for b in &page.boxes {
            collect_link_areas(b, &self.settings, &mut page_links);
            collect_anchor_positions(
                b,
                &self.links.anchor_names,
                &self.settings,
                &mut page_anchors,
            );
        }
        self.links.retain_enabled(&mut page_links);
        for (name, x, y) in page_anchors {
            if !self
                .destinations
                .iter()
                .any(|(existing, ..)| *existing == name)
            {
                self.destinations
                    .push((name, page_id, self.output.to_pt(x), self.output.to_pt(y)));
            }
        }
        let mut annotation_ids = Vec::with_capacity(page_links.len());
        for area in &page_links {
            let id = self.alloc.next();
            annotation_ids.push(id);
            let mut chunk = Chunk::new();
            write_link_annotation(
                chunk.annotation(id),
                area,
                self.links.annotation_base_href(),
                self.output.scale,
            );
            self.write_chunk(id, &chunk)?;
        }

        let form_refs: Vec<Ref> = pending_forms.iter().map(|(id, _)| *id).collect();
        let mut chunk = Chunk::new();
        {
            let mut p = chunk.page(page_id);
            p.parent(self.pages_tree_id);
            p.media_box(PdfRect::new(
                0.0,
                0.0,
                self.output.to_pt(self.settings.size.width),
                self.output.to_pt(self.settings.size.height),
            ));
            p.contents(content_id);
            if !annotation_ids.is_empty() {
                p.annotations(annotation_ids.iter().copied());
            }
            write_resources(
                p.resources(),
                &self.font_resource_names,
                &self.font_ids,
                &page_image_refs,
                &form_refs,
                &self.alpha_gs_names,
                &self.alpha_gs_ids,
            );
        }
        self.write_chunk(page_id, &chunk)?;

        // Write out the Form XObjects of the opacity groups for real
        // (the same policy as batch mode).
        for (form_ref, bytes) in &pending_forms {
            let mut chunk = Chunk::new();
            {
                let mut form = chunk.form_xobject(*form_ref, bytes);
                form.bbox(PdfRect::new(
                    0.0,
                    0.0,
                    self.settings.size.width,
                    self.settings.size.height,
                ));
                form.group().transparency().isolated(true).knockout(false);
                write_resources(
                    form.resources(),
                    &self.font_resource_names,
                    &self.font_ids,
                    &page_image_refs,
                    &form_refs,
                    &self.alpha_gs_names,
                    &self.alpha_gs_ids,
                );
            }
            self.write_chunk(*form_ref, &chunk)?;
        }

        Ok(())
    }

    /// Write out every remaining object (font embedding, the page tree, the catalog, the xref
    /// and the trailer) and call `sink.finish()`.
    pub fn finish(mut self, fonts: &FontCollection) -> Result<S::Output, S::Error> {
        let font_ids = self.font_ids.clone();
        let usages = std::mem::take(&mut self.usages);
        for ((font, &ids), usage) in fonts.fonts().iter().zip(font_ids.iter()).zip(usages.iter()) {
            for (id, chunk) in embed_font_streaming_chunks(font, ids, usage, self.output.compress) {
                self.write_chunk(id, &chunk)?;
            }
        }

        let mut chunk = Chunk::new();
        chunk
            .pages(self.pages_tree_id)
            .kids(self.page_ids.iter().copied())
            .count(self.page_ids.len() as i32);
        self.write_chunk(self.pages_tree_id, &chunk)?;

        // The named destinations are resolved here, once every page has been written.
        // A forward-referencing link only gets its destination at this point.
        let destinations = std::mem::take(&mut self.destinations);
        let dests_id = (!destinations.is_empty()).then(|| self.alloc.next());
        if let Some(dests_id) = dests_id {
            let mut chunk = Chunk::new();
            {
                let mut dests = chunk.destinations(dests_id);
                for (name, page_id, x, y) in &destinations {
                    dests
                        .insert(Name(name.as_bytes()))
                        .page(*page_id)
                        .xyz(*x, *y, None);
                }
            }
            self.write_chunk(dests_id, &chunk)?;
        }

        let mut chunk = Chunk::new();
        {
            let mut catalog = chunk.indirect(self.catalog_id).start::<Catalog>();
            catalog.pages(self.pages_tree_id);
            if let Some(dests_id) = dests_id {
                catalog.destinations(dests_id);
            }
        }
        self.write_chunk(self.catalog_id, &chunk)?;

        let info_id = self.alloc.next();
        let mut chunk = Chunk::new();
        write_document_info(
            chunk
                .indirect(info_id)
                .start::<pdf_writer::writers::DocumentInfo>(),
            &self.output.metadata,
        );
        self.write_chunk(info_id, &chunk)?;

        self.write_xref_and_trailer(info_id)?;

        self.sink.finish()
    }

    /// Write `chunk`'s bytes (assumed to hold a single indirect object) to `sink` and record
    /// its starting offset for the xref.
    fn write_chunk(&mut self, id: Ref, chunk: &Chunk) -> Result<(), S::Error> {
        self.write_objects(chunk, &[(id, 0)])
    }

    /// Write a chunk containing several objects. `offsets` are the starting positions of each
    /// object within the chunk (used where one chunk holds several objects, as with an SVG's
    /// Form XObjects).
    fn write_objects(&mut self, chunk: &Chunk, offsets: &[(Ref, usize)]) -> Result<(), S::Error> {
        for &(id, offset) in offsets {
            self.offsets.push((id, self.output_len + offset));
        }
        let bytes = chunk.as_bytes();
        self.output_len += bytes.len();
        self.sink.write(bytes)
    }

    fn write_xref_and_trailer(&mut self, info_id: Ref) -> Result<(), S::Error> {
        let xref_offset = self.output_len;
        let size = self
            .offsets
            .iter()
            .map(|(id, _)| id.get())
            .max()
            .unwrap_or(0)
            + 1;

        self.offsets.sort_by_key(|(id, _)| id.get());

        let mut buf = Vec::new();
        buf.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
        buf.extend_from_slice(b"0000000000 65535 f \n");
        for (_, offset) in &self.offsets {
            buf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        // `/ID` (the file identifier) is built the same way as in the batch writer.
        // We write the trailer ourselves here rather than through `pdf_writer::Pdf`, so it is
        // written directly as a hex string (the same value as a byte string).
        let id: String = file_identifier(&self.output.metadata, self.page_ids.len())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        buf.extend_from_slice(
            format!(
                "trailer\n<< /Size {size} /Root {} 0 R /Info {} 0 R /ID [<{id}> <{id}>] >>\n",
                self.catalog_id.get(),
                info_id.get()
            )
            .as_bytes(),
        );
        buf.extend_from_slice(format!("startxref\n{xref_offset}\n%%EOF").as_bytes());

        self.output_len += buf.len();
        self.sink.write(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Font;
    use crate::html;
    use crate::layout::{paginate_document, paginate_streaming};
    use crate::sink::MemorySink;
    use crate::style::{compute_styles, parse_stylesheet, user_agent_stylesheet, Stylesheet};

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
    const CJK_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fonts/NotoSansCJK-Regular.ttc"
    );

    fn test_fonts() -> FontCollection {
        FontCollection::new(vec![
            Font::load(DEJAVU_PATH).expect("should load bundled test font")
        ])
    }

    fn test_fonts_with_cjk() -> FontCollection {
        FontCollection::new(vec![
            Font::load(DEJAVU_PATH).expect("should load bundled DejaVu test font"),
            Font::load_indexed(CJK_PATH, 0).expect("should load bundled CJK test font"),
        ])
    }

    fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .filter(|w| *w == needle)
            .count()
    }

    #[test]
    fn streaming_writer_produces_a_valid_pdf_with_embedded_font() {
        let dom = html::parse(b"<p>Hello, world!</p>");
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);

        let mut writer = StreamingPdfWriter::new(
            &fonts,
            settings,
            MemorySink::new(),
            Vec::new(),
            LinkSettings::default(),
        )
        .expect("new should not fail");
        for page in &pages {
            writer
                .write_page(page, &styles, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
        }
        let bytes = writer.finish(&fonts).expect("finish should not fail");

        assert!(bytes.starts_with(b"%PDF-"));
        assert!(count_occurrences(&bytes, b"%%EOF") > 0);
        assert!(count_occurrences(&bytes, b"/Subtype /Type0") > 0);
        assert!(count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0);
        assert!(count_occurrences(&bytes, b"/Identity-H") > 0);
        assert!(count_occurrences(&bytes, b"/FontFile2") > 0);
        assert!(
            count_occurrences(&bytes, b"/Type /CMap") > 0,
            "ToUnicode CMap should be embedded"
        );
        assert!(
            count_occurrences(&bytes, b"/CMapName /Custom") > 0,
            "CMap stream dictionary must carry /CMapName (ISO 32000-1 table 120)"
        );
        assert!(
            count_occurrences(&bytes, b"/CIDSystemInfo") >= 2,
            "CMap stream dictionary must carry /CIDSystemInfo (ISO 32000-1 table 120)"
        );
        assert!(
            count_occurrences(&bytes, b"/Ordering (UCS)") > 0,
            "ToUnicode CMap /CIDSystemInfo should be Adobe-UCS-0"
        );
    }

    #[test]
    fn streaming_writer_output_is_readable_by_pdf_parsing_via_pymupdf_equivalent_checks() {
        // Check that even with several pages and several fonts (where the glyph set changes
        // from page to page) the PDF comes out structurally valid.
        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(pages.len() > 1, "expected multiple pages");

        let mut writer = StreamingPdfWriter::new(
            &fonts,
            settings,
            MemorySink::new(),
            Vec::new(),
            LinkSettings::default(),
        )
        .expect("new should not fail");
        for page in &pages {
            writer
                .write_page(page, &styles, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
        }
        let bytes = writer.finish(&fonts).expect("finish should not fail");

        assert_eq!(count_occurrences(&bytes, b"/MediaBox"), pages.len());
        assert_eq!(count_occurrences(&bytes, b"/FontFile2"), 1);
    }

    #[test]
    fn streaming_writer_subsets_a_large_cjk_font() {
        let dom = html::parse("<p>日本語のテスト</p>".as_bytes());
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts_with_cjk();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);

        let mut writer = StreamingPdfWriter::new(
            &fonts,
            settings,
            MemorySink::new(),
            Vec::new(),
            LinkSettings::default(),
        )
        .expect("new should not fail");
        for page in &pages {
            writer
                .write_page(page, &styles, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
        }
        let bytes = writer.finish(&fonts).expect("finish should not fail");

        let cjk_font_size = std::fs::metadata(CJK_PATH).unwrap().len() as usize;
        assert!(
            bytes.len() < cjk_font_size / 10,
            "subsetted output ({} bytes) should be far smaller than the original CJK font ({} bytes)",
            bytes.len(),
            cjk_font_size
        );
        assert_eq!(count_occurrences(&bytes, b"/FontFile2"), 2);
    }

    #[test]
    fn streaming_writer_handles_glyphs_that_only_appear_on_a_later_page() {
        // The case where characters absent from page 1 ("Q"/"z") appear only on page 2.
        // Font embedding (subsetting plus CIDToGIDMap) happens all at once after every page
        // is processed, so the usage of those glyphs is not yet settled when page 1's content
        // stream is built.
        let dom1 = html::parse(b"<p>Hello, world!</p>");
        let dom2 = html::parse(b"<p>Quick zebra jumps.</p>");
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles1 = compute_styles(&dom1, &ua, &author);
        let styles2 = compute_styles(&dom2, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages1 = paginate_document(&dom1, &styles1, &fonts, &settings);
        let pages2 = paginate_document(&dom2, &styles2, &fonts, &settings);

        let mut writer = StreamingPdfWriter::new(
            &fonts,
            settings,
            MemorySink::new(),
            Vec::new(),
            LinkSettings::default(),
        )
        .expect("new should not fail");
        for page in &pages1 {
            writer
                .write_page(page, &styles1, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
        }
        for page in &pages2 {
            writer
                .write_page(page, &styles2, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
        }
        let bytes = writer.finish(&fonts).expect("finish should not fail");

        assert!(bytes.starts_with(b"%PDF-"));
        assert_eq!(count_occurrences(&bytes, b"/MediaBox"), 2);
        assert_eq!(count_occurrences(&bytes, b"/FontFile2"), 1);
    }

    #[test]
    fn streaming_writer_matches_paginate_streaming_page_count() {
        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let tree = crate::layout::build_box_tree(&dom, &styles);
        let laid_out =
            crate::layout::layout_document(&tree, &styles, &fonts, settings.content_width());

        let mut writer = StreamingPdfWriter::new(
            &fonts,
            settings,
            MemorySink::new(),
            Vec::new(),
            LinkSettings::default(),
        )
        .expect("new should not fail");
        let mut page_count = 0usize;
        let mut laid_out = laid_out;
        paginate_streaming(&mut laid_out, settings.content_height(), &mut |page| {
            writer
                .write_page(&page, &styles, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
            page_count += 1;
        });
        let bytes = writer.finish(&fonts).expect("finish should not fail");

        assert!(page_count > 1);
        assert_eq!(count_occurrences(&bytes, b"/MediaBox"), page_count);
    }

    #[test]
    fn streaming_writer_works_through_a_buffered_s3_style_sink() {
        // Check that `StreamingPdfWriter` copes correctly with `Sink::write` being called
        // many times in small pieces, even through a `BufferedSink` (as intended for S3
        // multipart uploads). It uses the same `crate::sink::BufferedSink` as the real
        // S3-bound buffering sink, with a small threshold to force splitting into parts.

        use crate::sink::BufferedSink;

        let mut html_src = String::from("<div>");
        for i in 0..20 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</div>");
        let dom = html::parse(html_src.as_bytes());

        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(".item { height: 100px; margin: 0; }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert!(pages.len() > 1, "expected multiple pages");

        // Production would use `MULTIPART_MIN_PART_SIZE` (5MB), but the test uses a small
        // threshold to guarantee a split across several parts.
        let mut uploaded_parts: Vec<usize> = Vec::new();
        let sink: BufferedSink<(), std::io::Error, _> = BufferedSink::new(2048, |part| {
            uploaded_parts.push(part.len());
            Ok(())
        });

        let mut writer =
            StreamingPdfWriter::new(&fonts, settings, sink, Vec::new(), LinkSettings::default())
                .expect("new should not fail");
        for page in &pages {
            writer
                .write_page(page, &styles, &HashMap::new(), &fonts, None)
                .expect("write_page should not fail");
        }
        writer.finish(&fonts).expect("finish should not fail");

        assert!(
            uploaded_parts.len() > 1,
            "expected the PDF to be split into multiple upload parts, got {}",
            uploaded_parts.len()
        );
        // Every part but the last should be exactly the threshold size (as S3 requires, only
        // the last part may be under it).
        for &len in &uploaded_parts[..uploaded_parts.len() - 1] {
            assert_eq!(len, 2048);
        }
        assert!(uploaded_parts.last().copied().unwrap_or(0) <= 2048);
    }
}
