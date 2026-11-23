//! Sound levelling: songs play at the same loudness, also the ones whose files have no ReplayGain tags.
//!
//! Most apps (Encore, Symfonium, Feishin…) level songs themselves with the OpenSubsonic `replayGain` object a song
//! comes with. That's only there when the file has ReplayGain tags, and many don't (most of what comes from
//! Soulseek). So a background worker measures those songs with ffmpeg (EBU R128 integrated loudness and true peak,
//! the `ebur128` filter) and the apps get the measured values in the same object:
//!
//! - track gain = −18 LUFS (ReplayGain 2.0's reference) − the song's integrated loudness; peak = its true peak
//!   (linear, 1.0 = full scale);
//! - album gain from the album's measured songs: their loudness averaged by energy, each weighted by its length (the
//!   same as measuring the album as one long file, without the gating across songs); album peak = the highest;
//! - **tags always win**: a measurement fills only what the tags don't have. Measurements are kept in their own
//!   columns (`lm_*`, migration 0023), which a scan never touches, so a rescan keeps them; a file whose size or time
//!   changes is measured again.
//!
//! Which songs: those without a track gain tag or without an album gain tag. ffmpeg runs at the lowest priority
//! (nice 19 and idle disk priority on Linux, IDLE_PRIORITY_CLASS on Windows: below conversions, which someone is
//! waiting for), `loudness_threads` at a time (1), resting between songs half as long as the last one took, and
//! never while a scan runs. A song takes about half a second (four minutes of FLAC on a desktop) to a few seconds
//! (a small home server), ~70 MB. What's done is in the database, so after a restart it goes on where it stopped;
//! at shutdown the task is dropped and its ffmpeg killed (`kill_on_drop`). A file ffmpeg can't read is marked as
//! tried and not tried again until it changes.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

use crate::{api::AppState, config::Config, db::Db, library};

/// ReplayGain 2.0's reference loudness: a song measured at this plays without gain.
pub const REFERENCE_LUFS: f64 = -18.0;
/// Quieter than this (ebur128 says −70 for silence) has no gain: there's nothing to level.
const QUIETEST_LUFS: f64 = -60.0;
/// The most a measured song is turned up (a very quiet one would otherwise get its noise turned up 30 dB too).
const MOST_BOOST_DB: f64 = 15.0;
/// After the server starts: the first scan goes first.
const START_DELAY: Duration = Duration::from_secs(60);
/// Between looks for songs to measure (new ones from a scan, changed files).
const LOOK_EVERY: Duration = Duration::from_secs(60);
/// One song may take this long at most (a damaged file that makes ffmpeg hang, a very long mix on a slow disk).
const MOST_PER_SONG: Duration = Duration::from_secs(20 * 60);
/// What's read of ffmpeg's messages (the summary is at the end, the file's tags at the start).
const MOST_OUTPUT: u64 = 4 << 20;

// ───────────── what the apps get ─────────────

/// The gain for a measured loudness (None for silence or no measurement), rounded as tags are (0.01 dB).
pub fn gain(loudness: Option<f64>) -> Option<f64> {
    let l = loudness.filter(|l| l.is_finite() && *l > QUIETEST_LUFS)?;
    Some(((REFERENCE_LUFS - l).min(MOST_BOOST_DB) * 100.0).round() / 100.0)
}

fn peak(p: Option<f64>) -> Option<f64> {
    p.filter(|p| p.is_finite() && *p > 0.0).map(|p| (p * 1e6).round() / 1e6)
}

/// A song's levels: what its tags say, and what was measured.
#[derive(Debug, Default, Clone, Copy)]
pub struct Levels {
    pub track_gain: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain: Option<f64>,
    pub album_peak: Option<f64>,
    /// measured: integrated loudness (LUFS) and true peak, the song's and its album's
    pub loudness: Option<f64>,
    pub peak: Option<f64>,
    pub album_loudness: Option<f64>,
    pub album_peak_measured: Option<f64>,
}

/// The OpenSubsonic `replayGain` object: each field from the tags when they have it, else measured, else left out.
pub fn replay_gain(l: &Levels) -> serde_json::Map<String, Value> {
    let measured = gain(l.loudness).is_some();
    let album_measured = gain(l.album_loudness).is_some();
    let mut rg = serde_json::Map::new();
    for (k, x) in [
        ("trackGain", l.track_gain.or(gain(l.loudness))),
        ("trackPeak", l.track_peak.or(peak(l.peak).filter(|_| measured))),
        ("albumGain", l.album_gain.or(gain(l.album_loudness))),
        ("albumPeak", l.album_peak.or(peak(l.album_peak_measured).filter(|_| album_measured))),
    ] {
        if let Some(x) = x {
            rg.insert(k.into(), x.into());
        }
    }
    rg
}

/// A song's `replayGain` object by its id (for pages that don't go through the Subsonic API: share links).
pub async fn replay_gain_of(db: &Db, id: &str) -> Value {
    type Row = (Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<f64>);
    let r: Option<Row> = sqlx::query_as(
        "SELECT rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, lm_loudness, lm_peak, lm_album_loudness, lm_album_peak
           FROM track WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let Some(r) = r else { return json!({}) };
    Value::Object(replay_gain(&Levels {
        track_gain: r.0,
        track_peak: r.1,
        album_gain: r.2,
        album_peak: r.3,
        loudness: r.4,
        peak: r.5,
        album_loudness: r.6,
        album_peak_measured: r.7,
    }))
}

// ───────────── levelled conversions ─────────────

/// The loudest a levelled conversion may peak: −1 dBTP (EBU R128's ceiling, which leaves room for a lossy encoder's
/// overshoot and for peaks from tags that are sample peaks, not true peaks).
pub const CEILING: f64 = 0.891_250_938_133_745_5; // 10^(−1/20)

/// What a levelled conversion (`level_conversions`) applies to a song.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Applied {
    /// dB, rounded as tags are (0.01 dB)
    pub gain: f64,
    /// the album's gain (else the song's own)
    pub album: bool,
    /// turned up less than its gain says, so its peak stays under the ceiling
    pub capped: bool,
}

/// The gain a levelled conversion applies: the same gain the apps get in `replayGain` (tags first, else measured),
/// the album's when `album` and the song has one, else its own; None when nothing is known (it's converted as it is).
/// Capped by the peak so nothing clips: turned up at most until the peak reaches the ceiling, and never turned up
/// when the peak isn't known (a gain tag without a peak tag). The album's gain is capped by the album's peak (every
/// song of the album the same, as a player does), else the song's.
pub fn conversion_gain(l: &Levels, album: bool) -> Option<Applied> {
    let rg = replay_gain(l);
    let num = |k: &str| rg.get(k).and_then(Value::as_f64);
    let (gain, peak, album) = match (album, num("albumGain")) {
        (true, Some(g)) => (g, num("albumPeak").or(num("trackPeak")), true),
        _ => (num("trackGain")?, num("trackPeak"), false),
    };
    if !gain.is_finite() {
        return None;
    }
    let most = match peak {
        Some(p) if p > 0.0 => 20.0 * (CEILING / p).log10(),
        _ => 0.0,
    };
    let applied = (gain.min(most) * 100.0).round() / 100.0;
    Some(Applied { gain: applied, album, capped: applied < gain - 0.005 })
}

// ───────────── how far it is ─────────────

/// Songs that need measuring (no track or album gain tag), present. (Only the track table has these columns.)
const NEEDS: &str = "missing_since IS NULL AND (rg_track_gain IS NULL OR rg_album_gain IS NULL)";

/// For Podium: how many of the songs without tags are measured ("measured 812 of 1676").
pub async fn status(st: &AppState) -> Value {
    let q = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(lm_size = size AND lm_mtime = mtime), 0),
                COALESCE(SUM(lm_size = size AND lm_mtime = mtime AND lm_loudness IS NULL), 0)
           FROM track WHERE {NEEDS}"
    );
    let (songs, measured, failed): (i64, i64, i64) =
        sqlx::query_as(sqlx::AssertSqlSafe(q)).fetch_one(&st.db).await.unwrap_or((0, 0, 0));
    json!({ "on": st.cfg.loudness_analysis, "songs": songs, "measured": measured, "unreadable": failed })
}

// ───────────── measuring ─────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    /// integrated loudness, LUFS
    pub loudness: f64,
    /// true peak, linear
    pub peak: f64,
}

/// ffmpeg at the lowest priority there is: below everything else on the computer, conversions too.
pub(crate) fn ffmpeg(cfg: &Config) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(cfg.ffmpeg());
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        // in the child, before ffmpeg starts (so its threads inherit it): nice 19, and on Linux the disk only when
        // nothing else wants it (a spinning USB drive stays quick for songs being played)
        cmd.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 19);
            #[cfg(target_os = "linux")]
            {
                const IOPRIO_WHO_PROCESS: libc::c_long = 1;
                const IOPRIO_CLASS_IDLE: libc::c_long = 3;
                libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0 as libc::c_long, IOPRIO_CLASS_IDLE << 13);
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        const IDLE_PRIORITY_CLASS: u32 = 0x0000_0040;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(IDLE_PRIORITY_CLASS | CREATE_NO_WINDOW);
    }
    cmd
}

/// Why a song couldn't be measured.
#[derive(Debug)]
pub enum Failed {
    /// ffmpeg isn't there (or can't be started): nothing can be measured
    NoFfmpeg(String),
    /// this file: damaged, not audio, too long
    File(String),
}

/// Measure one file: its integrated loudness and true peak.
pub async fn measure(cfg: &Config, path: &Path) -> Result<Measurement, Failed> {
    let mut cmd = ffmpeg(cfg);
    cmd.args(["-nostdin", "-hide_banner", "-nostats", "-v", "info", "-threads", "1", "-filter_threads", "1", "-i"]);
    cmd.arg(path);
    cmd.args(["-map", "0:a:0", "-vn", "-sn", "-dn", "-af", "ebur128=peak=true:framelog=verbose", "-f", "null", "-"]);
    let mut child = cmd.spawn().map_err(|e| Failed::NoFfmpeg(format!("{}: {e}", cfg.ffmpeg().display())))?;
    let mut err = child.stderr.take().expect("piped");
    let run = async {
        let mut out = Vec::new();
        // read as it comes (a full pipe would stop ffmpeg); only the start and the end are kept
        let mut buf = vec![0u8; 64 << 10];
        let mut total = 0u64;
        loop {
            let n = err.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            total += n as u64;
            out.extend_from_slice(&buf[..n]);
            if out.len() as u64 > MOST_OUTPUT {
                out.drain(..out.len() - (MOST_OUTPUT as usize / 2));
            }
        }
        let status = child.wait().await;
        (out, status, total)
    };
    let (out, status, _) = tokio::time::timeout(MOST_PER_SONG, run).await.map_err(|_| Failed::File("took too long".into()))?;
    let text = String::from_utf8_lossy(&out);
    match status {
        Ok(s) if s.success() => parse(&text).ok_or_else(|| Failed::File("no loudness in ffmpeg's summary".into())),
        Ok(s) => Err(Failed::File(format!("ffmpeg {s}: {}", last_line(&text)))),
        Err(e) => Err(Failed::File(e.to_string())),
    }
}

fn last_line(s: &str) -> &str {
    s.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// The loudness and true peak from the ebur128 filter's summary:
///
/// ```text
///   Integrated loudness:
///     I:         -25.2 LUFS
///   ...
///   True peak:
///     Peak:      -14.1 dBFS
/// ```
fn parse(out: &str) -> Option<Measurement> {
    let s = &out[out.rfind("Summary:")?..];
    let value = |section: &str, key: &str| -> Option<f64> {
        let rest = &s[s.find(section)? + section.len()..];
        let rest = &rest[rest.find(key)? + key.len()..];
        rest.split_whitespace().next()?.parse::<f64>().ok() // ("-inf" for silence parses too)
    };
    let loudness = value("Integrated loudness:", "I:")?;
    let peak_db = value("True peak:", "Peak:")?;
    let peak = if peak_db.is_finite() { 10f64.powf(peak_db / 20.0) } else { 0.0 };
    (!loudness.is_nan()).then_some(Measurement { loudness, peak })
}

/// The file's size and time as the scanner keeps them (seconds).
fn stat(path: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some((md.len() as i64, mtime))
}

// ───────────── albums ─────────────

/// An album's loudness from its songs' (length in ms, loudness, peak): energy-weighted by length (silent songs left
/// out), and the highest peak.
pub fn album_level(songs: &[(i64, f64, f64)]) -> Option<(f64, f64)> {
    let (mut energy, mut secs, mut top) = (0.0, 0.0, 0.0f64);
    for &(ms, l, p) in songs {
        if !l.is_finite() || l <= QUIETEST_LUFS {
            continue;
        }
        let w = (ms.max(1000) as f64) / 1000.0;
        energy += w * 10f64.powf(l / 10.0);
        secs += w;
        top = top.max(p);
    }
    (secs > 0.0).then(|| (10.0 * (energy / secs).log10(), top))
}

/// Work out the albums' measured levels again (all albums, or only these) and write those that changed onto their
/// songs.
async fn refresh_albums(db: &Db, only: Option<&HashSet<String>>) -> Result<usize> {
    let rows: Vec<(String, i64, f64, Option<f64>)> = sqlx::query_as(
        "SELECT album_id, duration_ms, lm_loudness, lm_peak FROM track WHERE missing_since IS NULL AND lm_loudness IS NOT NULL",
    )
    .fetch_all(db)
    .await?;
    let mut by: HashMap<String, Vec<(i64, f64, f64)>> = HashMap::new();
    for (a, ms, l, p) in rows {
        if only.is_none_or(|o| o.contains(&a)) {
            by.entry(a).or_default().push((ms, l, p.unwrap_or(0.0)));
        }
    }
    let now: Vec<(String, Option<f64>, Option<f64>)> =
        sqlx::query_as("SELECT album_id, MAX(lm_album_loudness), MAX(lm_album_peak) FROM track GROUP BY album_id").fetch_all(db).await?;
    let close = |a: Option<f64>, b: Option<f64>| match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 0.005,
        (None, None) => true,
        _ => false,
    };
    let mut changed = 0;
    for (album, was_l, was_p) in now {
        if only.is_some_and(|o| !o.contains(&album)) {
            continue;
        }
        let (l, p) = match by.get(&album).and_then(|s| album_level(s)) {
            Some((l, p)) => (Some(l), Some(p)),
            None => (None, None),
        };
        if close(l, was_l) && close(p, was_p) {
            continue;
        }
        sqlx::query("UPDATE track SET lm_album_loudness = ?, lm_album_peak = ? WHERE album_id = ?")
            .bind(l)
            .bind(p)
            .bind(&album)
            .execute(db)
            .await?;
        changed += 1;
    }
    Ok(changed)
}

// ───────────── the worker ─────────────

/// What a pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Pass {
    /// songs that needed measuring when it started
    pub due: usize,
    pub measured: usize,
    /// couldn't be read (marked, not tried again until the file changes)
    pub unreadable: usize,
}

/// Measure every song due now: no gain tags, never measured or measured before its file changed. Returns when
/// they're all done (or ffmpeg isn't there).
pub async fn pass(st: &AppState) -> Result<Pass> {
    refresh_albums(&st.db, None).await?;
    let online: HashSet<i64> = st.libs.iter().filter(|(_, l)| library::is_online(l)).map(|(id, _)| *id).collect();
    let q = format!(
        "SELECT t.id, t.library_id, t.rel_path, t.album_id, t.lm_size, t.lm_mtime FROM track t JOIN album al ON al.id = t.album_id
          WHERE {NEEDS} AND (t.lm_size IS NULL OR t.lm_size != t.size OR t.lm_mtime != t.mtime)
          ORDER BY al.created_at DESC, t.album_id, t.disc, t.number"
    );
    type Due = (String, i64, String, String, Option<i64>, Option<i64>);
    let due: Vec<Due> = sqlx::query_as(sqlx::AssertSqlSafe(q)).fetch_all(&st.db).await?;
    let due: Vec<Due> = due.into_iter().filter(|d| online.contains(&d.1)).collect();
    let mut rep = Pass { due: due.len(), ..Pass::default() };
    if due.is_empty() {
        return Ok(rep);
    }
    let total = status(st).await["songs"].as_i64().unwrap_or(0);
    let threads = st.cfg.loudness_threads.clamp(1, 64);
    info!(songs = due.len(), threads, "sound levelling: measuring the songs without ReplayGain tags");
    let started = Instant::now();
    let (done, unreadable, no_ffmpeg) = (AtomicUsize::new(0), AtomicUsize::new(0), std::sync::Mutex::new(None::<String>));
    let last_log = std::sync::Mutex::new(Instant::now());
    let busy = std::sync::Mutex::new(Duration::ZERO);
    futures_util::stream::iter(due)
        .for_each_concurrent(threads, |(id, lib, rel, album, lm_size, lm_mtime)| {
            let (done, unreadable, no_ffmpeg, last_log, busy) = (&done, &unreadable, &no_ffmpeg, &last_log, &busy);
            async move {
                if no_ffmpeg.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                    return;
                }
                // never while a scan reads the files
                while st.scan.scanning.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                let Some(path) = library::track_path(&st.libs, lib, &rel) else { return };
                let Some(before) = stat(&path) else { return }; // gone, or the drive went away: not a failure
                if (Some(before.0), Some(before.1)) == (lm_size, lm_mtime) {
                    return; // measured as it is (the database's size or time is stale until the next scan)
                }
                let t0 = Instant::now();
                let r = measure(&st.cfg, &path).await;
                let took = t0.elapsed();
                *busy.lock().unwrap_or_else(|e| e.into_inner()) += took;
                if stat(&path) != Some(before) {
                    return; // changed (or went away) while it was measured: next time
                }
                let (l, p) = match r {
                    Ok(m) => (Some(m.loudness), Some(m.peak)),
                    Err(Failed::NoFfmpeg(e)) => {
                        *no_ffmpeg.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
                        return;
                    }
                    Err(Failed::File(e)) => {
                        warn!(file = %path.display(), "sound levelling: couldn't measure this song ({e})");
                        unreadable.fetch_add(1, Ordering::Relaxed);
                        (None, None)
                    }
                };
                let w = sqlx::query("UPDATE track SET lm_loudness = ?, lm_peak = ?, lm_size = ?, lm_mtime = ? WHERE id = ?")
                    .bind(l)
                    .bind(p)
                    .bind(before.0)
                    .bind(before.1)
                    .bind(&id)
                    .execute(&st.db)
                    .await;
                if let Err(e) = w {
                    warn!("sound levelling: couldn't save a measurement: {e}");
                    return;
                }
                let _ = refresh_albums(&st.db, Some(&HashSet::from([album]))).await;
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                debug!(file = %path.display(), loudness = ?l, ms = took.as_millis() as u64, "measured");
                let say = {
                    let mut last = last_log.lock().unwrap_or_else(|e| e.into_inner());
                    let due = last.elapsed() >= Duration::from_secs(300);
                    if due {
                        *last = Instant::now();
                    }
                    due
                };
                if say {
                    let each = busy.lock().unwrap_or_else(|e| e.into_inner()).as_secs_f64() / n as f64;
                    let m = status(st).await;
                    info!("sound levelling: measured {} of {total} songs without ReplayGain tags ({each:.1} s a song)", m["measured"]);
                }
                // a rest, half as long as that song took: gentle on a small server's cores (and fans)
                tokio::time::sleep(took / 2).await;
            }
        })
        .await;
    if let Some(e) = no_ffmpeg.into_inner().unwrap_or_else(|e| e.into_inner()) {
        bail!("ffmpeg isn't there ({e}): songs without ReplayGain tags aren't levelled");
    }
    refresh_albums(&st.db, None).await?;
    rep.measured = done.into_inner();
    rep.unreadable = unreadable.into_inner();
    let busy = busy.into_inner().unwrap_or_else(|e| e.into_inner());
    if rep.measured > 0 {
        info!(
            measured = rep.measured,
            unreadable = rep.unreadable,
            "sound levelling: done in {:.0} s ({:.2} s a song measuring)",
            started.elapsed().as_secs_f64(),
            busy.as_secs_f64() / rep.measured as f64
        );
    }
    Ok(rep)
}

/// Started with the server: a pass now and then, while `loudness_analysis` is on; then, in the same round and as
/// gently, what songs sound like (`audio_analysis`, crate::audio), at most `audio::ROUND` at a time so that new songs
/// are levelled first. One after the other, never both at once.
pub async fn worker(st: Arc<AppState>) {
    if !st.cfg.loudness_analysis && !st.cfg.audio_analysis {
        return;
    }
    tokio::time::sleep(START_DELAY).await;
    loop {
        while st.scan.scanning.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        let mut rest = LOOK_EVERY;
        if st.cfg.loudness_analysis
            && let Err(e) = pass(&st).await
        {
            warn!("{e:#}");
            // (ffmpeg missing: look again in an hour, it may have been installed)
            rest = Duration::from_secs(3600);
        }
        let round = crate::audio::Budget { songs: usize::MAX, time: crate::audio::ROUND };
        if rest == LOOK_EVERY
            && let Err(e) = crate::audio::pass(&st, round).await
        {
            warn!("{e:#}");
            rest = Duration::from_secs(3600);
        }
        tokio::time::sleep(rest).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "[Parsed_ebur128_0 @ 0x7f20e4004980] Summary:

  Integrated loudness:
    I:         -25.2 LUFS
    Threshold: -35.2 LUFS

  Loudness range:
    LRA:         0.1 LU
    Threshold: -45.2 LUFS
    LRA low:   -25.2 LUFS
    LRA high:  -25.1 LUFS

  True peak:
    Peak:      -14.1 dBFS
[out#0/null @ 0x5648654c2440] video:0KiB audio:41344KiB";

    #[test]
    fn reads_ffmpegs_summary() {
        let m = parse(OUT).unwrap();
        assert_eq!(m.loudness, -25.2);
        assert!((m.peak - 0.1972).abs() < 0.001, "{}", m.peak);
        let silent = OUT.replace("-25.2 LUFS\n    Threshold: -35.2", "-70.0 LUFS\n    Threshold: -80.0").replace("-14.1 dBFS", "-inf dBFS");
        let m = parse(&silent).unwrap();
        assert_eq!((m.loudness, m.peak), (-70.0, 0.0));
        assert_eq!(gain(Some(m.loudness)), None, "silence isn't levelled");
        assert!(parse("Input #0, flac, from 'x.flac':").is_none());
        assert!(parse(&OUT.replace("Peak:", "Pk:")).is_none());
        // the per-frame lines before the summary (framelog at "info") don't confuse it
        let framed = format!("[Parsed_ebur128_0 @ 0x1] t: 0.1 TARGET:-23 LUFS M: -30.0 S: -40.0 I: -12.0 LUFS\n{OUT}");
        assert_eq!(parse(&framed).unwrap().loudness, -25.2);
    }

    #[test]
    fn gain_is_from_the_reference() {
        assert_eq!(gain(Some(-18.0)), Some(0.0));
        assert_eq!(gain(Some(-8.3)), Some(-9.7));
        assert_eq!(gain(Some(-25.25)), Some(7.25));
        assert_eq!(gain(Some(-50.0)), Some(MOST_BOOST_DB), "a very quiet song is turned up only so far");
        assert_eq!(gain(None), None);
        assert_eq!(gain(Some(f64::NEG_INFINITY)), None);
    }

    #[test]
    fn album_level_is_energy_weighted_by_length() {
        // the same loudness: the album's is that
        let (l, p) = album_level(&[(200_000, -10.0, 0.5), (100_000, -10.0, 0.9)]).unwrap();
        assert!((l + 10.0).abs() < 1e-9 && p == 0.9);
        // two songs as long as each other, 10 dB apart: the energy mean is 10·log10((1 + 0.1) / 2) above the
        // quieter one's plus 10, i.e. −12.6, nearer the loud one than the average (−15)
        let (l, _) = album_level(&[(180_000, -10.0, 0.5), (180_000, -20.0, 0.1)]).unwrap();
        assert!((l - (-12.596)).abs() < 0.01, "{l}");
        // a long quiet song counts for more than a short one
        let (long, _) = album_level(&[(60_000, -10.0, 0.5), (600_000, -20.0, 0.1)]).unwrap();
        assert!(long < -15.0, "{long}");
        // silence (a hidden-track gap) is left out
        let (l, _) = album_level(&[(180_000, -10.0, 0.5), (600_000, -70.0, 0.0)]).unwrap();
        assert!((l + 10.0).abs() < 1e-9);
        assert_eq!(album_level(&[(1000, -70.0, 0.0)]), None);
        assert_eq!(album_level(&[]), None);
    }

    #[test]
    fn a_levelled_conversion_never_clips() {
        // measured: a loud song turned down, its own gain (track) or its album's
        let loud = Levels { loudness: Some(-8.0), peak: Some(0.9), album_loudness: Some(-10.0), album_peak_measured: Some(0.95), ..Default::default() };
        assert_eq!(conversion_gain(&loud, false), Some(Applied { gain: -10.0, album: false, capped: false }));
        assert_eq!(conversion_gain(&loud, true), Some(Applied { gain: -8.0, album: true, capped: false }));
        // album, but the song has no album gain: its own
        let alone = Levels { loudness: Some(-8.0), peak: Some(0.9), ..Default::default() };
        assert_eq!(conversion_gain(&alone, true), Some(Applied { gain: -10.0, album: false, capped: false }));
        // a quiet song with a high peak: turned up only until its peak is at −1 dB
        let quiet = Levels { loudness: Some(-26.0), peak: Some(0.5), ..Default::default() };
        let a = conversion_gain(&quiet, false).unwrap();
        assert!(a.capped && (a.gain - 5.02).abs() < 0.01, "{a:?}");
        assert!(0.5 * 10f64.powf(a.gain / 20.0) <= CEILING + 1e-3);
        // a gain tag without a peak: turned down as it says, never up
        let tagged_down = Levels { track_gain: Some(-4.5), ..Default::default() };
        assert_eq!(conversion_gain(&tagged_down, false), Some(Applied { gain: -4.5, album: false, capped: false }));
        let tagged_up = Levels { track_gain: Some(3.0), ..Default::default() };
        assert_eq!(conversion_gain(&tagged_up, false), Some(Applied { gain: 0.0, album: false, capped: true }));
        // tags win over measurements, as in replayGain
        let both = Levels { track_gain: Some(-2.0), track_peak: Some(0.5), ..loud };
        assert_eq!(conversion_gain(&both, false).unwrap().gain, -2.0);
        // nothing known: not levelled
        assert_eq!(conversion_gain(&Levels::default(), false), None);
        assert_eq!(conversion_gain(&Levels::default(), true), None);
    }

    #[test]
    fn tags_win_and_measurements_fill_the_rest() {
        let measured = Levels { loudness: Some(-8.0), peak: Some(0.98), album_loudness: Some(-9.0), album_peak_measured: Some(1.02), ..Default::default() };
        let rg = replay_gain(&measured);
        assert_eq!(Value::Object(rg), json!({ "trackGain": -10.0, "trackPeak": 0.98, "albumGain": -9.0, "albumPeak": 1.02 }));
        let tagged = Levels { track_gain: Some(-6.5), track_peak: Some(0.7), ..measured };
        let rg = replay_gain(&tagged);
        assert_eq!(Value::Object(rg), json!({ "trackGain": -6.5, "trackPeak": 0.7, "albumGain": -9.0, "albumPeak": 1.02 }));
        // a track gain tag without a peak: the measured peak (a fact about the file, whatever the gain's reference)
        let rg = replay_gain(&Levels { track_gain: Some(-6.5), ..measured });
        assert_eq!(rg["trackPeak"], 0.98);
        // nothing known: an empty object (as before)
        assert!(replay_gain(&Levels::default()).is_empty());
        // silence measured: no gain, and no peak of 0 either
        assert!(replay_gain(&Levels { loudness: Some(-70.0), peak: Some(0.0), ..Default::default() }).is_empty());
    }
}
