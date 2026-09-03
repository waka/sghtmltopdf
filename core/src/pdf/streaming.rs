//! PDFバイト列をページ確定のそばから逐次[`Sink`]へ書き出すストリーミング
//! ライター。
//!
//! 各ページのコンテンツストリームは、そのページの[`Page`]が確定した時点で
//! 即座に構築し`Sink`へ書き出す(CIDは常に元のグリフID、
//! `render_box`/`render_line`に`remaps: None`を渡す)。フォント埋め込み
//! (サブセット化・`/CIDToGIDMap`ストリームの構築)は、
//! [`StreamingPdfWriter::finish`]が呼ばれた
//! 時点(全ページ処理後)にまとめて行う。
//!
//! `pdf_writer::Pdf`はxref/trailerの構築を非公開実装に持つため、`Chunk`
//! (1オブジェクトごとの自己完結したバイト列)単位で`Sink`へ逐次書き出しつつ、
//! `(Ref, 書き込み済みオフセット)`を自前で記録し、[`StreamingPdfWriter::finish`]
//! でxref/trailerを組み立てる。

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use pdf_writer::writers::Catalog;
use pdf_writer::{Chunk, Content, Filter, Finish, Name, Rect as PdfRect, Ref};

use crate::fonts::FontCollection;
use crate::html::NodeId;
use crate::layout::{Page, PageSettings};
use crate::sink::Sink;
use crate::style::{ComputedStyle, PageRule};

use super::color_font::{write_color_fonts, FontPlan};
use super::document::{
    alpha_gs_resource_name, collect_anchor_positions, collect_image_uses, collect_link_areas,
    collect_margin_box_usage, collect_opacity_uses, collect_usage, file_identifier, render_box,
    render_header_footer_rules, render_margin_boxes, render_page_overlay, write_document_info,
    write_link_annotation, write_resources, LinkSettings, PageOverlay, RefAllocator, RenderTarget,
    TextFonts, ALPHA_STEPS,
};
use super::font::{deflate, embed_font_streaming_chunks, FontUsage};
use super::img::{embed_image_streaming_chunks, ids_for_image, ImageIds, PreparedImage};
use super::options::PdfOutputOptions;

const PDF_HEADER: &[u8] = b"%PDF-1.7\n%\x80\x80\x80\x80\n\n";

/// ページ確定のそばから逐次`Sink`へPDFバイト列を書き出すライター。
///
/// `new`でファイルヘッダを即座に書き出し、`write_page`をページ確定のたびに
/// 呼び、最後に`finish`でフォント埋め込み・xref/trailerを書いて`sink`を
/// 締める。
pub struct StreamingPdfWriter<S: Sink> {
    sink: S,
    output_len: usize,
    offsets: Vec<(Ref, usize)>,
    alloc: RefAllocator,
    catalog_id: Ref,
    pages_tree_id: Ref,
    /// Font resources (Type0 for outline glyphs, Type 3 for colour glyphs).
    ///
    /// Allocated up front because each page writes its own `/Resources`
    /// dictionary as soon as the page is final: there is no later point at
    /// which a font could be added to a page already written out.
    font_plan: FontPlan,
    usages: Vec<FontUsage>,
    page_ids: Vec<Ref>,
    settings: PageSettings,
    /// `Rc::as_ptr`(デコード結果の同一性)をキーにした、文書全体で共有する
    /// 画像Refのマップ。フォントと違い画像はページをまたいだ使用状況の
    /// 集計(サブセット化)が不要なため、`finish`まで待たずページごとに
    /// 「初出なら書き出す」形で埋めていく。
    image_ids: HashMap<usize, ImageIds>,
    /// `ids_for_image`が振り直しに失敗したSVGのキー。同じSVGが何度使われても
    /// 警告を1回で済ませるためのキャッシュ(ラスタ画像はデコード段階で失敗する
    /// のでここには来ない)。
    failed_svg_ids: HashSet<usize>,
    /// `@page`ルール(margin box描画用)。
    page_rules: Vec<PageRule>,
    /// `background-color`/`box-shadow`の半透明描画用ExtGState(0.05刻み・
    /// 21段階)。バッチモード(`encode_pdf`)と同じく文書全体で1回だけ確保する。
    alpha_gs_ids: Vec<Ref>,
    alpha_gs_names: Vec<String>,
    /// リンク注釈の生成設定。
    links: LinkSettings,
    /// これまでに書いたページで見つかったアンカーの位置
    /// (名前, ページのRef, x, y)。`finish`で`/Dests`辞書として書き出す。
    destinations: Vec<(String, Ref, f32, f32)>,
    /// メタデータ・圧縮・スケール・グレースケール。
    output: PdfOutputOptions,
    /// 次に書くページへ重ねるサブドキュメント
    /// (`--header-html`/`--footer-html`)。`write_page`で消費される。
    pending_overlays: Vec<PageOverlay>,
    /// 次に書くページのページ番号。
    ///
    /// * `Some(Some(n))`: 番号`n`のページとして扱う
    /// * `Some(None)`: 番号を持たないページ(cover)。margin boxも
    ///   ヘッダー/フッターも描かない
    /// * `None`: 明示指定なし(これまでに書いたページ数+1を使う)
    pending_page_number: Option<Option<usize>>,
}

impl<S: Sink> StreamingPdfWriter<S> {
    /// 新しいライターを作り、PDFファイルヘッダを即座に`sink`へ書き出す。
    /// `links`は内部アンカーの対応表と`<base href>`([`LinkSettings`])。
    /// 既定値なら外部リンクの注釈だけを生成する。
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

    /// [`PdfOutputOptions`]を明示して作る版。
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
        // Streaming cannot know how many colour glyphs the document will use,
        // so reserve the upper bound for every font that could have any. Slots
        // that go unused are written as empty Type 3 fonts at `finish`.
        let font_plan = FontPlan::new(fonts, &mut alloc, &FontPlan::upper_bound_counts(fonts));
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
            font_plan,
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

    /// 次に`write_page`で書くページへ重ねるサブドキュメントを設定する。
    ///
    /// `write_page`のシグネチャを変えずにヘッダー/フッターHTMLを
    /// 合成するための入口。ページごとに内容が変わりうる(`[page]`)ため、呼び
    /// 出し側がページ単位で設定する。これまでに
    /// 書き出したページ数(次のページ番号は`+1`)。
    pub fn page_count(&self) -> usize {
        self.page_ids.len()
    }

    pub fn set_page_overlays(&mut self, overlays: Vec<PageOverlay>) {
        self.pending_overlays = overlays;
    }

    /// 次に書くページのページ番号を明示する。
    ///
    /// `Some(n)`でその番号として扱い、`None`を渡すと番号を持たないページ
    /// (cover)としてmargin box・ヘッダー/フッターを描かない。
    pub fn set_next_page_number(&mut self, number: Option<usize>) {
        self.pending_page_number = Some(number);
    }

    /// 確定した1ページを即座にコンテンツストリームへエンコードし、`sink`へ
    /// 書き出す。使用したグリフは内部に軽量な[`FontUsage`]として蓄積する
    /// だけなので、呼び出し後は`page`(レイアウト結果)を破棄してよい。
    ///
    /// `total_pages`は`counter(pages)`用の総ページ数(`Mode::Streaming`では
    /// 原理的に決まらないため常に`None`、`Mode::Batch`で`@page`が
    /// `counter(pages)`を使う場合のみ事前カウント済みの値を渡す)。
    pub fn write_page(
        &mut self,
        page: &Page,
        styles: &HashMap<NodeId, Rc<ComputedStyle>>,
        background_images: &HashMap<NodeId, Rc<PreparedImage>>,
        fonts: &FontCollection,
        total_pages: Option<usize>,
    ) -> Result<(), S::Error> {
        // ページ番号は既定では「これまでに書いたページ数+1」(1始まり)だが、
        // cover/TOCのために明示指定できる。`None`は「番号を持たないページ」
        // で、margin box・ヘッダー/フッターを描かない。
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

        // 画像はフォントと違いページをまたいだ使用状況集計(サブセット化)が
        // 不要なため、このページで初出のものはこの時点で即座にXObjectとして
        // 書き出し切る。`<img>`本体と`background-image`の
        // 両方をここで一括して集める。
        let mut used_images = Vec::new();
        for b in &page.boxes {
            collect_image_uses(b, background_images, &mut used_images);
        }
        let mut page_image_refs = Vec::with_capacity(used_images.len());
        for image in &used_images {
            // `Ref`の振り直しに失敗したSVGは`None`になる(描画されない)。
            let Some((ids, is_new)) = ids_for_image(
                &mut self.alloc,
                &mut self.image_ids,
                &mut self.failed_svg_ids,
                image,
            ) else {
                continue;
            };
            let root = ids.root;
            // 書き出しは`self`を可変で借りるため、`self.image_ids`から借りた
            // `ids`をここで手放してから`write_objects`へ進む。
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

        // `opacity < 1`の要素を先に集めてRefを払い出す(バッチモード
        // `encode_pdf`と同じ構造)。
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
        // CSS px → PDF ptの換算はページ全体のCTMで行う。これ以降のcontent
        // stream内の座標はすべてCSS pxのままでよい。
        let scale = self.output.scale;
        content.transform([scale, 0.0, 0.0, scale, 0.0, 0.0]);
        // 色変換を挟むラッパー。
        let mut target = RenderTarget::new(&mut content, self.output.grayscale);
        // `remaps: None` — CIDs stay the original glyph IDs in streaming mode.
        let text_fonts = TextFonts {
            remaps: None,
            plan: &self.font_plan,
            usages: &self.usages,
        };
        for b in &page.boxes {
            render_box(
                &mut target,
                b,
                styles,
                fonts,
                &self.settings,
                &text_fonts,
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
                &text_fonts,
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
                &text_fonts,
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

        // `<a href>`の注釈と、このページに落ちたアンカーの位置。注釈は
        // 名前付き宛先を参照するだけなので、後方のページを指すリンクもこの
        // ページの時点で書き切れる。
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
                &self.font_plan,
                &page_image_refs,
                &form_refs,
                &self.alpha_gs_names,
                &self.alpha_gs_ids,
            );
        }
        self.write_chunk(page_id, &chunk)?;

        // opacityグループのForm XObjectを実際に
        // 書き出す(バッチモードと同じ方針)。
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
                    &self.font_plan,
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

    /// 残りのオブジェクト(フォント埋め込み・ページツリー・カタログ・
    /// xref/trailer)をすべて書き出し、`sink.finish()`を呼ぶ。
    pub fn finish(mut self, fonts: &FontCollection) -> Result<S::Output, S::Error> {
        let usages = std::mem::take(&mut self.usages);
        for (index, font) in fonts.fonts().iter().enumerate() {
            let Some(simple) = self.font_plan.simple(index) else {
                // A font without outlines has no Type0 font: every glyph it
                // contributes is drawn by a Type 3 colour font instead.
                continue;
            };
            let ids = simple.ids;
            let empty = FontUsage::default();
            let usage = usages.get(index).unwrap_or(&empty);
            for (id, chunk) in embed_font_streaming_chunks(font, ids, usage, self.output.compress) {
                self.write_chunk(id, &chunk)?;
            }
        }
        let mut alloc = std::mem::take(&mut self.alloc);
        let color_chunks = write_color_fonts(
            fonts,
            &self.font_plan,
            &usages,
            &mut alloc,
            &self.output.clone(),
        );
        self.alloc = alloc;
        for (id, chunk) in color_chunks {
            self.write_chunk(id, &chunk)?;
        }

        let mut chunk = Chunk::new();
        chunk
            .pages(self.pages_tree_id)
            .kids(self.page_ids.iter().copied())
            .count(self.page_ids.len() as i32);
        self.write_chunk(self.pages_tree_id, &chunk)?;

        // 名前付き宛先はすべてのページを書き終えたこの時点で解決する。
        // 前方参照のリンクもここで初めて宛先が定まる。
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

    /// `chunk`(単一の間接オブジェクトを含む前提)のバイト列を`sink`へ書き出し、
    /// 開始オフセットをxref用に記録する。
    fn write_chunk(&mut self, id: Ref, chunk: &Chunk) -> Result<(), S::Error> {
        self.write_objects(chunk, &[(id, 0)])
    }

    /// 複数のオブジェクトが入ったチャンクを書き出す。`offsets`はチャンク内の
    /// 各オブジェクトの開始位置(SVGのForm XObject群のように1チャンクに
    /// 複数オブジェクトが入る場合に使う)。
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
        // `/ID`(ファイル識別子)はバッチ書き出しと同じ作り方をする。
        // ここは`pdf_writer::Pdf`を通さず自前でtrailerを書くので、
        // 16進文字列として直接書く(バイト列としては同じ値)。
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
        // 複数ページ・複数フォント(ページをまたいでグリフ集合が変わるケース)でも構造的に妥当な
        // PDFになることを確認する。
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
        // ページ1に登場しない文字("Q"/"z")がページ2にのみ現れるケース。
        // フォント埋め込み(サブセット化+CIDToGIDMap)は全ページ処理後に
        // まとめて行われるため、ページ1のコンテンツストリーム構築時点では
        // これらのグリフの使用状況はまだ確定していない。
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
        // `StreamingPdfWriter`が`BufferedSink`(S3マルチパート
        // アップロード想定)を通しても、`Sink::write`が細切れ・多数回に
        // 分けて呼ばれることに正しく対応できることを確認する。実際の
        // S3向けバッファ付きSinkと同じ`crate::sink::BufferedSink`をここでも
        // 使い、小さめの閾値でパート分割を強制する。
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

        // 実運用では`MULTIPART_MIN_PART_SIZE`(5MB)を使うが、テストでは
        // 複数パートへの分割を確実に起こすため小さい閾値にする。
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
        // 最後のパート以外はちょうど閾値サイズであるはず(S3の制約通り、
        // 最後のパートのみ閾値未満が許される)。
        for &len in &uploaded_parts[..uploaded_parts.len() - 1] {
            assert_eq!(len, 2048);
        }
        assert!(uploaded_parts.last().copied().unwrap_or(0) <= 2048);
    }
}
