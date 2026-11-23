//! "Sounds like" from the audio itself, end to end (crate::audio): songs of three clearly different kinds (fast
//! percussive noise bursts, low sustained tones in A minor, bright high tones in F♯) are analysed, and songs of the
//! same kind come back as each other's nearest in getSonicSimilarTracks, getSimilarSongs2 and findSonicPath; the
//! analysis goes on where it stopped after a restart; a changed file is analysed again; off means nothing is
//! analysed and the answers are what they were.

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

use conductor::{
    api::{self, AppState},
    audio::{self, Budget},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
};
use serde_json::Value;

const USER: &str = "tester";
const PASS: &str = "s3cret pass";
const SECS: u32 = 20;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    /// fast percussive noise bursts (about 150 BPM)
    Beat,
    /// slow, low sustained tones in A minor
    Pad,
    /// bright high tones with overtones in F♯, a slow tremolo
    Bright,
}

const KINDS: [Kind; 3] = [Kind::Beat, Kind::Pad, Kind::Bright];

fn kind_of(title: &str) -> Kind {
    match title.split(' ').next().unwrap() {
        "Beat" => Kind::Beat,
        "Pad" => Kind::Pad,
        "Bright" => Kind::Bright,
        t => panic!("{t}"),
    }
}

/// The expression for ffmpeg's aevalsrc: song `v` (0…3) of a kind, each a little different.
fn expr(kind: Kind, v: u32) -> String {
    let v = v as f64;
    match kind {
        Kind::Beat => {
            let every = 60.0 / (146.0 + 4.0 * v);
            format!("{:.2}*(2*random(0)-1)*exp(-30*mod(t,{every:.5}))", 0.5 + 0.05 * v)
        }
        Kind::Pad => {
            let (a, c, e) = (110.0 * (1.0 + 0.002 * v), 130.81, 164.81);
            format!(
                "(0.75+0.25*sin(2*PI*{:.2}*t))*({:.2}*sin(2*PI*{a:.2}*t)+0.15*sin(2*PI*{c}*t)+0.12*sin(2*PI*{e}*t)+0.08*sin(2*PI*{:.2}*t))",
                0.08 + 0.02 * v,
                0.25 + 0.02 * v,
                2.0 * a
            )
        }
        Kind::Bright => {
            let (f, c) = (739.99, 1108.73);
            let o = 0.1 + 0.02 * v;
            format!(
                "(0.8+0.2*sin(2*PI*{:.2}*t))*(0.2*sin(2*PI*{f}*t)+{o:.2}*sin(2*PI*{}*t)+0.06*sin(2*PI*{}*t)+0.15*sin(2*PI*{c}*t)+{o:.2}*sin(2*PI*{}*t))",
                0.4 + 0.1 * v,
                2.0 * f,
                3.0 * f,
                2.0 * c
            )
        }
    }
}

fn make(path: &Path, title: &str, kind: Kind, v: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let e = expr(kind, v);
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("aevalsrc='{e}':s=44100:d={SECS}")]);
    for (k, val) in [("title", title), ("artist", "Tester"), ("album", title), ("track", "1")] {
        cmd.args(["-metadata", &format!("{k}={val}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success(), "ffmpeg failed for {}", path.display());
}

fn file_of(music: &Path, kind: Kind, v: u32) -> PathBuf {
    music.join(format!("{kind:?}/{kind:?} {v}/01 {kind:?} {v}.flac"))
}

/// Four songs of each kind: "Beat 0" … "Bright 3".
fn library(music: &Path) {
    for k in KINDS {
        for v in 0..4 {
            make(&file_of(music, k, v), &format!("{k:?} {v}"), k, v);
        }
    }
}

struct Server {
    base: String,
    http: reqwest::Client,
    st: Arc<AppState>,
    dir: Arc<tempfile::TempDir>,
    music: PathBuf,
}

impl Server {
    async fn start(make: impl FnOnce(&Path), tune: impl FnOnce(&mut Config)) -> Self {
        let dir = Arc::new(tempfile::tempdir().unwrap());
        make(&dir.path().join("music"));
        Self::open(dir, tune).await
    }

    /// The server on a data folder (a new one, or the one a server used before: a restart).
    async fn open(dir: Arc<tempfile::TempDir>, tune: impl FnOnce(&mut Config)) -> Self {
        let music = dir.path().join("music");
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
        let first = !cfg.data_dir.exists();
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        if first {
            auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        }
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        Server { base, http: reqwest::Client::new(), st, dir, music }
    }

    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{PASS}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", USER), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        let url = format!("{}/rest/{method}.view?{}", self.base, q.finish());
        let r: Value = self.http.get(url).send().await.unwrap().json().await.unwrap();
        r["subsonic-response"].clone()
    }

    async fn ok(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let r = self.call(method, params).await;
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }

    /// Every song's id, by title.
    async fn ids(&self) -> std::collections::BTreeMap<String, String> {
        let r = self.ok("search3", &[("query", ""), ("songCount", "500"), ("albumCount", "0"), ("artistCount", "0")]).await;
        r["searchResult3"]["song"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["title"].as_str().unwrap().to_string(), s["id"].as_str().unwrap().to_string()))
            .collect()
    }

    async fn rescan(&self, full: bool) {
        library::scan(&self.st.db, &self.st.libs, &self.st.scan, full).await.unwrap();
    }

    async fn analysed_at(&self) -> Vec<(String, i64)> {
        sqlx::query_as("SELECT track_id, analysed_at FROM sound ORDER BY track_id").fetch_all(&self.st.db).await.unwrap()
    }
}

fn titles(list: &Value, key: &str) -> Vec<String> {
    list.as_array().unwrap().iter().map(|x| (if key.is_empty() { x } else { &x[key] })["title"].as_str().unwrap().to_string()).collect()
}

/// A file changed later than the scanner saw it (a second's resolution).
fn touch_later(path: &Path) {
    let f = std::fs::File::options().append(true).open(path).unwrap();
    f.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
}

#[tokio::test]
async fn songs_of_a_kind_sound_alike() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(|m| {
        library(m);
        // not audio: marked as tried, not tried again
        std::fs::create_dir_all(m.join("Broken")).unwrap();
        std::fs::write(m.join("Broken/01 Broken.flac"), b"fLaC this isn't a song at all").unwrap();
    }, |_| {})
    .await;
    let ids = s.ids().await;
    let broken = ids.contains_key("01 Broken") || ids.contains_key("Broken");
    // before anything's analysed: "not available", as before
    assert_eq!(s.call("getSonicSimilarTracks", &[("id", &ids["Beat 0"])]).await["error"]["code"], 70);

    let t0 = std::time::Instant::now();
    let rep = audio::pass(&s.st, Budget::ALL).await.unwrap();
    eprintln!("analysed {} songs in {:?}", rep.analysed, t0.elapsed());
    assert_eq!(rep.due, 12 + broken as usize, "{rep:?}");
    assert_eq!(rep.analysed, rep.due, "{rep:?}");
    assert_eq!(rep.unreadable, broken as usize, "{rep:?}");
    let st = audio::status(&s.st).await;
    assert_eq!((st["songs"].as_i64(), st["analysed"].as_i64(), st["on"].as_bool()), (Some(rep.due as i64), Some(rep.due as i64), Some(true)), "{st}");

    // each song's nearest three are the other three of its kind
    for (title, id) in ids.iter().filter(|(t, _)| !t.contains("Broken")) {
        let k = kind_of(title);
        let r = s.ok("getSonicSimilarTracks", &[("id", id), ("count", "3")]).await;
        let m = &r["sonicMatch"];
        let found = titles(m, "entry");
        assert_eq!(found.len(), 3, "{title}: {found:?}");
        assert!(found.iter().all(|t| kind_of(t) == k && t != title), "{title} sounds like {found:?}");
        for x in m.as_array().unwrap() {
            let sim = x["similarity"].as_f64().unwrap();
            assert!(sim > 0.0 && sim <= 1.0, "{sim}");
        }
        // and the most alike first: the one of another kind is further than all of its own kind
        let all = s.ok("getSonicSimilarTracks", &[("id", id), ("count", "11")]).await;
        let all = titles(&all["sonicMatch"], "entry");
        assert_eq!(all.len(), 11, "{all:?}");
        assert!(all[..3].iter().all(|t| kind_of(t) == k) && all[3..].iter().all(|t| kind_of(t) != k), "{title}: {all:?}");
        // song radio, blended with the artists that go with it (here all the same): the same kind first too
        let r = s.ok("getSimilarSongs2", &[("id", id), ("count", "3")]).await;
        let found = titles(&r["similarSongs2"]["song"], "");
        assert!(found.len() == 3 && found.iter().all(|t| kind_of(t) == k), "{title}: radio {found:?}");
    }
    // a path from one sound to another: from the first, to the last
    let r = s.ok("findSonicPath", &[("startSongId", &ids["Beat 0"]), ("endSongId", &ids["Pad 0"]), ("count", "5")]).await;
    let p = titles(&r["sonicPath"], "entry");
    assert_eq!(p.len(), 5, "{p:?}");
    assert_eq!((p[0].as_str(), p[4].as_str()), ("Beat 0", "Pad 0"), "{p:?}");
    assert_eq!(p.iter().collect::<std::collections::HashSet<_>>().len(), 5, "each song once: {p:?}");

    // nothing more to do (not the broken file either), and a rescan, a full one too, keeps every analysis
    assert_eq!(audio::pass(&s.st, Budget::ALL).await.unwrap().due, 0);
    let before = s.analysed_at().await;
    s.rescan(false).await;
    s.rescan(true).await;
    assert_eq!(audio::pass(&s.st, Budget::ALL).await.unwrap().due, 0, "a rescan keeps the analysis");
    assert_eq!(s.analysed_at().await, before);
}

#[tokio::test]
async fn analysis_goes_on_after_a_restart_and_a_changed_file_is_analysed_again() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(library, |_| {}).await;
    // stopped after five songs (as at a shutdown)
    let rep = audio::pass(&s.st, Budget { songs: 5, time: std::time::Duration::MAX }).await.unwrap();
    assert_eq!((rep.due, rep.analysed), (12, 5), "{rep:?}");
    let first = s.analysed_at().await;
    assert_eq!(first.len(), 5);
    assert_eq!(audio::status(&s.st).await["analysed"], 5);
    let ids = s.ids().await;
    let (dir, music) = (s.dir.clone(), s.music.clone());
    drop(s);

    // started again on the same data: the other seven, and the five aren't analysed again
    let s = Server::open(dir, |_| {}).await;
    let rep = audio::pass(&s.st, Budget::ALL).await.unwrap();
    assert_eq!((rep.due, rep.analysed), (7, 7), "{rep:?}");
    let all = s.analysed_at().await;
    assert_eq!(all.len(), 12);
    for x in &first {
        assert!(all.contains(x), "{x:?} analysed again");
    }
    let near = |r: Value| titles(&r["sonicMatch"], "entry");
    let was = near(s.ok("getSonicSimilarTracks", &[("id", &ids["Beat 3"]), ("count", "3")]).await);
    assert!(was.iter().all(|t| kind_of(t) == Kind::Beat), "{was:?}");

    // a file that changes (Beat 3 is now a low pad) is analysed again, and only that one
    let f = file_of(&music, Kind::Beat, 3);
    make(&f, "Beat 3", Kind::Pad, 3);
    touch_later(&f);
    s.rescan(false).await;
    let rep = audio::pass(&s.st, Budget::ALL).await.unwrap();
    assert_eq!((rep.due, rep.analysed), (1, 1), "{rep:?}");
    let now = near(s.ok("getSonicSimilarTracks", &[("id", &ids["Beat 3"]), ("count", "3")]).await);
    assert!(now.iter().all(|t| kind_of(t) == Kind::Pad), "it sounds like the pads now: {now:?}");
    // and the pads find it among them
    let pads = near(s.ok("getSonicSimilarTracks", &[("id", &ids["Pad 3"]), ("count", "4")]).await);
    assert!(pads.contains(&"Beat 3".to_string()), "{pads:?}");
}

#[tokio::test]
async fn off_means_no_analysis_and_the_answers_as_before() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let s = Server::start(library, |c| c.audio_analysis = false).await;
    let rep = audio::pass(&s.st, Budget::ALL).await.unwrap();
    assert_eq!(rep, audio::Pass::default(), "nothing analysed");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sound").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(n, 0);
    assert_eq!(audio::status(&s.st).await["on"], false);
    let ids = s.ids().await;
    // "not available", so apps fall back, as before
    let r = s.call("getSonicSimilarTracks", &[("id", &ids["Beat 0"])]).await;
    assert_eq!(r["error"]["code"], 70, "{r}");
    let r = s.call("findSonicPath", &[("startSongId", &ids["Beat 0"]), ("endSongId", &ids["Pad 0"])]).await;
    assert_eq!(r["error"]["code"], 70, "{r}");
    // song radio as before: the artist's own songs (all twelve are by the same artist here)
    let r = s.ok("getSimilarSongs2", &[("id", &ids["Beat 0"]), ("count", "20")]).await;
    let found = titles(&r["similarSongs2"]["song"], "");
    assert_eq!(found.len(), 11, "{found:?}");

    // analysed earlier with it on, then turned off: what was analysed isn't used either
    let dir = s.dir.clone();
    drop(s);
    let on = Server::open(dir.clone(), |_| {}).await;
    assert_eq!(audio::pass(&on.st, Budget::ALL).await.unwrap().analysed, 12);
    assert_eq!(on.ok("getSonicSimilarTracks", &[("id", &ids["Beat 0"])]).await["sonicMatch"].as_array().unwrap().len(), 11);
    drop(on);
    let off = Server::open(dir, |c| c.audio_analysis = false).await;
    assert_eq!(off.call("getSonicSimilarTracks", &[("id", &ids["Beat 0"])]).await["error"]["code"], 70);
}
