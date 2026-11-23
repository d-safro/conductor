//! Conversions with ffmpeg, for apps that ask for another format or a lower bitrate.
//!
//! Each conversion is written to a file in the cache (`<cache>/transcode/`) while the app is already sent what's
//! there so far, so ffmpeg runs at full speed for a second or two and is done, however slowly the app listens. The
//! same song in the same format and bitrate is then sent from the cache (with seeking), until the source file
//! changes (its size and time are in the name) or the cache grows past `transcode_cache_mb` (the least recently used
//! go first).
//!
//! `transcode_slots` ffmpegs (half the cores, at least 2) run at once without a second thought; each uses ~1.2 cores
//! and ~50 MB while it works. When they're all busy, a stream waits half a second for one, then starts anyway, up
//! to four times as many (at least 8): nobody waits seconds for someone else's song, and a crowd starting songs at
//! once still can't take all the memory of a small server. ffmpeg runs at a lower priority than everything else on
//! the machine, so the other services stay quick while it's busy.
//!
//! A second request for a conversion that's being made (two devices, an app that asks twice) follows the one that's
//! running instead of starting another ffmpeg; the conversion goes on to the end while anyone listens, and a few
//! seconds after the last one stops (an app seeking or asking again in the meantime follows it). Once nobody has
//! listened for `ABANDONED` it's stopped and nothing is kept: someone skipping through songs on a small server left
//! a conversion of every song skipped running to its end, and the song they stayed on started seconds late (a
//! Raspberry Pi 4, eight songs skipped a second apart: 4.1 s to the first byte instead of 0.5). With
//! `transcode_cache_mb = 0` nothing is kept: each conversion is sent from its open file and the file is gone once
//! it's done.
//!
//! A stream from the middle of a song (`timeOffset`) is converted straight to the app, not cached.
//!
//! Levelled (`level_conversions`, for apps that don't level songs themselves): the gain is applied with ffmpeg's
//! `volume` filter (already capped by the song's peak, see `loudness::conversion_gain`), the copy's own ReplayGain
//! tags are left out (it's levelled already), and the gain is part of the cache file's name: a levelled and an
//! unlevelled copy of a song are different files, and an unlevelled one keeps the name it always had.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use sha1::{Digest, Sha1};
use tokio::io::AsyncReadExt;

use crate::config::Config;

/// How long a stream waits for an ffmpeg to be free before it's turned away (all of them busy, the hard limit).
const WAIT_FOR_SLOT: Duration = Duration::from_secs(30);
/// How long it waits for one of the usual slots before it starts anyway (under the hard limit).
const WAIT_FOR_USUAL: Duration = Duration::from_millis(500);

/// How long a conversion goes on once nobody listens to it any more (an app seeking asks again within a second).
const ABANDONED: Duration = Duration::from_secs(3);

/// The conversions being made into the cache: cache file → (its part file, its state, who listens).
type Running = HashMap<PathBuf, (PathBuf, Arc<AtomicU8>, Arc<Listeners>)>;

/// How many are listening to a conversion being made, and since when nobody is.
pub struct Listeners {
    count: AtomicUsize,
    alone_since: Mutex<std::time::Instant>,
}

impl Listeners {
    fn new() -> Arc<Self> {
        Arc::new(Self { count: AtomicUsize::new(0), alone_since: Mutex::new(std::time::Instant::now()) })
    }

    /// One more (counted while the map's lock is held, so it can't be stopped in between).
    fn join(self: &Arc<Self>) -> Listener {
        self.count.fetch_add(1, Ordering::SeqCst);
        Listener(self.clone())
    }

    /// Nobody has listened for `ABANDONED`.
    fn abandoned(&self) -> bool {
        self.count.load(Ordering::SeqCst) == 0 && self.alone_since.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= ABANDONED
    }
}

/// Someone listening to a conversion being made (until it's dropped: the app hung up, or the answer is sent).
pub struct Listener(Arc<Listeners>);

impl Listener {
    /// A listener of nothing (for a conversion that isn't watched).
    pub fn none() -> Self {
        Listeners::new().join()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let mut since = self.0.alone_since.lock().unwrap_or_else(|e| e.into_inner());
        if self.0.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            *since = std::time::Instant::now();
        }
    }
}

/// The ffmpegs that may run: the usual number, and the most there may be (for memory); and the conversions being
/// made for the cache now (cache file → its part file and state), for a second request to follow.
pub struct Slots {
    usual: Arc<tokio::sync::Semaphore>,
    most: Arc<tokio::sync::Semaphore>,
    running: Arc<Mutex<Running>>,
    started: AtomicUsize,
}

/// A running ffmpeg's place (given back when it ends).
pub struct Place(#[allow(dead_code)] tokio::sync::OwnedSemaphorePermit, #[allow(dead_code)] Option<tokio::sync::OwnedSemaphorePermit>);

impl Slots {
    pub fn new(usual: usize) -> Self {
        Self {
            usual: Arc::new(tokio::sync::Semaphore::new(usual)),
            most: Arc::new(tokio::sync::Semaphore::new((usual * 4).max(8))),
            running: Default::default(),
            started: AtomicUsize::new(0),
        }
    }

    /// How many ffmpegs were started for the cache (each conversion once, however many asked for it).
    pub fn started(&self) -> usize {
        self.started.load(Ordering::Relaxed)
    }

    /// The conversion being made into `file`, opened to follow it.
    fn follow_running(&self, file: &Path) -> Option<(tokio::fs::File, Arc<AtomicU8>, Listener)> {
        let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let (part, state, listeners) = running.get(file)?;
        // (under the lock: the part is moved into place or removed only after it's taken out of the map)
        let f = std::fs::File::open(part).ok()?;
        Some((tokio::fs::File::from_std(f), state.clone(), listeners.join()))
    }

    async fn take(&self) -> Result<Place> {
        let most = tokio::time::timeout(WAIT_FOR_SLOT, self.most.clone().acquire_owned())
            .await
            .map_err(|_| anyhow!("The server is busy converting songs; try again in a moment"))??;
        let usual = tokio::time::timeout(WAIT_FOR_USUAL, self.usual.clone().acquire_owned()).await.ok().and_then(Result::ok);
        Ok(Place(most, usual))
    }
}

const RUNNING: u8 = 0;
const DONE: u8 = 1;
const FAILED: u8 = 2;

#[derive(Clone, Copy)]
pub struct Target {
    pub codec: &'static str,
    pub container: &'static str,
    pub content_type: &'static str,
    pub suffix: &'static str,
}

pub enum Converted {
    /// in the cache: send the file (ranges and all); true when it was there before this request
    Cached(PathBuf, bool),
    /// being written: send it as it grows (listening to it while it's sent)
    Growing(tokio::fs::File, Arc<AtomicU8>, Listener),
    /// straight from ffmpeg (from the middle of a song)
    Piped(tokio::process::ChildStdout, Vec<u8>),
}

/// The ffmpeg command, at a lower priority than everything else on this computer.
fn ffmpeg(cfg: &Config) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(cfg.ffmpeg());
    cmd.args(["-v", "error", "-nostdin"]).stdin(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    unsafe {
        // in the child, before ffmpeg starts (so its threads inherit it): nice 10
        cmd.pre_exec(|| {
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

/// ReplayGain tags, left out of a levelled copy (an app or a car stereo reading them would level it again).
const GAIN_TAGS: &[&str] =
    &["REPLAYGAIN_TRACK_GAIN", "REPLAYGAIN_TRACK_PEAK", "REPLAYGAIN_ALBUM_GAIN", "REPLAYGAIN_ALBUM_PEAK", "R128_TRACK_GAIN", "R128_ALBUM_GAIN"];

fn args(cmd: &mut tokio::process::Command, src: &Path, x: Target, kbps: i64, gain: Option<f64>) {
    cmd.arg("-i").arg(src);
    cmd.args(["-map", "0:a:0", "-vn"]);
    if let Some(g) = gain {
        cmd.args(["-af", &format!("volume={g:.2}dB")]);
        for t in GAIN_TAGS {
            // (the file's tags, and an Ogg file's, which are the stream's)
            cmd.args(["-metadata", &format!("{t}="), "-metadata:s:a:0", &format!("{t}=")]);
        }
    }
    // Vorbis and Opus take only some bitrates for some channels and sample rates: Vorbis none at all for a 96 kHz
    // FLAC, nor 320k for a mono song or 32k for a stereo one; Opus at most 256k for mono. ffmpeg then failed and the
    // song didn't play. Given stereo (Vorbis at 44.1 kHz, between 48k and 480k), every song converts.
    let kbps = match x.codec {
        "libvorbis" => {
            cmd.args(["-ac", "2", "-ar", "44100"]);
            kbps.clamp(48, 480)
        }
        "libopus" => {
            cmd.args(["-ac", "2"]);
            kbps
        }
        _ => kbps,
    };
    // each packet written as it's made: by itself ffmpeg kept a few hundred KB before its first write, so an app
    // waited for 15 s or so of the song to be converted before it got anything (2 s on a Raspberry Pi 4 core, 0.9 s
    // with this; 8 apps converting at once on it: seconds more)
    cmd.args(["-c:a", x.codec, "-b:a", &format!("{kbps}k"), "-f", x.container, "-flush_packets", "1"]);
}

fn dir(cfg: &Config) -> PathBuf {
    cfg.cache_dir().join("transcode")
}

/// The cache file for this song (as its file is now), format, bitrate and gain (levelled).
fn cache_file(cfg: &Config, src: &Path, x: Target, kbps: i64, gain: Option<f64>) -> PathBuf {
    let meta = std::fs::metadata(src).ok();
    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = meta.and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    let mut h = Sha1::new();
    h.update(src.to_string_lossy().as_bytes());
    h.update(format!("|{size}|{mtime}|{}|{kbps}", x.suffix).as_bytes());
    if let Some(g) = gain {
        h.update(format!("|level {g:.2}").as_bytes());
    }
    dir(cfg).join(format!("{}.{}", hex::encode(&h.finalize()[..12]), x.suffix))
}

/// A converted copy of `src` (levelled by `gain` dB when given): from the cache, being written to it now, or (from
/// `offset` seconds) straight from ffmpeg. An error when ffmpeg can't read the file (damaged, not audio, an offset
/// past the end): the app gets that, not an empty song that "plays" as silence.
pub async fn convert(cfg: &Config, slots: &Slots, src: &Path, x: Target, kbps: i64, gain: Option<f64>, offset: i64) -> Result<Converted> {
    if offset == 0 {
        let file = cache_file(cfg, src, x, kbps, gain);
        if let Some(hit) = cached(&file) {
            return Ok(hit);
        }
        // being made already (another app, the same app asking twice): follow that one
        if let Some((f, state, l)) = slots.follow_running(&file) {
            return first_bytes(f, state, l, &file).await;
        }
        let permit = slots.take().await?;
        std::fs::create_dir_all(dir(cfg))?;
        let started = {
            let mut running = slots.running.lock().unwrap_or_else(|e| e.into_inner());
            // while this one waited for a slot, another may have started it, or finished it
            if running.contains_key(&file) || file.is_file() {
                drop(permit); // (not needed: following takes none)
                None
            } else {
                Some(start(cfg, &mut running, slots, permit, src, x, kbps, gain, &file)?)
            }
        };
        return match started {
            Some((reader, state, l)) => first_bytes(reader, state, l, &file).await,
            None => match (cached(&file), slots.follow_running(&file)) {
                (Some(hit), _) => Ok(hit),
                (None, Some((f, state, l))) => first_bytes(f, state, l, &file).await,
                // (it failed and is gone: as if it had failed for this one)
                (None, None) => bail!("This song can't be transcoded (ffmpeg couldn't read it)"),
            },
        };
    }
    // from the middle of a song: straight to the app (an ffmpeg slot while it runs)
    let permit = slots.take().await?;
    let mut cmd = ffmpeg(cfg);
    cmd.args(["-ss", &offset.to_string()]);
    args(&mut cmd, src, x, kbps, gain);
    cmd.arg("pipe:1").stdout(Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| anyhow!("ffmpeg can't start ({e}); is it installed?"))?;
    let mut out = child.stdout.take().ok_or_else(|| anyhow!("ffmpeg gave no output"))?;
    let mut first = vec![0u8; 64 * 1024];
    let n = out.read(&mut first).await.unwrap_or(0);
    if n == 0 {
        let _ = child.wait().await;
        bail!("This song can't be transcoded (ffmpeg couldn't read it)");
    }
    first.truncate(n);
    tokio::spawn(async move {
        let _ = child.wait().await; // ends when the app stops listening (ffmpeg gets a broken pipe)
        drop(permit);
    });
    Ok(Converted::Piped(out, first))
}

/// An ffmpeg started into the cache (the caller holds the map's lock): its part, opened, its state, and the caller
/// listening to it.
#[allow(clippy::too_many_arguments)]
fn start(
    cfg: &Config,
    running: &mut Running,
    slots: &Slots,
    permit: Place,
    src: &Path,
    x: Target,
    kbps: i64,
    gain: Option<f64>,
    file: &Path,
) -> Result<(tokio::fs::File, Arc<AtomicU8>, Listener)> {
    let part = file.with_extension(format!("{}.{}.part", x.suffix, crate::db::random_id()));
    // the part is made (and opened) here, and ffmpeg writes into it: what's sent is read from this open file,
    // which stays readable when the part is moved into the cache or (transcode_cache_mb = 0) removed
    let reader = std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&part)?;
    let mut cmd = ffmpeg(cfg);
    args(&mut cmd, src, x, kbps, gain);
    cmd.arg("-y").arg(&part).stdout(Stdio::null()).kill_on_drop(false);
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            bail!("ffmpeg can't start ({e}); is it installed?");
        }
    };
    slots.started.fetch_add(1, Ordering::Relaxed);
    let state = Arc::new(AtomicU8::new(RUNNING));
    let listeners = Listeners::new();
    let me = listeners.join();
    running.insert(file.to_path_buf(), (part.clone(), state.clone(), listeners.clone()));
    let (st2, part2, file2, limit) = (state.clone(), part.clone(), file.to_path_buf(), cfg.transcode_cache_mb);
    let (cache, map) = (dir(cfg), slots.running.clone());
    // (spawned: it runs to the end for whoever follows it, even when this request is dropped, until nobody has
    // listened for a few seconds)
    tokio::spawn(async move {
        let ok = loop {
            tokio::select! {
                s = child.wait() => break s.map(|s| s.success()).unwrap_or(false),
                _ = tokio::time::sleep(Duration::from_millis(500)) => {
                    // decided under the lock: nobody can start following it in between
                    let stop = {
                        let mut running = map.lock().unwrap_or_else(|e| e.into_inner());
                        let stop = listeners.abandoned();
                        if stop {
                            running.remove(&file2);
                        }
                        stop
                    };
                    if stop {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                        break false;
                    }
                }
            }
        };
        drop(permit); // the next ffmpeg may start
        let filled = std::fs::metadata(&part2).map(|m| m.len() > 0).unwrap_or(false);
        let kept = {
            let mut running = map.lock().unwrap_or_else(|e| e.into_inner());
            running.remove(&file2);
            ok && filled && limit > 0 && std::fs::rename(&part2, &file2).is_ok()
        };
        // ffmpeg stopped part way (killed, the disk full): the app is told the song broke off, rather than
        // sent its first half as if that were all of it
        st2.store(if ok && filled { DONE } else { FAILED }, Ordering::SeqCst);
        if kept {
            trim(&cache, limit);
        } else {
            // (failed, or nothing is kept: those sending it still have it open)
            let _ = std::fs::remove_file(&part2);
        }
    });
    Ok((tokio::fs::File::from_std(reader), state, me))
}

/// A conversion in the cache, marked as used again (the last to go when the cache is trimmed).
fn cached(file: &Path) -> Option<Converted> {
    if !file.is_file() {
        return None;
    }
    if let Ok(f) = std::fs::File::options().append(true).open(file) {
        let _ = f.set_modified(std::time::SystemTime::now());
    }
    Some(Converted::Cached(file.to_path_buf(), true))
}

/// A conversion being made, once it has its first bytes (or has failed): what's answered.
async fn first_bytes(f: tokio::fs::File, state: Arc<AtomicU8>, l: Listener, file: &Path) -> Result<Converted> {
    loop {
        let st = state.load(Ordering::SeqCst);
        // done already (a very short song) and in the cache: sent from there, with ranges
        if st == DONE && file.is_file() {
            return Ok(Converted::Cached(file.to_path_buf(), false));
        }
        if f.metadata().await.map(|m| m.len() > 0).unwrap_or(false) {
            return Ok(Converted::Growing(f, state, l));
        }
        match st {
            RUNNING => tokio::time::sleep(Duration::from_millis(15)).await,
            _ => bail!("This song can't be transcoded (ffmpeg couldn't read it)"),
        }
    }
}

/// A file being written by ffmpeg, as a stream: what's there, then waits for more until ffmpeg is done. Listening
/// to it until the stream is dropped (the app hung up) or ends.
pub fn follow(f: tokio::fs::File, state: Arc<AtomicU8>, l: Listener) -> impl futures_util::Stream<Item = std::io::Result<axum::body::Bytes>> {
    futures_util::stream::unfold(Some((f, state, l)), |s| async move {
        let (mut f, state, l) = s?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match f.read(&mut buf).await {
                Ok(0) if state.load(Ordering::SeqCst) == RUNNING => tokio::time::sleep(Duration::from_millis(20)).await,
                Ok(0) => {
                    // ffmpeg has ended: one more look, for what it wrote just before
                    return match f.read(&mut buf).await {
                        Ok(n) if n > 0 => {
                            buf.truncate(n);
                            Some((Ok(axum::body::Bytes::from(buf)), Some((f, state, l))))
                        }
                        // it failed part way: the answer breaks off (not a song that "ends" early)
                        _ if state.load(Ordering::SeqCst) == FAILED => {
                            Some((Err(std::io::Error::other("the conversion failed part way")), None))
                        }
                        _ => None,
                    };
                }
                Ok(n) => {
                    buf.truncate(n);
                    return Some((Ok(axum::body::Bytes::from(buf)), Some((f, state, l))));
                }
                Err(e) => return Some((Err(e), None)),
            }
        }
    })
}

/// The cache back under its size: the least recently used conversions go first.
fn trim(dir: &Path, limit_mb: u64) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().ends_with(".part"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            m.is_file().then(|| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len(), e.path()))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    let limit = limit_mb.saturating_mul(1024 * 1024);
    if total <= limit {
        return;
    }
    files.sort();
    for (_, len, p) in files {
        if total <= limit {
            break;
        }
        if std::fs::remove_file(&p).is_ok() {
            total -= len;
        }
    }
}

/// Conversions left half-written by a server that stopped (they're never finished).
pub fn clean_parts(cfg: &Config) {
    if let Ok(rd) = std::fs::read_dir(dir(cfg)) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().ends_with(".part") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_conversion_that_fails_part_way_breaks_off() {
        use futures_util::StreamExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.mp3.abc.part");
        std::fs::write(&p, b"half a song").unwrap();
        for (state, broke_off) in [(DONE, false), (FAILED, true)] {
            let f = tokio::fs::File::open(&p).await.unwrap();
            let got: Vec<_> = follow(f, Arc::new(AtomicU8::new(state)), Listener::none()).collect().await;
            assert_eq!(got[0].as_ref().unwrap().as_ref(), b"half a song");
            assert_eq!(got.len(), if broke_off { 2 } else { 1 });
            assert_eq!(got.last().unwrap().is_err(), broke_off);
        }
    }

    /// A conversion nobody listens to any more is stopped (and nothing kept); one someone listens to, or comes back
    /// to within a moment, goes on. (ffmpeg reading at the song's own speed, `-re`, so it's still going.)
    #[cfg(unix)]
    #[tokio::test]
    async fn a_conversion_nobody_listens_to_is_stopped() {
        use std::os::unix::fs::PermissionsExt;
        if !std::process::Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("song.flac");
        let made = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=30"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(made.success());
        let slow = d.path().join("slow-ffmpeg");
        std::fs::write(&slow, "#!/bin/sh\nexec ffmpeg -re \"$@\"\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = Config { data_dir: d.path().join("data"), ffmpeg: Some(slow), ..Config::default() };
        let slots = Slots::new(2);
        let mp3 = Target { codec: "libmp3lame", container: "mp3", content_type: "audio/mpeg", suffix: "mp3" };
        let running = |slots: &Slots| slots.running.lock().unwrap().len();
        let files = || std::fs::read_dir(dir(&cfg)).map(|rd| rd.count()).unwrap_or(0);

        // the first bytes come as soon as they're made (ffmpeg kept ~15 s of the song before its first write)
        let t0 = std::time::Instant::now();
        let first = convert(&cfg, &slots, &src, mp3, 128, None, 0).await.unwrap();
        assert!(matches!(first, Converted::Growing(..)));
        assert!(t0.elapsed() < Duration::from_secs(5), "the first bytes after {:?}", t0.elapsed());
        // listened to: it goes on past the moment
        tokio::time::sleep(ABANDONED + Duration::from_millis(1500)).await;
        assert_eq!(running(&slots), 1, "still listened to");
        // the app hangs up and asks again at once (seeking): the same conversion, followed
        drop(first);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let again = convert(&cfg, &slots, &src, mp3, 128, None, 0).await.unwrap();
        assert!(matches!(again, Converted::Growing(..)));
        assert_eq!(slots.started(), 1, "followed, not started again");
        tokio::time::sleep(ABANDONED + Duration::from_millis(1000)).await;
        assert_eq!(running(&slots), 1, "the follower listens");

        // nobody listens any more: stopped soon after (long before the song's end), nothing kept
        drop(again);
        let t0 = std::time::Instant::now();
        while running(&slots) > 0 {
            assert!(t0.elapsed() < ABANDONED + Duration::from_secs(3), "a conversion nobody listens to went on");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(files(), 0, "a stopped conversion leaves nothing in the cache");
        // asked for again later: converted again
        let later = convert(&cfg, &slots, &src, mp3, 128, None, 0).await.unwrap();
        assert!(matches!(later, Converted::Growing(..)));
        assert_eq!(slots.started(), 2);
    }

    #[test]
    fn the_cache_is_trimmed_least_recently_used_first() {
        let d = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now();
        for (name, age_s) in [("old.mp3", 300), ("mid.mp3", 200), ("new.mp3", 100), ("x.mp3.abc.part", 400)] {
            let p = d.path().join(name);
            std::fs::write(&p, vec![0u8; 600 * 1024]).unwrap();
            std::fs::File::options().append(true).open(&p).unwrap().set_modified(now - Duration::from_secs(age_s)).unwrap();
        }
        trim(d.path(), 1); // 1 MB: room for one 600 KB file
        let mut left: Vec<String> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        left.sort();
        assert_eq!(left, ["new.mp3", "x.mp3.abc.part"], "a conversion being written is never removed");
    }
}
