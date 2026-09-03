//! 外部リソースのバイト列取得。ローカルファイル/HTTP(S)/`data:` URIを[`ImgSrc`]の分類に従って統一的に扱う。
//!
//! `<img src>`のほか、`<link rel=stylesheet>`・`@import`・`@font-face`の
//! `url()`もすべてここを通る(取得元の信頼境界とサイズ上限を1箇所に集約
//! するため)。

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{Agent, Error as UreqError};

use super::{resolve_local_asset_path, ImgSrc, ResolvedAssetPath};

/// 取得したバイト列の既定上限(20MiB)。ローカル/リモート/data:のいずれの
/// 取得元にも同じ上限を適用する(軽量・低メモリという設計方針上、非HTTP
/// 経由だからといって無制限にする理由が無いため)。
const DEFAULT_MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// 取得の失敗理由。何を取りに行って失敗したのかは呼び出し側が知っている
/// (画像・外部スタイルシート・`@import`・`@font-face`)ので、ここでは
/// 理由だけを持ち、種別を表す前置きは付けない。
#[derive(Debug)]
pub struct FetchError(String);

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FetchError {}

/// Which file a local reference resolves to, and how it should be judged.
struct LocalTarget {
    /// The path to read.
    path: PathBuf,
    /// Whether it lies outside `base_dir` (refused unless `--allow` says so).
    escapes_base_dir: bool,
    /// The other candidate, set only when neither existed, so that the error
    /// can name it.
    also_tried: Option<PathBuf>,
}

/// `<img>`のバイト列取得を担う。文書ごとに1つ構築し、複数の`<img>`要素の
/// 取得で使い回す想定(`ureq::Agent`の内部コネクションプーリングも活かせる)。
pub struct ImageFetcher {
    /// ローカル相対パスの基準ディレクトリ(`@font-face`のurl()解決と同じ
    /// 役割)。
    base_dir: PathBuf,
    /// リモート(http/https)フェッチを許可するかどうか。既定は無効
    /// (「既定無効・明示オプトイン」方針)。`data:`/ローカルパスはこの値に
    /// 関わらず常に許可する(ネットワークを介さない、
    /// または`@font-face`と同じ信頼境界のため)。
    allow_remote: bool,
    max_bytes: u64,
    agent: Agent,
    /// `<base href>`の値。相対参照はフェッチ前にこれに対して解決される。
    /// `<img src>`・`<link href>`・`@import`はいずれもこのフェッチャを
    /// 共有するため、ここ1箇所で3種類すべてに効く。
    base_href: Option<String>,
    /// ローカルファイル参照を許すか(`--disable-local-file-access`でfalse)。
    allow_local: bool,
    /// 空でなければ、ローカル参照をこのディレクトリ配下に限定する
    /// (`--allow`)。
    allowed_dirs: Vec<PathBuf>,
}

impl ImageFetcher {
    /// サイズ上限は[`DEFAULT_MAX_IMAGE_BYTES`]を使う。
    pub fn new(base_dir: PathBuf, allow_remote: bool) -> Self {
        Self::with_max_bytes(base_dir, allow_remote, DEFAULT_MAX_IMAGE_BYTES)
    }

    /// `<base href>`を設定した同じフェッチャを返す(ビルダー的に使う)。
    pub fn with_base_href(mut self, base_href: Option<String>) -> Self {
        self.base_href = base_href.filter(|href| !href.trim().is_empty());
        self
    }

    /// ローカルファイルの読み込み可否と、許可ディレクトリ(`--allow`)を設定する。
    ///
    /// `allow_local`が`false`のとき、ローカルパス参照はすべて拒否する
    /// (HTTPサーバモードの既定を想定)。`allowed_dirs`が空でなければ、
    /// 解決後のパスがそのいずれかの配下に無い参照を拒否する。
    pub fn with_local_access(mut self, allow_local: bool, allowed_dirs: Vec<PathBuf>) -> Self {
        self.allow_local = allow_local;
        // 実パスへの解決はここで1回だけ行う。参照のたびに解決すると、
        // 失敗したときに生のパスでの比較へ落ちてしまうため。
        //
        // 解決できなかったものはそのまま残すが、比較相手(参照先)は必ず
        // 実パスなので一致せず、拒否側に倒れる。CLIから来る場合は
        // `ConvertArgs::local_access`が解決済みかつ検証済みのものを渡す。
        self.allowed_dirs = allowed_dirs
            .into_iter()
            .map(|dir| dir.canonicalize().unwrap_or(dir))
            .collect();
        self
    }

    /// 生の参照値を`<base href>`に対して解決し、URL/パスとして分類する。
    /// フェッチ経路はすべてここを通す。
    pub fn resolve(&self, raw: &str) -> Option<super::ImgSrc> {
        let resolved = super::resolve_against_base_href(self.base_href.as_deref(), raw);
        super::classify_img_src(&resolved)
    }

    pub fn with_max_bytes(base_dir: PathBuf, allow_remote: bool, max_bytes: u64) -> Self {
        let config = Config::builder()
            .max_redirects(5)
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_global(Some(Duration::from_secs(15)))
            .build();
        let agent = Agent::with_parts(
            config,
            DefaultConnector::default(),
            PolicyResolver {
                inner: DefaultResolver::default(),
            },
        );
        Self {
            base_dir,
            allow_remote,
            max_bytes,
            agent,
            base_href: None,
            allow_local: true,
            allowed_dirs: Vec::new(),
        }
    }

    /// `src`の分類([`ImgSrc`])に応じてバイト列を取得する。
    pub fn fetch(&self, src: &ImgSrc) -> Result<Vec<u8>, FetchError> {
        match src {
            ImgSrc::LocalPath(path) => self.read_local(path),
            ImgSrc::RemoteUrl(url) => self.fetch_remote(url),
            ImgSrc::DataUri { bytes, .. } => {
                self.ensure_within_limit(bytes.len() as u64)?;
                Ok(bytes.clone())
            }
        }
    }

    /// Picks which of the resolved candidates to read, and says whether it lies
    /// outside `base_dir`.
    ///
    /// The site-root reading of `raw` comes first, so a document that works
    /// today keeps working. Only when that file does not exist is `raw` taken
    /// as a filesystem path, which is what `<img src="/Users/me/app/logo.png">`
    /// means. Reading it still has to get past `--allow`, exactly like any
    /// other reference outside base_dir.
    fn choose_candidate(&self, resolved: ResolvedAssetPath) -> LocalTarget {
        if resolved.path.exists() {
            return LocalTarget {
                path: resolved.path,
                escapes_base_dir: resolved.escapes_base_dir,
                also_tried: None,
            };
        }
        match resolved.absolute {
            Some(absolute) if absolute.exists() => LocalTarget {
                escapes_base_dir: !self.is_within_base_dir(&absolute),
                path: absolute,
                also_tried: None,
            },
            // Neither candidate exists. Report the site-root one, the reading
            // the reference was written for, and name the other one so that a
            // filesystem path does not just look like a nonsense join.
            other => LocalTarget {
                path: resolved.path,
                escapes_base_dir: resolved.escapes_base_dir,
                also_tried: other,
            },
        }
    }

    /// Whether `path` is inside `base_dir`, compared as real paths (`base_dir`
    /// may be relative, as it is with the CLI default of the input's own
    /// directory). A path that cannot be resolved counts as outside, so that an
    /// unclear case needs `--allow`.
    fn is_within_base_dir(&self, path: &Path) -> bool {
        let (Ok(base), Ok(candidate)) = (self.base_dir.canonicalize(), path.canonicalize()) else {
            return false;
        };
        candidate.starts_with(base)
    }

    /// The error for a reference that resolved to nothing readable.
    ///
    /// `/…`は`--base-url`からのサイトルート相対として解決するため、
    /// ファイルシステムの絶対パスを書いたつもりの人に連結後のパスだけを
    /// 見せても、何が起きたのか分からない。両方を挙げる。
    fn not_found(path: &Path, also_tried: &Option<PathBuf>, error: std::io::Error) -> FetchError {
        match also_tried {
            Some(absolute) => FetchError(format!(
                "{}: {error}\n  \
                 (`/`で始まる参照は --base-url からのサイトルート相対として解決します。\n  \
                 絶対パス {} としても探しましたが、そちらもありませんでした)",
                path.display(),
                absolute.display()
            )),
            None => FetchError(format!("{}: {error}", path.display())),
        }
    }

    /// Reads a local file, relative to `base_dir`.
    /// A reference that leaves base_dir through `..` is refused by
    /// [`resolve_local_asset_path`]; name the directories to be read on purpose
    /// with `--allow`.
    fn read_local(&self, path: &str) -> Result<Vec<u8>, FetchError> {
        if !self.allow_local {
            return Err(FetchError(
                "ローカルファイルの読み込みは許可されていません(--enable-local-file-access)"
                    .to_string(),
            ));
        }
        let resolved = resolve_local_asset_path(&self.base_dir, path);
        let LocalTarget {
            path: full_path,
            escapes_base_dir,
            also_tried,
        } = self.choose_candidate(resolved);
        // Without `--allow`, base_dir is the boundary itself. With it, the
        // allowed directories decide, so leaving base_dir is let through here
        // and judged below.
        if escapes_base_dir && self.allowed_dirs.is_empty() {
            return Err(FetchError(format!(
                "基準ディレクトリ({})の外を参照しています。\n  \
                 外部のファイルを読む場合は --allow でディレクトリを明示してください",
                self.base_dir.display()
            )));
        }
        if !self.allowed_dirs.is_empty() {
            // 実パスに解決できなければ「許可範囲内だと確認できなかった」として
            // 拒否する。生のパスへフォールバックすると`..`を含んだまま
            // `starts_with`することになり、`/var/www/../etc/passwd`が
            // `/var/www`配下と判定されてしまう(比較はパス文字列上の
            // コンポーネント単位で、ファイルシステムを見ないため)。
            let canonical = full_path
                .canonicalize()
                .map_err(|e| Self::not_found(&full_path, &also_tried, e))?;
            if !self
                .allowed_dirs
                .iter()
                .any(|dir| canonical.starts_with(dir))
            {
                return Err(FetchError(format!(
                    "{}: --allowで許可されたディレクトリの外です",
                    full_path.display()
                )));
            }
        }
        let metadata = std::fs::metadata(&full_path)
            .map_err(|e| Self::not_found(&full_path, &also_tried, e))?;
        self.ensure_within_limit(metadata.len()).map_err(|_| {
            FetchError(format!(
                "{}: ファイルサイズが上限を超えています",
                full_path.display()
            ))
        })?;
        std::fs::read(&full_path).map_err(|e| FetchError(format!("{}: {e}", full_path.display())))
    }

    fn fetch_remote(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        if !self.allow_remote {
            return Err(FetchError(
                "リモート取得は既定で無効です(--allow-remote-assetsで許可してください)".to_string(),
            ));
        }
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|e| FetchError(e.to_string()))?;
        response
            .body_mut()
            .with_config()
            .limit(self.max_bytes)
            .read_to_vec()
            .map_err(|e| FetchError(e.to_string()))
    }

    fn ensure_within_limit(&self, len: u64) -> Result<(), FetchError> {
        if len > self.max_bytes {
            return Err(FetchError(format!(
                "サイズが上限({}バイト)を超えています",
                self.max_bytes
            )));
        }
        Ok(())
    }
}

/// グローバルに到達可能でないIPかどうかを判定する。
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

/// グローバルに到達可能でないIPv4かどうかを判定する。
///
/// 標準ライブラリの述語だけでは足りない範囲があるので、CIDRを直接見る
/// (`is_shared`・`is_reserved`・`is_benchmarking`等は安定化されていない)。
fn is_blocked_ipv4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = v4.octets();

    // 標準ライブラリで判定できるもの。
    // private(10/8・172.16/12・192.168/16)、loopback(127/8)、
    // link-local(169.254/16。クラウドのメタデータ169.254.169.254を含む)、
    // multicast(224/4)、broadcast(255.255.255.255)、
    // documentation(192.0.2/24・198.51.100/24・203.0.113/24)。
    if v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_documentation()
    {
        return true;
    }

    // 0.0.0.0/8 "this network"。`is_unspecified`は0.0.0.0ちょうどしか見ない
    // が、0.x.y.z全体が非グローバルで、Linuxでは0.0.0.1のような宛先が
    // ローカルへ向かう。
    if a == 0 {
        return true;
    }
    // 100.64.0.0/10 CGNAT。クラウドの内部ロードバランサやKubernetesの
    // ネットワークで使われるため、外形上はグローバルに見えても内部を指す。
    if a == 100 && (64..128).contains(&b) {
        return true;
    }
    // 192.0.0.0/24 IETF Protocol Assignments。
    if v4.octets()[..3] == [192, 0, 0] {
        return true;
    }
    // 198.18.0.0/15 ベンチマーク用。
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    // 240.0.0.0/4 予約済み(255.255.255.255のbroadcastは上で弾いている)。
    if a >= 240 {
        return true;
    }

    false
}

/// グローバルに到達可能でないIPv6かどうかを判定する。
fn is_blocked_ipv6(v6: std::net::Ipv6Addr) -> bool {
    // `to_ipv4`はIPv4-mapped(`::ffff:a.b.c.d`)に加えて、非推奨の
    // IPv4-compatible(`::a.b.c.d`)も拾う。`to_ipv4_mapped`だけだと
    // 後者が素通りする。
    if let Some(v4) = v6.to_ipv4() {
        return is_blocked_ipv4(v4);
    }

    let segments = v6.segments();

    // 64:ff9b::/96(NAT64のwell-known prefix)と64:ff9b:1::/48(local-use)。
    // 下位32ビットにIPv4が埋まっており、NAT64ゲートウェイがある環境では
    // これ経由でIPv4側へ到達できる。
    if segments[0] == 0x0064 && segments[1] == 0xff9b {
        let [a, b, c, d] = (((segments[6] as u32) << 16) | segments[7] as u32).to_be_bytes();
        return is_blocked_ipv4(std::net::Ipv4Addr::new(a, b, c, d));
    }
    // 2002::/16(6to4)。第2〜3セグメントに埋め込まれたIPv4を見る。
    if segments[0] == 0x2002 {
        let [a, b, c, d] = (((segments[1] as u32) << 16) | segments[2] as u32).to_be_bytes();
        return is_blocked_ipv4(std::net::Ipv4Addr::new(a, b, c, d));
    }

    // 2001::/32(Teredo)。クライアント側のIPv4が難読化して埋め込まれて
    // おり、復元してまで通す価値がないのでプレフィクスごと弾く。
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return true;
    }
    // 2001:db8::/32 ドキュメント用。
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return true;
    }
    // 2001:20::/28 ORCHIDv2。
    if segments[0] == 0x2001 && (0x0020..0x0030).contains(&segments[1]) {
        return true;
    }
    // 100::/64 discard-only。
    if segments[0] == 0x0100 && segments[1..4] == [0, 0, 0] {
        return true;
    }

    // loopback(::1)、multicast(ff00::/8)、unspecified(::)、
    // unique local(fc00::/7)、link-local unicast(fe80::/10)。
    v6.is_loopback()
        || v6.is_multicast()
        || v6.is_unspecified()
        || v6.is_unique_local()
        || v6.is_unicast_link_local()
}

/// 任意の`Resolver`をラップし、解決結果からブロック対象IPを除去する。
/// 1件も残らなければ`Error::HostNotFound`で拒否する(「ブロックされた」と
/// 「そもそも存在しない」を呼び出し元から区別させない)。
#[derive(Debug)]
struct PolicyResolver<R> {
    inner: R,
}

impl<R: Resolver> Resolver for PolicyResolver<R> {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, UreqError> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        let mut filtered: ResolvedSocketAddrs = ResolvedSocketAddrs::from_fn(|_| {
            SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)
        });
        for addr in addrs.iter().filter(|a| !is_blocked_ip(a.ip())) {
            filtered.push(*addr);
        }
        if filtered.is_empty() {
            return Err(UreqError::HostNotFound);
        }
        Ok(filtered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};

    /// `engine.rs`の既存テストと同じ`std::env::temp_dir()`ベースの一時
    /// ディレクトリ作成パターン。呼び出し側が最後に`remove_dir_all`する。
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sghtmltopdf-img-fetch-test-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_a_local_file_relative_to_base_dir() {
        let dir = temp_dir("reads_local");
        std::fs::write(dir.join("logo.png"), b"fake png bytes").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("logo.png".to_string()))
            .expect("local read should succeed");
        assert_eq!(bytes, b"fake png bytes");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_root_relative_local_path_stays_inside_base_dir() {
        // `/logo.png`のようなroot-relativeなsrc(`<link
        // href="/stylesheets/main.css" />`と同種の書き方)が
        // base_dirの外(OSのファイルシステムルート)へ逃げず、base_dir配下の
        // ファイルとして読めることを確認する。
        let dir = temp_dir("root_relative");
        std::fs::write(dir.join("logo.png"), b"fake png bytes").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("/logo.png".to_string()))
            .expect("root-relative local read should succeed within base_dir");
        assert_eq!(bytes, b"fake png bytes");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 既定(`--allow`なし)では、`..`でbase_dirの外へ出る参照を拒否する。
    /// 信頼できないHTMLからの任意ファイル読み出しを塞ぐための既定挙動。
    #[test]
    fn a_local_path_escaping_base_dir_is_refused_by_default() {
        let dir = temp_dir("escape_default");
        let inner = dir.join("pages");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(dir.join("secret.png"), b"outside").unwrap();
        let fetcher = ImageFetcher::new(inner.clone(), false);

        let err = fetcher
            .fetch(&ImgSrc::LocalPath("../secret.png".to_string()))
            .expect_err("base_dirの外は既定で拒否されるべき");
        assert!(
            err.to_string().contains("基準ディレクトリ"),
            "封じ込めによる拒否であること: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `--allow`を指定したときは、その範囲がbase_dirより外でも読める
    #[test]
    fn allow_lets_a_reference_reach_outside_base_dir() {
        let dir = temp_dir("escape_allowed");
        let inner = dir.join("pages");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(dir.join("logo.png"), b"outside but allowed").unwrap();
        let fetcher =
            ImageFetcher::new(inner.clone(), false).with_local_access(true, vec![dir.clone()]);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("../logo.png".to_string()))
            .expect("--allowの範囲内なら読めるべき");
        assert_eq!(bytes, b"outside but allowed");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `--allow`があっても、その範囲の外はやはり読めない。
    #[test]
    fn allow_still_refuses_paths_outside_the_allowed_dirs() {
        let dir = temp_dir("escape_allow_bounds");
        let inner = dir.join("pages");
        let allowed = dir.join("assets");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::write(dir.join("secret.png"), b"outside").unwrap();
        let fetcher =
            ImageFetcher::new(inner.clone(), false).with_local_access(true, vec![allowed]);

        let result = fetcher.fetch(&ImgSrc::LocalPath("../secret.png".to_string()));
        assert!(result.is_err(), "--allowの範囲外は読めてはならない");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 許可ディレクトリは構築時に実パスへ解決される。`..`を含む形で
    /// 渡しても、その中のファイルはちゃんと読める。
    #[test]
    fn allowed_dirs_are_canonicalized_once_at_construction() {
        let dir = temp_dir("allow_canonical");
        let inner = dir.join("pages");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(dir.join("logo.png"), b"allowed").unwrap();

        // `<dir>/pages/..` は実体としては `<dir>`。
        let dotted = inner.join("..");
        let fetcher = ImageFetcher::new(inner.clone(), false).with_local_access(true, vec![dotted]);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("../logo.png".to_string()))
            .expect("解決後のディレクトリ配下なので読めるべき");
        assert_eq!(bytes, b"allowed");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// 実パスに解決できない参照は、`--allow`の判定を通さず拒否する。
    #[test]
    fn a_path_that_cannot_be_resolved_is_refused_instead_of_compared_raw() {
        let dir = temp_dir("allow_unresolvable");
        let allowed = dir.join("assets");
        std::fs::create_dir_all(&allowed).unwrap();
        let fetcher = ImageFetcher::new(allowed.clone(), false)
            .with_local_access(true, vec![allowed.clone()]);

        // 許可ディレクトリ配下に見えるが実在しないパス。
        let result = fetcher.fetch(&ImgSrc::LocalPath("../secret/none.png".to_string()));
        assert!(result.is_err(), "解決できない参照は拒否されるべき");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// base_dirの中で完結する`..`(`assets/../images/x`)は読める。
    #[test]
    fn a_parent_reference_that_stays_inside_base_dir_still_reads() {
        let dir = temp_dir("escape_inside");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("logo.png"), b"inside").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("assets/../logo.png".to_string()))
            .expect("base_dir内で完結する..は許されるべき");
        assert_eq!(bytes, b"inside");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `src` written as a filesystem path that lands inside base_dir is read
    /// as it is: the site-root reading (`<base_dir>/<the whole path>`) does not
    /// exist, so the reference falls back to the path itself, which is inside
    /// base_dir and therefore needs no `--allow`.
    #[test]
    fn an_absolute_path_inside_base_dir_is_read_without_allow() {
        let dir = temp_dir("absolute_inside");
        std::fs::write(dir.join("logo.png"), b"inside").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let src = ImgSrc::LocalPath(dir.join("logo.png").display().to_string());
        let bytes = fetcher
            .fetch(&src)
            .expect("an absolute path inside base_dir should be read");
        assert_eq!(bytes, b"inside");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One that lands outside base_dir is judged like any other reference that
    /// leaves it: refused by default, read once `--allow` names the directory.
    #[test]
    fn an_absolute_path_outside_base_dir_needs_allow() {
        let dir = temp_dir("absolute_outside");
        let base = dir.join("public");
        let outside = dir.join("shared");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("logo.png"), b"outside").unwrap();
        let src = ImgSrc::LocalPath(outside.join("logo.png").display().to_string());

        let refused = ImageFetcher::new(base.clone(), false).fetch(&src);
        let message = refused.expect_err("outside base_dir should be refused").0;
        assert!(message.contains("--allow"), "{message}");

        let allowed = ImageFetcher::new(base.clone(), false)
            .with_local_access(true, vec![outside.clone()])
            .fetch(&src)
            .expect("--allow should let it through");
        assert_eq!(allowed, b"outside");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `/assets/logo.png` keeps meaning `<base_dir>/assets/logo.png` even when a
    /// file of that name exists at the root of the filesystem view: the web
    /// spelling wins, so no document changes meaning.
    #[test]
    fn the_site_root_reading_wins_when_both_candidates_exist() {
        let dir = temp_dir("site_root_wins");
        let base = dir.join("public");
        std::fs::create_dir_all(base.join("assets")).unwrap();
        std::fs::write(base.join("assets/logo.png"), b"site root").unwrap();
        // The same path spelled from the filesystem root also exists.
        let absolute = dir.join("assets");
        std::fs::create_dir_all(&absolute).unwrap();
        std::fs::write(absolute.join("logo.png"), b"filesystem").unwrap();

        let fetcher =
            ImageFetcher::new(base.clone(), false).with_local_access(true, vec![dir.clone()]);
        let src = ImgSrc::LocalPath(dir.join("assets/logo.png").display().to_string());
        // Written from base_dir, the same reference is the site-root one.
        let site_root = ImgSrc::LocalPath("/assets/logo.png".to_string());

        assert_eq!(fetcher.fetch(&site_root).unwrap(), b"site root");
        assert_eq!(fetcher.fetch(&src).unwrap(), b"filesystem");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// When neither candidate exists, the error names both, so that a
    /// filesystem path does not read as a nonsense join.
    #[test]
    fn a_missing_absolute_path_is_named_in_the_error() {
        let dir = temp_dir("absolute_missing");
        let fetcher =
            ImageFetcher::new(dir.clone(), false).with_local_access(true, vec![PathBuf::from("/")]);

        let message = fetcher
            .fetch(&ImgSrc::LocalPath("/no/such/logo.png".to_string()))
            .expect_err("a missing file is an error")
            .0;
        assert!(message.contains("/no/such/logo.png"), "{message}");
        assert!(message.contains("--base-url"), "{message}");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_local_file_is_an_error() {
        let dir = temp_dir("missing_local");
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let result = fetcher.fetch(&ImgSrc::LocalPath("does-not-exist.png".to_string()));
        assert!(result.is_err());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn oversized_local_file_is_rejected() {
        let dir = temp_dir("oversized_local");
        std::fs::write(dir.join("big.png"), b"way too big").unwrap();
        let fetcher = ImageFetcher::with_max_bytes(dir.clone(), false, 4);

        let result = fetcher.fetch(&ImgSrc::LocalPath("big.png".to_string()));
        assert!(
            result.is_err(),
            "file larger than the byte cap should be rejected"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn data_uri_bytes_are_returned_as_is() {
        let fetcher = ImageFetcher::new(Path::new(".").to_path_buf(), false);
        let src = ImgSrc::DataUri {
            mime_type: "image/png".to_string(),
            bytes: b"decoded bytes".to_vec(),
        };
        let bytes = fetcher.fetch(&src).expect("data uri fetch should succeed");
        assert_eq!(bytes, b"decoded bytes");
    }

    #[test]
    fn oversized_data_uri_is_rejected() {
        let fetcher = ImageFetcher::with_max_bytes(Path::new(".").to_path_buf(), false, 4);
        let src = ImgSrc::DataUri {
            mime_type: "image/png".to_string(),
            bytes: b"way too big".to_vec(),
        };
        assert!(fetcher.fetch(&src).is_err());
    }

    fn blocked(ip: &str) -> bool {
        is_blocked_ip(ip.parse().expect("テストのIPリテラルが不正"))
    }

    /// 標準ライブラリの述語で判定できる範囲。
    #[test]
    fn well_known_private_ranges_are_blocked() {
        for ip in [
            "127.0.0.1",       // loopback
            "10.0.0.1",        // private
            "172.16.0.1",      // private
            "192.168.1.1",     // private
            "169.254.169.254", // link-local(クラウドのメタデータ)
            "224.0.0.1",       // multicast
            "255.255.255.255", // broadcast
            "192.0.2.1",       // documentation
            "0.0.0.0",         // unspecified
        ] {
            assert!(blocked(ip), "{ip}は拒否されるべき");
        }
    }

    /// 標準ライブラリの述語では拾えず、以前は素通りしていた範囲。
    #[test]
    fn ranges_missed_by_the_standard_predicates_are_blocked() {
        for (ip, why) in [
            ("0.1.2.3", "0.0.0.0/8 this network"),
            ("100.64.0.1", "100.64.0.0/10 CGNAT"),
            ("100.127.255.254", "CGNATの上端"),
            ("192.0.0.1", "192.0.0.0/24 IETF protocol assignments"),
            ("198.18.0.1", "198.18.0.0/15 ベンチマーク用"),
            ("198.19.255.254", "ベンチマーク用の上端"),
            ("240.0.0.1", "240.0.0.0/4 予約済み"),
        ] {
            assert!(blocked(ip), "{ip}({why})は拒否されるべき");
        }
    }

    /// CGNATの境界。100.63.x と 100.128.x はグローバル。
    #[test]
    fn the_cgnat_boundaries_are_respected() {
        assert!(!blocked("100.63.255.255"), "CGNATの手前はグローバル");
        assert!(blocked("100.64.0.0"), "CGNATの下端");
        assert!(blocked("100.127.255.255"), "CGNATの上端");
        assert!(!blocked("100.128.0.0"), "CGNATの直後はグローバル");
    }

    /// 通常のグローバルアドレスは通ること(過剰に弾いていないかの確認)。
    #[test]
    fn ordinary_global_addresses_are_allowed() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "239.255.255.255", // multicastの上端の直後を確認するため
            "2606:4700:4700::1111",
            "2400:cb00::1",
        ] {
            if ip == "239.255.255.255" {
                // 239/8はmulticastなので拒否が正しい。
                assert!(blocked(ip));
                continue;
            }
            assert!(!blocked(ip), "{ip}は通るべき");
        }
    }

    /// IPv4を埋め込むIPv6表記から、IPv4側のフィルタを迂回できないこと。
    #[test]
    fn ipv6_forms_embedding_ipv4_are_checked_against_the_ipv4_rules() {
        for (ip, why) in [
            ("::ffff:127.0.0.1", "IPv4-mapped"),
            ("::ffff:169.254.169.254", "IPv4-mappedのメタデータ"),
            ("::127.0.0.1", "IPv4-compatible(非推奨だが解決されうる)"),
            ("::ffff:100.64.0.1", "IPv4-mapped経由のCGNAT"),
            ("64:ff9b::127.0.0.1", "NAT64 well-known prefix"),
            ("64:ff9b::a00:1", "NAT64経由の10.0.0.1"),
            ("2002:7f00:1::", "6to4経由の127.0.0.1"),
            ("2002:a00:1::", "6to4経由の10.0.0.1"),
        ] {
            assert!(blocked(ip), "{ip}({why})は拒否されるべき");
        }
    }

    /// 埋め込まれたIPv4がグローバルなら通ること(埋め込み表記を一律で
    /// 弾いているわけではないことの確認)。
    #[test]
    fn embedded_ipv4_that_is_global_still_passes() {
        assert!(!blocked("::ffff:8.8.8.8"), "IPv4-mappedのグローバル");
        assert!(!blocked("64:ff9b::8.8.8.8"), "NAT64経由のグローバル");
        assert!(!blocked("2002:808:808::"), "6to4経由の8.8.8.8");
    }

    /// IPv6側の非グローバル範囲。
    #[test]
    fn non_global_ipv6_ranges_are_blocked() {
        for (ip, why) in [
            ("::1", "loopback"),
            ("::", "unspecified"),
            ("fe80::1", "link-local"),
            ("fc00::1", "unique local"),
            ("fd00::1", "unique local"),
            ("ff02::1", "multicast"),
            ("2001::1", "Teredo"),
            ("2001:db8::1", "ドキュメント用"),
            ("2001:20::1", "ORCHIDv2"),
            ("100::1", "discard-only"),
        ] {
            assert!(blocked(ip), "{ip}({why})は拒否されるべき");
        }
    }

    #[test]
    fn remote_fetch_is_disabled_by_default() {
        let fetcher = ImageFetcher::new(Path::new(".").to_path_buf(), false);
        let result = fetcher.fetch(&ImgSrc::RemoteUrl(
            "http://127.0.0.1:1/should-not-even-try".to_string(),
        ));
        assert!(result.is_err(), "remote fetch must be opt-in");
    }

    #[test]
    fn remote_fetch_blocks_loopback_targets_even_when_enabled() {
        let fetcher = ImageFetcher::new(Path::new(".").to_path_buf(), true);
        let result = fetcher.fetch(&ImgSrc::RemoteUrl(
            "http://127.0.0.1:1/should-be-blocked".to_string(),
        ));
        assert!(
            result.is_err(),
            "the SSRF policy resolver must block loopback targets regardless of opt-in"
        );
    }

    #[test]
    fn remote_fetch_succeeds_against_a_public_looking_loopback_server_once_allowed_and_unblocked() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
                )
                .unwrap();
        });

        let mut response = ureq::get(format!("http://127.0.0.1:{}/", addr.port()))
            .call()
            .expect("plain ureq agent without the policy resolver should reach loopback");
        let body = response.body_mut().read_to_vec().expect("should read body");
        assert_eq!(body, b"hello");
    }
}
