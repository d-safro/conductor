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

#[cfg(test)]
mod tests {
    #[test]
    fn redacts_logins() {
        assert_eq!(super::redact("socks5h://me:pw@10.0.0.1:1080"), "socks5h://10.0.0.1:1080");
        assert_eq!(super::redact("http://proxy:3128"), "http://proxy:3128");
    }
}
