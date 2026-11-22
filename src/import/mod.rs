//! Bring your music in from elsewhere: Spotify, Apple Music / iTunes, YouTube Music, Last.fm, ListenBrainz, Deezer,
//! plain M3U / CSV playlists. Each source reads what that service gives you (its data export, or its API) into one
//! shape (`Parsed`): liked songs, saved albums, followed artists, playlists, plays with their times, ratings, play
//! counts and podcasts. Then the songs are matched to your library (`matching`) and applied (`apply`): to Conductor
//! directly, as a batch that can be undone, or to any Subsonic server through its API (e.g. Navidrome before a
//! switch).
//!
//! Privacy: the exports also hold IP addresses, countries, devices and searches. Parsers read only the fields listed
//! in this module's types; nothing else is kept or logged, and an uploaded file is deleted once it's been imported.
//!
//! Adding a source: a function `fn(&Files, &mut Parsed) -> Result<bool>` in `FILE_SOURCES` (it says whether the
//! files were its own), or an online one in `online`.

pub mod apple;
pub mod apply;
pub mod cli;
pub mod csv;
pub mod google;
pub mod matching;
pub mod misc;
pub mod online;
pub mod jobs;
pub mod spotify;
pub mod subsonic;

use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// A song as another service describes it. Any of the ids may be there; the names usually are.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Song {
    pub title: String,
    pub artist: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub album: String,
    #[serde(skip_serializing_if = "is_zero")]
    pub duration_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub isrc: Option<String>,
    /// Spotify track id (the part after "spotify:track:")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spotify: Option<String>,
    /// MusicBrainz recording id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deezer: Option<i64>,
    /// YouTube video id
    #[serde(skip_serializing_if = "Option::is_none")]
    pub youtube: Option<String>,
    /// a file path (M3U playlists): matched to the library's files first
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

impl Song {
    pub fn named(title: &str, artist: &str, album: &str) -> Self {
        Song { title: title.trim().into(), artist: artist.trim().into(), album: album.trim().into(), ..Default::default() }
    }
    /// One key per song, for counting and matching each song once.
    pub fn key(&self) -> String {
        if let Some(s) = &self.spotify {
            return format!("sp:{s}");
        }
        if let Some(i) = &self.isrc {
            return format!("isrc:{}", i.to_uppercase());
        }
        if let Some(m) = &self.mbid {
            return format!("mb:{m}");
        }
        if let Some(d) = self.deezer {
            return format!("dz:{d}");
        }
        if self.title.is_empty()
            && let Some(y) = &self.youtube
        {
            return format!("yt:{y}");
        }
        format!("n:{}|{}", crate::catalog::text::tk(&self.title), crate::catalog::text::ak(&self.artist))
    }
    /// Something to match on at all.
    pub fn usable(&self) -> bool {
        !self.title.trim().is_empty() || self.isrc.is_some() || self.mbid.is_some() || self.spotify.is_some() || self.path.is_some()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Play {
    pub song: Song,
    /// when it started, seconds
    pub at: i64,
    /// how long it played, when the service says
    pub ms_played: Option<i64>,
}

/// Under this, a play counts as a skip.
pub const SKIP_MS: i64 = 30_000;

impl Play {
    pub fn is_skip(&self) -> bool {
        self.ms_played.is_some_and(|ms| ms < SKIP_MS)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Playlist {
    pub name: String,
    pub description: String,
    pub songs: Vec<Song>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Album {
    pub title: String,
    pub artist: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Podcast {
    pub title: String,
    pub publisher: String,
    /// the RSS feed, when the export has it (else looked up)
    pub feed: Option<String>,
}

/// Everything found in an export, in one shape.
#[derive(Debug, Default, Serialize)]
pub struct Parsed {
    /// what was recognised, e.g. "Spotify extended streaming history"
    pub sources: BTreeSet<String>,
    /// liked / loved / favourite songs, with when (seconds) when known
    pub liked: Vec<(Song, Option<i64>)>,
    pub albums: Vec<Album>,
    pub artists: Vec<String>,
    pub playlists: Vec<Playlist>,
    pub plays: Vec<Play>,
    /// 1-5 stars
    pub ratings: Vec<(Song, u8)>,
    /// play counts without the single plays (iTunes / Apple Music library): (song, count, last played)
    pub counts: Vec<(Song, i64, Option<i64>)>,
    pub podcasts: Vec<Podcast>,
    /// things worth telling (e.g. "watch-history.html: export the history as JSON")
    pub notes: Vec<String>,
}

impl Parsed {
    pub fn is_empty(&self) -> bool {
        self.liked.is_empty()
            && self.albums.is_empty()
            && self.artists.is_empty()
            && self.playlists.is_empty()
            && self.plays.is_empty()
            && self.ratings.is_empty()
            && self.counts.is_empty()
            && self.podcasts.is_empty()
    }
    /// Drop repeats (the same liked song twice, the same album, artist or podcast).
    pub fn tidy(&mut self) {
        let mut seen = std::collections::HashSet::new();
        self.liked.retain(|(s, _)| s.usable() && seen.insert(s.key()));
        let mut seen = std::collections::HashSet::new();
        self.albums.retain(|a| !a.title.is_empty() && seen.insert((a.title.to_lowercase(), a.artist.to_lowercase())));
        let mut seen = std::collections::HashSet::new();
        self.artists.retain(|a| !a.trim().is_empty() && seen.insert(a.to_lowercase()));
        let mut seen = std::collections::HashSet::new();
        self.podcasts.retain(|p| !p.title.is_empty() && seen.insert(p.title.to_lowercase()));
        self.plays.retain(|p| p.song.usable() && p.at > 0);
        self.plays.sort_by_key(|p| p.at);
        for p in &mut self.playlists {
            p.songs.retain(Song::usable);
        }
        self.playlists.retain(|p| !p.songs.is_empty());
    }
}

// ───────────── the files of an export ─────────────

/// The files of an export (a zip, zips inside it, a folder, or one file), with their paths inside it.
#[derive(Default)]
pub struct Files {
    pub files: Vec<(String, Vec<u8>)>,
}

const MAX_FILE: u64 = 512 << 20;
/// All the files of one export together, unpacked (a small zip can unpack to far more than it is).
const MAX_TOTAL: u64 = 1 << 30;
const WANTED: &[&str] = &["json", "jsonl", "csv", "xml", "m3u", "m3u8", "txt", "zip"];

fn wanted(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    WANTED.contains(&ext.as_str())
}

impl Files {
    pub fn load(path: &Path) -> Result<Files> {
        Self::load_as(path, None)
    }

    /// `name`: what a single file was called when it was sent (its format is told partly from its name).
    pub fn load_as(path: &Path, name: Option<&str>) -> Result<Files> {
        let mut f = Files::default();
        let mut left = MAX_TOTAL;
        if path.is_dir() {
            for e in walkdir::WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
                if !e.file_type().is_file() || !wanted(&e.file_name().to_string_lossy()) {
                    continue;
                }
                if e.metadata().map(|m| m.len()).unwrap_or(0) > MAX_FILE {
                    continue;
                }
                let rel = e.path().strip_prefix(path).unwrap_or(e.path()).to_string_lossy().replace('\\', "/");
                f.add(rel, std::fs::read(e.path())?, 0, &mut left)?;
            }
        } else {
            if std::fs::metadata(path)?.len() > 4 * MAX_FILE {
                bail!("that file is too big to be a music export");
            }
            let name = name.map(|n| base(&n.replace('\\', "/")).to_string())
                .unwrap_or_else(|| path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
            f.add(name, std::fs::read(path).with_context(|| format!("reading {}", path.display()))?, 0, &mut left)?;
        }
        Ok(f)
    }

    /// One file; a zip is opened (and zips in it, as Apple's export has). `left`: how much more may be unpacked
    /// in all; a zip that unpacks to more (a zip bomb, or a size its listing understates) is refused, rather than
    /// filling the memory until the server is killed.
    pub fn add(&mut self, name: String, bytes: Vec<u8>, depth: usize, left: &mut u64) -> Result<()> {
        *left = left.checked_sub(bytes.len() as u64).context("that export unpacks to more than 1 GB")?;
        if bytes.starts_with(b"PK\x03\x04") && depth < 3 {
            *left += bytes.len() as u64; // counted as what it unpacks to instead
            let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("opening the zip")?;
            for i in 0..z.len() {
                let e = z.by_index(i)?;
                let inner = e.name().map(|n| n.replace('\\', "/")).unwrap_or_default();
                if e.is_dir() || e.size() > MAX_FILE || !wanted(&inner) {
                    continue;
                }
                if e.size() > *left {
                    bail!("that export unpacks to more than 1 GB");
                }
                let mut b = Vec::with_capacity(e.size() as usize);
                e.take(MAX_FILE.min(*left) + 1).read_to_end(&mut b)?;
                if b.len() as u64 > MAX_FILE.min(*left) {
                    bail!("{inner} unpacks to more than its zip says (or more than 1 GB in all)");
                }
                self.add(inner, b, depth + 1, left)?;
            }
            return Ok(());
        }
        if wanted(&name) {
            self.files.push((name, bytes));
        }
        Ok(())
    }

    /// Files whose name (not folder) matches.
    pub fn named<'a>(&'a self, pred: impl Fn(&str) -> bool + 'a) -> impl Iterator<Item = (&'a str, &'a [u8])> + 'a {
        self.files.iter().filter(move |(p, _)| pred(base(p))).map(|(p, b)| (p.as_str(), b.as_slice()))
    }
}

/// The file name of a path inside an export.
pub fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

pub fn stem(path: &str) -> &str {
    let b = base(path);
    b.rsplit_once('.').map(|(s, _)| s).unwrap_or(b)
}

/// Text of a file, without a byte-order mark.
pub fn text(b: &[u8]) -> String {
    let b = b.strip_prefix(b"\xef\xbb\xbf").unwrap_or(b);
    if b.starts_with(b"\xff\xfe") {
        // UTF-16 LE (Windows tools)
        let u: Vec<u16> = b[2..].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        return String::from_utf16_lossy(&u);
    }
    String::from_utf8_lossy(b).into_owned()
}

/// "2024-01-02 13:45", "2024-01-02T13:45:10Z", "2024-01-02T13:45:10.123+02:00", "2024-01-02" → seconds (UTC).
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(t) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(t.timestamp());
    }
    for f in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%d %H:%M:%S%.f"] {
        if let Ok(t) = chrono::NaiveDateTime::parse_from_str(s.trim_end_matches('Z').trim_end_matches(" UTC"), f) {
            return Some(t.and_utc().timestamp());
        }
    }
    chrono::NaiveDate::parse_from_str(&s[..s.len().min(10)], "%Y-%m-%d").ok().map(|d| d.and_hms_opt(12, 0, 0).unwrap().and_utc().timestamp())
}

/// The id in "spotify:track:ID" or "https://open.spotify.com/track/ID?si=…".
pub fn spotify_id(uri: &str) -> Option<String> {
    let id = if let Some(r) = uri.strip_prefix("spotify:track:") {
        r
    } else if let Some((_, r)) = uri.split_once("open.spotify.com/track/") {
        r.split(['?', '/']).next().unwrap_or("")
    } else {
        return None;
    };
    (id.len() == 22 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_string())
}

type FileSource = fn(&Files, &mut Parsed) -> Result<bool>;

/// Every file format, tried in turn; each takes the files that are its own.
const FILE_SOURCES: &[(&str, FileSource)] = &[
    ("spotify", spotify::parse),
    ("apple", apple::parse),
    ("youtube", google::parse),
    ("listenbrainz", misc::listenbrainz),
    ("playlists", misc::playlists),
];

/// Read an export (any of the formats above, detected from the files).
pub fn parse_files(files: &Files) -> Result<Parsed> {
    let mut p = Parsed::default();
    for (name, f) in FILE_SOURCES {
        f(files, &mut p).with_context(|| format!("reading the {name} files"))?;
    }
    p.tidy();
    if p.is_empty() && p.notes.is_empty() {
        bail!("nothing here looks like a music export (Spotify, Apple Music / iTunes, YouTube Music, ListenBrainz, M3U / CSV)");
    }
    Ok(p)
}

pub fn parse_path(path: &Path) -> Result<Parsed> {
    parse_files(&Files::load(path)?)
}

pub fn parse_upload(path: &Path, name: Option<&str>) -> Result<Parsed> {
    parse_files(&Files::load_as(path, name)?)
}

/// Where uploads wait until they're imported (deleted after, or after a day).
pub fn upload_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("imports")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zip_that_unpacks_too_big_is_refused() {
        use std::io::Write;
        let zip_of = |files: &[(&str, Vec<u8>)]| {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            for (n, b) in files {
                w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
                w.write_all(b).unwrap();
            }
            w.finish().unwrap().into_inner()
        };
        let big = vec![b' '; 4000]; // packs down to almost nothing
        let z = zip_of(&[("a.json", big.clone()), ("b.json", big.clone()), ("c.json", big.clone())]);
        assert!(z.len() < 1000);
        assert!(Files::default().add("x.zip".into(), z.clone(), 0, &mut 10_000).is_err(), "3 × 4000 unpacked is over 10 000");
        let mut f2 = Files::default();
        f2.add("x.zip".into(), z.clone(), 0, &mut 20_000).unwrap();
        assert_eq!(f2.files.len(), 3);
        // a zip in a zip counts what the inner one unpacks to
        let outer = zip_of(&[("inner.zip", z)]);
        assert!(Files::default().add("y.zip".into(), outer, 0, &mut 10_000).is_err());
    }

    #[test]
    fn times_ids_and_keys() {
        assert_eq!(parse_time("2024-01-02 13:45"), Some(1704203100));
        assert_eq!(parse_time("2024-01-02T13:45:00Z"), Some(1704203100));
        assert_eq!(parse_time("2024-01-02T15:45:00+02:00"), Some(1704203100));
        assert_eq!(parse_time("2024-01-02T13:45:00.000Z"), Some(1704203100));
        assert_eq!(parse_time("2024-01-02"), Some(1704196800));
        assert_eq!(parse_time(""), None);
        assert_eq!(spotify_id("spotify:track:4uLU6hMCjMI75M1A2tKUQC").as_deref(), Some("4uLU6hMCjMI75M1A2tKUQC"));
        assert_eq!(spotify_id("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=x").as_deref(), Some("4uLU6hMCjMI75M1A2tKUQC"));
        assert_eq!(spotify_id("spotify:episode:4uLU6hMCjMI75M1A2tKUQC"), None);
        let a = Song::named("Movin' Out (Anthony's Song) - 2008 Remaster", "Billy Joel", "");
        let b = Song::named("Movin' Out (Anthony's Song)", "billy joel", "The Stranger");
        assert_eq!(a.key(), b.key());
        assert_eq!(text(b"\xef\xbb\xbfhi"), "hi");
    }
}
