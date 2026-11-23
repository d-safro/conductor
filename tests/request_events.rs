//! `encore.events`' `requests` event: an account hears when what it asked for (`encore.requests`) changes, an admin
//! when anything does; nobody hears of someone else's. A server in-process (as in tests/events.rs, with its own
//! small harness here), an admin and a user, a stand-in for slskd whose download moves on, and requests put
//! straight into the database.

use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
};
use serde_json::{Value, json};

const ADMIN: &str = "tester";
const PASS: &str = "s3cret pass";
const USER: &str = "other";
const USER_PASS: &str = "other pass";

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn tone(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
        .args(["-metadata", "title=One", "-metadata", "artist=Alpha", "-metadata", "album=First"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg failed for {}", path.display());
}

/// slskd, as Conductor uses it: one download under way, as far along as `percent` says (in tenths).
async fn fake_slskd(percent: Arc<AtomicU64>) -> String {
    use axum::{extract::Request, response::IntoResponse};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = axum::Router::new().fallback(move |req: Request| {
        let percent = percent.clone();
        async move {
            let v = match req.uri().path() {
                "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
                "/api/v0/transfers/downloads" => json!([{ "username": "fast", "directories": [{ "files": [
                    { "id": "t1", "filename": r"Music\Beta\Moving.flac", "state": "InProgress",
                      "percentComplete": percent.load(Ordering::SeqCst) as f64 / 10.0 }] }] }]),
                _ => json!({}),
            };
            axum::Json(v).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

struct Server {
    base: String,
    http: reqwest::Client,
    st: Arc<AppState>,
    percent: Arc<AtomicU64>,
    _dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        tone(&music.join("Alpha/First/01 One.flac"));
        let percent = Arc::new(AtomicU64::new(400));
        let mut cfg = Config {
            data_dir: dir.path().join("data"),
            libraries: vec![LibraryConfig { name: "Music".into(), path: music, sentinel: None, scan_threads: None }],
            ..Config::default()
        };
        // nothing online: these tests never reach another server (and the downloads worker isn't started)
        cfg.catalog = false;
        cfg.deezer_url = "http://127.0.0.1:9".into();
        cfg.lyrics_online = false;
        cfg.covers_fetch = false;
        cfg.concerts = false;
        cfg.info_online = false;
        cfg.slskd_url = Some(fake_slskd(percent.clone()).await);
        cfg.slskd_api_key = Some("k".into());
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, ADMIN, PASS, true).await.unwrap();
        auth::create_user(&pool, &secrets, USER, USER_PASS, false).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        st.live.set_requests_every(Duration::from_millis(100));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        let s = Server { base, http: reqwest::Client::new(), st, percent, _dir: dir };
        s.requests().await;
        s
    }

    async fn uid(&self, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(name).fetch_one(&self.st.db).await.unwrap()
    }

    async fn sql(&self, q: &'static str) {
        sqlx::query(q).execute(&self.st.db).await.unwrap();
    }

    /// The user played "Trying" (on trial) and put "Moving" (downloading, 40%) in a playlist; the admin hearted
    /// "Mine".
    async fn requests(&self) {
        let (admin, user) = (self.uid(ADMIN).await, self.uid(USER).await);
        for (key, title, status) in [("dz-t-1", "Trying", "retry"), ("dz-t-2", "Moving", "queued"), ("dz-t-3", "Mine", "retry")] {
            sqlx::query("INSERT INTO request (key, title, artists_json, status, asked_at) VALUES (?, ?, '[\"Beta\"]', ?, 100)")
                .bind(key)
                .bind(title)
                .bind(status)
                .execute(&self.st.db)
                .await
                .unwrap();
        }
        self.sql("UPDATE request SET slsk_user = 'fast', slsk_file = 'Music\\Beta\\Moving.flac' WHERE key = 'dz-t-2'").await;
        sqlx::query("UPDATE request SET trial = 1, trial_at = 100, folder = 'Trial', trial_user = ? WHERE key = 'dz-t-1'")
            .bind(&user)
            .execute(&self.st.db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO request_want (key, user_id, playlist_id) VALUES ('dz-t-2', ?, 'pl1')").bind(&user).execute(&self.st.db).await.unwrap();
        sqlx::query("INSERT INTO request_want (key, user_id, star) VALUES ('dz-t-3', ?, 1)").bind(&admin).execute(&self.st.db).await.unwrap();
    }

    fn url(&self, user: &str, method: &str, params: &[(&str, &str)]) -> String {
        let pass = if user == USER { USER_PASS } else { PASS };
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{pass}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", user), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        format!("{}/rest/{method}.view?{}", self.base, q.finish())
    }

    async fn ok(&self, user: &str, method: &str, params: &[(&str, &str)]) -> Value {
        let r: Value = self.http.get(self.url(user, method, params)).send().await.unwrap().json().await.unwrap();
        let r = r["subsonic-response"].clone();
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }

    async fn open(&self, user: &str) -> Sse {
        let resp = self.http.get(self.url(user, "eventStream", &[])).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        let mut s = Sse { resp, buf: String::new() };
        let (kind, data) = s.event(Duration::from_secs(5)).await.expect("hello");
        assert_eq!(kind, "hello");
        assert!(data["events"].as_array().unwrap().contains(&json!("requests")), "{data}");
        s
    }
}

/// A stream read the way an app reads it (keep-alives and ids left out).
struct Sse {
    resp: reqwest::Response,
    buf: String,
}

impl Sse {
    /// The next event: (its name, its data), or None when nothing comes `within`.
    async fn event(&mut self, within: Duration) -> Option<(String, Value)> {
        let end = tokio::time::Instant::now() + within;
        loop {
            while let Some(i) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..i + 2).collect();
                let kind = block.lines().find_map(|l| l.strip_prefix("event: ")).unwrap_or("").to_string();
                let data = block.lines().find_map(|l| l.strip_prefix("data: ")).map(|d| serde_json::from_str(d).expect("data is JSON"));
                if !kind.is_empty() {
                    return Some((kind, data.unwrap_or(Value::Null)));
                }
            }
            match tokio::time::timeout_at(end, self.resp.chunk()).await {
                Ok(Ok(Some(b))) => self.buf.push_str(std::str::from_utf8(&b).unwrap()),
                _ => return None,
            }
        }
    }

    /// A `requests` event comes within a few seconds (others before it are read past).
    async fn hears(&mut self) {
        let end = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            match self.event(left).await {
                Some((k, data)) if k == "requests" => {
                    assert_eq!(data, json!({}), "a hint, nothing more");
                    return;
                }
                Some(_) => {}
                None => panic!("no requests event"),
            }
        }
    }

    /// No `requests` event for a while (the watcher has looked several times).
    async fn quiet(&mut self) {
        let end = tokio::time::Instant::now() + Duration::from_millis(800);
        loop {
            let left = end.saturating_duration_since(tokio::time::Instant::now());
            match self.event(left).await {
                Some((k, _)) if k == "requests" => panic!("an unexpected requests event"),
                Some(_) => {}
                None => return,
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

#[test]
fn the_event_is_recorded_with_the_others() {
    let shapes: Value =
        serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/api/shapes/eventStream.json")).unwrap()).unwrap();
    assert_eq!(shapes["requests"], json!(["object"]));
}

#[tokio::test]
async fn each_account_hears_of_its_own_requests_and_an_admin_of_all() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    let mut admin = s.open(ADMIN).await;
    let mut user = s.open(USER).await;
    // nothing changed: nobody hears anything
    user.quiet().await;
    admin.quiet().await;

    // the user's song on trial was found and is coming: both hear
    s.sql("UPDATE request SET status = 'queued' WHERE key = 'dz-t-1'").await;
    user.hears().await;
    admin.hears().await;

    // the admin's own song: only the admin hears
    s.sql("UPDATE request SET status = 'notfound' WHERE key = 'dz-t-3'").await;
    admin.hears().await;
    user.quiet().await;

    // a download moving on: not for every percent, for every 10
    s.percent.store(450, Ordering::SeqCst);
    user.quiet().await;
    s.percent.store(610, Ordering::SeqCst);
    user.hears().await;
    admin.hears().await;

    // the song arrived and was hearted (kept): the user hears
    let one: String = sqlx::query_scalar("SELECT id FROM track LIMIT 1").fetch_one(&s.st.db).await.unwrap();
    sqlx::query("UPDATE request SET status = 'done', local_id = ?, arrived_at = 200 WHERE key = 'dz-t-2'").bind(&one).execute(&s.st.db).await.unwrap();
    user.hears().await;
    s.sql("UPDATE request SET keep = 1 WHERE key = 'dz-t-1'").await;
    user.hears().await;
    admin.hears().await;

    // an admin's action through the API: the user whose song it is hears
    s.ok(ADMIN, "setRequestAside", &[("id", "dz-t-3")]).await;
    admin.hears().await;
    user.quiet().await;
    s.ok(ADMIN, "cancelRequest", &[("id", "dz-t-1")]).await;
    user.hears().await;
    let left = s.ok(USER, "getRequests", &[]).await;
    assert_eq!(left["requests"]["request"].as_array().unwrap().len(), 1, "{left}");

    // Lidarr's albums are the admin's alone
    s.sql("INSERT INTO lidarr_grab (album_id, title, artist, status, files_json, since) VALUES (5, 'Big', 'Gamma', 'retry', '[]', 1)").await;
    admin.hears().await;
    user.quiet().await;
}

#[tokio::test]
async fn nothing_is_looked_at_while_no_stream_is_open() {
    if !ffmpeg_or_skip() {
        return;
    }
    let s = Server::start().await;
    // a change before any stream: the stream opened after it doesn't hear of it (it fetches the list itself)
    s.sql("UPDATE request SET status = 'notfound' WHERE key = 'dz-t-1'").await;
    let mut user = s.open(USER).await;
    user.quiet().await;
    s.sql("UPDATE request SET status = 'retry' WHERE key = 'dz-t-1'").await;
    user.hears().await;
    drop(user);
    // gone: the watcher forgets, and a change meanwhile isn't told to the next stream
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while s.st.live.stream_count(None) > 0 {
        assert!(std::time::Instant::now() < end, "the stream stays open");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    s.sql("UPDATE request SET status = 'notfound' WHERE key = 'dz-t-1'").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut again = s.open(USER).await;
    again.quiet().await;
}
