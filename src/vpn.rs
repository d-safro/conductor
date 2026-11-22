//! Downloads through a VPN (Prelude's VPN step): checking it, the kill switch, and the forwarded port.
//!
//! - The check: the public address the downloads use (gluetun's control server for slskd behind a VPN, or a
//!   request through the proxy) is compared with this computer's own. Up when they differ. Only up / down and the
//!   exit country are kept; the addresses themselves are never stored, shown or logged.
//! - The kill switch (`downloads_require_vpn`): Stagehand waits while the check fails, or hasn't passed yet, and
//!   tries again every `vpn_check_secs`. It never falls back to the open connection.
//! - The forwarded port (`vpn_port_sync`, slskd inside gluetun): when the VPN's forwarded port changes, slskd's
//!   Soulseek listen port follows (through slskd's remote configuration) and slskd reconnects, so peers can reach it.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::{api::AppState, config::Config, db::now};

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// none · existing · gluetun · proxy
    pub mode: String,
    /// None: not checked (yet)
    pub up: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    pub checked: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub forwarded_port: Option<u16>,
}

/// Downloads wait (the kill switch is on and the VPN isn't known to be up).
pub fn paused(st: &AppState) -> bool {
    st.cfg.downloads_require_vpn && st.vpn.read().unwrap_or_else(|e| e.into_inner()).up != Some(true)
}

pub fn status(st: &AppState) -> Status {
    st.vpn.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn client(proxy: Option<&str>) -> reqwest::Client {
    crate::net::with_proxy(reqwest::Client::builder().timeout(Duration::from_secs(15)).user_agent(crate::wiki::UA), proxy)
        .build()
        .expect("http client")
}

/// The public address (and country) an IP service or gluetun's control server reports.
pub async fn public_ip(http: &reqwest::Client, url: &str, key: Option<&str>) -> Result<(String, Option<String>)> {
    let mut r = http.get(url).header("accept", "application/json");
    if let Some(k) = key.filter(|k| !k.is_empty()) {
        r = r.header("X-API-Key", k);
    }
    let resp = r.send().await.context("no answer")?;
    if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
        bail!("refused (the check needs its key)");
    }
    if !resp.status().is_success() {
        bail!("answered {}", resp.status().as_u16());
    }
    let text = resp.text().await?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let ip = v["public_ip"].as_str().or(v["ip"].as_str()).or(v["query"].as_str()).map(String::from).unwrap_or_else(|| text.trim().to_string());
    if ip.is_empty() || ip.len() > 64 || ip.contains(char::is_whitespace) {
        bail!("no address in the answer");
    }
    let country = v["country"].as_str().or(v["country_name"].as_str()).filter(|c| !c.is_empty()).map(String::from);
    Ok((ip, country))
}

/// Check the VPN now. Never returns or logs the addresses.
pub async fn check(cfg: &Config) -> Status {
    let mut s = Status { mode: cfg.vpn.clone(), checked: now(), ..Default::default() };
    let direct = client(None);
    let tunnel = match cfg.vpn.as_str() {
        "proxy" => match crate::net::downloads_proxy(cfg) {
            Some(p) => public_ip(&client(Some(&p)), &cfg.vpn_ip_url, None).await,
            None => Err(anyhow::anyhow!("no proxy is set")),
        },
        "existing" | "gluetun" => match &cfg.vpn_check_url {
            Some(u) => public_ip(&direct, u, cfg.vpn_check_key.as_deref()).await,
            None => Err(anyhow::anyhow!("there's no way to check it (vpn_check_url isn't set)")),
        },
        _ => {
            if cfg.downloads_require_vpn {
                s.up = Some(false);
                s.problem = Some("no VPN is set up (run conductor setup)".into());
            }
            return s;
        }
    };
    let (tunnel_ip, country) = match tunnel {
        Ok(x) => x,
        Err(e) => {
            s.up = Some(false);
            s.problem = Some(format!("the VPN check failed: {e:#}"));
            return s;
        }
    };
    let home = public_ip(&direct, &cfg.vpn_ip_url, None).await;
    match home {
        Ok((home_ip, _)) if home_ip == tunnel_ip => {
            s.up = Some(false);
            s.problem = Some("downloads would use this computer's own address".into());
        }
        Ok(_) => {
            s.up = Some(true);
            s.country = country;
        }
        Err(e) => {
            // the open connection can't be asked: can't tell, so not up
            s.up = Some(false);
            s.problem = Some(format!("this computer's own address couldn't be checked: {e:#}"));
        }
    }
    s
}

/// gluetun's control server address from the check URL ("http://127.0.0.1:8000/v1/publicip/ip" → "http://127.0.0.1:8000").
fn control_base(check_url: &str) -> Option<String> {
    let (scheme, rest) = check_url.split_once("://")?;
    Some(format!("{scheme}://{}", rest.split('/').next()?))
}

/// Set `soulseek.listen_port` in slskd's YAML configuration.
pub fn set_listen_port(yaml: &str, port: u16) -> String {
    let mut lines: Vec<String> = yaml.lines().map(String::from).collect();
    let soulseek = lines.iter().position(|l| l.trim_end() == "soulseek:");
    match soulseek {
        Some(at) => {
            // inside the soulseek block: indented lines after it
            let mut i = at + 1;
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].trim().is_empty()) {
                if lines[i].trim_start().starts_with("listen_port:") {
                    let indent = &lines[i][..lines[i].len() - lines[i].trim_start().len()];
                    lines[i] = format!("{indent}listen_port: {port}");
                    return lines.join("\n") + "\n";
                }
                i += 1;
            }
            lines.insert(at + 1, format!("  listen_port: {port}"));
        }
        None => {
            lines.push("soulseek:".into());
            lines.push(format!("  listen_port: {port}"));
        }
    }
    lines.join("\n") + "\n"
}

/// Keep slskd's listen port on the VPN's forwarded port. Returns the forwarded port.
async fn sync_port(st: &AppState) -> Result<Option<u16>> {
    let (Some(check), Some(slskd), Some(key)) = (&st.cfg.vpn_check_url, &st.cfg.slskd_url, &st.cfg.slskd_api_key) else {
        return Ok(None);
    };
    let base = control_base(check).context("vpn_check_url")?;
    let http = client(None);
    let mut port = None;
    for path in ["/v1/portforward", "/v1/openvpn/portforwarded"] {
        let mut r = http.get(format!("{base}{path}"));
        if let Some(k) = &st.cfg.vpn_check_key {
            r = r.header("X-API-Key", k);
        }
        if let Ok(resp) = r.send().await
            && resp.status().is_success()
            && let Ok(v) = resp.json::<Value>().await
        {
            port = v["port"].as_u64().filter(|p| *p > 0 && *p < 65536).map(|p| p as u16);
            break;
        }
    }
    let Some(port) = port else { return Ok(None) };
    let sl = crate::catalog::acquire::Slskd::new(slskd, key);
    // its YAML, as a JSON string or as the text itself; never rewritten without having read it
    let r = http
        .get(format!("{}/api/v0/options/yaml", slskd.trim_end_matches('/')))
        .header("X-API-Key", key.as_str())
        .send()
        .await
        .context("reading slskd's configuration")?;
    if !r.status().is_success() {
        bail!("slskd didn't give its configuration ({}; is remote_configuration on?)", r.status().as_u16());
    }
    let text = r.text().await?;
    let yaml = serde_json::from_str::<String>(&text).unwrap_or(text);
    if !yaml.lines().any(|l| l.trim_end() == "soulseek:") {
        bail!("slskd's configuration has no soulseek section: not changing it");
    }
    let yaml = yaml.as_str();
    let new = set_listen_port(yaml, port);
    if new.trim_end() != yaml.trim_end() {
        sl.call(reqwest::Method::PUT, "/options/yaml", Some(Value::String(new))).await.context("writing slskd's configuration")?;
        // reconnect, so the Soulseek server tells peers the new port
        let _ = sl.call(reqwest::Method::DELETE, "/server", None).await;
        let _ = sl.call(reqwest::Method::PUT, "/server", None).await;
        info!(port, "slskd's listen port follows the VPN's forwarded port");
    }
    Ok(Some(port))
}

/// Checks the VPN every `vpn_check_secs` (and keeps the forwarded port in step).
pub async fn worker(st: Arc<AppState>) {
    if st.cfg.vpn == "none" && !st.cfg.downloads_require_vpn {
        return;
    }
    let mut was: Option<bool> = None;
    loop {
        let mut s = check(&st.cfg).await;
        if st.cfg.vpn_port_sync && s.up == Some(true) {
            match sync_port(&st).await {
                Ok(p) => s.forwarded_port = p,
                Err(e) => warn!("forwarded port: {e:#}"),
            }
        }
        if was != s.up {
            match s.up {
                Some(true) => info!(country = s.country.as_deref().unwrap_or("?"), "VPN up{}", if st.cfg.downloads_require_vpn { ": downloads go on" } else { "" }),
                _ => warn!(
                    "VPN down ({}){}",
                    s.problem.as_deref().unwrap_or("?"),
                    if st.cfg.downloads_require_vpn { ": downloads paused" } else { "" }
                ),
            }
            was = s.up;
            if s.up == Some(true) {
                st.acquire_wake.notify_one();
            }
        }
        *st.vpn.write().unwrap_or_else(|e| e.into_inner()) = s;
        tokio::time::sleep(Duration::from_secs(st.cfg.vpn_check_secs.max(1))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_port_in_yaml() {
        assert_eq!(set_listen_port("soulseek:\n  username: x\n  listen_port: 50300\nweb:\n  port: 5030\n", 41234), "soulseek:\n  username: x\n  listen_port: 41234\nweb:\n  port: 5030\n");
        assert_eq!(set_listen_port("soulseek:\n  username: x\nweb:\n  port: 5030", 7), "soulseek:\n  listen_port: 7\n  username: x\nweb:\n  port: 5030\n");
        assert_eq!(set_listen_port("web:\n  port: 5030", 7), "web:\n  port: 5030\nsoulseek:\n  listen_port: 7\n");
        assert_eq!(control_base("http://127.0.0.1:8000/v1/publicip/ip").as_deref(), Some("http://127.0.0.1:8000"));
    }
}
