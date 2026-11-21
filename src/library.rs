//! Library scanning, with a guard for drives that drop out.
//!
//! A library counts as connected only when its folder exists and either holds its sentinel file (if one is set)
//! or holds anything at all. While it isn't connected nothing is scanned and nothing is marked missing. A scan
//! that would suddenly lose more than half of the library's songs also leaves them alone (a half-mounted drive).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicI64, Ordering},
    },
    time::UNIX_EPOCH,
};

use anyhow::Result;
use lofty::prelude::*;
use tracing::{info, warn};

use crate::{
    config::LibraryConfig,
    db::{self, Db},
};

pub const AUDIO: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "opus", "m4a", "m4b", "mp4", "aac", "alac", "wav", "aif", "aiff", "ape", "wv", "mpc", "wma",
    "dsf",
];

pub fn content_type(suffix: &str) -> &'static str {
    match suffix {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/ogg; codecs=opus",
        "m4a" | "m4b" | "mp4" | "aac" | "alac" => "audio/mp4",
        "wav" => "audio/wav",
        "aif" | "aiff" => "audio/aiff",
        "ape" => "audio/x-ape",
        "wv" => "audio/x-wavpack",
        "mpc" => "audio/x-musepack",
        "wma" => "audio/x-ms-wma",
        "dsf" => "audio/x-dsf",
        _ => "application/octet-stream",
    }
}

#[derive(Default)]
pub struct ScanStatus {
    pub scanning: AtomicBool,
    pub count: AtomicI64,
    pub last_scan: AtomicI64,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct ScanReport {
    pub libraries_offline: Vec<String>,
    pub added: usize,
    pub updated: usize,
    pub missing: usize,
    pub unchanged: usize,
    pub skipped_missing_guard: bool,
}

/// Is this library's folder really there (and not an empty mount point)?
pub fn is_online(lib: &LibraryConfig) -> bool {
    let p = &lib.path;
    if !p.is_dir() {
        return false;
    }
    match &lib.sentinel {
        Some(s) => p.join(s).exists(),
        None => std::fs::read_dir(p).map(|mut d| d.next().is_some()).unwrap_or(false),
    }
}

pub async fn ensure_libraries(db: &Db, libs: &[LibraryConfig]) -> Result<Vec<(i64, LibraryConfig)>> {
    let mut out = vec![];
    for l in libs {
        let root = l.path.to_string_lossy().to_string();
        sqlx::query(
            "INSERT INTO library (name, root_path) VALUES (?, ?) ON CONFLICT(root_path) DO UPDATE SET name = excluded.name",
        )
        .bind(&l.name)
        .bind(&root)
        .execute(db)
        .await?;
        let id: i64 = sqlx::query_scalar("SELECT id FROM library WHERE root_path = ?").bind(&root).fetch_one(db).await?;
        out.push((id, l.clone()));
    }
    Ok(out)
}

#[derive(Debug, Clone)]
struct FileStat {
    rel: String,
    abs: PathBuf,
    size: i64,
    mtime: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub compilation: bool,
    pub disc: Option<i64>,
    pub number: Option<i64>,
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub mbid: Option<String>,
    pub album_mbid: Option<String>,
    pub artist_mbid: Option<String>,
    pub album_artist_mbid: Option<String>,
    pub duration_ms: i64,
    pub bitrate: i64,
    pub sample_rate: i64,
    pub bit_depth: Option<i64>,
    pub channels: i64,
    pub has_picture: bool,
    pub has_lyrics: bool,
    pub isrc: Option<String>,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
}

fn year_of(s: &str) -> Option<i64> {
    let d: String = s.chars().take_while(|c| c.is_ascii_digit()).take(4).collect();
    if d.len() == 4 { d.parse().ok() } else { None }
}

/// "-6.52 dB" or "0.98" → the number.
fn db_value(s: &str) -> Option<f64> {
    let t = s.trim().trim_end_matches(|c: char| c.is_alphabetic() || c.is_whitespace());
    t.replace(',', ".").parse::<f64>().ok().filter(|v| v.is_finite())
}

fn first_num(s: &str) -> Option<i64> {
    s.split('/').next()?.trim().parse().ok()
}

pub fn read_meta(path: &Path) -> Meta {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut m = Meta { title: stem, ..Default::default() };
    // Pictures are skipped here (they're often most of a file's tag bytes); covers are read when asked for.
    let opts = lofty::config::ParseOptions::new().read_cover_art(false);
    let Ok(tf) = lofty::probe::Probe::open(path).and_then(|p| p.options(opts).read()) else {
        return m;
    };
    let p = tf.properties();
    m.duration_ms = p.duration().as_millis() as i64;
    m.bitrate = p.audio_bitrate().or(p.overall_bitrate()).unwrap_or(0) as i64;
    m.sample_rate = p.sample_rate().unwrap_or(0) as i64;
    m.bit_depth = p.bit_depth().map(|b| b as i64);
    m.channels = p.channels().unwrap_or(0) as i64;
    if let Some(tag) = tf.primary_tag().or_else(|| tf.first_tag()) {
        let get = |k: ItemKey| tag.get_string(&k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if let Some(t) = get(ItemKey::TrackTitle) {
            m.title = t;
        }
        m.artist = get(ItemKey::TrackArtist).unwrap_or_default();
        m.album = get(ItemKey::AlbumTitle).unwrap_or_default();
        m.album_artist = get(ItemKey::AlbumArtist).unwrap_or_default();
        m.compilation = get(ItemKey::FlagCompilation).is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        m.number = get(ItemKey::TrackNumber).and_then(|s| first_num(&s));
        m.disc = get(ItemKey::DiscNumber).and_then(|s| first_num(&s));
        m.year = get(ItemKey::Year)
            .or_else(|| get(ItemKey::RecordingDate))
            .or_else(|| get(ItemKey::OriginalReleaseDate))
            .and_then(|s| year_of(&s));
        m.genre = get(ItemKey::Genre);
        m.isrc = get(ItemKey::Isrc);
        m.rg_track_gain = get(ItemKey::ReplayGainTrackGain).and_then(|s| db_value(&s));
        m.rg_track_peak = get(ItemKey::ReplayGainTrackPeak).and_then(|s| db_value(&s));
        m.rg_album_gain = get(ItemKey::ReplayGainAlbumGain).and_then(|s| db_value(&s));
        m.rg_album_peak = get(ItemKey::ReplayGainAlbumPeak).and_then(|s| db_value(&s));
        m.mbid = get(ItemKey::MusicBrainzRecordingId);
        m.album_mbid = get(ItemKey::MusicBrainzReleaseId);
        m.artist_mbid = get(ItemKey::MusicBrainzArtistId);
        m.album_artist_mbid = get(ItemKey::MusicBrainzReleaseArtistId);
        m.has_lyrics = get(ItemKey::Lyrics).is_some();
    }
    if !m.has_lyrics {
        m.has_lyrics = path.with_extension("lrc").exists();
    }
    if m.artist.is_empty() {
        m.artist = if m.album_artist.is_empty() { "[Unknown Artist]".into() } else { m.album_artist.clone() };
    }
    if m.album_artist.is_empty() {
        m.album_artist = if m.compilation { "Various Artists".into() } else { m.artist.clone() };
    }
    if m.album.is_empty() {
        m.album = "[Unknown Album]".into();
    }
    m
}

pub fn sort_key(s: &str) -> String {
    let l = s.to_lowercase();
    for a in ["the ", "a ", "an "] {
        if let Some(r) = l.strip_prefix(a)
            && !r.is_empty()
        {
            return r.to_string();
        }
    }
    l
}

pub fn artist_id(name: &str) -> String {
    db::stable_id("artist", &name.to_lowercase())
}

/// One album per album artist + album name, whichever release each song came from (like ND_PID_ALBUM=albumartistid,album).
pub fn album_id(album_artist: &str, album: &str) -> String {
    db::stable_id("album", &format!("{}\u{1f}{}", album_artist.to_lowercase(), album.to_lowercase()))
}

pub fn track_id(library_id: i64, rel: &str) -> String {
    db::stable_id("track", &format!("{library_id}:{rel}"))
}

fn walk(root: &Path) -> Vec<FileStat> {
    let mut out = vec![];
    for e in walkdir::WalkDir::new(root).follow_links(true).into_iter().filter_map(|e| e.ok()) {
        if !e.file_type().is_file() {
            continue;
        }
        let name = e.file_name().to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Some(ext) = e.path().extension().map(|x| x.to_string_lossy().to_lowercase()) else { continue };
        if !AUDIO.contains(&ext.as_str()) {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
        let rel = e
            .path()
            .strip_prefix(root)
            .unwrap_or(e.path())
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        out.push(FileStat { rel, abs: e.path().to_path_buf(), size: md.len() as i64, mtime });
    }
    out
}

/// Read tags on a few threads. Spinning drives do best with 1-2, SSDs with more (`scan_threads`).
fn read_all(files: Vec<FileStat>, threads: usize) -> Vec<(FileStat, Meta)> {
    let threads = threads.clamp(1, 16);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut out: Vec<Option<Meta>> = vec![None; files.len()];
    let slots: Vec<std::sync::Mutex<&mut Option<Meta>>> = out.iter_mut().map(std::sync::Mutex::new).collect();
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let m = read_meta(&f.abs);
                    **slots[i].lock().unwrap() = Some(m);
                }
            });
        }
    });
    drop(slots);
    files.into_iter().zip(out).map(|(f, m)| (f, m.unwrap_or_default())).collect()
}

/// Scan every library. `full` re-reads tags of unchanged files too.
pub async fn scan(db: &Db, libs: &[(i64, LibraryConfig)], status: &Arc<ScanStatus>, full: bool) -> Result<ScanReport> {
    if status.scanning.swap(true, Ordering::SeqCst) {
        return Ok(ScanReport::default());
    }
    let r = scan_inner(db, libs, status, full).await;
    status.scanning.store(false, Ordering::SeqCst);
    status.last_scan.store(db::now(), Ordering::SeqCst);
    if let Ok(rep) = &r {
        info!(?rep, "scan done");
    }
    r
}

async fn scan_inner(db: &Db, libs: &[(i64, LibraryConfig)], status: &Arc<ScanStatus>, full: bool) -> Result<ScanReport> {
    let mut rep = ScanReport::default();
    status.count.store(0, Ordering::SeqCst);
    for (lib_id, lib) in libs {
        let online = is_online(lib);
        sqlx::query("UPDATE library SET online = ? WHERE id = ?").bind(online).bind(lib_id).execute(db).await?;
        if !online {
            warn!(library = %lib.name, path = %lib.path.display(), "library not connected: not scanning, nothing marked missing");
            rep.libraries_offline.push(lib.name.clone());
            continue;
        }
        let root = lib.path.clone();
        let threads = lib.scan_threads.unwrap_or(2);
        let files = tokio::task::spawn_blocking(move || walk(&root)).await?;
        let known: HashMap<String, (i64, i64, Option<i64>)> = sqlx::query_as::<_, (String, i64, i64, Option<i64>)>(
            "SELECT rel_path, size, mtime, missing_since FROM track WHERE library_id = ?",
        )
        .bind(lib_id)
        .fetch_all(db)
        .await?
        .into_iter()
        .map(|(r, s, m, x)| (r, (s, m, x)))
        .collect();
        let present_before = known.values().filter(|v| v.2.is_none()).count();

        let changed: Vec<FileStat> = files
            .iter()
            .filter(|f| full || known.get(&f.rel).is_none_or(|k| k.0 != f.size || k.1 != f.mtime || k.2.is_some()))
            .cloned()
            .collect();
        rep.unchanged += files.len() - changed.len();
        let metas = tokio::task::spawn_blocking(move || read_all(changed, threads)).await?;

        let now = db::now();
        let mut tx = db.begin().await?;
        for (f, m) in &metas {
            status.count.fetch_add(1, Ordering::Relaxed);
            let suffix = Path::new(&f.rel).extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
            let id = track_id(*lib_id, &f.rel);
            if known.contains_key(&f.rel) {
                rep.updated += 1
            } else {
                rep.added += 1
            }
            sqlx::query(
                "INSERT INTO track (id, library_id, rel_path, size, mtime, suffix, content_type, bitrate, sample_rate, bit_depth, channels,
                    duration_ms, title, sort_title, artist_id, artist_name, album_id, album_name, disc, number, year, genre, mbid,
                    has_picture, has_lyrics, created_at, isrc, rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, missing_since)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,NULL)
                 ON CONFLICT(id) DO UPDATE SET size=excluded.size, mtime=excluded.mtime, suffix=excluded.suffix,
                    content_type=excluded.content_type, bitrate=excluded.bitrate, sample_rate=excluded.sample_rate,
                    bit_depth=excluded.bit_depth, channels=excluded.channels, duration_ms=excluded.duration_ms, title=excluded.title,
                    sort_title=excluded.sort_title, artist_id=excluded.artist_id, artist_name=excluded.artist_name,
                    album_id=excluded.album_id, album_name=excluded.album_name, disc=excluded.disc, number=excluded.number,
                    year=excluded.year, genre=excluded.genre, mbid=excluded.mbid, has_picture=excluded.has_picture,
                    has_lyrics=excluded.has_lyrics, isrc=excluded.isrc, rg_track_gain=excluded.rg_track_gain,
                    rg_track_peak=excluded.rg_track_peak, rg_album_gain=excluded.rg_album_gain,
                    rg_album_peak=excluded.rg_album_peak, missing_since=NULL",
            )
            .bind(&id).bind(lib_id).bind(&f.rel).bind(f.size).bind(f.mtime).bind(&suffix).bind(content_type(&suffix))
            .bind(m.bitrate).bind(m.sample_rate).bind(m.bit_depth).bind(m.channels).bind(m.duration_ms)
            .bind(&m.title).bind(sort_key(&m.title)).bind(artist_id(&m.artist)).bind(&m.artist)
            .bind(album_id(&m.album_artist, &m.album)).bind(&m.album).bind(m.disc).bind(m.number).bind(m.year)
            .bind(&m.genre).bind(&m.mbid).bind(m.has_picture).bind(m.has_lyrics).bind(now)
            .bind(&m.isrc).bind(m.rg_track_gain).bind(m.rg_track_peak).bind(m.rg_album_gain).bind(m.rg_album_peak)
            .execute(&mut *tx)
            .await?;
            for (name, mbid) in [(&m.artist, &m.artist_mbid), (&m.album_artist, &m.album_artist_mbid)] {
                sqlx::query(
                    "INSERT INTO artist (id, name, sort_name, mbid) VALUES (?,?,?,?)
                             ON CONFLICT(id) DO UPDATE SET mbid = COALESCE(excluded.mbid, artist.mbid)",
                )
                .bind(artist_id(name))
                .bind(name)
                .bind(sort_key(name))
                .bind(mbid)
                .execute(&mut *tx)
                .await?;
            }
            let aid = album_id(&m.album_artist, &m.album);
            sqlx::query(
                "INSERT INTO album (id, name, sort_name, artist_id, artist_name, year, genre, mbid, compilation, created_at)
                 VALUES (?,?,?,?,?,?,?,?,?,?)
                 ON CONFLICT(id) DO UPDATE SET year = COALESCE(excluded.year, album.year), genre = COALESCE(excluded.genre, album.genre),
                    mbid = COALESCE(excluded.mbid, album.mbid), compilation = MAX(album.compilation, excluded.compilation)",
            )
            .bind(&aid).bind(&m.album).bind(sort_key(&m.album)).bind(artist_id(&m.album_artist)).bind(&m.album_artist)
            .bind(m.year).bind(&m.genre).bind(&m.album_mbid).bind(m.compilation).bind(now)
            .execute(&mut *tx)
            .await?;
        }

        // Files that are gone. Not when most of the library seems to have vanished at once (a drive half there).
        let seen: std::collections::HashSet<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        let gone: Vec<&String> =
            known.iter().filter(|(r, v)| v.2.is_none() && !seen.contains(r.as_str())).map(|(r, _)| r).collect();
        if present_before >= 20 && gone.len() * 2 > present_before {
            warn!(library = %lib.name, gone = gone.len(), of = present_before, "most songs seem gone at once: leaving them alone (drive half-connected?)");
            rep.skipped_missing_guard = true;
        } else {
            for r in gone {
                sqlx::query("UPDATE track SET missing_since = ? WHERE library_id = ? AND rel_path = ?")
                    .bind(now)
                    .bind(lib_id)
                    .bind(r)
                    .execute(&mut *tx)
                    .await?;
                rep.missing += 1;
            }
        }
        sqlx::query("UPDATE library SET last_scan_at = ? WHERE id = ?").bind(now).bind(lib_id).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    refresh_counts(db).await?;
    Ok(rep)
}

/// Album and artist totals from the songs that are present.
pub async fn refresh_counts(db: &Db) -> Result<()> {
    let mut tx = db.begin().await?;
    sqlx::query(
        "UPDATE album SET
            song_count = (SELECT COUNT(*) FROM track t WHERE t.album_id = album.id AND t.missing_since IS NULL),
            duration_ms = (SELECT COALESCE(SUM(duration_ms), 0) FROM track t WHERE t.album_id = album.id AND t.missing_since IS NULL),
            cover_track_id = (SELECT id FROM track t WHERE t.album_id = album.id AND t.missing_since IS NULL
                              ORDER BY has_picture DESC, disc, number LIMIT 1)",
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE artist SET
            album_count = (SELECT COUNT(*) FROM album a WHERE a.artist_id = artist.id AND a.song_count > 0),
            song_count = (SELECT COUNT(*) FROM track t WHERE t.artist_id = artist.id AND t.missing_since IS NULL)",
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// The file on disk for a track, if its library is configured.
pub fn track_path(libs: &[(i64, LibraryConfig)], library_id: i64, rel: &str) -> Option<PathBuf> {
    let (_, l) = libs.iter().find(|(id, _)| *id == library_id)?;
    let mut p = l.path.clone();
    for part in rel.split('/') {
        if part == ".." {
            return None;
        }
        p.push(part);
    }
    Some(p)
}
