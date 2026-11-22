//! Prelude's "set one up for me": slskd inside a VPN (gluetun), as a Docker Compose folder beside config.toml.
//!
//!   compose.yml         gluetun + slskd (`network_mode: service:gluetun`: slskd only ever talks through the VPN)
//!   vpn.env             the VPN's keys (mode 600)
//!   gluetun/auth/config.toml   who may ask gluetun's control server what (Conductor, with its own key; mode 600)
//!   slskd/slskd.yml     slskd's settings: Soulseek login, API key for Conductor, remote configuration (mode 600)
//!
//! The VPN: a WireGuard .conf from any provider (gluetun's "custom" provider), or a provider gluetun knows and its
//! WireGuard private key. With port forwarding (providers that offer it), Conductor keeps slskd's listen port on the
//! forwarded one (vpn.rs).

use std::{
    net::ToSocketAddrs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};

/// Providers whose forwarded ports gluetun can fetch.
pub const PORT_FORWARDING: &[&str] = &["protonvpn", "private internet access", "perfect privacy", "privatevpn"];
/// A few common ones (gluetun knows more: any of its names works).
pub const PROVIDERS: &[&str] = &["protonvpn", "mullvad", "airvpn", "ivpn", "private internet access", "windscribe", "nordvpn", "surfshark"];

#[derive(Debug, Clone, PartialEq)]
pub struct WgConf {
    pub private_key: String,
    pub addresses: String,
    pub public_key: String,
    pub endpoint_ip: String,
    pub endpoint_port: u16,
    pub preshared_key: Option<String>,
}

/// A WireGuard .conf ([Interface] PrivateKey, Address; [Peer] PublicKey, Endpoint, PresharedKey).
pub fn parse_wg(text: &str) -> Result<WgConf> {
    let mut v: std::collections::HashMap<String, String> = Default::default();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some((k, val)) = line.split_once('=') {
            v.entry(k.trim().to_lowercase()).or_insert_with(|| val.trim().to_string());
        }
    }
    let get = |k: &str| v.get(k).cloned().filter(|s| !s.is_empty()).with_context(|| format!("the .conf has no {k}"));
    let endpoint = get("endpoint")?;
    let (host, port) = endpoint.rsplit_once(':').context("Endpoint isn't host:port")?;
    let port: u16 = port.parse().context("Endpoint's port")?;
    let host = host.trim_matches(['[', ']']);
    // gluetun wants the server's address, not its name
    let ip = (host, port).to_socket_addrs().with_context(|| format!("looking up {host}"))?.next().context("no address")?.ip().to_string();
    Ok(WgConf {
        private_key: get("privatekey")?,
        addresses: get("address")?.split(',').next().unwrap_or("").trim().to_string(),
        public_key: get("publickey")?,
        endpoint_ip: ip,
        endpoint_port: port,
        preshared_key: v.get("presharedkey").cloned().filter(|s| !s.is_empty()),
    })
}

#[derive(Debug, Clone)]
pub enum Vpn {
    Conf(WgConf),
    Provider { name: String, private_key: String, addresses: Option<String>, countries: Option<String> },
}

#[derive(Debug, Clone)]
pub struct Setup {
    pub dir: PathBuf,
    pub vpn: Vpn,
    pub soulseek_user: String,
    pub soulseek_password: String,
    /// where finished downloads go, on this computer
    pub downloads: PathBuf,
    pub web_port: u16,
    pub control_port: u16,
    pub port_forwarding: bool,
    pub slskd_key: String,
    pub control_key: String,
    /// slskd's own web page login (it's in slskd.yml)
    pub slskd_password: String,
}

fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default() // a quoted string YAML and TOML both read
}

fn private(path: &Path, text: &str) -> Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::config::write_private(path, text.as_bytes()).with_context(|| format!("writing {}", path.display()))
}

pub fn compose_yaml(s: &Setup) -> String {
    let (uid, gid) = ids();
    format!(
        "# Made by `conductor setup` (Prelude): Soulseek downloads (slskd) only ever go through a VPN (gluetun).
# The secrets are in vpn.env and slskd/slskd.yml (readable only by you). Start: docker compose up -d
name: conductor-downloads
services:
  gluetun:
    image: qmcgaw/gluetun:latest
    container_name: conductor-gluetun
    cap_add: [NET_ADMIN]
    devices: [/dev/net/tun:/dev/net/tun]
    env_file: [vpn.env]
    environment:
      HTTP_CONTROL_SERVER_AUTH_CONFIG_FILEPATH: /gluetun/auth/config.toml
      VPN_PORT_FORWARDING: \"{pf}\"
    volumes:
      - ./gluetun:/gluetun
    ports:
      - \"127.0.0.1:{web}:5030\"     # slskd (its web page and Conductor's way in)
      - \"127.0.0.1:{control}:8000\"  # gluetun's control server (the VPN check, the forwarded port)
    restart: unless-stopped
  slskd:
    image: slskd/slskd:latest
    container_name: conductor-slskd
    network_mode: service:gluetun
    depends_on:
      gluetun: {{ condition: service_healthy }}
    user: \"{uid}:{gid}\"
    environment:
      SLSKD_REMOTE_CONFIGURATION: \"true\"
    volumes:
      - ./slskd:/app
      - {downloads}
    restart: unless-stopped
",
        pf = if s.port_forwarding { "on" } else { "off" },
        web = s.web_port,
        control = s.control_port,
        downloads = q(&format!("{}:/app/downloads", s.downloads.display())),
    )
}

pub fn vpn_env(v: &Vpn) -> String {
    let mut out = String::from("# The VPN's keys (gluetun). Keep this file to yourself.\nVPN_TYPE=wireguard\n");
    match v {
        Vpn::Conf(c) => {
            out += "VPN_SERVICE_PROVIDER=custom\n";
            out += &format!("WIREGUARD_ENDPOINT_IP={}\nWIREGUARD_ENDPOINT_PORT={}\n", c.endpoint_ip, c.endpoint_port);
            out += &format!("WIREGUARD_PUBLIC_KEY={}\nWIREGUARD_PRIVATE_KEY={}\nWIREGUARD_ADDRESSES={}\n", c.public_key, c.private_key, c.addresses);
            if let Some(p) = &c.preshared_key {
                out += &format!("WIREGUARD_PRESHARED_KEY={p}\n");
            }
        }
        Vpn::Provider { name, private_key, addresses, countries } => {
            out += &format!("VPN_SERVICE_PROVIDER={name}\nWIREGUARD_PRIVATE_KEY={private_key}\n");
            if let Some(a) = addresses.as_ref().filter(|a| !a.is_empty()) {
                out += &format!("WIREGUARD_ADDRESSES={a}\n");
            }
            if let Some(c) = countries.as_ref().filter(|c| !c.is_empty()) {
                out += &format!("SERVER_COUNTRIES={c}\n");
            }
        }
    }
    out
}

pub fn slskd_yaml(s: &Setup) -> String {
    format!(
        "# slskd's settings, made by `conductor setup`. Its web page: http://127.0.0.1:{web} (user slskd, the password below).
remote_configuration: true
directories:
  downloads: /app/downloads
  incomplete: /app/incomplete
soulseek:
  username: {user}
  password: {pass}
  listen_port: 50300
web:
  authentication:
    username: slskd
    password: {web_pass}
    api_keys:
      conductor:
        key: {key}
        role: administrator
        cidr: 0.0.0.0/0,::/0
",
        web = s.web_port,
        user = q(&s.soulseek_user),
        pass = q(&s.soulseek_password),
        web_pass = q(&s.slskd_password),
        key = q(&s.slskd_key),
    )
}

pub fn control_auth(key: &str) -> String {
    format!(
        "# Who may ask gluetun's control server what: Conductor, with its key.
[[roles]]
name = \"conductor\"
routes = [\"GET /v1/publicip/ip\", \"GET /v1/portforward\", \"GET /v1/openvpn/portforwarded\"]
auth = \"apikey\"
apikey = {}
",
        q(key)
    )
}

/// Write the folder (replacing what an earlier run made there).
pub fn write(s: &Setup) -> Result<()> {
    std::fs::create_dir_all(&s.dir)?;
    std::fs::create_dir_all(&s.downloads).with_context(|| format!("making {}", s.downloads.display()))?;
    std::fs::write(s.dir.join("compose.yml"), compose_yaml(s))?;
    private(&s.dir.join("vpn.env"), &vpn_env(&s.vpn))?;
    private(&s.dir.join("gluetun/auth/config.toml"), &control_auth(&s.control_key))?;
    private(&s.dir.join("slskd/slskd.yml"), &slskd_yaml(s))?;
    Ok(())
}

fn ids() -> (u32, u32) {
    #[cfg(unix)]
    {
        let uid = std::process::Command::new("id").arg("-u").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.trim().parse().ok());
        let gid = std::process::Command::new("id").arg("-g").output().ok().and_then(|o| String::from_utf8(o.stdout).ok()).and_then(|s| s.trim().parse().ok());
        (uid.unwrap_or(1000), gid.unwrap_or(1000))
    }
    #[cfg(not(unix))]
    {
        (1000, 1000)
    }
}

fn docker(dir: &Path, args: &[&str]) -> Result<String> {
    let o = Command::new("docker").arg("compose").args(args).current_dir(dir).output().context("running docker compose (is Docker installed?)")?;
    if !o.status.success() {
        bail!("docker compose {}: {}", args.join(" "), String::from_utf8_lossy(&o.stderr).lines().last().unwrap_or(""));
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn has_docker() -> bool {
    Command::new("docker").args(["compose", "version"]).output().is_ok_and(|o| o.status.success())
}

/// `docker compose config`: the file is valid (nothing is started).
pub fn validate(dir: &Path) -> Result<()> {
    docker(dir, &["config", "-q"]).map(|_| ())
}

pub fn start(dir: &Path) -> Result<()> {
    docker(dir, &["up", "-d"]).map(|_| ())
}

/// Wait (up to `secs`) for gluetun to say it's healthy: the tunnel is up.
pub fn wait_healthy(secs: u64) -> bool {
    for _ in 0..secs / 2 {
        let o = Command::new("docker").args(["inspect", "-f", "{{.State.Health.Status}}", "conductor-gluetun"]).output();
        if o.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "healthy") {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    false
}

/// Names resolve inside the tunnel.
pub fn dns_works(dir: &Path) -> bool {
    docker(dir, &["exec", "-T", "gluetun", "wget", "-q", "-T", "10", "-O", "/dev/null", "https://example.com"]).is_ok()
}

/// An slskd container already behind a VPN: (slskd's container, the one it shares its network with).
pub fn find_existing() -> Option<(String, String)> {
    let o = Command::new("docker").args(["ps", "--format", "{{.Names}}\t{{.Image}}"]).output().ok()?;
    let list = String::from_utf8_lossy(&o.stdout).into_owned();
    for line in list.lines() {
        let (name, image) = line.split_once('\t')?;
        if !image.contains("slskd") {
            continue;
        }
        let o = Command::new("docker").args(["inspect", "-f", "{{.HostConfig.NetworkMode}}", name]).output().ok()?;
        let mode = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if let Some(other) = mode.strip_prefix("container:") {
            let o = Command::new("docker").args(["inspect", "-f", "{{.Name}}", other]).output().ok()?;
            return Some((name.to_string(), String::from_utf8_lossy(&o.stdout).trim().trim_start_matches('/').to_string()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = "[Interface]\nPrivateKey = cFakePrivateKeyForTestsOnly0000000000000000=\nAddress = 10.2.0.2/32, fd00::2/128\nDNS = 10.2.0.1\n\n\
                        [Peer]\nPublicKey = sFakePublicKeyForTestsOnly00000000000000000=\nAllowedIPs = 0.0.0.0/0\nEndpoint = 127.0.0.1:51820\n";

    #[test]
    fn wireguard_conf() {
        let c = parse_wg(CONF).unwrap();
        assert_eq!((c.addresses.as_str(), c.endpoint_ip.as_str(), c.endpoint_port), ("10.2.0.2/32", "127.0.0.1", 51820));
        assert!(parse_wg("[Interface]\nPrivateKey = x\n").is_err());
        let env = vpn_env(&Vpn::Conf(c));
        assert!(env.contains("VPN_SERVICE_PROVIDER=custom") && env.contains("WIREGUARD_ENDPOINT_IP=127.0.0.1"));
    }
}
