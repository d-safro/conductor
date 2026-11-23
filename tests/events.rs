//! `encore.events`: the event stream, end to end. A server in-process (as in tests/api.rs, with its own small
//! harness here), two accounts, and streams read as an app reads them.

use std::{
    process::Command,
    sync::Arc,
    time::{Duration, Instant},
};

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
    live::{self, Audience},
};
use serde_json::{Value, json};

const USER: &str = "tester";
const PASS: &str = "s3cret pass";
const OTHER: &str = "other";
const OTHER_PASS: &str = "other pass";

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn tone(path: &std::path::Path, title: &str, track: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={}:duration=2", 220 * track)])
        .args(["-metadata", &format!("title={title}"), "-metadata", "artist=Alpha", "-metadata", "album=First"])
        .args(["-metadata", &format!("track={track}")])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg failed for {}", path.display());
}

struct Server {
    base: String,
    addr: std::net::SocketAddr,
    http: reqwest::Client,
    st: Arc<AppState>,
    _dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        tone(&music.join("Alpha/First/01 One.flac"), "One", 1);
        tone(&music.join("Alpha/First/02 Two.flac"), "Two", 2);
        let mut cfg = Config {
            data_dir: dir.path().join("data"),
            libraries: vec![LibraryConfig { name: "Music".into(), path: music, sentinel: None, scan_threads: None }],
            jam_public_url: Some("https://jam.example.test".into()),
            ..Config::default()
        };
        // nothing online: these tests never reach another server
        cfg.catalog = false;
        cfg.deezer_url = "http://127.0.0.1:9".into();
        cfg.lyrics_online = false;
        cfg.covers_fetch = false;
        cfg.concerts = false;
        cfg.info_online = false;
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        auth::create_user(&pool, &secrets, OTHER, OTHER_PASS, false).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        Server { base: format!("http://{addr}"), addr, http: reqwest::Client::new(), st, _dir: dir }
    }

    fn query(user: &str, pass: &str, params: &[(&str, &str)]) -> String {
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{pass}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", user), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        q.finish()
    }

    fn url(&self, user: &str, method: &str, params: &[(&str, &str)]) -> String {
        let pass = if user == OTHER { OTHER_PASS } else { PASS };
        format!("{}/rest/{method}.view?{}", self.base, Self::query(user, pass, params))
    }

    async fn ok_as(&self, user: &str, method: &str, params: &[(&str, &str)]) -> Value {
        let r: Value = self.http.get(self.url(user, method, params)).send().await.unwrap().json().await.unwrap();
        let r = r["subsonic-response"].clone();
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }

    async fn ok(&self, method: &str, params: &[(&str, &str)]) -> Value {
        self.ok_as(USER, method, params).await
    }

    async fn uid(&self, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(name).fetch_one(&self.st.db).await.unwrap()
    }

    async fn song(&self) -> String {
        self.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string()
    }

    async fn open_with(&self, user: &str, params: &[(&str, &str)], last_id: Option<&str>) -> Sse {
        let mut req = self.http.get(self.url(user, "eventStream", params));
        if let Some(id) = last_id {
            req = req.header("Last-Event-ID", id);
        }
        let resp = req.send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.headers()["content-type"], "text/event-stream; charset=utf-8");
        let mut s = Sse { resp, buf: String::new() };
        let hello = s.event(Duration::from_secs(5)).await.expect("hello");
        assert_eq!(hello.event, "hello", "{hello:?}");
        s
    }

    async fn open(&self, user: &str, params: &[(&str, &str)]) -> Sse {
        self.open_with(user, params, None).await
    }

    /// Wait until `f` holds (polling), for at most `secs`.
    async fn until(&self, secs: u64, what: &str, f: impl Fn(&Self) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while !f(self) {
            assert!(Instant::now() < end, "timed out waiting: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Frame {
    id: Option<String>,
    event: String,
    data: Value,
    comment: Option<String>,
    retry: Option<u64>,
}

enum Next {
    Frame(Frame),
    Ended,
    Quiet,
}

/// A stream read the way an app reads it.
struct Sse {
    resp: reqwest::Response,
    buf: String,
}

impl Sse {
    async fn next(&mut self, within: Duration) -> Next {
        let end = tokio::time::Instant::now() + within;
        loop {
            if let Some(i) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..i + 2).collect();
                let mut f = Frame::default();
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(c) = line.strip_prefix(':') {
                        f.comment = Some(c.trim().to_string());
                    } else if let Some((k, v)) = line.split_once(": ") {
                        match k {
                            "id" => f.id = Some(v.to_string()),
                            "event" => f.event = v.to_string(),
                            "data" => data.push_str(v),
                            "retry" => f.retry = v.parse().ok(),
                            _ => panic!("unknown field {k}"),
                        }
                    }
                }
                if !data.is_empty() {
                    f.data = serde_json::from_str(&data).expect("data is JSON");
                }
                return Next::Frame(f);
            }
            match tokio::time::timeout_at(end, self.resp.chunk()).await {
                Ok(Ok(Some(b))) => self.buf.push_str(std::str::from_utf8(&b).unwrap()),
                Ok(_) => return Next::Ended,
                Err(_) => return Next::Quiet,
            }
        }
    }

    /// The next event (keep-alives skipped), recorded for the shape.
    async fn event(&mut self, within: Duration) -> Option<Frame> {
        loop {
            match self.next(within).await {
                Next::Frame(f) if f.event.is_empty() => continue,
                Next::Frame(f) => {
                    shapes::observe(&f);
                    return Some(f);
                }
                _ => return None,
            }
        }
    }

    /// The next event of `kind`, and the others that came before it.
    async fn until(&mut self, kind: &str) -> (Frame, Vec<Frame>) {
        let mut before = vec![];
        loop {
            let f = self.event(Duration::from_secs(5)).await.unwrap_or_else(|| panic!("no {kind} event; got {before:?}"));
            if f.event == kind {
                return (f, before);
            }
            before.push(f);
        }
    }

    /// Ends within `secs` (anything still sent before is read past).
    async fn ends(&mut self, secs: u64) -> bool {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            match self.next(end.saturating_duration_since(Instant::now())).await {
                Next::Ended => return true,
                Next::Quiet => return false,
                Next::Frame(_) => {}
            }
        }
    }
}

/// The events' fields, recorded in docs/api/shapes/eventStream.json like every call's answer (tests/api.rs's
/// contract): each event's data under its name. A field may be added (CONDUCTOR_BLESS=1 records it), never dropped
/// or retyped.
mod shapes {
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Mutex,
    };

    use serde_json::Value;

    type Shape = BTreeMap<String, BTreeSet<String>>;
    static SEEN: Mutex<Option<Shape>> = Mutex::new(None);

    fn file() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/api/shapes/eventStream.json")
    }

    fn walk(v: &Value, path: &str, out: &mut Shape) {
        let t = match v {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        out.entry(path.to_string()).or_default().insert(t.to_string());
        match v {
            Value::Array(a) => a.iter().for_each(|x| walk(x, &format!("{path}[]"), out)),
            Value::Object(o) => o.iter().for_each(|(k, x)| walk(x, &format!("{path}.{k}"), out)),
            _ => {}
        }
    }

    pub fn observe(f: &super::Frame) {
        let mut shape = Shape::new();
        walk(&f.data, &f.event, &mut shape);
        let recorded: Shape =
            std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        if std::env::var_os("CONDUCTOR_BLESS").is_some() {
            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            let all = seen.get_or_insert_with(|| recorded.clone());
            for (p, t) in shape {
                all.entry(p).or_default().extend(t);
            }
            std::fs::write(file(), serde_json::to_string_pretty(&*all).unwrap() + "\n").unwrap();
            return;
        }
        for (p, t) in &shape {
            match recorded.get(p) {
                None => panic!("eventStream: {p} isn't recorded (a new field: CONDUCTOR_BLESS=1 cargo test --test events)"),
                Some(k) if !t.is_subset(k) => panic!("eventStream: {p} is {t:?}, recorded as {k:?}"),
                _ => {}
            }
        }
    }
}

fn ffmpeg_or_skip() -> bool {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return false;
    }
    true
}

#[tokio::test]
async fn a_login_is_needed_and_the_extension_is_listed() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    let exts = s.ok("getOpenSubsonicExtensions", &[]).await;
    let ext = exts["openSubsonicExtensions"].as_array().unwrap().iter().find(|e| e["name"] == "encore.events").cloned();
    assert_eq!(ext.expect("encore.events listed")["versions"], json!([1]));

    // no login, a wrong one: a Subsonic error, not a stream
    for url in [
        format!("{}/rest/eventStream.view?f=json&c=test", s.base),
        format!("{}/rest/eventStream.view?{}", s.base, Server::query(USER, "wrong", &[])),
    ] {
        let r = s.http.get(url).send().await.unwrap();
        assert!(r.headers()["content-type"].to_str().unwrap().starts_with("application/json"));
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["subsonic-response"]["status"], "failed");
        assert!([10, 40].contains(&v["subsonic-response"]["error"]["code"].as_i64().unwrap()), "{v}");
    }
    assert_eq!(s.st.live.stream_count(None), 0);

    // an API key works too (and as a form POST, the login out of the address)
    let key = auth::create_api_key(&s.st.db, USER, "events").await.unwrap();
    let r = s
        .http
        .post(format!("{}/rest/eventStream", s.base))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_urlencoded::Serializer::new(String::new()).extend_pairs([("apiKey", key.as_str()), ("c", "t"), ("f", "json")]).finish())
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["content-type"], "text/event-stream; charset=utf-8");
    assert_eq!(r.headers()["cache-control"], "no-cache, no-store, no-transform");
    assert_eq!(r.headers()["x-accel-buffering"], "no");
    let mut sse = Sse { resp: r, buf: String::new() };
    let Next::Frame(first) = sse.next(Duration::from_secs(5)).await else { panic!("nothing at once") };
    assert_eq!(first.retry, Some(5000));
    assert_eq!(first.event, "hello");
    assert_eq!(first.data["resumed"], false);
    assert!(first.data["events"].as_array().unwrap().iter().any(|e| e == "command"));
    shapes::observe(&first);
}

#[tokio::test]
async fn each_kind_of_event_reaches_its_account() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    let (a, b) = ("laptop-aaaa1111", "phone-bbbb2222");
    let me = s.uid(USER).await;
    let mut laptop = s.open(USER, &[("id", a)]).await;
    let mut phone = s.open(USER, &[("id", b)]).await;
    let mut other = s.open(OTHER, &[]).await;

    // devices: a hello, a state, a goodbye
    s.ok("deviceHello", &[("id", a), ("name", "Laptop"), ("kind", "computer")]).await;
    s.ok("deviceHello", &[("id", b), ("name", "Pixel")]).await;
    let (e, _) = phone.until("devices").await;
    assert_eq!(e.data["device"], a);
    s.ok("deviceState", &[("id", a), ("state", r#"{"playing":true}"#)]).await;
    let (e, _) = phone.until("devices").await;
    assert!(e.id.unwrap().contains('-'));

    // a command: for the phone only, as it said which device it is
    let r = s.ok("deviceCommand", &[("id", a), ("target", b), ("command", r#"{"type":"pause"}"#)]).await;
    let (e, _) = phone.until("command").await;
    assert_eq!(e.data, json!({ "device": b, "seq": r["deviceCommand"]["seq"] }));
    // (the command itself comes from devicePoll, at once)
    let p = s.ok("devicePoll", &[("id", b), ("ack", "0"), ("wait", "0")]).await;
    assert_eq!(p["devicePoll"]["commands"][0]["type"], "pause");

    // sync
    s.ok("syncSet", &[("key", "pins"), ("value", "[1]"), ("id", a)]).await;
    let (e, before) = laptop.until("sync").await;
    assert_eq!((e.data["key"].as_str(), e.data["by"].as_str()), (Some("pins"), Some(a)));
    assert!(e.data["at"].as_i64().unwrap() > 0);
    assert!(!before.iter().any(|f| f.event == "command"), "the laptop got the phone's command: {before:?}");
    // a value that didn't win isn't news
    s.ok("syncSet", &[("key", "pins"), ("value", "[2]"), ("at", "1"), ("id", b)]).await;

    // playlists: made, changed, deleted
    let pl = s.ok("createPlaylist", &[("name", "Mine")]).await["playlist"]["id"].as_str().unwrap().to_string();
    let (e, before) = laptop.until("playlist").await;
    assert_eq!(e.data, json!({ "id": pl, "deleted": false }));
    assert!(!before.iter().any(|f| f.event == "sync"), "an older value that lost was announced: {before:?}");
    s.ok("updatePlaylist", &[("playlistId", &pl), ("name", "Mine too")]).await;
    assert_eq!(laptop.until("playlist").await.0.data["deleted"], false);
    s.ok("deletePlaylist", &[("id", &pl)]).await;
    assert_eq!(laptop.until("playlist").await.0.data["deleted"], true);

    // Jam: a guest comes, and adds a song
    let jl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jbase = format!("http://{}", jl.local_addr().unwrap());
    let app = conductor::jam::router(s.st.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(jl, app).await.unwrap() });
    let tok = s.ok("jamStart", &[]).await["jam"]["token"].as_str().unwrap().to_string();
    s.http.get(format!("{jbase}/jam/{tok}/state?g=guest1&n=Bob")).send().await.unwrap();
    laptop.until("jam").await;
    let one = s.song().await;
    let r = s.http.post(format!("{jbase}/jam/{tok}/add")).json(&json!({"id": one, "by": "Bob"})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    laptop.until("jam").await;

    // now playing: everyone sees it (as getNowPlaying shows it to everyone), once per song
    s.ok("scrobble", &[("id", &one), ("submission", "false")]).await;
    s.ok("scrobble", &[("id", &one), ("submission", "false")]).await;
    laptop.until("nowPlaying").await;
    let (_, before) = other.until("nowPlaying").await;
    // and that's all the other account heard: nothing of this one's devices, commands, sync, playlists or Jam
    assert!(before.is_empty(), "{before:?}");

    // the scan: started, finished, for everyone
    s.ok("startScan", &[]).await;
    let (start, before) = other.until("scan").await;
    assert!(before.is_empty(), "the same song again was announced: {before:?}");
    assert_eq!(start.data["state"], "started");
    assert_eq!(start.data["scanning"], true);
    let (end, _) = other.until("scan").await;
    assert_eq!((end.data["state"].as_str(), end.data["scanning"].as_bool()), (Some("finished"), Some(false)));
    assert!(end.data["count"].is_number());
    s.ok("reportPlayback", &[("mediaId", &one), ("mediaType", "song"), ("positionMs", "0"), ("state", "stopped")]).await;
    other.until("nowPlaying").await;

    // the other account's playlist: only it hears
    let theirs = s.ok_as(OTHER, "createPlaylist", &[("name", "Theirs")]).await["playlist"]["id"].as_str().unwrap().to_string();
    assert_eq!(other.until("playlist").await.0.data["id"], theirs.as_str());
    s.ok("deviceBye", &[("id", b)]).await;
    let (_, before) = laptop.until("devices").await;
    assert!(!before.iter().any(|f| f.event == "playlist"), "another account's playlist: {before:?}");

    // the stream of a device keeps it listed as online (as devicePoll does)
    s.st.live.set_keepalive(Duration::from_millis(200));
    s.st.live.lock().devices.get_mut(&(me.clone(), a.to_string())).unwrap().seen_ms = 0;
    let mut quick = s.open(USER, &[("id", a)]).await; // (the stream that knows the new keep-alive)
    assert!(laptop.ends(5).await, "the newer stream of the same device replaces the older one");
    let Next::Frame(k) = quick.next(Duration::from_secs(3)).await else { panic!("no keep-alive") };
    assert_eq!(k.comment.as_deref(), Some("keep-alive"));
    assert!(s.st.live.lock().devices[&(me, a.to_string())].seen_ms > 0);
}

#[tokio::test]
async fn keep_alives_carry_the_newest_id() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    s.st.live.set_keepalive(Duration::from_millis(300));
    let mut sse = s.open(USER, &[]).await;
    let t0 = Instant::now();
    let Next::Frame(k) = sse.next(Duration::from_secs(3)).await else { panic!("no keep-alive") };
    assert_eq!(k.comment.as_deref(), Some("keep-alive"));
    assert!(t0.elapsed() >= Duration::from_millis(200), "{:?}", t0.elapsed());
    // the other account's events move the id on, without being sent here
    let other = s.uid(OTHER).await;
    for i in 0..3 {
        s.st.live.publish("sync", Audience::User(other.clone()), json!({ "key": format!("k{i}"), "by": "x", "at": 1 }));
    }
    let id = s.st.live.event_id(s.st.live.last_event());
    loop {
        match sse.next(Duration::from_secs(3)).await {
            Next::Frame(f) if f.comment.is_some() && f.id.as_deref() == Some(id.as_str()) => break,
            Next::Frame(f) => assert!(f.event.is_empty(), "another account's event: {f:?}"),
            _ => panic!("no keep-alive with {id}"),
        }
    }
}

#[tokio::test]
async fn a_reconnect_gets_what_it_missed_or_a_resync() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    let me = s.uid(USER).await;
    let mut sse = s.open(USER, &[]).await;
    s.ok("syncSet", &[("key", "k1"), ("value", "1")]).await;
    let (e1, _) = sse.until("sync").await;
    let id1 = e1.id.unwrap();
    drop(sse);
    s.until(5, "the stream to close", |s| s.st.live.stream_count(None) == 0).await;
    s.ok("syncSet", &[("key", "k2"), ("value", "2")]).await;
    s.ok("syncSet", &[("key", "k3"), ("value", "3")]).await;

    // with Last-Event-ID (or lastEventId=): what came since, in order, and no resync
    for (params, header) in [(vec![], Some(id1.as_str())), (vec![("lastEventId", id1.as_str())], None)] {
        let resp = s.http.get(s.url(USER, "eventStream", &params));
        let resp = if let Some(h) = header { resp.header("Last-Event-ID", h) } else { resp };
        let mut sse = Sse { resp: resp.send().await.unwrap(), buf: String::new() };
        let hello = sse.event(Duration::from_secs(5)).await.unwrap();
        assert_eq!((hello.event.as_str(), &hello.data["resumed"]), ("hello", &json!(true)));
        let keys: Vec<Value> = [sse.event(Duration::from_secs(5)).await.unwrap(), sse.event(Duration::from_secs(5)).await.unwrap()]
            .iter()
            .map(|f| {
                assert_eq!(f.event, "sync");
                f.data["key"].clone()
            })
            .collect();
        assert_eq!(keys, [json!("k2"), json!("k3")]);
    }

    // an id from before the server started, or not one: resync
    for stale in ["0000abcd-3", "nonsense"] {
        let mut sse = s.open_with(USER, &[], Some(stale)).await;
        assert_eq!(sse.event(Duration::from_secs(5)).await.unwrap().event, "resync", "{stale}");
    }

    // one older than the log keeps: resync, then on as usual
    for i in 0..live::EVENT_LOG + 10 {
        s.st.live.publish("playlist", Audience::User(me.clone()), json!({ "id": format!("p{i}"), "deleted": false }));
    }
    let mut sse = s.open_with(USER, &[], Some(&id1)).await;
    assert_eq!(sse.event(Duration::from_secs(5)).await.unwrap().event, "resync");
    s.ok("syncSet", &[("key", "k4"), ("value", "4")]).await;
    assert_eq!(sse.event(Duration::from_secs(5)).await.unwrap().data["key"], "k4");

    // many at once: a resync instead; the same one many times: once
    for i in 0..40 {
        s.st.live.publish("playlist", Audience::User(me.clone()), json!({ "id": format!("q{i}"), "deleted": false }));
    }
    assert_eq!(sse.event(Duration::from_secs(5)).await.unwrap().event, "resync");
    for _ in 0..10 {
        s.st.live.publish("playlist", Audience::User(me.clone()), json!({ "id": "same", "deleted": false }));
    }
    s.ok("syncSet", &[("key", "k5"), ("value", "5")]).await;
    let (_, before) = sse.until("sync").await;
    assert_eq!(before.len(), 1, "{before:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_doesnt_read_is_dropped_not_buffered() {
    if !ffmpeg_or_skip() {
        return;
    }
    use tokio::io::AsyncWriteExt;
    let s = Server::start().await;
    s.st.live.set_keepalive(Duration::from_millis(200));
    let me = s.uid(USER).await;
    // one that reads along, and one that never reads (with a small window, so the system's buffers fill soon)
    let mut reader = s.open(USER, &[("id", "reader-1111aaaa")]).await;
    let reading = tokio::spawn(async move {
        let mut n = 0usize;
        loop {
            match reader.next(Duration::from_secs(10)).await {
                Next::Frame(_) => n += 1,
                _ => return n,
            }
        }
    });
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.set_recv_buffer_size(2048).unwrap();
    let mut slow = sock.connect(s.addr).await.unwrap();
    let q = Server::query(USER, PASS, &[("id", "slowpoke-2222bbbb")]);
    slow.write_all(format!("GET /rest/eventStream.view?{q} HTTP/1.1\r\nHost: test\r\n\r\n").as_bytes()).await.unwrap();
    s.until(5, "both streams open", |s| s.st.live.stream_count(Some(&me)) == 2).await;

    let t0 = Instant::now();
    let mut i = 0u64;
    while s.st.live.stream_count(Some(&me)) == 2 {
        assert!(t0.elapsed() < Duration::from_secs(30), "the one that doesn't read is still open after {i} events");
        for _ in 0..20 {
            i += 1;
            let pad = "x".repeat(200);
            s.st.live.publish("sync", Audience::User(me.clone()), json!({ "key": format!("k{i}"), "by": pad, "at": i }));
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    // the reader is still there, and got its events
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(s.st.live.stream_count(Some(&me)), 1, "the reader was dropped too");
    drop(slow);
    s.st.live.close_streams();
    assert!(reading.await.unwrap() > 10);
}

#[tokio::test]
async fn streams_per_account_are_capped_and_a_device_replaces_its_own() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    let me = s.uid(USER).await;
    let mut first = s.open(USER, &[("id", "desk-aaaa1111")]).await;
    let _second = s.open(USER, &[("id", "desk-aaaa1111")]).await;
    assert!(first.ends(5).await, "the older stream of the device is closed");
    s.until(5, "one stream left", |s| s.st.live.stream_count(Some(&me)) == 1).await;
    let mut others = vec![];
    for _ in 1..live::MAX_STREAMS {
        others.push(s.open(USER, &[]).await);
    }
    let r = s.http.get(s.url(USER, "eventStream", &[])).send().await.unwrap();
    assert_eq!(r.status(), 429);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["subsonic-response"]["status"], "failed");
    // the other account isn't held back by this one's
    let _theirs = s.open(OTHER, &[]).await;
    // and the device can still come back (its old stream goes)
    let _again = s.open(USER, &[("id", "desk-aaaa1111")]).await;
    assert_eq!(s.st.live.stream_count(Some(&me)), live::MAX_STREAMS);
}

#[tokio::test]
async fn a_changed_password_or_stopping_server_ends_the_stream() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    s.st.live.set_keepalive(Duration::from_millis(200));
    let mut mine = s.open(USER, &[]).await;
    let mut theirs = s.open(OTHER, &[]).await;
    s.ok_as(OTHER, "changePassword", &[("username", OTHER), ("password", "a new one")]).await;
    assert!(theirs.ends(5).await, "the stream outlived the password it was opened with");
    assert!(!mine.ends(1).await, "another account's stream ended");
    let key = auth::create_api_key(&s.st.db, USER, "k").await.unwrap();
    let resp = s.http.get(format!("{}/rest/eventStream.view?apiKey={key}&c=t&f=json", s.base)).send().await.unwrap();
    let mut keyed = Sse { resp, buf: String::new() };
    assert_eq!(keyed.event(Duration::from_secs(5)).await.unwrap().event, "hello");
    sqlx::query("DELETE FROM api_key").execute(&s.st.db).await.unwrap();
    assert!(keyed.ends(5).await, "the stream outlived its API key");
    s.st.live.close_streams();
    assert!(mine.ends(5).await, "stopping the server didn't end the stream");
}
