//! レイアウト結果(ページごとの[`LaidOutBox`]木)をPDFへエンコードする。
//!
//! 一括変換(ストリーミングなし)では、文書全体を`pdf_writer::Pdf`で
//! 組み立てて最後に1回だけ[`Sink`]へ書き出す。
//!
//! エンコードは2パスで行う: (1) 全ページを走査し、フォントごとに実際に使われた
//! グリフを集める、(2) 使用グリフだけにサブセット化したフォントを埋め込み、
//! 元グリフID→サブセット後グリフID(CID)の対応表を得てから、コンテンツ
//! ストリームを実際に書く。レイアウト時([`crate::layout::inline`])に
//! シェイピング済みの[`crate::fonts::ShapedGlyph`]をそのまま使うため、
//! テキストの再シェイピングは発生しない。
//!
//! テキストの色・太字・イタリックは[`crate::layout::inline::TextRun`]に
//! レイアウト時点で焼き込み済み(`<b>`/`<span style="...">`等のインライン要素
//! ごとに異なりうる)なので、ページ分割で無名化されたインライン断片
//! (`node: None`)であっても正しい見た目で描画される。
//!
//! 枠線は`border-style`が`none`でなく、かつ幅が0より大きい辺のみ描画する。
//! `solid`/`double`は、border-box外周から内周までを辺ごとの四角形(太さが
//! 不揃いなら台形)として塗りつぶす。隣接する2辺は共有する頂点(外側の角・
//! 内側の角)から独立に頂点を計算するため、太さ・色が異なっていても角が
//! 斜めにミトー結合される(ピクチャーフレームと同じ要領)。`dashed`/`dotted`
//! はダッシュパターンをストロークで表現する都合上、太さの中心線を
//! ストロークする従来方式のまま(ミトー結合はしない)。
//! `border-radius`が指定されておらず、かつ4辺すべての太さ・スタイル・色が
//! 同一の場合は角丸のベジェ曲線パスでまとめてストロークし、それ以外
//! (角丸なし、または4辺が不揃い)は上記の辺ごとの描画にフォールバックする。
//!
//! ページ分割で断片化したボックス([`crate::layout::FragmentPosition`]参照)は、
//! 継続中の辺(分割位置に接する辺)に`border-radius`を適用しない
//! (レイアウト側で`Layout::fragment`として渡された情報を見て角丸を抑制する)。
//!
//! - 太字・イタリックは対応する字形を持つフォントファイルを別途要求せず、
//!   通常字形に対して塗り+縁取り(疑似太字)・テキスト行列のせん断(疑似
//!   イタリック)で代用する
//! - 1行の中で複数フォント・複数フォントサイズが混在する場合、行のベースライン
//!   位置は先頭ランのフォント・サイズのメトリクスを基準に揃える
//! - `border-radius`が指定されていても4辺の太さ・スタイル・色が不揃いな場合は
//!   角丸を諦め、直線4辺のストロークにフォールバックする(角ごとの複雑な
//!   ブレンド処理は非対応)
//! - `border-style`の`groove`/`ridge`/`inset`/`outset`(2階調の疑似立体陰影)は非対応

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

use super::color_font::{write_color_fonts, FontPlan};
use super::font::{deflate, embed_font, FontUsage};
use super::img::{embed_image, ids_for_image, image_resource_name, ImageIds, PreparedImage};
use super::options::{current_datetime, producer_string, DocumentMetadata, PdfOutputOptions};

/// `Content`に色変換を挟むラッパー。
///
/// `--grayscale`のために描画関数すべてへ`PdfOutputOptions`を配るのは
/// 影響が大きい(`settings`は244箇所で参照されている)ため、
/// 色を書く経路だけをこの型で包む。`Deref`/`DerefMut`により
/// `Content`のメソッドはそのまま使え、`set_fill_rgb`/`set_stroke_rgb`だけが
/// ここの実装で上書きされる。
pub(super) struct RenderTarget<'a> {
    content: &'a mut Content,
    grayscale: bool,
}

impl<'a> RenderTarget<'a> {
    pub(super) fn new(content: &'a mut Content, grayscale: bool) -> Self {
        Self { content, grayscale }
    }

    /// 同じ設定で別の`Content`(form XObjectの中身など)を包む。
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

/// DOM由来のレイアウト結果(ページ列)をPDFバイト列にエンコードする。
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

/// [`encode_pdf`]に、内部アンカー(`<a href="#id">`)の対応表を渡せるようにした版。
///
/// `links`は内部アンカーの対応表と`<base href>`([`LinkSettings`])。
/// 既定値を渡した場合は外部リンクの注釈だけが生成される(既存の`encode_pdf`の
/// シグネチャを変えずに済ませるための分割)。
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

/// [`encode_pdf_with_anchors`]に、PDF書き出しオプション(メタデータ・
/// 圧縮・スケール・グレースケール)を渡せるようにした版。
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

    // `background-color`/`box-shadow`の半透明描画用ExtGState。使用状況に
    // 関わらず0.05刻み・21段階を文書全体で1回だけ確保し、フォントと同じく全
    // ページのResourcesへ無条件で列挙する。
    let alpha_gs_ids: Vec<Ref> = (0..=ALPHA_STEPS).map(|_| alloc.next()).collect();
    let alpha_gs_names: Vec<String> = (0..=ALPHA_STEPS).map(alpha_gs_resource_name).collect();
    for (step, &id) in alpha_gs_ids.iter().enumerate() {
        let a = step as f32 / ALPHA_STEPS as f32;
        pdf.ext_graphics(id).non_stroking_alpha(a).stroking_alpha(a);
    }

    // Pass 1: collect the glyphs the document uses (no content stream yet).
    // This also decides, per glyph, whether it is drawn as an outline or as a
    // colour glyph, which is what tells us how many Type 3 fonts to allocate.
    let mut usages: Vec<FontUsage> = (0..fonts.len()).map(|_| FontUsage::default()).collect();
    for page in pages {
        for b in &page.boxes {
            collect_usage(b, fonts, &mut usages);
        }
    }

    let color_font_counts: Vec<usize> = usages.iter().map(|u| u.color_font_count()).collect();
    let plan = FontPlan::new(fonts, &mut alloc, &color_font_counts);

    // Subset each font down to the glyphs actually used and embed it, keeping
    // the original-GID -> CID mapping. Fonts without outlines have no Type0
    // font at all, so there is nothing to embed for them.
    let remaps: Vec<HashMap<u16, u16>> = fonts
        .fonts()
        .iter()
        .enumerate()
        .zip(usages.iter())
        .map(|((index, font), usage)| match plan.simple(index) {
            Some(simple) => embed_font(&mut pdf, font, simple.ids, usage, output.compress)
                .into_iter()
                .collect(),
            None => HashMap::new(),
        })
        .collect();
    for (_, chunk) in write_color_fonts(fonts, &plan, &usages, &mut alloc, output) {
        pdf.extend(&chunk);
    }
    let text_fonts = TextFonts {
        remaps: Some(&remaps),
        plan: &plan,
        usages: &usages,
    };

    // Pass 2: 実際にページのコンテンツストリームを書く。画像XObjectは、
    // フォントと違ってページ間で使い回すための事前サブセット化情報が
    // 不要なため、ページごとに「初出なら書き出す」形で済ませる。
    let mut image_ids: HashMap<usize, ImageIds> = HashMap::new();
    // 振り直しに失敗したSVGを1文書内で1回しか警告しないための記録。
    let mut failed_svg_ids: HashSet<usize> = HashSet::new();
    let mut page_ids = Vec::with_capacity(pages.len());
    // 名前付き宛先(`/Dests`)は全ページを書き終えてから解決する。
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
            // `Ref`の振り直しに失敗したSVGは`None`になる(描画されない)。
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

        // `opacity < 1`の要素を先に集めてRefを払い出す(画像・フォントと同じ
        // 構造)。実際のForm XObject化(サブツリーの描画・埋め込み)は
        // `render_box`の中で行われ、その結果は`pending_forms`に積まれる。
        let mut opacity_nodes = Vec::new();
        for b in &page.boxes {
            collect_opacity_uses(b, styles, &mut opacity_nodes);
        }
        let opacity_form_ids: HashMap<NodeId, Ref> =
            opacity_nodes.iter().map(|&n| (n, alloc.next())).collect();
        let mut pending_forms: Vec<(Ref, Vec<u8>)> = Vec::new();

        let mut content = Content::new();
        // CSS px → PDF ptの換算はページ全体のCTMで行う。
        content.transform([output.scale, 0.0, 0.0, output.scale, 0.0, 0.0]);
        // 色変換を挟むラッパー。
        let mut target = RenderTarget::new(&mut content, output.grayscale);
        for b in &page.boxes {
            render_box(
                &mut target,
                b,
                styles,
                fonts,
                settings,
                &text_fonts,
                &image_ids,
                background_images,
                &alpha_gs_names,
                &opacity_form_ids,
                &mut pending_forms,
            );
        }
        let content_bytes = content.finish();

        // `<a href>`の注釈と、このページに落ちたアンカーの位置を集める。
        let mut page_links = Vec::new();
        let mut page_anchors = Vec::new();
        for b in &page.boxes {
            collect_link_areas(b, settings, &mut page_links);
            collect_anchor_positions(b, &links.anchor_names, settings, &mut page_anchors);
        }
        // 無効化された種類のリンク(`--disable-external-links`等)を落とす。
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
            &plan,
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

        // opacityグループのForm XObjectを実際に書き出す。`/BBox`はページの
        // content area全体(box-shadowのにじみ出し・`overflow: visible`・
        // transformとの組み合わせでborder-boxを超える描画がある可能性を
        // 考慮し、安全側に倒す。
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
                &plan,
                &page_image_refs,
                &form_refs,
                &alpha_gs_names,
                &alpha_gs_ids,
            );
        }
    }

    // 注釈本体を書く。内部アンカーは名前付き宛先を参照するだけなので、
    // 対象がどのページにあるか(前方参照かどうか)を気にしなくてよい。
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

/// `/Link`注釈1つを書く。内部アンカー(`#id`)は名前付き宛先(`/Dest`)を、外部
/// リンクは`/URI`アクションを書く。
pub(super) fn write_link_annotation(
    mut annotation: pdf_writer::writers::Annotation<'_>,
    area: &LinkArea,
    base_href: Option<&str>,
    scale: f32,
) {
    annotation.subtype(AnnotationType::Link);
    // 注釈の`/Rect`はページ座標系(content streamのCTMの影響を受けない)なので、
    // ここでCSS px → ptへ換算する。
    annotation.rect(PdfRect::new(
        area.x0 * scale,
        area.y0 * scale,
        area.x1 * scale,
        area.y1 * scale,
    ));
    // 既定の枠線(ビューアによっては黒枠が出る)を消す。
    annotation.border(0.0, 0.0, 0.0, None);

    match internal_anchor_target(&area.href) {
        // 名前を書くだけなので、対象がまだ書き出していない後方のページに
        // あっても構わない。対象が存在しない場合は`/Dests`に名前が現れず、
        // ビューアはクリックしても何もしない。
        Some(id) => {
            let name = anchor_destination_name(id);
            annotation.pair(Name(b"Dest"), Name(name.as_bytes()));
        }
        None => {
            // 相対URLのままではPDFビューアが解決できないため、`<base href>`が
            // 絶対URLなら解決してから書く。
            let uri = resolve_against_base_href(base_href, &area.href);
            annotation
                .action()
                .action_type(ActionType::Uri)
                .uri(pdf_writer::Str(uri.as_bytes()));
        }
    }
}

/// アンカーの`id`から、PDFの名前付き宛先で使う名前を作る。
///
/// `id`の値をそのまま名前オブジェクトにすると、空白・`#`・区切り文字の
/// エスケープが必要になる。ASCIIの英数字と`-`/`_`だけを残し、それ以外を
/// `_`に置き換えた上で接頭辞を付ける(衝突しても「同じ名前の宛先が最初の
/// 1つに解決される」だけで壊れない)。
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

/// [`crate::layout::paginate_document`]の結果を、実際に`sink`へ書き出すところまで行う。
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

/// [`write_document`]に、リンク設定とPDF書き出しオプションを渡せる版。
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

/// trailerに書く`/ID`(ファイル識別子)。
///
/// PDFの規定では2つの文字列の配列で、1つ目は文書の作成時に決まる恒久的な
/// 識別子、2つ目は更新のたびに変わる識別子。このクレートは追記更新
/// (incremental update)を行わないので、両方に同じ値を書く。
///
/// 値の中身に規定は無く、文書ごとに一意であればよい。Info辞書に書くのと
/// 同じメタデータ・作成日時・ページ数を混ぜたハッシュから16バイトを作る。
/// PDF/Aはファイル識別子を要求するため、無いと適合しない。
pub(super) fn file_identifier(metadata: &DocumentMetadata, page_count: usize) -> Vec<u8> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let datetime = current_datetime();
    let mut id = Vec::with_capacity(16);
    // 64bitのハッシュを2本つないで16バイトにする(saltを変えて別の値にする)。
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
        // `pdf_writer::Finish`もスコープにあるので、どちらの`finish`かを明示する。
        id.extend_from_slice(&Hasher::finish(&hasher).to_be_bytes());
    }
    id
}

/// PDF Info辞書を書く。`/Producer`と`/CreationDate`は常時、残りは
/// 指定されたものだけを書く。
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

/// ページの`/Resources`辞書、および各opacityグループForm XObjectの
/// `/Resources`辞書(ページと同じ内容)を組み立てる共有ロジック。
#[allow(clippy::too_many_arguments)]
pub(super) fn write_resources(
    mut resources: pdf_writer::writers::Resources<'_>,
    plan: &FontPlan,
    page_image_refs: &[Ref],
    form_refs: &[Ref],
    alpha_gs_names: &[String],
    alpha_gs_ids: &[Ref],
) {
    let mut font_dict = resources.fonts();
    for (name, id) in plan.resource_entries() {
        font_dict.pair(Name(name.as_bytes()), id);
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

/// Everything the text writer needs to know about fonts.
///
/// Carried from `render_box` down to `render_line`. It holds both the one
/// difference between batch and streaming output (whether CIDs are renumbered
/// to subset glyph IDs) and the routing of each glyph to either the ordinary
/// Type0 font or a Type 3 colour font.
pub(super) struct TextFonts<'a> {
    /// Maps original glyph IDs to subset glyph IDs (CIDs). `Some` only in
    /// batch mode (`encode_pdf`); streaming passes `None` and always uses the
    /// original glyph ID as the CID.
    pub remaps: Option<&'a [HashMap<u16, u16>]>,
    /// The font resources that were allocated (Type0 and Type 3).
    pub plan: &'a FontPlan,
    /// The glyph routing table. Read back exactly as `FontUsage::record` left
    /// it: if collection and drawing disagreed, glyphs would come out wrong.
    pub usages: &'a [FontUsage],
}

impl TextFonts<'_> {
    /// Which PDF font and which code to draw `glyph_id` of font `font_index`
    /// with.
    fn target(&self, font_index: usize, glyph_id: u16) -> GlyphTarget<'_> {
        if let Some((ordinal, code)) = self
            .usages
            .get(font_index)
            .and_then(|usage| usage.color_code(glyph_id))
        {
            if let Some(color) = self.plan.color(font_index, ordinal) {
                return GlyphTarget::Color {
                    name: &color.name,
                    code,
                };
            }
        }
        let Some(simple) = self.plan.simple(font_index) else {
            return GlyphTarget::Dropped;
        };
        // With `remaps` (batch) translate to the subset glyph ID; without it
        // (streaming) keep the original glyph ID.
        let cid = match self.remaps {
            Some(remaps) => match remaps.get(font_index).and_then(|m| m.get(&glyph_id)) {
                Some(&cid) => cid,
                // Not in the subset, i.e. judged undrawable.
                None => return GlyphTarget::Dropped,
            },
            None => glyph_id,
        };
        GlyphTarget::Simple {
            name: &simple.name,
            cid,
        }
    }
}

/// How a single glyph is to be drawn.
#[derive(PartialEq)]
enum GlyphTarget<'a> {
    /// The ordinary Type0 font, with two-byte CIDs.
    Simple { name: &'a str, cid: u16 },
    /// A Type 3 colour font, with one-byte codes.
    Color { name: &'a str, code: u8 },
    /// A glyph with neither an outline nor a colour representation. Nothing
    /// is emitted for it.
    Dropped,
}

impl GlyphTarget<'_> {
    /// The resource name, which is what decides whether two glyphs can share
    /// one `Tf`/`Tm`.
    fn resource_name(&self) -> Option<&str> {
        match self {
            GlyphTarget::Simple { name, .. } | GlyphTarget::Color { name, .. } => Some(name),
            GlyphTarget::Dropped => None,
        }
    }

    /// The bytes to hand to a text-showing operator: two for Type0, one for
    /// Type 3.
    fn code_bytes(&self) -> Vec<u8> {
        match self {
            GlyphTarget::Simple { cid, .. } => cid.to_be_bytes().to_vec(),
            GlyphTarget::Color { code, .. } => vec![*code],
            GlyphTarget::Dropped => Vec::new(),
        }
    }
}

#[derive(Default)]
pub(super) struct RefAllocator(i32);

impl RefAllocator {
    pub(super) fn next(&mut self) -> Ref {
        self.0 += 1;
        Ref::new(self.0)
    }

    /// 次に払い出される`Ref`を、消費せずに覗く。
    ///
    /// 「払い出してみて、駄目だったら払い出さなかったことにする」ために使う
    /// (SVGの`Ref`振り直しは失敗しうるが、失敗しても番号を消費してしまうと
    /// 書き出されないオブジェクト番号が生まれ、`StreamingPdfWriter`の
    /// 「1から連番で全部書かれている」前提のxrefが壊れる)。
    #[cfg(feature = "svg")]
    pub(super) fn peek(&self) -> Ref {
        Ref::new(self.0 + 1)
    }

    /// [`peek`](Self::peek)から始まる`count`個をまとめて消費する。
    #[cfg(feature = "svg")]
    pub(super) fn commit(&mut self, count: usize) {
        self.0 += i32::try_from(count).expect("Refの払い出し数がi32に収まらない");
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
                // 行内の`display: inline-block`の
                // 中身も同じ文書のグリフを使う。
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

/// `line`(通常の行、または`display: list-item`のマーカーを表す1ラン限りの
/// 合成`LineBox`)が実際に使うグリフを集める。
fn collect_line_usage(line: &LineBox, fonts: &FontCollection, usages: &mut [FontUsage]) {
    for run in &line.runs {
        let Some(font) = fonts.get(run.font_index) else {
            continue;
        };
        for (i, glyph) in run.glyphs.iter().enumerate() {
            let text = cluster_text(&run.text, &run.glyphs, i);
            usages[run.font_index].record(font, glyph.glyph_id, text);
        }
        // `text-emphasis-style: <string>`のマークはテキストに現れない文字を
        // グリフとして描くため、サブセットから落ちないようここで記録する
        // (キーワード指定のマークはパスで描くので収集は不要)。
        if let Some(EmphasisStyle::String(ch)) = run.emphasis.as_ref().map(|mark| &mark.style) {
            if let Some(glyph_id) = font.glyph_id(*ch) {
                usages[run.font_index].record(font, glyph_id, ch.encode_utf8(&mut [0u8; 4]));
            }
        }
    }
}

/// `glyphs[index]`が表す元テキスト(クラスタ)を`text`から切り出す。
///
/// `ShapedGlyph::cluster`は元テキスト内のバイトオフセットで、シェイピングの
/// 結果1グリフが複数文字に対応することがある(`fl`のような合字)。次に
/// オフセットが進むグリフの手前までを、そのグリフが表す文字列として扱う。
/// これを1文字に切り詰めると、`/ToUnicode`が不完全になりPDFのテキスト検索・
/// コピーで文字が欠ける。
///
/// 逆に、1つのクラスタに複数グリフが対応する場合(結合文字等)や、
/// クラスタが前へ戻る場合(RTL)は、従来どおり先頭1文字だけを割り当てる。
/// 前者で全グリフにクラスタ全体を割り当てると抽出時に文字が重複し、後者では
/// クラスタの範囲を前方だけからは決められないため。
fn cluster_text<'a>(text: &'a str, glyphs: &[crate::fonts::ShapedGlyph], index: usize) -> &'a str {
    let start = (glyphs[index].cluster as usize).min(text.len());
    let single_char = || {
        let len = text[start..].chars().next().map_or(0, char::len_utf8);
        &text[start..start + len]
    };

    // このグリフだけがクラスタを担当しているときのみ、クラスタ全体を割り当てる。
    if index > 0 && glyphs[index - 1].cluster as usize == start {
        return single_char();
    }
    let end = match glyphs.get(index + 1) {
        Some(next) if (next.cluster as usize) > start => (next.cluster as usize).min(text.len()),
        Some(_) => return single_char(),
        None => text.len(),
    };

    // クラスタ境界が文字境界と食い違っている場合(想定外のシェイピング結果)に
    // 部分文字列の切り出しでpanicしないよう、`get`で確かめる。
    text.get(start..end).unwrap_or_else(single_char)
}

/// ページ(群)を再帰的に走査し、実際に使われている画像(`<img>`本体と
/// `background-image`の両方)を`Rc`のポインタアイデンティティで重複排除して
/// 集める。フォントの`collect_usage`と同じ「使用状況を先に集めてから
/// Refを払い出す」構造。`background_images`は`NodeId → Rc<PreparedImage>`
/// 側マップ。
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

/// リンク注釈の生成に必要な文書単位の設定。
#[derive(Debug, Clone)]
pub struct LinkSettings {
    /// アンカー対象要素の`NodeId` → 名前付き宛先の名前。空なら内部アンカーの
    /// 宛先は生成されない(リンク自体は書かれるが、
    /// ビューアがクリックしても何も起きない)。
    pub anchor_names: HashMap<NodeId, String>,
    /// `<base href>`。外部リンクの相対URLをこれに対して解決する。
    pub base_href: Option<String>,
    /// 外部リンク(http(s))の注釈を出すか(`--disable-external-links`)。
    pub external: bool,
    /// 内部リンク(`#id`)の注釈を出すか(`--disable-internal-links`)。
    pub internal: bool,
    /// 相対URLを`<base href>`で絶対化せずそのまま書くか
    /// (`--keep-relative-links`)。
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
    /// 収集済みのリンク矩形から、無効化された種類のものを取り除く。
    pub(super) fn retain_enabled(&self, areas: &mut Vec<LinkArea>) {
        areas.retain(|area| {
            if internal_anchor_target(&area.href).is_some() {
                self.internal
            } else {
                self.external
            }
        });
    }

    /// 注釈へ渡す`<base href>`。`--keep-relative-links`のときは
    /// 解決に使わせないため`None`にする。
    pub(super) fn annotation_base_href(&self) -> Option<&str> {
        if self.keep_relative {
            None
        } else {
            self.base_href.as_deref()
        }
    }
}

/// PDFの`/Link`注釈1個分(ページ内の矩形+リンク先)。
///
/// 矩形はPDF座標系(左下原点、y上向き、ページ左下からの絶対座標)。
#[derive(Debug, Clone, PartialEq)]
pub(super) struct LinkArea {
    pub href: Rc<str>,
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// ページ内のボックスを走査し、`<a href>`に属するテキストランから
/// `/Link`注釈の矩形を集める。行内で同じリンクが連続するランは1つの
/// 矩形にまとめ、折り返しで別の行になった分は別の矩形にする。
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
        // ランのベースライン(`vertical-align`のずれ込み)を基準に、ascent〜
        // descentの範囲を注釈の高さにする。
        let baseline_y = to_pdf_y(settings, line.rect.y + line.baseline) + run.baseline_shift;
        let y0 = baseline_y - run.descent;
        let y1 = baseline_y + run.ascent;

        match &mut current {
            // 同じリンクが連続する間は1つの矩形へ広げる。
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

/// ページ内のボックスを走査し、アンカー対象(`anchor_names`に含まれる
/// `NodeId`)が最初に現れた位置(border box上端のPDF y座標)を集める。
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

/// `href`が内部アンカー(`#id`)なら、その`id`部分を返す。
pub(super) fn internal_anchor_target(href: &str) -> Option<&str> {
    href.strip_prefix('#').filter(|id| !id.is_empty())
}

/// ページ(群)を再帰的に走査し、`opacity < 1`の要素の`NodeId`を集める。
/// フォント・画像と同じ「使用状況を先に集めてからRefを払い出す」構造。
/// `opacity`要素は必ず実DOM要素に対応する(無名ボックスには`style`が
/// 付かないため`opacity`を持ちようがない)ので`b.node`は常に`Some`のはず。
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

/// [`collect_opacity_uses`]で払い出した`Ref`の、Form XObject用の固定
/// リソース名(画像の`image_resource_name`と同じパターン)。
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
    text_fonts: &TextFonts<'_>,
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
        text_fonts,
        image_ids,
        background_images,
        alpha_gs_names,
        opacity_form_ids,
        pending_forms,
    );
}

/// [`render_box`]の本体。通常は`b.node`から引いた素の`style`で描画するが、
/// `border-collapse: collapse`時のセル(隣接セルと統合した枠線を使う必要が
/// ある)のように、呼び出し側が上書きしたスタイルで描画したい場合のために
/// `style`を引数として分離してある。
///
/// `transform`が指定されている場合、実際の描画([`render_box_with_style_inner`])
/// をコンテンツストリームの`q cm ... Q`(CTM操作)で包む。レイアウトには
/// 一切影響しない(視覚効果のみ、CSS仕様通り)。
#[allow(clippy::too_many_arguments)]
fn render_box_with_style(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    text_fonts: &TextFonts<'_>,
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
            text_fonts,
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
        text_fonts,
        image_ids,
        background_images,
        alpha_gs_names,
        opacity_form_ids,
        pending_forms,
    );
    content.restore_state();
}

/// `opacity < 1`の場合、実際の描画([`render_box_with_style_inner`])を別の
/// `Content`へ書いてPDFの透明グループ(Form XObject + `/Group /S
/// /Transparency`)として`pending_forms`へ積み、元の`content`側には
/// `q /GSn gs /FoN Do Q`だけを書く。`transform`のCTM包みより内側で呼ぶ
/// 必要があるため、`render_box_with_style`から分離してある。
#[allow(clippy::too_many_arguments)]
fn render_box_opacity_wrapped(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    text_fonts: &TextFonts<'_>,
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
            text_fonts,
            image_ids,
            background_images,
            alpha_gs_names,
            opacity_form_ids,
            pending_forms,
        );
        return;
    }

    // `collect_opacity_uses`が事前にこのノードのRefを払い出し済みのはず
    // (`b.node`は必ず実DOM要素)。
    let form_ref = *b
        .node
        .and_then(|n| opacity_form_ids.get(&n))
        .expect("opacity < 1の要素は事前にRefが払い出されているはず");

    let mut sub_content = Content::new();
    let mut sub_target = content.wrap(&mut sub_content);
    render_box_with_style_inner(
        &mut sub_target,
        b,
        style,
        styles,
        fonts,
        settings,
        text_fonts,
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

/// `b`の`transform`/`transform-origin`から、PDFの`cm`オペランドを組み立てる。
/// CSS座標系(Y下向き)で関数を合成してから、まずPDF座標系(Y上向き)へ変換し
/// (この時点の平行移動成分は`translate`由来の相対量のままなので、Y成分の
/// 符号反転だけで正しく変換できる)、最後に`transform-origin`をPDF座標(ページ
/// 絶対座標)へ変換した上で基準点として適用する。原点の平行移動を先にCSS
/// 座標側で行ってしまうと、ページ高さのオフセットが変形行列の回転・拡大成分と
/// 混ざり込み誤った結果になるため、「原点調整はPDF座標に変換した後で行う」
/// という順序が重要。
fn transform_matrix_pdf_space(
    b: &LaidOutBox,
    style: &ComputedStyle,
    settings: &PageSettings,
) -> [f32; 6] {
    let border_box = b.layout.border_box();
    let css_matrix = compose_transform(&style.transform, border_box.width, border_box.height);
    let [a, b_, c, d, e, f] = css_matrix;
    // Y軸反転の共役変換。`translate`/`matrix`のe/fは常に相対量(ページ
    // 絶対座標ではない)なので、符号反転だけで正しくPDF座標系の相対量になる。
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

/// `transform-origin`(基準点`(ox, oy)`)を基準に`m`を適用できるよう、
/// `Translate(ox,oy) ∘ m ∘ Translate(-ox,-oy)`へ調整する。`m`と`(ox, oy)`は
/// 同じ座標系(このプロジェクトでは常にPDF座標系)で揃っている必要がある。
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

/// [`render_box_with_style`]の実体(装飾〜子要素再帰)。`transform`のCTM包み
/// より内側に置く必要があるため分離してある。
#[allow(clippy::too_many_arguments)]
fn render_box_with_style_inner(
    content: &mut RenderTarget<'_>,
    b: &LaidOutBox,
    style: &ComputedStyle,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    fonts: &FontCollection,
    settings: &PageSettings,
    text_fonts: &TextFonts<'_>,
    image_ids: &HashMap<usize, ImageIds>,
    background_images: &HashMap<NodeId, Rc<PreparedImage>>,
    alpha_gs_names: &[String],
    opacity_form_ids: &HashMap<NodeId, Ref>,
    pending_forms: &mut Vec<(Ref, Vec<u8>)>,
) {
    // `visibility: hidden`(`collapse`も同一視)。このボックス自身の装飾・
    // 内容は描画しないが、`Blocks`/`Table`の子要素へは引き続き再帰する(子孫が
    // `visibility: visible`で上書きしていれば、`render_box`が子自身の計算
    // スタイルを個別に評価し直すため正しく再描画される、仕様通り)。テーブルの
    // 場合は`border-collapse`統合描画の簡略化として通常の`render_box`で
    // 再帰する(隠れたテーブル内で特定セルだけ`visible`に上書きする、という
    // 稀なケースでは隣接セルとの枠線統合は行われない)。
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
                        text_fonts,
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
                        text_fonts,
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
                        text_fonts,
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
                            text_fonts,
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

    // `background-image`は`<img>`と異なりboxの中身ではなく装飾なので、
    // `b.node`から側マップを引いて`Ref`とintrinsicサイズを解決する。
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

    // `display: list-item`のマーカー。通常のテキスト行と同じ`render_line`を
    // 再利用する。
    if let Some(marker) = &b.marker {
        render_line(content, marker, fonts, settings, text_fonts, alpha_gs_names);
    }

    // `overflow: hidden`/`scroll`/`auto`(区別せず同じクリップとして扱う)。
    // 装飾(背景・枠線・outline・マーカー)は上で描画済みでクリップの影響を
    // 受けない。クリップ境界は常に直線の
    // padding-box (`border-radius`には沿わせない)。
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
                    text_fonts,
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
                    text_fonts,
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
                render_line(content, line, fonts, settings, text_fonts, alpha_gs_names);
                // 行内の`display: inline-block`は通常のブロックと同じ
                // 描画経路を通す(枠線・背景・中身のテキスト)。
                for atomic in &line.atomics {
                    render_box(
                        content,
                        &atomic.content,
                        styles,
                        fonts,
                        settings,
                        text_fonts,
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
                    text_fonts,
                    image_ids,
                    background_images,
                    alpha_gs_names,
                    opacity_form_ids,
                    pending_forms,
                );
            }
            // `border-collapse`は`table`/`inline-table`要素にのみ適用されるため
            // テーブル自身の`style`を見るが、`empty-cells`は`table-cell`要素に
            // 適用されるプロパティ(CSS2.1 17.6.1.1)なのでセル自身の計算済み
            // スタイルを見る必要がある(セル単位の上書きに対応するため)。
            // `empty-cells: hide`は`border-collapse: separate`でのみ意味を持つ
            // (CSS仕様通り)。内容が空のセルは元々装飾以外何も描画しないため、
            // `render_box`呼び出し自体をスキップしてよい。
            let collapse = style.border_collapse == BorderCollapse::Collapse;
            // `border-collapse: collapse`時、隣接セル間の枠線を統合するために
            // 全セルのフラットな一覧が要る
            // (隣接判定は矩形の接触で幾何的に行う)。
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
                        // 枠線の描画太さは`ComputedStyle`ではなく`layout.border`
                        // (レイアウト確定時に計算済み)を見るため
                        // ([`render_border`]参照)、統合後の太さを反映した
                        // クローンを作って描画する。
                        let mut resolved_cell = cell.clone();
                        resolved_cell.layout.border = resolved_border;
                        render_box_with_style(
                            content,
                            &resolved_cell,
                            &resolved_style,
                            styles,
                            fonts,
                            settings,
                            text_fonts,
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
                            text_fonts,
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

/// `children`を`z-index`とfloatに従って描画する順序へ並べ替える
/// (`(z-index, floatか, 文書順)`で安定ソート)。`position: static`の要素には
/// `z-index`が効果を持たない(仕様通り)ため実効値は常に`0`として扱う。
/// `sort_by_key`は安定ソートなので、キーが同じ要素同士は文書順が保たれる。
/// スタッキングコンテキストの分離は非対応(同一の直接の親を持つ兄弟間の
/// 描画順のみを制御する)。
///
/// floatを同じ`z-index`の通常フローのブロックより後(上)に描画するのは
/// CSS2.1 Appendix Eの規定による(ブロックの背景・枠線はfloatより前の
/// レイヤー)。これが無いと、floatの直後に背景色を持つブロックが来たときに
/// その背景がfloatを塗り潰してしまう。
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

/// セルの内容が空かどうか(テキストが空白のみ/子要素が無い)。`empty-cells:
/// hide`の判定に使う。ネストしたテーブル・置換要素(`<img>`)は常に非空扱い
/// (内容として意味を持つため)。
fn laid_content_is_empty(content: &LaidOutContent) -> bool {
    match content {
        // `<td>&nbsp;</td>`は「空のセル」ではない(枠を出すための定番の書き方)。
        // `str::trim`は`&nbsp;`も落としてしまうためCSSの分類で判定する。
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

/// `border-collapse: collapse`時、`cell`の枠線を隣接セルと統合した
/// `ComputedStyle`と、実際に描画する枠線の太さ(`layout.border`の差し替え用
/// `EdgeSizes`)を作る(枠線以外は`cell_style`のまま)。
///
/// レイアウト自体は`border-collapse`の値に関わらずseparateモデルと同一に
/// 保っているため、collapse時は`h_spacing`/`v_spacing`が0になり、隣接セルの
/// 矩形は座標が一致するまで接する。これを利用して、矩形の接触を幾何的に
/// 判定するだけでrowspan/colspanのグリッド
/// 情報を別途持たずに隣接セルを見つけられる。
///
/// 同じ境界が両側から二重に描画されるのを防ぐため、常に「左隣が見つかれば
/// 自分の左辺は描画しない(右隣側が統合済みの枠線を右辺として描画する)」
/// という向きで統一する(上/下も同様)。cellとtable自体の境界は対象外
/// (テーブル自身の枠線はborder-box外側の帯として描画されセルの矩形と
/// 重ならないため、二重描画の問題が生じない)。1つの辺に複数の隣接セルが接する
/// 場合(rowspanが絡む場合等)は、先に見つかったものを使う(既知の簡略化)。
///
/// 枠線の実際の描画太さは(`ComputedStyle`ではなく)レイアウト確定時に計算
/// 済みの`layout.border`(`EdgeSizes`)を見る([`render_border`]参照)ため、
/// 統合後の`ComputedStyle`だけでなく、それに対応する`EdgeSizes`
/// (`layout::resolve_border`と同じ正規化)も併せて返し、呼び出し側で
/// `cell.layout.border`を差し替えてもらう。
fn resolve_collapsed_cell_style(
    cell: &LaidOutBox,
    cell_style: &ComputedStyle,
    all_cells: &[&LaidOutBox],
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
) -> (ComputedStyle, EdgeSizes) {
    // 矩形の接触判定に許容する誤差(浮動小数点の丸め対策)。
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

/// `style: none`の辺は指定幅に関わらず実効幅0として扱う
/// (`layout::resolve_border`と同じ正規化、`resolve_border_conflict`の
/// 幅比較を単純にするため)。
fn border_edge(width: f32, style: BorderStyle, color: RgbaColor) -> (f32, BorderStyle, RgbaColor) {
    if style == BorderStyle::None {
        (0.0, BorderStyle::None, color)
    } else {
        (width, style, color)
    }
}

/// CSS2.1 §17.6.2の枠線競合解決の簡略版: 幅が太い方が勝ち、幅が同じなら
/// スタイルの優先順位(仕様通りの強さの見た目順: double > solid > dashed >
/// dotted > ridge > outset > groove > inset > none)で決める。`hidden`は
/// [`BorderStyle`]に無いため非対応。幅・スタイルとも同着の場合は`a`を採用する
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

/// 背景・枠線を描画する。角丸(`border-radius`)が指定されていなければ従来通り
/// 直線の矩形/4辺独立ストロークで描き、指定されていれば[`render_rounded_decoration`]
/// に委譲する。`background_image_ref`はborder-boxいっぱいにストレッチ表示する
/// 背景画像のXObject Ref(`border-radius`によるクリップは非対応)。背景色→
/// 背景画像→枠線の順で描画する。
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

/// ぼかし近似の段階数。
const BOX_SHADOW_BLUR_STEPS: u32 = 4;

/// `box-shadow`を描画する(要素本体の背景・枠線より前に呼ぶこと)。リストの
/// 先頭が最前面になるよう後ろから塗る。`inset`は非対応。
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

/// 1つの影を描く。ぼかしは真のガウスぼかしではなく、`spread-radius`分だけ
/// 広げたコア矩形の外側に、`blur-radius`まで均等`BOX_SHADOW_BLUR_STEPS`段階で
/// 広がる半透明の同心矩形を外側(最も広く・最も薄い)から内側(コアに近く・
/// 最も濃い)の順に重ね塗りして近似する。角丸は要素本体の半径(`radii`)
/// をそのまま使い、拡大に応じて広げない。
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
        // 既知の簡略化: `spread-radius`が負で矩形が縮退する場合はそのリングを
        // 描画しない(ゼロ・負サイズの矩形は無意味なため)。
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
    // コア(spreadのみ、フルアルファ)を最後に重ね、blur-radius: 0の場合と
    // 輪郭が確実に一致するようにする。
    draw(content, shadow.spread_radius, shadow.color.alpha);
}

/// 1コーナー分の実効半径(水平, 垂直)のpx値。真円は水平=垂直。
type CornerRadiusPx = (f32, f32);

/// スタイル上の`border-radius`を、そのボックスがページ分割された断片の
/// どの位置にあるか([`FragmentPosition`])に応じて丸める。継続中の断片
/// (`Middle`/上端なら`Last`/下端なら`First`)では、本来枠線が無い辺の角を
/// 丸めてしまわないよう、その辺に接する角の半径を0にする。
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

/// アルファ量子化の段階数(0.05刻み・21段階)。
pub(super) const ALPHA_STEPS: usize = 20;

/// アルファ値を`0..=ALPHA_STEPS`の段階(0.05刻み)へ丸める。
fn quantize_alpha_step(alpha: f32) -> usize {
    (alpha.clamp(0.0, 1.0) * ALPHA_STEPS as f32).round() as usize
}

/// `alpha_gs_names`(`ALPHA_STEPS + 1`要素、段階インデックスで引く)の
/// 固定リソース名(`"GSA{段階}"`)。
pub(super) fn alpha_gs_resource_name(step: usize) -> String {
    format!("GSA{step}")
}

/// アルファ値に応じて`gs`演算子(`/ca`・`/CA`)を発行する。1.0(完全不透明)は
/// 何もしない(PDFの既定状態のため)。呼び出し側が
/// `save_state`/`restore_state`でスコープを囲むこと。
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

/// `rect`いっぱいに画像XObjectを描画する。`<img>`(content box)・
/// `background-image`(タイル1枚分の矩形、[`background_tile_rects`]参照)
/// いずれの呼び出し元からも使う共通ヘルパー。`resource_ref`が指すXObjectは、
/// 呼び出し元がページの`/Resources/XObject`辞書へ既に登録済みであること
/// ([`image_resource_name`]と同じ命名規則でリソース名を導出する)。
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

/// `<img>`(置換要素)を`object-fit`/`object-position`に従って描画する。
/// `object-fit`の値によらず常にcontent-boxへクリップする。
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

/// `object-fit`/`object-position`から実際に描画すべき画像の矩形(content-box
/// 基準の座標系、レイアウト空間)を計算する。intrinsicサイズが縮退している
/// 場合はcontent-box全体への単純な描画にフォールバックする
/// (`background_tile_rects`と同じゼロ除算回避)。
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
        // 仕様通り`none`と`contain`のうち小さい方。
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

/// `background-image`の描画に必要な情報。`render_box`が`b.node`から
/// 側マップ(`background_images`)経由で解決する。
#[derive(Debug, Clone, Copy)]
struct BackgroundImagePaint {
    resource: Ref,
    /// 内在サイズ(px)。SVGでは小数になりうる([`PreparedImage`]参照)。
    intrinsic_width: f32,
    intrinsic_height: f32,
}

/// `background-size`/`-position`/`-repeat`から、実際に描画すべき画像タイルの
/// 矩形群(border-box基準の座標系、レイアウト空間)を計算する。intrinsic
/// サイズが縮退している(0を含む)場合はborder-box全体への単純な1枚描画に
/// フォールバックする(ゼロ除算回避)。
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

/// `background-size`の1軸分の指定値を、`auto`なら`None`(アスペクト比から
/// 導出させる)、それ以外は`basis`(border-boxの対応する辺)基準のpx値へ解決する。
fn resolve_background_size_component(value: LengthPercentageOrAuto, basis: f32) -> Option<f32> {
    match value {
        LengthPercentageOrAuto::Auto => None,
        LengthPercentageOrAuto::LengthPercentage(lp) => Some(resolve_length_percentage(lp, basis)),
    }
}

/// `background-position`の1軸分の計算値から、border-box原点からのオフセット
/// (px)を求める。パーセンテージは`(コンテナ - タイル)`に対する割合
/// (CSS仕様通りの式)、長さはそのまま原点からのオフセットとして使う。
fn resolve_background_position_offset(value: LengthPercentage, container: f32, tile: f32) -> f32 {
    match value {
        LengthPercentage::Length(l) => l,
        LengthPercentage::Percentage(p) => (container - tile) * p,
        LengthPercentage::Calc { px, percent } => px + (container - tile) * percent,
    }
}

/// 1軸分のタイル開始座標を列挙する。`repeat`が偽、またはタイル幅が0以下なら
/// `origin`の1枚のみ。それ以外は`[min, max)`(border-boxの範囲)を覆うのに
/// 必要な分だけ`origin`から`tile`間隔で並べる。防御的に1軸あたり200枚を
/// 超えたら打ち切る(病的な小さい`background-size`に対するフェイルセーフ)。
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

/// [`background_tile_rects`]で計算した矩形群を描画する。タイルが1枚かつ
/// border-boxとちょうど一致する場合(`background-repeat: no-repeat`+
/// `background-size`未指定でも十分収まる等)を除き、border-boxへのクリップ
/// (`overflow`クリップと同じパターン)を挟んでタイルがboxからはみ出さない
/// ようにする。
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

/// `<tr>`(`display: table-row`)の`background-color`を、その行のセル群を
/// 覆う矩形として塗る。
///
/// 行ボックスは`LaidOutTableRow`にジオメトリを持たないため、行に属するセルの
/// border boxの和集合を行の矩形とみなす(`border-spacing`がある場合、セル間の
/// 隙間も行の背景で塗られる。これはCSS2.1 17.5.1の描画順=行の背景がセルの
/// 背景の下に敷かれる、という規定と同じ見え方になる)。CSSの
/// `tr { background-color: ... }`とレガシー表示属性の`<tr bgcolor>`のどちらも
/// この経路で描画される。
fn render_row_background(
    content: &mut RenderTarget<'_>,
    row: &LaidOutTableRow,
    styles: &HashMap<NodeId, Rc<ComputedStyle>>,
    settings: &PageSettings,
    alpha_gs_names: &[String],
) {
    // 無名行(`node: None`)は背景を指定しようがないので何も描かない。
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

/// `border-radius`が指定されている場合の背景・枠線描画。
///
/// 背景は各角の半径に従った角丸矩形として塗りつぶす。枠線は、4辺すべての
/// 太さ・スタイル・色が同一の場合のみ角丸パスをストロークする
/// (辺ごとに異なる太さ・色・スタイルと角丸の組み合わせは、角での複雑な
/// ブレンド処理が必要になるため非対応。その場合は角丸を諦め、
/// 直線4辺の[`render_border`]にフォールバックする)。
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
    // 角丸パスへのクリップは行わず、常に直線の矩形として描画する
    // (border-radiusとの組み合わせは非対応)。
    if let Some(paint) = background_image_paint {
        render_background_image(content, border_box, style, settings, &paint);
    }

    // groove/ridge/inset/outsetは辺ごとの陰影が必要で、角丸パスの単純な
    // ストロークでは表現できないため、常に直線4辺へフォールバックする
    // (既存の「4辺不揃い+角丸」フォールバックと同じパターン)。
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
        // 太さを3等分し、外周から1/6・5/6の位置(それぞれの帯の中心線)に
        // 1/3幅の角丸パスを2本ストロークする(中央の1/3は空白として残る)。
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

    // ストロークは太さの中心線を通るため、外周パスを半分だけ内側へ詰める
    // (半径も同じ量だけ縮める簡易近似)。
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

/// 4辺すべての`border-width`/`border-style`/`border-color`が一致するか。
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

/// 四分円をベジェ曲線で近似する際の制御点オフセット係数。
const BEZIER_KAPPA: f32 = 0.552_284_8;

/// PDF空間(Y-up、`y_top` > `y_bottom`)で角丸矩形のパスを構築して閉じる
/// (塗り/ストロークは呼び出し側が行う)。半径は`(top_left, top_right,
/// bottom_right, bottom_left)`の順(CSSの`border-radius`と同じ並び)、各コーナー
/// は`(水平半径, 垂直半径)`のペア(楕円コーナー対応)。
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

/// `outline`を描く。`border`と違いレイアウトに一切影響しないため、`layout`は
/// 参照するだけで書き換えない。border-boxの外側にoutline-widthの太さで
/// 描画する点だけが`render_border`と異なり、それ以外(4辺の頂点構成・
/// `render_border_side`への委譲)は全く同じ仕組みを再利用する。
/// `outline-offset`(outlineとborder-boxの間隔)は非対応、常に0固定。
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

    // outlineの内側の辺(border-boxそのもの)。
    let tl_inner = (x0, y_top);
    let tr_inner = (x1, y_top);
    let br_inner = (x1, y_bottom);
    let bl_inner = (x0, y_bottom);
    // outlineの外側の辺(border-boxから`t`だけ外側へ張り出す)。
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

/// 4辺それぞれの`border-width`/`border-style`/`border-color`に従って枠線を描く。
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

/// 辺の識別子。`groove`/`ridge`/`inset`/`outset`の陰影が上・左辺と下・右辺で
/// 異なる色になるため必要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BorderSideKind {
    Top,
    Right,
    Bottom,
    Left,
}

/// RGB各成分をwhiteへ`amount`だけブレンドして明るくする(簡易実装、正確な色
/// 再現は目指さない)。
fn lighten(color: RgbaColor, amount: f32) -> RgbaColor {
    let mix = |c: u8| (c as f32 + (255.0 - c as f32) * amount).round() as u8;
    RgbaColor {
        red: mix(color.red),
        green: mix(color.green),
        blue: mix(color.blue),
        alpha: color.alpha,
    }
}

/// RGB各成分をblackへ`amount`だけブレンドして暗くする。
fn darken(color: RgbaColor, amount: f32) -> RgbaColor {
    let mix = |c: u8| (c as f32 * (1.0 - amount)).round() as u8;
    RgbaColor {
        red: mix(color.red),
        green: mix(color.green),
        blue: mix(color.blue),
        alpha: color.alpha,
    }
}

/// `groove`/`ridge`/`inset`/`outset`の明暗ブレンド比率。
const SHADE_AMOUNT: f32 = 0.35;

/// 1辺分の外側寄り帯・内側寄り帯それぞれの実効描画色。`solid`/`dashed`/
/// `dotted`(呼び出し元で個別処理する`double`含む)は帯間で同色。
struct BorderSideColors {
    outer: RgbaColor,
    inner: RgbaColor,
}

/// `border_style`/`side`から実効描画色を決める。光源は左上からと仮定する(CSS
/// 仕様の一般的な慣習): `inset`は上・左辺が暗色、下・右辺が明色(押し込まれた
/// 凹み)。`outset`はその逆(浮き出た凸み)。`groove`/`ridge`は各辺の太さを2
/// 等分し、外側帯・内側帯に異なる色を割り
/// 当てることで溝/稜線の視覚効果を出す。
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

/// 1辺分の枠線を構成する4頂点。`outer_a`→`outer_b`が外形の辺、
/// `inner_b`→`inner_a`が内形の辺(`outer_b`/`inner_b`が隣の辺と共有する角)。
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

/// 1辺分の枠線を描く。
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
            // 太さを2等分し、外側帯・内側帯をそれぞれ`border_side_colors`が
            // 決めた色で塗る(`inset`/`outset`は外側=内側で同色になり、結果的に
            // 1色の`Solid`と同じ見た目になる)。
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
            // 太さを3等分し、外側1/3・内側1/3それぞれをミトー結合済みの帯として
            // 塗る(中央の1/3は空白として残る)。外形/内形の頂点間を線形補間して
            // 各帯の境界を求める(辺ごとに太さが異なっていても、隣接辺との
            // 境界は共有する頂点から計算されるため引き続き綺麗に合う)。
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
            // ダッシュパターンはストロークでのみ表現できるため、太さの中心線を
            // 従来通りストロークする(ミトー結合はしない)。
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

/// 単純な実線を太さ・色を指定してストロークする(text-decorationの下線・
/// 取り消し線用)。
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

/// 4頂点(a→b→c→d→閉じる)の四角形パスを構築して塗りつぶす。
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

/// `border-style`に応じたダッシュパターン/線キャップを設定する。
/// `Double`は2本ストロークする専用処理(呼び出し側)で扱うためここには来ない。
/// `Groove`/`Ridge`/`Inset`/`Outset`は角丸パスのストロークでは表現できず
/// 常に直線4辺へフォールバックするためここには来ないが、`match`を網羅するため
/// `Solid`と同じ扱いにしておく。
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
            // 長さ0の破線+丸キャップで点線を表現する(PDFの定石)。
            content.set_line_cap(LineCapStyle::RoundCap);
            content.set_dash_pattern([0.01, thickness * 2.0], 0.0);
        }
        BorderStyle::None => {}
    }
}

/// 疑似イタリック(シアー変形)の傾斜角(12度)。埋め込みフォントに本物の
/// イタリック字形がない前提で、テキスト行列をせん断することで代用する。
const ITALIC_SHEAR: f32 = 0.2126; // tan(12°)
/// 疑似ボールド(塗り+縁取り)の線幅を、フォントサイズに対する比率で表す。
const BOLD_STROKE_RATIO: f32 = 0.03;

/// A stretch of a run that can be drawn with one PDF font.
///
/// Even within a single run the destination can change from glyph to glyph: a
/// colour emoji goes to a Type 3 font and everything else to the ordinary
/// Type0 font, and the two differ in both resource name and code width, so
/// each switch needs a fresh `Tf` and `Tm`.
///
/// `x` is relative to the run's origin, in px. `letter-spacing` is added
/// separately through `Tc`, but it still has to be counted into the pen
/// advance to find where a segment starts.
struct RunSegment<'a> {
    x: f32,
    /// Where the segment's first glyph goes; its resource name stands for the
    /// whole segment.
    first: GlyphTarget<'a>,
    /// The glyphs in this segment, as indices into `run.glyphs`.
    range: std::ops::Range<usize>,
}

/// Cut `run` into [`RunSegment`]s. An undrawable glyph
/// ([`GlyphTarget::Dropped`]) joins no segment; only the pen advances past it.
///
/// With `include_color` false, colour glyphs are dropped too. That is for
/// `text-shadow`, where redrawing the full-colour artwork at an offset would
/// not be a shadow.
fn run_segments<'a>(
    run: &TextRun,
    text_fonts: &'a TextFonts<'a>,
    include_color: bool,
) -> Vec<RunSegment<'a>> {
    let mut segments: Vec<RunSegment<'a>> = Vec::new();
    let mut x = 0.0;
    for (index, glyph) in run.glyphs.iter().enumerate() {
        let target = match text_fonts.target(run.font_index, glyph.glyph_id) {
            GlyphTarget::Color { .. } if !include_color => GlyphTarget::Dropped,
            target => target,
        };
        if target != GlyphTarget::Dropped {
            match segments.last_mut() {
                Some(last)
                    if last.range.end == index
                        && last.first.resource_name() == target.resource_name() =>
                {
                    last.range.end = index + 1;
                }
                _ => segments.push(RunSegment {
                    x,
                    first: target,
                    range: index..index + 1,
                }),
            }
        }
        x += glyph.x_advance + run.letter_spacing;
    }
    segments
}

/// The smallest advance-width discrepancy worth correcting, in px. Below this
/// a correction only inflates the TJ array without being visible.
const ADVANCE_EPSILON: f32 = 0.01;

/// Write out one segment's glyphs, inserting a TJ correction after any glyph
/// whose advance disagrees with the font's own width.
///
/// The width by which a PDF advances past a glyph comes from the font's width
/// information (a CIDFont's `/W`, a Type 3 font's `/Widths`), which can hold
/// only one value per glyph ID. Layout, meanwhile, uses the `x_advance` the
/// shaper returned, and the two need not agree.
///
/// * `merge_adjacent_runs` restores a word space as "a space glyph carrying
///   the gap's advance". A gap widened by `text-align: justify` is not the
///   space's own width, so without a correction a justified line falls short
///   of the right margin by however much it was stretched.
/// * For a fixed-width space the font lacks (`&thinsp;` and friends) the
///   shaper substitutes the space glyph but overrides the advance to the
///   prescribed value (em/5 and so on). A plain space uses the same glyph, so
///   the width information can only express one of the two.
///
/// The difference is made up with TJ array corrections. TJ numbers are in
/// thousandths of a text space unit and are subtracted from the advance
/// (positive tightens), so widening takes a negative value. `letter-spacing`
/// is added separately through `Tc` and so is left out of this difference.
fn show_segment_glyphs(
    content: &mut RenderTarget<'_>,
    run: &TextRun,
    font: &Font,
    text_fonts: &TextFonts<'_>,
    segment: &RunSegment<'_>,
) {
    let glyphs = &run.glyphs[segment.range.clone()];
    let code_of = |glyph_id: u16| text_fonts.target(run.font_index, glyph_id).code_bytes();

    let units_per_em = font.units_per_em() as f32;
    // フォントサイズ0のランは補正のしようがない(1/1000単位への換算ができない)。
    if run.font_size <= 0.0 || units_per_em <= 0.0 {
        let mut glyph_bytes = Vec::with_capacity(glyphs.len() * 2);
        for glyph in glyphs {
            glyph_bytes.extend_from_slice(&code_of(glyph.glyph_id));
        }
        content.show(pdf_writer::Str(&glyph_bytes));
        return;
    }

    let mut positioned = content.show_positioned();
    let mut items = positioned.items();
    // 補正の要らないグリフはまとめて1つの文字列として出す(補正が1つも無ければ
    // 要素1つのTJ配列になり、`Tj`と同じ大きさに収まる)。
    let mut pending = Vec::with_capacity(glyphs.len() * 2);
    for glyph in glyphs {
        pending.extend_from_slice(&code_of(glyph.glyph_id));
        let pdf_advance = font.glyph_hor_advance(glyph.glyph_id).unwrap_or(0) as f32
            * run.font_size
            / units_per_em;
        let delta = glyph.x_advance - pdf_advance;
        if delta.abs() < ADVANCE_EPSILON {
            continue;
        }
        items.show(pdf_writer::Str(&pending));
        pending.clear();
        // 小数第2位までに丸める。両端揃えの文書では単語間のすべての隙間に補正が
        // 入るため、`f32`をそのまま書くとコンテンツストリームがおよそ1割膨らむ。
        // 1/1000単位の0.01は12ptで0.00012pxなので、見た目には効かない。
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
    text_fonts: &TextFonts<'_>,
    alpha_gs_names: &[String],
) {
    if line.runs.is_empty() {
        return;
    }

    // 行のベースライン位置はレイアウト時に確定済み。各ランは`vertical-align`
    // 由来の`baseline_shift`(正=上)だけそこからずれる。
    let baseline_y = to_pdf_y(settings, line.rect.y + line.baseline);

    // インライン要素の背景(`<mark>`等)は、ランのascent〜descentの矩形として
    // テキストより先に塗る。ブロックの背景([`render_decoration`])と違い
    // ボーダーボックスを持たないため、フォントメトリクスで代用する。
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

    // `text-shadow`はテキスト本体より先に描く。
    render_text_shadows(
        content,
        line,
        fonts,
        settings,
        text_fonts,
        alpha_gs_names,
        baseline_y,
    );

    content.begin_text();

    // ランどうしの間に、実際のグリフ幅の合計を超える隙間があれば単語境界
    // (=空白1文字分)とみなす。単語内でスタイル/フォントが切り替わる場合の
    // ラン境界は隙間0で連続しているため、ここでは誤って空白扱いにならない。
    const WORD_GAP_EPSILON: f32 = 0.01;
    let mut previous_run_end: Option<f32> = None;

    for run in &line.runs {
        if run.glyphs.is_empty() {
            continue;
        }

        // 単語間の空白は、レイアウト上は隙間(x_offsetの加算)としてのみ表現され、
        // どの`TextRun.text`にも実際の空白文字を含めていない(フォント混在時の
        // グリフ幅計測を単純にするため)。そのままではPDFからのテキスト抽出時、
        // 特にフォント(リソース名)が切り替わるラン境界で空白が失われることが
        // あるため、見た目に影響しない`ActualText`付きの空マーク付きコンテンツ
        // 区間を挿入し、抽出用にスペースの存在を明示する。
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
            // 枠線描画がダッシュパターン/丸キャップを残している場合があるため、
            // テキストの縁取りには影響しないよう明示的に実線・矩形キャップへ戻す。
            content.set_line_cap(LineCapStyle::ButtCap);
            content.set_dash_pattern([], 0.0);
            content.set_text_rendering_mode(TextRenderingMode::FillStroke);
        } else {
            content.set_text_rendering_mode(TextRenderingMode::Fill);
        }

        let x = settings.margin.left + line.rect.x + run.x_offset;
        let shear = if run.italic { ITALIC_SHEAR } else { 0.0 };
        // `letter-spacing`はグリフ幅そのもの(フォントの`/Widths`)には反映
        // できないため、PDFの`Tc`(character spacing)を使う。`Tw`(word
        // spacing)と異なり複合フォント(2バイトCID)にも適用される。0でも
        // 明示的に設定し、前のランの値が
        // グラフィックステートに残らないようにする。
        content.set_char_spacing(run.letter_spacing);
        // Colour glyphs and the rest use different PDF fonts even inside one
        // run, so emit a fresh `Tf` and `Tm` per segment. The `Tm` is written
        // absolute, as the run origin plus the segment's offset within it.
        for segment in run_segments(run, text_fonts, true) {
            let Some(resource_name) = segment.first.resource_name() else {
                continue;
            };
            content.set_font(Name(resource_name.as_bytes()), run.font_size);
            content.set_text_matrix([
                1.0,
                0.0,
                shear,
                1.0,
                x + segment.x,
                baseline_y + run.baseline_shift,
            ]);
            show_segment_glyphs(content, run, font, text_fonts, &segment);
        }
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
        // 装飾線もそのランのベースライン(=行のベースライン+`vertical-align`の
        // ずれ)を基準に引く。
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

    // `text-emphasis`のマークは装飾線と同じくテキスト本体の後に描く。
    render_emphasis_marks(content, line, fonts, settings, text_fonts, baseline_y);
}

/// `text-emphasis`のマークを描く。
/// `dot`/`circle`/`double-circle`/`triangle`/`sesame`はフォントの字形に
/// 依存しないようパスで描き、`<string>`指定だけはグリフとして描く。マークは
/// 空白でない文字1つごとに1個置く(`text-emphasis-skip`は非対応)。
#[allow(clippy::too_many_arguments)]
fn render_emphasis_marks(
    content: &mut RenderTarget<'_>,
    line: &LineBox,
    fonts: &FontCollection,
    settings: &PageSettings,
    text_fonts: &TextFonts<'_>,
    baseline_y: f32,
) {
    for run in &line.runs {
        let Some(mark) = &run.emphasis else {
            continue;
        };
        let run_baseline_y = baseline_y + run.baseline_shift;
        // マーク分の高さは`ascent`/`descent`に
        // 加算済み。その帯の中央にマークを置く。
        let center_y = match mark.position {
            EmphasisPosition::Over => run_baseline_y + run.ascent - mark.size / 2.0,
            EmphasisPosition::Under => run_baseline_y - run.descent + mark.size / 2.0,
        };

        let mut x = settings.margin.left + line.rect.x + run.x_offset;
        for glyph in &run.glyphs {
            let advance = glyph.x_advance + run.letter_spacing;
            // 空白文字にはマークを付けない(仕様の"skip: spaces"相当)。
            // `cluster`はランのテキスト内バイトオフセットだが、範囲外でも
            // panicしないよう`get`で引く。
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
                    text_fonts,
                );
            }
            x += advance;
        }
    }
}

/// マーク1つ分を`(center_x, center_y)`を中心に描く。
#[allow(clippy::too_many_arguments)]
fn render_emphasis_mark(
    content: &mut RenderTarget<'_>,
    mark: &EmphasisMark,
    center_x: f32,
    center_y: f32,
    run: &TextRun,
    fonts: &FontCollection,
    text_fonts: &TextFonts<'_>,
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
                content, *ch, center_x, center_y, mark, run, fonts, text_fonts,
            );
            return;
        }
    };

    content.set_fill_rgb(r, g, b);
    content.set_stroke_rgb(r, g, b);
    // 輪郭のみ(`open`)の線幅はマークサイズに比例させる。
    let stroke_width = (mark.size * 0.08).max(0.3);
    content.set_line_width(stroke_width);
    content.set_line_cap(LineCapStyle::ButtCap);
    content.set_dash_pattern([], 0.0);

    match shape {
        // `dot`は小さめ、`circle`は大きめ(仕様に厳密な寸法規定は無いため、
        // 一般的なブラウザの見た目に近い比率を採用する)。
        EmphasisShape::Dot => {
            circle_path(content, center_x, center_y, mark.size * 0.16);
            finish_mark_path(content, filled);
        }
        EmphasisShape::Circle => {
            circle_path(content, center_x, center_y, mark.size * 0.3);
            finish_mark_path(content, filled);
        }
        EmphasisShape::DoubleCircle => {
            // 二重丸(◉/◎)は外側を常に輪郭で描く。外側を塗ってしまうと
            // 内側が潰れて単なる丸に見える。
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
        // `sesame`(ゴマ点)は縦長の楕円で近似する。
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

/// `text-emphasis-style: <string>`のマークを、そのランのフォントのグリフとして描く。
/// 字形を持たないフォントでは何も描かれない。
#[allow(clippy::too_many_arguments)]
fn render_emphasis_glyph(
    content: &mut RenderTarget<'_>,
    ch: char,
    center_x: f32,
    center_y: f32,
    mark: &EmphasisMark,
    run: &TextRun,
    fonts: &FontCollection,
    text_fonts: &TextFonts<'_>,
) {
    let Some(glyph_id) = fonts.get(run.font_index).and_then(|font| font.glyph_id(ch)) else {
        return;
    };
    // A mark is a single glyph, so there is nothing to segment. A colour
    // glyph works here through the same mechanism.
    let target = text_fonts.target(run.font_index, glyph_id);
    let Some(resource_name) = target.resource_name() else {
        return;
    };
    let code = target.code_bytes();
    if code.iter().all(|&b| b == 0) {
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
    // マークサイズを1emとみなし、中心に来るよう左下へずらす。
    content.set_text_matrix([
        1.0,
        0.0,
        0.0,
        1.0,
        center_x - mark.size / 2.0,
        center_y - mark.size / 2.0,
    ]);
    content.show(pdf_writer::Str(&code));
    content.end_text();
}

/// マークのパスを塗る(`filled`)か輪郭を描く(`open`)。
fn finish_mark_path(content: &mut RenderTarget<'_>, filled: bool) {
    if filled {
        content.fill_nonzero();
    } else {
        content.stroke();
    }
}

/// 中心と半径から真円のパスを引く(4本のベジェ曲線で近似)。
fn circle_path(content: &mut RenderTarget<'_>, cx: f32, cy: f32, r: f32) {
    ellipse_path(content, cx, cy, r, r);
}

/// 中心と水平/垂直半径から楕円のパスを引く。
fn ellipse_path(content: &mut RenderTarget<'_>, cx: f32, cy: f32, rx: f32, ry: f32) {
    let (kx, ky) = (rx * BEZIER_KAPPA, ry * BEZIER_KAPPA);
    content.move_to(cx + rx, cy);
    content.cubic_to(cx + rx, cy + ky, cx + kx, cy + ry, cx, cy + ry);
    content.cubic_to(cx - kx, cy + ry, cx - rx, cy + ky, cx - rx, cy);
    content.cubic_to(cx - rx, cy - ky, cx - kx, cy - ry, cx, cy - ry);
    content.cubic_to(cx + kx, cy - ry, cx + rx, cy - ky, cx + rx, cy);
    content.close_path();
}

/// `text-shadow`のぼかし近似の段階数。中心+この段階数×4方向を重ね描きする。
const TEXT_SHADOW_BLUR_STEPS: usize = 2;

/// `text-shadow`を描く(テキスト本体より先に呼ぶこと)。PDFにはぼかしフィルタが
/// 無いため、アルファを下げた同じグリフ列を微小オフセットで重ね描きして
/// 近似する。カンマ区切りの複数指定は後ろに書いたものほど奥に描く。
#[allow(clippy::too_many_arguments)]
fn render_text_shadows(
    content: &mut RenderTarget<'_>,
    line: &LineBox,
    fonts: &FontCollection,
    settings: &PageSettings,
    text_fonts: &TextFonts<'_>,
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
        // 影は本体と同じグリフ列なので、送り幅の補正も同じでなければずれる。
        let Some(font) = fonts.get(run.font_index) else {
            continue;
        };
        // Colour glyphs cast no shadow: all that would happen is the artwork
        // itself being redrawn at the shadow's offset.
        let segments = run_segments(run, text_fonts, false);
        if segments.is_empty() {
            continue;
        }

        let x = settings.margin.left + line.rect.x + run.x_offset;
        let run_baseline_y = baseline_y + run.baseline_shift;
        let shear = if run.italic { ITALIC_SHEAR } else { 0.0 };

        // 先頭が最前面 = 後ろに書いたものほど奥。奥から順に描く。
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
                content.set_char_spacing(run.letter_spacing);
                for segment in &segments {
                    let Some(resource_name) = segment.first.resource_name() else {
                        continue;
                    };
                    content.set_font(Name(resource_name.as_bytes()), run.font_size);
                    // CSSのoffset-yは下向き正、PDFのYは上向き正。
                    content.set_text_matrix([
                        1.0,
                        0.0,
                        shear,
                        1.0,
                        x + segment.x + shadow.offset_x + dx,
                        run_baseline_y - shadow.offset_y - dy,
                    ]);
                    show_segment_glyphs(content, run, font, text_fonts, segment);
                }
                content.end_text();
                content.restore_state();
            }
        }
    }
}

/// ぼかし近似のオフセット列(`(dx, dy, アルファ倍率)`)。`blur_radius`が0なら
/// 中心1回だけ。それ以外は中心+各段階の4方向を、
/// 合計のアルファが概ね1になるよう分配する。
fn shadow_blur_offsets(blur_radius: f32) -> Vec<(f32, f32, f32)> {
    if blur_radius <= 0.0 {
        return vec![(0.0, 0.0, 1.0)];
    }
    let mut offsets = Vec::with_capacity(1 + TEXT_SHADOW_BLUR_STEPS * 4);
    let count = 1 + TEXT_SHADOW_BLUR_STEPS * 4;
    let alpha_scale = 1.0 / count as f32;
    offsets.push((0.0, 0.0, alpha_scale));
    for step in 1..=TEXT_SHADOW_BLUR_STEPS {
        // ぼかし半径の内側から外側へ均等に配置する。
        let r = blur_radius * step as f32 / TEXT_SHADOW_BLUR_STEPS as f32;
        offsets.push((r, 0.0, alpha_scale));
        offsets.push((-r, 0.0, alpha_scale));
        offsets.push((0.0, r, alpha_scale));
        offsets.push((0.0, -r, alpha_scale));
    }
    offsets
}

/// 量子化後に完全透明になる(=描いても見えない)アルファかどうか。
fn quantize_alpha_step_is_transparent(alpha: f32) -> bool {
    quantize_alpha_step(alpha) == 0
}

/// フォントの`post`(下線)/`OS2`(取り消し線)テーブルから、ベースラインからの
/// 符号付きオフセットと線の太さをpx単位で求める。テーブルを持たないフォントでは
/// `fallback_ratio`(フォントサイズに対する比率)をアセント基準の位置として使う。
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

/// ページコンテンツ領域上端からの距離(CSSのY、下向き正)を、PDFのユーザー空間の
/// Y座標(ページ物理下端からの距離、上向き正)に変換する。
fn to_pdf_y(settings: &PageSettings, y_from_content_top: f32) -> f32 {
    settings.size.height - settings.margin.top - y_from_content_top
}

/// `@page`のmargin box(`@top-left`等、16個)の水平/垂直方向の内容配置。
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

/// margin box 1つ分の矩形と、内容の配置規則を返す。
///
/// 座標系は`render_line`/`render_image`と同じ「content area(padding/borderの
/// 内側ではなく、ページの余白の内側=`settings.margin`の内側)基準の相対座標」
/// (`render_line`が`settings.margin.left + line.rect.x`・`to_pdf_y`が
/// `settings.size.height - settings.margin.top - y`という式でPDF座標へ変換する
/// 前提に合わせる必要がある)。margin boxはこのcontent areaの外側にあるため、
/// x/yが負の値やcontent_width/content_heightを超える値になるのが正しい。
///
/// 角の4boxは固定サイズ、残り12boxは辺のマージン幅を3等分する簡易配分
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

/// margin boxの宣言リストから、シェイピングに必要な最小限のスタイルを
/// 組み立てる(`font-*`/`color`のみ、`ComputedStyle::default`を基点に
/// 上書きする)。margin boxはDOM要素を持たないためカスケード・継承は行わない。
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

/// ページの余白領域へ重ねて描くサブドキュメント(`--header-html`/
/// `--footer-html`)。
///
/// レイアウト済みのボックス列と、その描画基準となる`PageSettings`を持つ。
/// 基準を余白領域に合わせた専用の`PageSettings`にすることで、既存の
/// `render_box`(y座標を`settings`から換算する)をそのまま使える。
#[derive(Clone)]
pub struct PageOverlay {
    pub boxes: Vec<LaidOutBox>,
    pub styles: HashMap<NodeId, Rc<ComputedStyle>>,
    /// 余白領域を基準にした描画用の設定。
    pub settings: PageSettings,
    /// はみ出しを切るクリップ矩形(CSS px・ページ左上原点)。
    pub clip: Rect,
}

/// [`PageOverlay`]をページのcontent streamへ描く。
pub(super) fn render_page_overlay(
    content: &mut RenderTarget<'_>,
    overlay: &PageOverlay,
    fonts: &FontCollection,
    text_fonts: &TextFonts<'_>,
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
    // 余白からはみ出した分は切る(マージンの自動拡張はしない)。
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
            text_fonts,
            &empty_image_ids,
            &empty_images,
            alpha_gs_names,
            &empty_form_ids,
            &mut pending_forms,
        );
    }
    content.restore_state();
}

/// `--header-line`/`--footer-line`の罫線を引く。
///
/// margin boxは装飾(枠線)非対応のため、ページ描画時に水平線として直接引く。
/// 位置はコンテンツ領域の上端(ヘッダー)と下端(フッター)。
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

/// このページで実際に描画すべきmargin boxを、`content`が空でないものだけ
/// シェイピング済みの状態で返す。描画(`render_margin_boxes`)・使用グリフ
/// 収集(`collect_margin_box_usage`)の両方から呼ばれる共通処理。
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

/// 確定した`shape_margin_boxes_for_page`の結果を、実際にコンテンツ
/// ストリームへ描画する(alignmentに応じて原点を配置してから`render_line`を
/// 再利用する)。
#[allow(clippy::too_many_arguments)]
pub(super) fn render_margin_boxes(
    content: &mut RenderTarget<'_>,
    settings: &PageSettings,
    fonts: &FontCollection,
    page_rules: &[PageRule],
    page_number: usize,
    total_pages: Option<usize>,
    text_fonts: &TextFonts<'_>,
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
        render_line(content, &line, fonts, settings, text_fonts, &[]);
    }
}

/// margin boxが使うグリフをフォントサブセット化のために集める
/// (`render_margin_boxes`と同じ`shape_margin_boxes_for_page`を再利用)。
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
        // A4相当、margin 80/60(上下/左右)を想定。座標系は`render_line`と同じ
        // content area相対(margin boxはこの外側にあるため負・content超過が
        // 正しい)。
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
        // 既定値(position: 0% 0%, size: auto auto, repeat: repeat)なので、
        // intrinsicサイズ(40x30)のタイルが左上起点で敷き詰められる。
        assert!(rects.iter().all(|r| r.width == 40.0 && r.height == 30.0));
        assert!(rects.contains(&Rect {
            x: 0.0,
            y: 0.0,
            width: 40.0,
            height: 30.0
        }));
        // 幅100を40刻みで覆うには3列(0,40,80)、高さ60を30刻みで覆うには2行(0,30)必要。
        assert_eq!(rects.len(), 3 * 2);
    }

    #[test]
    fn quantize_alpha_step_rounds_to_the_nearest_of_21_levels() {
        assert_eq!(quantize_alpha_step(1.0), ALPHA_STEPS);
        assert_eq!(quantize_alpha_step(0.0), 0);
        // 0.3 * 20 = 6.0 ちょうど。
        assert_eq!(quantize_alpha_step(0.3), 6);
        // 範囲外はクランプする。
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
        let style = ComputedStyle::default(); // object-fit初期値はFill
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
        // intrinsic 32x24(アスペクト比4:3) を 150x80(アスペクト比15:8) へcover。
        // scale = max(150/32, 80/24) = max(4.6875, 3.333..) = 4.6875。
        let rect = object_fit_rect(content_box, &style, (32.0, 24.0));
        assert!((rect.width - 150.0).abs() < 0.01);
        assert!((rect.height - 112.5).abs() < 0.01);
        // 初期object-position(50% 50%)で中央寄せなので、はみ出し分の半分だけ
        // content-box原点より上に描画開始する。
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
        // intrinsic(32x24)は既にcontent-box(150x80)より小さいので、noneと同じ。
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
        // intrinsic(320x240)はcontent-box(150x80)よりずっと大きいので、containと同じ。
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
        // 高さが既にcontent-boxちょうどなので、右寄せ(x軸)のみ観測できる。
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
        // `background-position: center`。
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
        // scale = min(200/100, 100/100) = 1 → 100x100のまま、中央寄せ。
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
        // 1px幅のタイルで100,000pxを覆おうとすると本来10万枚必要だが、
        // 1軸あたり200枚で打ち切られる。
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

        // タイルがborder-boxとちょうど一致する(background-size:200px 100px、
        // box自身も200x100)ので、クリップ矩形は出力されない。
        assert_eq!(count_occurrences(&decompressed, b"re\nW\nn\n"), 0);
        // XObject(画像)は1回だけ描画される。
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
        // intrinsic 40x30なので、100x60のborder-boxを覆うには3列(0,40,80)x
        // 2行(0,30)=6タイル必要。
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

    /// PDFバイト列中の全`stream`〜`endstream`区間を取り出し、
    /// zlib(`/FlateDecode`)で圧縮されていれば展開して連結したものを返す。
    /// コンテンツストリームは圧縮済みなので、オペレータ列を文字列として
    /// 検証したいテストはこちらを使う(構造レベルの辞書キー、例えば
    /// `/Subtype /Type0`のような、ストリーム本体の外にある文字列は
    /// 元の`bytes`のままで検証してよい)。
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
        // DejaVu Sansは"fl"を1グリフの合字にする。ToUnicodeに1文字しか
        // 載せないと、PDFのテキスト抽出・検索で"float"が"foat"になる。
        let dom = html::parse(b"<p>float</p>");
        let ua = user_agent_stylesheet();
        let styles = compute_styles(&dom, &ua, &Stylesheet::default());
        let fonts = test_fonts();
        let settings = PageSettings::default();

        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        // UTF-16BEで'f'=0066、'l'=006C。
        assert!(
            count_occurrences(&decompressed, b"<0066006C>") > 0,
            "the fl ligature glyph should map to both characters"
        );
    }

    #[test]
    fn subsetting_keeps_embedded_font_small() {
        // CJKフォント(元は約19MB)を、短いテキストだけ使ってPDFに埋め込む。
        // サブセット化が効いていれば、出力PDF全体が元フォントよりずっと小さいはず。
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

        // 4辺分の塗りつぶし(`f`オペレータ)が追加されているはず(各辺は
        // 外形/内形の頂点を結ぶミトー結合済みの四角形として塗る)。
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

        // 4辺 x 2帯(外側/内側) = 8回以上の塗りつぶしが追加されているはず。
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

        // 角丸パス(4角ぶんのベジェ曲線)を2周分ストロークするはず(背景色は
        // 未指定なので塗りつぶしはなし)。
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

        // 角丸パスはベジェ曲線オペレータ`c`を使う。
        assert!(
            count_occurrences(&decompressed, b" c\n") >= 8,
            "rounded corners should use cubic bezier curve operators (4 corners x fill+stroke)"
        );
        // 直線矩形の`re`は(角丸なので)使われないはず。
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

        // 4辺が不揃いなので角丸は諦め、直線4辺のフォールバックになるはず。
        // `border-style: solid dotted`は上下がsolid(塗り)、左右がdotted
        // (ストローク)に展開されるので、両方が現れるはず。
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

        // ページ余白0・丸い数値のPageSettingsを使い、座標を手計算で予測できる
        // ようにする。4辺の太さ・色をすべて不揃いにし、隣接する2辺が
        // 「内側の角の頂点」を正確に共有する(=斜めにミトー結合される)ことを、
        // 生成された実際のコンテンツストリームの座標列で確認する。
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

        // border-box: x∈[0,360](border-left 40 + width 300 + border-right 20)、
        // PDF空間でy_top=1000(border-top 10)、y_bottom=760(border-bottom 30)。
        // 右上の外側の角(360,1000)と内側の角(340,990)は、top/rightの両方の
        // パスに現れるはず(top側は終端、right側は始端として)。
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

        // 2つのフォント(DejaVu Sans, Noto Sans CJK JP)がそれぞれ埋め込まれているはず。
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

        // 各セルのテキストがコンテンツストリームに(グリフとして)出力されている
        // ことを、フォント使用状況(グリフ数)経由で間接的に確認する。
        // "Header"/"Apple"/"100"のテキストが1つのフォントに集約されているはず
        // なので、埋め込みフォントは1つだけ。
        assert_eq!(
            count_occurrences(&bytes, b"/FontFile2"),
            1,
            "all table cell text should use the single loaded font"
        );

        // colspanで結合されたヘッダーセルの背景・枠線と、通常セルの背景・枠線を
        // 合わせて複数の塗りつぶし(`f`)が出力されているはず(テーブル自身には
        // 背景/枠線を指定していないので、セル由来のみ)。
        assert!(
            count_occurrences(&decompressed, b"\nf\n") >= 2,
            "cell borders/backgrounds should produce fill operators"
        );
        // 明示的に指定したセル背景色がfillの色として現れるはず。
        assert!(
            text.contains("0.78431374 0.78431374 0.78431374 rg"),
            "the explicit cell background-color should be painted"
        );
    }

    /// 与えたHTML/CSSをPDF化し、展開したコンテンツストリーム中の塗りつぶし
    /// (`f`)演算子の出現数を返す(背景・枠線描画の合計を数える簡易プロキシ)。
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
        // `<td>&nbsp;</td>`は枠を出すための定番の書き方。`&nbsp;`は畳み込まれない
        // 内容を持つので、`empty-cells: hide`で消してはいけない
        // (`str::trim`で空判定していた頃は空セル扱いになっていた)。
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
        // テーブル自身はデフォルト(show)のまま、空セルにだけ`empty-cells: hide`
        // を指定した場合でもそのセルの装飾が抑制されることを確認する
        // (このプロパティは`table-cell`要素に適用されるため、テーブル単位では
        // なくセル単位で見る必要がある)。
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
        // 隣接する2セルが同じ枠線を指定している場合、separateモデルでは
        // 各セルが独立に4辺とも描画する(2+2セル分=8回)。collapseモデルでは
        // 内部で接する1辺の描画が抑制されて1回に統合されるため、合計は1回
        // 減った7回になるはず。
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
        // 左セルは枠線を指定していない(none)が、右セルの左辺(実際には隣接
        // する境界の統合先である左セルの右辺として解決される)に実際の枠線が
        // あるため、境界に枠線が現れなくなってはいけない
        // (「own=none」を無条件に採用してはいけないことの回帰テスト)。
        let html_src = r#"<table><tr><td class="a">a</td><td class="b">b</td></tr></table>"#;
        let css = "body { margin: 0; } \
                   table { border-collapse: collapse; } \
                   .a { border: none; } \
                   .b { border: 2px solid black; }";

        let fills = fill_operator_count(html_src, css);
        // 右セルの上/右/下辺(3, 左辺は隣接があるため抑制)+左セルの右辺
        // (隣接セルの枠線を継承して1)=合計4のはず。
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
        // `style: none`の辺は幅の指定に関わらず実効幅0として扱われるため、
        // 幅の数値だけを見れば「勝って」しまいそうな場合でも負けるはず。
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
        // "Invoice"(DejaVu)と"請求書"(CJK)はフォントが切り替わるラン境界に
        // またがる単語境界で、どちらのTextRun.textにも実際の空白文字を含まない
        // (単語間の空白はx_offsetの隙間としてのみ表現される)。座標ギャップに
        // 頼るテキスト抽出はフォント切り替えを伴う境界で崩れることがあるため、
        // 視覚描画に影響しない`ActualText`付きマーク区間で明示しているはず。
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
        // `letter-spacing`はグリフ幅そのものには反映できないため、PDFの`Tc`
        // (character spacing)演算子として出力される必要がある。
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
        // 本文中に一切数字が登場しない文書でも、マーカーの
        // '1'(U+0031)が`/ToUnicode`CMapに実際に埋め込まれることを確認する。
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
        // ::before/::afterのcontent(attr/counter)が生成する文字も、通常の
        // テキストスパンと同じ`BoxContent::Inline`経路(collect_line_usage)を
        // 通るため、マーカーの時とは異なり専用の収集漏れは生じないはずだが、
        // 本文中に一切登場しない数字(counter由来の'1')が実際に埋め
        // 込まれることを確認する。
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

            // outsetはinsetの明暗を反転しただけ(同じ辺で逆の色)。
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

        // groove: top/leftは外側が暗く内側が明るい(みぞの奥行き)、right/bottomは逆。
        let top_groove = border_side_colors(BorderStyle::Groove, BorderSideKind::Top, color);
        assert_eq!(top_groove.outer, dark);
        assert_eq!(top_groove.inner, light);
        let right_groove = border_side_colors(BorderStyle::Groove, BorderSideKind::Right, color);
        assert_eq!(right_groove.outer, light);
        assert_eq!(right_groove.inner, dark);

        // ridgeはgrooveの外側/内側を反転しただけ。
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

        // outlineはレイアウトに影響しないため、`div`のcontent boxの位置・寸法は
        // outlineの有無で変わらないはず。
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

        // 親が`visibility: hidden`でも、子が明示的に`visible`を指定していれば
        // 描画される(仕様通り)。
        let dom = html::parse(br#"<div class="outer"><p class="inner">shown</p></div>"#);
        let author = parse_stylesheet(
            ".outer { visibility: hidden; background-color: rgb(255, 0, 0); } \
             .inner { visibility: visible; }",
        );
        let styles = compute_styles(&dom, &ua, &author);
        let pages = paginate_document(&dom, &styles, &fonts, &settings);
        let bytes = encode_pdf(&pages, &styles, &HashMap::new(), &fonts, &settings);
        let decompressed = decompressed_stream_bytes(&bytes);

        // outerの背景(赤)は描画されないはず。
        assert_eq!(
            count_occurrences(&decompressed, b"1 0 0 rg"),
            0,
            "hidden outer's red background should not be painted"
        );
        // innerのテキストは(何らかのグリフ描画として)出力されるはず。
        // グリフ列は送り幅の補正を挟めるよう常に`TJ`で出す([`show_run_glyphs`])。
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
        // html5everが暗黙に`<html>`/`<body>`を補うため、`<div>`のNodeIdを
        // 辿って探す(木の深さを決め打ちしない)。
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
        // b(z-index:-1) < c/d(static、z-indexが効かずauto=0扱い、文書順でc→d) < a(z-index:2)。
        assert_eq!(order, vec!["b", "c", "d", "a"]);
    }

    #[test]
    fn paint_order_puts_floats_above_in_flow_blocks() {
        // floatが先に描かれると、直後のブロックの背景がfloatを塗り潰して
        // しまう(CSS2.1 Appendix Eではブロックの背景よりfloatが後のレイヤー)。
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

    // ===== `<a href>`のリンク注釈 =====

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
        // リンクは行頭ではないので、矩形は左端から始まらない。
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
        // 行ごとに縦位置が異なる。
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
