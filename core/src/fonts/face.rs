//! `@font-face`ルールから実際のフォントファイルを読み込む。
//!
//! `src: url(...)`は`<img>`・`<link>`・`@import`と同じ[`ImageFetcher`]で解決・
//! 取得する。したがってローカルパス(HTMLファイル自身のディレクトリが基準)・
//! `http(s)`・`data:`URIのいずれも扱え、`<base href>`やローカル/リモートの
//! アクセス制御も同じ規則が適用される。`src: local(...)`はシステムフォントの
//! フルネーム/PostScript名として[`super::system::SystemFonts`]から解決する。

use cssparser::UnicodeRange;

use crate::img::ImageFetcher;
use crate::style::{FontFaceRule, FontFaceSource, FontStyle, FontWeight};

use super::font::{warn_font_without_outlines, Font};
use super::system::SystemFonts;

/// `@font-face`から読み込めたフォントと、CSS側で宣言されたfamily名・weight・style・unicode-range。
pub struct LoadedFontFace {
    pub family: String,
    pub weight: FontWeight,
    pub style: FontStyle,
    pub unicode_range: Vec<UnicodeRange>,
    pub font: Font,
}

/// `font_faces`それぞれについて、`src`に列挙された`url(...)`/`local(...)`を
/// 先頭から順に試し、最初に読み込めたものを採用する(`format()`ヒントは検証
/// しないため、非対応フォーマット(WOFF/WOFF2等)は単にパース失敗として次の
/// 候補に読み進める)。どの`src`も読み込めなかった`@font-face`ルールは標準
/// エラー出力に警告を出して無視する(1つのフォントの欠落のために変換全体を
/// 失敗させない)。
pub fn load_font_faces(
    font_faces: &[FontFaceRule],
    fetcher: &ImageFetcher,
    system: &SystemFonts,
) -> Vec<LoadedFontFace> {
    font_faces
        .iter()
        .filter_map(|rule| load_one(rule, fetcher, system))
        .collect()
}

fn load_one(
    rule: &FontFaceRule,
    fetcher: &ImageFetcher,
    system: &SystemFonts,
) -> Option<LoadedFontFace> {
    for src in &rule.src {
        let font = match src {
            // 読み込みは`<img>`・`<link>`・`@import`と同じ[`ImageFetcher`]を通す。
            // 分類も同じ`resolve`に任せる(ここで`LocalPath`と決め打ちすると
            // `data:`URIがファイルパス扱いになって必ず失敗する)。
            FontFaceSource::Url(raw) => fetcher
                .resolve(raw)
                .and_then(|src| fetcher.fetch(&src).ok())
                .and_then(|bytes| Font::from_bytes(bytes, 0).ok()),
            FontFaceSource::Local(name) => system.load_by_full_name(name),
        };
        if let Some(font) = font {
            // 読み込めても輪郭が無ければ何も描けないので採らず、次のsrcへ進む。
            if !font.has_outlines() {
                warn_font_without_outlines(&format!("@font-face \"{}\"のsrc", rule.family));
                continue;
            }
            return Some(LoadedFontFace {
                family: rule.family.clone(),
                weight: rule.weight,
                style: rule.style,
                unicode_range: rule.unicode_range.clone(),
                font,
            });
        }
    }
    eprintln!(
        "警告: @font-face \"{}\"の読み込みに失敗しました(有効なsrcが見つかりません)",
        rule.family
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::{FontStyle, FontWeight};
    use std::path::Path;

    const DEJAVU_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts");

    /// テスト用のフェッチャ。
    /// 既定はCLIと同じ「ローカル読み込み可・許可ディレクトリの限定なし」。
    fn fetcher() -> ImageFetcher {
        ImageFetcher::new(Path::new(DEJAVU_PATH).to_path_buf(), false)
    }

    fn rule(family: &str, src: Vec<FontFaceSource>) -> FontFaceRule {
        FontFaceRule {
            family: family.to_string(),
            src,
            weight: FontWeight::Normal,
            style: FontStyle::Normal,
            unicode_range: Vec::new(),
        }
    }

    /// `tests/fonts`配下のフォントを、そのまま`data:`URIに埋め込んだ文字列にする。
    fn data_uri(file_name: &str) -> String {
        use base64::Engine;

        let bytes = std::fs::read(Path::new(DEJAVU_PATH).join(file_name)).expect("test font");
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        format!("data:font/ttf;base64,{encoded}")
    }

    fn no_system_fonts() -> SystemFonts {
        // ローカルの空ディレクトリを走査させ、システムフォントが1つも
        // 無い状態を作る(local()解決の対象外テスト用)。
        SystemFonts::from_dir(Path::new(DEJAVU_PATH).join("does-not-exist").as_path())
    }

    #[test]
    fn loads_a_font_from_a_relative_url() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    #[test]
    fn resolves_a_root_relative_url_within_base_dir() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("/DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    /// フォント本体をbase64で埋め込んだ`data:`URI。文字列入力やHTTPサーバ
    /// 経由の変換では、フォントを自己完結させる唯一の手段になる。
    #[test]
    fn loads_a_font_from_a_data_uri() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url(data_uri("DejaVuSansMono.ttf"))],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    /// `data:`URIはファイルを読まないので、ローカル読み込みを禁止しても使えること。
    #[test]
    fn a_data_uri_source_survives_disabled_local_file_access() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url(data_uri("DejaVuSansMono.ttf"))],
        )];
        let blocked = fetcher().with_local_access(false, Vec::new());

        let loaded = load_font_faces(&rules, &blocked, &no_system_fonts());
        assert_eq!(loaded.len(), 1);
    }

    /// `<base href>`が`@font-face`の`url()`にも効くこと。
    #[test]
    fn resolves_a_url_source_against_base_href() {
        // base_dirをtests/に置き、fonts/への前置を`<base href>`に担わせる。
        let base = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).to_path_buf();
        let based = ImageFetcher::new(base, false).with_base_href(Some("fonts/".to_string()));

        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &based, &no_system_fonts());

        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn falls_through_to_the_next_src_when_the_first_one_is_missing() {
        let rules = vec![rule(
            "Custom Brand",
            vec![
                FontFaceSource::Url("does-not-exist.ttf".to_string()),
                FontFaceSource::Url("DejaVuSans.ttf".to_string()),
            ],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    #[test]
    fn skips_a_font_face_rule_whose_sources_are_all_unusable() {
        let rules = vec![rule(
            "Missing Brand",
            vec![FontFaceSource::Url("does-not-exist.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &no_system_fonts());

        assert!(loaded.is_empty());
    }

    #[test]
    fn resolves_local_source_from_the_system_font_database() {
        let system = SystemFonts::from_dir(Path::new(DEJAVU_PATH));
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Local("DejaVu Sans".to_string())],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &system);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }

    /// `--disable-local-file-access`が`@font-face`の`url()`にも効くこと。
    #[test]
    fn a_url_source_is_refused_when_local_file_access_is_disabled() {
        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let blocked = ImageFetcher::new(Path::new(DEJAVU_PATH).to_path_buf(), false)
            .with_local_access(false, Vec::new());

        let loaded = load_font_faces(&rules, &blocked, &no_system_fonts());
        assert!(
            loaded.is_empty(),
            "ローカル読み込みを禁止したらurl()のフォントも読めてはならない"
        );
    }

    /// A font outside the `--allow-path` directories must not be readable, even
    /// by way of `..`.
    #[test]
    fn a_url_source_outside_the_allowed_dirs_is_refused() {
        let base = Path::new(DEJAVU_PATH).to_path_buf();
        // base_dir自身は許可せず、その下の存在しないサブディレクトリだけ許可する。
        let allowed = vec![base.join("allowed-subdir")];
        let restricted = ImageFetcher::new(base, false).with_local_access(true, allowed);

        let rules = vec![rule(
            "Custom Brand",
            vec![FontFaceSource::Url("DejaVuSans.ttf".to_string())],
        )];
        let loaded = load_font_faces(&rules, &restricted, &no_system_fonts());
        assert!(
            loaded.is_empty(),
            "--allow-pathの範囲外にあるフォントは読めてはならない"
        );
    }

    #[test]
    fn falls_through_from_an_unresolvable_local_source_to_a_url() {
        let system = SystemFonts::from_dir(Path::new(DEJAVU_PATH));
        let rules = vec![rule(
            "Custom Brand",
            vec![
                FontFaceSource::Local("Definitely Not Installed".to_string()),
                FontFaceSource::Url("DejaVuSans.ttf".to_string()),
            ],
        )];
        let loaded = load_font_faces(&rules, &fetcher(), &system);

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].family, "Custom Brand");
    }
}
