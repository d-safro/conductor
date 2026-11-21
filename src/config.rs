//! Settings: `config.toml` (path from `--config` or `MAESTRO_CONFIG`), then `MAESTRO_*` environment variables.

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
            libraries: vec![],
            scan_interval_min: 15,
            ffmpeg: None,
            jam_listen: "127.0.0.1:4535".into(),
            jam_public_url: None,
            podcast_dir: None,
            podcast_keep: 0,
            podcast_clean_days: 7,
        }
    }
}

fn default_data_dir() -> PathBuf {
    if cfg!(windows) {
        std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\ProgramData")).join("maestro")
    } else if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        PathBuf::from(x).join("maestro")
    } else if let Some(h) = std::env::var_os("HOME") {
        PathBuf::from(h).join(".local/share/maestro")
    } else {
        PathBuf::from("./data")
    }
}

impl Config {
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = path.or_else(|| std::env::var_os("MAESTRO_CONFIG").map(PathBuf::from));
        let mut c: Config = match &path {
            Some(p) => toml::from_str(&std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?)
                .with_context(|| format!("parsing {}", p.display()))?,
            None => Config::default(),
        };
        if let Some(v) = std::env::var_os("MAESTRO_DATA_DIR") {
            c.data_dir = v.into();
        }
        if let Ok(v) = std::env::var("MAESTRO_LISTEN") {
            c.listen = v.parse().context("MAESTRO_LISTEN")?;
        }
        if let Some(v) = std::env::var_os("MAESTRO_MUSIC") {
            // one library from the environment, handy for Docker: MAESTRO_MUSIC=/music
            c.libraries = vec![LibraryConfig {
                name: "Music".into(),
                path: v.into(),
                sentinel: std::env::var("MAESTRO_SENTINEL").ok(),
                scan_threads: std::env::var("MAESTRO_SCAN_THREADS").ok().and_then(|v| v.parse().ok()),
            }];
        }
        if let Ok(v) = std::env::var("MAESTRO_SCAN_INTERVAL_MIN") {
            c.scan_interval_min = v.parse().context("MAESTRO_SCAN_INTERVAL_MIN")?;
        }
        if let Ok(v) = std::env::var("MAESTRO_JAM_LISTEN") {
            c.jam_listen = v;
        }
        if let Ok(v) = std::env::var("MAESTRO_JAM_PUBLIC_URL") {
            c.jam_public_url = Some(v);
        }
        if let Some(v) = std::env::var_os("MAESTRO_PODCAST_DIR") {
            c.podcast_dir = Some(v.into());
        }
        if let Ok(v) = std::env::var("MAESTRO_PODCAST_KEEP") {
            c.podcast_keep = v.parse().context("MAESTRO_PODCAST_KEEP")?;
        }
        if let Ok(v) = std::env::var("MAESTRO_PODCAST_CLEAN_DAYS") {
            c.podcast_clean_days = v.parse().context("MAESTRO_PODCAST_CLEAN_DAYS")?;
        }
        if let Some(v) = std::env::var_os("MAESTRO_FFMPEG") {
            c.ffmpeg = Some(v.into());
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
