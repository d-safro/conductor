//! Prelude, the setup wizard: `conductor setup` (or `conductor prelude`), also on the first start in a terminal when
//! there are no settings yet. Each step checks what it can, and running it again shows the current values as the
//! defaults and can change one part (`conductor setup vpn`):
//!
//!   music      the music folders: there, readable, songs counted, an optional sentinel file. With none yet (or
//!              --music auto), the drives here are looked at first: music already on them, or a new folder on one
//!   server     the data folder, the address and port
//!   admin      the first admin (the password typed hidden)
//!   downloads  Stagehand (optional): set the downloader up as one Docker container (slskd), or use your own slskd
//!              (its address, API key and downloads folder, tested), or none
//!   vpn        downloads through a VPN: the container inside one (gluetun) / your slskd already behind one / a
//!              proxy / none; the kill switch
//!   services   Lidarr, ListenBrainz, Last.fm, AudioMuse, Pitch, Metronome, Ensemble's public address, AI (off)
//!   domain     how people reach it from outside: your own domain with HTTPS (Caddy, set up for you), a Cloudflare
//!              Tunnel, Tailscale Funnel, or not now
//!   import     Navidrome or music-lookup moving over; Transpose for an export (a preview, then a yes)
//!   service    run it as a service (systemd, the Windows service, or Docker), then a first scan
//!
//! `--yes` asks nothing: the flags (and the current settings) are the answers. Secrets (keys, tokens, the proxy,
//! the VPN's keys) go to secrets.toml (mode 600) or the VPN folder's own files, never to config.toml, the screen or
//! the log.

pub mod drives;
pub mod gluetun;
pub mod service;
pub mod stack;
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

pub const SECTIONS: &[&str] = &["music", "server", "admin", "downloads", "vpn", "services", "domain", "import", "service"];

const VALUE_FLAGS: &[&str] = &[
    "music", "sentinel", "data", "listen", "admin", "slskd-url", "slskd-key", "slskd-downloads", "downloads", "docker", "vpn", "vpn-check-url", "vpn-check-key",
    "wg-conf", "vpn-provider", "vpn-key", "vpn-address", "vpn-countries", "slsk-user", "slsk-pass", "downloads-dir", "proxy", "lidarr-url", "lidarr-key",
    "listenbrainz-token", "lastfm-key", "lastfm-secret", "audiomuse-url", "audiomuse-token", "songid-url", "mixinfo-dir", "jam-public-url", "ollama-url",
    "dj-voice-url", "import-navidrome", "import-music-lookup", "transpose", "service", "domain", "jam-domain", "domain-email", "tunnel-token", "tunnel-host",
];
const SWITCHES: &[&str] =
    &["yes", "no-downloads", "require-vpn", "no-require-vpn", "port-forwarding", "start", "pull-model", "ai", "scan", "proxy-metadata", "funnel", "no-domain"];

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
    /// ports given out in this run (so two services set up now don't get the same one)
    ports: HashSet<u16>,
    /// the drives here (looked at once, when needed; tests give their own)
    drives: Option<Vec<drives::Drive>>,
    /// a new music folder was made on a drive: the downloads go beside it, not in the data folder
    downloads_near: Option<String>,
}

/// The answers for the downloader container.
struct Downloader {
    soulseek_user: String,
    soulseek_password: String,
    downloads: String,
}

pub const HELP: &str = "conductor setup [PART] [--yes] [settings]   (also: conductor prelude)

Parts: music, server, admin, downloads, vpn, services, domain, import, service (default: all).
--yes asks nothing: the settings below, then the current ones, are the answers.

  --music PATH (again for more; auto: the one the drives suggest)  --sentinel NAME  --data DIR  --listen ADDR:PORT
  --admin NAME                   (password: CONDUCTOR_PASSWORD)
  --downloads docker --slsk-user NAME (password: CONDUCTOR_SLSK_PASSWORD) [--downloads-dir DIR] [--start]
        (the downloader set up for you, one container)   or --slskd-url URL --slskd-key KEY --slskd-downloads DIR
        (your own slskd)   or --no-downloads
  --vpn existing --vpn-check-url URL [--vpn-check-key KEY]
  --vpn wireguard --wg-conf PATH | --vpn provider --vpn-provider NAME (key: CONDUCTOR_VPN_KEY)
        [--vpn-address ADDR (mullvad, ivpn, airvpn, windscribe, surfshark)] [--port-forwarding (protonvpn)]
        (the downloader container inside that VPN; Linux)
  --vpn proxy --proxy URL [--proxy-metadata]
  --vpn none
  --require-vpn | --no-require-vpn   (pause downloads while the VPN is down)
  --docker lidarr,audiomuse,pitch,metronome,ai   (set those up for you, in Docker; with --start, started;
        --pull-model also fetches the AI model, a few GB)
  --lidarr-url --lidarr-key  --listenbrainz-token  --lastfm-key --lastfm-secret  --audiomuse-url --audiomuse-token
  --songid-url  --mixinfo-dir  --jam-public-url  --ai --ollama-url --dj-voice-url
  --domain NAME [--jam-domain NAME] [--domain-email E]   (HTTPS on your domain: Caddy, set up for you)
        or --tunnel-token TOKEN (or CONDUCTOR_TUNNEL_TOKEN) --tunnel-host NAME   (a Cloudflare Tunnel)
        or --funnel (Tailscale Funnel)   or --no-domain (takes Caddy or the tunnel out)
  --import-navidrome DB  --import-music-lookup DIR  --transpose EXPORT
  --service systemd-user|systemd-system|windows|docker|none  --scan

Secrets can come from the environment instead of the command line: CONDUCTOR_SLSKD_API_KEY, CONDUCTOR_VPN_CHECK_KEY,
CONDUCTOR_PROXY, CONDUCTOR_LIDARR_API_KEY, CONDUCTOR_LISTENBRAINZ_TOKEN, CONDUCTOR_LASTFM_API_KEY, CONDUCTOR_AUDIOMUSE_TOKEN,
CONDUCTOR_TUNNEL_TOKEN.";

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
    run_with(config_path, args, ui, None).await
}

/// The same, with these drives instead of the ones found here (tests).
pub async fn run_with(config_path: Option<PathBuf>, args: &[&str], ui: &mut dyn Ui, drives: Option<Vec<drives::Drive>>) -> Result<PathBuf> {
    let f = Flags::parse(args)?;
    let explicit = config_path.is_some();
    let path = match config_path {
        Some(p) => p,
        None => f.get("data").map(|d| PathBuf::from(d).join("config.toml")).unwrap_or_else(config::default_config_path),
    };
    let fresh = !path.is_file();
    let d = Draft::load(&path)?;
    let mut w = Wizard { ui, f, d, make: None, ports: HashSet::new(), drives, downloads_near: None };
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
            "domain" => w.domain().await?,
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
        let mut vpn_ran = only.contains(&"vpn");
        // only the downloads step: a VPN set up before is never dropped without a word
        if *s == "downloads" && !vpn_ran && w.make.is_some() {
            match w.d.str("vpn").as_deref() {
                Some("gluetun") if gluetun::current(&stack::dir(&w.d.path)).vpn => {
                    w.ui.say("  It stays inside your VPN (conductor setup vpn changes that).");
                    let pf = w.d.bool("vpn_port_sync").unwrap_or(false);
                    let a = w.make.take().expect("asked for");
                    w.make_downloader(Some((gluetun::Vpn::Keep, pf)), a).await?;
                    w.d.save()?;
                }
                Some(v) if v != "none" => {
                    w.ui.say(&format!("  Your downloads used a VPN ({v}): choose how the new downloader does."));
                    w.vpn().await?;
                    vpn_ran = true;
                }
                _ => {}
            }
        }
        // the downloader on its own: the VPN step didn't put it inside one (or didn't run)
        if (*s == "vpn" || (*s == "downloads" && !only.contains(&"vpn")))
            && let Some(a) = w.make.take()
        {
            w.make_downloader(None, a).await?;
            w.d.set("vpn", "none");
            w.d.set("vpn_port_sync", false);
            if !vpn_ran {
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
        // --music auto: the folder the drives suggest
        let auto_pick = wanted.first().is_some_and(|w| w == "auto");
        if auto_pick {
            wanted[0] = String::new(); // not a folder: typed after all, nothing is offered
        }
        if wanted.is_empty() {
            wanted = current.iter().map(|c| c.0.clone()).collect();
        }
        // no music folder yet: look at the drives first (asking nothing, only with --music auto)
        let mut look = auto_pick || (wanted.is_empty() && !self.ui.auto());
        let mut libs = vec![];
        let mut i = 0;
        loop {
            let def = wanted.get(i).cloned().unwrap_or_default();
            let q = if i == 0 { "Music folder" } else { "Another music folder (Enter: no more)" };
            // made just now: empty, and that's fine
            let (path, new) = match look.then(|| self.pick_music()) {
                Some(Some(p)) => p,
                Some(None) if auto_pick && self.ui.auto() => (String::new(), false),
                _ => (self.ui.ask(q, &def), false),
            };
            look = false;
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
                Err(_) if new => self.ui.say(&format!("  ✓ {} is ready for your songs", p.display())),
                Err(e) => {
                    self.ui.say(&format!("  ✗ {e}"));
                    if !self.ui.yes("Use it anyway (e.g. a drive that isn't connected now)?", self.ui.auto()) {
                        continue;
                    }
                }
            }
            let cur_sentinel = current.iter().find(|c| c.0 == path).and_then(|c| c.1.clone());
            // on a drive that can be unplugged, the drive guard's file is the default
            let unplug = self.drive_of(&p).filter(|d| d.removable).map(|d| d.describe());
            if let Some(what) = &unplug {
                self.ui.say(&format!("  It's on a drive that can be unplugged ({what}). A file in it (the drive guard) tells Conductor the drive is really there,"));
                self.ui.say("  so while it's unplugged your songs stay in the library instead of looking deleted.");
            }
            let sentinel = self.ask_or("sentinel", "A file that must be in it for the drive to count as connected (Enter: none)", cur_sentinel, if unplug.is_some() { ".library-id" } else { "" });
            let mut t = Table::new();
            t.insert("name".into(), (if i == 0 { "Music".to_string() } else { format!("Music {}", i + 1) }).into());
            t.insert("path".into(), path.clone().into());
            if !sentinel.is_empty() {
                if !p.join(&sentinel).exists() && self.ui.yes(&format!("{sentinel} isn't there yet; make it now?"), unplug.is_some()) {
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

    /// The drives here, looked at once.
    fn drives(&mut self) -> &[drives::Drive] {
        self.drives.get_or_insert_with(drives::detect)
    }

    fn drive_of(&mut self, p: &Path) -> Option<drives::Drive> {
        let p = std::fs::canonicalize(p).map(drives::plain).unwrap_or_else(|_| p.to_path_buf());
        drives::drive_of(self.drives(), &p).cloned()
    }

    /// Where the library lives, from what's on the drives: a folder (and whether it was made just now), or None
    /// to type one.
    fn pick_music(&mut self) -> Option<(String, bool)> {
        let drives = self.drives().to_vec();
        if drives.is_empty() {
            return None;
        }
        self.ui.say("  Looking for music on this computer's drives…");
        let found = drives::find_music(&drives);
        let pick = drives::default_choice(&drives, &found);
        let about = |d: &drives::Drive| match d.describe() {
            s if s.is_empty() => String::new(),
            s => format!(" ({s})"),
        };
        let mut opts: Vec<String> = found
            .iter()
            .map(|f| {
                let d = &drives[f.drive];
                {
                let n = format!("{}{}", drives::thousands(f.songs), if f.more { "+" } else { "" });
                let songs = if f.songs == 1 && !f.more { "1 song".to_string() } else { format!("{n} songs") };
                format!("{}{} · {songs} · {} free", f.path.display(), about(d), drives::size(d.free))
            }
            })
            .collect();
        opts.push("Make a new music folder on a drive".into());
        opts.push("Type a path".into());
        let def = match pick {
            drives::Pick::Found(i) => i,
            drives::Pick::NewOn(_) => found.len(),
            drives::Pick::Type => found.len() + 1,
        };
        let refs: Vec<&str> = opts.iter().map(String::as_str).collect();
        let c = self.ui.choose("Where is your music (or where should it go)?", &refs, def);
        if c < found.len() {
            return Some((found[c].path.to_string_lossy().to_string(), false));
        }
        if c > found.len() {
            return None;
        }
        // a new folder: the drives with the most free space first
        let mut order: Vec<usize> = (0..drives.len()).filter(|i| drives[*i].kind.as_deref() != Some("network")).collect();
        order.sort_by_key(|i| std::cmp::Reverse(drives[*i].free));
        if order.is_empty() {
            return None;
        }
        let labels: Vec<String> = order.iter().map(|i| &drives[*i]).map(|d| format!("{}{} · {} free of {}", d.mount.display(), about(d), drives::size(d.free), drives::size(d.size))).collect();
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        let ddef = match pick {
            drives::Pick::NewOn(d) => order.iter().position(|x| *x == d).unwrap_or(0),
            _ => 0,
        };
        let d = drives[order[self.ui.choose("On which drive?", &refs, ddef)]].clone();
        let path = self.ui.ask("The new folder", &drives::new_folder(&d).to_string_lossy());
        if let Err(e) = std::fs::create_dir_all(&path) {
            self.ui.say(&format!("  ✗ can't make {path}: {e}"));
            return None;
        }
        self.ui.say(&format!("  ✓ made {path}"));
        // the downloads on the same drive, beside it (on the system drive they stay in the data folder)
        if !d.system {
            self.downloads_near = Path::new(&path).parent().map(|p| p.join("Downloads").to_string_lossy().to_string());
        }
        Some((path, true))
    }

    /// Where downloads go before they're filed, unless set already.
    fn downloads_default(&self) -> String {
        self.downloads_near.clone().unwrap_or_else(|| self.data_dir().join("downloads").to_string_lossy().to_string())
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
        // the Jam listener's usual port taken by something else (another Conductor, say): a free one near it, so
        // guests and share links don't silently go nowhere
        if self.d.cfg.get("jam_listen").is_none()
            && let Ok(a) = Config::default().jam_listen.parse::<std::net::SocketAddr>()
            && std::net::TcpListener::bind(a).is_err()
        {
            let p = self.port(a.port() + 1);
            self.d.set("jam_listen", format!("127.0.0.1:{p}"));
            self.ui.say(&format!("  ! {a} (for Jam guests and share links) is taken: they get 127.0.0.1:{p}"));
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
        // --admin NAME: that admin is made if it isn't there yet (with --yes too)
        let wanted = self.f.get("admin").map(String::from);
        if let Some(n) = wanted.as_deref().filter(|n| admins.iter().any(|a| a == n)) {
            self.ui.say(&format!("  ✓ {n} is an admin already"));
            return Ok(());
        }
        if !admins.is_empty() && !self.ui.yes(&format!("Admins: {}. Add another?", admins.join(", ")), wanted.is_some()) {
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
            let cur = gluetun::current(&stack::dir(&self.d.path));
            let user = self.ask_or("slsk-user", "Your Soulseek user name (a new account is made if the name is free)", cur.soulseek_user, "");
            let pass = self.secret_or("slsk-pass", "SLSK_PASSWORD", "Its Soulseek password", cur.soulseek_password);
            if user.is_empty() || pass.is_empty() {
                self.ui.say("  ✗ a Soulseek user name and password are needed; downloads stay off");
                return Ok(());
            }
            let dl = self.ask_or("downloads-dir", "Where downloads go before they're filed", self.d.str("slskd_downloads"), &self.downloads_default());
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
            let cur = self.d.str("vpn");
            let def = match self.f.get("vpn").or(cur.as_deref()) {
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
        let cur = gluetun::current(&stack::dir(&self.d.path));
        // a VPN set up here before can stay as it is (the default then)
        let can_keep = cur.vpn && self.d.str("vpn").as_deref() == Some("gluetun");
        let mut opts = vec!["A WireGuard .conf file from your provider (any provider)", "A provider gluetun knows, and its WireGuard key"];
        if can_keep {
            opts.insert(0, "Keep the current VPN");
        }
        let def = if self.f.get("wg-conf").is_some() {
            1
        } else if self.f.get("vpn-provider").is_some() {
            2
        } else if can_keep {
            0
        } else {
            1
        };
        let first = if can_keep { 0 } else { 1 };
        let how = self.ui.choose("The VPN:", &opts, def - first) + first;
        let vpn = match how {
            0 => Ok(gluetun::Vpn::Keep),
            1 => {
                let p = self.ask_or("wg-conf", "Path to the .conf", None, "");
                if p.is_empty() {
                    Err("no .conf given".to_string())
                } else {
                    std::fs::read_to_string(&p).with_context(|| format!("reading {p}")).and_then(|t| gluetun::parse_wg(&t)).map(gluetun::Vpn::Conf).map_err(|e| format!("{e:#}"))
                }
            }
            _ => {
                self.ui.say(&format!("  e.g. {}", gluetun::PROVIDERS.join(", ")));
                let name = self.ask_or("vpn-provider", "Provider", None, "protonvpn").to_lowercase();
                let key = self.secret_or("vpn-key", "VPN_KEY", "Its WireGuard private key", None);
                let needs = gluetun::NEEDS_ADDRESS.contains(&name.as_str());
                let address = if needs {
                    self.ask_or("vpn-address", "Its WireGuard address (the Address line of a .conf it gives, e.g. 10.64.0.2/32)", None, "")
                } else {
                    self.f.get("vpn-address").unwrap_or("").to_string()
                };
                let countries = self.ask_or("vpn-countries", "Countries to use (Enter: any)", None, "");
                if key.is_empty() {
                    Err("the provider's WireGuard private key is needed (--vpn-key or CONDUCTOR_VPN_KEY)".to_string())
                } else if needs && address.is_empty() {
                    Err(format!("{name} needs its WireGuard address too (--vpn-address)"))
                } else {
                    Ok(gluetun::Vpn::Provider { name, private_key: key, addresses: Some(address).filter(|a| !a.is_empty()), countries: Some(countries).filter(|c| !c.is_empty()) })
                }
            }
        };
        let vpn = match vpn {
            Ok(v) => v,
            Err(e) if can_keep => {
                self.ui.say(&format!("  ✗ {e}: the current VPN stays"));
                gluetun::Vpn::Keep
            }
            Err(e) => {
                self.ui.say(&format!("  ✗ {e}: no VPN set up"));
                return Ok(());
            }
        };
        let pf_default = self.f.has("port-forwarding")
            || match &vpn {
                gluetun::Vpn::Provider { name, .. } => gluetun::PORT_FORWARDING.contains(&name.as_str()),
                gluetun::Vpn::Keep => self.d.bool("vpn_port_sync").unwrap_or(false),
                gluetun::Vpn::Conf(_) => false,
            };
        let pf = self.ui.yes("Use the VPN's port forwarding (Proton VPN: more downloads work)?", pf_default);
        let a = match self.make.take() {
            Some(a) => a,
            None => {
                // Enter keeps what slskd has now
                let user = self.ask_or("slsk-user", "Your Soulseek user name (a new one is made if it's free)", cur.soulseek_user, "");
                let pass = self.secret_or("slsk-pass", "SLSK_PASSWORD", "Its Soulseek password", cur.soulseek_password);
                let dl = self.ask_or("downloads-dir", "Where downloads go", self.d.str("slskd_downloads"), &self.downloads_default());
                if user.is_empty() || pass.is_empty() {
                    self.ui.say("  ✗ a Soulseek user name and password are needed; nothing changed");
                    return Ok(());
                }
                Downloader { soulseek_user: user, soulseek_password: pass, downloads: dl }
            }
        };
        self.make_downloader(Some((vpn, pf)), a).await
    }

    /// The downloader folder: written, checked, its address and key saved, and started if you say so.
    async fn make_downloader(&mut self, vpn: Option<(gluetun::Vpn, bool)>, a: Downloader) -> Result<()> {
        let dir = stack::dir(&self.d.path);
        let cur = gluetun::current(&dir);
        let web_port = self.keep_port(cur.web_port, 5030);
        let control_port = self.keep_port(cur.control_port, 8000);
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
            // inside the VPN it's gluetun's own; on its own, the one this computer gave it before
            listen_port: if with_vpn { 50300 } else { self.keep_port(cur.listen_port.filter(|_| !cur.vpn), 50300) },
            control_port,
            port_forwarding: pf,
            slskd_key: cur.slskd_key.unwrap_or_else(new_key),
            control_key: cur.control_key.unwrap_or_else(new_key),
            slskd_password: cur.web_password.unwrap_or_else(|| new_key()[..20].to_string()),
        };
        gluetun::write(&setup)?;
        if with_vpn {
            self.ui.say(&format!("  ✓ written: {} (downloads.yml; the keys in vpn.env and slskd/slskd.yml, readable only by you)", setup.dir.display()));
        } else {
            self.ui.say(&format!("  ✓ written: {} (downloads.yml; the keys in slskd/slskd.yml, readable only by you)", setup.dir.display()));
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
            self.ui.say(&format!("  Start it later: cd {} && docker compose up -d --build", setup.dir.display()));
        }
        self.check_writable();
        Ok(())
    }

    // ── 6. services ──
    /// "Set it up for me" (Docker), "I have one already", or "No", for one service. 0, 1 or 2.
    fn how(&mut self, q: &str, part: &str, have: bool) -> usize {
        let docker_ok = gluetun::has_docker() || self.ui.auto();
        // --docker LIST: exactly those are set up for you (one that's there and not listed comes out)
        let list = self.f.get("docker");
        let wanted = list.is_some_and(|l| l.split(',').any(|x| x.trim() == part));
        let mine = stack::has_part(&stack::dir(&self.d.path), part);
        let def = match (wanted, mine, list.is_some()) {
            (true, _, _) => 0,
            (false, true, false) => 0,
            (false, true, true) => 2,
            _ if have => 1,
            _ => 2,
        };
        if docker_ok {
            self.ui.choose(q, &["Yes: set it up for me (Docker)", "Yes, I have one already", "No"], def)
        } else {
            match self.ui.choose(q, &["Yes, I have one already", "No"], if def == 2 { 1 } else { 0 }) {
                0 => 1,
                _ => 2,
            }
        }
    }

    /// The port a part had before (its own container holds it), else a free one.
    fn keep_port(&mut self, had: Option<u16>, want: u16) -> u16 {
        match had {
            Some(p) if self.ports.insert(p) => p,
            _ => self.port(want),
        }
    }

    /// A port on 127.0.0.1 that's free, and not given to another service in this run.
    fn port(&mut self, want: u16) -> u16 {
        let p = (want..want.saturating_add(200))
            .find(|p| !self.ports.contains(p) && std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
            .unwrap_or(want);
        self.ports.insert(p);
        p
    }

    fn all_music(&self) -> Vec<String> {
        self.d.cfg.get("libraries").and_then(|l| l.as_array()).into_iter().flatten().filter_map(|l| l.get("path").and_then(|p| p.as_str()).map(String::from)).collect()
    }

    fn first_music(&self) -> Option<String> {
        self.d.cfg.get("libraries").and_then(|l| l.as_array()).and_then(|l| l.first()).and_then(|l| l.get("path")).and_then(|p| p.as_str()).map(String::from)
    }

    fn data_dir(&self) -> PathBuf {
        self.d.str("data_dir").map(PathBuf::from).unwrap_or_else(config::default_data_dir)
    }

    /// Conductor's address as the services' containers reach it, and whether they can (it must listen beyond 127.0.0.1).
    fn conductor_from_docker(&self) -> (String, bool) {
        let listen = self.d.str("listen").unwrap_or_else(|| "0.0.0.0:4533".into());
        let port = listen.rsplit(':').next().unwrap_or("4533").to_string();
        (format!("http://host.docker.internal:{port}"), !local_only(&listen))
    }

    async fn services(&mut self) -> Result<()> {
        self.ui.say("\n6. Other services (each optional; \"set it up for me\" adds it to one Docker folder, started together)");
        let dir = stack::dir(&self.d.path);
        let mut made: Vec<&str> = vec![];
        let music = self.first_music();

        // Lidarr
        let c = self.how("Lidarr (albums you want, fetched from Soulseek)?", "lidarr", self.d.str("lidarr_url").is_some() || self.f.get("lidarr-url").is_some());
        match c {
            0 => match music.clone() {
                Some(m) => {
                    let port = self.keep_port(stack::part_port(&dir, "lidarr", 8686), 8686);
                    // the key Lidarr has already, if it's been set up here before
                    let key = stack::env_value(&dir.join("lidarr.env"), "LIDARR__AUTH__APIKEY").unwrap_or_else(|| new_key()[..32].to_string());
                    let dl = self.d.str("slskd_downloads");
                    let windows = cfg!(windows);
                    stack::lidarr(&dir, &stack::Lidarr { port, key: &key, music: &m, downloads: dl.as_deref(), windows })?;
                    self.d.set("lidarr_url", format!("http://127.0.0.1:{port}"));
                    self.d.set("lidarr_api_key", key);
                    // where it sees the downloads: the same place, or on Windows /downloads in its container
                    match stack::lidarr_paths(&m, dl.as_deref(), windows).1.filter(|_| windows) {
                        Some(inside) => self.d.set("lidarr_downloads", inside),
                        None => self.d.unset("lidarr_downloads"),
                    }
                    self.ui.say(&format!("  ✓ Lidarr: its page will be http://127.0.0.1:{port}; your music is its root folder"));
                    made.push("lidarr");
                }
                None => self.ui.say("  ✗ set the music folder up first (conductor setup music)"),
            },
            1 => {
                stack::remove_part(&dir, "lidarr")?;
                let url = self.ask_or("lidarr-url", "Lidarr's address", self.d.str("lidarr_url"), "http://127.0.0.1:8686");
                let key = self.secret_or("lidarr-key", "LIDARR_API_KEY", "Lidarr's API key", self.d.str("lidarr_api_key"));
                let r = http().get(format!("{}/api/v1/system/status", url.trim_end_matches('/'))).header("X-Api-Key", &key).send().await;
                self.say_check("Lidarr", r.map(|r| r.status().is_success()).unwrap_or(false));
                self.d.set("lidarr_url", url);
                self.d.set("lidarr_api_key", key);
            }
            _ => {
                stack::remove_part(&dir, "lidarr")?;
                self.d.unset("lidarr_url");
                self.d.unset("lidarr_api_key");
            }
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
        // Last.fm (Transpose by user name; with the shared secret, everyone can link their Last.fm to scrobble there)
        let on = self.d.str("lastfm_api_key").is_some() || self.f.get("lastfm-key").is_some();
        if self.ui.yes("A Last.fm API key (to bring in Last.fm scrobbles, and scrobble there)?", on) {
            let key = self.secret_or("lastfm-key", "LASTFM_API_KEY", "Last.fm API key (last.fm/api/account/create)", self.d.str("lastfm_api_key"));
            self.d.set("lastfm_api_key", key);
            let secret = self.secret_or(
                "lastfm-secret",
                "LASTFM_SECRET",
                "Its shared secret (shown with the key; lets everyone link their Last.fm in Encore; empty for none)",
                self.d.str("lastfm_secret"),
            );
            if secret.trim().is_empty() {
                self.d.unset("lastfm_secret");
            } else {
                self.d.set("lastfm_secret", secret.trim().to_string());
            }
        }
        // AudioMuse
        let c = self.how("AudioMuse-AI (songs that sound alike; it listens to each song once)?", "audiomuse", self.d.str("audiomuse_url").is_some() || self.f.get("audiomuse-url").is_some());
        match c {
            0 => {
                let (url, reachable) = self.conductor_from_docker();
                if !reachable {
                    self.ui.say("  ! Conductor listens only on 127.0.0.1, so AudioMuse (in Docker) can't reach it: use 0.0.0.0 in the address step");
                }
                let port = self.keep_port(stack::part_port(&dir, "audiomuse", 8000), 8010);
                // what AudioMuse has already, if it's been set up here before
                let env = dir.join("audiomuse.env");
                let token = stack::env_value(&env, "API_TOKEN").unwrap_or_else(new_key);
                let web = stack::env_value(&env, "AUDIOMUSE_PASSWORD").unwrap_or_else(|| new_key()[..20].to_string());
                let pass = stack::env_value(&env, "NAVIDROME_PASSWORD").unwrap_or_else(|| new_key()[..24].to_string());
                // its own account, to read the library with (not yours)
                self.service_account("audiomuse", &pass).await?;
                stack::audiomuse(&dir, &stack::AudioMuse { port, conductor_url: &url, user: "audiomuse", password: &pass, api_token: &token, web_password: &web })?;
                self.d.set("audiomuse_url", format!("http://127.0.0.1:{port}"));
                self.d.set("audiomuse_token", token);
                self.ui.say(&format!("  ✓ AudioMuse: its page will be http://127.0.0.1:{port} (user admin; the password is in audiomuse.env there)"));
                self.ui.say("    It reads your library with its own Conductor account, \"audiomuse\" (not an admin).");
                made.push("audiomuse");
            }
            1 => {
                stack::remove_part(&dir, "audiomuse")?;
                let url = self.ask_or("audiomuse-url", "AudioMuse's address", self.d.str("audiomuse_url"), "http://127.0.0.1:8000");
                let tok = self.secret_or("audiomuse-token", "AUDIOMUSE_TOKEN", "Its API token (Enter: none)", self.d.str("audiomuse_token"));
                self.say_check("AudioMuse", http().get(&url).send().await.is_ok());
                self.d.set("audiomuse_url", url);
                if !tok.is_empty() {
                    self.d.set("audiomuse_token", tok);
                }
            }
            _ => {
                stack::remove_part(&dir, "audiomuse")?;
                self.d.unset("audiomuse_url");
                self.d.unset("audiomuse_token");
            }
        }
        // Pitch (song ID)
        let c = self.how("Pitch: \"What's playing?\" against your own songs (offline)?", "pitch", self.d.str("songid_url").is_some() || self.f.get("songid-url").is_some());
        match c {
            0 => match music.clone() {
                Some(_) => {
                    let port = self.keep_port(stack::part_port(&dir, "pitch", 4540), 4540);
                    let songs: usize = self.all_music().iter().filter_map(|m| check_music(Path::new(m)).ok()).sum();
                    stack::pitch(&dir, port, &self.all_music(), songs)?;
                    self.d.set("songid_url", format!("http://127.0.0.1:{port}"));
                    let need = (songs as u64 * 80).div_ceil(1024).max(50);
                    self.ui.say(&match stack::pitch_memory(songs) {
                        Some(mb) => format!("  ✓ Pitch: it fingerprints your {songs} songs once, slowly, newest first (about {need} MB of memory; its limit: {mb} MB)"),
                        None => format!("  ✓ Pitch: it fingerprints your {songs} songs once, slowly, newest first. It needs about {need} MB of memory, so it has no limit: make sure the computer has it"),
                    });
                    self.ui.say("    (with many more songs later, run conductor setup services again: the limit grows with them)");
                    made.push("pitch");
                }
                None => self.ui.say("  ✗ set the music folder up first (conductor setup music)"),
            },
            1 => {
                stack::remove_part(&dir, "pitch")?;
                let url = self.ask_or("songid-url", "The song-id service's address", self.d.str("songid_url"), "http://127.0.0.1:4540");
                self.say_check("Pitch", http().get(format!("{}/status", url.trim_end_matches('/'))).send().await.is_ok_and(|r| r.status().is_success()));
                self.d.set("songid_url", url);
            }
            _ => {
                stack::remove_part(&dir, "pitch")?;
                self.d.unset("songid_url");
            }
        }
        // Metronome (Automix)
        let c = self.how("Metronome: Automix's analysis (where songs start, end and fade)?", "metronome", self.d.str("mixinfo_dir").is_some() || self.f.get("mixinfo-dir").is_some());
        match c {
            0 => match music.clone() {
                Some(_) => {
                    let out = self.data_dir().join("mixinfo").to_string_lossy().to_string();
                    stack::metronome(&dir, &self.all_music(), &out)?;
                    self.d.set("mixinfo_dir", out);
                    self.ui.say("  ✓ Metronome: the songs coming up in your queue first, then slowly the rest");
                    made.push("metronome");
                }
                None => self.ui.say("  ✗ set the music folder up first (conductor setup music)"),
            },
            1 => {
                stack::remove_part(&dir, "metronome")?;
                let d = self.ask_or("mixinfo-dir", "The mix analyzer's folder", self.d.str("mixinfo_dir"), "");
                self.say_check("Metronome", Path::new(&d).is_dir());
                self.d.set("mixinfo_dir", d);
            }
            _ => {
                stack::remove_part(&dir, "metronome")?;
                self.d.unset("mixinfo_dir");
            }
        }
        // Ensemble (Jam)
        let on = self.d.str("jam_public_url").is_some() || self.f.get("jam-public-url").is_some();
        if self.ui.yes("Ensemble: let friends join a Jam (and open share links) from the internet?", on) {
            let url = self.ask_or("jam-public-url", "The guest page's public address (https://…)", self.d.str("jam_public_url"), "");
            if !url.starts_with("https://") {
                self.ui.say("  ! not https: friends' browsers may warn");
            }
            self.d.set("jam_public_url", url);
        }
        // AI: off unless chosen
        let on = self.f.has("ai") || self.d.str("ollama_url").is_some();
        if self.ui.yes("AI features (Interlude, the DJ; Libretto, lyrics translation)? Local models only, nothing leaves this computer", on) {
            let have = self.d.str("ollama_url").is_some() || self.f.get("ollama-url").is_some();
            match self.how("Ollama and Interlude's voice?", "ai", have) {
                0 => {
                    // an Ollama already running here is used as it is (one set up here before is ours, and stays)
                    let ours = std::fs::read_to_string(dir.join("ai.yml")).is_ok_and(|t| t.contains("conductor-ollama"));
                    let running = !ours && http().get("http://127.0.0.1:11434/api/tags").send().await.is_ok_and(|r| r.status().is_success());
                    let ollama_port = if running { None } else { Some(self.keep_port(stack::part_port(&dir, "ai", 11434), 11434)) };
                    let voice = self.keep_port(stack::part_port(&dir, "ai", 4541), 4541);
                    stack::ai(&dir, ollama_port, voice)?;
                    self.d.set("ollama_url", format!("http://127.0.0.1:{}", ollama_port.unwrap_or(11434)));
                    self.d.set("dj_voice_url", format!("http://127.0.0.1:{voice}"));
                    if running {
                        self.ui.say("  ✓ Ollama: the one already running here; Interlude's voice in Docker");
                    } else {
                        self.ui.say("  ✓ Ollama and Interlude's voice (Piper; its voice, about 60 MB, comes the first time)");
                    }
                    made.push("ai");
                }
                1 => {
                    stack::remove_part(&dir, "ai")?;
                    let url = self.ask_or("ollama-url", "Ollama's address", self.d.str("ollama_url"), "http://127.0.0.1:11434");
                    self.say_check("Ollama", http().get(format!("{}/api/tags", url.trim_end_matches('/'))).send().await.is_ok_and(|r| r.status().is_success()));
                    self.d.set("ollama_url", url);
                    let voice = self.ask_or("dj-voice-url", "Interlude's voice (dj-voice) address (Enter: none)", self.d.str("dj_voice_url"), "");
                    if !voice.is_empty() {
                        self.d.set("dj_voice_url", voice);
                    }
                }
                _ => {
                    stack::remove_part(&dir, "ai")?;
                    self.d.unset("ollama_url");
                    self.d.unset("dj_voice_url");
                }
            }
        } else {
            stack::remove_part(&dir, "ai")?;
            self.d.unset("ollama_url");
            self.d.unset("dj_voice_url");
        }

        if made.is_empty() {
            return Ok(());
        }
        self.ui.say(&format!("  ✓ written: {} ({})", dir.display(), made.iter().map(|p| format!("{p}.yml")).collect::<Vec<_>>().join(", ")));
        if gluetun::has_docker() {
            match stack::validate(&dir) {
                Ok(()) => self.ui.say("  ✓ docker compose accepts it"),
                Err(e) => self.ui.say(&format!("  ✗ {e:#}")),
            }
        }
        self.d.save()?;
        if !self.ui.yes("Start them now (docker compose up -d --build; the first time takes a few minutes)?", self.f.has("start")) {
            self.ui.say(&format!("  Start them later: cd {} && docker compose up -d --build", dir.display()));
            return Ok(());
        }
        if let Err(e) = stack::up(&dir) {
            self.ui.say(&format!("  ✗ {e:#}"));
            return Ok(());
        }
        self.ui.say("  ✓ started");
        if made.contains(&"lidarr") {
            self.lidarr_root().await;
        }
        if made.contains(&"ai") {
            let model = self.d.str("translate_model").unwrap_or_else(|| Config::default().translate_model);
            // a few GB: only when asked for (--pull-model), never just because the services were started
            if self.ui.yes(&format!("Fetch the AI model now ({model}, a few GB)?"), self.f.has("pull-model")) {
                let target = if stack::has_part(&dir, "ai") && std::fs::read_to_string(dir.join("ai.yml")).is_ok_and(|t| t.contains("conductor-ollama")) { "conductor-ollama" } else { "" };
                if target.is_empty() {
                    self.ui.say(&format!("  On the Ollama already running here: ollama pull {model}"));
                } else {
                    match stack::exec(target, &["ollama", "pull", &model]) {
                        Ok(()) => self.ui.say(&format!("  ✓ {model} is ready")),
                        Err(e) => self.ui.say(&format!("  ✗ {e:#}")),
                    }
                }
            }
        }
        Ok(())
    }

    /// The account a service reads the library with: made, or its password set again.
    async fn service_account(&mut self, name: &str, password: &str) -> Result<()> {
        let data = self.data_dir();
        std::fs::create_dir_all(&data)?;
        let db = crate::db::open(&data.join("conductor.db")).await?;
        let secrets = crate::auth::Secrets::load_or_create(&data.join("secret.key"))?;
        if !crate::auth::set_password(&db, &secrets, name, password).await? {
            crate::auth::create_user(&db, &secrets, name, password, false).await?;
        }
        db.close().await;
        Ok(())
    }

    /// Lidarr's root folder: the music folder (once it answers).
    async fn lidarr_root(&mut self) {
        let (Some(url), Some(key), Some(music)) = (self.d.str("lidarr_url"), self.d.str("lidarr_api_key"), self.first_music()) else { return };
        let music = stack::lidarr_paths(&music, None, cfg!(windows)).0; // as Lidarr sees it
        let mut up = false;
        for _ in 0..45 {
            if http().get(format!("{url}/api/v1/system/status")).header("X-Api-Key", &key).send().await.is_ok_and(|r| r.status().is_success()) {
                up = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        if !up {
            self.ui.say("  ✗ Lidarr didn't answer in 90 seconds: docker logs conductor-lidarr says why");
            return;
        }
        let have: Vec<Value> = match http().get(format!("{url}/api/v1/rootfolder")).header("X-Api-Key", &key).send().await {
            Ok(r) => r.json().await.unwrap_or_default(),
            Err(_) => vec![],
        };
        if have.iter().any(|r| r["path"].as_str() == Some(music.as_str())) {
            self.ui.say("  ✓ Lidarr already has your music folder");
            return;
        }
        let body = serde_json::json!({ "name": "Music", "path": music, "defaultQualityProfileId": 1, "defaultMetadataProfileId": 1,
            "defaultMonitorOption": "none", "defaultNewItemMonitorOption": "none" });
        match http().post(format!("{url}/api/v1/rootfolder")).header("X-Api-Key", &key).json(&body).send().await {
            Ok(r) if r.status().is_success() => self.ui.say("  ✓ Lidarr: your music folder is its root folder"),
            Ok(r) => self.ui.say(&format!("  ✗ Lidarr didn't take the music folder ({}); add it in Lidarr › Settings › Media Management", r.status())),
            Err(e) => self.ui.say(&format!("  ✗ Lidarr: {e}")),
        }
    }

    fn say_check(&mut self, what: &str, ok: bool) {
        self.ui.say(&if ok { format!("  ✓ {what} answers") } else { format!("  ✗ {what} doesn't answer (saved anyway)") });
    }

    // ── 7. domain ──
    /// Conductor and its Jam listener as a proxy on this computer reaches them.
    fn upstreams(&mut self) -> (String, String) {
        if self.d.cfg.get("jam_listen").and_then(|v| v.as_str()) == Some("") {
            self.d.set("jam_listen", "127.0.0.1:4535");
            self.ui.say("  (the Jam listener was off; it's on again at 127.0.0.1:4535, and serves only Jam and share pages)");
        }
        let app = local_addr(&self.d.str("listen").unwrap_or_else(|| "0.0.0.0:4533".into()));
        let jam = local_addr(&self.d.str("jam_listen").unwrap_or_else(|| Config::default().jam_listen));
        (app, jam)
    }

    async fn domain(&mut self) -> Result<()> {
        self.ui.say("\n7. Reach Conductor from outside your home (optional)");
        let dir = stack::dir(&self.d.path);
        let (caddy, tunnel) = (stack::has_part(&dir, "caddy"), stack::has_part(&dir, "tunnel"));
        let def = if self.f.get("domain").is_some() {
            0
        } else if self.f.get("tunnel-host").is_some() || self.f.secret("tunnel-token", "TUNNEL_TOKEN").is_some() {
            1
        } else if self.f.has("funnel") {
            2
        } else if self.f.has("no-domain") {
            3
        } else if caddy {
            0
        } else if tunnel {
            1
        } else {
            3
        };
        let c = self.ui.choose(
            "How do people reach it from outside?",
            &[
                "My own domain, with HTTPS (Caddy, set up for me)",
                "A Cloudflare Tunnel (no ports to open)",
                "Tailscale Funnel (Jam and share links public; the apps on your tailnet)",
                "Not now",
            ],
            def,
        );
        let made = match c {
            0 => self.domain_caddy(&dir).await?,
            1 => self.domain_tunnel(&dir)?,
            2 => {
                // Funnel does it now: Caddy or a tunnel set up before would keep Conductor open on its old name too
                if self.domain_funnel() {
                    for (part, name) in [("caddy", "Caddy"), ("tunnel", "the Cloudflare Tunnel")] {
                        if stack::has_part(&dir, part) && self.ui.yes(&format!("Take {name} out (Funnel does it now)?"), true) {
                            stack::remove_part(&dir, part)?;
                            self.ui.say(&format!("  ✓ {name} is out (its container goes at the next docker compose up)"));
                        }
                    }
                }
                false
            }
            _ => {
                for (part, name) in [("caddy", "Caddy"), ("tunnel", "the Cloudflare Tunnel")] {
                    if stack::has_part(&dir, part) && self.ui.yes(&format!("Take {name} out?"), self.f.has("no-domain")) {
                        stack::remove_part(&dir, part)?;
                        self.ui.say(&format!("  ✓ {name} is out (its container goes at the next docker compose up); the public address for Jam stays until you change it"));
                    }
                }
                false
            }
        };
        if !made {
            return Ok(());
        }
        if gluetun::has_docker() {
            match stack::validate(&dir) {
                Ok(()) => self.ui.say("  ✓ docker compose accepts it"),
                Err(e) => self.ui.say(&format!("  ✗ {e:#}")),
            }
        }
        self.d.save()?;
        if self.ui.yes("Start it now (docker compose up -d)?", self.f.has("start")) {
            match stack::up(&dir) {
                Ok(()) => self.ui.say("  ✓ started (restart Conductor too if its address changed)"),
                Err(e) => self.ui.say(&format!("  ✗ {e:#}")),
            }
        } else {
            self.ui.say(&format!("  Start it later: cd {} && docker compose up -d", dir.display()));
        }
        Ok(())
    }

    async fn domain_caddy(&mut self, dir: &Path) -> Result<bool> {
        let (cur, cur_jam, cur_email) = stack::caddy_current(dir);
        let raw = self.ask_or("domain", "Your domain (e.g. music.example.com)", cur, "");
        let Some(domain) = clean_domain(&raw) else {
            self.ui.say(&format!("  ✗ \"{raw}\" isn't a domain name; nothing changed"));
            return Ok(false);
        };
        let jam = self.ask_or("jam-domain", "A second name for Jam guests and share links (Enter: the same one, at /jam/)", cur_jam, "");
        let jam = match (jam.is_empty(), clean_domain(&jam)) {
            (true, _) => None,
            (false, Some(j)) => Some(j).filter(|j| *j != domain),
            (false, None) => {
                self.ui.say(&format!("  ✗ \"{jam}\" isn't a domain name: Jam uses {domain}/jam/"));
                None
            }
        };
        let email = self.ask_or("domain-email", "Your email, for Let's Encrypt's notices about the certificate (Enter: none)", cur_email, "");
        // it goes into the Caddyfile as it is: one word with an @, else none
        let email = match email.trim() {
            "" => None,
            e if e.contains('@') && !e.contains(|c: char| c.is_whitespace() || c.is_control() || "{}\"#".contains(c)) => Some(e.to_string()),
            e => {
                self.ui.say(&format!("  ✗ \"{e}\" isn't an email address: none is given to Let's Encrypt"));
                None
            }
        };
        for name in std::iter::once(&domain).chain(jam.as_ref()) {
            self.check_dns(name).await;
        }
        self.ui.say("    People reach it on ports 80 and 443: behind a router, forward those two to this computer.");
        // with Caddy in front, only it needs to reach Conductor. Asked before the Caddyfile is written, which points
        // at the address Conductor listens on; offered first only when Caddy is new (run again, the address you
        // have now is the one you chose)
        let had_caddy = stack::has_part(dir, "caddy");
        let listen = self.d.str("listen").unwrap_or_else(|| "0.0.0.0:4533".into());
        if !local_only(&listen) {
            let port = listen.rsplit(':').next().unwrap_or("4533").to_string();
            let docker = stack::has_part(dir, "audiomuse");
            if docker {
                self.ui.say("    (AudioMuse, in Docker, reaches Conductor over this computer's address: it needs more than 127.0.0.1)");
            }
            let q = format!("Let only Caddy reach Conductor (listen on 127.0.0.1:{port}; your apps use https://{domain} then, at home too)?");
            if self.ui.yes(&q, !docker && !had_caddy && self.f.get("listen").is_none()) {
                self.d.set("listen", format!("127.0.0.1:{port}"));
                self.ui.say(&format!("  ✓ Conductor listens on 127.0.0.1:{port} (restart it for that)"));
            }
        }
        let (app, jam_at) = self.upstreams();
        stack::caddy(dir, &stack::Caddy { domain: &domain, jam_domain: jam.as_deref(), email: email.as_deref(), app: &app, jam: &jam_at })?;
        if stack::has_part(dir, "tunnel") {
            stack::remove_part(dir, "tunnel")?;
            self.ui.say("  (the Cloudflare Tunnel is out: Caddy does it now)");
        }
        let public = format!("https://{}", jam.as_deref().unwrap_or(&domain));
        self.d.set("jam_public_url", public.clone());
        self.ui.say(&format!("  ✓ written: {} (caddy.yml; caddy/Caddyfile: https://{domain} → Conductor, /jam/ → Jam)", dir.display()));
        self.ui.say(&format!("  ✓ Jam guests and share links: {public}"));
        if cfg!(windows) {
            self.ui.say(&format!("    On Windows, if Docker's host network doesn't reach Conductor, Caddy for Windows runs the same file: caddy run --config {}", dir.join("caddy/Caddyfile").display()));
        }
        Ok(true)
    }

    /// The name points at this computer's public address (as vpn_ip_url sees it). Neither address is shown.
    async fn check_dns(&mut self, name: &str) {
        let found = tokio::time::timeout(std::time::Duration::from_secs(5), tokio::net::lookup_host((name.to_string(), 443))).await;
        let addrs: Vec<std::net::IpAddr> = match found {
            Ok(Ok(a)) => a.map(|s| s.ip()).collect(),
            _ => vec![],
        };
        if addrs.is_empty() {
            self.ui.say(&format!("  ✗ {name} doesn't point anywhere yet: give it a DNS record (A) with this computer's public address. Caddy gets the certificate once it does."));
            return;
        }
        let url = self.d.str("vpn_ip_url").unwrap_or_else(|| Config::default().vpn_ip_url);
        let mine: Option<std::net::IpAddr> = match http().get(&url).send().await {
            Ok(r) => r.json::<Value>().await.ok().and_then(|v| v["ip"].as_str().and_then(|s| s.parse().ok())),
            Err(_) => None,
        };
        match mine {
            Some(ip) if addrs.contains(&ip) => self.ui.say(&format!("  ✓ {name} points at this computer's public address")),
            Some(_) => self.ui.say(&format!(
                "  ! {name} doesn't point at this computer's public address: check its DNS record (behind a router, the router's address, with ports 80 and 443 forwarded here)"
            )),
            None => self.ui.say(&format!("  ? {name} points somewhere; this computer's public address couldn't be checked")),
        }
    }

    fn domain_tunnel(&mut self, dir: &Path) -> Result<bool> {
        self.ui.say("  cloudflared, in its own container, connects out to Cloudflare: no port to open on your router.");
        self.ui.say("  Make the tunnel in Cloudflare's dashboard (Zero Trust › Networks › Tunnels › Create a tunnel, Cloudflared) and copy its token.");
        let cur = stack::env_value(&dir.join("tunnel.env"), "TUNNEL_TOKEN");
        let token = self.secret_or("tunnel-token", "TUNNEL_TOKEN", "The tunnel's token (the long text after --token)", cur);
        // the whole command pasted: its last word is the token
        let token = token.split_whitespace().last().unwrap_or("").to_string();
        if token.is_empty() {
            self.ui.say("  ✗ no token: nothing changed");
            return Ok(false);
        }
        stack::tunnel(dir, &token)?;
        if stack::has_part(dir, "caddy") {
            stack::remove_part(dir, "caddy")?;
            self.ui.say("  (Caddy is out: the tunnel does it now)");
        }
        let (app, jam) = self.upstreams();
        self.ui.say(&format!("  ✓ written: {} (tunnel.yml; the token in tunnel.env, readable only by you)", dir.display()));
        self.ui.say("  In the tunnel's Public Hostname tab, add:");
        self.ui.say(&format!("    your name, path ^/jam/  →  http://{jam}   (Jam guests and share links; first)"));
        self.ui.say(&format!("    the same name           →  http://{app}   (the apps)"));
        let cur_host = self.d.str("jam_public_url").map(|u| u.trim_start_matches("https://").trim_end_matches('/').to_string());
        let host = self.ask_or("tunnel-host", "That public hostname (e.g. music.example.com)", cur_host, "");
        match clean_domain(&host) {
            Some(h) => {
                self.d.set("jam_public_url", format!("https://{h}"));
                self.ui.say(&format!("  ✓ Jam guests and share links: https://{h}"));
            }
            None if host.is_empty() => self.ui.say("  ! no hostname yet: set it later (conductor setup domain)"),
            None => self.ui.say(&format!("  ✗ \"{host}\" isn't a hostname: set it later (conductor setup domain)")),
        }
        Ok(true)
    }

    /// True when Funnel is set up (Tailscale is here and signed in).
    fn domain_funnel(&mut self) -> bool {
        let run = |args: &[&str]| std::process::Command::new("tailscale").args(args).output().ok().filter(|o| o.status.success());
        if run(&["version"]).is_none() {
            self.ui.say("  ✗ Tailscale isn't on this computer: install it (tailscale.com/download), sign in (tailscale up), then run conductor setup domain");
            return false;
        }
        let name = run(&["status", "--json"])
            .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
            .and_then(|v| v["Self"]["DNSName"].as_str().map(|s| s.trim_end_matches('.').to_string()))
            .filter(|s| !s.is_empty());
        let Some(name) = name else {
            self.ui.say("  ✗ Tailscale isn't signed in, so this computer has no name yet: tailscale up, then conductor setup domain");
            return false;
        };
        let (app, jam) = self.upstreams();
        self.ui.say(&format!("  Funnel puts the Jam listener (it serves nothing but Jam and share pages) on the internet at https://{name};"));
        self.ui.say(&format!("  the apps stay on your tailnet, at https://{name}:8443. These two do it (they stay set after a restart):"));
        let jam_target = format!("http://{jam}");
        let app_target = format!("http://{app}");
        let cmds: [Vec<&str>; 2] = [vec!["tailscale", "funnel", "--bg", &jam_target], vec!["tailscale", "serve", "--bg", "--https=8443", &app_target]];
        for c in &cmds {
            self.ui.say(&format!("    {}", c.join(" ")));
        }
        if self.ui.yes("Run them now?", self.f.has("start")) {
            for c in &cmds {
                match run(&c[1..]) {
                    Some(_) => self.ui.say(&format!("  ✓ {}", c.join(" "))),
                    None => self.ui.say(&format!("  ✗ {} didn't work (as root, or after: sudo tailscale set --operator=$USER)", c.join(" "))),
                }
            }
        }
        self.d.set("jam_public_url", format!("https://{name}"));
        self.ui.say(&format!("  ✓ Jam guests and share links: https://{name}"));
        true
    }

    // ── 8. import ──
    async fn import(&mut self) -> Result<()> {
        self.ui.say("\n8. Bring your music's history in");
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
        // a server in use: importing replaces its plays, hearts and playlists, so only when you say so (never with --yes)
        let replace = !nd.is_empty() && crate::import_nd::has_data(&db).await?;
        let go = !nd.is_empty()
            && self.ui.yes(&format!("Import Navidrome's users, ids, plays and playlists from {nd}?"), true)
            && (!replace
                || self.ui.yes("This server has plays, hearts or playlists already: replace them all with Navidrome's (a backup first: conductor backup)?", false));
        if replace && !go && !self.ui.auto() {
            self.ui.say("  Navidrome not imported: nothing changed");
        } else if replace && !go {
            self.ui.say(&format!("  ✗ this server has plays, hearts or playlists already, which importing replaces: nothing changed (to do it: conductor import-navidrome {nd} --replace)"));
        }
        if go {
            let key = config::var("ND_PASSWORD_KEY").ok();
            let rep = crate::import_nd::run(&cfg, &db, &secrets, &libs, Path::new(&nd), None, key.as_deref(), replace).await?;
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
            // the songs are matched against the library, which a new server hasn't scanned yet (its service starts
            // after this step): without this, everything showed as 0% here and nothing came in
            if let Err(e) = crate::library::scan(&st.db, &st.libs, &st.scan, false).await {
                self.ui.say(&format!("  ! the music couldn't be scanned first ({e:#}): fewer songs may match"));
            }
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
                // asked for by name (a path typed, or --transpose): yes unless you say no
                && self.ui.yes("Bring these in now (it can be undone in Podium's Transpose)?", true)
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
        self.ui.say("\n9. Running Conductor");
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
            format!("From outside (Jam, share links): {}", self.d.str("jam_public_url").unwrap_or_else(|| "not set up".into())),
            format!("Also: {}", {
                let also: Vec<&str> = [("lidarr_url", "Lidarr"), ("listenbrainz_token", "ListenBrainz"), ("audiomuse_url", "AudioMuse"), ("songid_url", "Pitch"), ("mixinfo_dir", "Metronome"), ("jam_public_url", "Ensemble"), ("ollama_url", "AI")]
                    .iter()
                    .filter(|(k, _)| set(&self.d, k))
                    .map(|(_, n)| *n)
                    .collect();
                if also.is_empty() { "nothing else".to_string() } else { also.join(", ") }
            }),
        ];
        self.ui.say("\nDone.");
        for l in lines {
            self.ui.say(&format!("  {l}"));
        }
        self.ui.say("  Change any part later: conductor setup (or conductor setup vpn, …)");
    }
}

/// Where a proxy on this computer reaches a listener: "0.0.0.0:4533" (every address) is 127.0.0.1:4533.
pub fn local_addr(listen: &str) -> String {
    match listen.rsplit_once(':') {
        Some(("0.0.0.0" | "[::]" | "" | "localhost", p)) => format!("127.0.0.1:{p}"),
        Some(_) => listen.to_string(),
        None => format!("127.0.0.1:{listen}"),
    }
}

fn local_only(listen: &str) -> bool {
    listen.starts_with("127.") || listen.starts_with("[::1]") || listen.starts_with("localhost")
}

/// A domain name as typed ("https://Music.Example.com/" is music.example.com), or None.
pub fn clean_domain(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches("https://").trim_start_matches("http://").trim_end_matches('/').to_lowercase();
    let label = |l: &str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    (s.contains('.') && s.split('.').all(label)).then_some(s)
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_names_and_addresses() {
        assert_eq!(clean_domain(" https://Music.Example.com/ ").as_deref(), Some("music.example.com"));
        for bad in ["", "localhost", "music example.com", "-a.example.com", "a..example.com", "example.com:443"] {
            assert_eq!(clean_domain(bad), None, "{bad}");
        }
        assert_eq!(local_addr("0.0.0.0:4533"), "127.0.0.1:4533");
        assert_eq!(local_addr("[::]:4533"), "127.0.0.1:4533");
        assert_eq!(local_addr("192.168.1.5:4533"), "192.168.1.5:4533");
        assert_eq!(local_addr("127.0.0.1:4535"), "127.0.0.1:4535");
    }
}
