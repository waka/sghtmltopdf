//! Encoding the layout result (a [`LaidOutBox`] tree per page) into PDF.
//!
//! In a batch conversion (no streaming), the whole document is assembled with
//! `pdf_writer::Pdf` and written to the [`Sink`] once at the end.
//!
//! Encoding runs in two passes: (1) walk every page and collect, per font, the glyphs
//! actually used; (2) embed the fonts subsetted to just those glyphs, obtain the mapping
//! from original glyph ID to subsetted glyph ID (CID), and only then write the content
//! streams. The already-shaped [`crate::fonts::ShapedGlyph`]s from layout
//! ([`crate::layout::inline`]) are used as-is, so no text is ever reshaped.
//!
//!
//! Text colour, bold and italic are baked into [`crate::layout::inline::TextRun`] at layout
//! time (they can differ per inline element, such as `<b>` or `<span style="...">`), so even
//! an inline fragment left anonymous by pagination (`node: None`) is drawn with the correct
//! appearance.
//!
//! A border edge is drawn only where `border-style` is not `none` and the width is greater
//! than 0. `solid`/`double` fill each edge as a quadrilateral running from the outside of
//! the border box to the inside (a trapezium where the widths differ). Two adjacent edges
//! compute their vertices independently from the shared corners (outer and inner), so even
//! with differing widths and colours the corners mitre diagonally (as in a picture frame).
//! `dashed`/`dotted` express the dash pattern as a stroke, so they keep the traditional
//! approach of stroking the centre line of the width (no mitring).
//! Where no `border-radius` is set and all four edges share a width, style and colour, they
//! are stroked together as one rounded Bezier path; anything else (no rounding, or four
//! edges that differ) falls back to the per-edge drawing above.
//!
//! A box fragmented by pagination (see [`crate::layout::FragmentPosition`]) does not apply
//! `border-radius` to a continuing edge (one touching a break)
//! (the rounding is suppressed using the information layout passed as `Layout::fragment`).
//!
//! - Bold and italic do not require a separate font file with those glyph shapes: the
//!   regular shapes are filled and outlined (faux bold) or the text matrix is sheared (faux
//!   italic) instead
//! - Where several fonts and font sizes are mixed on one line, the line's baseline is
//!   aligned against the metrics of the first run's font and size
//! - Even where `border-radius` is set, if the four edges differ in width, style or colour
//!   the rounding is given up and it falls back to stroking four straight edges (complex
//!   per-corner blending is not supported)
//! - `border-style`'s `groove`/`ridge`/`inset`/`outset` (two-tone pseudo-3D shading) are not supported

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use pdf_writer::types::{ActionType, AnnotationType, LineCapStyle, TextRenderingMode};
use pdf_writer::{Content, Finish, Name, Pdf, Rect as PdfRect, Ref, TextStr};

use crate::fonts::{Font, FontCollection};
use crate::html::NodeId;
use crate::img::resolve_against_base_href;
use crate::layout::{
    resolve_border, shape_standalone_line, EdgeSizes, EmphasisMark, FragmentPosition, LaidOutBox,
    LaidOutContent, LaidOutTableRow, Layout, LineBox, Page, PageSettings, Rect, TextRun,
};
use crate::sink::Sink;
use crate::style::{
    compose_transform, resolve_margin_box_content, resolve_page_rules, BackgroundRepeat,
    BackgroundSize, BorderCollapse, BorderStyle, Color, ComputedBoxShadow, ComputedStyle,
    CornerRadius, EmphasisPosition, EmphasisShape, EmphasisStyle, EmptyCells, Length,
    LengthPercentage, LengthPercentageOrAuto, MarginBoxArea, ObjectFit, PageRule, Position,
    PropertyDeclaration, RgbaColor,
};

use super::font::{deflate, embed_font, FontIds, FontUsage};
use super::img::{embed_image, ids_for_image, image_resource_name, ImageIds, PreparedImage};
use super::options::{current_datetime, producer_string, DocumentMetadata, PdfOutputOptions};

/// A wrapper interposing colour conversion on `Content`.
///
/// Handing `PdfOutputOptions` to every drawing function for the sake of `--grayscale` would
/// be too invasive (`settings` is referenced in 244 places), so only the paths that write
/// colour are wrapped in this type. `Deref`/`DerefMut` keep `Content`'s methods usable as-is,
/// and only `set_fill_rgb`/`set_stroke_rgb` are overridden by the implementation here.
pub(super) struct RenderTarget<'a> {
    content: &'a mut Content,
    grayscale: bool,
}

impl<'a> RenderTarget<'a> {
    pub(super) fn new(content: &'a mut Content, grayscale: bool) -> Self {
        Self { content, grayscale }
    }

    /// Wrap another `Content` (a form XObject's contents, say) with the same settings.
    pub(super) fn wrap<'b>(&self, content: &'b mut Content) -> RenderTarget<'b> {
        RenderTarget::new(content, self.grayscale)
    }

    fn map(&self, r: f32, g: f32, b: f32) -> (f32, f32, f32) {
        if !self.grayscale {
            return (r, g, b);
        }
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        (y, y, y)
    }

    pub(super) fn set_fill_rgb(&mut self, r: f32, g: f32, b: f32) -> &mut Content {
        let (r, g, b) = self.map(r, g, b);
        self.content.set_fill_rgb(r, g, b)
    }

    pub(super) fn set_stroke_rgb(&mut self, r: f32, g: f32, b: f32) -> &mut Content {
        let (r, g, b) = self.map(r, g, b);
        self.content.set_stroke_rgb(r, g, b)
    }
}

impl std::ops::Deref for RenderTarget<'_> {
    type Target = Content;

    fn deref(&self) -> &Self::Target {
        self.content
    }
}

impl std::ops::DerefMut for RenderTarget<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.content
    }
}

/// Encode a DOM-derived layout result (a list of pages) into PDF bytes.
pub fn encode_pdf(
    pages: &[Page],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    fonts: &FontCollection,
    settings: &PageSettings,
) -> Vec<u8> {
    encode_pdf_with_anchors(
        pages,
        styles,
        background_images,
        fonts,
        settings,
        &LinkSettings::default(),
    )
}

/// The version of [`encode_pdf`] that also takes the internal anchor table (`<a href="#id">`).
///
/// `links` is the internal anchor table plus `<base href>` ([`LinkSettings`]).
/// Passing the default value generates only external link annotations (the split exists so
/// the existing `encode_pdf` signature need not change).
pub fn encode_pdf_with_anchors(
    pages: &[Page],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    links: &LinkSettings,
) -> Vec<u8> {
    encode_pdf_with_options(
        pages,
        styles,
        background_images,
        fonts,
        settings,
        links,
        &PdfOutputOptions::default(),
    )
}

/// The version of [`encode_pdf_with_anchors`] that also takes the PDF output options
/// (metadata, compression, scale and grayscale).
pub fn encode_pdf_with_options(
    pages: &[Page],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    links: &LinkSettings,
    output: &PdfOutputOptions,
) -> Vec<u8> {
    let mut pdf = Pdf::new();
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
            // `encode_pdf` uses `/CIDToGIDMap /Identity` (embed_font) and never refers to it,
            // but it is allocated anyway to keep `FontIds` the same type as the one
            // `embed_font_streaming_chunks` uses.
            cid_to_gid_map: alloc.next(),
        })
        .collect();
    let font_resource_names: Vec<String> = (0..fonts.len()).map(|i| format!("F{i}")).collect();

    // The ExtGStates for semi-transparent drawing of `background-color`/`box-shadow`.
    // Regardless of use, 21 steps of 0.05 are allocated once for the whole document and, like
    // the fonts, listed unconditionally in every page's Resources.
    let alpha_gs_ids: Vec<Ref> = (0..=ALPHA_STEPS).map(|_| alloc.next()).collect();
    let alpha_gs_names: Vec<String> = (0..=ALPHA_STEPS).map(alpha_gs_resource_name).collect();
    for (step, &id) in alpha_gs_ids.iter().enumerate() {
        let a = step as f32 / ALPHA_STEPS as f32;
        pdf.ext_graphics(id).non_stroking_alpha(a).stroking_alpha(a);
    }

    // Pass 1: collect the glyphs used (no content stream is written yet).
    let mut usages: Vec<FontUsage> = (0..fonts.len()).map(|_| FontUsage::default()).collect();
    for page in pages {
        for b in &page.boxes {
            collect_usage(b, fonts, &mut usages);
        }
    }

    // Embed the fonts subsetted to just those glyphs, obtaining the original GID to CID mapping.
    let remaps: Vec<HashMap<u16, u16>> = fonts
        .fonts()
        .iter()
        .zip(font_ids.iter())
        .zip(usages.iter())
        .map(|((font, &ids), usage)| {
            embed_font(&mut pdf, font, ids, usage, output.compress)
                .into_iter()
                .collect()
        })
        .collect();

    // Pass 2: actually write the pages' content streams. Unlike fonts, image XObjects need no
    // up-front subsetting information to be reused across pages, so "write it if this is its
    // first appearance" per page is enough.
    let mut image_ids: HashMap<usize, ImageIds> = HashMap::new();
    // A record so an SVG whose renumbering failed is warned about only once per document.
    let mut failed_svg_ids: HashSet<usize> = HashSet::new();
    let mut page_ids = Vec::with_capacity(pages.len());
    // Named destinations (`/Dests`) are resolved once every page has been written.
    let mut destinations: Vec<(String, Ref, f32, f32)> = Vec::new();
    let mut link_annotations: Vec<(Ref, LinkArea)> = Vec::new();
    for page in pages {
        let page_id = alloc.next();
        let content_id = alloc.next();
        page_ids.push(page_id);

        let mut used_images = Vec::new();
        for b in &page.boxes {
            collect_image_uses(b, background_images, &mut used_images);
        }
        let mut page_image_refs = Vec::with_capacity(used_images.len());
        for image in &used_images {
            // An SVG whose `Ref` renumbering failed becomes `None` (and is not drawn).
            let Some((ids, is_new)) =
                ids_for_image(&mut alloc, &mut image_ids, &mut failed_svg_ids, image)
            else {
                continue;
            };
            if is_new {
                embed_image(&mut pdf, image, ids, output.grayscale);
            }
            page_image_refs.push(ids.root);
        }

        // Collect the elements with `opacity < 1` first and allocate their Refs (the same
        // structure as images and fonts). Turning them into Form XObjects (drawing and
        // embedding the subtree) happens inside `render_box`, piling up in `pending_forms`.
        let mut opacity_nodes = Vec::new();
        for b in &page.boxes {
            collect_opacity_uses(b, styles, &mut opacity_nodes);
        }
        let opacity_form_ids: HashMap<NodeId, Ref> =
            opacity_nodes.iter().map(|&n| (n, alloc.next())).collect();
        let mut pending_forms: Vec<(Ref, Vec<u8>)> = Vec::new();

        let mut content = Content::new();
        // The CSS px to PDF pt conversion is done by the page's overall CTM.
        content.transform([output.scale, 0.0, 0.0, output.scale, 0.0, 0.0]);
        // A wrapper interposing the colour conversion.
        let mut target = RenderTarget::new(&mut content, output.grayscale);
        for b in &page.boxes {
            render_box(
                &mut target,
                b,
                styles,
                fonts,
                settings,
                Some(&remaps),
                &font_resource_names,
                &image_ids,
                background_images,
                &alpha_gs_names,
                &opacity_form_ids,
                &mut pending_forms,
            );
        }
        let content_bytes = content.finish();

        // Collect the `<a href>` annotations and the positions of the anchors landing on this page.
        let mut page_links = Vec::new();
        let mut page_anchors = Vec::new();
        for b in &page.boxes {
            collect_link_areas(b, settings, &mut page_links);
            collect_anchor_positions(b, &links.anchor_names, settings, &mut page_anchors);
        }
        // Drop the kinds of link that have been disabled (`--disable-external-links` and so on).
        links.retain_enabled(&mut page_links);
        for (name, x, y) in page_anchors {
            if !destinations.iter().any(|(existing, ..)| *existing == name) {
                destinations.push((name, page_id, x, y));
            }
        }
        let page_annotation_ids: Vec<Ref> = page_links
            .into_iter()
            .map(|area| {
                let id = alloc.next();
                link_annotations.push((id, area));
                id
            })
            .collect();

        let form_refs: Vec<Ref> = pending_forms.iter().map(|(id, _)| *id).collect();
        let mut p = pdf.page(page_id);
        p.parent(pages_tree_id);
        p.media_box(PdfRect::new(
            0.0,
            0.0,
            output.to_pt(settings.size.width),
            output.to_pt(settings.size.height),
        ));
        p.contents(content_id);
        if !page_annotation_ids.is_empty() {
            p.annotations(page_annotation_ids.iter().copied());
        }
        write_resources(
            p.resources(),
            &font_resource_names,
            &font_ids,
            &page_image_refs,
            &form_refs,
            &alpha_gs_names,
            &alpha_gs_ids,
        );
        p.finish();

        let stream_bytes = if output.compress {
            deflate(&content_bytes)
        } else {
            content_bytes.to_vec()
        };
        let mut content_stream = pdf.stream(content_id, &stream_bytes);
        if output.compress {
            content_stream.filter(pdf_writer::Filter::FlateDecode);
        }
        content_stream.finish();

        // Write out the Form XObjects of the opacity groups for real. `/BBox` is the page's
        // whole content area, erring on the safe side given that drawing can exceed the border
        // box through box-shadow bleed, `overflow: visible` or a combination with transform.

        for (form_ref, bytes) in &pending_forms {
            let mut form = pdf.form_xobject(*form_ref, bytes);
            form.bbox(PdfRect::new(
                0.0,
                0.0,
                settings.size.width,
                settings.size.height,
            ));
            form.group().transparency().isolated(true).knockout(false);
            write_resources(
                form.resources(),
                &font_resource_names,
                &font_ids,
                &page_image_refs,
                &form_refs,
                &alpha_gs_names,
                &alpha_gs_ids,
            );
        }
    }

    // Write the annotations themselves. An internal anchor only references a named
    // destination, so which page the target is on (a forward reference or not) does not matter.
    for (id, area) in &link_annotations {
        write_link_annotation(
            pdf.annotation(*id),
            area,
            links.annotation_base_href(),
            output.scale,
        );
    }

    let dests_id = (!destinations.is_empty()).then(|| alloc.next());
    if let Some(dests_id) = dests_id {
        let mut dests = pdf.destinations(dests_id);
        for (name, page_id, x, y) in &destinations {
            dests.insert(Name(name.as_bytes())).page(*page_id).xyz(
                output.to_pt(*x),
                output.to_pt(*y),
                None,
            );
        }
    }

    pdf.pages(pages_tree_id)
        .kids(page_ids.iter().copied())
        .count(page_ids.len() as i32);
    let mut catalog = pdf.catalog(catalog_id);
    catalog.pages(pages_tree_id);
    if let Some(dests_id) = dests_id {
        catalog.destinations(dests_id);
    }
    catalog.finish();

    let info_id = alloc.next();
    write_document_info(pdf.document_info(info_id), &output.metadata);

    let id = file_identifier(&output.metadata, pages.len());
    pdf.set_file_id((id.clone(), id));

    pdf.finish()
}

/// Write one `/Link` annotation. An internal anchor (`#id`) writes a named destination
/// (`/Dest`), and an external link a `/URI` action.
pub(super) fn write_link_annotation(
    mut annotation: pdf_writer::writers::Annotation<'_>,
    area: &LinkArea,
    base_href: Option<&str>,
    scale: f32,
) {
    annotation.subtype(AnnotationType::Link);
    // An annotation's `/Rect` is in page coordinates (unaffected by the content stream's CTM),
    // so the CSS px to pt conversion happens here.
    annotation.rect(PdfRect::new(
        area.x0 * scale,
        area.y0 * scale,
        area.x1 * scale,
        area.y1 * scale,
    ));
    // Remove the default border (some viewers draw a black frame).
    annotation.border(0.0, 0.0, 0.0, None);

    match internal_anchor_target(&area.href) {
        // It only writes a name, so the target may be on a later page not yet written. If the
        // target does not exist, the name never appears in `/Dests` and a viewer simply does
        // nothing on a click.
        Some(id) => {
            let name = anchor_destination_name(id);
            annotation.pair(Name(b"Dest"), Name(name.as_bytes()));
        }
        None => {
            // A PDF viewer cannot resolve a relative URL, so it is resolved first when
            // `<base href>` is an absolute URL.
            let uri = resolve_against_base_href(base_href, &area.href);
            annotation
                .action()
                .action_type(ActionType::Uri)
                .uri(pdf_writer::Str(uri.as_bytes()));
        }
    }
}

/// Build the name used for the PDF named destination from an anchor's `id`.
///
/// Using the `id` value directly as a name object would require escaping spaces, `#` and
/// delimiters. Only ASCII alphanumerics plus `-`/`_` are kept, everything else is replaced
/// with `_`, and a prefix is added (a collision merely resolves "destinations of the same
/// name to the first one" and breaks nothing).
pub fn anchor_destination_name(id: &str) -> String {
    let mut name = String::with_capacity(id.len() + 2);
    name.push_str("a_");
    for ch in id.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            name.push(ch);
        } else {
            name.push('_');
        }
    }
    name
}

/// Take the result of [`crate::layout::paginate_document`] all the way through to writing it to `sink`.
pub fn write_document<S: Sink>(
    pages: &[Page],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    sink: S,
) -> Result<S::Output, S::Error> {
    write_document_with_options(
        pages,
        styles,
        background_images,
        fonts,
        settings,
        &LinkSettings::default(),
        &PdfOutputOptions::default(),
        sink,
    )
}

/// The version of [`write_document`] that also takes the link settings and the PDF output options.
#[allow(clippy::too_many_arguments)]
pub fn write_document_with_options<S: Sink>(
    pages: &[Page],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    links: &LinkSettings,
    output: &PdfOutputOptions,
    mut sink: S,
) -> Result<S::Output, S::Error> {
    let bytes = encode_pdf_with_options(
        pages,
        styles,
        background_images,
        fonts,
        settings,
        links,
        output,
    );
    sink.write(&bytes)?;
    sink.finish()
}

/// The `/ID` (file identifier) written in the trailer.
///
/// The PDF spec makes it an array of two strings: the first a permanent identifier fixed
/// when the document is created, the second one that changes on every update. This crate
/// performs no incremental updates, so the same value is written for both.
///
/// The spec says nothing about the contents beyond being unique per document. Sixteen bytes
/// are built from a hash mixing the same metadata, creation date and page count written to
/// the Info dictionary. PDF/A requires a file identifier, so without one it does not conform.
pub(super) fn file_identifier(metadata: &DocumentMetadata, page_count: usize) -> Vec<u8> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let datetime = current_datetime();
    let mut id = Vec::with_capacity(16);
    // Two 64-bit hashes joined to make 16 bytes (a different salt gives a different value).
    for salt in [0u64, 0x9e37_79b9_7f4a_7c15] {
        let mut hasher = DefaultHasher::new();
        salt.hash(&mut hasher);
        producer_string().hash(&mut hasher);
        metadata.title.hash(&mut hasher);
        metadata.author.hash(&mut hasher);
        metadata.subject.hash(&mut hasher);
        metadata.keywords.hash(&mut hasher);
        datetime.hash(&mut hasher);
        page_count.hash(&mut hasher);
        // `pdf_writer::Finish` is also in scope, so which `finish` is meant is made explicit.
        id.extend_from_slice(&Hasher::finish(&hasher).to_be_bytes());
    }
    id
}

/// Write the PDF Info dictionary. `/Producer` and `/CreationDate` are always written; the
/// rest only when given.
pub(super) fn write_document_info(
    mut info: pdf_writer::writers::DocumentInfo<'_>,
    metadata: &DocumentMetadata,
) {
    if let Some(title) = metadata.title.as_deref() {
        info.title(TextStr(title));
    }
    if let Some(author) = metadata.author.as_deref() {
        info.author(TextStr(author));
    }
    if let Some(subject) = metadata.subject.as_deref() {
        info.subject(TextStr(subject));
    }
    if let Some(keywords) = metadata.keywords.as_deref() {
        info.keywords(TextStr(keywords));
    }
    info.producer(TextStr(&producer_string()));

    let (year, month, day, hour, minute, second) = current_datetime();
    let date = pdf_writer::Date::new(year as u16)
        .month(month as u8)
        .day(day as u8)
        .hour(hour as u8)
        .minute(minute as u8)
        .second(second as u8)
        .utc_offset_hour(0)
        .utc_offset_minute(0);
    info.creation_date(date);
    info.finish();
}

/// The shared logic assembling a page's `/Resources` dictionary, and the `/Resources`
/// dictionary of each opacity group Form XObject (which has the same contents as the page's).
#[allow(clippy::too_many_arguments)]
pub(super) fn write_resources(
    mut resources: pdf_writer::writers::Resources<'_>,
    font_resource_names: &[String],
    font_ids: &[FontIds],
    page_image_refs: &[Ref],
    form_refs: &[Ref],
    alpha_gs_names: &[String],
    alpha_gs_ids: &[Ref],
) {
    let mut font_dict = resources.fonts();
    for (name, ids) in font_resource_names.iter().zip(font_ids.iter()) {
        font_dict.pair(Name(name.as_bytes()), ids.type0_font);
    }
    font_dict.finish();
    let mut xobject_dict = resources.x_objects();
    for color_ref in page_image_refs {
        xobject_dict.pair(Name(image_resource_name(*color_ref).as_bytes()), *color_ref);
    }
    for &form_ref in form_refs {
        xobject_dict.pair(Name(form_resource_name(form_ref).as_bytes()), form_ref);
    }
    xobject_dict.finish();
    let mut ext_g_state_dict = resources.ext_g_states();
    for (name, &id) in alpha_gs_names.iter().zip(alpha_gs_ids.iter()) {
        ext_g_state_dict.pair(Name(name.as_bytes()), id);
    }
}

#[derive(Default)]
pub(super) struct RefAllocator(i32);

impl RefAllocator {
    pub(super) fn next(&mut self) -> Ref {
        self.0 += 1;
        Ref::new(self.0)
    }

    /// Peek at the next `Ref` to be allocated without consuming it.
    ///
    /// Used to "try allocating and, if it did not work out, pretend we never did"
    /// (an SVG's `Ref` renumbering can fail, and consuming the numbers on failure would leave
    /// object numbers that are never written, breaking `StreamingPdfWriter`'s xref assumption
    /// that "everything from 1 upwards is written in sequence").
    #[cfg(feature = "svg")]
    pub(super) fn peek(&self) -> Ref {
        Ref::new(self.0 + 1)
    }

    /// Consume `count` numbers starting from [`peek`](Self::peek), all at once.
    #[cfg(feature = "svg")]
    pub(super) fn commit(&mut self, count: usize) {
        self.0 += i32::try_from(count).expect("the number of Refs allocated does not fit an i32");
    }
}

pub(super) fn collect_usage(b: &LaidOutBox, fonts: &FontCollection, usages: &mut [FontUsage]) {
    if let Some(marker) = &b.marker {
        collect_line_usage(marker, fonts, usages);
    }
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_usage(child, fonts, usages);
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                collect_usage(child, fonts, usages);
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines {
                collect_line_usage(line, fonts, usages);
                // The contents of a `display: inline-block` within a line use the same
                // document's glyphs.
                for atomic in &line.atomics {
                    collect_usage(&atomic.content, fonts, usages);
                }
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                collect_usage(caption, fonts, usages);
            }
            for row in &table.rows {
                for cell in &row.cells {
                    collect_usage(cell, fonts, usages);
                }
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

/// Collect the glyphs `line` really uses (an ordinary line, or a synthesised single-run
/// `LineBox` representing a `display: list-item` marker).
fn collect_line_usage(line: &LineBox, fonts: &FontCollection, usages: &mut [FontUsage]) {
    for run in &line.runs {
        let Some(font) = fonts.get(run.font_index) else {
            continue;
        };
        for (i, glyph) in run.glyphs.iter().enumerate() {
            let text = cluster_text(&run.text, &run.glyphs, i);
            usages[run.font_index].record(font, glyph.glyph_id, text);
        }
        // A `text-emphasis-style: <string>` mark draws as a glyph a character that never
        // appears in the text, so it is recorded here to keep it out of the subsetting
        // (a keyword mark is drawn as a path, so it needs no collecting).
        if let Some(EmphasisStyle::String(ch)) = run.emphasis.as_ref().map(|mark| &mark.style) {
            if let Some(glyph_id) = font.glyph_id(*ch) {
                usages[run.font_index].record(font, glyph_id, ch.encode_utf8(&mut [0u8; 4]));
            }
        }
    }
}

/// Cut from `text` the original text (the cluster) that `glyphs[index]` represents.
///
/// `ShapedGlyph::cluster` is a byte offset into the original text, and shaping can leave one
/// glyph corresponding to several characters (a ligature such as `fl`). Everything up to the
/// next glyph whose offset advances is treated as the string that glyph represents.
/// Truncating that to one character would leave `/ToUnicode` incomplete and lose characters
/// when a PDF is searched or copied.
///
/// Conversely, where several glyphs correspond to one cluster (combining characters and the
/// like), or the cluster moves backwards (RTL), only the first character is assigned, as
/// before. In the first case assigning the whole cluster to every glyph would duplicate
/// characters on extraction, and in the second the cluster's extent cannot be decided from the front alone.
fn cluster_text<'a>(text: &'a str, glyphs: &[crate::fonts::ShapedGlyph], index: usize) -> &'a str {
    let start = (glyphs[index].cluster as usize).min(text.len());
    let single_char = || {
        let len = text[start..].chars().next().map_or(0, char::len_utf8);
        &text[start..start + len]
    };

    // The whole cluster is assigned only when this glyph alone is responsible for it.
    if index > 0 && glyphs[index - 1].cluster as usize == start {
        return single_char();
    }
    let end = match glyphs.get(index + 1) {
        Some(next) if (next.cluster as usize) > start => (next.cluster as usize).min(text.len()),
        Some(_) => return single_char(),
        None => text.len(),
    };

    // Where a cluster boundary disagrees with a character boundary (an unexpected shaping
    // result), `get` is used so slicing the substring cannot panic.
    text.get(start..end).unwrap_or_else(single_char)
}

/// Walk the page (or pages) recursively and collect the images actually used (both `<img>`
/// itself and `background-image`), deduplicated by `Rc` pointer identity. The same
/// "collect the usage first, then allocate the Refs" structure as fonts' `collect_usage`.
/// `background_images` is the `NodeId` to `Rc<PreparedImage>` side map.
pub(super) fn collect_image_uses(
    b: &LaidOutBox,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    out: &mut Vec<Rc<PreparedImage>>,
) {
    if let Some(image) = b.node.and_then(|n| background_images.get(&n)) {
        push_unique_image(out, image);
    }

    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_image_uses(child, background_images, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                collect_image_uses(child, background_images, out);
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                collect_image_uses(caption, background_images, out);
            }
            for row in &table.rows {
                for cell in &row.cells {
                    collect_image_uses(cell, background_images, out);
                }
            }
        }
        LaidOutContent::Image(Some(image)) => push_unique_image(out, image),
        LaidOutContent::Inline(lines) => {
            for line in lines {
                for atomic in &line.atomics {
                    collect_image_uses(&atomic.content, background_images, out);
                }
            }
        }
        LaidOutContent::Image(None) => {}
    }
}

/// The document-wide settings needed to generate link annotations.
#[derive(Debug, Clone)]
pub struct LinkSettings {
    /// The `NodeId` of an anchor target element, mapped to the named destination's name. When
    /// empty, no internal anchor destinations are generated (the links themselves are still
    /// written, but clicking one in a viewer does nothing).
    pub anchor_names: HashMap<NodeId, String>,
    /// The `<base href>`. External links' relative URLs resolve against it.
    pub base_href: Option<String>,
    /// Whether to emit annotations for external links (http(s)) (`--disable-external-links`).
    pub external: bool,
    /// Whether to emit annotations for internal links (`#id`) (`--disable-internal-links`).
    pub internal: bool,
    /// Whether to write a relative URL as-is rather than making it absolute with `<base href>`
    /// (`--keep-relative-links`).
    pub keep_relative: bool,
}

impl Default for LinkSettings {
    fn default() -> Self {
        Self {
            anchor_names: HashMap::new(),
            base_href: None,
            external: true,
            internal: true,
            keep_relative: false,
        }
    }
}

impl LinkSettings {
    /// Remove the kinds that have been disabled from the collected link rectangles.
    pub(super) fn retain_enabled(&self, areas: &mut Vec<LinkArea>) {
        areas.retain(|area| {
            if internal_anchor_target(&area.href).is_some() {
                self.internal
            } else {
                self.external
            }
        });
    }

    /// The `<base href>` handed to the annotations. With `--keep-relative-links` it is
    /// `None`, so it is never used for resolution.
    pub(super) fn annotation_base_href(&self) -> Option<&str> {
        if self.keep_relative {
            None
        } else {
            self.base_href.as_deref()
        }
    }
}

/// One PDF `/Link` annotation (a rectangle within the page plus its destination).
///
/// The rectangle is in PDF coordinates (origin at the bottom left, y upwards, absolute from the page's bottom left).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LinkArea {
    pub href: Rc<str>,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Walk the boxes on a page and collect the `/Link` annotation rectangles from the text runs
/// belonging to an `<a href>`. Consecutive runs of the same link within a line are merged
/// into one rectangle, and what wrapped onto another line becomes a separate rectangle.
pub(super) fn collect_link_areas(b: &LaidOutBox, settings: &PageSettings, out: &mut Vec<LinkArea>) {
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_link_areas(child, settings, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                collect_link_areas(child, settings, out);
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                collect_link_areas(caption, settings, out);
            }
            for row in &table.rows {
                for cell in &row.cells {
                    collect_link_areas(cell, settings, out);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines {
                push_line_link_areas(line, settings, out);
                for atomic in &line.atomics {
                    collect_link_areas(&atomic.content, settings, out);
                }
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

fn push_line_link_areas(line: &LineBox, settings: &PageSettings, out: &mut Vec<LinkArea>) {
    let mut current: Option<LinkArea> = None;
    for run in &line.runs {
        let Some(href) = &run.link else {
            if let Some(area) = current.take() {
                out.push(area);
            }
            continue;
        };
        let x0 = settings.margin.left + line.rect.x + run.x_offset;
        let x1 = x0 + run.width;
        // The annotation's height is the ascent-to-descent range, relative to the run's
        // baseline (including any `vertical-align` shift).
        let baseline_y = to_pdf_y(settings, line.rect.y + line.baseline) + run.baseline_shift;
        let y0 = baseline_y - run.descent;
        let y1 = baseline_y + run.ascent;

        match &mut current {
            // The rectangle is extended while the same link continues.
            Some(area) if area.href == *href => {
                area.x1 = area.x1.max(x1);
                area.y0 = area.y0.min(y0);
                area.y1 = area.y1.max(y1);
            }
            _ => {
                if let Some(area) = current.take() {
                    out.push(area);
                }
                current = Some(LinkArea {
                    href: href.clone(),
                    x0,
                    y0,
                    x1,
                    y1,
                });
            }
        }
    }
    if let Some(area) = current {
        out.push(area);
    }
}

/// Walk the boxes on a page and collect where each anchor target (a `NodeId` present in
/// `anchor_names`) first appears (the PDF y coordinate of the top of its border box).
pub(super) fn collect_anchor_positions(
    b: &LaidOutBox,
    anchor_names: &HashMap<NodeId, String>,
    settings: &PageSettings,
    out: &mut Vec<(String, f32, f32)>,
) {
    if let Some(name) = b.node.and_then(|n| anchor_names.get(&n)) {
        if !out.iter().any(|(existing, _, _)| existing == name) {
            let border_box = b.layout.border_box();
            out.push((
                name.clone(),
                settings.margin.left + border_box.x,
                to_pdf_y(settings, border_box.y),
            ));
        }
    }

    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_anchor_positions(child, anchor_names, settings, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                collect_anchor_positions(child, anchor_names, settings, out);
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                collect_anchor_positions(caption, anchor_names, settings, out);
            }
            for row in &table.rows {
                for cell in &row.cells {
                    collect_anchor_positions(cell, anchor_names, settings, out);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines {
                for atomic in &line.atomics {
                    collect_anchor_positions(&atomic.content, anchor_names, settings, out);
                }
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

/// If `href` is an internal anchor (`#id`), return its `id` part.
pub(super) fn internal_anchor_target(href: &str) -> Option<&str> {
    href.strip_prefix('#').filter(|id| !id.is_empty())
}

/// Walk the page (or pages) recursively and collect the `NodeId`s of elements with
/// `opacity < 1`. The same "collect the usage first, then allocate the Refs" structure as
/// fonts and images. An `opacity` element always corresponds to a real DOM element (an
/// anonymous box has no `style` and so cannot carry `opacity`), so `b.node` should always be `Some`.
pub(super) fn collect_opacity_uses(
    b: &LaidOutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    out: &mut Vec<NodeId>,
) {
    if let Some(node) = b.node {
        if styles.get(&node).is_some_and(|s| s.opacity < 1.0) {
            out.push(node);
        }
    }
    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in children {
                collect_opacity_uses(child, styles, out);
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                collect_opacity_uses(child, styles, out);
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                collect_opacity_uses(caption, styles, out);
            }
            for row in &table.rows {
                for cell in &row.cells {
                    collect_opacity_uses(cell, styles, out);
                }
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines {
                for atomic in &line.atomics {
                    collect_opacity_uses(&atomic.content, styles, out);
                }
            }
        }
        LaidOutContent::Image(_) => {}
    }
}

/// The fixed resource name for the Form XObject of a `Ref` allocated by
/// [`collect_opacity_uses`] (the same pattern as images' `image_resource_name`).
pub(super) fn form_resource_name(form_ref: Ref) -> String {
    format!("Fo{}", form_ref.get())
}

fn push_unique_image(out: &mut Vec<Rc<PreparedImage>>, image: &Rc<PreparedImage>) {
    if !out.iter().any(|existing| Rc::ptr_eq(existing, image)) {
        out.push(image.clone());
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn render_box(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    image_ids: &HashMap<usize, ImageIds>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    alpha_gs_names: &[String],
    opacity_form_ids: &HashMap<NodeId, Ref>,
    pending_forms: &mut Vec<(Ref, Vec<u8>)>,
) {
    let style = b
        .node
        .and_then(|n| styles.get(&n))
        .cloned()
        .unwrap_or_default();
    render_box_with_style(
        content,
        b,
        &style,
        styles,
        fonts,
        settings,
        remaps,
        font_resource_names,
        image_ids,
        background_images,
        alpha_gs_names,
        opacity_form_ids,
        pending_forms,
    );
}

/// The body of [`render_box`]. It normally draws with the plain `style` looked up from
/// `b.node`, but `style` is a separate parameter so the caller can draw with an overridden
/// style, as with a cell under `border-collapse: collapse` (which needs the borders merged
/// with its neighbours).
///
/// Where a `transform` is set, the actual drawing
/// ([`render_box_with_style_inner`]) is wrapped in a `q cm ... Q` (a CTM operation) in the
/// content stream. It never affects layout (a visual effect only, as the CSS spec says).
#[allow(clippy::too_many_arguments)]
fn render_box_with_style(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    image_ids: &HashMap<usize, ImageIds>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    alpha_gs_names: &[String],
    opacity_form_ids: &HashMap<NodeId, Ref>,
    pending_forms: &mut Vec<(Ref, Vec<u8>)>,
) {
    if style.transform.is_empty() {
        render_box_opacity_wrapped(
            content,
            b,
            style,
            styles,
            fonts,
            settings,
            remaps,
            font_resource_names,
            image_ids,
            background_images,
            alpha_gs_names,
            opacity_form_ids,
            pending_forms,
        );
        return;
    }

    let pdf_matrix = transform_matrix_pdf_space(b, style, settings);
    content.save_state();
    content.transform(pdf_matrix);
    render_box_opacity_wrapped(
        content,
        b,
        style,
        styles,
        fonts,
        settings,
        remaps,
        font_resource_names,
        image_ids,
        background_images,
        alpha_gs_names,
        opacity_form_ids,
        pending_forms,
    );
    content.restore_state();
}

/// Where `opacity < 1`, the actual drawing ([`render_box_with_style_inner`]) is written to a
/// separate `Content` and pushed onto `pending_forms` as a PDF transparency group (a Form
/// XObject with `/Group /S /Transparency`), while the original `content` gets only
/// `q /GSn gs /FoN Do Q`. It has to be called inside the `transform` CTM wrapper, hence its
/// separation from `render_box_with_style`.
#[allow(clippy::too_many_arguments)]
fn render_box_opacity_wrapped(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    image_ids: &HashMap<usize, ImageIds>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    alpha_gs_names: &[String],
    opacity_form_ids: &HashMap<NodeId, Ref>,
    pending_forms: &mut Vec<(Ref, Vec<u8>)>,
) {
    if style.opacity >= 1.0 {
        render_box_with_style_inner(
            content,
            b,
            style,
            styles,
            fonts,
            settings,
            remaps,
            font_resource_names,
            image_ids,
            background_images,
            alpha_gs_names,
            opacity_form_ids,
            pending_forms,
        );
        return;
    }

    // `collect_opacity_uses` should have allocated this node's Ref in advance
    // (`b.node` is always a real DOM element).
    let form_ref = *b
        .node
        .and_then(|n| opacity_form_ids.get(&n))
        .expect("an element with opacity < 1 should have had a Ref allocated in advance");

    let mut sub_content = Content::new();
    let mut sub_target = content.wrap(&mut sub_content);
    render_box_with_style_inner(
        &mut sub_target,
        b,
        style,
        styles,
        fonts,
        settings,
        remaps,
        font_resource_names,
        image_ids,
        background_images,
        alpha_gs_names,
        opacity_form_ids,
        pending_forms,
    );
    pending_forms.push((form_ref, sub_content.finish().to_vec()));

    content.save_state();
    apply_fill_alpha(content, style.opacity, alpha_gs_names);
    content.x_object(Name(form_resource_name(form_ref).as_bytes()));
    content.restore_state();
}

/// Build the PDF `cm` operands from `b`'s `transform`/`transform-origin`.
/// The functions are composed in CSS coordinates (Y downwards) and then converted to PDF
/// coordinates (Y upwards) first (at that point the translation component is still the
/// relative amount from `translate`, so flipping the sign of Y converts it correctly), and
/// finally `transform-origin` is converted to PDF coordinates (absolute within the page) and
/// applied as the reference point. Translating the origin on the CSS side first would mix
/// the page height offset into the matrix's rotation and scale components and give the wrong
/// result, so the order matters: adjust the origin only after converting to PDF coordinates.
fn transform_matrix_pdf_space(
    b: &LaidOutBox,
    style: &ComputedStyle,
    settings: &PageSettings,
) -> [f32; 6] {
    let border_box = b.layout.border_box();
    let css_matrix = compose_transform(&style.transform, border_box.width, border_box.height);
    let [a, b_, c, d, e, f] = css_matrix;
    // The conjugate of the Y-axis flip. The e/f of `translate`/`matrix` are always relative
    // amounts (never absolute page coordinates), so flipping the sign alone gives the correct relative amount in PDF coordinates.
    let pdf_matrix_no_origin = [a, -b_, -c, d, e, -f];

    let origin_x = settings.margin.left
        + border_box.x
        + resolve_length_percentage(style.transform_origin.horizontal, border_box.width);
    let origin_y = to_pdf_y(
        settings,
        border_box.y
            + resolve_length_percentage(style.transform_origin.vertical, border_box.height),
    );
    apply_transform_origin(pdf_matrix_no_origin, origin_x, origin_y)
}

fn resolve_length_percentage(lp: LengthPercentage, basis: f32) -> f32 {
    match lp {
        LengthPercentage::Length(px) => px,
        LengthPercentage::Percentage(p) => p * basis,
        LengthPercentage::Calc { px, percent } => px + percent * basis,
    }
}

/// Adjust `m` to `Translate(ox,oy) * m * Translate(-ox,-oy)` so it can be applied about the
/// `transform-origin` reference point `(ox, oy)`. `m` and `(ox, oy)` must be in the same
/// coordinate system (always PDF coordinates in this project).
fn apply_transform_origin(m: [f32; 6], ox: f32, oy: f32) -> [f32; 6] {
    let [a, b, c, d, e, f] = m;
    [
        a,
        b,
        c,
        d,
        e + ox - a * ox - c * oy,
        f + oy - b * ox - d * oy,
    ]
}

/// The body of [`render_box_with_style`] (decoration through to child recursion). It has to
/// sit inside the `transform` CTM wrapper, hence its separation.
#[allow(clippy::too_many_arguments)]
fn render_box_with_style_inner(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    image_ids: &HashMap<usize, ImageIds>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    alpha_gs_names: &[String],
    opacity_form_ids: &HashMap<NodeId, Ref>,
    pending_forms: &mut Vec<(Ref, Vec<u8>)>,
) {
    // `visibility: hidden` (with `collapse` treated the same). This box's own decoration and
    // content are not drawn, but recursion into the `Blocks`/`Table` children continues (if a
    // descendant overrides it with `visibility: visible`, `render_box` re-evaluates that
    // child's own computed style and redraws it correctly, as the spec requires). For a
    // table it recurses through the ordinary `render_box` as a simplification of the
    // `border-collapse` merged drawing (in the rare case of overriding one cell to `visible`
    // inside a hidden table, borders are not merged with the neighbours).
    if style.visibility.is_hidden() {
        match &b.content {
            LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
                for child in paint_order(children, styles) {
                    render_box(
                        content,
                        child,
                        styles,
                        fonts,
                        settings,
                        remaps,
                        font_resource_names,
                        image_ids,
                        background_images,
                        alpha_gs_names,
                        opacity_form_ids,
                        pending_forms,
                    );
                }
            }
            LaidOutContent::Grid(grid) => {
                for child in grid.rows.iter().flat_map(|row| &row.items) {
                    render_box(
                        content,
                        child,
                        styles,
                        fonts,
                        settings,
                        remaps,
                        font_resource_names,
                        image_ids,
                        background_images,
                        alpha_gs_names,
                        opacity_form_ids,
                        pending_forms,
                    );
                }
            }
            LaidOutContent::Table(table) => {
                if let Some(caption) = &table.caption {
                    render_box(
                        content,
                        caption,
                        styles,
                        fonts,
                        settings,
                        remaps,
                        font_resource_names,
                        image_ids,
                        background_images,
                        alpha_gs_names,
                        opacity_form_ids,
                        pending_forms,
                    );
                }
                for row in &table.rows {
                    for cell in &row.cells {
                        render_box(
                            content,
                            cell,
                            styles,
                            fonts,
                            settings,
                            remaps,
                            font_resource_names,
                            image_ids,
                            background_images,
                            alpha_gs_names,
                            opacity_form_ids,
                            pending_forms,
                        );
                    }
                }
            }
            LaidOutContent::Inline(_) | LaidOutContent::Image(_) => {}
        }
        return;
    }

    // Unlike `<img>`, a `background-image` is decoration rather than the box's content, so
    // the `Ref` and intrinsic size are resolved from the side map keyed on `b.node`.
    let background_image_paint = b
        .node
        .and_then(|n| background_images.get(&n))
        .and_then(|image| {
            image_ids
                .get(&(Rc::as_ptr(image) as usize))
                .map(|ids| BackgroundImagePaint {
                    resource: ids.root,
                    intrinsic_width: image.width,
                    intrinsic_height: image.height,
                })
        });

    render_box_decoration(
        content,
        &b.layout,
        style,
        settings,
        background_image_paint,
        alpha_gs_names,
    );
    render_outline(content, &b.layout, style, settings);

    // The marker of a `display: list-item`. It reuses the same `render_line` as an ordinary
    // text line.
    if let Some(marker) = &b.marker {
        render_line(
            content,
            marker,
            fonts,
            settings,
            remaps,
            font_resource_names,
            alpha_gs_names,
        );
    }

    // `overflow: hidden`/`scroll`/`auto` (not distinguished; all clip the same way).
    // The decoration (background, borders, outline, marker) is drawn above and is unaffected
    // by the clip. The clip boundary is always the straight padding box
    // (it never follows `border-radius`).
    if style.overflow.clips() {
        let padding_box = b.layout.padding_box();
        let x = settings.margin.left + padding_box.x;
        let y = to_pdf_y(settings, padding_box.y + padding_box.height);
        content.save_state();
        content.rect(x, y, padding_box.width, padding_box.height);
        content.clip_nonzero();
        content.end_path();
    }

    match &b.content {
        LaidOutContent::Blocks(children) | LaidOutContent::Flex(children) => {
            for child in paint_order(children, styles) {
                render_box(
                    content,
                    child,
                    styles,
                    fonts,
                    settings,
                    remaps,
                    font_resource_names,
                    image_ids,
                    background_images,
                    alpha_gs_names,
                    opacity_form_ids,
                    pending_forms,
                );
            }
        }
        LaidOutContent::Grid(grid) => {
            for child in grid.rows.iter().flat_map(|row| &row.items) {
                render_box(
                    content,
                    child,
                    styles,
                    fonts,
                    settings,
                    remaps,
                    font_resource_names,
                    image_ids,
                    background_images,
                    alpha_gs_names,
                    opacity_form_ids,
                    pending_forms,
                );
            }
        }
        LaidOutContent::Inline(lines) => {
            for line in lines {
                render_line(
                    content,
                    line,
                    fonts,
                    settings,
                    remaps,
                    font_resource_names,
                    alpha_gs_names,
                );
                // A `display: inline-block` within a line goes through the same drawing path
                // as an ordinary block (borders, background and the text inside).
                for atomic in &line.atomics {
                    render_box(
                        content,
                        &atomic.content,
                        styles,
                        fonts,
                        settings,
                        remaps,
                        font_resource_names,
                        image_ids,
                        background_images,
                        alpha_gs_names,
                        opacity_form_ids,
                        pending_forms,
                    );
                }
            }
        }
        LaidOutContent::Image(image) => {
            if let Some(image) = image {
                if let Some(ids) = image_ids.get(&(Rc::as_ptr(image) as usize)) {
                    render_replaced_image(
                        content,
                        b.layout.content,
                        style,
                        settings,
                        image,
                        ids.root,
                    );
                }
            }
        }
        LaidOutContent::Table(table) => {
            if let Some(caption) = &table.caption {
                render_box(
                    content,
                    caption,
                    styles,
                    fonts,
                    settings,
                    remaps,
                    font_resource_names,
                    image_ids,
                    background_images,
                    alpha_gs_names,
                    opacity_form_ids,
                    pending_forms,
                );
            }
            // `border-collapse` applies only to `table`/`inline-table` elements, so the
            // table's own `style` is consulted; but `empty-cells` is a property applying to
            // `table-cell` elements (CSS2.1 17.6.1.1), so the cell's own computed style has to
            // be consulted (to honour a per-cell override).
            // `empty-cells: hide` is meaningful only with `border-collapse: separate` (as the
            // CSS spec says). A cell with empty content draws nothing but decoration anyway,
            // so the `render_box` call itself can be skipped.
            let collapse = style.border_collapse == BorderCollapse::Collapse;
            // Under `border-collapse: collapse`, a flat list of every cell is needed to merge
            // the borders between neighbours
            // (adjacency is decided geometrically, by rectangles touching).
            let all_cells: Vec<&LaidOutBox> = if collapse {
                table.rows.iter().flat_map(|row| &row.cells).collect()
            } else {
                Vec::new()
            };
            for row in &table.rows {
                render_row_background(content, row, styles, settings, alpha_gs_names);
                for cell in &row.cells {
                    let cell_style = cell
                        .node
                        .and_then(|n| styles.get(&n))
                        .cloned()
                        .unwrap_or_default();
                    let hide_this_cell = !collapse
                        && cell_style.empty_cells == EmptyCells::Hide
                        && laid_content_is_empty(&cell.content);
                    if hide_this_cell {
                        continue;
                    }
                    if collapse {
                        let (resolved_style, resolved_border) =
                            resolve_collapsed_cell_style(cell, &cell_style, &all_cells, styles);
                        // The drawn border thickness comes from `layout.border` (computed
                        // when layout settled) rather than `ComputedStyle`
                        // (see [`render_border`]), so a clone reflecting the merged
                        // thickness is made and drawn.
                        let mut resolved_cell = cell.clone();
                        resolved_cell.layout.border = resolved_border;
                        render_box_with_style(
                            content,
                            &resolved_cell,
                            &resolved_style,
                            styles,
                            fonts,
                            settings,
                            remaps,
                            font_resource_names,
                            image_ids,
                            background_images,
                            alpha_gs_names,
                            opacity_form_ids,
                            pending_forms,
                        );
                    } else {
                        render_box(
                            content,
                            cell,
                            styles,
                            fonts,
                            settings,
                            remaps,
                            font_resource_names,
                            image_ids,
                            background_images,
                            alpha_gs_names,
                            opacity_form_ids,
                            pending_forms,
                        );
                    }
                }
            }
        }
    }

    if style.overflow.clips() {
        content.restore_state();
    }
}

/// Reorder `children` into drawing order according to `z-index` and float
/// (a stable sort on `(z-index, is it a float, document order)`). `z-index` has no effect on
/// a `position: static` element (as the spec says), so its effective value is always `0`.
/// `sort_by_key` is stable, so elements with the same key keep their document order.
/// Separate stacking contexts are not supported (this controls only the drawing order among
/// siblings with the same direct parent).
///
/// Drawing floats after (above) normal-flow blocks of the same `z-index` follows CSS2.1
/// Appendix E (a block's background and borders are in a layer below floats). Without it, a
/// block with a background colour immediately after a float would paint over that float.
fn paint_order<'a>(
    children: &'a [LaidOutBox],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
) -> Vec<&'a LaidOutBox> {
    let effective_z_index = |child: &LaidOutBox| -> i32 {
        let Some(style) = child.node.and_then(|n| styles.get(&n)) else {
            return 0;
        };
        if style.position == Position::Relative {
            style.z_index.sort_key()
        } else {
            0
        }
    };
    let mut order: Vec<&LaidOutBox> = children.iter().collect();
    order.sort_by_key(|child| (effective_z_index(child), u8::from(child.is_float)));
    order
}

/// Whether a cell's content is empty (whitespace-only text, or no children). Used for the
/// `empty-cells: hide` decision. A nested table and a replaced element (`<img>`) always count
/// as non-empty (being meaningful as content).
fn laid_content_is_empty(content: &LaidOutContent) -> bool {
    match content {
        // `<td>&nbsp;</td>` is not "an empty cell" (it is the classic way to force a frame).
        // `str::trim` would drop `&nbsp;` too, so the CSS classification decides.
        LaidOutContent::Inline(lines) => lines.iter().all(|line| {
            line.runs
                .iter()
                .all(|run| crate::layout::is_collapsible_only(&run.text))
        }),
        LaidOutContent::Blocks(children) => {
            children.is_empty() || children.iter().all(|c| laid_content_is_empty(&c.content))
        }
        LaidOutContent::Grid(grid) => grid
            .rows
            .iter()
            .flat_map(|row| &row.items)
            .all(|item| laid_content_is_empty(&item.content)),
        LaidOutContent::Table(_) | LaidOutContent::Flex(_) | LaidOutContent::Image(_) => false,
    }
}

/// Under `border-collapse: collapse`, build the `ComputedStyle` with `cell`'s borders merged
/// with its neighbours, plus the thicknesses actually drawn (an `EdgeSizes` replacing
/// `layout.border`). Everything but the borders stays as in `cell_style`.
///
/// Layout itself is identical to the separate model regardless of `border-collapse`, so
/// under collapse `h_spacing`/`v_spacing` become 0 and neighbouring cells' rectangles touch
/// at matching coordinates. That lets a neighbour be found purely by deciding geometrically
/// whether the rectangles touch, without keeping separate rowspan/colspan grid
/// information.
///
/// To keep the same boundary from being drawn twice, from both sides, the direction is
/// always "if a left neighbour is found, do not draw my left edge (the right-hand side draws
/// the merged border as its right edge)" (and likewise for top and bottom). The boundary
/// between a cell and the table itself is out of scope (the table's own borders are drawn as
/// a band outside the border box and do not overlap a cell's rectangle, so no double drawing
/// arises). Where several neighbours touch one edge (a rowspan, say), the first found is used.
///
/// The thickness actually drawn comes from `layout.border` (an `EdgeSizes` computed when
/// layout settled) rather than `ComputedStyle` (see [`render_border`]), so as well as the
/// merged `ComputedStyle` the corresponding `EdgeSizes` is returned too (normalised the same
/// way as `layout::resolve_border`), for the caller to substitute into `cell.layout.border`.
fn resolve_collapsed_cell_style(
    cell: &LaidOutBox,
    cell_style: &ComputedStyle,
    all_cells: &[&LaidOutBox],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
) -> (ComputedStyle, EdgeSizes) {
    // The tolerance allowed when deciding whether rectangles touch (guarding against floating-point rounding).
    const EPSILON: f32 = 0.5;

    fn ranges_overlap(a_start: f32, a_end: f32, b_start: f32, b_end: f32) -> bool {
        a_start < b_end - EPSILON && b_start < a_end - EPSILON
    }

    let rect = cell.layout.border_box();
    let mut resolved = cell_style.clone();

    let has_left_neighbor = all_cells.iter().any(|other| {
        let o = other.layout.border_box();
        (o.x + o.width - rect.x).abs() < EPSILON
            && ranges_overlap(rect.y, rect.y + rect.height, o.y, o.y + o.height)
    });
    if has_left_neighbor {
        resolved.border_left_style = BorderStyle::None;
        resolved.border_left_width = Length(0.0);
    }

    let has_top_neighbor = all_cells.iter().any(|other| {
        let o = other.layout.border_box();
        (o.y + o.height - rect.y).abs() < EPSILON
            && ranges_overlap(rect.x, rect.x + rect.width, o.x, o.x + o.width)
    });
    if has_top_neighbor {
        resolved.border_top_style = BorderStyle::None;
        resolved.border_top_width = Length(0.0);
    }

    if let Some(right_neighbor) = all_cells.iter().find(|other| {
        let o = other.layout.border_box();
        (o.x - (rect.x + rect.width)).abs() < EPSILON
            && ranges_overlap(rect.y, rect.y + rect.height, o.y, o.y + o.height)
    }) {
        let neighbor_style = right_neighbor
            .node
            .and_then(|n| styles.get(&n))
            .cloned()
            .unwrap_or_default();
        let own = border_edge(
            cell_style.border_right_width.0,
            cell_style.border_right_style,
            cell_style.border_right_color,
        );
        let theirs = border_edge(
            neighbor_style.border_left_width.0,
            neighbor_style.border_left_style,
            neighbor_style.border_left_color,
        );
        let (width, style, color) = resolve_border_conflict(own, theirs);
        resolved.border_right_width = Length(width);
        resolved.border_right_style = style;
        resolved.border_right_color = color;
    }

    if let Some(bottom_neighbor) = all_cells.iter().find(|other| {
        let o = other.layout.border_box();
        (o.y - (rect.y + rect.height)).abs() < EPSILON
            && ranges_overlap(rect.x, rect.x + rect.width, o.x, o.x + o.width)
    }) {
        let neighbor_style = bottom_neighbor
            .node
            .and_then(|n| styles.get(&n))
            .cloned()
            .unwrap_or_default();
        let own = border_edge(
            cell_style.border_bottom_width.0,
            cell_style.border_bottom_style,
            cell_style.border_bottom_color,
        );
        let theirs = border_edge(
            neighbor_style.border_top_width.0,
            neighbor_style.border_top_style,
            neighbor_style.border_top_color,
        );
        let (width, style, color) = resolve_border_conflict(own, theirs);
        resolved.border_bottom_width = Length(width);
        resolved.border_bottom_style = style;
        resolved.border_bottom_color = color;
    }

    let border = resolve_border(&resolved);
    (resolved, border)
}

/// An edge with `style: none` counts as an effective width of 0 regardless of the width set
/// (the same normalisation as `layout::resolve_border`, to keep the width comparison in
/// `resolve_border_conflict` simple).
fn border_edge(width: f32, style: BorderStyle, color: RgbaColor) -> (f32, BorderStyle, RgbaColor) {
    if style == BorderStyle::None {
        (0.0, BorderStyle::None, color)
    } else {
        (width, style, color)
    }
}

/// A simplified version of CSS2.1 section 17.6.2's border conflict resolution: the thicker
/// width wins, and at equal widths the style priority decides (in the spec's order of
/// apparent strength: double > solid > dashed > dotted > ridge > outset > groove > inset >
/// none). `hidden` is absent from [`BorderStyle`] and is unsupported. On a tie in both width and style, `a` is taken
fn resolve_border_conflict(
    a: (f32, BorderStyle, RgbaColor),
    b: (f32, BorderStyle, RgbaColor),
) -> (f32, BorderStyle, RgbaColor) {
    if a.0 != b.0 {
        return if a.0 > b.0 { a } else { b };
    }
    fn style_priority(s: BorderStyle) -> u8 {
        match s {
            BorderStyle::Double => 8,
            BorderStyle::Solid => 7,
            BorderStyle::Dashed => 6,
            BorderStyle::Dotted => 5,
            BorderStyle::Ridge => 4,
            BorderStyle::Outset => 3,
            BorderStyle::Groove => 2,
            BorderStyle::Inset => 1,
            BorderStyle::None => 0,
        }
    }
    if style_priority(a.1) != style_priority(b.1) {
        return if style_priority(a.1) > style_priority(b.1) {
            a
        } else {
            b
        };
    }
    a
}

/// Draw the background and borders. With no `border-radius` set, it draws straight
/// rectangles and four independently stroked edges as before; with one set, it delegates to
/// [`render_rounded_decoration`]. `background_image_ref` is the XObject Ref of a background
/// image stretched to fill the border box (clipping by `border-radius` is not supported).
/// The order is background colour, then background image, then borders.
#[allow(clippy::too_many_arguments)]
fn render_box_decoration(
    content: &mut RenderTarget<'_>,
    layout: &Layout,
    style: &ComputedStyle,
    settings: &PageSettings,
    background_image_paint: Option<BackgroundImagePaint>,
    alpha_gs_names: &[String],
) {
    let radii = effective_radii(layout, style);
    let has_radius = [radii.0, radii.1, radii.2, radii.3]
        .into_iter()
        .any(|(rx, ry)| rx > 0.0 || ry > 0.0);

    render_box_shadows(content, layout, style, settings, radii, alpha_gs_names);

    if has_radius {
        render_rounded_decoration(
            content,
            layout,
            style,
            settings,
            radii,
            background_image_paint,
            alpha_gs_names,
        );
        return;
    }

    if style.background_color.alpha > 0.0 {
        render_background(
            content,
            layout.border_box(),
            style.background_color,
            settings,
            alpha_gs_names,
        );
    }
    if let Some(paint) = background_image_paint {
        render_background_image(content, layout.border_box(), style, settings, &paint);
    }
    render_border(content, layout, style, settings);
}

/// The number of steps in the blur approximation.
const BOX_SHADOW_BLUR_STEPS: u32 = 4;

/// Draw the `box-shadow`s (call before the element's own background and borders). They are
/// painted back to front so the first in the list ends up frontmost. `inset` is not supported.
fn render_box_shadows(
    content: &mut RenderTarget<'_>,
    layout: &Layout,
    style: &ComputedStyle,
    settings: &PageSettings,
    radii: (
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
    ),
    alpha_gs_names: &[String],
) {
    if style.box_shadow.is_empty() {
        return;
    }
    let border_box = layout.border_box();
    for shadow in style.box_shadow.iter().rev() {
        if shadow.inset {
            continue;
        }
        render_single_box_shadow(content, border_box, shadow, settings, radii, alpha_gs_names);
    }
}

/// Draw one shadow. The blur is not a true Gaussian: it is approximated by overpainting
/// concentric semi-transparent rectangles outside a core rectangle expanded by
/// `spread-radius`, spreading out to `blur-radius` in `BOX_SHADOW_BLUR_STEPS` even steps,
/// from the outermost (widest and faintest) to the innermost (closest to the core and
/// strongest). The corner radii use the element's own (`radii`) unchanged, without growing.
fn render_single_box_shadow(
    content: &mut RenderTarget<'_>,
    border_box: Rect,
    shadow: &ComputedBoxShadow,
    settings: &PageSettings,
    radii: (
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
    ),
    alpha_gs_names: &[String],
) {
    if shadow.color.alpha <= 0.0 {
        return;
    }

    let draw = |content: &mut RenderTarget<'_>, expand: f32, alpha: f32| {
        let x0 = settings.margin.left + border_box.x + shadow.offset_x - expand;
        let x1 = settings.margin.left + border_box.x + border_box.width + shadow.offset_x + expand;
        let y_top = to_pdf_y(settings, border_box.y + shadow.offset_y - expand);
        let y_bottom = to_pdf_y(
            settings,
            border_box.y + border_box.height + shadow.offset_y + expand,
        );
        // A known simplification: where a negative `spread-radius` degenerates the rectangle,
        // that ring is not drawn (a zero or negative sized rectangle being meaningless).
        if x1 <= x0 || y_top <= y_bottom {
            return;
        }
        let use_alpha = alpha < 1.0;
        if use_alpha {
            content.save_state();
            apply_fill_alpha(content, alpha, alpha_gs_names);
        }
        content.set_fill_rgb(
            shadow.color.red as f32 / 255.0,
            shadow.color.green as f32 / 255.0,
            shadow.color.blue as f32 / 255.0,
        );
        rounded_rect_path(content, x0, y_top, x1, y_bottom, radii);
        content.fill_nonzero();
        if use_alpha {
            content.restore_state();
        }
    };

    if shadow.blur_radius <= 0.0 {
        draw(content, shadow.spread_radius, shadow.color.alpha);
        return;
    }

    for step in (1..=BOX_SHADOW_BLUR_STEPS).rev() {
        let expand =
            shadow.spread_radius + shadow.blur_radius * step as f32 / BOX_SHADOW_BLUR_STEPS as f32;
        let alpha = shadow.color.alpha * (BOX_SHADOW_BLUR_STEPS + 1 - step) as f32
            / BOX_SHADOW_BLUR_STEPS as f32;
        draw(content, expand, alpha);
    }
    // The core (spread only, at full alpha) is overpainted last, so the outline matches the
    // blur-radius: 0 case exactly.
    draw(content, shadow.spread_radius, shadow.color.alpha);
}

/// The effective radii of one corner as px values (horizontal, vertical). A true circle has the two equal.
type CornerRadiusPx = (f32, f32);

/// Round down the `border-radius` from the style according to where the box sits among the
/// fragments pagination produced ([`FragmentPosition`]). On a continuing fragment
/// (`Middle`, the top of a `Last`, the bottom of a `First`), the radius of any corner
/// touching that edge is set to 0, so an edge that has no border is not rounded.
fn effective_radii(
    layout: &Layout,
    style: &ComputedStyle,
) -> (
    CornerRadiusPx,
    CornerRadiusPx,
    CornerRadiusPx,
    CornerRadiusPx,
) {
    let apply_top = matches!(
        layout.fragment,
        FragmentPosition::Whole | FragmentPosition::First
    );
    let apply_bottom = matches!(
        layout.fragment,
        FragmentPosition::Whole | FragmentPosition::Last
    );
    let px = |r: CornerRadius| (r.horizontal.0, r.vertical.0);
    (
        if apply_top {
            px(style.border_top_left_radius)
        } else {
            (0.0, 0.0)
        },
        if apply_top {
            px(style.border_top_right_radius)
        } else {
            (0.0, 0.0)
        },
        if apply_bottom {
            px(style.border_bottom_right_radius)
        } else {
            (0.0, 0.0)
        },
        if apply_bottom {
            px(style.border_bottom_left_radius)
        } else {
            (0.0, 0.0)
        },
    )
}

/// The number of alpha quantisation steps (21 steps of 0.05).
pub(super) const ALPHA_STEPS: usize = 20;

/// Round an alpha value to a step in `0..=ALPHA_STEPS` (in 0.05 increments).
fn quantize_alpha_step(alpha: f32) -> usize {
    (alpha.clamp(0.0, 1.0) * ALPHA_STEPS as f32).round() as usize
}

/// The fixed resource name (`"GSA{step}"`) for `alpha_gs_names`
/// (which has `ALPHA_STEPS + 1` entries, indexed by step).
pub(super) fn alpha_gs_resource_name(step: usize) -> String {
    format!("GSA{step}")
}

/// Emit the `gs` operator (`/ca` and `/CA`) for an alpha value. 1.0 (fully opaque) emits
/// nothing (being PDF's default state). The caller must enclose the scope with
/// `save_state`/`restore_state`.
fn apply_fill_alpha(content: &mut RenderTarget<'_>, alpha: f32, alpha_gs_names: &[String]) {
    let step = quantize_alpha_step(alpha);
    if step >= ALPHA_STEPS {
        return;
    }
    content.set_parameters(Name(alpha_gs_names[step].as_bytes()));
}

fn render_background(
    content: &mut RenderTarget<'_>,
    border_box: Rect,
    color: RgbaColor,
    settings: &PageSettings,
    alpha_gs_names: &[String],
) {
    let x = settings.margin.left + border_box.x;
    let y = to_pdf_y(settings, border_box.y + border_box.height);
    let use_alpha = color.alpha < 1.0;
    if use_alpha {
        content.save_state();
        apply_fill_alpha(content, color.alpha, alpha_gs_names);
    }
    content.set_fill_rgb(
        color.red as f32 / 255.0,
        color.green as f32 / 255.0,
        color.blue as f32 / 255.0,
    );
    content.rect(x, y, border_box.width, border_box.height);
    content.fill_nonzero();
    if use_alpha {
        content.restore_state();
    }
}

/// Draw an image XObject filling `rect`. A shared helper used both by `<img>` (its content
/// box) and by `background-image` (the rectangle of one tile; see [`background_tile_rects`]).
/// The XObject `resource_ref` points at must already be registered in the page's
/// `/Resources/XObject` dictionary by the caller (the resource name is derived by the same
/// naming rule as [`image_resource_name`]).
fn render_image(
    content: &mut RenderTarget<'_>,
    rect: Rect,
    settings: &PageSettings,
    resource_ref: Ref,
) {
    let x = settings.margin.left + rect.x;
    let y = to_pdf_y(settings, rect.y + rect.height);
    let name = image_resource_name(resource_ref);
    content.save_state();
    content.transform([rect.width, 0.0, 0.0, rect.height, x, y]);
    content.x_object(Name(name.as_bytes()));
    content.restore_state();
}

/// Draw an `<img>` (a replaced element) according to `object-fit`/`object-position`.
/// It always clips to the content box, whatever the `object-fit` value.
fn render_replaced_image(
    content: &mut RenderTarget<'_>,
    content_box: Rect,
    style: &ComputedStyle,
    settings: &PageSettings,
    image: &PreparedImage,
    resource_ref: Ref,
) {
    let rect = object_fit_rect(content_box, style, (image.width, image.height));

    let x = settings.margin.left + content_box.x;
    let y = to_pdf_y(settings, content_box.y + content_box.height);
    content.save_state();
    content.rect(x, y, content_box.width, content_box.height);
    content.clip_nonzero();
    content.end_path();
    render_image(content, rect, settings, resource_ref);
    content.restore_state();
}

/// Compute, from `object-fit`/`object-position`, the rectangle the image should really be
/// drawn in (in content-box-relative coordinates, in layout space). Where the intrinsic size
/// is degenerate it falls back to drawing plainly over the whole content box
/// (the same division-by-zero guard as `background_tile_rects`).
fn object_fit_rect(content_box: Rect, style: &ComputedStyle, intrinsic: (f32, f32)) -> Rect {
    let (iw, ih) = intrinsic;
    if iw <= 0.0 || ih <= 0.0 {
        return content_box;
    }

    let (draw_w, draw_h) = match style.object_fit {
        ObjectFit::Fill => (content_box.width, content_box.height),
        ObjectFit::Cover => {
            let scale = (content_box.width / iw).max(content_box.height / ih);
            (iw * scale, ih * scale)
        }
        ObjectFit::Contain => {
            let scale = (content_box.width / iw).min(content_box.height / ih);
            (iw * scale, ih * scale)
        }
        ObjectFit::None => (iw, ih),
        // As the spec says, the smaller of `none` and `contain`.
        ObjectFit::ScaleDown => {
            if iw <= content_box.width && ih <= content_box.height {
                (iw, ih)
            } else {
                let scale = (content_box.width / iw).min(content_box.height / ih);
                (iw * scale, ih * scale)
            }
        }
    };

    let x = content_box.x
        + resolve_background_position_offset(
            style.object_position.horizontal,
            content_box.width,
            draw_w,
        );
    let y = content_box.y
        + resolve_background_position_offset(
            style.object_position.vertical,
            content_box.height,
            draw_h,
        );

    Rect {
        x,
        y,
        width: draw_w,
        height: draw_h,
    }
}

/// The information needed to draw a `background-image`. `render_box` resolves it from
/// `b.node` through the side map (`background_images`).
#[derive(Debug, Clone, Copy)]
struct BackgroundImagePaint {
    resource: Ref,
    /// The intrinsic size (px). It can be fractional for an SVG (see [`PreparedImage`]).
    intrinsic_width: f32,
    intrinsic_height: f32,
}

/// Compute, from `background-size`/`-position`/`-repeat`, the rectangles of the image tiles
/// that should really be drawn (in border-box-relative coordinates, in layout space). Where
/// the intrinsic size is degenerate (contains a 0) it falls back to drawing one image plainly
/// over the whole border box (a division-by-zero guard).
fn background_tile_rects(
    border_box: Rect,
    style: &ComputedStyle,
    intrinsic: (f32, f32),
) -> Vec<Rect> {
    let (iw, ih) = intrinsic;
    if iw <= 0.0 || ih <= 0.0 {
        return vec![border_box];
    }

    let (draw_w, draw_h) = match style.background_size {
        BackgroundSize::Cover => {
            let scale = (border_box.width / iw).max(border_box.height / ih);
            (iw * scale, ih * scale)
        }
        BackgroundSize::Contain => {
            let scale = (border_box.width / iw).min(border_box.height / ih);
            (iw * scale, ih * scale)
        }
        BackgroundSize::WidthHeight(w, h) => {
            let resolved_w = resolve_background_size_component(w, border_box.width);
            let resolved_h = resolve_background_size_component(h, border_box.height);
            match (resolved_w, resolved_h) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, w * ih / iw),
                (None, Some(h)) => (h * iw / ih, h),
                (None, None) => (iw, ih),
            }
        }
    };
    if draw_w <= 0.0 || draw_h <= 0.0 {
        return Vec::new();
    }

    let origin_x = border_box.x
        + resolve_background_position_offset(
            style.background_position.horizontal,
            border_box.width,
            draw_w,
        );
    let origin_y = border_box.y
        + resolve_background_position_offset(
            style.background_position.vertical,
            border_box.height,
            draw_h,
        );

    let (repeat_x, repeat_y) = match style.background_repeat {
        BackgroundRepeat::Repeat => (true, true),
        BackgroundRepeat::RepeatX => (true, false),
        BackgroundRepeat::RepeatY => (false, true),
        BackgroundRepeat::NoRepeat => (false, false),
    };

    let xs = tile_starts(
        origin_x,
        draw_w,
        border_box.x,
        border_box.x + border_box.width,
        repeat_x,
    );
    let ys = tile_starts(
        origin_y,
        draw_h,
        border_box.y,
        border_box.y + border_box.height,
        repeat_y,
    );

    xs.into_iter()
        .flat_map(|x| {
            ys.iter().map(move |&y| Rect {
                x,
                y,
                width: draw_w,
                height: draw_h,
            })
        })
        .collect()
}

/// Resolve one axis of the `background-size` setting: `None` for `auto` (leaving it to be
/// derived from the aspect ratio), and otherwise a px value relative to `basis` (the corresponding side of the border box).
fn resolve_background_size_component(value: LengthPercentageOrAuto, basis: f32) -> Option<f32> {
    match value {
        LengthPercentageOrAuto::Auto => None,
        LengthPercentageOrAuto::LengthPercentage(lp) => Some(resolve_length_percentage(lp, basis)),
    }
}

/// From the computed value of one axis of `background-position`, find the offset (px) from
/// the border box's origin. A percentage is a proportion of `(container - tile)` (the
/// formula the CSS spec gives); a length is used directly as the offset from the origin.
fn resolve_background_position_offset(value: LengthPercentage, container: f32, tile: f32) -> f32 {
    match value {
        LengthPercentage::Length(l) => l,
        LengthPercentage::Percentage(p) => (container - tile) * p,
        LengthPercentage::Calc { px, percent } => px + (container - tile) * percent,
    }
}

/// Enumerate the tile start coordinates along one axis. With `repeat` false, or a tile width
/// of 0 or less, only the one at `origin`. Otherwise they run from `origin` at intervals of
/// `tile`, as many as are needed to cover `[min, max)` (the border box's extent).
/// Defensively it stops past 200 tiles per axis (a failsafe against a pathologically small `background-size`).
fn tile_starts(origin: f32, tile: f32, min: f32, max: f32, repeat: bool) -> Vec<f32> {
    if !repeat || tile <= 0.0 {
        return vec![origin];
    }
    const MAX_TILES_PER_AXIS: usize = 200;
    let steps_back = ((origin - min) / tile).ceil().max(0.0);
    let first = origin - steps_back * tile;

    let mut starts = Vec::new();
    let mut x = first;
    while x < max && starts.len() < MAX_TILES_PER_AXIS {
        starts.push(x);
        x += tile;
    }
    starts
}

/// Draw the rectangles computed by [`background_tile_rects`]. Unless there is exactly one
/// tile and it coincides with the border box (as with `background-repeat: no-repeat` plus no
/// `background-size`, where it fits anyway), a clip to the border box is interposed (the
/// same pattern as the `overflow` clip) so the tiles cannot spill out of the box.
fn render_background_image(
    content: &mut RenderTarget<'_>,
    border_box: Rect,
    style: &ComputedStyle,
    settings: &PageSettings,
    paint: &BackgroundImagePaint,
) {
    let rects = background_tile_rects(
        border_box,
        style,
        (paint.intrinsic_width, paint.intrinsic_height),
    );
    if rects.is_empty() {
        return;
    }

    let fits_without_clip = rects.len() == 1
        && rects[0].x >= border_box.x
        && rects[0].y >= border_box.y
        && rects[0].x + rects[0].width <= border_box.x + border_box.width
        && rects[0].y + rects[0].height <= border_box.y + border_box.height;

    if !fits_without_clip {
        let x = settings.margin.left + border_box.x;
        let y = to_pdf_y(settings, border_box.y + border_box.height);
        content.save_state();
        content.rect(x, y, border_box.width, border_box.height);
        content.clip_nonzero();
        content.end_path();
    }

    for rect in rects {
        render_image(content, rect, settings, paint.resource);
    }

    if !fits_without_clip {
        content.restore_state();
    }
}

/// Paint a `<tr>`'s (`display: table-row`'s) `background-color` as a rectangle covering that
/// row's cells.
///
/// A row box carries no geometry on `LaidOutTableRow`, so the union of the border boxes of
/// the cells belonging to the row is taken as the row's rectangle (with `border-spacing`, the
/// gaps between cells are painted with the row background too, which looks the same as CSS2.1
/// 17.5.1's drawing order, where the row background sits beneath the cell backgrounds).
/// Both the CSS `tr { background-color: ... }` and the legacy presentational attribute
/// `<tr bgcolor>` are drawn through this path.
fn render_row_background(
    content: &mut RenderTarget<'_>,
    row: &LaidOutTableRow,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    settings: &PageSettings,
    alpha_gs_names: &[String],
) {
    // An anonymous row (`node: None`) cannot have a background set, so nothing is drawn.
    let Some(style) = row.node.and_then(|node| styles.get(&node)) else {
        return;
    };
    if style.background_color.alpha <= 0.0 || row.cells.is_empty() {
        return;
    }

    let mut left = f32::MAX;
    let mut right = f32::MIN;
    let mut top = f32::MAX;
    let mut bottom = f32::MIN;
    for cell in &row.cells {
        let b = cell.layout.border_box();
        left = left.min(b.x);
        right = right.max(b.x + b.width);
        top = top.min(b.y);
        bottom = bottom.max(b.y + b.height);
    }
    if right <= left || bottom <= top {
        return;
    }

    let use_alpha = style.background_color.alpha < 1.0;
    if use_alpha {
        content.save_state();
        apply_fill_alpha(content, style.background_color.alpha, alpha_gs_names);
    }
    content.set_fill_rgb(
        style.background_color.red as f32 / 255.0,
        style.background_color.green as f32 / 255.0,
        style.background_color.blue as f32 / 255.0,
    );
    let x = settings.margin.left + left;
    let y_bottom = to_pdf_y(settings, bottom);
    content.rect(x, y_bottom, right - left, bottom - top);
    content.fill_nonzero();
    if use_alpha {
        content.restore_state();
    }
}

/// Background and border drawing when a `border-radius` is set.
///
/// The background is filled as a rounded rectangle following each corner's radius. The
/// borders are stroked as a rounded path only when all four edges share a width, style and
/// colour (combining rounding with per-edge widths, colours and styles would need complex
/// blending at the corners and is not supported; in that case the rounding is given up and
/// it falls back to the straight four-edge [`render_border`]).
#[allow(clippy::too_many_arguments)]
fn render_rounded_decoration(
    content: &mut RenderTarget<'_>,
    layout: &Layout,
    style: &ComputedStyle,
    settings: &PageSettings,
    radii: (
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
    ),
    background_image_paint: Option<BackgroundImagePaint>,
    alpha_gs_names: &[String],
) {
    let border_box = layout.border_box();
    let x0 = settings.margin.left + border_box.x;
    let x1 = x0 + border_box.width;
    let y_top = to_pdf_y(settings, border_box.y);
    let y_bottom = to_pdf_y(settings, border_box.y + border_box.height);

    if style.background_color.alpha > 0.0 {
        let use_alpha = style.background_color.alpha < 1.0;
        if use_alpha {
            content.save_state();
            apply_fill_alpha(content, style.background_color.alpha, alpha_gs_names);
        }
        content.set_fill_rgb(
            style.background_color.red as f32 / 255.0,
            style.background_color.green as f32 / 255.0,
            style.background_color.blue as f32 / 255.0,
        );
        rounded_rect_path(content, x0, y_top, x1, y_bottom, radii);
        content.fill_nonzero();
        if use_alpha {
            content.restore_state();
        }
    }
    // No clip to the rounded path: it is always drawn as a straight rectangle
    // (combining it with border-radius is not supported).
    if let Some(paint) = background_image_paint {
        render_background_image(content, border_box, style, settings, &paint);
    }

    // groove/ridge/inset/outset need per-edge shading, which a plain stroke of a rounded path
    // cannot express, so they always fall back to four straight edges
    // (the same pattern as the existing "four uneven edges plus rounding" fallback).
    let is_shaded_style = matches!(
        style.border_top_style,
        BorderStyle::Groove | BorderStyle::Ridge | BorderStyle::Inset | BorderStyle::Outset
    );
    if !is_uniform_border(style) || is_shaded_style {
        render_border(content, layout, style, settings);
        return;
    }

    let thickness = layout.border.top;
    if thickness <= 0.0 || style.border_top_style == BorderStyle::None {
        return;
    }

    content.set_stroke_rgb(
        style.border_top_color.red as f32 / 255.0,
        style.border_top_color.green as f32 / 255.0,
        style.border_top_color.blue as f32 / 255.0,
    );

    if style.border_top_style == BorderStyle::Double {
        // The thickness is split into thirds and two rounded paths of one third the width are
        // stroked at 1/6 and 5/6 from the outside (each band's centre line), leaving the middle third blank.
        let band = thickness / 3.0;
        content.set_line_cap(LineCapStyle::ButtCap);
        content.set_dash_pattern([], 0.0);
        content.set_line_width(band);
        for offset in [band / 2.0, thickness - band / 2.0] {
            rounded_rect_path(
                content,
                x0 + offset,
                y_top - offset,
                x1 - offset,
                y_bottom + offset,
                shrink_radii(radii, offset),
            );
            content.stroke();
        }
        return;
    }

    // A stroke runs along the centre line of the width, so the outer path is pulled inwards
    // by half (a simple approximation shrinking the radii by the same amount).
    let inset = thickness / 2.0;
    content.set_line_width(thickness);
    apply_border_style_dash(content, style.border_top_style, thickness);
    rounded_rect_path(
        content,
        x0 + inset,
        y_top - inset,
        x1 - inset,
        y_bottom + inset,
        shrink_radii(radii, inset),
    );
    content.stroke();
}

/// Whether all four edges' `border-width`/`border-style`/`border-color` match.
fn is_uniform_border(style: &ComputedStyle) -> bool {
    style.border_top_width == style.border_right_width
        && style.border_top_width == style.border_bottom_width
        && style.border_top_width == style.border_left_width
        && style.border_top_style == style.border_right_style
        && style.border_top_style == style.border_bottom_style
        && style.border_top_style == style.border_left_style
        && style.border_top_color == style.border_right_color
        && style.border_top_color == style.border_bottom_color
        && style.border_top_color == style.border_left_color
}

/// The control point offset factor for approximating a quarter circle with a Bezier curve.
const BEZIER_KAPPA: f32 = 0.552_284_8;

/// Build and close the path of a rounded rectangle in PDF space (Y-up, `y_top` > `y_bottom`)
/// (filling and stroking are the caller's job). The radii are in the order
/// `(top_left, top_right, bottom_right, bottom_left)` (the same order as CSS's
/// `border-radius`), each corner being a `(horizontal radius, vertical radius)` pair (elliptical corners are supported).
fn rounded_rect_path(
    content: &mut RenderTarget<'_>,
    x0: f32,
    y_top: f32,
    x1: f32,
    y_bottom: f32,
    radii: (
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
    ),
) {
    let max_rx = ((x1 - x0) / 2.0).max(0.0);
    let max_ry = ((y_top - y_bottom) / 2.0).max(0.0);
    let clamp = |(rx, ry): CornerRadiusPx| (rx.clamp(0.0, max_rx), ry.clamp(0.0, max_ry));
    let (tl, tr, br, bl) = radii;
    let (rx_tl, ry_tl) = clamp(tl);
    let (rx_tr, ry_tr) = clamp(tr);
    let (rx_br, ry_br) = clamp(br);
    let (rx_bl, ry_bl) = clamp(bl);

    content.move_to(x0 + rx_tl, y_top);
    content.line_to(x1 - rx_tr, y_top);
    if rx_tr > 0.0 || ry_tr > 0.0 {
        let kx = rx_tr * BEZIER_KAPPA;
        let ky = ry_tr * BEZIER_KAPPA;
        content.cubic_to(
            x1 - rx_tr + kx,
            y_top,
            x1,
            y_top - ry_tr + ky,
            x1,
            y_top - ry_tr,
        );
    }
    content.line_to(x1, y_bottom + ry_br);
    if rx_br > 0.0 || ry_br > 0.0 {
        let kx = rx_br * BEZIER_KAPPA;
        let ky = ry_br * BEZIER_KAPPA;
        content.cubic_to(
            x1,
            y_bottom + ry_br - ky,
            x1 - rx_br + kx,
            y_bottom,
            x1 - rx_br,
            y_bottom,
        );
    }
    content.line_to(x0 + rx_bl, y_bottom);
    if rx_bl > 0.0 || ry_bl > 0.0 {
        let kx = rx_bl * BEZIER_KAPPA;
        let ky = ry_bl * BEZIER_KAPPA;
        content.cubic_to(
            x0 + rx_bl - kx,
            y_bottom,
            x0,
            y_bottom + ry_bl - ky,
            x0,
            y_bottom + ry_bl,
        );
    }
    content.line_to(x0, y_top - ry_tl);
    if rx_tl > 0.0 || ry_tl > 0.0 {
        let kx = rx_tl * BEZIER_KAPPA;
        let ky = ry_tl * BEZIER_KAPPA;
        content.cubic_to(
            x0,
            y_top - ry_tl + ky,
            x0 + rx_tl - kx,
            y_top,
            x0 + rx_tl,
            y_top,
        );
    }
    content.close_path();
}

fn shrink_radii(
    radii: (
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
        CornerRadiusPx,
    ),
    inset: f32,
) -> (
    CornerRadiusPx,
    CornerRadiusPx,
    CornerRadiusPx,
    CornerRadiusPx,
) {
    let shrink = |(rx, ry): CornerRadiusPx| ((rx - inset).max(0.0), (ry - inset).max(0.0));
    (
        shrink(radii.0),
        shrink(radii.1),
        shrink(radii.2),
        shrink(radii.3),
    )
}

/// Draw the `outline`. Unlike `border` it has no effect on layout at all, so `layout` is only
/// read and never modified. The only difference from `render_border` is that it draws outside
/// the border box, at the outline-width thickness; everything else (how the four edges'
/// vertices are built, delegating to `render_border_side`) reuses exactly the same machinery.
/// `outline-offset` (the gap between the outline and the border box) is not supported and is always 0.
fn render_outline(
    content: &mut RenderTarget<'_>,
    layout: &Layout,
    style: &ComputedStyle,
    settings: &PageSettings,
) {
    let t = style.outline_width.0;
    if t <= 0.0 || style.outline_style == BorderStyle::None {
        return;
    }
    let border_box = layout.border_box();
    let x0 = settings.margin.left + border_box.x;
    let x1 = x0 + border_box.width;
    let y_top = to_pdf_y(settings, border_box.y);
    let y_bottom = to_pdf_y(settings, border_box.y + border_box.height);

    // The outline's inner edge (the border box itself).
    let tl_inner = (x0, y_top);
    let tr_inner = (x1, y_top);
    let br_inner = (x1, y_bottom);
    let bl_inner = (x0, y_bottom);
    // The outline's outer edge (extending `t` beyond the border box).
    let tl_outer = (x0 - t, y_top + t);
    let tr_outer = (x1 + t, y_top + t);
    let br_outer = (x1 + t, y_bottom - t);
    let bl_outer = (x0 - t, y_bottom - t);

    render_border_side(
        content,
        BorderSideKind::Top,
        style.outline_style,
        style.outline_color,
        t,
        BorderSideCorners::new(tl_outer, tr_outer, tr_inner, tl_inner),
    );
    render_border_side(
        content,
        BorderSideKind::Right,
        style.outline_style,
        style.outline_color,
        t,
        BorderSideCorners::new(tr_outer, br_outer, br_inner, tr_inner),
    );
    render_border_side(
        content,
        BorderSideKind::Bottom,
        style.outline_style,
        style.outline_color,
        t,
        BorderSideCorners::new(br_outer, bl_outer, bl_inner, br_inner),
    );
    render_border_side(
        content,
        BorderSideKind::Left,
        style.outline_style,
        style.outline_color,
        t,
        BorderSideCorners::new(bl_outer, tl_outer, tl_inner, bl_inner),
    );
}

/// Draw the borders according to each of the four edges' `border-width`/`border-style`/`border-color`.
fn render_border(
    content: &mut RenderTarget<'_>,
    layout: &Layout,
    style: &ComputedStyle,
    settings: &PageSettings,
) {
    let border_box = layout.border_box();
    let x0 = settings.margin.left + border_box.x;
    let x1 = x0 + border_box.width;
    let y_top = to_pdf_y(settings, border_box.y);
    let y_bottom = to_pdf_y(settings, border_box.y + border_box.height);
    let t = layout.border;

    let tl_outer = (x0, y_top);
    let tr_outer = (x1, y_top);
    let br_outer = (x1, y_bottom);
    let bl_outer = (x0, y_bottom);
    let tl_inner = (x0 + t.left, y_top - t.top);
    let tr_inner = (x1 - t.right, y_top - t.top);
    let br_inner = (x1 - t.right, y_bottom + t.bottom);
    let bl_inner = (x0 + t.left, y_bottom + t.bottom);

    render_border_side(
        content,
        BorderSideKind::Top,
        style.border_top_style,
        style.border_top_color,
        t.top,
        BorderSideCorners::new(tl_outer, tr_outer, tr_inner, tl_inner),
    );
    render_border_side(
        content,
        BorderSideKind::Right,
        style.border_right_style,
        style.border_right_color,
        t.right,
        BorderSideCorners::new(tr_outer, br_outer, br_inner, tr_inner),
    );
    render_border_side(
        content,
        BorderSideKind::Bottom,
        style.border_bottom_style,
        style.border_bottom_color,
        t.bottom,
        BorderSideCorners::new(br_outer, bl_outer, bl_inner, br_inner),
    );
    render_border_side(
        content,
        BorderSideKind::Left,
        style.border_left_style,
        style.border_left_color,
        t.left,
        BorderSideCorners::new(bl_outer, tl_outer, tl_inner, bl_inner),
    );
}

/// The identifier of an edge. Needed because the shading of `groove`/`ridge`/`inset`/`outset`
/// differs in colour between the top and left edges and the bottom and right ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderSideKind {
    Top,
    Right,
    Bottom,
    Left,
}

/// Lighten by blending each RGB component `amount` towards white (a simple implementation;
/// accurate colour reproduction is not the goal).
fn lighten(color: RgbaColor, amount: f32) -> RgbaColor {
    let mix = |c: u8| (c as f32 + (255.0 - c as f32) * amount).round() as u8;
    RgbaColor {
        red: mix(color.red),
        green: mix(color.green),
        blue: mix(color.blue),
        alpha: color.alpha,
    }
}

/// Darken by blending each RGB component `amount` towards black.
fn darken(color: RgbaColor, amount: f32) -> RgbaColor {
    let mix = |c: u8| (c as f32 * (1.0 - amount)).round() as u8;
    RgbaColor {
        red: mix(color.red),
        green: mix(color.green),
        blue: mix(color.blue),
        alpha: color.alpha,
    }
}

/// The light/dark blending ratio for `groove`/`ridge`/`inset`/`outset`.
const SHADE_AMOUNT: f32 = 0.35;

/// The effective drawing colours of one edge's outer and inner bands. `solid`/`dashed`/
/// `dotted` (and `double`, handled separately by the caller) use the same colour for both.
struct BorderSideColors {
    outer: RgbaColor,
    inner: RgbaColor,
}

/// Decide the effective drawing colours from `border_style` and `side`. The light source is
/// assumed to be at the top left (the usual convention in the CSS spec): `inset` makes the
/// top and left edges dark and the bottom and right ones light (a pressed-in hollow), and
/// `outset` the reverse (a raised bump). `groove`/`ridge` halve each edge's thickness and
/// assign different colours to the outer and inner bands, producing the groove or ridge effect.
fn border_side_colors(
    border_style: BorderStyle,
    side: BorderSideKind,
    color: RgbaColor,
) -> BorderSideColors {
    let light = lighten(color, SHADE_AMOUNT);
    let dark = darken(color, SHADE_AMOUNT);
    let is_top_or_left = matches!(side, BorderSideKind::Top | BorderSideKind::Left);

    match border_style {
        BorderStyle::Inset => {
            let c = if is_top_or_left { dark } else { light };
            BorderSideColors { outer: c, inner: c }
        }
        BorderStyle::Outset => {
            let c = if is_top_or_left { light } else { dark };
            BorderSideColors { outer: c, inner: c }
        }
        BorderStyle::Groove => {
            if is_top_or_left {
                BorderSideColors {
                    outer: dark,
                    inner: light,
                }
            } else {
                BorderSideColors {
                    outer: light,
                    inner: dark,
                }
            }
        }
        BorderStyle::Ridge => {
            if is_top_or_left {
                BorderSideColors {
                    outer: light,
                    inner: dark,
                }
            } else {
                BorderSideColors {
                    outer: dark,
                    inner: light,
                }
            }
        }
        _ => BorderSideColors {
            outer: color,
            inner: color,
        },
    }
}

/// The four vertices making up one edge's border. `outer_a` to `outer_b` is the outer edge
/// and `inner_b` to `inner_a` the inner one (`outer_b`/`inner_b` being the corner shared with the next edge).
struct BorderSideCorners {
    outer_a: (f32, f32),
    outer_b: (f32, f32),
    inner_b: (f32, f32),
    inner_a: (f32, f32),
}

impl BorderSideCorners {
    fn new(
        outer_a: (f32, f32),
        outer_b: (f32, f32),
        inner_b: (f32, f32),
        inner_a: (f32, f32),
    ) -> Self {
        Self {
            outer_a,
            outer_b,
            inner_b,
            inner_a,
        }
    }
}

/// Draw one edge's border.
fn render_border_side(
    content: &mut RenderTarget<'_>,
    side: BorderSideKind,
    border_style: BorderStyle,
    color: RgbaColor,
    thickness: f32,
    corners: BorderSideCorners,
) {
    if thickness <= 0.0 || border_style == BorderStyle::None {
        return;
    }
    let BorderSideCorners {
        outer_a,
        outer_b,
        inner_b,
        inner_a,
    } = corners;

    match border_style {
        BorderStyle::Solid => {
            content.set_fill_rgb(
                color.red as f32 / 255.0,
                color.green as f32 / 255.0,
                color.blue as f32 / 255.0,
            );
            fill_quad(content, outer_a, outer_b, inner_b, inner_a);
        }
        BorderStyle::Groove | BorderStyle::Ridge | BorderStyle::Inset | BorderStyle::Outset => {
            // Halve the thickness and paint the outer and inner bands in the colours
            // `border_side_colors` decided (`inset`/`outset` give the same colour for both,
            // which ends up looking like a single-colour `Solid`).
            let colors = border_side_colors(border_style, side, color);
            for (t0, t1, band_color) in [(0.0, 0.5, colors.outer), (0.5, 1.0, colors.inner)] {
                content.set_fill_rgb(
                    band_color.red as f32 / 255.0,
                    band_color.green as f32 / 255.0,
                    band_color.blue as f32 / 255.0,
                );
                fill_quad(
                    content,
                    lerp(outer_a, inner_a, t0),
                    lerp(outer_b, inner_b, t0),
                    lerp(outer_b, inner_b, t1),
                    lerp(outer_a, inner_a, t1),
                );
            }
        }
        BorderStyle::Double => {
            // Split the thickness into thirds and paint the outer and inner thirds as mitred
            // bands, leaving the middle third blank. Each band's boundary comes from linearly
            // interpolating between the outer and inner vertices (so even with edges of
            // differing thickness the boundary with a neighbour still meets cleanly, being computed from the shared corner).
            content.set_fill_rgb(
                color.red as f32 / 255.0,
                color.green as f32 / 255.0,
                color.blue as f32 / 255.0,
            );
            const BAND: f32 = 1.0 / 3.0;
            for (t0, t1) in [(0.0, BAND), (1.0 - BAND, 1.0)] {
                fill_quad(
                    content,
                    lerp(outer_a, inner_a, t0),
                    lerp(outer_b, inner_b, t0),
                    lerp(outer_b, inner_b, t1),
                    lerp(outer_a, inner_a, t1),
                );
            }
        }
        BorderStyle::Dashed | BorderStyle::Dotted => {
            // A dash pattern can only be expressed by a stroke, so the centre line of the
            // thickness is stroked as before (no mitring).
            content.set_stroke_rgb(
                color.red as f32 / 255.0,
                color.green as f32 / 255.0,
                color.blue as f32 / 255.0,
            );
            content.set_line_width(thickness);
            apply_border_style_dash(content, border_style, thickness);
            let from = lerp(outer_a, inner_a, 0.5);
            let to = lerp(outer_b, inner_b, 0.5);
            content.move_to(from.0, from.1);
            content.line_to(to.0, to.1);
            content.stroke();
        }
        BorderStyle::None => {}
    }
}

/// Stroke a plain solid line at a given thickness and colour (for text-decoration underlines
/// and strikethroughs).
fn stroke_line(
    content: &mut RenderTarget<'_>,
    thickness: f32,
    color: RgbaColor,
    from: (f32, f32),
    to: (f32, f32),
) {
    if thickness <= 0.0 {
        return;
    }
    content.set_stroke_rgb(
        color.red as f32 / 255.0,
        color.green as f32 / 255.0,
        color.blue as f32 / 255.0,
    );
    content.set_line_width(thickness);
    content.set_line_cap(LineCapStyle::ButtCap);
    content.set_dash_pattern([], 0.0);
    content.move_to(from.0, from.1);
    content.line_to(to.0, to.1);
    content.stroke();
}

fn lerp(a: (f32, f32), b: (f32, f32), t: f32) -> (f32, f32) {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// Build and fill the quadrilateral path of four vertices (a to b to c to d, then closed).
fn fill_quad(
    content: &mut RenderTarget<'_>,
    a: (f32, f32),
    b: (f32, f32),
    c: (f32, f32),
    d: (f32, f32),
) {
    content.move_to(a.0, a.1);
    content.line_to(b.0, b.1);
    content.line_to(c.0, c.1);
    content.line_to(d.0, d.1);
    content.close_path();
    content.fill_nonzero();
}

/// Set the dash pattern and line cap for a `border-style`.
/// `Double` never reaches here, being handled by the caller's dedicated two-stroke path.
/// `Groove`/`Ridge`/`Inset`/`Outset` never reach here either, a stroke of a rounded path
/// being unable to express them and always falling back to four straight edges, but they are
/// treated like `Solid` so the `match` stays exhaustive.
fn apply_border_style_dash(
    content: &mut RenderTarget<'_>,
    border_style: BorderStyle,
    thickness: f32,
) {
    match border_style {
        BorderStyle::Solid
        | BorderStyle::Double
        | BorderStyle::Groove
        | BorderStyle::Ridge
        | BorderStyle::Inset
        | BorderStyle::Outset => {
            content.set_line_cap(LineCapStyle::ButtCap);
            content.set_dash_pattern([], 0.0);
        }
        BorderStyle::Dashed => {
            content.set_line_cap(LineCapStyle::ButtCap);
            content.set_dash_pattern([thickness * 3.0], 0.0);
        }
        BorderStyle::Dotted => {
            // A dotted line is expressed as a zero-length dash with round caps (the standard PDF idiom).
            content.set_line_cap(LineCapStyle::RoundCap);
            content.set_dash_pattern([0.01, thickness * 2.0], 0.0);
        }
        BorderStyle::None => {}
    }
}

/// The slant angle for faux italic (a shear transform), 12 degrees. Assuming the embedded
/// font has no real italic glyph shapes, the text matrix is sheared instead.
const ITALIC_SHEAR: f32 = 0.2126; // tan(12°)
/// The stroke width for faux bold (fill plus outline), as a ratio of the font size.
const BOLD_STROKE_RATIO: f32 = 0.03;

/// The lower bound (px) for correcting a glyph advance mismatch. Anything below it only
/// inflates the TJ array without any visible effect, so it is ignored.
const ADVANCE_EPSILON: f32 = 0.01;

/// Write out a run's glyphs. Where an advance disagrees with `/W`, a TJ correction is inserted after the glyph.
///
/// The width by which a PDF advances a glyph comes from the CIDFont's `/W`, which can hold
/// only one value per glyph ID. Layout, meanwhile, uses the `x_advance` the shaper returns.
/// The two need not agree.
///
/// * `merge_adjacent_runs` restores inter-word whitespace as "a space glyph whose advance is
///   the gap". A gap widened by `text-align: justify` does not match the space's own width,
///   so without a correction a justified line falls short of the right edge by the amount it was stretched.
/// * For a fixed-width space the font lacks (`&thinsp;` and the like), the shaper substitutes
///   the space glyph while replacing only the advance with the prescribed value (em/5 and so
///   on). An ordinary space uses the same glyph, so `/W` can express only one of the widths.
///
/// The difference is made up by TJ array corrections. A TJ number is in 1/1000ths of text
/// space and is *subtracted* from the advance (a positive value tightens), so a negative
/// value is used to widen. `letter-spacing` is added separately by `Tc` and is not part of this difference.
fn show_run_glyphs(
    content: &mut RenderTarget<'_>,
    run: &TextRun,
    font: &Font,
    remap: Option<&HashMap<u16, u16>>,
) {
    // With `remaps` as `Some` (batch processing) the subsetted glyph IDs are used; with `None`
    // (streaming) the original glyph IDs stay.
    let cid_of = |glyph_id: u16| match remap {
        Some(remap) => remap.get(&glyph_id).copied().unwrap_or(0),
        None => glyph_id,
    };

    let units_per_em = font.units_per_em() as f32;
    // A run with a font size of 0 cannot be corrected (there is no conversion to 1/1000ths).
    if run.font_size <= 0.0 || units_per_em <= 0.0 {
        let mut glyph_bytes = Vec::with_capacity(run.glyphs.len() * 2);
        for glyph in &run.glyphs {
            glyph_bytes.extend_from_slice(&cid_of(glyph.glyph_id).to_be_bytes());
        }
        content.show(pdf_writer::Str(&glyph_bytes));
        return;
    }

    let mut positioned = content.show_positioned();
    let mut items = positioned.items();
    // Glyphs needing no correction are emitted together as one string (with no corrections at
    // all it becomes a one-element TJ array, no bigger than a `Tj`).
    let mut pending = Vec::with_capacity(run.glyphs.len() * 2);
    for glyph in &run.glyphs {
        pending.extend_from_slice(&cid_of(glyph.glyph_id).to_be_bytes());
        let pdf_advance = font.glyph_hor_advance(glyph.glyph_id).unwrap_or(0) as f32
            * run.font_size
            / units_per_em;
        let delta = glyph.x_advance - pdf_advance;
        if delta.abs() < ADVANCE_EPSILON {
            continue;
        }
        items.show(pdf_writer::Str(&pending));
        pending.clear();
        // Rounded to two decimal places. In a justified document every inter-word gap carries
        // a correction, so writing the `f32` verbatim inflates the content stream by around a
        // tenth. 0.01 of a 1/1000th is 0.00012px at 12pt, which is invisible.
        let adjustment = (-delta * 1000.0 / run.font_size * 100.0).round() / 100.0;
        items.adjust(adjustment);
    }
    if !pending.is_empty() {
        items.show(pdf_writer::Str(&pending));
    }
}

fn render_line(
    content: &mut RenderTarget<'_>,
    line: &LineBox,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    alpha_gs_names: &[String],
) {
    if line.runs.is_empty() {
        return;
    }

    // The line's baseline position is settled at layout time. Each run is offset from it only
    // by the `baseline_shift` from `vertical-align` (positive being up).
    let baseline_y = to_pdf_y(settings, line.rect.y + line.baseline);

    // An inline element's background (`<mark>` and so on) is painted before the text, as the
    // rectangle from the run's ascent to its descent. Unlike a block's background
    // ([`render_decoration`]) it has no border box, so the font metrics stand in.
    for run in &line.runs {
        if run.background_color.alpha <= 0.0 || run.width <= 0.0 {
            continue;
        }
        let run_baseline_y = baseline_y + run.baseline_shift;
        let use_alpha = run.background_color.alpha < 1.0;
        if use_alpha {
            content.save_state();
            apply_fill_alpha(content, run.background_color.alpha, alpha_gs_names);
        }
        content.set_fill_rgb(
            run.background_color.red as f32 / 255.0,
            run.background_color.green as f32 / 255.0,
            run.background_color.blue as f32 / 255.0,
        );
        content.rect(
            settings.margin.left + line.rect.x + run.x_offset,
            run_baseline_y - run.descent,
            run.width,
            run.ascent + run.descent,
        );
        content.fill_nonzero();
        if use_alpha {
            content.restore_state();
        }
    }

    // `text-shadow` is drawn before the text itself.
    render_text_shadows(
        content,
        line,
        fonts,
        settings,
        remaps,
        font_resource_names,
        alpha_gs_names,
        baseline_y,
    );

    content.begin_text();

    // Where the gap between two runs exceeds the sum of the actual glyph widths, it counts as
    // a word boundary (that is, one space). A run boundary from a style or font change within
    // a word continues with a gap of 0, so it is not mistaken for whitespace here.
    const WORD_GAP_EPSILON: f32 = 0.01;
    let mut previous_run_end: Option<f32> = None;

    for run in &line.runs {
        if run.glyphs.is_empty() {
            continue;
        }
        // With `remaps` as `Some` (batch processing) the translation table to subsetted glyph
        // IDs is consulted; with `None` (streaming) a CID is always the original glyph ID.
        let remap = match remaps {
            Some(remaps) => match remaps.get(run.font_index) {
                Some(remap) => Some(remap),
                None => continue,
            },
            None => None,
        };
        let Some(resource_name) = font_resource_names.get(run.font_index) else {
            continue;
        };

        // Inter-word whitespace is expressed in layout only as a gap (an addition to
        // x_offset), and no `TextRun.text` contains an actual whitespace character (to keep
        // glyph width measurement simple with mixed fonts). Left at that, extracting text from
        // the PDF can lose the space, especially at a run boundary where the font (resource
        // name) changes, so a marked-content section with an `ActualText` (which has no visual
        // effect) is inserted to state the space explicitly for extraction.
        if let Some(prev_end) = previous_run_end {
            if run.x_offset > prev_end + WORD_GAP_EPSILON {
                let mut marked = content.begin_marked_content_with_properties(Name(b"Span"));
                marked.properties().actual_text(TextStr(" "));
                marked.finish();
                content.end_marked_content();
            }
        }
        previous_run_end = Some(run.x_offset + run.width);

        let Some(font) = fonts.get(run.font_index) else {
            continue;
        };

        content.set_fill_rgb(
            run.color.red as f32 / 255.0,
            run.color.green as f32 / 255.0,
            run.color.blue as f32 / 255.0,
        );
        if run.bold {
            content.set_stroke_rgb(
                run.color.red as f32 / 255.0,
                run.color.green as f32 / 255.0,
                run.color.blue as f32 / 255.0,
            );
            content.set_line_width(run.font_size * BOLD_STROKE_RATIO);
            // Border drawing may have left a dash pattern or round caps set, so solid lines
            // and butt caps are restored explicitly so the text outline is unaffected.
            content.set_line_cap(LineCapStyle::ButtCap);
            content.set_dash_pattern([], 0.0);
            content.set_text_rendering_mode(TextRenderingMode::FillStroke);
        } else {
            content.set_text_rendering_mode(TextRenderingMode::Fill);
        }

        let x = settings.margin.left + line.rect.x + run.x_offset;
        let shear = if run.italic { ITALIC_SHEAR } else { 0.0 };
        content.set_font(Name(resource_name.as_bytes()), run.font_size);
        content.set_text_matrix([1.0, 0.0, shear, 1.0, x, baseline_y + run.baseline_shift]);
        // `letter-spacing` cannot be reflected in the glyph widths themselves (the font's
        // `/Widths`), so PDF's `Tc` (character spacing) is used. Unlike `Tw` (word spacing) it
        // applies to composite fonts (two-byte CIDs) too. It is set explicitly even at 0, so
        // the previous run's value cannot linger in the graphics state.

        content.set_char_spacing(run.letter_spacing);
        show_run_glyphs(content, run, font, remap);
    }

    content.end_text();

    for run in &line.runs {
        if !run.underline && !run.line_through {
            continue;
        }
        let Some(font) = fonts.get(run.font_index) else {
            continue;
        };
        let x = settings.margin.left + line.rect.x + run.x_offset;
        // A decoration line is also drawn against that run's baseline (the line's baseline
        // plus the `vertical-align` shift).
        let run_baseline_y = baseline_y + run.baseline_shift;
        if run.underline {
            let (y, thickness) =
                decoration_metrics(font, run.font_size, font.underline_metrics(), -0.1);
            stroke_line(
                content,
                thickness,
                run.color,
                (x, run_baseline_y + y),
                (x + run.width, run_baseline_y + y),
            );
        }
        if run.line_through {
            let (y, thickness) =
                decoration_metrics(font, run.font_size, font.strikeout_metrics(), 0.3);
            stroke_line(
                content,
                thickness,
                run.color,
                (x, run_baseline_y + y),
                (x + run.width, run_baseline_y + y),
            );
        }
    }

    // Like the decoration lines, `text-emphasis` marks are drawn after the text itself.
    render_emphasis_marks(
        content,
        line,
        fonts,
        settings,
        remaps,
        font_resource_names,
        baseline_y,
    );
}

/// Draw the `text-emphasis` marks.
/// `dot`/`circle`/`double-circle`/`triangle`/`sesame` are drawn as paths so they do not
/// depend on the font's glyph shapes; only a `<string>` value is drawn as a glyph. One mark
/// is placed per non-whitespace character (`text-emphasis-skip` is not supported).
#[allow(clippy::too_many_arguments)]
fn render_emphasis_marks(
    content: &mut RenderTarget<'_>,
    line: &LineBox,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    baseline_y: f32,
) {
    for run in &line.runs {
        let Some(mark) = &run.emphasis else {
            continue;
        };
        let run_baseline_y = baseline_y + run.baseline_shift;
        // The marks' height is already added to `ascent`/`descent`. The mark goes in the
        // centre of that band.
        let center_y = match mark.position {
            EmphasisPosition::Over => run_baseline_y + run.ascent - mark.size / 2.0,
            EmphasisPosition::Under => run_baseline_y - run.descent + mark.size / 2.0,
        };

        let mut x = settings.margin.left + line.rect.x + run.x_offset;
        for glyph in &run.glyphs {
            let advance = glyph.x_advance + run.letter_spacing;
            // No mark is placed on a whitespace character (the equivalent of the spec's
            // "skip: spaces"). `cluster` is a byte offset within the run's text, but it is
            // looked up with `get` so an out-of-range value cannot panic.
            let ch = run
                .text
                .get(glyph.cluster as usize..)
                .and_then(|rest| rest.chars().next());
            if !ch.is_some_and(|ch| ch.is_whitespace()) {
                render_emphasis_mark(
                    content,
                    mark,
                    x + advance / 2.0,
                    center_y,
                    run,
                    fonts,
                    remaps,
                    font_resource_names,
                );
            }
            x += advance;
        }
    }
}

/// Draw one mark centred on `(center_x, center_y)`.
#[allow(clippy::too_many_arguments)]
fn render_emphasis_mark(
    content: &mut RenderTarget<'_>,
    mark: &EmphasisMark,
    center_x: f32,
    center_y: f32,
    run: &TextRun,
    fonts: &FontCollection,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
) {
    let (r, g, b) = (
        mark.color.red as f32 / 255.0,
        mark.color.green as f32 / 255.0,
        mark.color.blue as f32 / 255.0,
    );

    let (shape, filled) = match &mark.style {
        EmphasisStyle::None => return,
        EmphasisStyle::Shape { shape, filled } => (*shape, *filled),
        EmphasisStyle::String(ch) => {
            render_emphasis_glyph(
                content,
                *ch,
                center_x,
                center_y,
                mark,
                run,
                fonts,
                remaps,
                font_resource_names,
            );
            return;
        }
    };

    content.set_fill_rgb(r, g, b);
    content.set_stroke_rgb(r, g, b);
    // The stroke width of an outline-only (`open`) mark is proportional to the mark size.
    let stroke_width = (mark.size * 0.08).max(0.3);
    content.set_line_width(stroke_width);
    content.set_line_cap(LineCapStyle::ButtCap);
    content.set_dash_pattern([], 0.0);

    match shape {
        // `dot` is on the small side and `circle` on the large side (the spec prescribes no
        // exact dimensions, so ratios close to what browsers commonly show are used).
        EmphasisShape::Dot => {
            circle_path(content, center_x, center_y, mark.size * 0.16);
            finish_mark_path(content, filled);
        }
        EmphasisShape::Circle => {
            circle_path(content, center_x, center_y, mark.size * 0.3);
            finish_mark_path(content, filled);
        }
        EmphasisShape::DoubleCircle => {
            // A double circle always draws its outer ring as an outline. Filling the outer
            // ring would crush the inner one and it would look like a plain circle.
            circle_path(content, center_x, center_y, mark.size * 0.34);
            finish_mark_path(content, false);
            circle_path(content, center_x, center_y, mark.size * 0.15);
            finish_mark_path(content, filled);
        }
        EmphasisShape::Triangle => {
            let s = mark.size * 0.34;
            content.move_to(center_x, center_y + s);
            content.line_to(center_x + s, center_y - s);
            content.line_to(center_x - s, center_y - s);
            content.close_path();
            finish_mark_path(content, filled);
        }
        // `sesame` (a sesame dot) is approximated with a vertically elongated ellipse.
        EmphasisShape::Sesame => {
            ellipse_path(
                content,
                center_x,
                center_y,
                mark.size * 0.12,
                mark.size * 0.3,
            );
            finish_mark_path(content, filled);
        }
    }
}

/// Draw a `text-emphasis-style: <string>` mark as a glyph of that run's font.
/// Nothing is drawn in a font lacking that glyph shape.
#[allow(clippy::too_many_arguments)]
fn render_emphasis_glyph(
    content: &mut RenderTarget<'_>,
    ch: char,
    center_x: f32,
    center_y: f32,
    mark: &EmphasisMark,
    run: &TextRun,
    fonts: &FontCollection,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
) {
    let Some(resource_name) = font_resource_names.get(run.font_index) else {
        return;
    };
    let Some(glyph_id) = fonts.get(run.font_index).and_then(|font| font.glyph_id(ch)) else {
        return;
    };
    let cid = match remaps {
        Some(remaps) => match remaps.get(run.font_index) {
            Some(remap) => remap.get(&glyph_id).copied().unwrap_or(0),
            None => return,
        },
        None => glyph_id,
    };
    if cid == 0 {
        return;
    }

    content.begin_text();
    content.set_fill_rgb(
        mark.color.red as f32 / 255.0,
        mark.color.green as f32 / 255.0,
        mark.color.blue as f32 / 255.0,
    );
    content.set_text_rendering_mode(TextRenderingMode::Fill);
    content.set_font(Name(resource_name.as_bytes()), mark.size);
    content.set_char_spacing(0.0);
    // Treating the mark size as 1em, shift it down and left so it ends up centred.
    content.set_text_matrix([
        1.0,
        0.0,
        0.0,
        1.0,
        center_x - mark.size / 2.0,
        center_y - mark.size / 2.0,
    ]);
    content.show(pdf_writer::Str(&cid.to_be_bytes()));
    content.end_text();
}

/// Fill the mark's path (`filled`) or stroke its outline (`open`).
fn finish_mark_path(content: &mut RenderTarget<'_>, filled: bool) {
    if filled {
        content.fill_nonzero();
    } else {
        content.stroke();
    }
}

/// Build the path of a true circle from a centre and a radius (approximated with four Bezier curves).
fn circle_path(content: &mut RenderTarget<'_>, cx: f32, cy: f32, r: f32) {
    ellipse_path(content, cx, cy, r, r);
}

/// Build the path of an ellipse from a centre and horizontal/vertical radii.
fn ellipse_path(content: &mut RenderTarget<'_>, cx: f32, cy: f32, rx: f32, ry: f32) {
    let (kx, ky) = (rx * BEZIER_KAPPA, ry * BEZIER_KAPPA);
    content.move_to(cx + rx, cy);
    content.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    content.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    content.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    content.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    content.close_path();
}

/// The number of steps in the `text-shadow` blur approximation. The centre plus this many steps in each of four directions are overpainted.
const TEXT_SHADOW_BLUR_STEPS: usize = 2;

/// Draw the `text-shadow`s (call before the text itself). PDF has no blur filter, so it is
/// approximated by overpainting the same glyph run at reduced alpha with tiny offsets.
/// With comma-separated multiples, the later one is drawn further back.
#[allow(clippy::too_many_arguments)]
fn render_text_shadows(
    content: &mut RenderTarget<'_>,
    line: &LineBox,
    fonts: &FontCollection,
    settings: &PageSettings,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
    alpha_gs_names: &[String],
    baseline_y: f32,
) {
    for run in &line.runs {
        let Some(shadows) = run.text_shadow.as_deref() else {
            continue;
        };
        if shadows.is_empty() || run.glyphs.is_empty() {
            continue;
        }
        let remap = match remaps {
            Some(remaps) => match remaps.get(run.font_index) {
                Some(remap) => Some(remap),
                None => continue,
            },
            None => None,
        };
        let Some(resource_name) = font_resource_names.get(run.font_index) else {
            continue;
        };
        // A shadow is the same glyph run as the text itself, so its advance corrections have to match or it will shift.
        let Some(font) = fonts.get(run.font_index) else {
            continue;
        };

        let x = settings.margin.left + line.rect.x + run.x_offset;
        let run_baseline_y = baseline_y + run.baseline_shift;
        let shear = if run.italic { ITALIC_SHEAR } else { 0.0 };

        // The first is frontmost, so the later one is further back. They are drawn back to front.
        for shadow in shadows.iter().rev() {
            for (dx, dy, alpha_scale) in shadow_blur_offsets(shadow.blur_radius) {
                let alpha = shadow.color.alpha * alpha_scale;
                if quantize_alpha_step_is_transparent(alpha) {
                    continue;
                }
                content.save_state();
                apply_fill_alpha(content, alpha, alpha_gs_names);
                content.begin_text();
                content.set_fill_rgb(
                    shadow.color.red as f32 / 255.0,
                    shadow.color.green as f32 / 255.0,
                    shadow.color.blue as f32 / 255.0,
                );
                content.set_text_rendering_mode(TextRenderingMode::Fill);
                content.set_font(Name(resource_name.as_bytes()), run.font_size);
                // CSS's offset-y is positive downwards; PDF's Y is positive upwards.
                content.set_text_matrix([
                    1.0,
                    0.0,
                    shear,
                    1.0,
                    x + shadow.offset_x + dx,
                    run_baseline_y - shadow.offset_y - dy,
                ]);
                content.set_char_spacing(run.letter_spacing);
                show_run_glyphs(content, run, font, remap);
                content.end_text();
                content.restore_state();
            }
        }
    }
}

/// The offsets used to approximate the blur (`(dx, dy, alpha multiplier)`). With a
/// `blur_radius` of 0 it is just the centre, once. Otherwise the centre plus four directions
/// per step, distributed so the alphas add up to roughly 1.
fn shadow_blur_offsets(blur_radius: f32) -> Vec<(f32, f32, f32)> {
    if blur_radius <= 0.0 {
        return vec![(0.0, 0.0, 1.0)];
    }
    let mut offsets = Vec::with_capacity(1 + TEXT_SHADOW_BLUR_STEPS * 4);
    let count = 1 + TEXT_SHADOW_BLUR_STEPS * 4;
    let alpha_scale = 1.0 / count as f32;
    offsets.push((0.0, 0.0, alpha_scale));
    for step in 1..=TEXT_SHADOW_BLUR_STEPS {
        // Spread evenly from the inside of the blur radius to the outside.
        let r = blur_radius * step as f32 / TEXT_SHADOW_BLUR_STEPS as f32;
        offsets.push((r, 0.0, alpha_scale));
        offsets.push((-r, 0.0, alpha_scale));
        offsets.push((0.0, r, alpha_scale));
        offsets.push((0.0, -r, alpha_scale));
    }
    offsets
}

/// Whether an alpha becomes fully transparent after quantisation (that is, drawing it would be invisible).
fn quantize_alpha_step_is_transparent(alpha: f32) -> bool {
    quantize_alpha_step(alpha) == 0
}

/// From the font's `post` (underline) and `OS2` (strikethrough) tables, find the signed
/// offset from the baseline and the line thickness in px. In a font without those tables,
/// `fallback_ratio` (a ratio of the font size) is used as a position relative to the ascent.
fn decoration_metrics(
    font: &crate::fonts::Font,
    font_size: f32,
    metrics: Option<(i16, i16)>,
    fallback_ratio: f32,
) -> (f32, f32) {
    let units_per_em = font.units_per_em() as f32;
    match metrics {
        Some((position, thickness)) if thickness > 0 => (
            position as f32 / units_per_em * font_size,
            thickness as f32 / units_per_em * font_size,
        ),
        _ => (font_size * fallback_ratio, font_size * 0.05),
    }
}

/// Convert a distance from the top of the page's content area (CSS Y, positive downwards)
/// into a PDF user-space Y coordinate (a distance from the physical bottom of the page, positive upwards).
fn to_pdf_y(settings: &PageSettings, y_from_content_top: f32) -> f32 {
    settings.size.height - settings.margin.top - y_from_content_top
}

/// The horizontal and vertical placement of the content in an `@page` margin box (`@top-left` and the other 15).
#[derive(Debug, Clone, Copy, PartialEq)]
enum HAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// Return the rectangle of one margin box plus the placement rules for its content.
///
/// The coordinate system is the same as `render_line`/`render_image`: relative to the
/// content area (not the inside of the padding/border, but the inside of the page's margins,
/// that is inside `settings.margin`). It has to match the assumption that `render_line`
/// converts to PDF coordinates as `settings.margin.left + line.rect.x` and `to_pdf_y` as
/// `settings.size.height - settings.margin.top - y`. A margin box lies outside that content
/// area, so it is correct for its x/y to go negative or exceed content_width/content_height.
///
/// The four corner boxes are a fixed size; the other twelve share an edge's margin width in thirds
fn margin_box_area_rect(area: MarginBoxArea, settings: &PageSettings) -> (Rect, HAlign, VAlign) {
    let m = settings.margin;
    let content_width = settings.content_width();
    let content_height = settings.content_height();
    let strip_w = content_width / 3.0;
    let strip_h = content_height / 3.0;

    use MarginBoxArea::*;
    match area {
        TopLeftCorner => (
            rect(-m.left, -m.top, m.left, m.top),
            HAlign::Left,
            VAlign::Middle,
        ),
        TopLeft => (
            rect(0.0, -m.top, strip_w, m.top),
            HAlign::Left,
            VAlign::Middle,
        ),
        TopCenter => (
            rect(strip_w, -m.top, strip_w, m.top),
            HAlign::Center,
            VAlign::Middle,
        ),
        TopRight => (
            rect(strip_w * 2.0, -m.top, strip_w, m.top),
            HAlign::Right,
            VAlign::Middle,
        ),
        TopRightCorner => (
            rect(content_width, -m.top, m.right, m.top),
            HAlign::Right,
            VAlign::Middle,
        ),

        BottomLeftCorner => (
            rect(-m.left, content_height, m.left, m.bottom),
            HAlign::Left,
            VAlign::Middle,
        ),
        BottomLeft => (
            rect(0.0, content_height, strip_w, m.bottom),
            HAlign::Left,
            VAlign::Middle,
        ),
        BottomCenter => (
            rect(strip_w, content_height, strip_w, m.bottom),
            HAlign::Center,
            VAlign::Middle,
        ),
        BottomRight => (
            rect(strip_w * 2.0, content_height, strip_w, m.bottom),
            HAlign::Right,
            VAlign::Middle,
        ),
        BottomRightCorner => (
            rect(content_width, content_height, m.right, m.bottom),
            HAlign::Right,
            VAlign::Middle,
        ),

        LeftTop => (
            rect(-m.left, 0.0, m.left, strip_h),
            HAlign::Center,
            VAlign::Top,
        ),
        LeftMiddle => (
            rect(-m.left, strip_h, m.left, strip_h),
            HAlign::Center,
            VAlign::Middle,
        ),
        LeftBottom => (
            rect(-m.left, strip_h * 2.0, m.left, strip_h),
            HAlign::Center,
            VAlign::Bottom,
        ),

        RightTop => (
            rect(content_width, 0.0, m.right, strip_h),
            HAlign::Center,
            VAlign::Top,
        ),
        RightMiddle => (
            rect(content_width, strip_h, m.right, strip_h),
            HAlign::Center,
            VAlign::Middle,
        ),
        RightBottom => (
            rect(content_width, strip_h * 2.0, m.right, strip_h),
            HAlign::Center,
            VAlign::Bottom,
        ),
    }
}

fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Build, from a margin box's declaration list, the minimum style shaping needs (`font-*`
/// and `color` only, overriding onto `ComputedStyle::default`). A margin box has no DOM
/// element, so there is no cascade or inheritance.
fn margin_box_style(decls: &[PropertyDeclaration]) -> ComputedStyle {
    let mut style = ComputedStyle::default();
    for decl in decls {
        match decl {
            PropertyDeclaration::FontSize(v) => {
                style.font_size = v.resolve(style.font_size.0, style.font_size.0)
            }
            PropertyDeclaration::FontFamily(v) => style.font_family = v.clone(),
            PropertyDeclaration::FontWeight(v) => style.font_weight = *v,
            PropertyDeclaration::FontStyle(v) => style.font_style = *v,
            PropertyDeclaration::Color(Color::Rgba {
                red,
                green,
                blue,
                alpha,
            }) => {
                style.color = RgbaColor {
                    red: *red,
                    green: *green,
                    blue: *blue,
                    alpha: *alpha,
                }
            }
            _ => {}
        }
    }
    style
}

struct ShapedMarginBox {
    rect: Rect,
    h_align: HAlign,
    v_align: VAlign,
    line: LineBox,
}

/// A subdocument drawn over the page's margin area
/// (`--header-html`/`--footer-html`).
///
/// It holds a laid-out list of boxes plus the `PageSettings` its drawing is relative to.
/// Making that a dedicated `PageSettings` aligned to the margin area lets the existing
/// `render_box` (which derives y coordinates from `settings`) be reused unchanged.
#[derive(Clone)]
pub struct PageOverlay {
    pub boxes: Vec<LaidOutBox>,
    pub styles: HashMap<NodeId, Rc<ComputedStyle>>,
    /// The drawing settings relative to the margin area.
    pub settings: PageSettings,
    /// The clip rectangle trimming any overflow (CSS px, origin at the page's top left).
    pub clip: Rect,
}

/// Draw a [`PageOverlay`] into the page's content stream.
pub(super) fn render_page_overlay(
    content: &mut RenderTarget<'_>,
    overlay: &PageOverlay,
    fonts: &FontCollection,
    font_resource_names: &[String],
    alpha_gs_names: &[String],
) {
    if overlay.boxes.is_empty() {
        return;
    }
    let empty_images: HashMap<NodeId, Rc<PreparedImage>> = HashMap::new();
    let empty_image_ids: HashMap<usize, ImageIds> = HashMap::new();
    let empty_form_ids: HashMap<NodeId, Ref> = HashMap::new();
    let mut pending_forms: Vec<(Ref, Vec<u8>)> = Vec::new();

    content.save_state();
    // Anything spilling out of the margin is trimmed (the margin is never grown automatically).
    let y = overlay.settings.size.height - overlay.clip.y - overlay.clip.height;
    content.rect(overlay.clip.x, y, overlay.clip.width, overlay.clip.height);
    content.clip_nonzero();
    content.end_path();

    for b in &overlay.boxes {
        render_box(
            content,
            b,
            &overlay.styles,
            fonts,
            &overlay.settings,
            None,
            font_resource_names,
            &empty_image_ids,
            &empty_images,
            alpha_gs_names,
            &empty_form_ids,
            &mut pending_forms,
        );
    }
    content.restore_state();
}

/// Draw the rule for `--header-line`/`--footer-line`.
///
/// A margin box supports no decoration (no borders), so it is drawn directly as a horizontal
/// line when the page is drawn. The positions are the top (header) and bottom (footer) of the content area.
pub(super) fn render_header_footer_rules(
    content: &mut RenderTarget<'_>,
    settings: &PageSettings,
    header_line: bool,
    footer_line: bool,
) {
    if !header_line && !footer_line {
        return;
    }
    let x0 = settings.margin.left;
    let x1 = settings.size.width - settings.margin.right;

    content.save_state();
    content.set_stroke_rgb(0.0, 0.0, 0.0);
    content.set_line_width(1.0);
    if header_line {
        let y = to_pdf_y(settings, 0.0);
        content.move_to(x0, y);
        content.line_to(x1, y);
        content.stroke();
    }
    if footer_line {
        let y = to_pdf_y(settings, settings.content_height());
        content.move_to(x0, y);
        content.line_to(x1, y);
        content.stroke();
    }
    content.restore_state();
}

/// Return the margin boxes that should really be drawn on this page - only those whose
/// `content` is non-empty - already shaped. Shared by both drawing (`render_margin_boxes`)
/// and glyph usage collection (`collect_margin_box_usage`).
fn shape_margin_boxes_for_page(
    settings: &PageSettings,
    fonts: &FontCollection,
    page_rules: &[PageRule],
    page_number: usize,
    total_pages: Option<usize>,
) -> Vec<ShapedMarginBox> {
    if page_rules.is_empty() {
        return Vec::new();
    }
    let is_first = page_number == 1;
    let is_left = page_number.is_multiple_of(2);
    let resolved = resolve_page_rules(page_rules, is_first, is_left);

    resolved
        .margin_boxes
        .iter()
        .filter_map(|(area, decls)| {
            let content_decl = decls.iter().rev().find_map(|d| match d {
                PropertyDeclaration::Content(parts) => Some(parts.clone()),
                _ => None,
            })?;
            let parts = content_decl?;
            let text = resolve_margin_box_content(&parts, page_number, total_pages);
            if text.is_empty() {
                return None;
            }
            let style = margin_box_style(decls);
            let (rect, h_align, v_align) = margin_box_area_rect(*area, settings);
            let line = shape_standalone_line(&text, &style, fonts, 0.0, 0.0);
            Some(ShapedMarginBox {
                rect,
                h_align,
                v_align,
                line,
            })
        })
        .collect()
}

/// Draw the settled result of `shape_margin_boxes_for_page` into the content stream for real
/// (placing the origin according to the alignment and then reusing `render_line`).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_margin_boxes(
    content: &mut RenderTarget<'_>,
    settings: &PageSettings,
    fonts: &FontCollection,
    page_rules: &[PageRule],
    page_number: usize,
    total_pages: Option<usize>,
    remaps: Option<&[HashMap<u16, u16>]>,
    font_resource_names: &[String],
) {
    for shaped in shape_margin_boxes_for_page(settings, fonts, page_rules, page_number, total_pages)
    {
        let mut line = shaped.line;
        line.rect.x = match shaped.h_align {
            HAlign::Left => shaped.rect.x,
            HAlign::Center => shaped.rect.x + (shaped.rect.width - line.rect.width) / 2.0,
            HAlign::Right => shaped.rect.x + shaped.rect.width - line.rect.width,
        };
        line.rect.y = match shaped.v_align {
            VAlign::Top => shaped.rect.y,
            VAlign::Middle => shaped.rect.y + (shaped.rect.height - line.rect.height) / 2.0,
            VAlign::Bottom => shaped.rect.y + shaped.rect.height - line.rect.height,
        };
        render_line(
            content,
            &line,
            fonts,
            settings,
            remaps,
            font_resource_names,
            &[],
        );
    }
}

/// Collect the glyphs the margin boxes use, for font subsetting
/// (reusing the same `shape_margin_boxes_for_page` as `render_margin_boxes`).
pub(super) fn collect_margin_box_usage(
    settings: &PageSettings,
    fonts: &FontCollection,
    page_rules: &[PageRule],
    page_number: usize,
    total_pages: Option<usize>,
    usages: &mut [FontUsage],
) {
    for shaped in shape_margin_boxes_for_page(settings, fonts, page_rules, page_number, total_pages)
    {
        collect_line_usage(&shaped.line, fonts, usages);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::Font;
    use crate::html;
    use crate::layout::{paginate_document, PageSize};
    use crate::sink::MemorySink;
    use crate::style::BackgroundPosition;
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

    #[test]
    fn margin_box_area_rect_places_corners_and_strips_relative_to_the_content_area() {
        // Assumes an A4 equivalent with margins of 80 (top/bottom) and 60 (left/right). The
        // coordinate system is relative to the content area, as in `render_line` (a margin box
        // lies outside it, so negative values and values past content are correct).
        let settings = PageSettings {
            size: PageSize {
                width: 800.0,
                height: 1100.0,
            },
            margin: EdgeSizes {
                top: 80.0,
                right: 60.0,
                bottom: 80.0,
                left: 60.0,
            },
        };
        let content_width = settings.content_width();
        let content_height = settings.content_height();

        let (top_left_corner, h, v) = margin_box_area_rect(MarginBoxArea::TopLeftCorner, &settings);
        assert_eq!(
            top_left_corner,
            Rect {
                x: -60.0,
                y: -80.0,
                width: 60.0,
                height: 80.0
            }
        );
        assert_eq!((h, v), (HAlign::Left, VAlign::Middle));

        let (top_center, h, v) = margin_box_area_rect(MarginBoxArea::TopCenter, &settings);
        assert_eq!(top_center.y, -80.0);
        assert_eq!(top_center.height, 80.0);
        assert_eq!(top_center.x, content_width / 3.0);
        assert_eq!((h, v), (HAlign::Center, VAlign::Middle));

        let (bottom_center, h, v) = margin_box_area_rect(MarginBoxArea::BottomCenter, &settings);
        assert_eq!(bottom_center.y, content_height);
        assert_eq!(bottom_center.height, 80.0);
        assert_eq!((h, v), (HAlign::Center, VAlign::Middle));

        let (bottom_right_corner, ..) =
            margin_box_area_rect(MarginBoxArea::BottomRightCorner, &settings);
        assert_eq!(
            bottom_right_corner,
            Rect {
                x: content_width,
                y: content_height,
                width: 60.0,
                height: 80.0
            }
        );

        let (right_middle, h, v) = margin_box_area_rect(MarginBoxArea::RightMiddle, &settings);
        assert_eq!(right_middle.x, content_width);
        assert_eq!(right_middle.width, 60.0);
        assert_eq!(right_middle.y, content_height / 3.0);
        assert_eq!((h, v), (HAlign::Center, VAlign::Middle));
    }

    #[test]
    fn background_tile_rects_defaults_to_intrinsic_size_tiled_from_the_top_left() {
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 60.0,
        };
        let style = ComputedStyle::default();
        let rects = background_tile_rects(border_box, &style, (40.0, 30.0));
        // With the defaults (position: 0% 0%, size: auto auto, repeat: repeat), tiles of the
        // intrinsic size (40x30) are laid from the top left.
        assert!(rects.iter().all(|r| r.width == 40.0 && r.height == 30.0));
        assert!(rects.contains(&Rect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 30.0
        }));
        // Covering a width of 100 in steps of 40 needs 3 columns (0,40,80); a height of 60 in steps of 30 needs 2 rows (0,30).
        assert_eq!(rects.len(), 3 * 2);
    }

    #[test]
    fn quantize_alpha_step_rounds_to_the_nearest_of_21_levels() {
        assert_eq!(quantize_alpha_step(1.0), ALPHA_STEPS);
        assert_eq!(quantize_alpha_step(0.0), 0);
        // 0.3 * 20 = exactly 6.0.
        assert_eq!(quantize_alpha_step(0.3), 6);
        // Anything out of range is clamped.
        assert_eq!(quantize_alpha_step(-0.5), 0);
        assert_eq!(quantize_alpha_step(1.5), ALPHA_STEPS);
    }

    fn content_box_150x80() -> Rect {
        Rect {
            x: 10.0,
            y: 20.0,
            width: 150.0,
            height: 80.0,
        }
    }

    #[test]
    fn object_fit_rect_fill_stretches_to_the_content_box_non_uniformly() {
        let content_box = content_box_150x80();
        let style = ComputedStyle::default(); // the initial object-fit is Fill
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        assert_eq!(rect, content_box);
    }

    #[test]
    fn object_fit_rect_cover_scales_up_to_fill_and_overflows_the_shorter_axis() {
        let content_box = content_box_150x80();
        let style = ComputedStyle {
            object_fit: ObjectFit::Cover,
            ..Default::default()
        };
        // Covering an intrinsic 32x24 (a 4:3 ratio) into 150x80 (a 15:8 ratio).
        // scale = max(150/32, 80/24) = max(4.6875, 3.333..) = 4.6875.
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        assert!((rect.width - 150.0).abs() < 0.01);
        assert!((rect.height - 112.5).abs() < 0.01);
        // The initial object-position (50% 50%) centres it, so drawing starts half the
        // overflow above the content box's origin.
        assert!((rect.y - (content_box.y - (112.5 - 80.0) / 2.0)).abs() < 0.01);
    }

    #[test]
    fn object_fit_rect_contain_scales_down_and_letterboxes() {
        let content_box = content_box_150x80();
        let style = ComputedStyle {
            object_fit: ObjectFit::Contain,
            ..Default::default()
        };
        // scale = min(150/32, 80/24) = min(4.6875, 3.333..) = 3.333..
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        assert!((rect.width - 320.0 / 3.0).abs() < 0.01);
        assert!((rect.height - 80.0).abs() < 0.01);
    }

    #[test]
    fn object_fit_rect_none_uses_intrinsic_size_regardless_of_content_box() {
        let content_box = content_box_150x80();
        let style = ComputedStyle {
            object_fit: ObjectFit::None,
            ..Default::default()
        };
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        assert_eq!(rect.width, 32.0);
        assert_eq!(rect.height, 24.0);
    }

    #[test]
    fn object_fit_rect_scale_down_behaves_like_none_when_intrinsic_already_fits() {
        let content_box = content_box_150x80();
        let style = ComputedStyle {
            object_fit: ObjectFit::ScaleDown,
            ..Default::default()
        };
        // The intrinsic 32x24 is already smaller than the content box (150x80), so it is the same as none.
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        assert_eq!(rect.width, 32.0);
        assert_eq!(rect.height, 24.0);
    }

    #[test]
    fn object_fit_rect_scale_down_behaves_like_contain_when_intrinsic_overflows() {
        let content_box = content_box_150x80();
        let style = ComputedStyle {
            object_fit: ObjectFit::ScaleDown,
            ..Default::default()
        };
        // The intrinsic 320x240 is far larger than the content box (150x80), so it is the same as contain.
        let rect = object_fit_rect(content_box, &style, (320.0, 240.0));
        assert!((rect.height - 80.0).abs() < 0.01);
        assert!(rect.width < content_box.width);
    }

    #[test]
    fn object_fit_rect_object_position_moves_the_image_within_the_content_box() {
        let content_box = content_box_150x80();
        let style = ComputedStyle {
            object_fit: ObjectFit::Contain,
            object_position: BackgroundPosition {
                horizontal: LengthPercentage::Percentage(1.0),
                vertical: LengthPercentage::Percentage(1.0),
            },
            ..Default::default()
        };
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        // The height already matches the content box exactly, so only the horizontal (right) alignment is observable.
        assert!((rect.x - (content_box.x + content_box.width - rect.width)).abs() < 0.01);
    }

    #[test]
    fn background_tile_rects_cover_scales_up_to_fill_the_box_uniformly() {
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        let style = ComputedStyle {
            background_size: BackgroundSize::Cover,
            background_repeat: BackgroundRepeat::NoRepeat,
            ..ComputedStyle::default()
        };
        let rects = background_tile_rects(border_box, &style, (100.0, 50.0));
        assert_eq!(
            rects,
            vec![Rect {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 100.0
            }]
        );
    }

    #[test]
    fn background_tile_rects_contain_scales_down_and_centers_by_default_position() {
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 200.0,
            height: 100.0,
        };
        // `background-position: center`.
        let style = ComputedStyle {
            background_size: BackgroundSize::Contain,
            background_repeat: BackgroundRepeat::NoRepeat,
            background_position: crate::style::BackgroundPosition {
                horizontal: LengthPercentage::Percentage(0.5),
                vertical: LengthPercentage::Percentage(0.5),
            },
            ..ComputedStyle::default()
        };
        let rects = background_tile_rects(border_box, &style, (100.0, 100.0));
        // scale = min(200/100, 100/100) = 1, so it stays 100x100 and is centred.
        assert_eq!(
            rects,
            vec![Rect {
                x: 50.0,
                y: 0.0,
                width: 100.0,
                height: 100.0
            }]
        );
    }

    #[test]
    fn background_tile_rects_caps_tile_count_per_axis_for_pathological_sizes() {
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 100_000.0,
            height: 10.0,
        };
        let style = ComputedStyle {
            background_size: BackgroundSize::WidthHeight(
                LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(1.0)),
                LengthPercentageOrAuto::LengthPercentage(LengthPercentage::Length(10.0)),
            ),
            ..ComputedStyle::default()
        };
        let rects = background_tile_rects(border_box, &style, (1.0, 10.0));
        // Covering 100,000px with a 1px-wide tile would really need 100,000 of them, but it
        // stops at 200 per axis.
        assert_eq!(rects.len(), 200);
    }

    fn fake_prepared_image(width: f32, height: f32) -> Rc<PreparedImage> {
        Rc::new(PreparedImage {
            width,
            height,
            content: super::super::img::PreparedContent::Raster {
                color: super::super::img::ImagePlane {
                    data: Vec::new(),
                    filter: pdf_writer::Filter::FlateDecode,
                    color_space: super::super::img::PlaneColorSpace::Rgb,
                    bits_per_component: 8,
                },
                alpha: None,
            },
        })
    }

    #[test]
    fn background_image_no_repeat_draws_a_single_xobject_without_a_clip() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<div class="box">hello</div>"#);
        let author = parse_stylesheet(
            r#".box {
                width: 200px; height: 100px;
                background-image: url("bg.png");
                background-repeat: no-repeat;
                background-size: 200px 100px;
            }"#,
        );
        let styles = compute_styles(&dom, &ua, &author);
        let div = find_tag(&dom, dom.document(), "div").expect("div not found");
        let mut background_images = HashMap::new();
        background_images.insert(div, fake_prepared_image(40.0, 30.0));

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &background_images, &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        // The tile coincides exactly with the border box (background-size: 200px 100px, and
        // the box itself 200x100), so no clip rectangle is emitted.
        assert_eq!(count_occurrences(&decompressed, b"re\nW\nn\n"), 0);
        // The XObject (the image) is drawn only once.
        assert_eq!(count_occurrences(&decompressed, b" Do\n"), 1);
    }

    #[test]
    fn background_image_repeat_tiles_and_clips_to_the_border_box() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<div class="box">hello</div>"#);
        let author = parse_stylesheet(
            r#".box {
                width: 100px; height: 60px;
                background-image: url("bg.png");
                background-repeat: repeat;
            }"#,
        );
        let styles = compute_styles(&dom, &ua, &author);
        let div = find_tag(&dom, dom.document(), "div").expect("div not found");
        let mut background_images = HashMap::new();
        // With an intrinsic 40x30, covering a 100x60 border box needs 3 columns (0,40,80) x
        // 2 rows (0,30) = 6 tiles.
        background_images.insert(div, fake_prepared_image(40.0, 30.0));

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &background_images, &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        assert!(
            count_occurrences(&decompressed, b"re\nW\nn\n") > 0,
            "tiling beyond the border box should clip"
        );
        assert_eq!(count_occurrences(&decompressed, b" Do\n"), 6);
    }

    fn find_tag(dom: &crate::html::Dom, id: NodeId, tag: &str) -> Option<NodeId> {
        if let crate::html::NodeData::Element { name, .. } = &dom.node(id).data {
            if &*name.local == tag {
                return Some(id);
            }
        }
        dom.children(id).find_map(|child| find_tag(dom, child, tag))
    }

    fn find_laid_out(b: &LaidOutBox, target: NodeId) -> Option<&LaidOutBox> {
        if b.node == Some(target) {
            return Some(b);
        }
        if let LaidOutContent::Blocks(children) = &b.content {
            return children.iter().find_map(|c| find_laid_out(c, target));
        }
        None
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

    /// Extract every `stream` to `endstream` region from the PDF bytes, inflating anything
    /// zlib (`/FlateDecode`) compressed, and return them concatenated.
    /// Content streams are compressed, so tests wanting to check the operator sequence as a
    /// string use this (structural dictionary keys such as `/Subtype /Type0`, which live
    /// outside the stream body, can be checked against the original `bytes`).
    fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
        fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
            haystack.windows(needle.len()).position(|w| w == needle)
        }

        let mut out = Vec::new();
        let mut i = 0;
        while let Some(pos) = find_subslice(&pdf_bytes[i..], b"stream\n") {
            let start = i + pos + b"stream\n".len();
            let Some(end_rel) = find_subslice(&pdf_bytes[start..], b"\nendstream") else {
                break;
            };
            let end = start + end_rel;
            let raw = &pdf_bytes[start..end];

            let mut decoder = flate2::read::ZlibDecoder::new(raw);
            let mut decompressed = Vec::new();
            if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
                out.extend_from_slice(&decompressed);
            } else {
                out.extend_from_slice(raw);
            }
            out.push(b'\n');

            i = end + b"\nendstream".len();
        }
        out
    }

    #[test]
    fn encodes_a_valid_pdf_with_embedded_font() {
        let dom = html::parse(b"<p>Hello, world!</p>");
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

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
        assert!(
            count_occurrences(&bytes, b"/FlateDecode") > 0,
            "font stream should be compressed"
        );
    }

    #[test]
    fn to_unicode_maps_a_ligature_glyph_to_every_character_it_stands_for() {
        // DejaVu Sans makes "fl" a single ligature glyph. Putting only one character in
        // ToUnicode would make "float" extract and search as "foat".
        let dom = html::parse(b"<p>float</p>");
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        // In UTF-16BE, 'f' = 0066 and 'l' = 006C.
        assert!(
            count_occurrences(&decompressed, b"<0066006C>") > 0,
            "the fl ligature glyph should map to both characters"
        );
    }

    #[test]
    fn subsetting_keeps_embedded_font_small() {
        // Embed a CJK font (about 19MB originally) in a PDF using only a short piece of text.
        // With subsetting working, the whole output PDF should be far smaller than the original font.
        let dom = html::parse("<p>日本語のテスト</p>".as_bytes());
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts_with_cjk();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        let cjk_font_size = std::fs::metadata(CJK_PATH).unwrap().len() as usize;
        assert!(
            bytes.len() < cjk_font_size / 10,
            "subsetted output ({} bytes) should be far smaller than the original CJK font ({} bytes)",
            bytes.len(),
            cjk_font_size
        );
    }

    #[test]
    fn multi_page_document_produces_one_media_box_per_page() {
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
        assert!(
            pages.len() > 1,
            "expected pagination to produce multiple pages"
        );

        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        assert_eq!(count_occurrences(&bytes, b"/MediaBox"), pages.len());
    }

    #[test]
    fn background_color_adds_fill_drawing_to_content_stream() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom_with_bg = html::parse(br#"<div class="box">x</div>"#);
        let author_with_bg = parse_stylesheet(".box { background-color: rgb(10, 20, 30); }");
        let styles_with = compute_styles(&dom_with_bg, &ua, &author_with_bg);
        let pages_with = paginate_document(&dom_with_bg, &styles_with, &fonts, &settings);
        let bytes_with = encode_pdf(
            &pages_with,
            &styles_with,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let dom_without_bg = html::parse(br#"<div class="box">x</div>"#);
        let styles_without = compute_styles(&dom_without_bg, &ua, &Stylesheet::default());
        let pages_without = paginate_document(&dom_without_bg, &styles_without, &fonts, &settings);
        let bytes_without = encode_pdf(
            &pages_without,
            &styles_without,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        assert!(
            bytes_with.len() > bytes_without.len(),
            "background-color should add extra drawing operators to the content stream"
        );
    }

    #[test]
    fn solid_border_fills_a_mitered_quad_per_side() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom_with = html::parse(br#"<div class="box">x</div>"#);
        let author_with = parse_stylesheet(".box { border: 2px solid rgb(10, 20, 30); }");
        let styles_with = compute_styles(&dom_with, &ua, &author_with);
        let pages_with = paginate_document(&dom_with, &styles_with, &fonts, &settings);
        let bytes_with = encode_pdf(
            &pages_with,
            &styles_with,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let dom_without = html::parse(br#"<div class="box">x</div>"#);
        let styles_without = compute_styles(&dom_without, &ua, &Stylesheet::default());
        let pages_without = paginate_document(&dom_without, &styles_without, &fonts, &settings);
        let bytes_without = encode_pdf(
            &pages_without,
            &styles_without,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        // Four edges' worth of fills (the `f` operator) should have been added (each edge is
        // filled as a mitred quadrilateral joining the outer and inner vertices).
        let fill_count_with = count_occurrences(&decompressed_stream_bytes(&bytes_with), b"\nf\n");
        let fill_count_without =
            count_occurrences(&decompressed_stream_bytes(&bytes_without), b"\nf\n");
        assert!(
            fill_count_with >= fill_count_without + 4,
            "solid border should add 4 filled mitered quads (with={fill_count_with}, without={fill_count_without})"
        );
    }

    #[test]
    fn text_decoration_underline_adds_stroke_operator() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom_decorated = html::parse(br#"<p class="u">underlined</p>"#);
        let author = parse_stylesheet(".u { text-decoration: underline; }");
        let styles_decorated = compute_styles(&dom_decorated, &ua, &author);
        let pages_decorated =
            paginate_document(&dom_decorated, &styles_decorated, &fonts, &settings);
        let bytes_decorated = encode_pdf(
            &pages_decorated,
            &styles_decorated,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let dom_plain = html::parse(br#"<p class="u">underlined</p>"#);
        let styles_plain = compute_styles(&dom_plain, &ua, &Stylesheet::default());
        let pages_plain = paginate_document(&dom_plain, &styles_plain, &fonts, &settings);
        let bytes_plain = encode_pdf(
            &pages_plain,
            &styles_plain,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        assert!(
            count_occurrences(&decompressed_stream_bytes(&bytes_decorated), b"\nS\n")
                > count_occurrences(&decompressed_stream_bytes(&bytes_plain), b"\nS\n"),
            "underline should add an extra stroke operator to the content stream"
        );
    }

    #[test]
    fn double_border_fills_two_bands_per_side() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom_with = html::parse(br#"<div class="box">x</div>"#);
        let author_with = parse_stylesheet(".box { border: 9px double rgb(0, 0, 0); }");
        let styles_with = compute_styles(&dom_with, &ua, &author_with);
        let pages_with = paginate_document(&dom_with, &styles_with, &fonts, &settings);
        let bytes_with = encode_pdf(
            &pages_with,
            &styles_with,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let dom_without = html::parse(br#"<div class="box">x</div>"#);
        let styles_without = compute_styles(&dom_without, &ua, &Stylesheet::default());
        let pages_without = paginate_document(&dom_without, &styles_without, &fonts, &settings);
        let bytes_without = encode_pdf(
            &pages_without,
            &styles_without,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        // 4 edges x 2 bands (outer/inner) = at least 8 fills should have been added.
        let fill_count_with = count_occurrences(&decompressed_stream_bytes(&bytes_with), b"\nf\n");
        let fill_count_without =
            count_occurrences(&decompressed_stream_bytes(&bytes_without), b"\nf\n");
        assert!(
            fill_count_with >= fill_count_without + 8,
            "double border should fill two mitered bands per side"
        );
    }

    #[test]
    fn double_border_with_radius_strokes_two_rounded_paths() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<div class="box">x</div>"#);
        let author =
            parse_stylesheet(".box { border: 9px double rgb(0, 0, 0); border-radius: 10px; }");
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        // The rounded path (four corners of Bezier curves) should be stroked twice round (with
        // no background colour set, there is no fill).
        let decompressed = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&decompressed, b" c\n") >= 8,
            "double border with radius should draw two rounded stroke paths"
        );
        assert!(
            count_occurrences(&decompressed, b"\nS\n") >= 2,
            "double border with radius should stroke twice"
        );
    }

    #[test]
    fn dotted_border_uses_round_cap_and_dash_pattern() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<div class="box">x</div>"#);
        let author = parse_stylesheet(".box { border: 1px dotted rgb(0, 0, 0); }");
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let text = String::from_utf8_lossy(&decompressed_stream_bytes(&bytes)).into_owned();

        assert!(text.contains(" J\n"), "dotted border should set a line cap");
        assert!(
            text.contains(" d\n"),
            "dotted border should set a dash pattern"
        );
    }

    #[test]
    fn uniform_border_radius_draws_curved_path_instead_of_straight_rect() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<div class="box">x</div>"#);
        let author = parse_stylesheet(
            ".box { border: 2px solid rgb(0, 0, 0); background-color: rgb(200, 200, 200); border-radius: 10px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);
        let text = String::from_utf8_lossy(&decompressed);

        // A rounded path uses the Bezier curve operator `c`.
        assert!(
            count_occurrences(&decompressed, b" c\n") >= 8,
            "rounded corners should use cubic bezier curve operators (4 corners x fill+stroke)"
        );
        // The straight rectangle `re` should not be used (the corners being rounded).
        assert!(
            !text.contains(" re\n"),
            "rounded box should not use a plain rectangle"
        );
    }

    #[test]
    fn non_uniform_border_with_radius_falls_back_to_straight_edges() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<div class="box">x</div>"#);
        let author = parse_stylesheet(
            ".box { border-style: solid dotted; border-width: 2px; border-color: rgb(0,0,0); border-radius: 10px; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        // The four edges differ, so the rounding is given up and it falls back to four straight
        // edges. `border-style: solid dotted` expands to solid (filled) top and bottom and
        // dotted (stroked) left and right, so both should appear.
        let decompressed = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&decompressed, b"\nf\n") >= 2,
            "the two solid sides should fill mitered quads"
        );
        assert!(
            count_occurrences(&decompressed, b"\nS\n") >= 2,
            "the two dotted sides should still stroke a centerline"
        );
    }

    #[test]
    fn non_uniform_solid_border_corners_share_exact_miter_vertices() {
        use crate::layout::{EdgeSizes, PageSize};

        // Use PageSettings with zero page margins and round numbers so the coordinates can be
        // predicted by hand. Every edge is given a different width and colour, and the actual
        // coordinate sequence in the generated content stream is checked to confirm that two
        // adjacent edges share the inner corner vertex exactly (that is, mitre diagonally).
        let settings = PageSettings {
            size: PageSize {
                width: 800.0,
                height: 1000.0,
            },
            margin: EdgeSizes {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
        };
        let fonts = test_fonts();

        let dom = html::parse(br#"<div class="box">x</div>"#);
        let author = parse_stylesheet(
            "html, body { margin: 0; } \
             .box { border-style: solid; border-width: 10px 20px 30px 40px; \
             border-color: rgb(255,0,0) rgb(0,255,0) rgb(0,0,255) rgb(255,255,0); \
             width: 300px; height: 200px; margin: 0; }",
        );
        let styles = compute_styles(&dom, &user_agent_stylesheet(), &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let text = String::from_utf8_lossy(&decompressed_stream_bytes(&bytes)).into_owned();

        // border box: x in [0,360] (border-left 40 + width 300 + border-right 20);
        // in PDF space y_top=1000 (border-top 10) and y_bottom=760 (border-bottom 30).
        // The top right outer corner (360,1000) and inner corner (340,990) should appear in
        // both the top and right paths (as the top's end and the right's start).
        assert_eq!(
            count_occurrences(text.as_bytes(), b"360 1000"),
            2,
            "the top-right outer corner should be shared by the top and right quads"
        );
        assert_eq!(
            count_occurrences(text.as_bytes(), b"340 990"),
            2,
            "the top-right inner (mitered) corner should be shared by the top and right quads"
        );
    }

    #[test]
    fn border_style_none_suppresses_drawing_even_with_nonzero_width() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom_with = html::parse(br#"<div class="box">x</div>"#);
        let author_with = parse_stylesheet(".box { border-width: 5px; border-style: none; }");
        let styles_with = compute_styles(&dom_with, &ua, &author_with);
        let pages_with = paginate_document(&dom_with, &styles_with, &fonts, &settings);
        let bytes_with = encode_pdf(
            &pages_with,
            &styles_with,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let dom_without = html::parse(br#"<div class="box">x</div>"#);
        let styles_without = compute_styles(&dom_without, &ua, &Stylesheet::default());
        let pages_without = paginate_document(&dom_without, &styles_without, &fonts, &settings);
        let bytes_without = encode_pdf(
            &pages_without,
            &styles_without,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        assert_eq!(
            bytes_with.len(),
            bytes_without.len(),
            "border-style: none should suppress drawing regardless of border-width"
        );
    }

    #[test]
    fn mixed_script_document_embeds_both_fonts() {
        let dom = html::parse("<p>Invoice 請求書</p>".as_bytes());
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts_with_cjk();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        // The two fonts (DejaVu Sans and Noto Sans CJK JP) should each be embedded.
        assert_eq!(count_occurrences(&bytes, b"/FontFile2"), 2);
        assert_eq!(count_occurrences(&bytes, b"/Subtype /Type0"), 2);
    }

    #[test]
    fn table_cells_render_text_borders_and_backgrounds() {
        let dom = html::parse(
            br#"<table>
                <tr><th colspan="2">Header</th></tr>
                <tr><td style="background-color: rgb(200,200,200);">Apple</td><td>100</td></tr>
            </table>"#,
        );
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet("td, th { border: 1px solid rgb(0,0,0); }");
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);
        let text = String::from_utf8_lossy(&decompressed);

        // Confirm indirectly, through the font usage (the glyph count), that each cell's text
        // is emitted into the content stream (as glyphs).
        // The text "Header"/"Apple"/"100" should all fall to one font, so only one font is embedded.

        assert_eq!(
            count_occurrences(&bytes, b"/FontFile2"),
            1,
            "all table cell text should use the single loaded font"
        );

        // The background and borders of the colspan-merged header cell plus those of the
        // ordinary cells should produce several fills (`f`) between them (the table itself has
        // no background or borders set, so they all come from the cells).
        assert!(
            count_occurrences(&decompressed, b"\nf\n") >= 2,
            "cell borders/backgrounds should produce fill operators"
        );
        // The explicitly set cell background colour should appear as a fill colour.
        assert!(
            text.contains("0.78431374 0.78431374 0.78431374 rg"),
            "the explicit cell background-color should be painted"
        );
    }

    /// Convert the given HTML/CSS to PDF and return the number of fill (`f`) operators in the
    /// inflated content stream (a simple proxy counting the total of background and border drawing).
    fn fill_operator_count(html_src: &str, css: &str) -> usize {
        let dom = html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(css);
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        count_occurrences(&decompressed_stream_bytes(&bytes), b"\nf\n")
    }

    #[test]
    fn empty_cells_hide_suppresses_decoration_for_empty_cells_in_separate_mode() {
        let html_src = r#"<table><tr><td>Apple</td><td></td></tr></table>"#;
        let base_css = "td { border: 1px solid black; background-color: rgb(200,200,200); }";

        let shown = fill_operator_count(html_src, base_css);
        let hidden = fill_operator_count(
            html_src,
            &format!("{base_css} table {{ empty-cells: hide; }}"),
        );

        assert!(
            hidden < shown,
            "hiding the empty cell should remove its border/background fills \
             (shown={shown}, hidden={hidden})"
        );
    }

    #[test]
    fn a_cell_holding_only_a_no_break_space_does_not_count_as_empty() {
        // `<td>&nbsp;</td>` is the classic way to force a frame. `&nbsp;` is non-collapsing
        // content, so `empty-cells: hide` must not remove it
        // (back when emptiness was decided with `str::trim` it counted as an empty cell).
        let css = "td { border: 1px solid black; background-color: rgb(200,200,200); } \
                   table { empty-cells: hide; }";

        let truly_empty =
            fill_operator_count(r#"<table><tr><td>Apple</td><td></td></tr></table>"#, css);
        let with_nbsp =
            fill_operator_count("<table><tr><td>Apple</td><td>\u{a0}</td></tr></table>", css);

        assert!(
            with_nbsp > truly_empty,
            "a cell with &nbsp; should keep its decoration \
             (nbsp={with_nbsp}, empty={truly_empty})"
        );
    }

    #[test]
    fn empty_cells_hide_has_no_effect_when_border_collapse_is_collapse() {
        let html_src = r#"<table><tr><td>Apple</td><td></td></tr></table>"#;
        let base_css = "td { border: 1px solid black; background-color: rgb(200,200,200); } \
             table { border-collapse: collapse; }";

        let without_hide = fill_operator_count(html_src, base_css);
        let with_hide = fill_operator_count(
            html_src,
            &format!("{base_css} table {{ empty-cells: hide; }}"),
        );

        assert_eq!(
            without_hide, with_hide,
            "empty-cells: hide should be a no-op under border-collapse: collapse"
        );
    }

    #[test]
    fn empty_cells_hide_can_be_set_on_an_individual_cell() {
        // Check that leaving the table itself at the default (show) and setting
        // `empty-cells: hide` on the empty cell alone still suppresses that cell's decoration
        // (the property applies to `table-cell` elements, so it has to be read per cell rather
        // than per table).
        let html_src = r#"<table><tr><td>Apple</td><td class="empty"></td></tr></table>"#;
        let base_css = "td { border: 1px solid black; background-color: rgb(200,200,200); }";

        let shown = fill_operator_count(html_src, base_css);
        let hidden = fill_operator_count(
            html_src,
            &format!("{base_css} .empty {{ empty-cells: hide; }}"),
        );

        assert!(
            hidden < shown,
            "hiding via a per-cell override should remove that cell's fills \
             (shown={shown}, hidden={hidden})"
        );
    }

    #[test]
    fn border_collapse_avoids_drawing_a_double_thick_border_at_a_shared_edge() {
        // Where two adjacent cells set the same border, the separate model has each cell draw
        // all four edges independently (2+2 cells' worth = 8 times). The collapse model
        // suppresses one of the two drawings of the shared internal edge and merges it into
        // one, so the total should be 7, one fewer.
        let html_src = r#"<table><tr><td>a</td><td>b</td></tr></table>"#;
        let base_css = "body { margin: 0; } td { border: 1px solid black; }";

        let separate = fill_operator_count(html_src, base_css);
        let collapse = fill_operator_count(
            html_src,
            &format!("{base_css} table {{ border-collapse: collapse; }}"),
        );

        assert_eq!(
            separate, 8,
            "each cell should draw all 4 sides independently in separate mode"
        );
        assert_eq!(
            collapse, 7,
            "collapse should merge the shared edge into a single draw (8-1=7): {collapse}"
        );
    }

    #[test]
    fn border_collapse_uses_the_neighbors_border_when_own_side_declares_none() {
        // The left cell sets no border (none), but the right cell's left edge (which really
        // resolves as the left cell's right edge, that being where the shared boundary is
        // merged) does have one, so a border must not vanish from the boundary
        // (a regression test that "own = none" must not be taken unconditionally).
        let html_src = r#"<table><tr><td class="a">a</td><td class="b">b</td></tr></table>"#;
        let css = "body { margin: 0; } \
                   table { border-collapse: collapse; } \
                   .a { border: none; } \
                   .b { border: 2px solid black; }";

        let fills = fill_operator_count(html_src, css);
        // The right cell's top, right and bottom edges (3; its left is suppressed by the
        // neighbour) plus the left cell's right edge (1, inheriting the neighbour's border) = 4 in total.
        assert_eq!(
            fills, 4,
            "the shared edge should still be drawn using the neighbor's border spec: {fills}"
        );
    }

    #[test]
    fn resolve_border_conflict_prefers_the_wider_border() {
        let wide = (
            3.0,
            BorderStyle::Solid,
            RgbaColor {
                red: 255,
                green: 0,
                blue: 0,
                alpha: 255.0,
            },
        );
        let narrow = (
            1.0,
            BorderStyle::Solid,
            RgbaColor {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 255.0,
            },
        );
        assert_eq!(resolve_border_conflict(wide, narrow), wide);
        assert_eq!(resolve_border_conflict(narrow, wide), wide);
    }

    #[test]
    fn resolve_border_conflict_prefers_a_stronger_style_when_widths_tie() {
        let solid = (
            1.0,
            BorderStyle::Solid,
            RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255.0,
            },
        );
        let dotted = (
            1.0,
            BorderStyle::Dotted,
            RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255.0,
            },
        );
        let double = (
            1.0,
            BorderStyle::Double,
            RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255.0,
            },
        );
        assert_eq!(resolve_border_conflict(solid, dotted), solid);
        assert_eq!(resolve_border_conflict(double, solid), double);
    }

    #[test]
    fn resolve_border_conflict_ignores_a_declared_width_when_style_is_none() {
        // An edge with `style: none` counts as an effective width of 0 regardless of the width
        // set, so it should lose even where the width alone would seem to "win".
        let none_but_wide = (
            10.0,
            BorderStyle::None,
            RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255.0,
            },
        );
        let thin_solid = (
            1.0,
            BorderStyle::Solid,
            RgbaColor {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255.0,
            },
        );
        let a = border_edge(none_but_wide.0, none_but_wide.1, none_but_wide.2);
        let b = border_edge(thin_solid.0, thin_solid.1, thin_solid.2);
        assert_eq!(resolve_border_conflict(a, b), b);
    }

    #[test]
    fn word_boundary_across_a_font_switch_gets_an_actual_text_space_marker() {
        // "Invoice" (DejaVu) and the Japanese (CJK) sit either side of a word boundary that
        // crosses a run boundary where the font changes, and neither TextRun.text contains an
        // actual whitespace character (inter-word space being expressed only as an x_offset
        // gap). Text extraction relying on coordinate gaps can break at a boundary with a font
        // change, so it should be stated explicitly by a marked section with an `ActualText`, which has no visual effect.
        let dom = html::parse("<p>Invoice 請求書</p>".as_bytes());
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let fonts = test_fonts_with_cjk();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        assert!(
            count_occurrences(&decompressed_stream_bytes(&bytes), b"/ActualText") > 0,
            "a word boundary spanning a font switch should get an ActualText space marker"
        );
    }

    #[test]
    fn single_word_does_not_insert_an_actual_text_marker() {
        let dom = html::parse(b"<p>hello</p>");
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        assert_eq!(
            count_occurrences(&decompressed_stream_bytes(&bytes), b"/ActualText"),
            0,
            "a single word with no boundary needs no ActualText marker"
        );
    }

    #[test]
    fn letter_spacing_emits_a_tc_operator_with_the_resolved_value() {
        // `letter-spacing` cannot be reflected in the glyph widths themselves, so it has to be
        // emitted as PDF's `Tc` (character spacing) operator.
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom = html::parse(br#"<p class="s">spaced</p>"#);
        let author = parse_stylesheet(".s { letter-spacing: 3px; }");
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);

        let stream = decompressed_stream_bytes(&bytes);
        assert!(
            count_occurrences(&stream, b" Tc\n") > 0,
            "letter-spacing should emit a Tc operator"
        );
        assert!(
            count_occurrences(&stream, b"3 Tc\n") > 0,
            "the Tc operand should match the resolved letter-spacing value"
        );
    }

    #[test]
    fn write_document_writes_pdf_bytes_to_sink() {
        let dom = html::parse(b"<p>hi</p>");
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();
        let pages = paginate_document(&dom, &styles, &fonts, &settings);

        let bytes = write_document(
            &pages,
            &styles,
            &HashMap::new(),
            &fonts,
            &settings,
            MemorySink::new(),
        )
        .unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn list_item_marker_glyphs_are_embedded_in_the_font_subset() {
        // Confirm that even in a document where no digit appears in the body at all, the
        // marker's '1' (U+0031) really is embedded in the `/ToUnicode` CMap.
        let dom = html::parse(br#"<ol><li>apple</li></ol>"#);
        let ua = user_agent_stylesheet();
        let author = Stylesheet::default();
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        assert!(
            count_occurrences(&decompressed, b"<0031>") > 0,
            "the marker's '1' glyph (from the \"1.\" decimal marker) should be \
             embedded in the ToUnicode CMap"
        );
    }

    #[test]
    fn generated_content_glyphs_are_embedded_in_the_font_subset() {
        // The characters generated by ::before/::after content (attr/counter) go through the
        // same `BoxContent::Inline` path (collect_line_usage) as an ordinary text span, so
        // unlike the marker case there should be no dedicated collection gap. Even so, this
        // confirms that a digit never appearing in the body (the counter-derived '1') really
        // is embedded.
        let dom = html::parse(br#"<div><h2>intro</h2></div>"#);
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(
            "div { counter-reset: section; } \
             h2 { counter-increment: section; } \
             h2::before { content: counter(section) \". \"; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        assert!(
            count_occurrences(&decompressed, b"<0031>") > 0,
            "the counter()-generated '1' glyph should be embedded in the ToUnicode CMap"
        );
    }

    #[test]
    fn border_side_colors_shade_inset_and_outset_by_top_left_vs_bottom_right() {
        let color = RgbaColor {
            red: 51,
            green: 102,
            blue: 204,
            alpha: 1.0,
        };
        let light = lighten(color, SHADE_AMOUNT);
        let dark = darken(color, SHADE_AMOUNT);
        assert_ne!(light, dark);

        for (side, expect_dark) in [
            (BorderSideKind::Top, true),
            (BorderSideKind::Left, true),
            (BorderSideKind::Right, false),
            (BorderSideKind::Bottom, false),
        ] {
            let colors = border_side_colors(BorderStyle::Inset, side, color);
            let expected = if expect_dark { dark } else { light };
            assert_eq!(colors.outer, expected, "inset outer for {side:?}");
            assert_eq!(colors.inner, expected, "inset inner for {side:?}");

            // outset is inset with the light and dark reversed (the opposite colour on the same edge).
            let outset_colors = border_side_colors(BorderStyle::Outset, side, color);
            let outset_expected = if expect_dark { light } else { dark };
            assert_eq!(
                outset_colors.outer, outset_expected,
                "outset outer for {side:?}"
            );
        }
    }

    #[test]
    fn border_side_colors_groove_and_ridge_split_outer_and_inner_bands() {
        let color = RgbaColor {
            red: 51,
            green: 102,
            blue: 204,
            alpha: 1.0,
        };
        let light = lighten(color, SHADE_AMOUNT);
        let dark = darken(color, SHADE_AMOUNT);

        // groove: top/left are dark outside and light inside (the depth of the groove); right/bottom are the reverse.
        let top_groove = border_side_colors(BorderStyle::Groove, BorderSideKind::Top, color);
        assert_eq!(top_groove.outer, dark);
        assert_eq!(top_groove.inner, light);
        let right_groove = border_side_colors(BorderStyle::Groove, BorderSideKind::Right, color);
        assert_eq!(right_groove.outer, light);
        assert_eq!(right_groove.inner, dark);

        // ridge is groove with the outside and inside swapped.
        let top_ridge = border_side_colors(BorderStyle::Ridge, BorderSideKind::Top, color);
        assert_eq!(top_ridge.outer, light);
        assert_eq!(top_ridge.inner, dark);
    }

    #[test]
    fn border_side_colors_solid_uses_the_same_color_for_both_bands() {
        let color = RgbaColor {
            red: 1,
            green: 2,
            blue: 3,
            alpha: 1.0,
        };
        let colors = border_side_colors(BorderStyle::Solid, BorderSideKind::Top, color);
        assert_eq!(colors.outer, color);
        assert_eq!(colors.inner, color);
    }

    #[test]
    fn outline_adds_drawing_without_affecting_layout() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let dom_without = html::parse(br#"<div class="box">x</div>"#);
        let styles_without = compute_styles(&dom_without, &ua, &Stylesheet::default());
        let pages_without = paginate_document(&dom_without, &styles_without, &fonts, &settings);
        let bytes_without = encode_pdf(
            &pages_without,
            &styles_without,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let dom_with = html::parse(br#"<div class="box">x</div>"#);
        let author_with = parse_stylesheet(".box { outline: 4px solid rgb(255, 0, 0); }");
        let styles_with = compute_styles(&dom_with, &ua, &author_with);
        let pages_with = paginate_document(&dom_with, &styles_with, &fonts, &settings);
        let bytes_with = encode_pdf(
            &pages_with,
            &styles_with,
            &HashMap::new(),
            &fonts,
            &settings,
        );

        let fill_count_with = count_occurrences(&decompressed_stream_bytes(&bytes_with), b"\nf\n");
        let fill_count_without =
            count_occurrences(&decompressed_stream_bytes(&bytes_without), b"\nf\n");
        assert!(
            fill_count_with >= fill_count_without + 4,
            "outline should add 4 filled mitered quads outside the border-box"
        );

        // An outline does not affect layout, so the `div`'s content box position and size
        // should not change with or without one.
        let div_without = find_tag(&dom_without, dom_without.document(), "div").unwrap();
        let div_with = find_tag(&dom_with, dom_with.document(), "div").unwrap();
        let box_without = pages_without[0]
            .boxes
            .iter()
            .find_map(|b| find_laid_out(b, div_without))
            .unwrap();
        let box_with = pages_with[0]
            .boxes
            .iter()
            .find_map(|b| find_laid_out(b, div_with))
            .unwrap();
        assert_eq!(box_without.layout.content, box_with.layout.content);
    }

    #[test]
    fn overflow_hidden_emits_a_clip_path_and_visible_does_not() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        for (css, should_clip) in [
            (
                ".box { overflow: hidden; width: 50px; height: 50px; }",
                true,
            ),
            (
                ".box { overflow: scroll; width: 50px; height: 50px; }",
                true,
            ),
            (".box { overflow: auto; width: 50px; height: 50px; }", true),
            (
                ".box { overflow: visible; width: 50px; height: 50px; }",
                false,
            ),
            (".box { width: 50px; height: 50px; }", false),
        ] {
            let dom = html::parse(br#"<div class="box"><p>hello</p></div>"#);
            let styles = compute_styles(&dom, &ua, &parse_stylesheet(css));
            let pages = paginate_document(&dom, &styles, &fonts, &settings);
            let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
            let decompressed = decompressed_stream_bytes(&bytes);
            let has_clip = count_occurrences(&decompressed, b"re\nW\nn\n") > 0;
            assert_eq!(has_clip, should_clip, "css={css}");
        }
    }

    #[test]
    fn visibility_hidden_skips_own_decoration_but_still_renders_a_visible_descendant() {
        let ua = user_agent_stylesheet();
        let fonts = test_fonts();
        let settings = PageSettings::default();

        // Even under a `visibility: hidden` parent, a child explicitly setting `visible` is
        // drawn (as the spec requires).
        let dom = html::parse(br#"<div class="outer"><p class="inner">shown</p></div>"#);
        let author = parse_stylesheet(
            ".outer { visibility: hidden; background-color: rgb(255, 0, 0); } \
             .inner { visibility: visible; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        // outer's background (red) should not be drawn.
        assert_eq!(
            count_occurrences(&decompressed, b"1 0 0 rg"),
            0,
            "hidden outer's red background should not be painted"
        );
        // inner's text should be emitted (as some glyph drawing).
        // A glyph run is always emitted with `TJ` so advance corrections can be interposed (see [`show_run_glyphs`]).
        assert!(
            count_occurrences(&decompressed, b"TJ") > 0,
            "visible descendant's text should still be painted"
        );
    }

    #[test]
    fn paint_order_sorts_by_z_index_and_falls_back_to_document_order() {
        let dom = html::parse(
            br#"<div>
                <p class="a" style="position: relative; z-index: 2;">a</p>
                <p class="b" style="position: relative; z-index: -1;">b</p>
                <p class="c">c</p>
                <p class="d" style="z-index: 5;">d</p>
            </div>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let fonts = test_fonts();
        let tree = crate::layout::build_box_tree(&dom, &styles);
        let laid = crate::layout::layout_document(&tree, &styles, &fonts, 800.0);
        // html5ever supplies `<html>`/`<body>` implicitly, so the `<div>`'s NodeId is found by
        // walking (rather than assuming a fixed tree depth).
        let div_node = find_tag(&dom, dom.document(), "div").expect("div not found");
        let div_box = find_laid_out(&laid, div_node).expect("div box not found");
        let LaidOutContent::Blocks(children) = &div_box.content else {
            panic!("expected the div's own children");
        };

        let ordered = paint_order(children, &styles);
        let text_of = |b: &LaidOutBox| -> String {
            let LaidOutContent::Inline(lines) = &b.content else {
                panic!("expected inline content");
            };
            lines[0].runs[0].text.clone()
        };
        let order: Vec<String> = ordered.iter().map(|b| text_of(b)).collect();
        // b(z-index:-1) < c/d (static, so z-index has no effect and counts as auto=0, in document order c then d) < a(z-index:2).
        assert_eq!(order, vec!["b", "c", "d", "a"]);
    }

    #[test]
    fn paint_order_puts_floats_above_in_flow_blocks() {
        // If a float were drawn first, the background of the block immediately after would
        // paint over it (in CSS2.1 Appendix E a float is in a layer above a block's background).
        let dom = html::parse(
            br#"<div>
                <p class="f" style="float: left; width: 100px;">f</p>
                <p class="c">c</p>
            </div>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let fonts = test_fonts();
        let tree = crate::layout::build_box_tree(&dom, &styles);
        let laid = crate::layout::layout_document(&tree, &styles, &fonts, 800.0);
        let div_node = find_tag(&dom, dom.document(), "div").expect("div not found");
        let div_box = find_laid_out(&laid, div_node).expect("div box not found");
        let LaidOutContent::Blocks(children) = &div_box.content else {
            panic!("expected the div's own children");
        };

        let ordered = paint_order(children, &styles);
        let text_of = |b: &LaidOutBox| -> String {
            let LaidOutContent::Inline(lines) = &b.content else {
                panic!("expected inline content");
            };
            lines[0].runs[0].text.clone()
        };
        let order: Vec<String> = ordered.iter().map(|b| text_of(b)).collect();
        assert_eq!(order, vec!["c", "f"]);
    }

    // ===== `<a href>` link annotations =====

    fn link_areas_of(html_src: &str, css: &str) -> Vec<LinkArea> {
        let dom = html::parse(html_src.as_bytes());
        let ua = user_agent_stylesheet();
        let author = parse_stylesheet(css);
        let styles = compute_styles(&dom, &ua, &author);
        let fonts = test_fonts();
        let settings = PageSettings::default();
        let pages = paginate_document(&dom, &styles, &fonts, &settings);

        let mut out = Vec::new();
        for page in &pages {
            for b in &page.boxes {
                collect_link_areas(b, &settings, &mut out);
            }
        }
        out
    }

    #[test]
    fn a_link_produces_one_area_per_line() {
        let areas = link_areas_of(
            r#"<p><a href="https://example.com">link text</a></p>"#,
            "body { margin: 0; }",
        );
        assert_eq!(areas.len(), 1);
        assert_eq!(&*areas[0].href, "https://example.com");
        assert!(areas[0].x1 > areas[0].x0, "{areas:?}");
        assert!(areas[0].y1 > areas[0].y0, "{areas:?}");
    }

    #[test]
    fn text_outside_the_link_is_not_part_of_the_area() {
        let areas = link_areas_of(
            r#"<p>before <a href="https://example.com">link</a> after</p>"#,
            "body { margin: 0; }",
        );
        assert_eq!(areas.len(), 1);
        // The link is not at the start of the line, so the rectangle does not start at the left edge.
        assert!(areas[0].x0 > 0.0, "{areas:?}");
    }

    #[test]
    fn a_link_broken_across_lines_produces_one_area_per_line() {
        let areas = link_areas_of(
            r#"<p><a href="https://example.com">word word word word word word word word word word word word word word word word</a></p>"#,
            "body { margin: 0; } p { width: 120px; }",
        );
        assert!(
            areas.len() > 1,
            "expected several line areas, got {areas:?}"
        );
        assert!(areas.iter().all(|a| &*a.href == "https://example.com"));
        // The vertical position differs per line.
        assert!(areas[0].y0 > areas[1].y0, "{areas:?}");
    }

    #[test]
    fn two_different_links_on_one_line_produce_two_areas() {
        let areas = link_areas_of(
            r#"<p><a href="https://a.example">a</a> <a href="https://b.example">b</a></p>"#,
            "body { margin: 0; }",
        );
        assert_eq!(areas.len(), 2);
        assert_eq!(&*areas[0].href, "https://a.example");
        assert_eq!(&*areas[1].href, "https://b.example");
    }

    #[test]
    fn a_javascript_href_is_not_turned_into_a_link() {
        let areas = link_areas_of(
            r#"<p><a href="javascript:alert(1)">click</a></p>"#,
            "body { margin: 0; }",
        );
        assert!(areas.is_empty(), "{areas:?}");
    }

    #[test]
    fn an_anchor_without_href_is_not_a_link() {
        let areas = link_areas_of(r#"<p><a name="x">anchor</a></p>"#, "body { margin: 0; }");
        assert!(areas.is_empty(), "{areas:?}");
    }

    #[test]
    fn internal_anchor_targets_are_detected_by_their_hash() {
        assert_eq!(internal_anchor_target("#section-1"), Some("section-1"));
        assert_eq!(internal_anchor_target("#"), None);
        assert_eq!(internal_anchor_target("https://example.com/#frag"), None);
    }

    #[test]
    fn destination_names_are_sanitised_for_pdf_names() {
        assert_eq!(anchor_destination_name("sec1"), "a_sec1");
        assert_eq!(anchor_destination_name("sec 1"), "a_sec_1");
        assert_eq!(anchor_destination_name("日本語"), "a____");
        assert_eq!(anchor_destination_name("a-b_c"), "a_a-b_c");
    }

    #[test]
    fn anchor_positions_are_collected_per_page() {
        let dom = html::parse(
            br#"<p id="top">top</p><p style="break-before: page;" id="second">second</p>"#,
        );
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &parse_stylesheet("body { margin: 0; }"));
        let fonts = test_fonts();
        let settings = PageSettings::default();
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        assert_eq!(pages.len(), 2, "the test document should span two pages");

        let anchor_names: HashMap<NodeId, String> = crate::html::collect_anchor_targets(&dom)
            .into_iter()
            .map(|(node, id)| (node, anchor_destination_name(&id)))
            .collect();

        let mut first_page = Vec::new();
        for b in &pages[0].boxes {
            collect_anchor_positions(b, &anchor_names, &settings, &mut first_page);
        }
        let mut second_page = Vec::new();
        for b in &pages[1].boxes {
            collect_anchor_positions(b, &anchor_names, &settings, &mut second_page);
        }

        assert_eq!(
            first_page
                .iter()
                .map(|(n, ..)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["a_top"]
        );
        assert_eq!(
            second_page
                .iter()
                .map(|(n, ..)| n.as_str())
                .collect::<Vec<_>>(),
            vec!["a_second"]
        );
    }
}
