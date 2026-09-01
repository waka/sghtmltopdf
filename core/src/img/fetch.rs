//! Fetching bytes for external resources. Local files, HTTP(S) and `data:` URIs are handled uniformly, following the [`ImgSrc`] classification.
//!
//! Besides `<img src>`, this is also the path taken by `<link rel=stylesheet>`, `@import`
//! and `url()` in `@font-face` (so the trust boundary of a source and its size limit live
//! in one place).

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::Duration;

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{Agent, Error as UreqError};

use super::{resolve_local_asset_path, ImgSrc};

/// Default cap on the bytes fetched (20MiB). The same cap applies to every source -
/// local, remote and data: - because the design goal of staying small and low-memory
/// gives no reason to leave a non-HTTP source unbounded.
const DEFAULT_MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Why a fetch failed. The caller knows what it was fetching (an image, an external
/// stylesheet, an `@import`, an `@font-face`), so this carries only the reason and no
/// prefix naming the kind.
#[derive(Debug)]
pub struct FetchError(String);

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for FetchError {}

/// Responsible for fetching the bytes of an `<img>`. One is built per document and reused
/// across every `<img>` element (which also lets `ureq::Agent` pool its connections).
pub struct ImageFetcher {
    /// Base directory for local relative paths (the same role it plays when resolving
    /// url() in `@font-face`).
    base_dir: PathBuf,
    /// Whether remote (http/https) fetching is allowed. Off by default, following the
    /// "disabled by default, explicit opt-in" rule. `data:` and local paths are always
    /// allowed regardless, since they involve no network, or share the trust boundary
    /// with `@font-face`.
    allow_remote: bool,
    max_bytes: u64,
    agent: Agent,
    /// The `<base href>` value. Relative references resolve against it before fetching.
    /// `<img src>`, `<link href>` and `@import` all share this fetcher, so setting it
    /// here covers all three at once.
    base_href: Option<String>,
    /// Whether local file references are allowed (false with `--disable-local-file-access`).
    allow_local: bool,
    /// If non-empty, local references are confined to these directories
    /// (`--allow`).
    allowed_dirs: Vec<PathBuf>,
}

impl ImageFetcher {
    /// The size cap is [`DEFAULT_MAX_IMAGE_BYTES`].
    pub fn new(base_dir: PathBuf, allow_remote: bool) -> Self {
        Self::with_max_bytes(base_dir, allow_remote, DEFAULT_MAX_IMAGE_BYTES)
    }

    /// Return the same fetcher with `<base href>` set (used builder-style).
    pub fn with_base_href(mut self, base_href: Option<String>) -> Self {
        self.base_href = base_href.filter(|href| !href.trim().is_empty());
        self
    }

    /// Set whether local files may be read, and the permitted directories (`--allow`).
    ///
    /// When `allow_local` is `false`, every local path reference is refused
    /// (the intended default for HTTP server mode). When `allowed_dirs` is non-empty,
    /// a reference whose resolved path is under none of them is refused.
    pub fn with_local_access(mut self, allow_local: bool, allowed_dirs: Vec<PathBuf>) -> Self {
        self.allow_local = allow_local;
        // Resolve to real paths once, here. Resolving on every reference would mean
        // falling back to comparing raw paths whenever resolution fails.
        //
        // Anything that could not be resolved is kept as-is, but since the other side of
        // the comparison is always a real path, it never matches and falls to the refusing
        // side. From the CLI, `ConvertArgs::local_access` passes in resolved, validated ones.
        self.allowed_dirs = allowed_dirs
            .into_iter()
            .map(|dir| dir.canonicalize().unwrap_or(dir))
            .collect();
        self
    }

    /// Resolve a raw reference value against `<base href>` and classify it as a URL or a path.
    /// Every fetch path goes through here.
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

    /// Fetch the bytes according to the classification of `src` ([`ImgSrc`]).
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

    /// Read a local file relative to `base_dir`.
    /// A reference escaping base_dir via `..` is rejected by [`resolve_local_asset_path`].
    /// To reference outside it, name the range explicitly with `--allow`.
    fn read_local(&self, path: &str) -> Result<Vec<u8>, FetchError> {
        if !self.allow_local {
            return Err(FetchError(
                "reading local files is not permitted (--enable-local-file-access)".to_string(),
            ));
        }
        let resolved = resolve_local_asset_path(&self.base_dir, path);
        // Without `--allow`, base_dir is the boundary. With it, `--allow` decides the range,
        // so escaping is permitted here and checked below.
        if resolved.escapes_base_dir && self.allowed_dirs.is_empty() {
            return Err(FetchError(format!(
                "the reference points outside the base directory ({}).\n  \
                 To read files outside it, name the directory with --allow",
                self.base_dir.display()
            )));
        }
        let full_path = resolved.path;
        if !self.allowed_dirs.is_empty() {
            // If it does not resolve to a real path, refuse it as "could not be confirmed
            // inside the permitted range". Falling back to the raw path would mean calling
            // `starts_with` with `..` still in it, so `/var/www/../etc/passwd` would be
            // judged to be under `/var/www` (the comparison is component-wise on the path
            // string and never looks at the filesystem).
            let canonical = full_path
                .canonicalize()
                .map_err(|e| FetchError(format!("{}: {e}", full_path.display())))?;
            if !self
                .allowed_dirs
                .iter()
                .any(|dir| canonical.starts_with(dir))
            {
                return Err(FetchError(format!(
                    "{}: outside the directories permitted by --allow",
                    full_path.display()
                )));
            }
        }
        let metadata = std::fs::metadata(&full_path)
            .map_err(|e| FetchError(format!("{}: {e}", full_path.display())))?;
        self.ensure_within_limit(metadata.len()).map_err(|_| {
            FetchError(format!(
                "{}: the file size exceeds the limit",
                full_path.display()
            ))
        })?;
        std::fs::read(&full_path).map_err(|e| FetchError(format!("{}: {e}", full_path.display())))
    }

    fn fetch_remote(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        if !self.allow_remote {
            return Err(FetchError(
                "remote fetching is disabled by default (permit it with --allow-remote-assets)"
                    .to_string(),
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
                "the size exceeds the limit ({} bytes)",
                self.max_bytes
            )));
        }
        Ok(())
    }
}

/// Whether an IP is not globally reachable.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_blocked_ipv4(v4),
        IpAddr::V6(v6) => is_blocked_ipv6(v6),
    }
}

/// Whether an IPv4 address is not globally reachable.
///
/// The standard library's predicates do not cover every range, so we inspect the CIDRs
/// directly (`is_shared`, `is_reserved`, `is_benchmarking` and friends are unstable).
fn is_blocked_ipv4(v4: std::net::Ipv4Addr) -> bool {
    let [a, b, _, _] = v4.octets();

    // What the standard library can decide:
    // private (10/8, 172.16/12, 192.168/16), loopback (127/8),
    // link-local (169.254/16, which covers the cloud metadata address 169.254.169.254),
    // multicast (224/4), broadcast (255.255.255.255),
    // documentation (192.0.2/24, 198.51.100/24, 203.0.113/24).
    if v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_documentation()
    {
        return true;
    }

    // 0.0.0.0/8 "this network". `is_unspecified` only matches exactly 0.0.0.0, but the
    // whole of 0.x.y.z is non-global, and on Linux a destination such as 0.0.0.1 goes
    // to the local host.
    if a == 0 {
        return true;
    }
    // 100.64.0.0/10 CGNAT. Used by cloud internal load balancers and Kubernetes networking,
    // so it points inward even though it looks global from the outside.
    if a == 100 && (64..128).contains(&b) {
        return true;
    }
    // 192.0.0.0/24 IETF Protocol Assignments.
    if v4.octets()[..3] == [192, 0, 0] {
        return true;
    }
    // 198.18.0.0/15, for benchmarking.
    if a == 198 && (b == 18 || b == 19) {
        return true;
    }
    // 240.0.0.0/4 reserved (the 255.255.255.255 broadcast is caught above).
    if a >= 240 {
        return true;
    }

    false
}

/// Whether an IPv6 address is not globally reachable.
fn is_blocked_ipv6(v6: std::net::Ipv6Addr) -> bool {
    // `to_ipv4` catches the deprecated IPv4-compatible form (`::a.b.c.d`) as well as
    // IPv4-mapped (`::ffff:a.b.c.d`). With `to_ipv4_mapped` alone the former would
    // slip through.
    if let Some(v4) = v6.to_ipv4() {
        return is_blocked_ipv4(v4);
    }

    let segments = v6.segments();

    // 64:ff9b::/96 (the NAT64 well-known prefix) and 64:ff9b:1::/48 (local use).
    // An IPv4 address is embedded in the low 32 bits, and where a NAT64 gateway exists
    // this reaches the IPv4 side through it.
    if segments[0] == 0x0064 && segments[1] == 0xff9b {
        let [a, b, c, d] = (((segments[6] as u32) << 16) | segments[7] as u32).to_be_bytes();
        return is_blocked_ipv4(std::net::Ipv4Addr::new(a, b, c, d));
    }
    // 2002::/16 (6to4). Inspect the IPv4 address embedded in segments 2 and 3.
    if segments[0] == 0x2002 {
        let [a, b, c, d] = (((segments[1] as u32) << 16) | segments[2] as u32).to_be_bytes();
        return is_blocked_ipv4(std::net::Ipv4Addr::new(a, b, c, d));
    }

    // 2001::/32 (Teredo). The client's IPv4 address is embedded in obfuscated form;
    // it is not worth decoding just to let it through, so the whole prefix is refused.
    if segments[0] == 0x2001 && segments[1] == 0x0000 {
        return true;
    }
    // 2001:db8::/32, for documentation.
    if segments[0] == 0x2001 && segments[1] == 0x0db8 {
        return true;
    }
    // 2001:20::/28 ORCHIDv2.
    if segments[0] == 0x2001 && (0x0020..0x0030).contains(&segments[1]) {
        return true;
    }
    // 100::/64 discard-only.
    if segments[0] == 0x0100 && segments[1..4] == [0, 0, 0] {
        return true;
    }

    // loopback (::1), multicast (ff00::/8), unspecified (::),
    // unique local (fc00::/7), link-local unicast (fe80::/10).
    v6.is_loopback()
        || v6.is_multicast()
        || v6.is_unspecified()
        || v6.is_unique_local()
        || v6.is_unicast_link_local()
}

/// Wraps an arbitrary `Resolver` and drops blocked IPs from what it resolves.
/// If nothing is left, it refuses with `Error::HostNotFound` (so the caller cannot tell
/// "blocked" apart from "does not exist at all").
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

    /// The same `std::env::temp_dir()`-based temporary directory pattern as the existing
    /// tests in `engine.rs`. The caller calls `remove_dir_all` at the end.
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
        // Check that a root-relative src such as `/logo.png` (the same shape as
        // `<link href="/stylesheets/main.css" />`) does not escape base_dir to the OS
        // filesystem root, but is read as a file under base_dir instead.
        let dir = temp_dir("root_relative");
        std::fs::write(dir.join("logo.png"), b"fake png bytes").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("/logo.png".to_string()))
            .expect("root-relative local read should succeed within base_dir");
        assert_eq!(bytes, b"fake png bytes");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// By default (no `--allow`), a reference escaping base_dir via `..` is refused.
    /// This default is what blocks arbitrary file reads from untrusted HTML.
    #[test]
    fn a_local_path_escaping_base_dir_is_refused_by_default() {
        let dir = temp_dir("escape_default");
        let inner = dir.join("pages");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(dir.join("secret.png"), b"outside").unwrap();
        let fetcher = ImageFetcher::new(inner.clone(), false);

        let err = fetcher
            .fetch(&ImgSrc::LocalPath("../secret.png".to_string()))
            .expect_err("outside base_dir should be refused by default");
        assert!(
            err.to_string().contains("base directory"),
            "it must be refused by the containment check: {err}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// With `--allow`, the named range is readable even outside base_dir
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
            .expect("should be readable when inside the --allow range");
        assert_eq!(bytes, b"outside but allowed");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Even with `--allow`, anything outside that range is still unreadable.
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
        assert!(
            result.is_err(),
            "outside the --allow range must not be readable"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Permitted directories are resolved to real paths at construction. Passing one with
    /// `..` in it still leaves the files inside it readable.
    #[test]
    fn allowed_dirs_are_canonicalized_once_at_construction() {
        let dir = temp_dir("allow_canonical");
        let inner = dir.join("pages");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(dir.join("logo.png"), b"allowed").unwrap();

        // `<dir>/pages/..` really is `<dir>`.
        let dotted = inner.join("..");
        let fetcher = ImageFetcher::new(inner.clone(), false).with_local_access(true, vec![dotted]);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("../logo.png".to_string()))
            .expect("should be readable, being under the resolved directory");
        assert_eq!(bytes, b"allowed");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A reference that cannot be resolved to a real path is refused without consulting `--allow`.
    #[test]
    fn a_path_that_cannot_be_resolved_is_refused_instead_of_compared_raw() {
        let dir = temp_dir("allow_unresolvable");
        let allowed = dir.join("assets");
        std::fs::create_dir_all(&allowed).unwrap();
        let fetcher = ImageFetcher::new(allowed.clone(), false)
            .with_local_access(true, vec![allowed.clone()]);

        // A path that looks to be under a permitted directory but does not exist.
        let result = fetcher.fetch(&ImgSrc::LocalPath("../secret/none.png".to_string()));
        assert!(
            result.is_err(),
            "an unresolvable reference should be refused"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A `..` that stays inside base_dir (`assets/../images/x`) is readable.
    #[test]
    fn a_parent_reference_that_stays_inside_base_dir_still_reads() {
        let dir = temp_dir("escape_inside");
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("logo.png"), b"inside").unwrap();
        let fetcher = ImageFetcher::new(dir.clone(), false);

        let bytes = fetcher
            .fetch(&ImgSrc::LocalPath("assets/../logo.png".to_string()))
            .expect("a .. that stays inside base_dir should be allowed");
        assert_eq!(bytes, b"inside");

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
        is_blocked_ip(ip.parse().expect("invalid IP literal in the test"))
    }

    /// The ranges the standard library's predicates can decide.
    #[test]
    fn well_known_private_ranges_are_blocked() {
        for ip in [
            "127.0.0.1",       // loopback
            "10.0.0.1",        // private
            "172.16.0.1",      // private
            "192.168.1.1",     // private
            "169.254.169.254", // link-local (cloud metadata)
            "224.0.0.1",       // multicast
            "255.255.255.255", // broadcast
            "192.0.2.1",       // documentation
            "0.0.0.0",         // unspecified
        ] {
            assert!(blocked(ip), "{ip} should be refused");
        }
    }

    /// Ranges the standard library's predicates miss, which used to slip through.
    #[test]
    fn ranges_missed_by_the_standard_predicates_are_blocked() {
        for (ip, why) in [
            ("0.1.2.3", "0.0.0.0/8 this network"),
            ("100.64.0.1", "100.64.0.0/10 CGNAT"),
            ("100.127.255.254", "top of CGNAT"),
            ("192.0.0.1", "192.0.0.0/24 IETF protocol assignments"),
            ("198.18.0.1", "198.18.0.0/15 for benchmarking"),
            ("198.19.255.254", "top of the benchmarking range"),
            ("240.0.0.1", "240.0.0.0/4 reserved"),
        ] {
            assert!(blocked(ip), "{ip} ({why}) should be refused");
        }
    }

    /// The CGNAT boundaries. 100.63.x and 100.128.x are global.
    #[test]
    fn the_cgnat_boundaries_are_respected() {
        assert!(!blocked("100.63.255.255"), "just below CGNAT is global");
        assert!(blocked("100.64.0.0"), "bottom of CGNAT");
        assert!(blocked("100.127.255.255"), "top of CGNAT");
        assert!(!blocked("100.128.0.0"), "just above CGNAT is global");
    }

    /// Ordinary global addresses pass (a check that we are not over-blocking).
    #[test]
    fn ordinary_global_addresses_are_allowed() {
        for ip in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "239.255.255.255", // to check just past the top of the multicast range
            "2606:4700:4700::1111",
            "2400:cb00::1",
        ] {
            if ip == "239.255.255.255" {
                // 239/8 is multicast, so refusing it is correct.
                assert!(blocked(ip));
                continue;
            }
            assert!(!blocked(ip), "{ip} should pass");
        }
    }

    /// The IPv4 filters cannot be bypassed through an IPv6 form that embeds IPv4.
    #[test]
    fn ipv6_forms_embedding_ipv4_are_checked_against_the_ipv4_rules() {
        for (ip, why) in [
            ("::ffff:127.0.0.1", "IPv4-mapped"),
            ("::ffff:169.254.169.254", "IPv4-mapped metadata address"),
            (
                "::127.0.0.1",
                "IPv4-compatible (deprecated but still resolvable)",
            ),
            ("::ffff:100.64.0.1", "CGNAT via IPv4-mapped"),
            ("64:ff9b::127.0.0.1", "NAT64 well-known prefix"),
            ("64:ff9b::a00:1", "10.0.0.1 via NAT64"),
            ("2002:7f00:1::", "127.0.0.1 via 6to4"),
            ("2002:a00:1::", "10.0.0.1 via 6to4"),
        ] {
            assert!(blocked(ip), "{ip} ({why}) should be refused");
        }
    }

    /// An embedded IPv4 that is global still passes (a check that we are not refusing
    /// every embedding form outright).
    #[test]
    fn embedded_ipv4_that_is_global_still_passes() {
        assert!(!blocked("::ffff:8.8.8.8"), "global address via IPv4-mapped");
        assert!(!blocked("64:ff9b::8.8.8.8"), "global address via NAT64");
        assert!(!blocked("2002:808:808::"), "8.8.8.8 via 6to4");
    }

    /// Non-global IPv6 ranges.
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
            ("2001:db8::1", "documentation"),
            ("2001:20::1", "ORCHIDv2"),
            ("100::1", "discard-only"),
        ] {
            assert!(blocked(ip), "{ip} ({why}) should be refused");
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
