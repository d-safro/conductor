//! "Sounds like" from the audio itself, without AudioMuse-AI: each song is analysed once in the background and songs
//! that sound alike are found by how near their numbers are.
//!
//! - **What's analysed**: a minute from the middle of each song (all of a shorter one), decoded by ffmpeg to mono at
//!   22,050 Hz and described by 53 numbers (dsp.rs: tempo and how clear the beat is, the spectrum's brightness,
//!   spread, rolloff and flatness, zero crossings, level and dynamics, a chroma profile, MFCCs). Kept in their own
//!   table (`sound`, migration 0028), which a scan never touches; a file whose size or time changes is analysed again.
//! - **Gently**: in the same background round as sound levelling (loudness.rs), after it: ffmpeg at the lowest
//!   priority (nice 19 and idle disk priority on Linux, IDLE_PRIORITY_CLASS on Windows), the numbers worked out on a
//!   thread at nice 19 (Linux), `loudness_threads` songs at a time (1), a rest after each song half as long as it
//!   took, never while a scan runs, and at most `ROUND` at a time before levelling gets a look again. Songs that were
//!   played come first, then the rest in a random order, so "sounds like" covers the whole library early. What's done
//!   is in the database: after a restart it goes on where it stopped.
//! - **Alike**: the numbers as z-scores across the library, weighed (index.rs), and the nearest by distance; each
//!   song's nearest are worked out once and kept until the analysis changes. Blended with which artists go with which
//!   (similar.rs: played together, genres, era…): mostly the sound for getSonicSimilarTracks (Instant Mix), more of
//!   both for getSimilarSongs (song radio). AudioMuse-AI, when it's set up, still comes first (api/sonic.rs).
//! - `audio_analysis = false`: nothing is analysed and nothing analysed is used (as before).

pub mod dsp;
pub mod index;

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use self::index::{Index, Song};
use crate::{
    api::AppState,
    config::Config,
    library,
    loudness::{self, Failed},
};

/// The numbers' version: when what's worked out changes, every song is analysed again.
pub const VERSION: i64 = 1;
/// How much of a song is analysed: this long from its middle.
const EXCERPT_SECS: f64 = 60.0;
/// One song may take this long at most.
const MOST_PER_SONG: Duration = Duration::from_secs(5 * 60);
/// Fewer analysed songs than this: no "sounds like" from them yet.
const LEAST_SONGS: usize = 6;
/// While songs are being analysed, the index is built again at most this often (and at the end of each round).
const KEEP: Duration = Duration::from_secs(60);
/// The longest a round goes on before sound levelling gets a look again.
pub const ROUND: Duration = Duration::from_secs(15 * 60);
/// How much the sound counts against which artists go with the song's (similar.rs): for "sounds like"
/// (getSonicSimilarTracks, mixes) and for "goes with" (getSimilarSongs).
pub const SOUNDS_LIKE: f32 = 0.85;
pub const GOES_WITH: f32 = 0.6;
/// The song's own artist, as an artist that goes with it (high, but not so high that a mix is all one artist).
const SAME_ARTIST: f32 = 0.8;

/// What's kept between calls (one per server).
#[derive(Default)]
pub struct Analysis {
    built: tokio::sync::Mutex<Option<Built>>,
    /// one more for every song analysed
    generation: AtomicU64,
    /// one more for every round that analysed a song
    rounds: AtomicU64,
}

struct Built {
    index: Option<Arc<Index>>,
    generation: u64,
    rounds: u64,
    scan: i64,
    at: Instant,
}

// ───────────── how far it is ─────────────

/// For Podium: how many songs are analysed ("analysed 812 of 1676").
pub async fn status(st: &AppState) -> Value {
    let (songs, analysed, failed): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*),
                COALESCE(SUM(s.size = t.size AND s.mtime = t.mtime AND s.version = ?1), 0),
                COALESCE(SUM(s.size = t.size AND s.mtime = t.mtime AND s.version = ?1 AND s.features IS NULL), 0)
           FROM track t LEFT JOIN sound s ON s.track_id = t.id WHERE t.missing_since IS NULL",
    )
    .bind(VERSION)
    .fetch_one(&st.db)
    .await
    .unwrap_or((0, 0, 0));
    json!({ "on": st.cfg.audio_analysis, "songs": songs, "analysed": analysed, "unreadable": failed })
}

// ───────────── analysing ─────────────

/// A minute from the middle of the file (all of a shorter one), mono at `dsp::RATE` Hz.
pub async fn decode(cfg: &Config, path: &Path, duration_ms: i64) -> Result<Vec<f32>, Failed> {
    let total = duration_ms as f64 / 1000.0;
    let start = if total > EXCERPT_SECS { (total - EXCERPT_SECS) / 2.0 } else { 0.0 };
    let x = decode_from(cfg, path, start).await?;
    if x.len() < (dsp::SHORTEST_SECS * dsp::RATE as f32) as usize && start > 0.0 {
        return decode_from(cfg, path, 0.0).await; // shorter than its tags said
    }
    Ok(x)
}

async fn decode_from(cfg: &Config, path: &Path, start: f64) -> Result<Vec<f32>, Failed> {
    let mut cmd = loudness::ffmpeg(cfg);
    cmd.stdout(Stdio::piped());
    cmd.args(["-nostdin", "-hide_banner", "-nostats", "-v", "error", "-threads", "1"]);
    if start > 0.0 {
        cmd.args(["-ss", &format!("{start:.2}")]);
    }
    cmd.arg("-i").arg(path);
    cmd.args(["-map", "0:a:0", "-vn", "-sn", "-dn", "-t", &EXCERPT_SECS.to_string(), "-ac", "1"]);
    cmd.args(["-ar", &dsp::RATE.to_string(), "-f", "f32le", "-"]);
    let mut child = cmd.spawn().map_err(|e| Failed::NoFfmpeg(format!("{}: {e}", cfg.ffmpeg().display())))?;
    let (mut out, mut err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let most = ((EXCERPT_SECS + 2.0) * dsp::RATE as f64) as u64 * 4;
    let run = async {
        let mut pcm = Vec::with_capacity(most as usize);
        let mut msg = Vec::new();
        let (mut o, mut e) = ((&mut out).take(most), (&mut err).take(64 << 10));
        let (a, _) = tokio::join!(o.read_to_end(&mut pcm), e.read_to_end(&mut msg));
        drop(out); // (more than asked for: ffmpeg stops at the closed pipe)
        let status = child.wait().await;
        (pcm, msg, a, status)
    };
    let (pcm, msg, read, status) =
        tokio::time::timeout(MOST_PER_SONG, run).await.map_err(|_| Failed::File("took too long".into()))?;
    let msg = String::from_utf8_lossy(&msg);
    let last = msg.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    match (status, read) {
        (Ok(s), Ok(_)) if s.success() || pcm.len() as u64 >= most => {}
        (Ok(s), _) => return Err(Failed::File(format!("ffmpeg {s}: {last}"))),
        (Err(e), _) => return Err(Failed::File(e.to_string())),
    }
    Ok(pcm.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// Run `f` on a thread of its own at the lowest priority (Linux: nice 19 for that thread only), so the numbers are
/// worked out only when nothing else wants the processor.
async fn gently<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("conductor-sound".into())
        .spawn(move || {
            #[cfg(target_os = "linux")]
            unsafe {
                // (on Linux a thread has its own nice value: this one only)
                libc::setpriority(libc::PRIO_PROCESS, libc::syscall(libc::SYS_gettid) as libc::id_t, 19);
            }
            let _ = tx.send(f());
        })
        .ok()?;
    rx.await.ok()
}

/// Analyse one file: its numbers (None: silent, or too short).
pub async fn analyse(cfg: &Config, path: &Path, duration_ms: i64) -> Result<Option<Vec<f32>>, Failed> {
    let x = decode(cfg, path, duration_ms).await?;
    Ok(gently(move || dsp::analyse(&x).map(|f| f.to_vec())).await.flatten())
}

/// The file's size and time as the scanner keeps them (seconds).
fn stat(path: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((md.len() as i64, mtime))
}

// ───────────── the worker ─────────────

/// What a round did.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Pass {
    /// songs that needed analysing when it started
    pub due: usize,
    pub analysed: usize,
    /// couldn't be read, or silent (marked, not tried again until the file changes)
    pub unreadable: usize,
}

/// How much a round may do.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub songs: usize,
    pub time: Duration,
}

impl Budget {
    pub const ALL: Budget = Budget { songs: usize::MAX, time: Duration::MAX };
}

/// Analyse the songs due now (never analysed, or their file changed since), as many as `budget` allows. Nothing
/// when `audio_analysis` is off.
pub async fn pass(st: &AppState, budget: Budget) -> Result<Pass> {
    if !st.cfg.audio_analysis {
        return Ok(Pass::default());
    }
    let online: HashSet<i64> = st.libs.iter().filter(|(_, l)| library::is_online(l)).map(|(id, _)| *id).collect();
    type Due = (String, i64, String, i64, Option<i64>, Option<i64>, Option<i64>);
    let due: Vec<Due> = sqlx::query_as(
        "SELECT t.id, t.library_id, t.rel_path, t.duration_ms, s.size, s.mtime, s.version
           FROM track t LEFT JOIN sound s ON s.track_id = t.id
           LEFT JOIN (SELECT DISTINCT track_id FROM play) pl ON pl.track_id = t.id
          WHERE t.missing_since IS NULL AND (s.track_id IS NULL OR s.size != t.size OR s.mtime != t.mtime OR s.version != ?)
          ORDER BY pl.track_id IS NULL, RANDOM()",
    )
    .bind(VERSION)
    .fetch_all(&st.db)
    .await?;
    let due: Vec<Due> = due.into_iter().filter(|d| online.contains(&d.1)).collect();
    let mut rep = Pass { due: due.len(), ..Pass::default() };
    if due.is_empty() {
        return Ok(rep);
    }
    let threads = st.cfg.loudness_threads.clamp(1, 64);
    info!(songs = due.len(), threads, "sounds like: analysing songs");
    let started = Instant::now();
    let (taken, done, unreadable) = (AtomicUsize::new(0), AtomicUsize::new(0), AtomicUsize::new(0));
    let no_ffmpeg = std::sync::Mutex::new(None::<String>);
    let busy = std::sync::Mutex::new(Duration::ZERO);
    futures_util::stream::iter(due)
        .for_each_concurrent(threads, |(id, lib, rel, ms, size, mtime, version)| {
            let (taken, done, unreadable, no_ffmpeg, busy) = (&taken, &done, &unreadable, &no_ffmpeg, &busy);
            async move {
                if no_ffmpeg.lock().unwrap_or_else(|e| e.into_inner()).is_some() || started.elapsed() >= budget.time {
                    return;
                }
                while st.scan.scanning.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let Some(path) = library::track_path(&st.libs, lib, &rel) else { return };
                let Some(before) = stat(&path) else { return }; // gone, or the drive went away: not a failure
                if (Some(before.0), Some(before.1), Some(VERSION)) == (size, mtime, version) {
                    return; // analysed as it is (the database's size or time is stale until the next scan)
                }
                if taken.fetch_add(1, Ordering::Relaxed) >= budget.songs {
                    return;
                }
                let t0 = Instant::now();
                let r = analyse(&st.cfg, &path, ms).await;
                let took = t0.elapsed();
                *busy.lock().unwrap_or_else(|e| e.into_inner()) += took;
                if stat(&path) != Some(before) {
                    return; // changed (or went away) while it was analysed: next time
                }
                let features = match r {
                    Ok(Some(f)) => Some(dsp::to_bytes(&f)),
                    Ok(None) => {
                        debug!(file = %path.display(), "sounds like: silent or too short");
                        unreadable.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Err(Failed::NoFfmpeg(e)) => {
                        *no_ffmpeg.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                        return;
                    }
                    Err(Failed::File(e)) => {
                        warn!(file = %path.display(), "sounds like: couldn't analyse this song ({e})");
                        unreadable.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                };
                let w = sqlx::query(
                    "INSERT OR REPLACE INTO sound (track_id, size, mtime, version, features, analysed_at) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(before.0)
                .bind(before.1)
                .bind(VERSION)
                .bind(features)
                .bind(crate::db::now())
                .execute(&st.db)
                .await;
                if let Err(e) = w {
                    warn!("sounds like: couldn't save an analysis: {e}");
                    return;
                }
                st.sound.generation.fetch_add(1, Ordering::Relaxed);
                done.fetch_add(1, Ordering::Relaxed);
                debug!(file = %path.display(), ms = took.as_millis() as u64, "analysed");
                // a rest, half as long as that song took
                tokio::time::sleep(took / 2).await;
            }
        })
        .await;
    if let Some(e) = no_ffmpeg.into_inner().unwrap_or_else(|e| e.into_inner()) {
        bail!("ffmpeg isn't there ({e}): no \"sounds like\" from the audio");
    }
    rep.analysed = done.into_inner();
    rep.unreadable = unreadable.into_inner();
    if rep.analysed > 0 {
        st.sound.rounds.fetch_add(1, Ordering::Relaxed);
        let busy = busy.into_inner().unwrap_or_else(|e| e.into_inner());
        let left = rep.due.saturating_sub(rep.analysed);
        info!(
            analysed = rep.analysed,
            unreadable = rep.unreadable,
            left,
            "sounds like: {} in {:.0} s ({:.2} s a song analysing)",
            if left == 0 { "done" } else { "a round" },
            started.elapsed().as_secs_f64(),
            busy.as_secs_f64() / rep.analysed as f64
        );
    }
    Ok(rep)
}

// ───────────── what sounds alike ─────────────

/// The analysed songs, as an index (None while there are too few, or when it's off). Built again after a round, after
/// a scan, and now and then while songs are being analysed.
async fn index(st: &AppState) -> Option<Arc<Index>> {
    if !st.cfg.audio_analysis {
        return None;
    }
    let mut g = st.sound.built.lock().await;
    let generation = st.sound.generation.load(Ordering::Relaxed);
    let rounds = st.sound.rounds.load(Ordering::Relaxed);
    let scan = st.scan.last_scan.load(Ordering::Relaxed);
    if let Some(b) = g.as_ref()
        && b.scan == scan
        && b.rounds == rounds
        && (b.generation == generation || b.at.elapsed() < KEEP)
    {
        return b.index.clone();
    }
    let rows: Vec<(String, String, String, Vec<u8>)> = sqlx::query_as(
        "SELECT s.track_id, t.artist_id, t.title, s.features FROM sound s JOIN track t ON t.id = s.track_id
          WHERE t.missing_since IS NULL AND s.features IS NOT NULL AND s.version = ? AND s.size = t.size AND s.mtime = t.mtime",
    )
    .bind(VERSION)
    .fetch_all(&st.db)
    .await
    .ok()?;
    let songs: Vec<Song> = rows
        .into_iter()
        .filter_map(|(id, artist, title, f)| Some(Song { id, artist, title, features: dsp::from_bytes(&f)? }))
        .collect();
    let index = tokio::task::spawn_blocking(move || Index::build(songs, LEAST_SONGS)).await.ok().flatten().map(Arc::new);
    *g = Some(Built { index: index.clone(), generation, rounds, scan, at: Instant::now() });
    index
}

/// Songs that sound like this one, best first, with how alike they sound (0…1). `sound` is how much the sound counts
/// against which artists go with the song's (similar.rs): 1 for the sound alone. One of each song (not the same song
/// from another album), and at most a quarter of them by one artist while there are others. None when the song
/// isn't analysed (or it's off).
pub async fn sounds_like(st: &AppState, id: &str, n: usize, sound: f32) -> Option<Vec<(String, f32)>> {
    let ix = index(st).await?;
    let i = *ix.pos.get(id)?;
    let near = ix.nearest(i);
    let goes: HashMap<String, f32> = if sound < 1.0 {
        let a = crate::similar::artists(st, &ix.artist[i], &[], 100).await;
        let top = a.first().map_or(1.0, |x| x.1).max(1e-6);
        a.into_iter().map(|(id, s)| (id, s / top)).collect()
    } else {
        HashMap::new()
    };
    let mut scored: Vec<(usize, f32, f32)> = near
        .iter()
        .map(|&(j, d)| {
            let s = ix.similarity(d);
            let g = if ix.artist[j] == ix.artist[i] { SAME_ARTIST } else { goes.get(&ix.artist[j]).copied().unwrap_or(0.0) };
            (j, sound * s + (1.0 - sound) * g, s)
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let most = (n / 4).max(2);
    let mut songs = HashSet::from([ix.same_song[i].as_str()]);
    let mut per: HashMap<&str, usize> = HashMap::new();
    let (mut out, mut later) = (vec![], vec![]);
    for &(j, _, s) in &scored {
        if out.len() >= n {
            break;
        }
        if !songs.insert(ix.same_song[j].as_str()) {
            continue;
        }
        let c = per.entry(ix.artist[j].as_str()).or_default();
        if *c >= most {
            later.push((j, s));
        } else {
            *c += 1;
            out.push((j, s));
        }
    }
    let room = n.saturating_sub(out.len());
    out.extend(later.into_iter().take(room));
    Some(out.into_iter().map(|(j, s)| (ix.ids[j].clone(), s)).collect())
}

/// A path of songs that moves from the sound of one to the other's (both ends in it). None when either isn't
/// analysed.
pub async fn path(st: &AppState, from: &str, to: &str, steps: usize) -> Option<Vec<String>> {
    let ix = index(st).await?;
    let (a, b) = (*ix.pos.get(from)?, *ix.pos.get(to)?);
    if a == b {
        return None;
    }
    Some(ix.path(a, b, steps).into_iter().map(|i| ix.ids[i].clone()).collect())
}
