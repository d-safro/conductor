//! Outbound connections and the proxy (Prelude's "Use a proxy" choice): Conductor's own download traffic (the ⬇
//! catalog: Deezer lookups and previews) goes through `proxy` when one is set, and the cover and lyrics fetches too
//! with `proxy_metadata`. A proxy that can't be used fails closed: the requests fail, they never go out directly.

use crate::config::Config;

/// The proxy for download traffic, if any.
pub fn downloads_proxy(cfg: &Config) -> Option<String> {
    cfg.proxy.clone().filter(|p| !p.trim().is_empty())
}

/// The proxy for cover / lyrics / info fetches, if they should use it.
pub fn metadata_proxy(cfg: &Config) -> Option<String> {
    if cfg.proxy_metadata { downloads_proxy(cfg) } else { None }
}

pub fn with_proxy(b: reqwest::ClientBuilder, proxy: Option<&str>) -> reqwest::ClientBuilder {
    let Some(p) = proxy else { return b };
    match reqwest::Proxy::all(p) {
        Ok(px) => b.proxy(px),
        // can't be read: nowhere rather than straight out
        Err(_) => b.proxy(reqwest::Proxy::all("socks5h://127.0.0.1:9").expect("a proxy address")),
    }
}

/// "socks5h://user:secret@host:1080" → "socks5h://host:1080", for showing.
pub fn redact(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://{host}"),
            None => url.to_string(),
        },
        None => url.to_string(),
    }
}

/// Is this an address on the internet (not this computer, the home network, a VPN's or a carrier's own range)?
pub fn is_public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(a) => {
            let o = a.octets();
            !(a.is_private()
                || a.is_loopback()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || a.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64/10: carrier NAT, Tailscale
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // benchmarking
                || o[0] >= 240)
        }
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(v4));
            }
            let s = a.segments();
            !(a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link local
                || (s[0] & 0xffc0) == 0xfec0 // site local (old)
                || (s[0] == 0x2001 && s[1] == 0x0db8) // documentation
                || (s[0] == 0x64 && s[1] == 0xff9b) // NAT64: an IPv4 address inside
                || (s[0..6] == [0, 0, 0, 0, 0, 0])) // IPv4-compatible (old)
        }
    }
}

/// Refuse an address written as a number that isn't on the internet (names are checked when they're looked up).
pub fn check_public_url(url: &reqwest::Url) -> Result<(), String> {
    let host = url.host_str().ok_or("no host")?;
    match host.trim_start_matches('[').trim_end_matches(']').parse::<std::net::IpAddr>() {
        Ok(ip) if !is_public_ip(ip) => Err(format!("{ip} isn't on the internet")),
        _ => Ok(()),
    }
}

/// Looks names up as usual, then keeps only internet addresses: a feed can't send Conductor to the home network.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let all: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let public: Vec<std::net::SocketAddr> = all.into_iter().filter(|a| is_public_ip(a.ip())).collect();
            if public.is_empty() {
                return Err(format!("{host} isn't on the internet").into());
            }
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Only internet addresses, redirects included.
pub fn public_only(b: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    b.dns_resolver(std::sync::Arc::new(PublicOnly)).redirect(reqwest::redirect::Policy::custom(|a| {
        if a.previous().len() >= 10 {
            return a.error("too many redirects");
        }
        match check_public_url(a.url()) {
            Ok(()) => a.follow(),
            Err(e) => a.error(e),
        }
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn public_addresses() {
        use super::is_public_ip;
        for a in ["1.1.1.1", "93.184.216.34", "2606:4700::1111", "100.128.0.1"] {
            assert!(is_public_ip(a.parse().unwrap()), "{a}");
        }
        for a in [
            "127.0.0.1",
            "10.0.0.5",
            "172.16.3.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.101.102.103",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
            "::7f00:1",
        ] {
            assert!(!is_public_ip(a.parse().unwrap()), "{a}");
        }
        assert!(super::check_public_url(&"http://192.168.1.9/feed".parse().unwrap()).is_err());
        assert!(super::check_public_url(&"http://[::1]:8080/feed".parse().unwrap()).is_err());
        assert!(super::check_public_url(&"https://feeds.example.com/x".parse().unwrap()).is_ok());
    }

    #[test]
    fn redacts_logins() {
        assert_eq!(super::redact("socks5h://me:pw@10.0.0.1:1080"), "socks5h://10.0.0.1:1080");
        assert_eq!(super::redact("http://proxy:3128"), "http://proxy:3128");
    }
}
