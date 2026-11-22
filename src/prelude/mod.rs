//! Prelude, the setup wizard: `conductor setup` (or `conductor prelude`), also on the first start in a terminal when
//! there are no settings yet. Each step checks what it can, and running it again shows the current values as the
//! defaults and can change one part (`conductor setup vpn`):
//!
//!   music      the music folders: there, readable, songs counted, an optional sentinel file
//!   server     the data folder, the address and port
//!   admin      the first admin (the password typed hidden)
//!   downloads  Stagehand (optional): set the downloader up as one Docker container (slskd), or use your own slskd
//!              (its address, API key and downloads folder, tested), or none
//!   vpn        downloads through a VPN: the container inside one (gluetun) / your slskd already behind one / a
//!              proxy / none; the kill switch
//!   services   Lidarr, ListenBrainz, Last.fm, AudioMuse, Pitch, Metronome, Ensemble's public address, AI (off)
//!   import     Navidrome or music-lookup moving over; Transpose for an export (a preview, then a yes)
//!   service    run it as a service (systemd, the Windows service, or Docker), then a first scan
//!
//! `--yes` asks nothing: the flags (and the current settings) are the answers. Secrets (keys, tokens, the proxy,
//! the VPN's keys) go to secrets.toml (mode 600) or the VPN folder's own files, never to config.toml, the screen or
//! the log.

pub mod gluetun;
pub mod service;
pub mod ui;
pub mod web;

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use toml::Table;

use crate::config::{self, Config, SECRET_KEYS};
use ui::Ui;

pub const SECTIONS: &[&str] = &["music", "server", "admin", "downloads", "vpn", "services", "import", "service"];

const VALUE_FLAGS: &[&str] = &[
    "music", "sentinel", "data", "listen", "admin", "slskd-url", "slskd-key", "slskd-downloads", "downloads", "vpn", "vpn-check-url", "vpn-check-key",
    "wg-conf", "vpn-provider", "vpn-key", "vpn-countries", "slsk-user", "slsk-pass", "downloads-dir", "proxy", "lidarr-url", "lidarr-key",
    "listenbrainz-token", "lastfm-key", "audiomuse-url", "audiomuse-token", "songid-url", "mixinfo-dir", "jam-public-url", "ollama-url",
    "dj-voice-url", "import-navidrome", "import-music-lookup", "transpose", "service",
];
const SWITCHES: &[&str] = &["yes", "no-downloads", "require-vpn", "no-require-vpn", "port-forwarding", "start", "ai", "scan", "proxy-metadata"];

/// `conductor setup [SECTION] [--yes] [--flag value …]`.
#[derive(Debug, Default)]
pub struct Flags {
    pub section: Option<String>,
    values: HashMap<String, Vec<String>>,
    switches: HashSet<String>,
}

impl Flags {
    pub fn parse(args: &[&str]) -> Result<Flags> {
        let mut f = Flags::default();
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            if let Some(name) = a.strip_prefix("--") {
                if SWITCHES.contains(&name) {
                    f.switches.insert(name.to_string());
                } else if VALUE_FLAGS.contains(&name) {
                    let v = args.get(i + 1).with_context(|| format!("--{name} needs a value"))?;
                    f.values.entry(name.to_string()).or_default().push(v.to_string());
                    i += 1;
                } else {
                    bail!("unknown setting --{name} (see `conductor setup --help`)");
                }
            } else if SECTIONS.contains(&a) && f.section.is_none() {
                f.section = Some(a.to_string());
            } else {
                bail!("what is {a}? Parts: {}", SECTIONS.join(", "));
            }
            i += 1;
        }
        Ok(f)
    }
    pub fn get(&self, k: &str) -> Option<&str> {
        self.values.get(k).and_then(|v| v.last()).map(String::as_str)
    }
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.values.get(k).map(|v| v.iter().map(String::as_str).collect()).unwrap_or_default()
    }
    pub fn has(&self, k: &str) -> bool {
        self.switches.contains(k)
    }
    /// A secret: the flag, else the environment (CONDUCTOR_<ENV>), so it needn't be on the command line.
    pub fn secret(&self, k: &str, env: &str) -> Option<String> {
        self.get(k).map(String::from).or_else(|| config::var(env).ok()).filter(|v| !v.is_empty())
    }
}

/// The settings being made: config.toml and, apart, secrets.toml.
pub struct Draft {
    pub path: PathBuf,
    pub cfg: Table,
    pub secrets: Table,
}

impl Draft {
    pub fn load(path: &Path) -> Result<Draft> {
        let read = |p: &Path| -> Result<Table> {
            match std::fs::read_to_string(p) {
                Ok(t) => toml::from_str(&t).with_context(|| format!("reading {}", p.display())),
                Err(_) => Ok(Table::new()),
            }
        };
        let mut cfg = read(path)?;
        let mut secrets = read(&config::secrets_path(path))?;
        // secrets an older config.toml held move to secrets.toml
        for k in SECRET_KEYS {
            if let Some(v) = cfg.remove(*k) {
                secrets.entry(k.to_string()).or_insert(v);
            }
        }
        Ok(Draft { path: path.to_path_buf(), cfg, secrets })
    }
    pub fn str(&self, k: &str) -> Option<String> {
        self.cfg.get(k).or_else(|| self.secrets.get(k)).and_then(|v| v.as_str()).map(String::from).filter(|s| !s.is_empty())
    }
    pub fn bool(&self, k: &str) -> Option<bool> {
        self.cfg.get(k).and_then(|v| v.as_bool())
    }
    pub fn set(&mut self, k: &str, v: impl Into<toml::Value>) {
        if SECRET_KEYS.contains(&k) {
            self.cfg.remove(k);
            self.secrets.insert(k.into(), v.into());
        } else {
            self.cfg.insert(k.into(), v.into());
        }
    }
    pub fn unset(&mut self, k: &str) {
        self.cfg.remove(k);
        self.secrets.remove(k);
    }
    pub fn save(&self) -> Result<()> {
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let text = format!(
            "# Conductor's settings, made by `conductor setup` (run it again to change them; see config.example.toml).\n\
             # Keys and passwords are in secrets.toml beside this file.\n\n{}",
            toml::to_string_pretty(&self.cfg)?
        );
        std::fs::write(&self.path, text).with_context(|| format!("writing {}", self.path.display()))?;
        let sp = config::secrets_path(&self.path);
        if self.secrets.is_empty() {
            let _ = std::fs::remove_file(&sp);
        } else {
            let text = format!("# Conductor's secrets (read with config.toml). Keep this file to yourself.\n\n{}", toml::to_string_pretty(&self.secrets)?);
            config::write_private(&sp, text.as_bytes()).with_context(|| format!("writing {}", sp.display()))?;
        }
        Ok(())
    }
    pub fn config(&self) -> Result<Config> {
        self.save()?;
        Config::load(Some(self.path.clone()))
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).user_agent(crate::wiki::UA).build().expect("http client")
}

/// A long random secret (keys Prelude makes for slskd and gluetun).
pub fn new_key() -> String {
    format!("{}{}", crate::db::random_id(), crate::db::random_id())
}

pub struct Wizard<'a> {
    pub ui: &'a mut dyn Ui,
    pub f: Flags,
    pub d: Draft,
    /// the downloader container to make: asked for in the downloads step, written in the VPN step (inside a VPN)
    /// or at the end (on its own)
    make: Option<Downloader>,
}

/// The answers for the downloader container.
struct Downloader {
    soulseek_user: String,
    soulseek_password: String,
    downloads: String,
}

pub const HELP: &str = "conductor setup [PART] [--yes] [settings]   (also: conductor prelude)

Parts: music, server, admin, downloads, vpn, services, import, service (default: all).
--yes asks nothing: the settings below, then the current ones, are the answers.

  --music PATH (again for more)  --sentinel NAME  --data DIR  --listen ADDR:PORT
  --admin NAME                   (password: CONDUCTOR_PASSWORD)
  --downloads docker --slsk-user NAME (password: CONDUCTOR_SLSK_PASSWORD) [--downloads-dir DIR] [--start]
        (the downloader set up for you, one container)   or --slskd-url URL --slskd-key KEY --slskd-downloads DIR
        (your own slskd)   or --no-downloads
  --vpn existing --vpn-check-url URL [--vpn-check-key KEY]
  --vpn wireguard --wg-conf PATH | --vpn provider --vpn-provider NAME (key: CONDUCTOR_VPN_KEY)
        [--port-forwarding]   (the downloader container inside that VPN; Linux)
  --vpn proxy --proxy URL [--proxy-metadata]
  --vpn none
  --require-vpn | --no-require-vpn   (pause downloads while the VPN is down)
  --lidarr-url --lidarr-key  --listenbrainz-token  --lastfm-key  --audiomuse-url --audiomuse-token
  --songid-url  --mixinfo-dir  --jam-public-url  --ai --ollama-url --dj-voice-url
  --import-navidrome DB  --import-music-lookup DIR  --transpose EXPORT
  --service systemd-user|systemd-system|windows|docker|none  --scan

Secrets can come from the environment instead of the command line: CONDUCTOR_SLSKD_API_KEY, CONDUCTOR_VPN_CHECK_KEY,
CONDUCTOR_PROXY, CONDUCTOR_LIDARR_API_KEY, CONDUCTOR_LISTENBRAINZ_TOKEN, CONDUCTOR_LASTFM_API_KEY, CONDUCTOR_AUDIOMUSE_TOKEN.";

/// `conductor setup …` in a terminal (or with --yes, without questions). Returns the settings file.
pub async fn cli(config_path: Option<PathBuf>, args: &[&str]) -> Result<PathBuf> {
    if args.contains(&"--help") || args.contains(&"-h") {
        println!("{HELP}");
        return Ok(config_path.unwrap_or_else(config::default_config_path));
    }
    let stdin = std::io::stdin();
    let hide = std::io::IsTerminal::is_terminal(&stdin);
    if args.contains(&"--yes") {
        run(config_path, args, &mut ui::Auto { out: std::io::stdout() }).await
    } else {
        run(config_path, args, &mut ui::Term { input: stdin.lock(), out: std::io::stdout(), hide }).await
    }
}

/// Run Prelude. `config_path`: the one given with --config, else the default one. Returns the settings file.
pub async fn run(config_path: Option<PathBuf>, args: &[&str], ui: &mut dyn Ui) -> Result<PathBuf> {
    let f = Flags::parse(args)?;
    let explicit = config_path.is_some();
    let path = match config_path {
        Some(p) => p,
        None => f.get("data").map(|d| PathBuf::from(d).join("config.toml")).unwrap_or_else(config::default_config_path),
    };
    let fresh = !path.is_file();
    let d = Draft::load(&path)?;
    let mut w = Wizard { ui, f, d, make: None };
    w.ui.say("Prelude: setting up Conductor.");
    if !fresh {
        w.ui.say(&format!("Your settings: {} (the current values are the defaults; Enter keeps them).", w.d.path.display()));
    }
    let only: Vec<&str> = match w.f.section.clone() {
        Some(s) => vec![SECTIONS.iter().find(|x| **x == s).copied().unwrap_or("music")],
        None if !fresh && !w.ui.auto() => {
            let mut opts = vec!["Everything"];
            opts.extend(SECTIONS);
            match w.ui.choose("What would you like to set up?", &opts, 0) {
                0 => SECTIONS.to_vec(),
                n => vec![SECTIONS[n - 1]],
            }
        }
        None => SECTIONS.to_vec(),
    };
    for s in &only {
        match *s {
            "music" => w.music()?,
            "server" => w.server(explicit)?,
            "admin" => w.admin().await?,
            "downloads" => w.downloads().await?,
            "vpn" => w.vpn().await?,
            "services" => w.services().await?,
            "import" => {
                w.d.save()?;
                w.import().await?
            }
            "service" => {
                w.d.save()?;
                w.service().await?
            }
            _ => {}
        }
        w.d.save()?;
        // the downloader on its own: the VPN step didn't put it inside one (or didn't run)
        if (*s == "vpn" || (*s == "downloads" && !only.contains(&"vpn")))
            && let Some(a) = w.make.take()
        {
            w.make_downloader(None, a).await?;
            w.d.set("vpn", "none");
            w.d.set("vpn_port_sync", false);
            if !only.contains(&"vpn") {
                w.d.set("downloads_require_vpn", false); // the VPN step didn't run: no VPN, nothing to wait for
            }
            w.d.save()?;
        }
    }
    w.summary();
    Ok(w.d.path.clone())
}

impl Wizard<'_> {
    fn ask_or(&mut self, flag: &str, q: &str, current: Option<String>, fallback: &str) -> String {
        let def = self.f.get(flag).map(String::from).or(current).unwrap_or_else(|| fallback.to_string());
        self.ui.ask(q, &def)
    }
    fn secret_or(&mut self, flag: &str, env: &str, q: &str, current: Option<String>) -> String {
        let def = self.f.secret(flag, env).or(current);
        self.ui.secret(q, def.as_deref())
    }

    // ── 1. music ──
    fn music(&mut self) -> Result<()> {
        self.ui.say("\n1. Your music");
        let current: Vec<(String, Option<String>)> = self.d.cfg.get("libraries").and_then(|l| l.as_array()).into_iter().flatten()
            .filter_map(|l| Some((l.get("path")?.as_str()?.to_string(), l.get("sentinel").and_then(|s| s.as_str()).map(String::from))))
            .collect();
        let mut wanted: Vec<String> = self.f.all("music").iter().map(|s| s.to_string()).collect();
        if wanted.is_empty() {
            wanted = current.iter().map(|c| c.0.clone()).collect();
        }
        let mut libs = vec![];
        let mut i = 0;
        loop {
            let def = wanted.get(i).cloned().unwrap_or_default();
            let path = self.ui.ask(if i == 0 { "Music folder" } else { "Another music folder (Enter: no more)" }, &def);
            if path.is_empty() {
                if i == 0 && !self.ui.auto() {
                    self.ui.say("A music folder is needed.");
                    continue;
                }
                break;
            }
            let p = PathBuf::from(&path);
            match check_music(&p) {
                Ok(n) => self.ui.say(&format!("  ✓ {} songs in {}", n, p.display())),
                Err(e) => {
                    self.ui.say(&format!("  ✗ {e}"));
                    if !self.ui.yes("Use it anyway (e.g. a drive that isn't connected now)?", self.ui.auto()) {
                        continue;
                    }
                }
            }
            let cur_sentinel = current.iter().find(|c| c.0 == path).and_then(|c| c.1.clone());
            let sentinel = self.ask_or("sentinel", "A file that must be in it for the drive to count as connected (Enter: none)", cur_sentinel, "");
            let mut t = Table::new();
            t.insert("name".into(), (if i == 0 { "Music".to_string() } else { format!("Music {}", i + 1) }).into());
            t.insert("path".into(), path.clone().into());
            if !sentinel.is_empty() {
                if !p.join(&sentinel).exists() && self.ui.yes(&format!("{sentinel} isn't there yet; make it now?"), false) {
                    let _ = std::fs::write(p.join(&sentinel), b"conductor\n");
                }
                t.insert("sentinel".into(), sentinel.into());
            }
            libs.push(toml::Value::Table(t));
            i += 1;
            if self.ui.auto() && i >= wanted.len() {
                break;
            }
        }
        if !libs.is_empty() {
            self.d.set("libraries", toml::Value::Array(libs));
        }
        Ok(())
    }

    // ── 2. server ──
    fn server(&mut self, explicit_config: bool) -> Result<()> {
        self.ui.say("\n2. Where Conductor keeps its data, and its address");
        let cur = self.d.str("data_dir");
        let data = self.ask_or("data", "Data folder (the database; on the internal disk, not the music drive)", cur, &config::default_data_dir().to_string_lossy());
        self.d.set("data_dir", data.clone());
        if !explicit_config {
            let p = PathBuf::from(&data).join("config.toml");
            if p != self.d.path {
                self.d.path = p;
            }
        }
        let listen = self.ask_or("listen", "Address and port for the apps", self.d.str("listen"), "0.0.0.0:4533");
        match listen.parse::<std::net::SocketAddr>() {
            Ok(a) => {
                if std::net::TcpListener::bind(a).is_err() {
                    self.ui.say(&format!("  ! {a} is in use now (fine if that's Conductor itself)"));
                }
                self.d.set("listen", listen);
            }
            Err(_) => self.ui.say(&format!("  ✗ {listen} isn't an address:port; keeping the current one")),
        }
        Ok(())
    }

    // ── 3. admin ──
    async fn admin(&mut self) -> Result<()> {
        self.ui.say("\n3. Your admin account");
        let cfg = self.d.config()?;
        config::move_old_names(&cfg);
        std::fs::create_dir_all(&cfg.data_dir)?;
        let db = crate::db::open(&cfg.data_dir.join("conductor.db")).await?;
        let secrets = crate::auth::Secrets::load_or_create(&cfg.data_dir.join("secret.key"))?;
        let admins: Vec<String> = sqlx::query_scalar("SELECT name FROM user WHERE is_admin = 1 ORDER BY name").fetch_all(&db).await?;
        if !admins.is_empty() && !self.ui.yes(&format!("Admins: {}. Add another?", admins.join(", ")), false) {
            return Ok(());
        }
        let name = self.ask_or("admin", "Admin user name", None, if admins.is_empty() { "admin" } else { "" });
        if name.is_empty() {
            return Ok(());
        }
        let pw = loop {
            let a = self.secret_or("admin-password", "PASSWORD", &format!("Password for {name}"), None);
            if self.ui.auto() {
                break a;
            }
            if a.chars().count() < 6 {
                self.ui.say("At least 6 characters.");
                continue;
            }
            let b = self.ui.secret("Once more", None);
            if a == b {
                break a;
            }
            self.ui.say("They differ; again.");
        };
        if pw.chars().count() < 6 {
            self.ui.say("  ! no password (set CONDUCTOR_PASSWORD with --admin): no admin made");
            return Ok(());
        }
        match crate::auth::create_user(&db, &secrets, &name, &pw, true).await {
            Ok(_) => self.ui.say(&format!("  ✓ {name} can sign in (apps, and Podium at /admin)")),
            Err(e) => self.ui.say(&format!("  ✗ {e:#}")),
        }
        Ok(())
    }

    // ── 4. downloads (Stagehand) ──
    async fn downloads(&mut self) -> Result<()> {
        self.ui.say("\n4. Stagehand: songs you don't have, downloaded from Soulseek (optional: everything else works without it)");
        let have = self.d.str("slskd_url").is_some();
        let def = if self.f.has("no-downloads") {
            2
        } else if self.f.get("downloads") == Some("docker") || self.f.get("vpn") == Some("wireguard") || self.f.get("vpn") == Some("provider") {
            0
        } else if have || self.f.get("slskd-url").is_some() {
            1
        } else {
            2
        };
        let c = self.ui.choose(
            "Download songs you don't have?",
            &["Yes: set the downloader up for me (one Docker container)", "Yes, with my own slskd", "No"],
            def,
        );
        if c == 2 {
            for k in ["slskd_url", "slskd_api_key", "slskd_downloads"] {
                self.d.unset(k);
            }
            return Ok(());
        }
        if c == 0 {
            if !gluetun::has_docker() && !self.ui.auto() {
                self.ui.say("  ✗ Docker (with compose) isn't on this computer. Install it (docs.docker.com/get-docker), then run");
                self.ui.say("    conductor setup downloads   Downloads stay off until then.");
                return Ok(());
            }
            self.ui.say("  slskd, a Soulseek client, runs in its own container; Conductor asks it for songs and files what arrives.");
            let user = self.ask_or("slsk-user", "Your Soulseek user name (a new account is made if the name is free)", None, "");
            let pass = self.secret_or("slsk-pass", "SLSK_PASSWORD", "Its Soulseek password", None);
            if user.is_empty() || pass.is_empty() {
                self.ui.say("  ✗ a Soulseek user name and password are needed; downloads stay off");
                return Ok(());
            }
            let data = self.d.str("data_dir").map(PathBuf::from).unwrap_or_else(config::default_data_dir);
            let dl = self.ask_or("downloads-dir", "Where downloads go before they're filed", self.d.str("slskd_downloads"), &data.join("downloads").to_string_lossy());
            self.make = Some(Downloader { soulseek_user: user, soulseek_password: pass, downloads: dl });
            self.ui.say("  (the next step can put it inside a VPN)");
            return Ok(());
        }
        let url = self.ask_or("slskd-url", "slskd's address", self.d.str("slskd_url"), "http://127.0.0.1:5030");
        let key = self.secret_or("slskd-key", "SLSKD_API_KEY", "slskd's API key", self.d.str("slskd_api_key"));
        match slskd_check(&url, &key).await {
            Ok(note) => self.ui.say(&format!("  ✓ slskd answers: {note}")),
            Err(e) => self.ui.say(&format!("  ✗ slskd: {e:#} (saved anyway; check its address and key)")),
        }
        let dl = self.ask_or("slskd-downloads", "slskd's finished-downloads folder, as this computer sees it", self.d.str("slskd_downloads"), "");
        if !dl.is_empty() && !Path::new(&dl).is_dir() {
            self.ui.say(&format!("  ! {dl} isn't a folder here"));
        }
        self.d.set("slskd_url", url);
        if !key.is_empty() {
            self.d.set("slskd_api_key", key);
        }
        if !dl.is_empty() {
            self.d.set("slskd_downloads", dl);
        }
        self.check_writable();
        Ok(())
    }

    fn check_writable(&mut self) {
        let lib = self.d.cfg.get("libraries").and_then(|l| l.as_array()).and_then(|l| l.first()).and_then(|l| l.get("path")).and_then(|p| p.as_str()).map(PathBuf::from);
        if let Some(p) = lib {
            let probe = p.join(".conductor-write-test");
            match std::fs::write(&probe, b"x").and_then(|_| std::fs::remove_file(&probe)) {
                Ok(()) => self.ui.say("  ✓ the music folder is writable (downloads are filed into Songs/ and Trial/ there)"),
                Err(e) => self.ui.say(&format!("  ! the music folder isn't writable ({e}): downloads couldn't be filed into Songs/ and Trial/")),
            }
        }
    }

    // ── 5. VPN ──
    async fn vpn(&mut self) -> Result<()> {
        self.ui.say("\n5. Send downloads through a VPN?");
        self.ui.say("   On Soulseek, others see the address you download from. A VPN hides it.");
        if self.make.is_some() {
            let def = match self.f.get("vpn") {
                Some("wireguard" | "provider" | "gluetun") => 0,
                _ => 1,
            };
            let c = self.ui.choose("The downloader:", &["Inside a VPN (WireGuard; Linux with Docker)", "No VPN (this computer's own connection)"], def);
            if c == 1 {
                self.d.set("downloads_require_vpn", false); // no VPN: nothing to wait for
                return Ok(()); // made on its own after this step
            }
            self.vpn_gluetun().await?;
            if self.make.is_some() {
                // the VPN couldn't be set up here: the downloader comes without one, and the kill switch you ask for
                // keeps it from using the open connection
                self.ui.say("  The downloader is set up without a VPN.");
            }
            let def = if self.f.has("no-require-vpn") { false } else { self.f.has("require-vpn") || self.d.bool("downloads_require_vpn").unwrap_or(true) };
            let kill = self.ui.yes("Pause downloads whenever the VPN is down (never use the open connection)?", def);
            self.d.set("downloads_require_vpn", kill);
            return Ok(());
        }
        let cur = self.d.str("vpn").unwrap_or_else(|| "none".into());
        let def = match self.f.get("vpn").unwrap_or(&cur) {
            "existing" => 0,
            "wireguard" | "provider" | "gluetun" => 1,
            "proxy" => 2,
            _ => 3,
        };
        let c = self.ui.choose(
            "How?",
            &[
                "My slskd already runs behind a VPN (check it)",
                "Set it up for me: the downloader inside a VPN, with Docker (Linux)",
                "Use a proxy (a VPN app's SOCKS5/HTTP proxy; good on Windows)",
                "No VPN",
            ],
            def,
        );
        match c {
            0 => self.vpn_existing().await?,
            1 => self.vpn_gluetun().await?,
            2 => self.vpn_proxy().await?,
            _ => {
                self.d.set("vpn", "none");
                self.d.set("downloads_require_vpn", false);
                self.d.set("vpn_port_sync", false);
                return Ok(());
            }
        }
        let def = if self.f.has("no-require-vpn") { false } else { self.f.has("require-vpn") || self.d.bool("downloads_require_vpn").unwrap_or(true) };
        let kill = self.ui.yes("Pause downloads whenever the VPN is down (never use the open connection)?", def);
        self.d.set("downloads_require_vpn", kill);
        Ok(())
    }

    async fn report_check(&mut self) -> Option<bool> {
        let Ok(cfg) = self.d.config() else { return None };
        let s = crate::vpn::check(&cfg).await;
        match s.up {
            Some(true) => self.ui.say(&format!("  ✓ the VPN works: downloads don't use this computer's own address (exit: {})", s.country.as_deref().unwrap_or("?"))),
            _ => self.ui.say(&format!("  ✗ {}", s.problem.unwrap_or_else(|| "the VPN check failed".into()))),
        }
        s.up
    }

    async fn vpn_existing(&mut self) -> Result<()> {
        if let Some((slskd, tunnel)) = gluetun::find_existing() {
            self.ui.say(&format!("  slskd ({slskd}) shares its network with {tunnel}: its traffic goes where {tunnel} sends it."));
        }
        self.ui.say("  To check it, Conductor asks the VPN which address the downloads use: gluetun's control server does this.");
        let url = self.ask_or("vpn-check-url", "The check's address (gluetun: http://HOST:8000/v1/publicip/ip)", self.d.str("vpn_check_url"), "http://127.0.0.1:8000/v1/publicip/ip");
        let key = self.secret_or("vpn-check-key", "VPN_CHECK_KEY", "Its API key, if it needs one (Enter: none)", self.d.str("vpn_check_key"));
        self.d.set("vpn", "existing");
        self.d.set("vpn_check_url", url);
        if key.is_empty() {
            self.d.unset("vpn_check_key");
        } else {
            self.d.set("vpn_check_key", key);
        }
        self.report_check().await;
        Ok(())
    }

    async fn vpn_proxy(&mut self) -> Result<()> {
        if cfg!(windows) {
            self.ui.say("  On Windows a VPN app with split tunnelling also works: put slskd (and Conductor) in its tunnel.");
        }
        self.ui.say("  The proxy carries Conductor's own download traffic (the ⬇ catalog and its previews). slskd itself has a proxy setting too.");
        let url = self.secret_or("proxy", "PROXY", "Proxy address (socks5h://user:pass@host:1080 or http://host:3128)", self.d.str("proxy"));
        if reqwest::Proxy::all(&url).is_err() {
            self.ui.say("  ✗ that isn't a proxy address");
            return Ok(());
        }
        self.d.set("vpn", "proxy");
        self.d.set("proxy", url.clone());
        self.ui.say(&format!("  proxy: {}", crate::net::redact(&url)));
        let meta = self.ui.yes("Send cover and lyrics fetches through it too?", self.f.has("proxy-metadata") || self.d.bool("proxy_metadata").unwrap_or(false));
        self.d.set("proxy_metadata", meta);
        self.report_check().await;
        Ok(())
    }

    async fn vpn_gluetun(&mut self) -> Result<()> {
        if cfg!(windows) {
            self.ui.say("  This needs Linux with Docker. On Windows, use a proxy or a VPN app with split tunnelling.");
            return Ok(());
        }
        if !gluetun::has_docker() && !self.ui.auto() {
            self.ui.say("  ✗ Docker (with compose) isn't here. Install it, or choose another way.");
            return Ok(());
        }
        let from_conf = self.f.get("wg-conf").is_some() || self.f.get("vpn-provider").is_none();
        let how = self.ui.choose("The VPN:", &["A WireGuard .conf file from your provider (any provider)", "A provider gluetun knows, and its WireGuard key"], if from_conf { 0 } else { 1 });
        let vpn = if how == 0 {
            let p = self.ask_or("wg-conf", "Path to the .conf", None, "");
            let text = std::fs::read_to_string(&p).with_context(|| format!("reading {p}"))?;
            gluetun::Vpn::Conf(gluetun::parse_wg(&text)?)
        } else {
            self.ui.say(&format!("  e.g. {}", gluetun::PROVIDERS.join(", ")));
            let name = self.ask_or("vpn-provider", "Provider", None, "protonvpn").to_lowercase();
            let key = self.secret_or("vpn-key", "VPN_KEY", "Its WireGuard private key", None);
            if key.is_empty() {
                bail!("the provider's WireGuard private key is needed (--vpn-key or CONDUCTOR_VPN_KEY)");
            }
            let countries = self.ask_or("vpn-countries", "Countries to use (Enter: any)", None, "");
            gluetun::Vpn::Provider { name, private_key: key, addresses: None, countries: Some(countries).filter(|c| !c.is_empty()) }
        };
        let pf_default = self.f.has("port-forwarding") || matches!(&vpn, gluetun::Vpn::Provider { name, .. } if gluetun::PORT_FORWARDING.contains(&name.as_str()));
        let pf = self.ui.yes("Use the VPN's port forwarding (Proton VPN, PIA: more downloads work)?", pf_default);
        let a = match self.make.take() {
            Some(a) => a,
            None => {
                let user = self.ask_or("slsk-user", "Your Soulseek user name (a new one is made if it's free)", None, "");
                let pass = self.secret_or("slsk-pass", "SLSK_PASSWORD", "Its Soulseek password", None);
                let data = self.d.str("data_dir").map(PathBuf::from).unwrap_or_else(config::default_data_dir);
                let dl = self.ask_or("downloads-dir", "Where downloads go", None, &data.join("downloads").to_string_lossy());
                Downloader { soulseek_user: user, soulseek_password: pass, downloads: dl }
            }
        };
        self.make_downloader(Some((vpn, pf)), a).await
    }

    /// The downloader folder: written, checked, its address and key saved, and started if you say so.
    async fn make_downloader(&mut self, vpn: Option<(gluetun::Vpn, bool)>, a: Downloader) -> Result<()> {
        let base = self.d.path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let old = base.join("downloads-vpn"); // earlier versions' name for it
        let dir = if old.join("compose.yml").is_file() { old } else { base.join("downloader") };
        let web_port = free_port(5030);
        let control_port = free_port(8000);
        let with_vpn = vpn.is_some();
        let (vpn, pf) = match vpn {
            Some((v, pf)) => (Some(v), pf),
            None => (None, false),
        };
        let setup = gluetun::Setup {
            dir,
            vpn,
            soulseek_user: a.soulseek_user,
            soulseek_password: a.soulseek_password,
            downloads: PathBuf::from(&a.downloads),
            web_port,
            listen_port: if with_vpn { 50300 } else { free_port(50300) },
            control_port,
            port_forwarding: pf,
            slskd_key: new_key(),
            control_key: new_key(),
            slskd_password: new_key()[..20].to_string(),
        };
        gluetun::write(&setup)?;
        if with_vpn {
            self.ui.say(&format!("  ✓ written: {} (compose.yml; the keys in vpn.env and slskd/slskd.yml, readable only by you)", setup.dir.display()));
        } else {
            self.ui.say(&format!("  ✓ written: {} (compose.yml; the keys in slskd/slskd.yml, readable only by you)", setup.dir.display()));
            self.ui.say(&format!("    For more downloads, forward port {} on your router to this computer.", setup.listen_port));
        }
        if gluetun::has_docker() {
            match gluetun::validate(&setup.dir) {
                Ok(()) => self.ui.say("  ✓ docker compose accepts it"),
                Err(e) => self.ui.say(&format!("  ✗ {e:#}")),
            }
        }
        self.d.set("slskd_url", format!("http://127.0.0.1:{web_port}"));
        self.d.set("slskd_api_key", setup.slskd_key.clone());
        self.d.set("slskd_downloads", a.downloads.clone());
        if with_vpn {
            self.d.set("vpn", "gluetun");
            self.d.set("vpn_check_url", format!("http://127.0.0.1:{control_port}/v1/publicip/ip"));
            self.d.set("vpn_check_key", setup.control_key.clone());
            self.d.set("vpn_port_sync", pf);
        }
        if self.ui.yes("Start it now (docker compose up -d)?", self.f.has("start")) {
            gluetun::start(&setup.dir)?;
            if with_vpn {
                self.ui.say("  waiting for the tunnel…");
                if gluetun::wait_healthy(120) {
                    self.ui.say(if gluetun::dns_works(&setup.dir) { "  ✓ the tunnel is up and names resolve through it" } else { "  ✗ the tunnel is up but names don't resolve" });
                    self.report_check().await;
                } else {
                    self.ui.say("  ✗ the tunnel didn't come up in 2 minutes: docker logs conductor-gluetun says why");
                }
            } else {
                self.ui.say("  waiting for slskd…");
                let url = format!("http://127.0.0.1:{web_port}");
                let mut last = String::new();
                let mut up = false;
                for _ in 0..30 {
                    match slskd_check(&url, &setup.slskd_key).await {
                        Ok(note) => {
                            self.ui.say(&format!("  ✓ slskd answers: {note}"));
                            up = true;
                            break;
                        }
                        Err(e) => last = format!("{e:#}"),
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                if !up {
                    self.ui.say(&format!("  ✗ slskd didn't answer in a minute ({last}): docker logs conductor-slskd says why"));
                }
            }
        } else {
            self.ui.say(&format!("  Start it later: cd {} && docker compose up -d", setup.dir.display()));
        }
        self.check_writable();
        Ok(())
    }

    // ── 6. services ──
    async fn services(&mut self) -> Result<()> {
        self.ui.say("\n6. Other services (each optional)");
        // Lidarr
        let on = self.d.str("lidarr_url").is_some() || self.f.get("lidarr-url").is_some();
        if self.ui.yes("Lidarr (its wanted albums fetched from Soulseek)?", on) {
            let url = self.ask_or("lidarr-url", "Lidarr's address", self.d.str("lidarr_url"), "http://127.0.0.1:8686");
            let key = self.secret_or("lidarr-key", "LIDARR_API_KEY", "Lidarr's API key", self.d.str("lidarr_api_key"));
            let r = http().get(format!("{}/api/v1/system/status", url.trim_end_matches('/'))).header("X-Api-Key", &key).send().await;
            self.say_check("Lidarr", r.map(|r| r.status().is_success()).unwrap_or(false));
            self.d.set("lidarr_url", url);
            self.d.set("lidarr_api_key", key);
        } else {
            self.d.unset("lidarr_url");
        }
        // ListenBrainz
        let on = self.d.str("listenbrainz_token").is_some() || self.f.get("listenbrainz-token").is_some();
        if self.ui.yes("ListenBrainz (plays sent there, its discovery playlists)?", on) {
            let tok = self.secret_or("listenbrainz-token", "LISTENBRAINZ_TOKEN", "Your ListenBrainz token (listenbrainz.org/settings)", self.d.str("listenbrainz_token"));
            let base = self.d.str("listenbrainz_url").unwrap_or_else(|| Config::default().listenbrainz_url);
            let r = http().get(format!("{}/1/validate-token", base.trim_end_matches('/'))).header("Authorization", format!("Token {tok}")).send().await;
            let ok = match r {
                Ok(r) => r.json::<Value>().await.map(|v| v["valid"] == true).unwrap_or(false),
                Err(_) => false,
            };
            self.say_check("ListenBrainz", ok);
            self.d.set("listenbrainz_token", tok);
        }
        // Last.fm (Transpose by user name)
        let on = self.d.str("lastfm_api_key").is_some() || self.f.get("lastfm-key").is_some();
        if self.ui.yes("A Last.fm API key (to bring in Last.fm scrobbles)?", on) {
            let key = self.secret_or("lastfm-key", "LASTFM_API_KEY", "Last.fm API key (last.fm/api/account/create)", self.d.str("lastfm_api_key"));
            self.d.set("lastfm_api_key", key);
        }
        // AudioMuse
        let on = self.d.str("audiomuse_url").is_some() || self.f.get("audiomuse-url").is_some();
        if self.ui.yes("AudioMuse-AI (songs that sound alike)?", on) {
            let url = self.ask_or("audiomuse-url", "AudioMuse's address", self.d.str("audiomuse_url"), "http://127.0.0.1:8000");
            let tok = self.secret_or("audiomuse-token", "AUDIOMUSE_TOKEN", "Its API token (Enter: none)", self.d.str("audiomuse_token"));
            self.say_check("AudioMuse", http().get(&url).send().await.is_ok());
            self.d.set("audiomuse_url", url);
            if !tok.is_empty() {
                self.d.set("audiomuse_token", tok);
            }
        }
        // Pitch (song ID) and Metronome (Automix)
        let on = self.d.str("songid_url").is_some() || self.f.get("songid-url").is_some();
        if self.ui.yes("Pitch: \"What's playing?\" (the song-id service)?", on) {
            let url = self.ask_or("songid-url", "The song-id service's address", self.d.str("songid_url"), "http://127.0.0.1:4540");
            self.say_check("Pitch", http().get(format!("{}/status", url.trim_end_matches('/'))).send().await.is_ok_and(|r| r.status().is_success()));
            self.d.set("songid_url", url);
        }
        let on = self.d.str("mixinfo_dir").is_some() || self.f.get("mixinfo-dir").is_some();
        if self.ui.yes("Metronome: Automix's analysis (the mix analyzer's folder)?", on) {
            let dir = self.ask_or("mixinfo-dir", "Its folder", self.d.str("mixinfo_dir"), "");
            self.say_check("Metronome", Path::new(&dir).is_dir());
            self.d.set("mixinfo_dir", dir);
        }
        // Ensemble (Jam)
        let on = self.d.str("jam_public_url").is_some() || self.f.get("jam-public-url").is_some();
        if self.ui.yes("Ensemble: let friends join a Jam from the internet?", on) {
            let url = self.ask_or("jam-public-url", "The guest page's public address (https://…)", self.d.str("jam_public_url"), "");
            if !url.starts_with("https://") {
                self.ui.say("  ! not https: friends' browsers may warn");
            }
            self.d.set("jam_public_url", url);
        }
        // AI: off unless chosen
        let on = self.f.has("ai") || self.d.str("ollama_url").is_some();
        if self.ui.yes("AI features (Interlude, the DJ; Libretto, lyrics translation)? They use Ollama on your network", on) {
            let url = self.ask_or("ollama-url", "Ollama's address", self.d.str("ollama_url"), "http://127.0.0.1:11434");
            self.say_check("Ollama", http().get(format!("{}/api/tags", url.trim_end_matches('/'))).send().await.is_ok_and(|r| r.status().is_success()));
            self.d.set("ollama_url", url);
            let voice = self.ask_or("dj-voice-url", "Interlude's voice (dj-voice) address (Enter: none)", self.d.str("dj_voice_url"), "");
            if !voice.is_empty() {
                self.d.set("dj_voice_url", voice);
            }
        } else {
            self.d.unset("ollama_url");
            self.d.unset("dj_voice_url");
        }
        Ok(())
    }

    fn say_check(&mut self, what: &str, ok: bool) {
        self.ui.say(&if ok { format!("  ✓ {what} answers") } else { format!("  ✗ {what} doesn't answer (saved anyway)") });
    }

    // ── 7. import ──
    async fn import(&mut self) -> Result<()> {
        self.ui.say("\n7. Bring your music's history in");
        let nd = self.ask_or("import-navidrome", "Moving from Navidrome? A copy of its navidrome.db (Enter: skip)", None, "");
        let ml = self.ask_or("import-music-lookup", "music-lookup's data folder (Enter: skip)", None, "");
        let tp = self.ask_or("transpose", "Transpose: an export from Spotify, Apple Music, YouTube Music… (Enter: skip)", None, "");
        if nd.is_empty() && ml.is_empty() && tp.is_empty() {
            return Ok(());
        }
        let cfg = Config::load(Some(self.d.path.clone()))?;
        config::move_old_names(&cfg);
        std::fs::create_dir_all(&cfg.data_dir)?;
        let db = crate::db::open(&cfg.data_dir.join("conductor.db")).await?;
        let secrets = crate::auth::Secrets::load_or_create(&cfg.data_dir.join("secret.key"))?;
        let libs = crate::library::ensure_libraries(&db, &cfg.libraries).await?;
        if !nd.is_empty() && self.ui.yes(&format!("Import Navidrome's users, ids, plays and playlists from {nd}?"), true) {
            let key = config::var("ND_PASSWORD_KEY").ok();
            let rep = crate::import_nd::run(&cfg, &db, &secrets, &libs, Path::new(&nd), None, key.as_deref()).await?;
            self.ui.say(&format!("  ✓ {} songs keep their ids, {} playlists, {} plays", rep.songs_matched_after_scan, rep.playlists, rep.plays));
            self.ui.say(&format!("  ✓ passwords carried over for {} of {} users", rep.passwords_carried, rep.users_imported));
            if !rep.users.is_empty() {
                self.ui.say(&format!("  ! set a password again for: {} (Navidrome's own key: CONDUCTOR_ND_PASSWORD_KEY)", rep.users.join(", ")));
            }
        }
        if !ml.is_empty() && self.ui.yes(&format!("Import music-lookup's history and settings from {ml}?"), true) {
            let rep = crate::import_nd::music_lookup(&db, Path::new(&ml), None).await?;
            self.ui.say(&format!("  ✓ {} plays, {} podcasts", rep.plays_added, rep.podcasts));
        }
        if !tp.is_empty() {
            let st = crate::api::AppState::new(db.clone(), secrets, cfg, libs);
            let p = crate::import::parse_path(Path::new(&tp))?;
            let plan = crate::import::jobs::plan(&st, &p, &Default::default()).await?;
            let r = &plan.report;
            self.ui.say(&format!(
                "  Found ({}): {} liked songs, {} playlists, {} plays; {:.0}% of the songs are in your library",
                r.sources.join(", "),
                r.found.liked,
                r.found.playlists,
                r.found.plays,
                r.match_rate * 100.0
            ));
            let user: Option<String> = sqlx::query_scalar("SELECT id FROM user WHERE is_admin = 1 ORDER BY rowid LIMIT 1").fetch_optional(&db).await?;
            if let Some(user) = user
                && self.ui.yes("Bring these in now (it can be undone in Podium's Transpose)?", false)
            {
                let id = crate::db::random_id()[..16].to_string();
                sqlx::query("INSERT INTO import_batch (id, user_id, input_json, status, created_at) VALUES (?, ?, ?, 'running', ?)")
                    .bind(&id)
                    .bind(&user)
                    .bind(serde_json::json!({ "path": tp }).to_string())
                    .bind(crate::db::now())
                    .execute(&db)
                    .await?;
                let rep = crate::import::jobs::run(&st, &id, &user, &serde_json::json!({ "path": tp }), &Default::default()).await?;
                self.ui.say(&format!("  ✓ imported: {}", rep["changes"]));
            }
        }
        Ok(())
    }

    // ── 8. service ──
    async fn service(&mut self) -> Result<()> {
        self.ui.say("\n8. Running Conductor");
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("conductor"));
        let opts: Vec<&str> = if cfg!(windows) {
            vec!["As a Windows service (starts with Windows)", "Not now"]
        } else {
            vec!["A systemd service for you (starts when you log in, or always with lingering)", "A systemd system service (needs root)", "With Docker (a compose file to use)", "Not now"]
        };
        let def = match self.f.get("service") {
            Some("windows") | Some("systemd-user") => 0,
            Some("systemd-system") => 1,
            Some("docker") => 2,
            _ => opts.len() - 1,
        }
        .min(opts.len() - 1);
        let c = self.ui.choose("How should it run?", &opts, def);
        let cfg_path = self.d.path.clone();
        let res = match (cfg!(windows), c) {
            (true, 0) => service::windows(self.ui, &exe, &cfg_path),
            (false, 0) => service::systemd_user(self.ui, &exe, &cfg_path),
            (false, 1) => service::systemd_system(self.ui, &exe, &cfg_path),
            (false, 2) => service::docker(self.ui, &self.d),
            _ => Ok(()),
        };
        if let Err(e) = res {
            self.ui.say(&format!("  ✗ {e:#}"));
        }
        if self.ui.yes("Scan the music now?", self.f.has("scan")) {
            let cfg = Config::load(Some(cfg_path))?;
            config::move_old_names(&cfg);
            std::fs::create_dir_all(&cfg.data_dir)?;
            let db = crate::db::open(&cfg.data_dir.join("conductor.db")).await?;
            let libs = crate::library::ensure_libraries(&db, &cfg.libraries).await?;
            let rep = crate::library::scan(&db, &libs, &std::sync::Arc::new(crate::library::ScanStatus::default()), false).await?;
            self.ui.say(&format!("  ✓ scanned: {} songs added", rep.added));
        }
        Ok(())
    }

    fn summary(&mut self) {
        let set = |d: &Draft, k: &str| d.str(k).is_some();
        let libs: Vec<String> = self.d.cfg.get("libraries").and_then(|l| l.as_array()).into_iter().flatten()
            .filter_map(|l| l.get("path").and_then(|p| p.as_str()).map(String::from)).collect();
        let vpn = self.d.str("vpn").unwrap_or_else(|| "none".into());
        let lines = [
            format!("Settings: {} (secrets apart, in secrets.toml)", self.d.path.display()),
            format!("Music: {}", if libs.is_empty() { "none yet".into() } else { libs.join(", ") }),
            format!("Address: {}", self.d.str("listen").unwrap_or_else(|| "0.0.0.0:4533".into())),
            format!("Stagehand (downloads): {}", if set(&self.d, "slskd_url") { "on" } else { "off" }),
            format!("VPN for downloads: {vpn}{}", if self.d.bool("downloads_require_vpn") == Some(true) { ", downloads pause while it's down" } else { "" }),
            format!(
                "Also: {}",
                [("lidarr_url", "Lidarr"), ("listenbrainz_token", "ListenBrainz"), ("audiomuse_url", "AudioMuse"), ("songid_url", "Pitch"), ("mixinfo_dir", "Metronome"), ("jam_public_url", "Ensemble"), ("ollama_url", "AI")]
                    .iter()
                    .filter(|(k, _)| set(&self.d, k))
                    .map(|(_, n)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ];
        self.ui.say("\nDone.");
        for l in lines {
            self.ui.say(&format!("  {l}"));
        }
        self.ui.say("  Change any part later: conductor setup (or conductor setup vpn, …)");
    }
}

/// The music folder: there, readable, and how many songs (counted up to a million).
pub fn check_music(p: &Path) -> Result<usize> {
    if !p.is_dir() {
        bail!("{} isn't a folder here", p.display());
    }
    std::fs::read_dir(p).with_context(|| format!("{} can't be read", p.display()))?;
    let n = walkdir::WalkDir::new(p)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| crate::library::AUDIO.contains(&x.to_string_lossy().to_lowercase().as_str())))
        .take(1_000_000)
        .count();
    if n == 0 {
        bail!("no songs in {} (yet)", p.display());
    }
    Ok(n)
}

/// slskd answers with this key: its version and whether it's on Soulseek.
pub async fn slskd_check(url: &str, key: &str) -> Result<String> {
    let r = http().get(format!("{}/api/v0/application", url.trim_end_matches('/'))).header("X-API-Key", key).send().await.context("no answer")?;
    if r.status().as_u16() == 401 || r.status().as_u16() == 403 {
        bail!("the API key isn't accepted");
    }
    let v: Value = r.error_for_status()?.json().await?;
    let on = v["server"]["isLoggedIn"] == true;
    Ok(format!("version {}, {}", v["version"]["current"].as_str().or(v["version"].as_str()).unwrap_or("?"), if on { "on Soulseek" } else { "not on Soulseek right now" }))
}

/// This port, or the next free one.
fn free_port(want: u16) -> u16 {
    (want..want + 100).find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok()).unwrap_or(want)
}
