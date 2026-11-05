//! The `Mode::Streaming` pipeline: each top-level element directly under `<body>` goes
//! through style computation, layout, pagination, PDF writing and DOM release as soon as it
//! is final (see the parent module's docs).
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

use super::{
    append_user_stylesheets, apply_content_options, apply_page_rule_settings_override,
    build_page_overlays, collect_bookmarks, ensure_default_font, load_explicit_fonts,
    overlay_fetcher, page_rules_with_extra, register_generic_fonts, warn_unresolved_font_families,
    Engine, EngineError, OverlayCache,
};
use crate::fonts::{
    ensure_cjk_fallback_font, load_font_faces, warn_uncovered_chars, FontCollection, SystemFonts,
};
use crate::html::{
    collect_anchor_targets, find_base_href, find_document_language, find_document_title, Dom,
    NodeId,
};
use crate::img::{DocumentImageCache, ImageFetcher};
use crate::layout::{
    build_box_for_element, build_box_for_inline_run, collapse_adjacent_margins,
    collect_completed_subtree_roots, has_visible_decoration, layout_document_from_with_absolutes,
    layout_out_of_flow_root, resolve_background_images, resolve_border, resolve_images,
    resolve_lpa_or_zero, resolve_padding, resolve_width_and_horizontal_margins,
    shift_box_y_in_place, top_level_kind, LaidOutBox, LayoutBox, PageSettings, PositionedBox,
    PositionedKind, PositionedLeftovers, StreamingPaginator, TopLevelKind,
};
use crate::pdf::{
    anchor_destination_name, ImageAssetCache, LinkSettings, PreparedImage, StreamingPdfWriter,
    SvgFontDb,
};
use crate::sink::Sink;
use crate::style::{
    compute_single_element_style, compute_styles_with_parent, extract_author_stylesheet,
    needs_preceding_siblings, root_custom_properties, rules_use_page_count,
    streaming_unsafe_selectors, user_agent_stylesheet, ComputedStyle, Float,
    LengthPercentageOrAuto, Stylesheet,
};

/// The state needed to process top-level elements under `Mode::Streaming`, settled once when
/// `<head>` closes (that is, when `<body>` is detected).
pub(super) struct StreamingState<S: Sink> {
    ua: Stylesheet,
    author: Stylesheet,
    fonts: FontCollection,
    /// The persistent map accumulating the styles of every top-level element processed.
    /// One page can hold boxes from several top-level elements, so
    /// `StreamingPdfWriter::write_page` needs all of it.
    styles: HashMap<NodeId, Rc<ComputedStyle>>,
    /// The side map letting the decoded image of an element with a `background-image` be
    /// looked up by `NodeId`. Like `styles`, it accumulates the top-level elements processed
    /// so far.
    background_images: HashMap<NodeId, Rc<PreparedImage>>,
    root_font_size: f32,
    /// The CSS counter state. It depends on document order, so it has to persist across
    /// top-level elements and is held here alongside `root_font_size`.
    counters: HashMap<String, Vec<i32>>,
    /// The nesting depth of `quotes` (a single counter unrelated to the tree structure).
    quote_depth: i32,
    /// The computed style of the `<body>` element itself. Used as the parent style when
    /// computing each top-level element's styles.
    body_style: ComputedStyle,
    /// The containing width for a top-level element, reflecting `<body>`'s `padding`,
    /// `border` and `margin`.
    content_width: f32,
    /// `<body>`'s `margin-left` + `border-left` + `padding-left`.
    start_x: f32,
    /// The starting Y coordinate of the next top-level element (the accumulated height so far).
    cursor_y: f32,
    /// The effective bottom margin of the last in-flow top-level element, which collapses with
    /// the next one's top margin. `None` until the first one is laid out.
    prev_margin_bottom: Option<f32>,
    /// `<body>`'s top margin when it collapses with its first in-flow child's (no top border
    /// or padding, and `height: auto`, as batch requires).
    body_collapsible_margin_top: Option<f32>,
    /// The nodes of the `fixed` boxes' subtrees, which are drawn on every page from where they
    /// appear: the per-page freeing leaves their styles and background images alone.
    pinned: HashSet<NodeId>,
    /// The inline-level top-level elements (text included) since the last block. Batch wraps
    /// such a run in one anonymous block, so it is laid out when the run ends.
    inline_run: Vec<NodeId>,
    /// The nodes of the inline runs already laid out, with the absolute index of the page each
    /// ends on: their styles are freed once that page is written.
    finished_runs: Vec<(usize, Vec<NodeId>)>,
    /// The page geometry (used to compute the overlay areas).
    page_settings: PageSettings,
    /// The layout result of a header/footer HTML that does not depend on the page number.
    overlay_cache: OverlayCache,
    /// The `font-family` names already warned about as unresolvable (so the same warning is
    /// not repeated).
    warned_font_families: Vec<String>,
    /// The characters already warned about as undrawable by any font. Streaming decides this
    /// per top-level element, so the characters already warned about are carried along to
    /// prevent duplicates.
    warned_uncovered_chars: HashSet<char>,
    /// Whether a `fixed` box, and an `absolute` one whose page was already written, have been
    /// warned about as not drawn (each is warned about once per document).
    warned_unplaced_fixed: bool,
    warned_unplaced_absolute: bool,
    /// The number of bookmarked elements so far, so each top-level element's bookmarks
    /// continue the document order.
    bookmark_count: usize,
    /// Whether a processed top-level element may be freed along with its subtree.
    ///
    /// In a document using selectors that need the preceding sibling, such as `+`/`~` or
    /// `:first-child`, the freeing is limited to the descendants and the element itself is
    /// kept. Without it, a later element would see itself as "the first child".
    release_whole_subtree: bool,
    paginator: StreamingPaginator,
    writer: StreamingPdfWriter<S>,
    /// The cache memoising `<img>` fetch and decode results within the document.
    image_cache: ImageAssetCache,
}

/// Warn about the `fixed`/`absolute` boxes `Mode::Streaming` could not draw as batch does, once
/// per kind per document. Leaving them out silently would lose content with nothing said.
fn warn_unplaced_positioned(
    leftovers: &PositionedLeftovers,
    warned_fixed: &mut bool,
    warned_absolute: &mut bool,
) {
    if leftovers.fixed_missed_pages && !*warned_fixed {
        *warned_fixed = true;
        eprintln!(
            "warning: an element with position: fixed is not drawn on the pages written before it in streaming mode\n  \
             (in batch mode it repeats on every page).\n  \
             To draw it on every page, drop --streaming or move it to the start of <body>"
        );
    }
    if !leftovers.dropped.is_empty() && !*warned_absolute {
        *warned_absolute = true;
        eprintln!(
            "warning: an element with position: absolute and no positioned ancestor is not drawn in streaming mode\n  \
             once the first page has been written (it belongs on the first page).\n  \
             To draw it, drop --streaming"
        );
    }
}

/// Lay out one in-flow top-level element below the ones before it, the way the block loop of
/// `<body>` does in batch: its top margin collapses with the previous sibling's bottom margin
/// and, for the first one, with `<body>`'s own top margin (CSS2.1 section 8.3.1).
fn lay_out_in_flow_item<S: Sink>(
    state: &mut StreamingState<S>,
    item_box: &LayoutBox,
    item_style: Option<&ComputedStyle>,
    page_area: (f32, f32),
) -> (LaidOutBox, Vec<PositionedBox>) {
    // A float takes no part in margin collapsing.
    let in_flow = item_style.is_none_or(|s| s.float == Float::None);
    if let (true, Some(prev_bottom)) = (in_flow, state.prev_margin_bottom) {
        let top = item_style.map_or(0.0, |s| {
            resolve_lpa_or_zero(s.margin_top, state.content_width)
        });
        state.cursor_y -= prev_bottom + top - collapse_adjacent_margins(prev_bottom, top);
    }
    let (mut laid_out, positioned) = layout_document_from_with_absolutes(
        item_box,
        &state.styles,
        &state.fonts,
        state.content_width,
        state.start_x,
        state.cursor_y,
        page_area,
    );
    if in_flow && state.prev_margin_bottom.is_none() {
        if let Some(body_top) = state.body_collapsible_margin_top {
            // Batch lifts the first child's effective top margin out of `<body>` and moves
            // every child up by what that saves.
            let first_top = laid_out.layout.margin.top;
            let saved = body_top + first_top - collapse_adjacent_margins(body_top, first_top);
            shift_box_y_in_place(&mut laid_out, saved);
            state.cursor_y -= saved;
        }
    }
    state.cursor_y += laid_out.layout.margin_box_height();
    if in_flow {
        state.prev_margin_bottom = Some(laid_out.layout.margin.bottom);
    }
    (laid_out, positioned)
}

impl<S: Sink> StreamingState<S> {
    /// Add the nodes of `root`'s subtree to the pinned set, whose styles and background images
    /// the per-page freeing leaves alone. `dom` may already be tombstoned under `root`, since
    /// the tree links survive.
    fn pin_subtree_styles(&mut self, dom: &Dom, root: NodeId) {
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            stack.extend(dom.children(id));
            self.pinned.insert(id);
        }
    }
}

/// Free a processed top-level element.
///
/// With `whole` as `false`, only the descendants are freed and the element itself is kept.
/// A kept element retains its tag name, classes and id, so a later sibling still sees it as "the preceding sibling".
fn release_processed(mut dom: std::cell::RefMut<'_, Dom>, node: NodeId, whole: bool) {
    if whole {
        dom.release_subtree(node);
    } else {
        dom.release_descendants(node);
    }
}

impl<S: Sink> Engine<S> {
    /// Create the `StreamingState` if `<body>` has been detected and one has not been created
    /// yet. `sink` is moved into `StreamingState::writer` here (`self.sink` is `None` from
    /// then on).
    pub(super) fn ensure_streaming_state_initialized(
        &mut self,
    ) -> Result<(), EngineError<S::Error>> {
        if self.streaming.is_some() {
            return Ok(());
        }
        let Some(body) = self.parser.body_node() else {
            return Ok(());
        };
        let sink = self
            .sink
            .take()
            .expect("the sink is taken exactly once, when the streaming state is initialised");
        let state = self.init_streaming_state(body, sink)?;
        self.streaming = Some(state);
        Ok(())
    }

    /// The initialisation done once when `<head>` closes (that is, when `<body>` is
    /// detected): resolving the fonts, computing the `<html>`/`<body>` styles, checking
    /// `<body>`'s decoration, and building the `StreamingPdfWriter`.
    fn init_streaming_state(
        &self,
        body: NodeId,
        sink: S,
    ) -> Result<StreamingState<S>, EngineError<S::Error>> {
        let mut ua = user_agent_stylesheet();
        append_user_stylesheets(&mut ua, &self.options.content.user_stylesheets);
        let base_dir = self
            .options
            .base_dir
            .as_deref()
            .unwrap_or_else(|| Path::new("."));
        // The fetcher and cache for retrieving external stylesheets (`<link>`). It is a
        // separate instance from the image `ImageAssetCache` (`image_cache` below).
        // `<base href>` appears in `<head>`, so it is already parsed by this point (when the
        // first top-level element becomes final).
        let base_href =
            find_base_href(&self.parser.dom()).or_else(|| self.options.base_href.clone());
        let css_fetcher =
            ImageFetcher::new(base_dir.to_path_buf(), self.options.allow_remote_assets)
                .with_base_href(base_href.clone())
                .with_local_access(
                    self.options.local_access.allow,
                    self.options.local_access.allowed_dirs.clone(),
                );
        let css_cache = DocumentImageCache::new();
        let author = {
            let dom = self.parser.dom();
            extract_author_stylesheet(&dom, &css_fetcher, &css_cache)
        };
        let root_properties = root_custom_properties(&self.parser.dom(), &ua, &author);
        let page_rules = page_rules_with_extra(
            self.options.extra_page_css.as_deref(),
            &author.page_rules,
            &root_properties,
        );
        let page_settings = apply_page_rule_settings_override(self.options.settings, &page_rules);
        if rules_use_page_count(&page_rules) {
            return Err(EngineError::UnsupportedInStreamingMode(
                "counter(pages) in an @page margin box cannot be used in streaming mode\n  \
                 (the total page count cannot be known in a single pass).\n  \
                 To use it, drop --streaming",
            ));
        }
        // `[topage]` in `--header-html`/`--footer-html` is unusable for the same reason.
        if self.options.header_footer_html.uses_total_pages() {
            return Err(EngineError::UnsupportedInStreamingMode(
                "[topage] in --header-html/--footer-html cannot be used in streaming mode\n  \
                 (the total page count cannot be known in a single pass).\n  \
                 To use it, drop --streaming",
            ));
        }
        // A table of contents cannot be built until the whole body has been paginated.
        if self.options.toc.enabled {
            return Err(EngineError::UnsupportedInStreamingMode(
                "--toc cannot be used in streaming mode\n  \
                 (a table of contents needs the body's page numbers).\n  \
                 To use it, drop --streaming",
            ));
        }
        // A backward-referencing selector always fails to match. It is not an error, but the
        // result changing silently is worth avoiding, so it is warned about.
        let unsafe_selectors = streaming_unsafe_selectors(&author);
        if !unsafe_selectors.is_empty() {
            eprintln!(
                "warning: {} gives a different result in streaming mode\n  \
                 (an element directly under <body> becomes final before its siblings either side are known).\n  \
                 To use them, drop --streaming",
                unsafe_selectors.join(", ")
            );
        }

        let system_fonts = if self.options.disable_system_fonts {
            SystemFonts::none()
        } else {
            SystemFonts::scan()
        };
        let mut fonts = FontCollection::new(load_explicit_fonts(&self.options.fonts)?);

        register_generic_fonts(&mut fonts, &self.options.generic_fonts)?;
        for loaded in load_font_faces(&author.font_faces, &css_fetcher, &system_fonts) {
            fonts.push_font_face(
                loaded.family,
                Some(loaded.weight),
                Some(loaded.style),
                loaded.unicode_range,
                loaded.font,
            );
        }
        // `load_missing_system_fonts` and `load_fonts_for_uncovered_chars` need the whole
        // document's styles (and characters), which genuine streaming never holds at once, so
        // they are not called here.
        // Instead, where no font has been supplied at all, a font covering CJK is added up
        // front alongside the default (latin) one.
        // Nothing is added unasked when `--font`/`@font-face` has supplied a font, to avoid
        // affecting the order of the faces (`unicode-range` being first-wins) and the
        // principle that "the font passed with `--font` is the default".
        let had_no_fonts = fonts.is_empty();
        ensure_default_font(&mut fonts, &system_fonts)?;
        if had_no_fonts {
            ensure_cjk_fallback_font(&mut fonts, &system_fonts);
        }

        // The CSS counters and quote depth are state depending on document order, so the same
        // state is carried consistently from <html> through each top-level element directly
        // under <body> (`StreamingState` persists it from then on).
        let mut counters = HashMap::new();
        let mut quote_depth = 0;
        let (html_style, body_style, root_font_size) = {
            let dom = self.parser.dom();
            let html_id = dom
                .parent(body)
                .expect("<body> should have a parent element (<html>)");
            let default_root_font_size = ComputedStyle::default().font_size.0;
            let html_style = compute_single_element_style(
                &dom,
                html_id,
                None,
                default_root_font_size,
                &ua,
                &author,
                &mut counters,
                &mut quote_depth,
            );
            let root_font_size = html_style.font_size.0;
            let body_style = compute_single_element_style(
                &dom,
                body,
                Some(&html_style),
                root_font_size,
                &ua,
                &author,
                &mut counters,
                &mut quote_depth,
            );
            (html_style, body_style, root_font_size)
        };
        let _ = html_style;

        if self.options.output.tagged {
            return Err(EngineError::UnsupportedInStreamingMode(
                "a tagged PDF cannot be written in streaming mode\n  \
                 (the structure tree needs the document's elements, which streaming frees as it goes).\n  \
                 To use it, drop --streaming",
            ));
        }

        let body_border = resolve_border(&body_style);
        if has_visible_decoration(&body_style, &body_border) {
            return Err(EngineError::UnsupportedInStreamingMode(
                "a <body> with a background colour or borders cannot be used in streaming mode\n  \
                 (decoration spanning several pages cannot be reproduced).\n  \
                 To use them, drop --streaming",
            ));
        }

        // The candidate destinations for `<a href="#id">`. Under `Mode::Streaming` only what
        // has been parsed by this point (when the first top-level element became final) is
        // visible, but a destination is recorded from "the box found when that page is
        // written", so elements parsed later are covered too (what is collected here is not a
        // list of `id`s but the mapping of which node has which name).

        let anchor_names: HashMap<NodeId, String> = collect_anchor_targets(&self.parser.dom())
            .into_iter()
            .map(|(node, id)| (node, anchor_destination_name(&id)))
            .collect();

        let page_width = page_settings.content_width();
        let body_padding = resolve_padding(&body_style, page_width);
        let (body_content_width, body_margin_left, _) = resolve_width_and_horizontal_margins(
            &body_style,
            page_width,
            body_padding.left + body_padding.right,
            body_border.left + body_border.right,
        );
        let start_x = body_margin_left + body_border.left + body_padding.left;
        let body_margin_top = resolve_lpa_or_zero(body_style.margin_top, page_width);
        let start_y = body_margin_top + body_border.top + body_padding.top;
        let body_collapsible_margin_top = (body_border.top == 0.0
            && body_padding.top == 0.0
            && matches!(body_style.height, LengthPercentageOrAuto::Auto))
        .then_some(body_margin_top);

        // With no `--title`, the `<title>` becomes the PDF's `/Title`. `<html lang>` becomes
        // the catalog's `/Lang`.
        let mut output = self.options.output.clone();
        output
            .metadata
            .fill_title_from_document(find_document_title(&self.parser.dom()));
        output
            .metadata
            .fill_language_from_document(find_document_language(&self.parser.dom()));

        let writer = StreamingPdfWriter::with_options(
            &fonts,
            page_settings,
            sink,
            page_rules.clone(),
            LinkSettings {
                anchor_names: anchor_names.clone(),
                base_href: base_href.clone(),
                external: self.options.content.external_links,
                internal: self.options.content.internal_links,
                keep_relative: self.options.content.keep_relative_links,
                bookmarks: HashMap::new(),
            },
            output,
            // Tagging is refused in streaming mode above.
            HashMap::new(),
        )
        .map_err(EngineError::Io)?;
        let image_cache = ImageAssetCache::with_fetcher(
            ImageFetcher::new(base_dir.to_path_buf(), self.options.allow_remote_assets)
                .with_base_href(base_href)
                .with_local_access(
                    self.options.local_access.allow,
                    self.options.local_access.allowed_dirs.clone(),
                ),
        )
        // `<text>` inside an SVG is drawn with the document's fonts. `fonts` is complete by
        // this point (and never changes afterwards), so it can be built here.
        .with_svg_fonts(SvgFontDb::from_collection(&fonts));

        // In a document not using selectors that need the preceding sibling, the whole subtree
        // is freed as before (keeping the element would accumulate one node per top-level
        // element, so it is not kept unless needed).
        let release_whole_subtree = !needs_preceding_siblings(&author);

        Ok(StreamingState {
            ua,
            author,
            fonts,
            styles: HashMap::new(),
            background_images: HashMap::new(),
            root_font_size,
            counters,
            quote_depth,
            body_style,
            content_width: body_content_width,
            start_x,
            cursor_y: start_y,
            prev_margin_bottom: None,
            body_collapsible_margin_top,
            pinned: HashSet::new(),
            inline_run: Vec::new(),
            finished_runs: Vec::new(),
            page_settings,
            overlay_cache: OverlayCache::default(),
            warned_font_families: Vec::new(),
            warned_uncovered_chars: HashSet::new(),
            warned_unplaced_fixed: false,
            warned_unplaced_absolute: false,
            bookmark_count: 0,
            release_whole_subtree,
            paginator: StreamingPaginator::new(page_settings.content_height()),
            writer,
            image_cache,
        })
    }

    /// Take one settled top-level element (a child directly under `<body>`, text included)
    /// through style computation. A block goes on through [`Self::place_item`]; inline-level
    /// content is held until its run ends ([`Self::flush_inline_run`]).
    pub(super) fn process_top_level_element(
        &mut self,
        node: NodeId,
    ) -> Result<(), EngineError<S::Error>> {
        // Checked before entering the layout and writing for this one element.
        self.check_deadline()?;
        let Engine {
            parser,
            streaming,
            options,
            ..
        } = self;
        let options_content = &options.content;
        let state = streaming.as_mut().expect(
            "process_top_level_element is only called after the streaming state is initialised",
        );

        let (sub_styles, kind) = {
            let dom = parser.dom();
            let sub_styles = compute_styles_with_parent(
                &dom,
                node,
                &state.body_style,
                state.root_font_size,
                &state.ua,
                &state.author,
                &mut state.counters,
                &mut state.quote_depth,
            );
            let mut sub_styles = sub_styles;
            apply_content_options(&mut sub_styles, options_content);
            warn_unresolved_font_families(
                &sub_styles,
                &state.fonts,
                &mut state.warned_font_families,
            );
            // Streaming cannot top up fonts from the characters, so an undrawable character
            // is warned about each time one appears.
            warn_uncovered_chars(
                &state.fonts,
                &dom,
                &sub_styles,
                &mut state.warned_uncovered_chars,
            );
            // Read here, while the subtree's text and attributes are still in the DOM.
            state.writer.add_bookmarks(collect_bookmarks(
                &dom,
                &sub_styles,
                node,
                &mut state.bookmark_count,
            ));
            let kind = top_level_kind(&dom, &sub_styles, node);
            (sub_styles, kind)
        };
        if options_content.load_images {
            state
                .background_images
                .extend(resolve_background_images(&sub_styles, &state.image_cache));
        }
        state.styles.extend(sub_styles);

        match kind {
            // `display: none` and the like. As in batch, it does not end a run of inline content.
            TopLevelKind::None => {
                remove_subtree_styles(
                    &parser.dom(),
                    node,
                    &mut state.styles,
                    &mut state.background_images,
                    &state.pinned,
                );
                release_processed(parser.dom_mut(), node, state.release_whole_subtree);
                return Ok(());
            }
            // Batch wraps the whole run of inline content in one anonymous block, so the node
            // is held until the run ends.
            TopLevelKind::Inline => {
                state.inline_run.push(node);
                return Ok(());
            }
            TopLevelKind::Block => {}
        }
        self.flush_inline_run()?;

        let state = self
            .streaming
            .as_mut()
            .expect("the streaming state was used above");
        let item_box = {
            let dom = self.parser.dom();
            let mut item_box = build_box_for_element(&dom, &state.styles, node);
            if let (Some(item_box), true) = (&mut item_box, self.options.content.load_images) {
                resolve_images(item_box, &dom, &state.styles, &state.image_cache);
            }
            item_box
        };
        let Some(item_box) = item_box else {
            remove_subtree_styles(
                &self.parser.dom(),
                node,
                &mut state.styles,
                &mut state.background_images,
                &state.pinned,
            );
            release_processed(self.parser.dom_mut(), node, state.release_whole_subtree);
            return Ok(());
        };
        self.place_item(item_box, vec![node])
    }

    /// Lay out the run of inline-level top-level elements held so far, as one anonymous
    /// block, and write the pages it settles.
    fn flush_inline_run(&mut self) -> Result<(), EngineError<S::Error>> {
        let state = self
            .streaming
            .as_mut()
            .expect("flush_inline_run is only called after the streaming state is initialised");
        if state.inline_run.is_empty() {
            return Ok(());
        }
        let nodes = std::mem::take(&mut state.inline_run);
        let item_box = {
            let dom = self.parser.dom();
            let mut item_box = build_box_for_inline_run(&dom, &state.styles, &nodes);
            if let (Some(item_box), true) = (&mut item_box, self.options.content.load_images) {
                resolve_images(item_box, &dom, &state.styles, &state.image_cache);
            }
            item_box
        };
        let Some(item_box) = item_box else {
            // Nothing but whitespace, which generates no box between blocks.
            for &node in &nodes {
                remove_subtree_styles(
                    &self.parser.dom(),
                    node,
                    &mut state.styles,
                    &mut state.background_images,
                    &state.pinned,
                );
                release_processed(self.parser.dom_mut(), node, state.release_whole_subtree);
            }
            return Ok(());
        };
        self.place_item(item_box, nodes)
    }

    /// Take one top-level box (an element's, or the anonymous one of an inline run made of
    /// `nodes`) through layout, pagination, PDF writing and DOM release.
    fn place_item(
        &mut self,
        item_box: LayoutBox,
        nodes: Vec<NodeId>,
    ) -> Result<(), EngineError<S::Error>> {
        let Engine {
            parser,
            streaming,
            options,
            ..
        } = self;
        let state = streaming
            .as_mut()
            .expect("place_item is only called after the streaming state is initialised");

        // The `absolute`/`fixed` boxes are laid out against the page area, as `Mode::Batch`
        // lays them out, and overlaid once the element is paginated.
        let page_area = (
            state.page_settings.content_width(),
            state.page_settings.content_height(),
        );
        let own_node = item_box.node;
        let item_style = own_node.and_then(|node| state.styles.get(&node).cloned());
        let mut pages = Vec::new();
        let positioned = if item_style
            .as_ref()
            .is_some_and(|s| s.position.is_out_of_flow())
        {
            // In batch the block loop of `<body>` takes this element out of the flow: it
            // takes no space, so the flow and its margins are left as they are.
            layout_out_of_flow_root(&item_box, &state.styles, &state.fonts, page_area)
        } else {
            let (mut laid_out, positioned) =
                lay_out_in_flow_item(state, &item_box, item_style.as_deref(), page_area);

            // Where this top-level element itself carries no decoration (a background,
            // borders or a background-image; `has_visible_decoration` covers
            // background-image too), `place_split` generates no decoration fragment and this
            // node never appears in `page.boxes`. That is, `node`'s own `ComputedStyle` and
            // background image are never referenced again by `write_page`, so they can be
            // removed right here (with decoration, they are removed via
            // `collect_completed_subtree_roots` below when the page the decoration fragment
            // really landed on is flushed).
            if let (Some(node), false) = (own_node, laid_out.has_visible_decoration) {
                state.styles.remove(&node);
                state.background_images.remove(&node);
            }
            pages = state.paginator.push_item(&mut laid_out);
            positioned
        };

        // Layout is already complete and this DOM subtree (its text content, attributes and
        // so on) is never read again, so it can be freed immediately without waiting for the
        // page to flush (the `ComputedStyle`s are held separately in `state.styles`).
        for &node in &nodes {
            release_processed(parser.dom_mut(), node, state.release_whole_subtree);
        }
        // An anonymous box never shows up in `collect_completed_subtree_roots`, so the styles
        // of its run are freed once the page it ends on is written.
        if own_node.is_none() {
            let last_page = state.paginator.current_page_index();
            state.finished_runs.push((last_page, nodes));
        }

        // A `fixed` box is drawn on every page from here on, so its styles must outlive the
        // pages it is first drawn on. The tree links survive the release above.
        for pb in &positioned {
            if let (PositionedKind::Fixed, Some(root)) = (pb.kind, pb.laid.node) {
                state.pin_subtree_styles(&parser.dom(), root);
            }
        }
        let leftovers = state.paginator.place_positioned(&mut pages, positioned);
        warn_unplaced_positioned(
            &leftovers,
            &mut state.warned_unplaced_fixed,
            &mut state.warned_unplaced_absolute,
        );
        for page in &pages {
            if !options.header_footer_html.is_empty() {
                let page_number = state.writer.page_count() + 1;
                // Under `Mode::Streaming` the total page count is unknown, so `[topage]` comes out empty.

                let overlays = build_page_overlays(
                    &options.header_footer_html,
                    &state.fonts,
                    &state.page_settings,
                    page_number,
                    None,
                    &overlay_fetcher(),
                    &DocumentImageCache::new(),
                    &mut state.overlay_cache,
                    options.content.load_images,
                );
                if options.content.abort_on_media_error {
                    if let Some(err) = state
                        .overlay_cache
                        .images
                        .as_ref()
                        .and_then(ImageAssetCache::had_errors)
                    {
                        return Err(EngineError::MediaLoad(err));
                    }
                }
                state.writer.set_page_overlays(overlays);
            }
            state
                .writer
                // `Mode::Streaming` cannot know the total page count in principle, so it is
                // always `None` (a use of `counter(pages)` has already been rejected in
                // `init_streaming_state`).
                .write_page(
                    page,
                    &state.styles,
                    &state.background_images,
                    &state.fonts,
                    None,
                )
                .map_err(EngineError::Io)?;
        }

        // Free the `ComputedStyle`s and background images of the descendant nodes really
        // placed on each page and not split further (`FragmentPosition::Whole`/`Last`).
        // The DOM itself is already tombstoned above, but the tree links survive, so it can
        // still be walked with `Dom::children`.
        let dom = parser.dom();
        for page in &pages {
            for root in collect_completed_subtree_roots(page) {
                remove_subtree_styles(
                    &dom,
                    root,
                    &mut state.styles,
                    &mut state.background_images,
                    &state.pinned,
                );
            }
        }
        // A box that is never drawn needs none of its styles either.
        for root in leftovers.dropped.iter().filter_map(|pb| pb.laid.node) {
            remove_subtree_styles(
                &dom,
                root,
                &mut state.styles,
                &mut state.background_images,
                &state.pinned,
            );
        }
        let written = state.writer.page_count();
        state.finished_runs.retain(|(last_page, nodes)| {
            if *last_page >= written {
                return true;
            }
            for &node in nodes {
                remove_subtree_styles(
                    &dom,
                    node,
                    &mut state.styles,
                    &mut state.background_images,
                    &state.pinned,
                );
            }
            false
        });
        drop(dom);

        Ok(())
    }

    /// The `Mode::Streaming` half of [`Engine::finish`]: every top-level element not yet
    /// processed (including the last one, which was being held back) is processed, and then
    /// `StreamingPdfWriter::finish` writes the font embedding, xref and trailer.
    pub(super) fn finish_streaming(mut self) -> Result<S::Output, EngineError<S::Error>> {
        self.ensure_depth_within_limit()?;
        self.check_deadline()?;
        self.ensure_streaming_state_initialized()?;
        let remaining = self.parser.take_all_remaining_top_level_children();
        for node in remaining {
            self.process_top_level_element(node)?;
        }
        if self.streaming.is_some() {
            self.flush_inline_run()?;
        }

        match self.streaming {
            Some(state) => {
                let StreamingState {
                    styles,
                    background_images,
                    fonts,
                    mut writer,
                    paginator,
                    image_cache,
                    page_settings,
                    mut overlay_cache,
                    ..
                } = state;
                if self.options.content.abort_on_media_error {
                    if let Some(err) = image_cache.had_errors() {
                        return Err(EngineError::MediaLoad(err));
                    }
                }
                for page in paginator.finish() {
                    if !self.options.header_footer_html.is_empty() {
                        let page_number = writer.page_count() + 1;
                        let overlays = build_page_overlays(
                            &self.options.header_footer_html,
                            &fonts,
                            &page_settings,
                            page_number,
                            None,
                            &overlay_fetcher(),
                            &DocumentImageCache::new(),
                            &mut overlay_cache,
                            self.options.content.load_images,
                        );
                        if self.options.content.abort_on_media_error {
                            if let Some(err) = overlay_cache
                                .images
                                .as_ref()
                                .and_then(ImageAssetCache::had_errors)
                            {
                                return Err(EngineError::MediaLoad(err));
                            }
                        }
                        writer.set_page_overlays(overlays);
                    }
                    writer
                        .write_page(&page, &styles, &background_images, &fonts, None)
                        .map_err(EngineError::Io)?;
                }
                writer.finish(&fonts).map_err(EngineError::Io)
            }
            None => {
                // `<body>` never appeared (an empty document, invalid input and so on).
                // It is treated as an empty sink (not a zero-page PDF, but finished with
                // nothing written).
                let sink = self
                    .sink
                    .take()
                    .expect("with streaming uninitialised, the sink should still be held");
                sink.finish().map_err(EngineError::Io)
            }
        }
    }
}

/// Remove from `styles` the `ComputedStyle`s of the nodes in the subtree under `root`.
/// `dom` may already have had everything under `root` freed by [`Dom::release_subtree`]
/// (tombstoned), since the tree links themselves survive.
///
/// The subtrees of the `fixed` boxes in `pinned` are left alone: they are drawn on every page
/// from where they appear, so their styles must stay in `styles`.
fn remove_subtree_styles(
    dom: &Dom,
    root: NodeId,
    styles: &mut HashMap<NodeId, Rc<ComputedStyle>>,
    background_images: &mut HashMap<NodeId, Rc<PreparedImage>>,
    pinned: &HashSet<NodeId>,
) {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        // A pinned node's whole subtree is pinned too.
        if pinned.contains(&id) {
            continue;
        }
        stack.extend(dom.children(id));
        styles.remove(&id);
        background_images.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::engine::{EngineOptions, FontSpec, Mode};
    use crate::layout::PageSettings;
    use crate::sink::MemorySink;
    use crate::Engine;

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");

    fn font_spec() -> FontSpec {
        FontSpec {
            path: PathBuf::from(DEJAVU_PATH),
            index: 0,
        }
    }

    #[test]
    fn streaming_mode_releases_computed_styles_of_hidden_top_level_elements() {
        // 200 `display: none` elements, each with a few descendants. They are never laid out,
        // so their styles must be freed as soon as they are processed.
        let mut html_src = String::from("<style>.hidden { display: none; }</style><body>");
        for i in 0..200 {
            html_src.push_str(&format!(
                r#"<div class="hidden"><p>item <b>{i}</b></p></div>"#
            ));
        }
        html_src.push_str("</body>");

        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            settings: PageSettings::default(),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html_src.as_bytes()).unwrap();

        let styles_len = engine
            .streaming
            .as_ref()
            .expect("<body> should have been detected by now")
            .styles
            .len();
        assert!(
            styles_len < 20,
            "expected the styles of display: none elements to be freed, but the styles map \
             grew to {styles_len} entries"
        );

        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }

    #[test]
    fn streaming_mode_releases_computed_styles_for_flushed_pages() {
        // 200 undecorated <p>s. Holding every element's `ComputedStyle` until `finish` would
        // leave 200 elements' worth (over 400 entries) in `styles`. Freeing them as each page
        // is flushed keeps it to roughly the most recent unflushed page's worth (a few dozen entries).

        let mut html_src = String::from("<style>.item { height: 100px; margin: 0; }</style><body>");
        for i in 0..200 {
            html_src.push_str(&format!(r#"<p class="item">item {i}</p>"#));
        }
        html_src.push_str("</body>");

        let options = EngineOptions {
            mode: Mode::Streaming,
            fonts: vec![font_spec()],
            settings: PageSettings::default(),
            ..EngineOptions::default()
        };
        let mut engine = Engine::new(options, MemorySink::new());
        engine.feed(html_src.as_bytes()).unwrap();

        let styles_len = engine
            .streaming
            .as_ref()
            .expect("<body> should have been detected by now")
            .styles
            .len();
        assert!(
            styles_len < 50,
            "expected the styles map to stay small while streaming (pages should \
             release their entries once flushed), but it grew to {styles_len} entries"
        );

        let bytes = engine.finish().unwrap();
        assert!(bytes.starts_with(b"%PDF-"));
    }
}
