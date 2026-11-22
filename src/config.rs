//! Settings: `config.toml` (path from `--config` or `CONDUCTOR_CONFIG`), then `CONDUCTOR_*` environment variables.

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
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
    /// Songs you don't have (⬇) in searches, albums and artists (from Deezer).
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
    /// Look for a better copy of songs it downloaded (the album asked from, FLAC over lossy) when there's nothing
    /// else to do, and swap it in (same song id).
    pub upgrades: bool,
    pub info_lang: String,
    pub wikidata_url: String,
    /// "{lang}" is replaced with the language
    pub wikipedia_url: String,
    pub coverart_url: String,
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
            jam_listen: "127.0.0.1:4535".into(),
            jam_public_url: None,
            podcast_dir: None,
            podcast_keep: 0,
            podcast_clean_days: 7,
            catalog: true,
            catalog_skip_clients: vec!["symfonium".into()],
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
            upgrades: true,
            info_lang: "en".into(),
            wikidata_url: "https://www.wikidata.org".into(),
            wikipedia_url: "https://{lang}.wikipedia.org".into(),
            coverart_url: "https://coverartarchive.org".into(),
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

fn default_data_dir() -> PathBuf {
    data_dir_named("conductor")
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

/// Moves what an older version left under the old name: the default data folder (when the new one isn't there
/// yet) and the database file in the data folder. Runs before the database is opened.
pub fn move_old_names(cfg: &Config) {
    let old_dir = data_dir_named(&OLD.to_lowercase());
    if cfg.data_dir == default_data_dir() && !cfg.data_dir.exists() && old_dir.is_dir() {
        match std::fs::rename(&old_dir, &cfg.data_dir) {
            Ok(()) => tracing::info!(from = %old_dir.display(), to = %cfg.data_dir.display(), "data folder moved to the new name"),
            Err(e) => tracing::warn!(from = %old_dir.display(), "couldn't move the old data folder ({e}); starting a new one"),
        }
    }
    let old_db = cfg.data_dir.join(format!("{}.db", OLD.to_lowercase()));
    let new_db = cfg.data_dir.join("conductor.db");
    if old_db.is_file() && !new_db.exists() {
        for ext in ["", "-wal", "-shm"] {
            let (a, b) = (PathBuf::from(format!("{}{ext}", old_db.display())), PathBuf::from(format!("{}{ext}", new_db.display())));
            if a.exists() && let Err(e) = std::fs::rename(&a, &b) {
                tracing::warn!("couldn't rename {} ({e})", a.display());
            }
        }
        tracing::info!("database renamed to conductor.db");
    }
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.or_else(|| var_os("CONFIG").map(PathBuf::from));
        let mut c: Config = match &path {
            Some(p) => toml::from_str(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
                .with_context(|| format!("parsing {}", p.display()))?,
            None => Config::default(),
        };
        if let Some(v) = var_os("DATA_DIR") {
            c.data_dir = v.into();
        }
        if let Ok(v) = var("LISTEN") {
            c.listen = v.parse().context("CONDUCTOR_LISTEN")?;
        }
        if let Some(v) = var_os("MUSIC") {
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
        if let Ok(v) = var("CATALOG") {
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
        Ok(c)
    }

    pub fn ffmpeg(&self) -> PathBuf {
        self.ffmpeg.clone().unwrap_or_else(|| PathBuf::from(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }))
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.data_dir.join("cache")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
        unsafe {
            std::env::remove_var(format!("{OLD}_SCAN_INTERVAL_MIN"));
            std::env::remove_var("CONDUCTOR_JAM_LISTEN");
            std::env::remove_var(format!("{OLD}_JAM_LISTEN"));
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
