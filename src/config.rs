//! Settings: `config.toml` (path from `--config` or `CONDUCTOR_CONFIG`), then `CONDUCTOR_*` environment variables.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Where the database, the password key and caches live. Keep it on the internal disk, not the music drive.
    pub data_dir: PathBuf,
    /// Address for the apps (Subsonic API).
    pub listen: SocketAddr,
    /// Never write to the music folders: no downloads filed, no trial songs moved or deleted, no covers saved.
    /// For running beside another server (a trial run) on the same music.
    pub read_only: bool,
    /// Music folders. Each one is a library.
    pub libraries: Vec<LibraryConfig>,
    /// Minutes between full scans (0: only at start and on request).
    pub scan_interval_min: u64,
    /// ffmpeg for transcoding; found on PATH when not set.
    pub ffmpeg: Option<PathBuf>,
    /// how many conversions may work at the same time (0: half the cores, at least 2)
    pub transcode_slots: usize,
    /// the most the conversions kept in the cache may take, in MB
    pub transcode_cache_mb: u64,
    /// Sound levelling: measure the loudness of songs whose files have no ReplayGain tags, in the background (ffmpeg
    /// at the lowest priority), so the apps can play every song at the same loudness. See src/loudness.rs.
    pub loudness_analysis: bool,
    /// how many songs are measured at the same time
    pub loudness_threads: usize,
    /// Address of the Jam guest page (the only thing to put on the internet). Empty string: no Jam listener.
    pub jam_listen: String,
    /// The guest page's public address (e.g. https://music.example.com:8443). Without it, Jam can't start.
    pub jam_public_url: Option<String>,
    /// Where podcast episodes saved on the server go (default: <data_dir>/podcasts). Not the music folder.
    pub podcast_dir: Option<PathBuf>,
    /// Also keep the newest N episodes of every show saved on the server (0: only the ones asked for).
    pub podcast_keep: i64,
    /// Played episodes saved on the server go after this many days (0: never).
    pub podcast_clean_days: i64,
    /// Let podcast feeds, episodes and pictures come from this computer or your own network (off: internet only).
    pub podcast_local: bool,
    /// Daily backups kept (0: none) and where (default: <data_dir>/backups). See src/backup.rs.
    pub backup_keep: usize,
    pub backup_dir: Option<PathBuf>,
    /// Songs you don't have (⬇) in searches, albums and artists (from Deezer). When it isn't set, it's on only if
    /// something can get those songs (slskd or Lidarr): without downloads, nothing shows that can't be played.
    pub catalog: bool,
    /// Apps that never get ⬇ items (part of the client name, any case).
    pub catalog_skip_clients: Vec<String>,
    pub deezer_url: String,
    /// slskd (Soulseek) for downloading songs asked for: its address and API key.
    pub slskd_url: Option<String>,
    pub slskd_api_key: Option<String>,
    /// slskd's finished-downloads folder, as Conductor sees it (path mapping when they run on different systems).
    pub slskd_downloads: Option<PathBuf>,
    /// The music folder downloads are filed into (a library name; default: the first).
    pub import_library: Option<String>,
    /// Songs you play but don't have go on trial: deleted this long after they arrive unless you keep them.
    pub trial_hours: f64,
    /// At most this many new trial downloads a day.
    pub trial_per_day: i64,
    /// Lidarr: albums it wants are fetched from Soulseek (one folder from one user) and handed to it to import.
    pub lidarr_url: Option<String>,
    pub lidarr_api_key: Option<String>,
    /// The downloads folder as Lidarr sees it (default: the same as slskd_downloads).
    pub lidarr_downloads: Option<String>,
    /// Minutes between looks at Lidarr's wanted list, and how many albums to take each time.
    pub lidarr_interval_min: u64,
    pub lidarr_albums_per_run: i64,
    /// Smallest share of an album's track names that must match a folder's file names (0..1).
    pub lidarr_match_ratio: f64,
    /// Lyrics from LRCLIB for songs that have none (no .lrc, nothing in the tags).
    pub lyrics_online: bool,
    pub lrclib_url: String,
    /// Save a cover.jpg (from iTunes, then Deezer) into album folders that have no picture.
    pub covers_fetch: bool,
    pub itunes_url: String,
    /// AudioMuse-AI (optional): sound-alike songs, paths between sounds, Encore's recommendation calls.
    pub audiomuse_url: Option<String>,
    pub audiomuse_token: Option<String>,
    /// ListenBrainz: plays are sent there (as Navidrome did), and its weekly playlists become discovery playlists.
    pub listenbrainz_user: Option<String>,
    pub listenbrainz_token: Option<String>,
    /// Whose plays go to ListenBrainz and who owns the discovery playlists (default: the first admin).
    pub listenbrainz_for: Option<String>,
    pub listenbrainz_url: String,
    /// Which ListenBrainz playlists to turn into discovery playlists ([] = none).
    pub discovery: Vec<String>,
    /// Spotify sync (spotify.py's "songs" mode): your Liked Songs and chosen playlists become playlists here; songs
    /// you don't have are asked for. The login and choices come from `import-music-lookup` (spotify/ folder).
    pub spotify_sync: bool,
    pub spotify_interval_hours: u64,
    pub spotify_api_url: String,
    pub spotify_accounts_url: String,
    /// Ollama (optional, local AI): lyrics translation and romanization. Off when not set.
    pub ollama_url: Option<String>,
    pub translate_model: String,
    /// The song-id service (optional): "What's playing?" against your own library. POST /identify, GET /status.
    pub songid_url: Option<String>,
    /// Encore's DJ: lines written by the Ollama above (`dj_model`, else `translate_model`) and read by Piper (the
    /// dj-voice service: POST /speak). Off unless both are set.
    pub dj_voice_url: Option<String>,
    pub dj_model: Option<String>,
    /// Automix: the mix analyzer's folder (info.jsonl in, wanted.txt out; paths relative to the music folder).
    pub mixinfo_dir: Option<PathBuf>,
    /// Concerts near you: Ticketmaster's Discovery API (with the key set in Encore), else MusicBrainz events.
    pub ticketmaster_url: String,
    pub musicbrainz_url: String,
    pub concerts: bool,
    /// Artist biographies and album notes (MusicBrainz → Wikidata → Wikipedia, pictures from the Cover Art
    /// Archive): looked up when an app first asks, then kept. `info_lang` is Wikipedia's language code.
    pub info_online: bool,
    /// Imports from Last.fm (scrobbles and loved songs by username) need an API key (last.fm/api/account/create).
    pub lastfm_url: String,
    pub lastfm_api_key: Option<String>,
    pub lastfm_secret: Option<String>,
    /// Look for a better copy of songs it downloaded (the album asked from, FLAC over lossy) when there's nothing
    /// else to do, and swap it in (same song id).
    pub upgrades: bool,
    pub info_lang: String,
    pub wikidata_url: String,
    /// "{lang}" is replaced with the language
    pub wikipedia_url: String,
    pub coverart_url: String,
    /// Downloads through a VPN (Prelude sets these): "none", "existing" (slskd already runs behind one), "gluetun"
    /// (slskd inside a gluetun container Prelude set up) or "proxy" (Conductor's own download traffic through `proxy`).
    pub vpn: String,
    /// Asked for the public address the downloads use: JSON with "public_ip" or "ip" (and "country"), e.g. gluetun's
    /// control server at /v1/publicip/ip. The address itself is never logged or shown.
    pub vpn_check_url: Option<String>,
    /// Sent as X-API-Key to vpn_check_url (gluetun's control server). A secret: keep it in secrets.toml.
    pub vpn_check_key: Option<String>,
    /// Where this computer's own public address is asked for, to compare (any JSON answer with "ip").
    pub vpn_ip_url: String,
    /// The kill switch: while the VPN check fails, downloads wait (never over the open connection).
    pub downloads_require_vpn: bool,
    /// Seconds between VPN checks.
    pub vpn_check_secs: u64,
    /// Keep slskd's Soulseek listen port on the VPN's forwarded port (from gluetun's control server).
    pub vpn_port_sync: bool,
    /// A SOCKS5 or HTTP proxy for Conductor's own download traffic (the ⬇ catalog and its previews), e.g.
    /// socks5h://user:pass@127.0.0.1:1080. A secret when it has a password: keep it in secrets.toml.
    pub proxy: Option<String>,
    /// Also send cover, lyrics and artist-info fetches through the proxy.
    pub proxy_metadata: bool,
    /// Where this was read from (None: defaults and the environment only).
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
    #[serde(skip)]
    pub slskd_fast_test: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryConfig {
    pub name: String,
    pub path: PathBuf,
    /// A file that must exist in `path` for the library to count as connected (e.g. `.library-id`).
    /// Without one, the folder counts as connected when it holds anything at all.
    #[serde(default)]
    pub sentinel: Option<String>,
    /// Files read at the same time while scanning (default 2; a spinning USB drive is slower with more).
    #[serde(default)]
    pub scan_threads: Option<usize>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            listen: "127.0.0.1:4533".parse().unwrap(),
            read_only: false,
            libraries: vec![],
            scan_interval_min: 15,
            ffmpeg: None,
            transcode_slots: 0,
            transcode_cache_mb: 1000,
            loudness_analysis: true,
            loudness_threads: 1,
            jam_listen: "127.0.0.1:4535".into(),
            jam_public_url: None,
            podcast_dir: None,
            podcast_keep: 0,
            podcast_clean_days: 7,
            podcast_local: false,
            backup_keep: 7,
            backup_dir: None,
            catalog: true,
            catalog_skip_clients: vec!["symfonium".into(), "audiomuse".into()],
            deezer_url: "https://api.deezer.com".into(),
            slskd_url: None,
            slskd_api_key: None,
            slskd_downloads: None,
            import_library: None,
            trial_hours: 24.0,
            trial_per_day: 40,
            lidarr_url: None,
            lidarr_api_key: None,
            lidarr_downloads: None,
            lidarr_interval_min: 5,
            lidarr_albums_per_run: 20,
            lidarr_match_ratio: 0.8,
            lyrics_online: true,
            lrclib_url: "https://lrclib.net".into(),
            covers_fetch: true,
            itunes_url: "https://itunes.apple.com".into(),
            audiomuse_url: None,
            audiomuse_token: None,
            listenbrainz_user: None,
            listenbrainz_token: None,
            listenbrainz_for: None,
            listenbrainz_url: "https://api.listenbrainz.org".into(),
            spotify_sync: true,
            spotify_interval_hours: 6,
            spotify_api_url: "https://api.spotify.com/v1".into(),
            spotify_accounts_url: "https://accounts.spotify.com".into(),
            discovery: vec!["weekly-exploration".into(), "weekly-jams".into(), "daily-jams".into()],
            ollama_url: None,
            translate_model: "gemma4:e2b-it-qat".into(),
            songid_url: None,
            dj_voice_url: None,
            dj_model: None,
            mixinfo_dir: None,
            ticketmaster_url: "https://app.ticketmaster.com/discovery/v2".into(),
            musicbrainz_url: "https://musicbrainz.org/ws/2".into(),
            concerts: true,
            info_online: true,
            lastfm_url: "https://ws.audioscrobbler.com/2.0".into(),
            lastfm_api_key: None,
            lastfm_secret: None,
            upgrades: true,
            info_lang: "en".into(),
            wikidata_url: "https://www.wikidata.org".into(),
            wikipedia_url: "https://{lang}.wikipedia.org".into(),
            coverart_url: "https://coverartarchive.org".into(),
            vpn: "none".into(),
            vpn_check_url: None,
            vpn_check_key: None,
            vpn_ip_url: "https://ipinfo.io/json".into(),
            downloads_require_vpn: false,
            vpn_check_secs: 180,
            vpn_port_sync: false,
            proxy: None,
            proxy_metadata: false,
            config_path: None,
            slskd_fast_test: false,
        }
    }
}

/// The folder data goes in by default, named `name` ("conductor"; Conductor was called Maestro before).
fn data_dir_named(name: &str) -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\ProgramData")).join(name)
    } else if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(x).join(name)
    } else if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h).join(".local/share").join(name)
    } else {
        PathBuf::from("./data")
    }
}

pub fn default_data_dir() -> PathBuf {
    data_dir_named("conductor")
}

/// Settings that are secrets: kept in secrets.toml (mode 600) beside config.toml, not in config.toml itself.
pub const SECRET_KEYS: &[&str] =
    &["slskd_api_key", "lidarr_api_key", "listenbrainz_token", "lastfm_api_key", "lastfm_secret", "audiomuse_token", "vpn_check_key", "proxy"];

/// The config file used when none is given: config.toml in the data folder (CONDUCTOR_DATA_DIR, else the default).
pub fn default_config_path() -> PathBuf {
    var_os("DATA_DIR").map(PathBuf::from).unwrap_or_else(default_data_dir).join("config.toml")
}

/// Write a file only its owner can read (secrets.toml, secret.key, the VPN's keys). It's made that way from the
/// start, under a temporary name beside it, then renamed over the old one: never readable by others, not even for
/// a moment, and never half written. On Windows (no modes) its ACL is set instead, as well as it can be: this
/// account, SYSTEM and administrators only.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let res = (|| {
        let mut f = o.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?; // a leftover temporary file had others' modes
        }
        #[cfg(windows)]
        restrict(&tmp); // before anything is written into it
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Windows: the file's inherited access (all local users can read C:\ProgramData) is replaced by this account,
/// SYSTEM and administrators. Best effort: without whoami or icacls the folder's access stays (install.ps1 makes
/// Conductor's data folder private too).
#[cfg(windows)]
fn restrict(path: &std::path::Path) {
    use std::process::Command;
    let me = Command::new("whoami").args(["/user", "/fo", "csv", "/nh"]).output().ok().and_then(|o| current_sid(&String::from_utf8_lossy(&o.stdout)));
    if let Some(sid) = me {
        let _ = Command::new("icacls").arg(path).args(icacls_private(&sid)).output();
    }
}

/// The SID in `whoami /user /fo csv /nh`'s answer ("host\\me","S-1-5-21-…").
#[cfg(any(windows, test))]
fn current_sid(csv: &str) -> Option<String> {
    let sid = csv.lines().next()?.rsplit(',').next()?.trim().trim_matches('"');
    sid.starts_with("S-1-").then(|| sid.to_string())
}

/// icacls' arguments: nothing inherited; full access for `sid`, SYSTEM and administrators (well-known SIDs, so any
/// Windows language works).
#[cfg(any(windows, test))]
fn icacls_private(sid: &str) -> Vec<String> {
    ["/inheritance:r", "/grant:r", "*S-1-5-18:F", "*S-1-5-32-544:F"].iter().map(|s| s.to_string()).chain([format!("*{sid}:F")]).collect()
}

/// A command run as root (`sudo conductor restore …`, `sudo conductor user add …`) on a data folder that belongs to
/// someone else, such as the Linux package's /var/lib/conductor (the conductor user's): what it made there is given
/// to the folder's owner. Left root's, the service running as that user couldn't write its own database (or even
/// open it, with root's -shm file beside it) and didn't start. Not root, or root's own folder: nothing to do.
pub fn give_to_data_owner(cfg: &Config) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and can't fail
        if unsafe { libc::geteuid() } != 0 {
            return;
        }
        let Ok(dir) = std::fs::metadata(&cfg.data_dir) else { return };
        if dir.uid() == 0 {
            return;
        }
        let mut paths: Vec<PathBuf> = ["conductor.db", "conductor.db-wal", "conductor.db-shm", "secret.key", "playlist-images"]
            .iter()
            .map(|n| cfg.data_dir.join(n))
            .collect();
        if let Ok(rd) = std::fs::read_dir(cfg.data_dir.join("playlist-images")) {
            paths.extend(rd.flatten().map(|e| e.path()));
        }
        // the settings too, when they're in a folder of the same owner (config.toml in the data folder)
        if let Some(p) = &cfg.config_path
            && p.parent().and_then(|d| std::fs::metadata(d).ok()).is_some_and(|m| m.uid() == dir.uid())
        {
            paths.extend([p.clone(), secrets_path(p)]);
        }
        for p in paths {
            // only what root made: anything else keeps its owner
            if std::fs::symlink_metadata(&p).is_ok_and(|m| m.uid() == 0 && !m.file_type().is_symlink())
                && let Err(e) = std::os::unix::fs::chown(&p, Some(dir.uid()), Some(dir.gid()))
            {
                tracing::warn!("couldn't give {} to the data folder's owner: {e}", p.display());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = cfg;
}

/// secrets.toml beside a config file.
pub fn secrets_path(config: &std::path::Path) -> PathBuf {
    config.with_file_name("secrets.toml")
}

/// Settings from the environment: CONDUCTOR_<name>, or the old MAESTRO_<name> (still read; noted in the log).
pub fn var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("CONDUCTOR_{name}")).or_else(|e| match std::env::var(format!("{OLD}_{name}")) {
        Ok(v) => {
            old_name(name);
            Ok(v)
        }
        Err(_) => Err(e),
    })
}

pub fn var_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(format!("CONDUCTOR_{name}")).or_else(|| {
        let v = std::env::var_os(format!("{OLD}_{name}"));
        if v.is_some() {
            old_name(name);
        }
        v
    })
}

/// Conductor's name before.
const OLD: &str = "MAESTRO";
static OLD_NAMES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn old_name(name: &str) {
    let mut v = OLD_NAMES.lock().unwrap_or_else(|e| e.into_inner());
    if !v.iter().any(|n| n == name) {
        v.push(name.to_string());
    }
}

/// The default data folder under the old name moves to the new one (when the new one isn't there yet and no data
/// folder is set in the environment). Done before settings are looked for, since config.toml is in it.
pub fn move_old_data_dir() {
    if var_os("DATA_DIR").is_some() {
        return;
    }
    let (old_dir, new_dir) = (data_dir_named(&OLD.to_lowercase()), default_data_dir());
    if !new_dir.exists() && old_dir.is_dir() {
        match std::fs::rename(&old_dir, &new_dir) {
            Ok(()) => tracing::info!(from = %old_dir.display(), to = %new_dir.display(), "data folder moved to the new name"),
            Err(e) => tracing::warn!(from = %old_dir.display(), "couldn't move the old data folder ({e}); starting a new one"),
        }
    }
}

/// Moves what an older version left under the old name: the default data folder (when the new one isn't there
/// yet) and the database file in the data folder. Runs before the database is opened.
pub fn move_old_names(cfg: &Config) {
    if cfg.data_dir == default_data_dir() {
        move_old_data_dir();
    }
    let old_db = cfg.data_dir.join(format!("{}.db", OLD.to_lowercase()));
    let new_db = cfg.data_dir.join("conductor.db");
    if old_db.is_file() && !new_db.exists() {
        // the database's journal first and the database itself last: while maestro.db is there, the move isn't
        // finished and is taken up again next time. (Moved the other way round, a stop in between left
        // conductor.db without its -wal, and what was in the -wal was lost.)
        for ext in ["-wal", "-shm", ""] {
            let (a, b) = (PathBuf::from(format!("{}{ext}", old_db.display())), PathBuf::from(format!("{}{ext}", new_db.display())));
            if a.exists() && let Err(e) = std::fs::rename(&a, &b) {
                tracing::warn!("couldn't rename {} ({e}); the database keeps its old name for now", a.display());
                return;
            }
        }
        tracing::info!("database renamed to conductor.db");
    }
}

/// Environment names read one by one in `Config::load` (their own meaning or parsing): the generic reading below
/// leaves them alone. (A test checks every `var("…")` here is listed.)
pub const ENV_EXPLICIT: &[&str] = &[
    "AUDIOMUSE_TOKEN", "AUDIOMUSE_URL", "BACKUP_DIR", "BACKUP_KEEP", "CATALOG", "CONCERTS", "CONFIG", "COVERS", "DATA_DIR",
    "DJ_MODEL", "DJ_VOICE_URL", "FFMPEG", "INFO_LANG", "INFO_ONLINE", "JAM_LISTEN", "JAM_PUBLIC_URL", "LASTFM_API_KEY",
    "LIDARR_API_KEY", "LIDARR_DOWNLOADS", "LIDARR_URL", "LISTEN", "LISTENBRAINZ_TOKEN", "LISTENBRAINZ_USER", "LYRICS_ONLINE",
    "MIXINFO_DIR", "MUSIC", "OLLAMA_URL", "PODCAST_CLEAN_DAYS", "PODCAST_DIR", "PODCAST_KEEP", "PODCAST_LOCAL", "READ_ONLY",
    "SCAN_INTERVAL_MIN", "SCAN_THREADS", "SENTINEL", "SLSKD_API_KEY", "SLSKD_DOWNLOADS", "SLSKD_URL", "SONGID_URL",
    "TRANSLATE_MODEL", "UPGRADES", "VPN", "VPN_IP_URL",
    // not settings: read elsewhere
    "ADMIN_USER", "ADMIN_PASSWORD", "PASSWORD", "IMPORT_PASSWORD", "ND_PASSWORD_KEY",
];

/// Every other `CONDUCTOR_<NAME>` (or the old `MAESTRO_<NAME>`) as the setting `<name>`: numbers and true/false as
/// such, and a value the setting can't take that way as text, or for a list, comma-separated text. Names that aren't
/// settings are ignored.
fn env_settings() -> Result<Vec<(String, toml::Value)>> {
    let fits = |k: &str, v: &toml::Value| {
        let mut one = toml::Table::new();
        one.insert(k.to_string(), v.clone());
        toml::Value::Table(one).try_into::<Config>().is_ok()
    };
    let mut out: Vec<(String, toml::Value)> = vec![];
    // the old names first, so the new ones win
    for prefix in [format!("{OLD}_"), "CONDUCTOR_".to_string()] {
        for (name, raw) in std::env::vars() {
            let Some(rest) = name.strip_prefix(&prefix) else { continue };
            if raw.trim().is_empty() || ENV_EXPLICIT.contains(&rest) {
                continue;
            }
            if prefix != "CONDUCTOR_" {
                old_name(rest);
            }
            let key = rest.to_lowercase();
            let parsed = toml::from_str::<toml::Table>(&format!("v = {raw}"))
                .ok()
                .and_then(|mut t| t.remove("v"))
                .filter(|v| !matches!(v, toml::Value::Datetime(_)));
            let text = toml::Value::String(raw.clone());
            let list = toml::Value::Array(raw.split(',').map(|x| toml::Value::String(x.trim().to_string())).filter(|x| x.as_str() != Some("")).collect());
            let Some(v) = [parsed, Some(text), Some(list)].into_iter().flatten().find(|v| fits(&key, v)) else {
                bail!("{name}: not a value the setting {key} can take");
            };
            out.retain(|(k, _)| *k != key);
            out.push((key, v));
        }
    }
    Ok(out)
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.or_else(|| var_os("CONFIG").map(PathBuf::from)).or_else(|| {
            // an older install's settings are in the old default folder: moved first, so they're found
            move_old_data_dir();
            Some(default_config_path()).filter(|p| p.is_file())
        });
        let mut t: toml::Table = match &path {
            Some(p) => {
                let mut t: toml::Table = toml::from_str(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
                    .with_context(|| format!("parsing {}", p.display()))?;
                // the secrets beside it (only the keys that are secrets)
                let sp = secrets_path(p);
                if let Ok(text) = std::fs::read_to_string(&sp) {
                    let secrets: toml::Table = toml::from_str(&text).with_context(|| format!("parsing {}", sp.display()))?;
                    for (k, v) in secrets {
                        if SECRET_KEYS.contains(&k.as_str()) {
                            t.insert(k, v);
                        }
                    }
                }
                t
            }
            None => toml::Table::new(),
        };
        // any setting from the environment: CONDUCTOR_<NAME> (wins over the file)
        for (k, v) in env_settings()? {
            t.insert(k, v);
        }
        let mut catalog_set = t.contains_key("catalog");
        let mut c: Config = toml::Value::Table(t).try_into().with_context(|| match &path {
            Some(p) => format!("parsing {}", p.display()),
            None => "reading the settings".into(),
        })?;
        c.config_path = path.clone();
        // empty: as if unset (Prelude's Docker file clears the image's defaults this way, to use config.toml)
        if let Some(v) = var_os("DATA_DIR").filter(|v| !v.is_empty()) {
            c.data_dir = v.into();
        }
        if let Ok(v) = var("LISTEN").map(|v| v.trim().to_string()).and_then(|v| if v.is_empty() { Err(std::env::VarError::NotPresent) } else { Ok(v) }) {
            c.listen = v.parse().context("CONDUCTOR_LISTEN")?;
        }
        if let Some(v) = var_os("MUSIC").filter(|v| !v.is_empty()) {
            // one library from the environment, handy for Docker: CONDUCTOR_MUSIC=/music
            c.libraries = vec![LibraryConfig {
                name: "Music".into(),
                path: v.into(),
                sentinel: var("SENTINEL").ok(),
                scan_threads: var("SCAN_THREADS").ok().and_then(|v| v.parse().ok()),
            }];
        }
        if let Ok(v) = var("SCAN_INTERVAL_MIN") {
            c.scan_interval_min = v.parse().context("CONDUCTOR_SCAN_INTERVAL_MIN")?;
        }
        if let Ok(v) = var("JAM_LISTEN") {
            c.jam_listen = v;
        }
        if let Ok(v) = var("JAM_PUBLIC_URL") {
            c.jam_public_url = Some(v);
        }
        if let Some(v) = var_os("PODCAST_DIR") {
            c.podcast_dir = Some(v.into());
        }
        if let Ok(v) = var("PODCAST_KEEP") {
            c.podcast_keep = v.parse().context("CONDUCTOR_PODCAST_KEEP")?;
        }
        if let Ok(v) = var("PODCAST_CLEAN_DAYS") {
            c.podcast_clean_days = v.parse().context("CONDUCTOR_PODCAST_CLEAN_DAYS")?;
        }
        if let Ok(v) = var("BACKUP_KEEP") {
            c.backup_keep = v.parse().context("CONDUCTOR_BACKUP_KEEP")?;
        }
        if let Some(v) = var_os("BACKUP_DIR") {
            c.backup_dir = Some(v.into());
        }
        if let Ok(v) = var("PODCAST_LOCAL") {
            c.podcast_local = v == "1" || v == "true";
        }
        if let Ok(v) = var("CATALOG") {
            catalog_set = true;
            c.catalog = v != "0" && v != "false";
        }
        if let Ok(v) = var("SLSKD_URL") {
            c.slskd_url = Some(v);
        }
        if let Ok(v) = var("SLSKD_API_KEY") {
            c.slskd_api_key = Some(v);
        }
        if let Some(v) = var_os("SLSKD_DOWNLOADS") {
            c.slskd_downloads = Some(v.into());
        }
        if let Ok(v) = var("LIDARR_URL") {
            c.lidarr_url = Some(v);
        }
        if let Ok(v) = var("LIDARR_API_KEY") {
            c.lidarr_api_key = Some(v);
        }
        if let Ok(v) = var("LIDARR_DOWNLOADS") {
            c.lidarr_downloads = Some(v);
        }
        if let Ok(v) = var("AUDIOMUSE_URL") {
            c.audiomuse_url = Some(v);
        }
        if let Ok(v) = var("AUDIOMUSE_TOKEN") {
            c.audiomuse_token = Some(v);
        }
        if let Ok(v) = var("OLLAMA_URL") {
            c.ollama_url = Some(v).filter(|v| !v.is_empty());
        }
        if let Ok(v) = var("TRANSLATE_MODEL") {
            c.translate_model = v;
        }
        if let Ok(v) = var("SONGID_URL") {
            c.songid_url = Some(v).filter(|v| !v.is_empty());
        }
        if let Ok(v) = var("MIXINFO_DIR") {
            c.mixinfo_dir = Some(PathBuf::from(v)).filter(|v| !v.as_os_str().is_empty());
        }
        if let Ok(v) = var("CONCERTS") {
            c.concerts = v != "0" && v != "false";
        }
        if let Ok(v) = var("INFO_ONLINE") {
            c.info_online = v != "0" && v != "false";
        }
        if let Ok(v) = var("DJ_VOICE_URL") {
            c.dj_voice_url = Some(v).filter(|v| !v.is_empty());
        }
        if let Ok(v) = var("DJ_MODEL") {
            c.dj_model = Some(v).filter(|v| !v.is_empty());
        }
        if let Ok(v) = var("UPGRADES") {
            c.upgrades = v != "0" && v != "false";
        }
        if let Ok(v) = var("VPN") {
            c.vpn = v;
        }
        for (name, slot) in [("VPN_CHECK_URL", &mut c.vpn_check_url), ("VPN_CHECK_KEY", &mut c.vpn_check_key), ("PROXY", &mut c.proxy)] {
            if let Ok(v) = var(name) {
                *slot = Some(v).filter(|v| !v.is_empty());
            }
        }
        if let Ok(v) = var("VPN_IP_URL") {
            c.vpn_ip_url = v;
        }
        for (name, slot) in
            [("DOWNLOADS_REQUIRE_VPN", &mut c.downloads_require_vpn), ("VPN_PORT_SYNC", &mut c.vpn_port_sync), ("PROXY_METADATA", &mut c.proxy_metadata)]
        {
            if let Ok(v) = var(name) {
                *slot = v != "0" && v != "false";
            }
        }
        if let Ok(v) = var("LASTFM_API_KEY") {
            c.lastfm_api_key = Some(v).filter(|v| !v.is_empty());
        }
        if let Ok(v) = var("INFO_LANG") {
            c.info_lang = v;
        }
        if let Ok(v) = var("READ_ONLY") {
            c.read_only = v == "1" || v == "true";
        }
        if let Ok(v) = var("COVERS") {
            c.covers_fetch = v != "0" && v != "false";
        }
        if let Ok(v) = var("LYRICS_ONLINE") {
            c.lyrics_online = v != "0" && v != "false";
        }
        if let Ok(v) = var("LISTENBRAINZ_USER") {
            c.listenbrainz_user = Some(v);
        }
        if let Ok(v) = var("LISTENBRAINZ_TOKEN") {
            c.listenbrainz_token = Some(v);
        }
        if let Some(v) = var_os("FFMPEG") {
            c.ffmpeg = Some(v.into());
        }
        let old = OLD_NAMES.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !old.is_empty() {
            tracing::warn!(
                "settings named {OLD}_* still work but are now CONDUCTOR_*: rename {}",
                old.iter().map(|n| format!("{OLD}_{n}")).collect::<Vec<_>>().join(", ")
            );
        }
        if !catalog_set && c.slskd_url.is_none() && c.lidarr_url.is_none() {
            c.catalog = false;
        }
        // checked here and not only when the Jam listener starts: an address that isn't one stopped the whole server
        // from starting, and Podium's Settings saved one as fine (it takes what this takes)
        if !c.jam_listen.is_empty() && c.jam_listen.parse::<std::net::SocketAddr>().is_err() {
            bail!("jam_listen: {:?} isn't an address with a port, like 127.0.0.1:4535 (or empty: no Jam listener)", c.jam_listen);
        }
        Ok(c)
    }

    /// How many conversions may work at once: the setting, or half the cores (at least 2).
    pub fn transcode_slots(&self) -> usize {
        if self.transcode_slots > 0 {
            // (a huge number is taken as plenty: past this the slots couldn't be made and the server didn't start)
            return self.transcode_slots.min(1024);
        }
        (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2) / 2).max(2)
    }

    pub fn ffmpeg(&self) -> PathBuf {
        self.ffmpeg.clone().unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }))
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }

    /// Playlists' own pictures. A playlist keeps only its picture's file name, so the pictures stay found when the
    /// data folder moves (the rename from Maestro, a backup restored elsewhere).
    pub fn playlist_images_dir(&self) -> PathBuf {
        self.data_dir.join("playlist-images")
    }

    /// The picture a playlist keeps (`image_path`), in this data folder. Older versions kept the whole path: the
    /// file of that name here, else that path as it was.
    pub fn playlist_image(&self, stored: &str) -> PathBuf {
        let here = self.own_playlist_image(stored);
        if here.as_ref().is_some_and(|p| p.is_file()) || !Path::new(stored).is_absolute() {
            return here.unwrap_or_else(|| self.playlist_images_dir());
        }
        PathBuf::from(stored)
    }

    /// The file of that name in this data folder's pictures (None for no name), the one to remove when the picture
    /// is changed: never a file outside the data folder.
    pub fn own_playlist_image(&self, stored: &str) -> Option<PathBuf> {
        let name = stored.rsplit(['/', '\\']).next().filter(|n| !n.is_empty() && *n != "." && *n != "..")?;
        Some(self.playlist_images_dir().join(name))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_huge_number_of_conversion_slots_still_starts() {
        // Semaphore::new panics past ~2^61 permits: that was the server not starting
        let cfg = super::Config { transcode_slots: 1 << 62, ..Default::default() };
        let _ = crate::transcode::Slots::new(cfg.transcode_slots());
        assert_eq!(super::Config { transcode_slots: 3, ..Default::default() }.transcode_slots(), 3);
    }

    #[test]
    fn playlist_pictures_follow_the_data_folder() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = super::Config { data_dir: dir.path().join("conductor"), ..Default::default() };
        std::fs::create_dir_all(cfg.playlist_images_dir()).unwrap();
        std::fs::write(cfg.playlist_images_dir().join("p1_ab.jpg"), b"x").unwrap();
        // as stored now: a file name
        assert_eq!(cfg.playlist_image("p1_ab.jpg"), cfg.playlist_images_dir().join("p1_ab.jpg"));
        // as an older version stored it, under the folder's old name
        let old = dir.path().join("maestro").join("playlist-images").join("p1_ab.jpg");
        assert_eq!(cfg.playlist_image(&old.to_string_lossy()), cfg.playlist_images_dir().join("p1_ab.jpg"));
        // an old whole path to a file that isn't here: that path, as before
        let elsewhere = dir.path().join("elsewhere.jpg");
        assert_eq!(cfg.playlist_image(&elsewhere.to_string_lossy()), elsewhere);
        assert_eq!(cfg.own_playlist_image(&elsewhere.to_string_lossy()), Some(cfg.playlist_images_dir().join("elsewhere.jpg")));
        assert_eq!(cfg.own_playlist_image("/x/.."), None);
    }

    use super::*;

    #[test]
    fn private_files_on_windows() {
        let sid = current_sid("\"desktop-1\\me\",\"S-1-5-21-1-2-3-1001\"\r\n").unwrap();
        assert_eq!(sid, "S-1-5-21-1-2-3-1001");
        assert_eq!(current_sid("INFO: no user\r\n"), None);
        assert_eq!(icacls_private(&sid), ["/inheritance:r", "/grant:r", "*S-1-5-18:F", "*S-1-5-32-544:F", "*S-1-5-21-1-2-3-1001:F"]);
    }

    #[test]
    fn private_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("secrets.toml");
        std::fs::write(&p, b"old").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        write_private(&p, b"key = 1").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"key = 1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // a new file (on every system; its mode is checked on Unix)
        write_private(&tmp.path().join("new.key"), b"k").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(tmp.path().join("new.key")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 2, "no temporary file left");
    }

    // one test touches the environment, so they don't race
    #[test]
    fn old_names_still_work() {
        let tmp = std::env::temp_dir().join(format!("conductor-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: only this test reads or writes these variables
        unsafe {
            std::env::set_var(format!("{OLD}_SCAN_INTERVAL_MIN"), "7");
            std::env::set_var("CONDUCTOR_JAM_LISTEN", "127.0.0.1:1");
            std::env::set_var(format!("{OLD}_JAM_LISTEN"), "127.0.0.1:2");
            std::env::set_var("XDG_DATA_HOME", &tmp);
        }
        let c = Config::load(None).unwrap();
        assert_eq!(c.scan_interval_min, 7, "the old prefix is still read");
        assert_eq!(c.jam_listen, "127.0.0.1:1", "the new one wins");
        assert!(OLD_NAMES.lock().unwrap().contains(&"SCAN_INTERVAL_MIN".to_string()));
        // an older install's data folder and database get the new names
        let old = tmp.join("maestro");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("maestro.db"), b"db").unwrap();
        std::fs::write(old.join("maestro.db-wal"), b"wal").unwrap();
        if cfg!(not(windows)) {
            assert_eq!(c.data_dir, tmp.join("conductor"));
            move_old_names(&c);
            assert!(!old.exists());
            assert_eq!(std::fs::read(c.data_dir.join("conductor.db")).unwrap(), b"db");
            assert!(c.data_dir.join("conductor.db-wal").exists());
            // the -wal can't be moved: the database isn't either (conductor.db would be opened without its -wal,
            // and what's in it lost); next time it's taken up again
            let dir = c.data_dir.clone();
            for f in ["conductor.db", "conductor.db-wal"] {
                std::fs::remove_file(dir.join(f)).unwrap();
            }
            std::fs::write(dir.join("maestro.db"), b"db2").unwrap();
            std::fs::write(dir.join("maestro.db-wal"), b"wal2").unwrap();
            std::fs::create_dir_all(dir.join("conductor.db-wal/in-the-way")).unwrap();
            move_old_names(&c);
            assert!(!dir.join("conductor.db").exists(), "not moved without its -wal");
            assert_eq!(std::fs::read(dir.join("maestro.db-wal")).unwrap(), b"wal2");
            std::fs::remove_dir_all(dir.join("conductor.db-wal")).unwrap();
            move_old_names(&c);
            assert_eq!(std::fs::read(dir.join("conductor.db")).unwrap(), b"db2");
            assert_eq!(std::fs::read(dir.join("conductor.db-wal")).unwrap(), b"wal2");
            assert!(!dir.join("maestro.db").exists());
            // an older install that used the default folder: its settings are found on the first start
            std::fs::remove_dir_all(&dir).unwrap();
            std::fs::create_dir_all(&old).unwrap();
            std::fs::write(old.join("config.toml"), "listen = \"127.0.0.1:4599\"\n").unwrap();
            let c = Config::load(None).unwrap();
            assert_eq!(c.listen.port(), 4599, "the old config.toml was read");
            assert!(dir.join("config.toml").is_file() && !old.exists());
        }
        unsafe {
            std::env::remove_var(format!("{OLD}_SCAN_INTERVAL_MIN"));
            std::env::remove_var("CONDUCTOR_JAM_LISTEN");
            std::env::remove_var(format!("{OLD}_JAM_LISTEN"));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod docs_tests {
    /// The settings' fields, from this file.
    fn fields() -> Vec<String> {
        let s = include_str!("config.rs").replace("\r\n", "\n"); // a Windows checkout has CRLF
        let body = &s[s.find("pub struct Config {").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        body.lines()
            .filter_map(|l| l.trim().strip_prefix("pub ").and_then(|r| r.split_once(':')).map(|(n, _)| n.trim().to_string()))
            .filter(|n| n != "config_path" && n != "slskd_fast_test")
            .collect()
    }

    #[test]
    fn every_setting_is_in_the_reference() {
        let doc = include_str!("../docs/CONFIG.md");
        let missing: Vec<String> = fields().into_iter().filter(|f| !doc.contains(&format!("`{f}`"))).collect();
        assert!(missing.is_empty(), "add these to docs/CONFIG.md: {missing:?}");
    }

    #[test]
    fn every_environment_name_read_here_is_listed() {
        let s = include_str!("config.rs");
        let mut missing = vec![];
        for (i, _) in s.match_indices("var(\"").chain(s.match_indices("var_os(\"")) {
            if s[..i].ends_with("env::") || s[..i].ends_with('_') && !s[..i].ends_with("var_") {
                continue; // std::env::var: not a CONDUCTOR_ name
            }
            let rest = &s[i..];
            let name = &rest[rest.find('"').unwrap() + 1..];
            let name = &name[..name.find('"').unwrap()];
            if name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') && !super::ENV_EXPLICIT.contains(&name) {
                missing.push(name.to_string());
            }
        }
        assert!(missing.is_empty(), "add these to ENV_EXPLICIT: {missing:?}");
    }
}
