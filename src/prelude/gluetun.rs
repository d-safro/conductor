//! Prelude's "set it up for me": the downloader (slskd) as one Docker container, optionally inside a VPN (gluetun),
//! as the `downloads` part of Prelude's services folder (stack.rs).
//!
//!   downloads.yml       slskd alone, or gluetun + slskd (`network_mode: service:gluetun`: then slskd only ever
//!                       talks through the VPN)
//!   slskd/slskd.yml     slskd's settings: Soulseek login, API key for Conductor, remote configuration (mode 600)
//!   vpn.env             with a VPN: its keys (mode 600)
//!   gluetun/auth/config.toml   with a VPN: who may ask gluetun's control server what (Conductor; mode 600)
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

/// Providers whose forwarded ports gluetun can fetch over WireGuard (the others it knows do it only with OpenVPN).
pub const PORT_FORWARDING: &[&str] = &["protonvpn"];
/// Providers whose WireGuard setup in gluetun needs your own address too (WIREGUARD_ADDRESSES).
pub const NEEDS_ADDRESS: &[&str] = &["mullvad", "ivpn", "airvpn", "windscribe", "surfshark"];
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
    /// the one set up before: vpn.env stays as it is
    Keep,
}

#[derive(Debug, Clone)]
pub struct Setup {
    pub dir: PathBuf,
    /// None: slskd on its own, on this computer's connection
    pub vpn: Option<Vpn>,
    pub soulseek_user: String,
    pub soulseek_password: String,
    /// where finished downloads go, on this computer
    pub downloads: PathBuf,
    pub web_port: u16,
    /// Soulseek's listen port (others connect to it; forwarded from the router without a VPN)
    pub listen_port: u16,
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
    if s.vpn.is_none() {
        return format!(
            "# Made by `conductor setup` (Prelude): Soulseek downloads for Conductor (slskd), on this computer's connection.
# Its settings and keys are in slskd/slskd.yml (readable only by you). Start: docker compose up -d
services:
  slskd:
    image: slskd/slskd:latest
    container_name: conductor-slskd
    user: \"{uid}:{gid}\"
    environment:
      SLSKD_REMOTE_CONFIGURATION: \"true\"
    ports:
      - \"127.0.0.1:{web}:5030\"   # slskd's web page, and Conductor's way in
      - \"{listen}:{listen}\"      # Soulseek: other people connect here
    volumes:
      - ./slskd:/app{z}
      - {downloads}
    restart: unless-stopped
",
            web = s.web_port,
            listen = s.listen_port,
            z = super::stack::opts(false),
            downloads = q(&format!("{}:/app/downloads{}", s.downloads.display(), super::stack::opts(false))),
        );
    }
    format!(
        "# Made by `conductor setup` (Prelude): Soulseek downloads (slskd) only ever go through a VPN (gluetun).
# The secrets are in vpn.env and slskd/slskd.yml (readable only by you). Start: docker compose up -d
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
      - ./gluetun:/gluetun{z}
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
      - ./slskd:/app{z}
      - {downloads}
    restart: unless-stopped
",
        z = super::stack::opts(false),
        pf = if s.port_forwarding { "on" } else { "off" },
        web = s.web_port,
        control = s.control_port,
        downloads = q(&format!("{}:/app/downloads{}", s.downloads.display(), super::stack::opts(false))),
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
        Vpn::Keep => {}
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
  listen_port: {listen}
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
        listen = s.listen_port,
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
    super::stack::write_part(&s.dir, "downloads", &compose_yaml(s), &[])?;
    if let Some(vpn) = &s.vpn {
        match vpn {
            Vpn::Keep if !s.dir.join("vpn.env").is_file() => bail!("there's no VPN set up to keep ({} has no vpn.env)", s.dir.display()),
            Vpn::Keep => {}
            v => private(&s.dir.join("vpn.env"), &vpn_env(v))?,
        }
        private(&s.dir.join("gluetun/auth/config.toml"), &control_auth(&s.control_key))?;
    }
    private(&s.dir.join("slskd/slskd.yml"), &slskd_yaml(s))?;
    Ok(())
}

/// What an earlier run left in the folder. Written again, it's kept: the running slskd and gluetun know these keys,
/// and their containers hold these ports.
#[derive(Debug, Default)]
pub struct Current {
    /// downloads.yml has gluetun and vpn.env is there
    pub vpn: bool,
    pub web_port: Option<u16>,
    pub control_port: Option<u16>,
    pub listen_port: Option<u16>,
    pub slskd_key: Option<String>,
    pub web_password: Option<String>,
    pub soulseek_user: Option<String>,
    pub soulseek_password: Option<String>,
    pub control_key: Option<String>,
}

pub fn current(dir: &Path) -> Current {
    let compose = std::fs::read_to_string(dir.join("downloads.yml")).unwrap_or_default();
    let slskd = std::fs::read_to_string(dir.join("slskd/slskd.yml")).unwrap_or_default();
    let control_key = std::fs::read_to_string(dir.join("gluetun/auth/config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| t.get("roles")?.as_array()?.iter().find_map(|r| r.get("apikey")?.as_str().map(String::from)));
    let get = |path: &[&str]| yaml_get(&slskd, path);
    Current {
        vpn: compose.contains("conductor-gluetun") && dir.join("vpn.env").is_file(),
        web_port: super::stack::part_port(dir, "downloads", 5030),
        control_port: super::stack::part_port(dir, "downloads", 8000),
        listen_port: get(&["soulseek", "listen_port"]).and_then(|p| p.parse().ok()),
        slskd_key: get(&["web", "authentication", "api_keys", "conductor", "key"]),
        web_password: get(&["web", "authentication", "password"]),
        soulseek_user: get(&["soulseek", "username"]),
        soulseek_password: get(&["soulseek", "password"]),
        control_key,
    }
}

/// One value from a YAML file in block style (slskd.yml): `path` is the keys from the top. Enough for the files
/// written here; anything it can't read is None (and made again).
pub fn yaml_get(text: &str, path: &[&str]) -> Option<String> {
    let mut parents: Vec<(usize, &str)> = vec![];
    for line in text.lines() {
        let body = line.trim_start();
        if body.is_empty() || body.starts_with('#') || body.starts_with('-') {
            continue;
        }
        let indent = line.len() - body.len();
        let Some((k, v)) = body.split_once(':') else { continue };
        while parents.last().is_some_and(|(i, _)| *i >= indent) {
            parents.pop();
        }
        let (k, v) = (k.trim().trim_matches(['"', '\'']), v.trim());
        let here = parents.len();
        if here + 1 == path.len() && k == path[here] && parents.iter().zip(path).all(|((_, a), b)| a == b) {
            return unquote(v).filter(|v| !v.is_empty());
        }
        if v.is_empty() {
            parents.push((indent, k));
        }
    }
    None
}

fn unquote(v: &str) -> Option<String> {
    if v.starts_with('"') {
        serde_json::from_str(v).ok()
    } else if let Some(s) = v.strip_prefix('\'') {
        Some(s.strip_suffix('\'')?.replace("''", "'"))
    } else {
        Some(v.split(" #").next().unwrap_or("").trim().to_string())
    }
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

/// `docker compose config`: the files are valid (nothing is started).
pub fn validate(dir: &Path) -> Result<()> {
    super::stack::validate(dir)
}

pub fn start(dir: &Path) -> Result<()> {
    super::stack::up(dir)
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
    fn what_an_earlier_run_left() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Setup {
            dir: tmp.path().to_path_buf(),
            vpn: None,
            soulseek_user: "some one".into(),
            soulseek_password: "pa:ss \"x\"".into(),
            downloads: tmp.path().join("dl"),
            web_port: 5031,
            listen_port: 50302,
            control_port: 8001,
            port_forwarding: false,
            slskd_key: "the-key".into(),
            control_key: "c".into(),
            slskd_password: "web-pw".into(),
        };
        write(&s).unwrap();
        let c = current(tmp.path());
        assert!(!c.vpn);
        assert_eq!((c.web_port, c.listen_port, c.control_port), (Some(5031), Some(50302), None));
        assert_eq!(c.slskd_key.as_deref(), Some("the-key"));
        assert_eq!(c.web_password.as_deref(), Some("web-pw"));
        assert_eq!((c.soulseek_user.as_deref(), c.soulseek_password.as_deref()), (Some("some one"), Some("pa:ss \"x\"")));
        // with a VPN: gluetun's ports and its key too; keeping it needs its vpn.env
        let v = Setup { vpn: Some(Vpn::Keep), ..s.clone() };
        assert!(write(&v).is_err(), "nothing to keep yet");
        let v = Setup { vpn: Some(Vpn::Conf(parse_wg(CONF).unwrap())), ..s };
        write(&v).unwrap();
        let env = std::fs::read_to_string(tmp.path().join("vpn.env")).unwrap();
        write(&Setup { vpn: Some(Vpn::Keep), ..v }).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join("vpn.env")).unwrap(), env, "kept as it was");
        let c = current(tmp.path());
        assert!(c.vpn);
        assert_eq!((c.web_port, c.control_port, c.control_key.as_deref()), (Some(5031), Some(8001), Some("c")));
        assert_eq!(yaml_get("a:\n  b: 'it''s'\n", &["a", "b"]).as_deref(), Some("it's"));
        assert_eq!(yaml_get("a:\n  b: 1\nb: 2\n", &["b"]).as_deref(), Some("2"));
    }

    #[test]
    fn providers() {
        assert_eq!(PORT_FORWARDING, ["protonvpn"], "gluetun forwards ports over WireGuard only for these");
        let env = vpn_env(&Vpn::Provider { name: "mullvad".into(), private_key: "k".into(), addresses: Some("10.64.0.2/32".into()), countries: None });
        assert!(env.contains("WIREGUARD_ADDRESSES=10.64.0.2/32"), "{env}");
    }

    #[test]
    fn wireguard_conf() {
        let c = parse_wg(CONF).unwrap();
        assert_eq!((c.addresses.as_str(), c.endpoint_ip.as_str(), c.endpoint_port), ("10.2.0.2/32", "127.0.0.1", 51820));
        assert!(parse_wg("[Interface]\nPrivateKey = x\n").is_err());
        let env = vpn_env(&Vpn::Conf(c));
        assert!(env.contains("VPN_SERVICE_PROVIDER=custom") && env.contains("WIREGUARD_ENDPOINT_IP=127.0.0.1"));
    }
}
