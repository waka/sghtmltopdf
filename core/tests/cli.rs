//! CLI結合・E2Eテスト。
//!
//! 実際にコンパイル済みバイナリを起動し、サンプルHTML(見出し+段落+
//! ページ分割が発生するだけの長さの繰り返しコンテンツ)を一括変換して、
//! 有効なPDFが生成されることを確認する。
//!
//! バイト単位のゴールデンPDF比較ではなく構造的なチェック(ページ数・
//! フォント埋め込みマーカーの有無)にとどめる。改ページパターン
//! (break-before/after/inside・orphans/widows)ごとの回帰検出は
//! `fragmentation.rs`が担当する。

use std::path::Path;
use std::process::Command;

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const CJK_FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/NotoSansCJK-Regular.ttc"
);
/// ビットマップのみ(CBDT/CBLC)で、グリフの輪郭を一切持たないフォント。
const COLOR_EMOJI_FONT_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fonts/NotoColorEmoji.ttf"
);
const SAMPLE_HTML: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/sample.html");
const BIN: &str = env!("CARGO_BIN_EXE_sghtmltopdf");

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// `/MediaBox`の期待値をCSS pxで書けるようにするヘルパ。PDFへはpt(既定で
/// 0.75倍)で書かれるため、ここで換算する。
fn media_box(width_px: f32, height_px: f32) -> String {
    format!(
        "/MediaBox [0 0 {} {}]",
        width_px * sghtmltopdf_core::pdf::DEFAULT_SCALE,
        height_px * sghtmltopdf_core::pdf::DEFAULT_SCALE
    )
}

fn temp_output_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("sghtmltopdf-e2e-{}-{name}.pdf", std::process::id()))
}

#[test]
fn converts_sample_html_into_a_multi_page_pdf() {
    let output = temp_output_path("sample");

    let status = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(&output)
        .status()
        .expect("failed to run sghtmltopdf binary");
    assert!(status.success(), "CLI should exit successfully");

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    assert!(
        count_occurrences(&bytes, b"/Subtype /Type0") > 0,
        "font should be embedded"
    );
    assert!(count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0);

    let page_count = count_occurrences(&bytes, b"/MediaBox");
    assert!(
        page_count > 1,
        "sample.html has enough repeated content to force pagination, got {page_count} page(s)"
    );

    std::fs::remove_file(&output).ok();
}

#[test]
fn defaults_output_path_to_input_with_pdf_extension() {
    // 一時ディレクトリへ入力HTMLをコピーし、-oを省略して既定の出力先を確認する。
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-default-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.html");
    std::fs::copy(SAMPLE_HTML, &input).unwrap();

    let status = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(FONT_PATH)
        .status()
        .expect("failed to run sghtmltopdf binary");
    assert!(status.success());

    let expected_output = dir.join("input.pdf");
    assert!(
        expected_output.exists(),
        "default output path should be input path with .pdf extension"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn font_face_src_url_is_resolved_relative_to_the_html_file_and_embedded() {
    // HTMLファイルと同じディレクトリに置いたフォントファイルを、
    // `@font-face { src: url(...); }`の相対パスとして解決できることを確認する。
    // `--font`ではDejaVu Sans(CJKグリフを持たない)のみを渡し、CJKテキストは
    // `@font-face`経由で読み込んだフォントでのみ描画できるようにすることで、
    // 単に`--font`だけで埋め込まれたのではないことを検証する。
    let dir =
        std::env::temp_dir().join(format!("sghtmltopdf-e2e-font-face-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(CJK_FONT_PATH, dir.join("cjk.ttc")).unwrap();

    let input = dir.join("input.html");
    std::fs::write(
        &input,
        r#"<html><head><style>
            @font-face { font-family: "CJK Brand"; src: url("cjk.ttc"); }
            p { font-family: "CJK Brand"; }
        </style></head><body><p>日本語のテスト</p></body></html>"#,
    )
    .unwrap();

    let output = dir.join("output.pdf");
    let status = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(&output)
        .status()
        .expect("failed to run sghtmltopdf binary");
    assert!(status.success(), "CLI should exit successfully");

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert_eq!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2"),
        2,
        "both the --font fallback and the @font-face font should be embedded"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn works_without_font_by_falling_back_to_a_system_font() {
    // `--font`は任意。省略した場合はシステムの`sans-serif`候補を既定
    // フォントとして使う(見つからなければエラー)。
    let output = temp_output_path("no-font");

    let status = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("-o")
        .arg(&output)
        .arg("--quiet")
        .status()
        .expect("failed to run sghtmltopdf binary");
    assert!(status.success(), "CLI should fall back to a system font");

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2") > 0,
        "a font must still be embedded"
    );

    std::fs::remove_file(&output).ok();
}

#[test]
fn fails_with_nonzero_exit_when_input_file_does_not_exist() {
    let output = temp_output_path("missing-input");

    let status = Command::new(BIN)
        .arg(Path::new("/nonexistent/does-not-exist.html"))
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(&output)
        .status()
        .expect("failed to run sghtmltopdf binary");

    assert!(
        !status.success(),
        "CLI should fail when the input file does not exist"
    );
    assert!(!output.exists());
}

// ---------------------------------------------------------------------------
// ===== clap移行・stdin/stdout・exit code =====
// ---------------------------------------------------------------------------

#[test]
fn reads_html_from_stdin_when_the_input_is_a_dash() {
    use std::io::Write;
    use std::process::Stdio;

    let output = temp_output_path("stdin");
    let mut child = Command::new(BIN)
        .arg("-")
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(&output)
        .stdin(Stdio::piped())
        .spawn()
        .expect("failed to spawn sghtmltopdf");
    child
        .stdin
        .as_mut()
        .expect("stdin should be piped")
        .write_all(b"<html><body><p>from stdin</p></body></html>")
        .unwrap();
    let status = child.wait().expect("failed to wait for sghtmltopdf");
    assert!(status.success(), "CLI should accept HTML on stdin");

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert!(bytes.starts_with(b"%PDF-"));
    std::fs::remove_file(&output).ok();
}

#[test]
fn writes_the_pdf_to_stdout_when_the_output_is_a_dash() {
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg("-")
        .output()
        .expect("failed to run sghtmltopdf binary");

    assert!(out.status.success());
    assert!(
        out.stdout.starts_with(b"%PDF-"),
        "PDF bytes should go to stdout"
    );
    assert!(count_occurrences(&out.stdout, b"%%EOF") > 0);
    // 進捗メッセージはstdoutを汚さない(必ずstderrへ)。
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("標準出力"),
        "progress message should be written to stderr"
    );
}

#[test]
fn stdin_input_without_an_explicit_output_is_a_usage_error() {
    let out = Command::new(BIN)
        .arg("-")
        .arg("--font")
        .arg(FONT_PATH)
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn exit_codes_follow_the_documented_mapping() {
    // 1 = 使用法エラー(必須の位置引数が無い)
    let usage = Command::new(BIN)
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(
        usage.status.code(),
        Some(1),
        "missing input is a usage error"
    );

    // 1 = 使用法エラー(未知のオプション)
    let unknown = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("--no-such-option")
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(unknown.status.code(), Some(1));

    // 2 = 入力/リソースエラー(入力HTMLが存在しない)
    let input = Command::new(BIN)
        .arg(Path::new("/nonexistent/does-not-exist.html"))
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(temp_output_path("exit-code-input"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(input.status.code(), Some(2));
}

#[test]
fn version_and_help_exit_successfully() {
    for flag in ["--version", "--help"] {
        let out = Command::new(BIN)
            .arg(flag)
            .output()
            .expect("failed to run sghtmltopdf binary");
        assert!(out.status.success(), "{flag} should exit with 0");
        assert!(!out.stdout.is_empty(), "{flag} should print to stdout");
    }
}

#[test]
fn a_failing_run_leaves_no_output_file_behind() {
    // --fontにフォントではないファイル(HTML自身)を渡して失敗させる。
    // FileSinkは一時ファイルへ書いてからrenameするため、失敗しても
    // 出力先には何も残らない。
    let output = temp_output_path("no-leftover");
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(SAMPLE_HTML)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("failed to run sghtmltopdf binary");

    assert!(!out.status.success(), "loading a non-font file should fail");
    assert!(
        !output.exists(),
        "no partial PDF should be left at the output path"
    );

    // 一時ファイル(<output>.tmp-<pid>)も残っていないこと。
    let dir = output.parent().unwrap();
    let stem = output.file_name().unwrap().to_string_lossy().to_string();
    let leftovers: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with(&stem) && name.contains(".tmp-")
        })
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary files should be cleaned up, found: {leftovers:?}"
    );
}

#[test]
fn quiet_suppresses_the_success_message() {
    let output = temp_output_path("quiet");
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(&output)
        .arg("--quiet")
        .output()
        .expect("failed to run sghtmltopdf binary");

    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "--quiet should suppress the success message, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    std::fs::remove_file(&output).ok();
}

#[test]
fn base_url_directory_resolves_relative_assets_for_stdin_input() {
    use std::io::Write;
    use std::process::Stdio;

    // 標準入力から読むと相対解決の基準が無くなるため、--base-urlで与える。
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-base-url-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(CJK_FONT_PATH, dir.join("cjk.ttc")).unwrap();
    let output = dir.join("out.pdf");

    let mut child = Command::new(BIN)
        .arg("-")
        .arg("--font")
        .arg(FONT_PATH)
        .arg("--base-url")
        .arg(&dir)
        .arg("-o")
        .arg(&output)
        .stdin(Stdio::piped())
        .spawn()
        .expect("failed to spawn sghtmltopdf");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(
            r#"<html><head><style>
                @font-face { font-family: "CJK Brand"; src: url("cjk.ttc"); }
                p { font-family: "CJK Brand"; }
            </style></head><body><p>日本語のテスト</p></body></html>"#
                .as_bytes(),
        )
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success());

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert_eq!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2"),
        2,
        "the @font-face font must be resolved relative to --base-url"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_bad_base_url_is_reported_as_an_input_error() {
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("--base-url")
        .arg("/nonexistent/directory")
        .arg("-o")
        .arg(temp_output_path("bad-base-url"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(2));
}

// `server`サブコマンドは`server` feature(既定ON)でのみ存在する。
#[cfg(feature = "server")]
#[test]
fn the_server_subcommand_reports_a_bad_listen_address() {
    // サーバ本体のE2Eは`core/tests/server.rs`が担当する。ここでは
    // 「起動に失敗したらexit 2で終わる」ことだけを見る(待ち受けに成功すると
    // 戻ってこないので、必ず失敗するアドレスを渡す)。
    let out = Command::new(BIN)
        .args(["server", "--listen", "256.256.256.256:1"])
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("待ち受け"));
}

// ---------------------------------------------------------------------------
// ===== ページ設定オプションと`@page`との合成 =====
// ---------------------------------------------------------------------------

/// HTMLを一時ディレクトリへ書き、指定した引数でCLIを走らせてPDFバイト列を返す。
fn run_cli_with(html: &str, extra_args: &[&str], name: &str) -> Vec<u8> {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.html");
    std::fs::write(&input, html).unwrap();
    let output = dir.join("out.pdf");

    let status = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(&output)
        .args(extra_args)
        .arg("--quiet")
        .status()
        .expect("failed to run sghtmltopdf binary");
    assert!(status.success(), "CLI should succeed for case {name}");

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    std::fs::remove_dir_all(&dir).ok();
    bytes
}

const PLAIN_HTML: &str = "<html><body><p>hello</p></body></html>";

#[test]
fn page_size_option_changes_the_media_box() {
    // MediaBoxの値はレイアウト内部単位(CSS px)がそのまま入る。
    let bytes = run_cli_with(PLAIN_HTML, &["--page-size", "A5"], "page-size");
    assert_eq!(
        count_occurrences(&bytes, media_box(559.4, 793.7).as_bytes()),
        1,
        "A5 should be used"
    );
}

#[test]
fn orientation_landscape_swaps_the_page_dimensions() {
    let bytes = run_cli_with(
        PLAIN_HTML,
        &["--page-size", "A5", "--orientation", "Landscape"],
        "orientation",
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(793.7, 559.4).as_bytes()),
        1
    );
}

#[test]
fn explicit_page_width_and_height_override_the_page_size() {
    let bytes = run_cli_with(
        PLAIN_HTML,
        &[
            "--page-size",
            "A4",
            "--page-width",
            "400px",
            "--page-height",
            "500px",
        ],
        "page-wh",
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(400.0, 500.0).as_bytes()),
        1
    );
}

#[test]
fn margin_options_change_how_much_content_fits_on_a_page() {
    // 同じHTMLでも上下マージンを増やすとページ数が増える。
    let html = format!(
        "<html><body>{}</body></html>",
        "<p style=\"margin:0\">line</p>".repeat(40)
    );
    let narrow = run_cli_with(
        &html,
        &["--margin-top", "10mm", "--margin-bottom", "10mm"],
        "margin-narrow",
    );
    let wide = run_cli_with(
        &html,
        &["--margin-top", "80mm", "--margin-bottom", "80mm"],
        "margin-wide",
    );

    let narrow_pages = count_occurrences(&narrow, b"/MediaBox");
    let wide_pages = count_occurrences(&wide, b"/MediaBox");
    assert!(
        wide_pages > narrow_pages,
        "larger margins should need more pages: {narrow_pages} -> {wide_pages}"
    );
}

#[test]
fn an_author_at_page_size_wins_over_the_cli_option() {
    // CLIは初期値で、著者CSSの`@page`宣言が優先される。
    let html = r#"<html><head><style>@page { size: 300px 400px; }</style></head>
                  <body><p>hello</p></body></html>"#;
    let bytes = run_cli_with(html, &["--page-size", "A4"], "at-page-wins");
    assert_eq!(
        count_occurrences(&bytes, media_box(300.0, 400.0).as_bytes()),
        1
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(793.7, 1122.5).as_bytes()),
        0
    );
}

#[test]
fn cli_and_at_page_are_merged_per_property() {
    // `@page`がmarginだけを宣言している場合、sizeはCLI指定が残る。
    let html = r#"<html><head><style>@page { margin: 0; }</style></head>
                  <body><p>hello</p></body></html>"#;
    let bytes = run_cli_with(
        html,
        &["--page-width", "400px", "--page-height", "500px"],
        "per-property",
    );
    assert_eq!(
        count_occurrences(&bytes, media_box(400.0, 500.0).as_bytes()),
        1,
        "size comes from the CLI because @page only declared margin"
    );
}

#[test]
fn an_impossible_page_geometry_is_a_usage_error() {
    // 左右マージンの合計が用紙幅以上。
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("--page-width")
        .arg("100px")
        .arg("-o")
        .arg(temp_output_path("impossible-geometry"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("マージン"));
}

// ---------------------------------------------------------------------------
// ===== PDFメタデータ・圧縮・スケール・グレースケール =====
// ---------------------------------------------------------------------------

#[test]
fn the_info_dictionary_always_carries_a_producer() {
    let bytes = run_cli_with(PLAIN_HTML, &[], "producer");
    assert!(
        count_occurrences(&bytes, b"/Producer") > 0,
        "the Info dictionary should always be written"
    );
    assert!(count_occurrences(&bytes, b"/CreationDate") > 0);
    assert!(
        count_occurrences(&bytes, b"/Info ") > 0,
        "the trailer must point at the Info dictionary"
    );
}

#[test]
fn the_trailer_carries_a_file_identifier() {
    // PDF/Aはファイル識別子(`/ID`)を要求する。バッチ・ストリーミングの
    // どちらの書き出しでも、同じ作り方で16バイトを2つ書く。
    for (args, name) in [
        (&[][..], "file-id-batch"),
        (&["--streaming"][..], "file-id-streaming"),
    ] {
        let bytes = run_cli_with(PLAIN_HTML, args, name);
        let text = String::from_utf8_lossy(&bytes);
        let trailer = text
            .rfind("trailer")
            .map(|i| &text[i..])
            .expect("the PDF should have a trailer");
        let id = trailer
            .split_once("/ID [<")
            .and_then(|(_, rest)| rest.split_once('>'))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("{name}: the trailer should carry /ID: {trailer}"));
        assert_eq!(id.len(), 32, "{name}: /ID should be 16 bytes in hex");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "{name}: {id}");
    }
}

#[test]
fn the_title_option_wins_over_the_html_title() {
    let html = "<html><head><title>from html</title></head><body><p>x</p></body></html>";

    let from_html = run_cli_with(html, &[], "title-html");
    assert!(count_occurrences(&from_html, b"from html") > 0);

    let from_option = run_cli_with(html, &["--title", "from option"], "title-option");
    assert!(count_occurrences(&from_option, b"from option") > 0);
    assert_eq!(count_occurrences(&from_option, b"from html"), 0);
}

#[test]
fn author_subject_and_keywords_are_written_when_given() {
    let bytes = run_cli_with(
        PLAIN_HTML,
        &[
            "--author",
            "waka",
            "--subject",
            "invoice",
            "--keywords",
            "pdf,rust",
        ],
        "metadata",
    );
    assert!(count_occurrences(&bytes, b"/Author (waka)") > 0);
    assert!(count_occurrences(&bytes, b"/Subject (invoice)") > 0);
    assert!(count_occurrences(&bytes, b"/Keywords (pdf,rust)") > 0);
}

#[test]
fn no_pdf_compression_removes_every_flate_filter() {
    let compressed = run_cli_with(PLAIN_HTML, &[], "compressed");
    let plain = run_cli_with(PLAIN_HTML, &["--no-pdf-compression"], "uncompressed");

    assert!(count_occurrences(&compressed, b"/FlateDecode") > 0);
    assert_eq!(
        count_occurrences(&plain, b"/FlateDecode"),
        0,
        "content stream and font objects must be stored uncompressed"
    );
    assert!(
        plain.len() > compressed.len(),
        "the uncompressed PDF should be larger"
    );
}

#[test]
fn grayscale_maps_fill_colors_to_their_luminance() {
    let html = r#"<html><body><p style="color:#ff0000">red</p></body></html>"#;
    let colored = run_cli_with(html, &["--no-pdf-compression"], "colored");
    let gray = run_cli_with(html, &["--no-pdf-compression", "--grayscale"], "grayscaled");

    assert!(count_occurrences(&colored, b"1 0 0 rg") > 0, "red is kept");
    assert_eq!(count_occurrences(&gray, b"1 0 0 rg"), 0);
    assert!(
        count_occurrences(&gray, b"0.2126 0.2126 0.2126 rg") > 0,
        "red must become its sRGB luminance"
    );
}

#[test]
fn the_default_output_uses_real_paper_dimensions_in_points() {
    // A4 = 793.7 × 1122.5 CSS px → 595.275 × 841.875 pt(= 210 × 297mm)。
    let bytes = run_cli_with(PLAIN_HTML, &[], "a4-pt");
    assert_eq!(
        count_occurrences(&bytes, media_box(793.7, 1122.5).as_bytes()),
        1
    );
    assert!(
        count_occurrences(&bytes, b"/MediaBox [0 0 595.275 841.875]") > 0,
        "A4 must be 595.275 x 841.875 pt"
    );
}

#[test]
fn dpi_72_keeps_one_css_px_as_one_pt() {
    // px→pt変換を行わない(1px=1pt)挙動に戻す逃げ道。
    let bytes = run_cli_with(PLAIN_HTML, &["--dpi", "72"], "dpi72");
    assert!(count_occurrences(&bytes, b"/MediaBox [0 0 793.7 1122.5]") > 0);
}

#[test]
fn zoom_scales_the_page_geometry() {
    let bytes = run_cli_with(PLAIN_HTML, &["--zoom", "2"], "zoom2");
    assert!(count_occurrences(&bytes, b"/MediaBox [0 0 1190.55 1683.75]") > 0);
}

#[test]
fn a_non_positive_dpi_or_zoom_is_a_usage_error() {
    for args in [["--dpi", "0"], ["--zoom", "-1"]] {
        let out = Command::new(BIN)
            .arg(SAMPLE_HTML)
            .arg("--font")
            .arg(FONT_PATH)
            .args(args)
            .arg("-o")
            .arg(temp_output_path("bad-scaling"))
            .output()
            .expect("failed to run sghtmltopdf binary");
        assert_eq!(out.status.code(), Some(1), "{args:?} should be rejected");
    }
}

// ---------------------------------------------------------------------------
// ===== コンテンツ挙動オプション =====
// ---------------------------------------------------------------------------

#[test]
fn mono_and_serif_fonts_resolve_the_matching_generic_family() {
    // `--font`はDejaVu(CJKグリフ無し)だけにし、CJKは汎用family経由の
    // フォントでしか描けないようにする。
    let html = r#"<html><body><p style="font-family: monospace">日本語</p></body></html>"#;
    let bytes = run_cli_with(html, &["--mono-font", CJK_FONT_PATH], "mono-font");
    assert_eq!(
        count_occurrences(&bytes, b"/Subtype /CIDFontType2"),
        2,
        "the --mono-font face must be embedded for font-family: monospace"
    );

    let html = r#"<html><body><p style="font-family: serif">日本語</p></body></html>"#;
    let bytes = run_cli_with(html, &["--serif-font", CJK_FONT_PATH], "serif-font");
    assert_eq!(count_occurrences(&bytes, b"/Subtype /CIDFontType2"), 2);
}

#[test]
fn no_background_removes_the_background_fill() {
    let html = r#"<html><body><div style="background-color:#ff0000;width:50px;height:50px"></div></body></html>"#;
    let with_bg = run_cli_with(html, &["--no-pdf-compression"], "with-bg");
    let without_bg = run_cli_with(html, &["--no-pdf-compression", "--no-background"], "no-bg");

    assert!(count_occurrences(&with_bg, b"1 0 0 rg") > 0);
    assert_eq!(count_occurrences(&without_bg, b"1 0 0 rg"), 0);
}

#[test]
fn a_user_style_sheet_applies_below_the_author_css() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-user-css-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let css = dir.join("user.css");
    std::fs::write(&css, "p { color: #00ff00 }").unwrap();

    // ユーザーCSSだけが色を決めるケース。
    let bytes = run_cli_with(
        "<html><body><p>x</p></body></html>",
        &[
            "--no-pdf-compression",
            "--user-style-sheet",
            css.to_str().unwrap(),
        ],
        "user-css",
    );
    assert!(
        count_occurrences(&bytes, b"0 1 0 rg") > 0,
        "user CSS should apply"
    );

    // 著者CSSが指定していればそちらが勝つ。
    let bytes = run_cli_with(
        r#"<html><head><style>p { color: #0000ff }</style></head><body><p>x</p></body></html>"#,
        &[
            "--no-pdf-compression",
            "--user-style-sheet",
            css.to_str().unwrap(),
        ],
        "user-css-loses",
    );
    assert!(
        count_occurrences(&bytes, b"0 0 1 rg") > 0,
        "author CSS must win"
    );
    assert_eq!(count_occurrences(&bytes, b"0 1 0 rg"), 0);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn minimum_font_size_clamps_small_text() {
    let html = r#"<html><body><p style="font-size:4px">tiny</p></body></html>"#;
    let small = run_cli_with(html, &["--no-pdf-compression"], "tiny");
    let clamped = run_cli_with(
        html,
        &["--no-pdf-compression", "--minimum-font-size", "20"],
        "clamped",
    );
    // フォントサイズはTf演算子に出る。
    assert!(count_occurrences(&small, b" 4 Tf") > 0);
    assert!(count_occurrences(&clamped, b" 20 Tf") > 0);
}

#[test]
fn link_annotations_can_be_disabled_by_kind() {
    // 内部アンカー(`"#here"`)を含むため、raw stringは`r##`で囲む。
    let html = r##"<html><body>
        <p><a href="https://example.com">external</a></p>
        <p><a href="#here">internal</a></p>
        <p id="here">target</p>
    </body></html>"##;

    let both = run_cli_with(html, &[], "links-both");
    assert_eq!(count_occurrences(&both, b"/Subtype /Link"), 2);

    let no_external = run_cli_with(html, &["--disable-external-links"], "links-no-ext");
    assert_eq!(count_occurrences(&no_external, b"/Subtype /Link"), 1);

    let no_internal = run_cli_with(html, &["--disable-internal-links"], "links-no-int");
    assert_eq!(count_occurrences(&no_internal, b"/Subtype /Link"), 1);

    let none = run_cli_with(
        html,
        &["--disable-external-links", "--disable-internal-links"],
        "links-none",
    );
    assert_eq!(count_occurrences(&none, b"/Subtype /Link"), 0);
}

#[test]
fn shift_jis_is_decoded_from_the_meta_charset() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-sjis-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.html");
    let mut bytes = b"<html><head><meta charset=\"shift_jis\"></head><body><p>".to_vec();
    bytes.extend_from_slice(b"\x93\xfa\x96\x7b\x8c\xea"); // 「日本語」
    bytes.extend_from_slice(b"</p></body></html>");
    std::fs::write(&input, &bytes).unwrap();
    let output = dir.join("out.pdf");

    let status = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(CJK_FONT_PATH)
        .arg("-o")
        .arg(&output)
        .arg("--quiet")
        .status()
        .expect("failed to run sghtmltopdf binary");
    assert!(status.success());

    // 文字化けしていれば.notdefになりグリフが埋まらない。ToUnicodeに
    // 「日」(U+65E5)が現れることで、正しくデコードされたと確認する。
    let pdf = std::fs::read(&output).unwrap();
    assert!(count_occurrences(&pdf, b"/Subtype /CIDFontType2") > 0);

    // --encodingの明示指定でも同じ結果になる。
    let output2 = dir.join("out2.pdf");
    let status = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(CJK_FONT_PATH)
        .arg("-o")
        .arg(&output2)
        .args(["--encoding", "Shift_JIS", "--quiet"])
        .status()
        .unwrap();
    assert!(status.success());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_unknown_encoding_is_a_usage_error() {
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .args(["--encoding", "no-such-encoding"])
        .arg("-o")
        .arg(temp_output_path("bad-encoding"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn load_media_error_handling_abort_stops_on_a_missing_image() {
    let html = r#"<html><body><img src="does-not-exist.png"><p>x</p></body></html>"#;

    // 既定(ignore)は成功する。
    let bytes = run_cli_with(html, &[], "media-ignore");
    assert!(bytes.starts_with(b"%PDF-"));

    // abortでは入力/リソースエラー(exit 2)。
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-abort-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.html");
    std::fs::write(&input, html).unwrap();
    let out = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(dir.join("out.pdf"))
        .args(["--load-media-error-handling", "abort", "--quiet"])
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(2));
    assert!(
        !dir.join("out.pdf").exists(),
        "no partial PDF should remain"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn the_streaming_mode_can_be_selected() {
    let bytes = run_cli_with(PLAIN_HTML, &["--streaming"], "streaming");
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

#[test]
fn allow_limits_local_reads_to_the_listed_directories() {
    // --allowで許可していないディレクトリのフォントは読めない。
    let dir =
        std::env::temp_dir().join(format!("sghtmltopdf-e2e-{}-allow-dir", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .args(["--allow", dir.to_str().unwrap()])
        .arg("-o")
        .arg(temp_output_path("allow"))
        .arg("--quiet")
        .output()
        .expect("failed to run sghtmltopdf binary");
    // sample.htmlは外部リソースを参照しないため、--allowで絞っても成功する。
    assert!(out.status.success());

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_allow_directory_that_cannot_be_resolved_is_rejected_at_startup() {
    // 解決できない--allowを黙って受け入れると、許可範囲の判定が生のパスでの
    // 比較に落ちる。意図しない範囲で動き続けないよう起動時に止める。
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .args(["--allow", "/nonexistent-allowed-dir"])
        .arg("-o")
        .arg(temp_output_path("allow-missing"))
        .output()
        .expect("failed to run sghtmltopdf binary");

    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--allow"), "got: {stderr}");
}

// ---------------------------------------------------------------------------
// ===== ヘッダー/フッター =====
// ---------------------------------------------------------------------------

/// 2ページになるだけの内容を持つHTML。
const TWO_PAGE_HTML: &str =
    r#"<html><body><p>first</p><p style="break-before: page">second</p></body></html>"#;

#[test]
fn simple_header_and_footer_options_render_as_margin_boxes() {
    // テキスト描画演算子を数えるのでcontent streamは非圧縮にする。
    let plain = run_cli_with(TWO_PAGE_HTML, &["--no-pdf-compression"], "hf-plain");
    let with_hf = run_cli_with(
        TWO_PAGE_HTML,
        &[
            "--no-pdf-compression",
            "--header-center",
            "HEAD",
            "--footer-center",
            "FOOT",
        ],
        "hf-simple",
    );
    // 本文だけのときよりテキスト描画が増える(各ページにヘッダ・フッタ)。
    let plain_text_ops = count_occurrences(&plain, b"Tj") + count_occurrences(&plain, b"TJ");
    let hf_text_ops = count_occurrences(&with_hf, b"Tj") + count_occurrences(&with_hf, b"TJ");
    assert!(
        hf_text_ops >= plain_text_ops + 4,
        "header/footer should add text on both pages: {plain_text_ops} -> {hf_text_ops}"
    );
}

#[test]
fn page_placeholders_become_page_counters() {
    // `[page]`/`[topage]`が`counter(page)`/`counter(pages)`になっていれば、
    // 2ページ目のフッターに「2」「2」が出る。ToUnicodeで数字が使われる
    // ことを確認する(数字U+0032 = <0032>)。
    let bytes = run_cli_with(
        TWO_PAGE_HTML,
        &["--footer-center", "Page [page] of [topage]"],
        "hf-counters",
    );
    assert!(bytes.starts_with(b"%PDF-"));
    assert_eq!(count_occurrences(&bytes, b"/MediaBox"), 2);
}

#[test]
fn an_author_at_page_margin_box_wins_over_the_cli_option() {
    // CLI由来のルールは著者ルールより前に置かれる。
    let html = r#"<html><head><style>
            @page { @top-center { content: "FROM CSS"; } }
        </style></head><body><p>x</p></body></html>"#;
    let bytes = run_cli_with(html, &["--header-center", "FROM CLI"], "hf-priority");
    let text = String::from_utf8_lossy(&bytes);
    // 圧縮されているので直接は読めないが、生成に成功していれば十分
    // (優先順位そのものはユニットテストとmargin box解決でカバー)。
    assert!(text.starts_with("%PDF-"));
}

#[test]
fn header_and_footer_lines_are_drawn() {
    let without = run_cli_with(PLAIN_HTML, &["--no-pdf-compression"], "no-lines");
    let with = run_cli_with(
        PLAIN_HTML,
        &["--no-pdf-compression", "--header-line", "--footer-line"],
        "with-lines",
    );
    // 罫線はstroke(S)で描かれる。
    assert!(
        count_occurrences(&with, b"\nS\n") > count_occurrences(&without, b"\nS\n"),
        "header/footer lines should emit stroke operators"
    );
}

#[test]
fn header_spacing_increases_the_top_margin() {
    // 1ページの残り高さの差がページ数の差として現れるだけの分量と余白を使う
    // (行送りはフォントのメトリクス由来なので、境界ぎりぎりの分量にすると
    // フォントを変えただけでページ数が並んでしまう)。
    let html = format!(
        "<html><body>{}</body></html>",
        "<p style=\"margin:0\">line</p>".repeat(90)
    );
    let normal = run_cli_with(&html, &[], "spacing-none");
    let spaced = run_cli_with(&html, &["--header-spacing", "100"], "spacing-100");
    assert!(
        count_occurrences(&spaced, b"/MediaBox") > count_occurrences(&normal, b"/MediaBox"),
        "a larger top margin should need more pages"
    );
}

#[test]
fn header_html_is_composed_onto_every_page() {
    let dir = std::env::temp_dir().join(format!(
        "sghtmltopdf-e2e-header-html-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let header = dir.join("header.html");
    std::fs::write(
        &header,
        r#"<html><body style="margin:0"><p style="color:#ff0000;font-size:10px">HDR [page]</p></body></html>"#,
    )
    .unwrap();

    let bytes = run_cli_with(
        TWO_PAGE_HTML,
        &[
            "--no-pdf-compression",
            "--header-html",
            header.to_str().unwrap(),
        ],
        "header-html",
    );

    assert_eq!(count_occurrences(&bytes, b"/MediaBox"), 2);
    // ヘッダーHTMLの赤文字が両ページに出る。
    assert!(
        count_occurrences(&bytes, b"1 0 0 rg") >= 2,
        "the header sub-document should be drawn on every page"
    );
    // 余白からのはみ出しを切るクリップが入る。
    assert!(
        count_occurrences(&bytes, b"W\n") >= 2,
        "overlay must be clipped"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn header_html_takes_precedence_over_the_simple_option() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-hf-both-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let header = dir.join("header.html");
    std::fs::write(
        &header,
        r#"<html><body style="margin:0"><p style="color:#ff0000">FROM HTML</p></body></html>"#,
    )
    .unwrap();

    // 同じ「上側」に両方指定した場合、HTMLだけが描かれる。
    let bytes = run_cli_with(
        PLAIN_HTML,
        &[
            "--no-pdf-compression",
            "--header-html",
            header.to_str().unwrap(),
            "--header-center",
            "FROM OPTION",
        ],
        "hf-both",
    );
    assert!(count_occurrences(&bytes, b"1 0 0 rg") > 0);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn topage_is_rejected_in_streaming_mode() {
    // 総ページ数はストリーミングでは決まらない。
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .args(["--streaming", "--footer-center", "of [topage]"])
        .arg("-o")
        .arg(temp_output_path("streaming-topage"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(3), "should be a render error");
}

#[test]
fn replace_substitutes_custom_placeholders() {
    let bytes = run_cli_with(
        PLAIN_HTML,
        &[
            "--footer-center",
            "[customer] 御中",
            "--replace",
            "customer=わか商店",
        ],
        "hf-replace",
    );
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn a_malformed_replace_is_a_usage_error() {
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .args(["--replace", "no-equals-sign"])
        .arg("-o")
        .arg(temp_output_path("bad-replace"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(1));
}

// ---------------------------------------------------------------------------
// ===== cover と TOC =====
// ---------------------------------------------------------------------------

/// 見出しを持ち、本文が2ページになるHTML。
const TOC_DOC_HTML: &str = r#"<html><body>
    <h1 id="intro">Introduction</h1><p>text</p>
    <h2>Background</h2><p>text</p>
    <h1>Second chapter</h1><p style="break-before: page">on page two</p>
</body></html>"#;

const COVER_HTML: &str = r#"<html><body><h1>COVER PAGE</h1></body></html>"#;
/// テキストを持たない表紙(ヘッダー/フッターの有無を数えるため)。
const BLANK_COVER_HTML: &str = r#"<html><body><div style="height:10px"></div></body></html>"#;

fn write_temp_html(dir: &std::path::Path, name: &str, html: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, html).unwrap();
    path
}

#[test]
fn toc_adds_pages_and_links_to_every_heading() {
    let without = run_cli_with(TOC_DOC_HTML, &[], "toc-without");
    let with = run_cli_with(TOC_DOC_HTML, &["--toc"], "toc-with");

    assert_eq!(
        count_occurrences(&without, b"/MediaBox"),
        2,
        "body is 2 pages"
    );
    assert_eq!(
        count_occurrences(&with, b"/MediaBox"),
        3,
        "the TOC should add one page in front"
    );
    // 見出し3つ分のリンク注釈が目次に張られる。
    assert_eq!(count_occurrences(&with, b"/Subtype /Link"), 3);
    // 名前付き宛先も作られる(id無しの見出しには自動命名)。
    assert!(count_occurrences(&with, b"/Dests") > 0);
}

#[test]
fn disable_toc_links_drops_the_link_annotations() {
    let bytes = run_cli_with(
        TOC_DOC_HTML,
        &["--toc", "--disable-toc-links"],
        "toc-nolinks",
    );
    assert_eq!(count_occurrences(&bytes, b"/MediaBox"), 3);
    assert_eq!(count_occurrences(&bytes, b"/Subtype /Link"), 0);
}

#[test]
fn a_cover_page_is_not_counted_and_has_no_header_or_footer() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-cover-{}", std::process::id()));
    let blank = write_temp_html(&dir, "cover.html", BLANK_COVER_HTML);

    // フッターに総ページ数を出す。coverを足してもその値は変わらないはず。
    let base_args = ["--no-pdf-compression", "--footer-center", "[topage]"];
    let without = run_cli_with(TOC_DOC_HTML, &base_args, "cover-without");
    let with = run_cli_with(
        TOC_DOC_HTML,
        &[
            "--no-pdf-compression",
            "--footer-center",
            "[topage]",
            "--cover",
            blank.to_str().unwrap(),
        ],
        "cover-with",
    );

    assert_eq!(count_occurrences(&without, b"/MediaBox"), 2);
    assert_eq!(
        count_occurrences(&with, b"/MediaBox"),
        3,
        "the cover adds a physical page"
    );

    // テキストを持たない表紙なので、描画されたテキストの数が増えていなければ
    // 「表紙にフッターが描かれていない」ことになる。
    let text_ops = |pdf: &[u8]| count_occurrences(pdf, b"Tj") + count_occurrences(pdf, b"TJ");
    assert_eq!(
        text_ops(&with),
        text_ops(&without),
        "the cover must not get a header/footer"
    );

    // 総ページ数(counter(pages))は表紙を含めず2のまま。
    assert!(
        count_occurrences(&with, b"<0032>") > 0,
        "total pages should be 2"
    );
    assert_eq!(
        count_occurrences(&with, b"<0033>"),
        0,
        "the cover must not be counted in counter(pages)"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_cover_renders_its_own_content() {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-cover2-{}", std::process::id()));
    let cover = write_temp_html(&dir, "cover.html", COVER_HTML);

    let plain = run_cli_with(TOC_DOC_HTML, &["--no-pdf-compression"], "cover-plain");
    let with = run_cli_with(
        TOC_DOC_HTML,
        &["--no-pdf-compression", "--cover", cover.to_str().unwrap()],
        "cover-content",
    );

    let text_ops = |pdf: &[u8]| count_occurrences(pdf, b"Tj") + count_occurrences(pdf, b"TJ");
    assert!(
        text_ops(&with) > text_ops(&plain),
        "the cover's own text must be drawn"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn page_offset_shifts_the_numbering() {
    // `--page-offset 10`にすると最初の本文ページが11になる。
    let bytes = run_cli_with(
        TOC_DOC_HTML,
        &[
            "--no-pdf-compression",
            "--footer-center",
            "[page]",
            "--page-offset",
            "10",
        ],
        "page-offset",
    );
    // 「11」「12」に含まれる数字1(U+0031)がToUnicodeに現れる。
    assert!(count_occurrences(&bytes, b"<0031>") > 0);
}

#[test]
fn toc_is_rejected_in_streaming_mode() {
    let out = Command::new(BIN)
        .arg(SAMPLE_HTML)
        .arg("--font")
        .arg(FONT_PATH)
        .args(["--streaming", "--toc"])
        .arg("-o")
        .arg(temp_output_path("streaming-toc"))
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--toc"));
}

#[test]
fn a_document_without_headings_still_produces_a_toc_page() {
    let bytes = run_cli_with(PLAIN_HTML, &["--toc"], "toc-empty");
    assert_eq!(
        count_occurrences(&bytes, b"/MediaBox"),
        2,
        "an empty TOC still occupies one page"
    );
    assert_eq!(count_occurrences(&bytes, b"/Subtype /Link"), 0);
}

#[test]
fn toc_appearance_options_are_accepted() {
    let bytes = run_cli_with(
        TOC_DOC_HTML,
        &[
            "--toc",
            "--toc-header-text",
            "目次",
            "--toc-level-indentation",
            "2em",
            "--toc-text-size-shrink",
            "0.5",
            "--disable-dotted-lines",
            "--enable-toc-back-links",
        ],
        "toc-appearance",
    );
    assert_eq!(count_occurrences(&bytes, b"/MediaBox"), 3);
    assert!(bytes.starts_with(b"%PDF-"));
}

#[test]
fn unsupported_wkhtmltopdf_options_explain_why() {
    // 黙って無視せず、理由と代替手段を示してexit 1。
    for (option, expected) in [
        ("--enable-javascript", "JavaScript"),
        ("--outline", "--toc"),
        ("--xsl-style-sheet", "--user-style-sheet"),
        ("--image-quality", "画像"),
        ("--proxy", "プロキシ"),
        ("--enable-forms", "フォーム"),
    ] {
        let out = Command::new(BIN)
            .arg(SAMPLE_HTML)
            .arg(option)
            .arg("dummy")
            .output()
            .expect("failed to run sghtmltopdf binary");

        assert_eq!(out.status.code(), Some(1), "{option} should exit with 1");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(option) && stderr.contains("対応していません"),
            "{option}: message should name the option, got: {stderr}"
        );
        assert!(
            stderr.contains(expected),
            "{option}: message should explain the alternative/cause, got: {stderr}"
        );
    }
}

#[test]
fn a_supported_option_is_not_mistaken_for_an_unsupported_one() {
    // `--toc`は対応済み。非対応リストの誤爆がないことを確認する。
    let bytes = run_cli_with(PLAIN_HTML, &["--toc"], "not-unsupported");
    assert!(bytes.starts_with(b"%PDF-"));
}

// ---------------------------------------------------------------------------
// ストリーミングモードで「黙って結果が変わる」箇所の警告
// ---------------------------------------------------------------------------

/// 警告を確認するため、stderrを取れる形でCLIを走らせる。
fn run_capturing_stderr(html: &str, extra_args: &[&str], name: &str) -> String {
    let dir = std::env::temp_dir().join(format!("sghtmltopdf-e2e-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.html");
    std::fs::write(&input, html).unwrap();

    let out = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(FONT_PATH)
        .arg("-o")
        .arg(dir.join("out.pdf"))
        .args(extra_args)
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert!(
        out.status.success(),
        "CLI should still succeed for case {name}"
    );

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    std::fs::remove_dir_all(&dir).ok();
    stderr
}

#[test]
fn streaming_warns_when_a_font_family_cannot_be_resolved() {
    // ストリーミングではフォントを処理開始時に固定するため、`font-family`名
    // からのシステムフォント探索ができない。黙って既定フォントで描かれるので
    // 警告を出す。
    let html = r#"<html><body><p style="font-family: monospace">mono</p></body></html>"#;

    let streaming = run_capturing_stderr(html, &["--streaming"], "warn-font-streaming");
    assert!(
        streaming.contains("警告") && streaming.contains("monospace"),
        "streaming should warn about the unresolved family, got: {streaming}"
    );

    // バッチモードは実際に解決できるので警告しない。
    let batch = run_capturing_stderr(html, &[], "warn-font-batch");
    assert!(
        !batch.contains("警告"),
        "batch mode resolves it and must stay quiet, got: {batch}"
    );
}

#[test]
fn streaming_warns_about_selectors_that_need_later_siblings() {
    // 「この先に同じ型の要素が続くか」が要るセレクタは、<body>直下の要素に
    // ついてバッチと結果が変わる。
    let html = r#"<html><head><style>
            p:nth-last-child(2) { color: blue }
            div:has(~ h1) { color: green }
        </style></head><body><p>x</p></body></html>"#;

    let streaming = run_capturing_stderr(html, &["--streaming"], "warn-selector-streaming");
    assert!(streaming.contains("警告"), "got: {streaming}");
    assert!(streaming.contains(":nth-last-child"), "got: {streaming}");
    assert!(streaming.contains(":has(~"), "got: {streaming}");

    let batch = run_capturing_stderr(html, &[], "warn-selector-batch");
    assert!(!batch.contains("警告"), "got: {batch}");
}

/// ストリーミングでもバッチと同じ結果になるセレクタは警告しない。
/// 過剰に警告すると、外す必要のない利用者にまで`--streaming`を諦めさせる。
#[test]
fn streaming_stays_quiet_for_selectors_that_keep_working() {
    let html = r#"<html><head><style>
            li:last-child { color: red }
            div:empty { color: green }
            section:has(h1) { color: blue }
            h1 + p { color: teal }
            li:first-child { color: navy }
        </style></head><body><p>x</p></body></html>"#;

    let streaming = run_capturing_stderr(html, &["--streaming"], "warn-selector-safe");
    assert!(!streaming.contains("警告"), "got: {streaming}");
}

#[test]
fn streaming_stays_quiet_when_everything_is_resolvable() {
    let html = r#"<html><body><p>plain</p></body></html>"#;
    let stderr = run_capturing_stderr(html, &["--streaming", "--quiet"], "warn-none");
    assert!(stderr.is_empty(), "no warning expected, got: {stderr}");
}

/// A real colour emoji font is used, and its glyphs come out as a Type 3 font.
///
/// The font has no outlines at all, so before colour font support it was
/// declined outright and the emoji silently disappeared. Now the emoji is
/// drawn from the font's embedded bitmaps, while the font program itself is
/// still never embedded (subsetting cannot shrink a font with no `glyf`, so
/// embedding it would drag the whole 10MB file into the PDF).
#[test]
fn a_colour_emoji_font_renders_its_bitmaps_without_embedding_the_font() {
    let dir = std::env::temp_dir().join(format!(
        "sghtmltopdf-e2e-{}-color-emoji",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let input = dir.join("input.html");
    std::fs::write(&input, "<html><body><p>A \u{1F389} B</p></body></html>").unwrap();
    let output = dir.join("out.pdf");

    let out = Command::new(BIN)
        .arg(&input)
        .arg("--font")
        .arg(COLOR_EMOJI_FONT_PATH)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("failed to run sghtmltopdf binary");
    assert!(out.status.success());

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        !stderr.contains("NotoColorEmoji.ttf"),
        "the font is usable now, so nothing should be said about declining it: {stderr}"
    );
    assert!(
        !stderr.contains('\u{1F389}'),
        "the emoji is drawable, so it must not be reported as uncovered: {stderr}"
    );

    let bytes = std::fs::read(&output).expect("output PDF should exist");
    assert!(bytes.starts_with(b"%PDF-"));
    assert!(
        count_occurrences(&bytes, b"/Subtype /Type3") >= 1,
        "the emoji should be drawn by a Type 3 font"
    );
    let source_size = std::fs::metadata(COLOR_EMOJI_FONT_PATH).unwrap().len();
    assert!(
        (bytes.len() as u64) < source_size / 10,
        "the bitmap font must not be embedded. PDF={} source font={source_size}",
        bytes.len()
    );

    std::fs::remove_dir_all(&dir).ok();
}
