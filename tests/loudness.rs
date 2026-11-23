//! Sound levelling end to end: songs without ReplayGain tags are measured (tones at known levels), the apps get the
//! measured gain in `replayGain`, tags win, a rescan keeps what was measured and a changed file is measured again.

use std::{path::Path, process::Command, sync::Arc};

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library, loudness,
};
use serde_json::Value;

const USER: &str = "tester";
const PASS: &str = "s3cret pass";

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A stereo 1 kHz sine at `amp` of full scale: its loudness is 20·log10(amp) LUFS and its true peak `amp`.
fn sine(path: &Path, title: &str, album: &str, track: u32, amp: f64, tags: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    let s = format!("{amp}*sin(2*PI*1000*t)");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("aevalsrc={s}|{s}:s=44100:d=6")]);
    for (k, v) in [("title", title), ("artist", "Tester"), ("album", album), ("track", &track.to_string())].iter().chain(tags) {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    if path.extension().is_some_and(|x| x == "mp3") {
        cmd.args(["-b:a", "320k"]);
    }
    assert!(cmd.arg(path).status().unwrap().success(), "ffmpeg failed for {}", path.display());
}

/// The gain a sine at `amp` should get: −18 LUFS − its loudness.
fn expected_gain(amp: f64) -> f64 {
    -18.0 - 20.0 * amp.log10()
}

struct Server {
    base: String,
    http: reqwest::Client,
    st: Arc<AppState>,
    _dir: tempfile::TempDir,
    music: std::path::PathBuf,
}

impl Server {
    async fn start(make: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        make(&music);
        let cfg = Config {
            data_dir: dir.path().join("data"),
            libraries: vec![LibraryConfig { name: "Music".into(), path: music.clone(), sentinel: None, scan_threads: None }],
            catalog: false,
            lyrics_online: false,
            covers_fetch: false,
            concerts: false,
            info_online: false,
            deezer_url: "http://127.0.0.1:9".into(),
            ..Config::default()
        };
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        Server { base, http: reqwest::Client::new(), st, _dir: dir, music }
    }

    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{PASS}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", USER), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        let url = format!("{}/rest/{method}.view?{}", self.base, q.finish());
        let r: Value = self.http.get(url).send().await.unwrap().json().await.unwrap();
        let r = r["subsonic-response"].clone();
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }

    /// Every song's `replayGain`, by title.
    async fn gains(&self) -> std::collections::HashMap<String, Value> {
        let r = self.call("search3", &[("query", ""), ("songCount", "500"), ("albumCount", "0"), ("artistCount", "0")]).await;
        r["searchResult3"]["song"].as_array().unwrap().iter().map(|s| (s["title"].as_str().unwrap().to_string(), s["replayGain"].clone())).collect()
    }

    async fn rescan(&self, full: bool) {
        library::scan(&self.st.db, &self.st.libs, &self.st.scan, full).await.unwrap();
    }
}

fn near(v: &Value, want: f64, by: f64, what: &str) {
    let got = v.as_f64().unwrap_or_else(|| panic!("{what}: no number in {v}"));
    assert!((got - want).abs() <= by, "{what}: {got} is not within {by} of {want}");
}

/// A file changed later than the scanner saw it (a second's resolution).
fn touch_later(path: &Path) {
    let f = std::fs::File::options().append(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
}

#[tokio::test]
async fn songs_without_tags_are_measured_and_tags_win() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(|m| {
        // no tags: both measured, the album's gain from both
        sine(&m.join("A/Loud/01 Loud.flac"), "Loud", "Loud", 1, 0.5, &[]);
        sine(&m.join("A/Loud/02 Quiet.flac"), "Quiet", "Loud", 2, 0.1, &[]);
        // every tag: never measured, the tags as they are
        sine(&m.join("B/Tagged/01 Tagged.flac"), "Tagged", "Tagged", 1, 0.5, &[
            ("REPLAYGAIN_TRACK_GAIN", "-3.00 dB"),
            ("REPLAYGAIN_ALBUM_GAIN", "-4.00 dB"),
        ]);
        // a track gain tag but no album gain: measured, and only the album's gain (and the peak) come from that
        sine(&m.join("C/Half/01 Half.mp3"), "Half", "Half", 1, 0.25, &[("REPLAYGAIN_TRACK_GAIN", "-1.00 dB")]);
        // not audio
        std::fs::create_dir_all(m.join("D/Broken")).unwrap();
        std::fs::write(m.join("D/Broken/01 Broken.flac"), b"fLaC this isn't a song at all").unwrap();
    })
    .await;

    // before: only the tags
    let g = s.gains().await;
    assert_eq!(g["Loud"], serde_json::json!({}));
    assert_eq!(g["Tagged"], serde_json::json!({ "trackGain": -3.0, "albumGain": -4.0 }));

    let started = std::time::Instant::now();
    let rep = loudness::pass(&s.st).await.unwrap();
    let broken: Option<String> =
        sqlx::query_scalar("SELECT title FROM track WHERE rel_path LIKE '%Broken%'").fetch_optional(&s.st.db).await.unwrap();
    let broken_scanned = broken.is_some();
    assert_eq!(rep.due, 3 + broken_scanned as usize, "{rep:?}");
    assert_eq!(rep.measured, rep.due, "{rep:?}");
    assert_eq!(rep.unreadable, broken_scanned as usize, "{rep:?}");
    eprintln!("measured {} songs in {:?}", rep.measured, started.elapsed());

    let g = s.gains().await;
    near(&g["Loud"]["trackGain"], expected_gain(0.5), 0.5, "loud song's gain");
    near(&g["Loud"]["trackPeak"], 0.5, 0.02, "loud song's peak");
    near(&g["Quiet"]["trackGain"], expected_gain(0.1), 0.5, "quiet song's gain");
    near(&g["Quiet"]["trackPeak"], 0.1, 0.01, "quiet song's peak");
    // the album: both songs as long as each other, energy-averaged (−8.8 LUFS), the louder one's peak
    let album = -18.0 - 10.0 * ((0.5f64.powi(2) + 0.1f64.powi(2)) / 2.0).log10();
    for t in ["Loud", "Quiet"] {
        near(&g[t]["albumGain"], album, 0.5, "album gain");
        near(&g[t]["albumPeak"], 0.5, 0.02, "album peak");
    }
    // tags win
    assert_eq!(g["Tagged"], serde_json::json!({ "trackGain": -3.0, "albumGain": -4.0 }), "a song with every tag isn't measured");
    assert_eq!(g["Half"]["trackGain"], -1.0, "the track gain tag, not the measurement");
    near(&g["Half"]["albumGain"], expected_gain(0.25), 0.5, "the measured album gain where there's no tag");
    near(&g["Half"]["trackPeak"], 0.25, 0.03, "the measured peak where the tag has none");
    if let Some(b) = &broken {
        assert_eq!(g[b], serde_json::json!({}), "a file that can't be read has no levels");
    }
    // how far it is, for Podium
    let st = loudness::status(&s.st).await;
    assert_eq!((st["songs"].as_i64(), st["measured"].as_i64()), (Some(3 + broken_scanned as i64), Some(3 + broken_scanned as i64)), "{st}");

    // nothing more to do: not even the broken file again
    assert_eq!(loudness::pass(&s.st).await.unwrap().due, 0);

    // a rescan, a full one too, keeps the measurements
    s.rescan(false).await;
    s.rescan(true).await;
    assert_eq!(s.gains().await, g, "a rescan keeps what was measured");
    assert_eq!(loudness::pass(&s.st).await.unwrap().due, 0, "and nothing is measured again");

    // a file that changes is measured again (and its album with it)
    sine(&s.music.join("A/Loud/02 Quiet.flac"), "Quiet", "Loud", 2, 0.5, &[]);
    touch_later(&s.music.join("A/Loud/02 Quiet.flac"));
    s.rescan(false).await;
    let rep = loudness::pass(&s.st).await.unwrap();
    assert_eq!((rep.due, rep.measured), (1, 1), "{rep:?}");
    let g2 = s.gains().await;
    near(&g2["Quiet"]["trackGain"], expected_gain(0.5), 0.5, "the changed song, measured again");
    near(&g2["Loud"]["albumGain"], expected_gain(0.5), 0.5, "its album too");
    assert_eq!(g2["Loud"]["trackGain"], g["Loud"]["trackGain"], "the other song isn't measured again");

    // tags added later win at once (before any measuring)
    sine(&s.music.join("A/Loud/01 Loud.flac"), "Loud", "Loud", 1, 0.5, &[
        ("REPLAYGAIN_TRACK_GAIN", "1.23 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "0.400000"),
    ]);
    touch_later(&s.music.join("A/Loud/01 Loud.flac"));
    s.rescan(false).await;
    let g3 = s.gains().await;
    assert_eq!(g3["Loud"]["trackGain"], 1.23);
    assert_eq!(g3["Loud"]["trackPeak"], 0.4);
    near(&g3["Loud"]["albumGain"], expected_gain(0.5), 0.5, "no album gain tag: still the measured one");

    // the share page's songs get the same object
    let id: String = sqlx::query_scalar("SELECT id FROM track WHERE title = 'Quiet'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(loudness::replay_gain_of(&s.st.db, &id).await, g3["Quiet"]);
}

#[tokio::test]
async fn a_pass_stopped_part_way_goes_on_where_it_stopped() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(|m| {
        for i in 1..=6 {
            sine(&m.join(format!("A/Many/{i:02} Song {i}.flac")), &format!("Song {i}"), "Many", i, 0.05 * i as f64, &[]);
        }
    })
    .await;
    // stopped (as at shutdown: the task dropped) once something's measured
    let st = s.st.clone();
    let task = tokio::spawn(async move { loudness::pass(&st).await });
    let measured = async || -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE lm_size IS NOT NULL").fetch_one(&s.st.db).await.unwrap()
    };
    let t0 = std::time::Instant::now();
    while measured().await == 0 {
        assert!(t0.elapsed().as_secs() < 60, "nothing measured");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    task.abort();
    let _ = task.await;
    let before = measured().await;
    assert!(before < 6, "stopped before the end ({before})");
    // the next pass (after a restart) measures only the rest
    let rep = loudness::pass(&s.st).await.unwrap();
    assert_eq!(rep.due as i64, 6 - before, "{rep:?}");
    assert_eq!(measured().await, 6);
    let g = s.gains().await;
    for i in 1..=6 {
        near(&g[&format!("Song {i}")]["trackGain"], expected_gain(0.05 * i as f64), 0.5, "gain");
    }
}
