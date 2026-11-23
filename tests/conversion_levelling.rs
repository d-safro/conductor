//! Levelling in conversions (`level_conversions`), end to end: tones at known loudness are converted for an app that
//! doesn't level songs itself, and the converted copy is measured: it's at the target (−18 LUFS) within 1 dB, never
//! past the ceiling, kept apart from the unlevelled copy in the cache; off changes nothing, and a stream of the file
//! itself is never touched.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig, Levelling},
    db, library, loudness,
};
use serde_json::Value;

const USER: &str = "tester";
const PASS: &str = "s3cret pass";

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// A stereo 1 kHz sine at `amp` of full scale (its true peak is `amp`), with these tags.
fn sine(path: &Path, title: &str, album: &str, track: u32, amp: f64, tags: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    let s = format!("{amp}*sin(2*PI*1000*t)");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("aevalsrc={s}|{s}:s=44100:d=6")]);
    for (k, v) in [("title", title), ("artist", "Tester"), ("album", album), ("track", &track.to_string())].iter().chain(tags) {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success(), "ffmpeg failed for {}", path.display());
}

/// The test library: two songs of one album without tags (measured), one with ReplayGain tags, one whose tags say
/// to turn it up further than its peak allows.
fn library(m: &Path) {
    sine(&m.join("A/Pair/01 Loud.flac"), "Loud", "Pair", 1, 0.5, &[]);
    sine(&m.join("A/Pair/02 Quiet.flac"), "Quiet", "Pair", 2, 0.05, &[]);
    sine(&m.join("B/Tagged/01 Tagged.flac"), "Tagged", "Tagged", 1, 0.25, &[
        ("REPLAYGAIN_TRACK_GAIN", "-3.00 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "0.250000"),
        ("REPLAYGAIN_ALBUM_GAIN", "-4.00 dB"),
        ("REPLAYGAIN_ALBUM_PEAK", "0.250000"),
    ]);
    sine(&m.join("C/Boost/01 Boost.flac"), "Boost", "Boost", 1, 0.5, &[
        ("REPLAYGAIN_TRACK_GAIN", "10.00 dB"),
        ("REPLAYGAIN_TRACK_PEAK", "0.500000"),
        ("REPLAYGAIN_ALBUM_GAIN", "10.00 dB"),
        ("REPLAYGAIN_ALBUM_PEAK", "0.500000"),
    ]);
}

struct Server {
    base: String,
    http: reqwest::Client,
    st: Arc<AppState>,
    dir: tempfile::TempDir,
    music: PathBuf,
}

struct Stream {
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

impl Stream {
    fn header(&self, k: &str) -> Option<&str> {
        self.headers.get(k).and_then(|v| v.to_str().ok())
    }
}

impl Server {
    async fn start(tune: impl FnOnce(&mut Config)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        library(&music);
        let mut cfg = Config {
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
        tune(&mut cfg);
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        // the songs without tags measured (as the background worker does)
        let rep = loudness::pass(&st).await.unwrap();
        assert_eq!(rep.measured, 2, "{rep:?}");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        Server { base, http: reqwest::Client::new(), st, dir, music }
    }

    /// A call's URL, as the app `client` makes it.
    fn url(&self, client: &str, method: &str, params: &[(&str, &str)]) -> String {
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{PASS}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", USER), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", client), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        format!("{}/rest/{method}.view?{}", self.base, q.finish())
    }

    async fn call(&self, client: &str, method: &str, params: &[(&str, &str)]) -> Value {
        let r: Value = self.http.get(self.url(client, method, params)).send().await.unwrap().json().await.unwrap();
        let r = r["subsonic-response"].clone();
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }

    async fn id(&self, title: &str) -> String {
        sqlx::query_scalar("SELECT id FROM track WHERE title = ?").bind(title).fetch_one(&self.st.db).await.unwrap()
    }

    async fn stream(&self, client: &str, title: &str, params: &[(&str, &str)]) -> Stream {
        let id = self.id(title).await;
        let mut p = vec![("id", id.as_str())];
        p.extend_from_slice(params);
        let r = self.http.get(self.url(client, "stream", &p)).send().await.unwrap();
        assert!(r.status().is_success(), "{title}: {}", r.status());
        let headers = r.headers().clone();
        let body = r.bytes().await.unwrap().to_vec();
        assert!(!body.is_empty(), "{title}: nothing sent");
        Stream { headers, body }
    }

    /// An mp3 at 192 kbps of the song, for `client`.
    async fn mp3(&self, client: &str, title: &str) -> Stream {
        self.stream(client, title, &[("format", "mp3"), ("maxBitRate", "192")]).await
    }

    /// Integrated loudness (LUFS) and true peak of what was sent.
    async fn measure(&self, bytes: &[u8]) -> loudness::Measurement {
        let p = self.dir.path().join(format!("sent-{}.mp3", rand_name()));
        std::fs::write(&p, bytes).unwrap();
        let m = loudness::measure(&self.st.cfg, &p).await.unwrap_or_else(|e| panic!("can't measure what was sent: {e:?}"));
        let _ = std::fs::remove_file(&p);
        m
    }

    /// The loudness of the song's own file.
    async fn source(&self, rel: &str) -> loudness::Measurement {
        loudness::measure(&self.st.cfg, &self.music.join(rel)).await.unwrap()
    }

    fn cached(&self) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.st.cfg.cache_dir().join("transcode"))
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|n| !n.ends_with(".part")).collect())
            .unwrap_or_default();
        v.sort();
        v
    }

    /// Waits until the conversions being written are in the cache.
    async fn settled(&self) {
        for _ in 0..500 {
            let parts = std::fs::read_dir(self.st.cfg.cache_dir().join("transcode"))
                .map(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".part")))
                .unwrap_or(false);
            if !parts {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("a conversion never finished");
    }
}

fn rand_name() -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed))
}

fn near(got: f64, want: f64, by: f64, what: &str) {
    assert!((got - want).abs() <= by, "{what}: {got:.2} is not within {by} of {want:.2}");
}

/// The tags of a file that was sent (ffprobe), names in capitals.
fn tags_of(dir: &Path, bytes: &[u8]) -> Vec<String> {
    let p = dir.join(format!("probe-{}.mp3", rand_name()));
    std::fs::write(&p, bytes).unwrap();
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format_tags:stream_tags", "-of", "json"])
        .arg(&p)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&p);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let mut names = vec![];
    for tags in [&v["format"]["tags"]].into_iter().chain(v["streams"].as_array().into_iter().flatten().map(|s| &s["tags"])) {
        if let Some(o) = tags.as_object() {
            names.extend(o.keys().map(|k| k.to_uppercase()));
        }
    }
    names
}

#[tokio::test]
async fn a_levelled_conversion_is_at_the_target_and_never_clips() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(|c| c.level_conversions = Levelling::Track).await;

    // the loud song turned down, the quiet one up: both at −18 LUFS
    for title in ["Loud", "Quiet"] {
        let st = s.mp3("car", title).await;
        let lv = st.header("x-levelled").unwrap_or_else(|| panic!("{title}: not levelled"));
        assert!(lv.starts_with("track ") && lv.ends_with(" dB"), "{lv}");
        let m = s.measure(&st.body).await;
        near(m.loudness, loudness::REFERENCE_LUFS, 1.0, &format!("{title}, levelled"));
        assert!(m.peak <= 1.0, "{title} clips: {}", m.peak);
    }
    // so far apart before (20 dB)
    let (loud, quiet) = (s.source("A/Pair/01 Loud.flac").await, s.source("A/Pair/02 Quiet.flac").await);
    assert!(loud.loudness - quiet.loudness > 15.0, "{loud:?} {quiet:?}");

    // the tags' gain (tags win), and the copy's own ReplayGain tags are left out (it's levelled already)
    let st = s.mp3("car", "Tagged").await;
    assert_eq!(st.header("x-levelled"), Some("track -3.00 dB"));
    let src = s.source("B/Tagged/01 Tagged.flac").await;
    near(s.measure(&st.body).await.loudness, src.loudness - 3.0, 1.0, "Tagged, its tag's gain");
    let tags = tags_of(s.dir.path(), &st.body);
    assert!(tags.contains(&"TITLE".to_string()), "the other tags stay: {tags:?}");
    assert!(!tags.iter().any(|t| t.contains("REPLAYGAIN")), "no ReplayGain tags in a levelled copy: {tags:?}");
    // (an unlevelled copy keeps them, as before)
    let plain = Server::start(|_| {}).await;
    let tags = tags_of(plain.dir.path(), &plain.mp3("car", "Tagged").await.body);
    assert!(tags.iter().any(|t| t.contains("REPLAYGAIN")), "{tags:?}");

    // +10 dB with a peak of 0.5: turned up only until the peak is at −1 dB, so nothing clips
    let st = s.mp3("car", "Boost").await;
    assert_eq!(st.header("x-levelled"), Some("track 5.02 dB capped"));
    let m = s.measure(&st.body).await;
    let src = s.source("C/Boost/01 Boost.flac").await;
    near(m.loudness, src.loudness + 5.02, 1.0, "Boost, capped");
    assert!(m.peak < 0.96, "a capped song stays under the ceiling: {}", m.peak);

    // from the middle of a song (straight from ffmpeg, not cached): levelled too
    let st = s.stream("car", "Loud", &[("format", "mp3"), ("timeOffset", "2")]).await;
    assert!(st.header("x-levelled").is_some());
    near(s.measure(&st.body).await.loudness, loudness::REFERENCE_LUFS, 1.0, "Loud from the middle");

    // the song's replayGain is what it always was: the app is told about the file, not about a stream
    let id = s.id("Loud").await;
    let song = s.call("car", "getSong", &[("id", &id)]).await;
    near(song["song"]["replayGain"]["trackGain"].as_f64().unwrap(), loudness::REFERENCE_LUFS - loud.loudness, 0.5, "replayGain");
}

#[tokio::test]
async fn album_levelling_keeps_an_albums_songs_apart() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(|c| c.level_conversions = Levelling::Album).await;
    let id = s.id("Loud").await;
    let rg = s.call("car", "getSong", &[("id", &id)]).await["song"]["replayGain"].clone();
    let album_gain = rg["albumGain"].as_f64().unwrap();
    // both songs get the album's gain: the quiet one stays quieter, as on the album
    for (title, rel) in [("Loud", "A/Pair/01 Loud.flac"), ("Quiet", "A/Pair/02 Quiet.flac")] {
        let st = s.mp3("car", title).await;
        let lv = st.header("x-levelled").unwrap();
        assert!(lv.starts_with("album "), "{title}: {lv}");
        let got = s.measure(&st.body).await.loudness;
        near(got, s.source(rel).await.loudness + album_gain, 1.0, &format!("{title}, the album's gain"));
    }
    // a song with an album gain tag: the tag's
    let tagged = s.mp3("car", "Tagged").await;
    assert_eq!(tagged.header("x-levelled"), Some("album -4.00 dB"), "the album gain tag");
}

#[tokio::test]
async fn the_cache_keeps_levelled_and_unlevelled_copies_apart() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(|c| {
        c.level_conversions = Levelling::Track;
        c.level_conversions_clients = vec!["Car".into()];
    })
    .await;
    let src = s.source("A/Pair/01 Loud.flac").await;

    let levelled = s.mp3("My Car Stereo", "Loud").await;
    s.settled().await;
    assert_eq!(levelled.header("x-transcode-cache"), Some("miss"));
    assert!(levelled.header("x-levelled").is_some(), "a listed app (any case, part of its name)");
    // another app, not listed: its own, unlevelled copy (not the levelled one from the cache)
    let plain = s.mp3("test", "Loud").await;
    s.settled().await;
    assert_eq!(plain.header("x-transcode-cache"), Some("miss"), "not the levelled copy");
    assert_eq!(plain.header("x-levelled"), None);
    assert_eq!(s.cached().len(), 2, "two copies: {:?}", s.cached());

    // each again from the cache, each as it was
    let again = s.mp3("my car stereo", "Loud").await;
    assert_eq!((again.header("x-transcode-cache"), again.header("x-levelled")), (Some("hit"), levelled.header("x-levelled")));
    assert_eq!(again.body, levelled.body);
    let again = s.mp3("test", "Loud").await;
    assert_eq!((again.header("x-transcode-cache"), again.header("x-levelled")), (Some("hit"), None));
    assert_eq!(again.body, plain.body);
    assert_eq!(s.cached().len(), 2);

    near(s.measure(&levelled.body).await.loudness, loudness::REFERENCE_LUFS, 1.0, "the levelled copy");
    near(s.measure(&plain.body).await.loudness, src.loudness, 0.5, "the unlevelled copy");

    // an app that levels songs itself is left alone when no apps are listed
    let d = Server::start(|c| c.level_conversions = Levelling::Track).await;
    for client in ["Encore", "Symfonium", "feishin"] {
        assert_eq!(d.mp3(client, "Loud").await.header("x-levelled"), None, "{client}");
    }
    assert!(d.mp3("DSub", "Loud").await.header("x-levelled").is_some(), "any other app is levelled");
}

#[tokio::test]
async fn off_changes_nothing_and_the_file_itself_is_never_touched() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    // off (the default): a conversion as it always was
    let off = Server::start(|_| {}).await;
    assert_eq!(off.st.cfg.level_conversions, Levelling::Off);
    let src = off.source("A/Pair/01 Loud.flac").await;
    let st = off.mp3("car", "Loud").await;
    assert_eq!(st.header("x-levelled"), None);
    near(off.measure(&st.body).await.loudness, src.loudness, 0.5, "off: not levelled");

    // on: a stream of the file itself (no format, format=raw, a download) is the file, byte for byte
    let on = Server::start(|c| c.level_conversions = Levelling::Album).await;
    for title in ["Loud", "Tagged", "Boost"] {
        let path: String = sqlx::query_scalar("SELECT rel_path FROM track WHERE title = ?").bind(title).fetch_one(&on.st.db).await.unwrap();
        let file = std::fs::read(on.music.join(&path)).unwrap();
        for params in [&[][..], &[("format", "raw")][..], &[("maxBitRate", "0")][..]] {
            let st = on.stream("car", title, params).await;
            assert_eq!(st.header("x-levelled"), None, "{title} {params:?}");
            assert!(st.body == file, "{title} {params:?}: not the file itself");
        }
        let id = on.id(title).await;
        let r = on.http.get(on.url("car", "download", &[("id", &id)])).send().await.unwrap();
        assert!(r.bytes().await.unwrap().as_ref() == file.as_slice(), "{title}: a download is the file");
    }
    assert!(on.cached().is_empty(), "nothing converted: {:?}", on.cached());
}

#[test]
fn the_settings_are_read_and_a_wrong_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.toml");
    std::fs::write(&file, "level_conversions = \"album\"\nlevel_conversions_clients = [\"car\"]\n").unwrap();
    let c = Config::load(Some(file.clone())).unwrap();
    assert_eq!((c.level_conversions, c.level_conversions_clients), (Levelling::Album, vec!["car".to_string()]));
    std::fs::write(&file, "").unwrap();
    assert_eq!(Config::load(Some(file.clone())).unwrap().level_conversions, Levelling::Off, "off unless set");
    // (Podium saves a value only when Conductor can read it back)
    std::fs::write(&file, "level_conversions = \"loud\"\n").unwrap();
    assert!(Config::load(Some(file)).is_err());
}
