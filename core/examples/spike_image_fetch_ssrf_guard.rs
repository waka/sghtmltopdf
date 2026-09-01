//! Spike: a PoC checking that an SSRF guard (a filter refusing private, loopback,
//! link-local and similar IPs) can be plugged into ureq's `Resolver` hook and works on the
//! real `Agent` plus request path.
//!
//! What we want to check:
//! - The `ureq::unversioned::resolver::Resolver` trait can be swapped in via
//!   `Agent::with_parts`, and is called again on every redirect
//!   (confirmed in the source: `call_run` in `ureq-3.3.0/src/run.rs` calls
//!   `connect()` and then `agent.resolver.resolve()` once per redirect loop iteration).
//!   So this one hook should block not only the initial access but also the classic SSRF
//!   bypass of "allowed as a public URL, then redirected to an internal IP"
//! - DNS rebinding: deciding the block purely from the *resolved* IPs refuses reliably,
//!   regardless of whether the host name looks like a public domain. This is demonstrated
//!   with a fake resolver (`AlwaysResolvesTo`) representing the worst case of "DNS always
//!   returns an internal IP"
//!
//! Run with: `cargo run --example spike_image_fetch_ssrf_guard`

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};
use ureq::{config::Config, Agent, Error};

/// Decide whether an IP should not be publicly reachable: private, loopback, link-local
/// (including the cloud metadata address 169.254.169.254), multicast, unspecified and so on.
/// An IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is decided recursively on the embedded
/// IPv4 (letting it through would bypass the IPv4 filter).
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local() // 169.254.0.0/16 (cloud metadata included)
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || v6.is_unique_local() // fc00::/7
                || v6.is_unicast_link_local() // fe80::/10
        }
    }
}

/// Wraps an arbitrary `Resolver` and drops blocked IPs from what it resolves.
/// If nothing is left, it refuses with `Error::HostNotFound`
/// (so the caller cannot tell "blocked" apart from "does not exist at all").
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
    ) -> Result<ResolvedSocketAddrs, Error> {
        let addrs = self.inner.resolve(uri, config, timeout)?;
        let mut filtered: ResolvedSocketAddrs =
            ResolvedSocketAddrs::from_fn(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
        for addr in addrs.iter().filter(|a| !is_blocked_ip(a.ip())) {
            filtered.push(*addr);
        }
        if filtered.is_empty() {
            return Err(Error::HostNotFound);
        }
        Ok(filtered)
    }
}

/// A fake resolver modelling the worst case of DNS rebinding: it never looks at the real
/// host name and always returns the given address (that is, one the attacker controls).
#[derive(Debug)]
struct AlwaysResolvesTo(SocketAddr);

impl Resolver for AlwaysResolvesTo {
    fn resolve(
        &self,
        _uri: &Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        let mut addrs =
            ResolvedSocketAddrs::from_fn(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
        addrs.push(self.0);
        Ok(addrs)
    }
}

fn main() {
    // --- Case 1: even posing as a public host name, it must be blocked when the standard
    // resolution result is a loopback or private IP (modelling a URL that accesses a
    // loopback service directly in a real environment).
    let agent = Agent::with_parts(
        Config::default(),
        DefaultConnector::default(),
        PolicyResolver {
            inner: DefaultResolver::default(),
        },
    );
    let result = agent.get("http://127.0.0.1:1/should-be-blocked").call();
    match result {
        Err(Error::HostNotFound) => {
            eprintln!("[OK] a request to loopback was blocked as HostNotFound")
        }
        other => panic!("a request to loopback was not blocked: {other:?}"),
    }

    // --- Case 2: the worst case of DNS rebinding. Whatever the host name string says (here
    // `example.invalid`), the same filter must refuse it when the *resolution result* is a
    // private IP (169.254.169.254, the classic cloud metadata address).

    let rebinding_agent = Agent::with_parts(
        Config::default(),
        DefaultConnector::default(),
        PolicyResolver {
            inner: AlwaysResolvesTo(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
                80,
            )),
        },
    );
    let result = rebinding_agent
        .get("http://example.invalid/latest/meta-data/")
        .call();
    match result {
        Err(Error::HostNotFound) => {
            eprintln!("[OK] DNS rebinding steering to the cloud metadata IP was blocked too")
        }
        other => panic!("rebinding to the metadata IP was not blocked: {other:?}"),
    }

    // --- Case 3 (the control): a public IP must pass through and get as far as a real TCP
    // connection (that is, the filter is not over-detecting). We cannot make a server on
    // loopback pose as a global IP without depending on the external network, so this is
    // checked as `is_blocked_ip` alone (no real network connection is made).

    let public_examples = [
        IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)), // the equivalent of example.com
        IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)),       // 8.8.8.8 (Google Public DNS)
    ];
    for ip in public_examples {
        assert!(
            !is_blocked_ip(ip),
            "a public IP was wrongly judged to be blocked: {ip}"
        );
    }
    eprintln!("[OK] the public IP examples were not judged to be blocked (no over-detection)");

    // --- Case 4: demonstrate by really redirecting what reading the source told us, that
    // resolve() is called again when a redirect is followed (call_run in run.rs calls
    // resolver.resolve() via connect() once per redirect loop iteration).
    // Here a pass-through resolver with no policy filter, plus a "record the URI seen"
    // feature, is used to confirm on a real request through one 302 that resolve() is
    // called twice with different authorities (host:port).
    // With that demonstrated, using PolicyResolver also guarantees that the redirect SSRF of
    // "the first URL was allowed but the 302 target was an internal IP" is refused by the
    // same filter on the second resolve() call.
    let seen_authorities: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let redirect_target_addr = spawn_plain_ok_server();
    let redirect_target = format!("http://127.0.0.1:{}/final", redirect_target_addr.port());
    let entry_addr = spawn_redirecting_server(redirect_target);

    let counting_agent = Agent::with_parts(
        Config::default(),
        DefaultConnector::default(),
        LoggingResolver {
            inner: DefaultResolver::default(),
            seen: seen_authorities.clone(),
        },
    );
    let response = counting_agent
        .get(format!("http://127.0.0.1:{}/start", entry_addr.port()))
        .call()
        .expect("the request through the redirect failed");
    assert_eq!(response.status(), 200);

    let seen = seen_authorities.lock().unwrap();
    assert_eq!(
        seen.len(),
        2,
        "expected resolve() to be called twice (once either side of the redirect), but it was called {} times: {seen:?}",
        seen.len()
    );
    assert_ne!(
        seen[0], seen[1],
        "expected resolve() to be called for different host:port either side of the redirect"
    );
    eprintln!(
        "[OK] confirmed that resolve() is called again when a redirect is followed: {seen:?}"
    );

    eprintln!("every SSRF guard scenario checked out");
}

/// A pass-through wrapper that merely records the authority (host:port) of the `Uri` passed
/// to `Resolver::resolve`. Used in case 4 to demonstrate whether it is called again on every
/// redirect (it applies no SSRF filter of its own).
#[derive(Debug)]
struct LoggingResolver<R> {
    inner: R,
    seen: Arc<Mutex<Vec<String>>>,
}

impl<R: Resolver> Resolver for LoggingResolver<R> {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, Error> {
        self.seen
            .lock()
            .unwrap()
            .push(uri.authority().map(|a| a.to_string()).unwrap_or_default());
        self.inner.resolve(uri, config, timeout)
    }
}

/// Start a loopback server returning `200 OK` exactly once, and return the address it bound to.
fn spawn_plain_ok_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind to loopback");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("failed to accept the connection");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    addr
}

/// Start a loopback server returning `302 Found` exactly once (with a Location header for
/// the given URL), and return the address it bound to.
fn spawn_redirecting_server(location: String) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to bind to loopback");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("failed to accept the connection");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    addr
}
