//! `encore.requests`: the songs asked for and Lidarr's albums, listed and acted on over the Subsonic API. A server
//! in-process (as in tests/api.rs, with its own small harness here), an admin and a user, a stand-in for slskd, and
//! requests put straight into the database in every state.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
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

/// What the stand-in for slskd was asked to download: (peer, file).
type Sent = Arc<Mutex<Vec<(String, String)>>>;

/// slskd, as Conductor uses it: logged in, a search that finds two files for "Four", and two downloads under way
/// (one at 40%, one waiting in the peer's queue).
async fn fake_slskd() -> (String, Sent) {
    use axum::{extract::Request, response::IntoResponse};
    let sent: Sent = Arc::default();
    let s2 = sent.clone();
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = axum::Router::new().fallback(move |req: Request| {
        let sent = s2.clone();
        async move {
            let (m, path) = (req.method().clone(), req.uri().path().to_string());
            let body = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap();
            let v = match (m.as_str(), path.as_str()) {
                (_, "/api/v0/application") => json!({ "server": { "isLoggedIn": true } }),
                ("POST", "/api/v0/searches") => json!({}),
                ("GET", p) if p.ends_with("/responses") => json!([{ "username": "peer", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
                    "files": [{ "filename": r"Music\Alpha\Second\04 Four.flac", "size": 1000, "bitRate": 0, "length": 2, "extension": "flac" },
                              { "filename": r"Music\Alpha\Live\Four (Live).mp3", "size": 900, "bitRate": 320, "length": 2 },
                              { "filename": r"Music\Alpha\Second\cover.jpg", "size": 9 }] }]),
                ("GET", p) if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
                ("POST", p) if p.starts_with("/api/v0/transfers/downloads/") => {
                    let files: Value = serde_json::from_slice(&body).unwrap_or_default();
                    let peer = p.rsplit('/').next().unwrap().to_string();
                    sent.lock().unwrap().push((peer, files[0]["filename"].as_str().unwrap_or("").to_string()));
                    json!({})
                }
                ("GET", "/api/v0/transfers/downloads") => json!([
                    { "username": "fast", "directories": [{ "files": [
                        { "id": "t1", "filename": r"Music\Beta\Moving.flac", "state": "InProgress", "percentComplete": 40.4 }] }] },
                    { "username": "slow", "directories": [{ "files": [
                        { "id": "t2", "filename": r"Music\Beta\Waiting.flac", "state": "Queued, Remotely", "percentComplete": 0.0 }] }] },
                    { "username": "albums", "directories": [{ "files": [
                        { "id": "t3", "filename": r"Share\Gamma - Big\01.flac", "state": "Completed, Succeeded", "percentComplete": 100.0 },
                        { "id": "t4", "filename": r"Share\Gamma - Big\02.flac", "state": "InProgress", "percentComplete": 50.0 }] }] }]),
                _ => json!({}),
            };
            axum::Json(v).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, sent)
}

struct Server {
    base: String,
    http: reqwest::Client,
    st: Arc<AppState>,
    sent: Sent,
    _dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        tone(&music.join("Alpha/First/01 One.flac"), "One", 1);
        let (slskd, sent) = fake_slskd().await;
        let mut cfg = Config {
            data_dir: dir.path().join("data"),
            libraries: vec![LibraryConfig { name: "Music".into(), path: music, sentinel: None, scan_threads: None }],
            ..Config::default()
        };
        // nothing online: these tests never reach another server
        cfg.catalog = false;
        cfg.deezer_url = "http://127.0.0.1:9".into();
        cfg.lyrics_online = false;
        cfg.covers_fetch = false;
        cfg.concerts = false;
        cfg.info_online = false;
        cfg.slskd_url = Some(slskd);
        cfg.slskd_api_key = Some("k".into());
        cfg.slskd_fast_test = true;
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        auth::create_user(&pool, &secrets, OTHER, OTHER_PASS, false).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        let s = Server { base, http: reqwest::Client::new(), st, sent, _dir: dir };
        s.requests().await;
        s
    }

    async fn uid(&self, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(name).fetch_one(&self.st.db).await.unwrap()
    }

    /// One request in each state, asked for by the admin (hearts), the user (a playlist, a trial, a mix) or a sync.
    async fn requests(&self) {
        let (me, them) = (self.uid(USER).await, self.uid(OTHER).await);
        let one: String = sqlx::query_scalar("SELECT id FROM track LIMIT 1").fetch_one(&self.st.db).await.unwrap();
        // (key, title, status, found, asked, extra columns)
        let rows: &[(&str, &str, &str, bool, i64, &str)] = &[
            ("dz-t-1", "Looking", "retry", false, 100, ""),
            ("dz-t-2", "Moving", "queued", false, 200, "slsk_user = 'fast', slsk_file = 'Music\\Beta\\Moving.flac'"),
            ("dz-t-3", "Waiting", "queued", false, 300, "slsk_user = 'slow', slsk_file = 'Music\\Beta\\Waiting.flac'"),
            ("dz-t-4", "Here", "done", true, 400, "rel = 'Songs/Beta/Beta - Here.flac', arrived_at = 450, slsk_user = 'p', slsk_file = 'x\\Here.flac'"),
            ("dz-t-5", "Nowhere", "notfound", false, 500, "searches = 4"),
            ("dz-t-6", "Unsent", "notfound", true, 600, "searches = 8, tried_json = '[\"a\"]'"),
            ("dz-t-7", "Aside", "skipped", false, 700, "skipped_from = 'notfound'"),
            ("dz-t-8", "Trying", "retry", false, 800, "trial = 1, trial_at = 800, folder = 'Trial'"),
            ("dz-t-9", "Mixed", "retry", false, 900, "trial = 1, trial_at = 900, folder = 'Trial', mix = 'discover', trial_hours = 168"),
            ("dz-t-10", "Discovered", "retry", false, 1000, "folder = 'Discovery'"),
            ("dz-t-11", "Four", "notfound", true, 1100, "album = 'Second', duration_ms = 2000, searches = 8"),
        ];
        for (key, title, status, found, asked, extra) in rows {
            sqlx::query("INSERT INTO request (key, title, artists_json, status, found, asked_at) VALUES (?, ?, '[\"Beta\", \"Guest\"]', ?, ?, ?)")
                .bind(key)
                .bind(title)
                .bind(status)
                .bind(found)
                .bind(asked)
                .execute(&self.st.db)
                .await
                .unwrap();
            if !extra.is_empty() {
                sqlx::query(sqlx::AssertSqlSafe(format!("UPDATE request SET {extra} WHERE key = ?"))).bind(key).execute(&self.st.db).await.unwrap();
            }
        }
        sqlx::query("UPDATE request SET local_id = ? WHERE key = 'dz-t-4'").bind(&one).execute(&self.st.db).await.unwrap();
        sqlx::query("UPDATE request SET artists_json = '[\"Alpha\"]' WHERE key = 'dz-t-11'").execute(&self.st.db).await.unwrap();
        // the admin hearted 1–7 and 11; the user put 2 and 5 in a playlist, played 8, has 9 in a mix
        for k in ["dz-t-1", "dz-t-2", "dz-t-3", "dz-t-4", "dz-t-5", "dz-t-6", "dz-t-7", "dz-t-11"] {
            sqlx::query("INSERT INTO request_want (key, user_id, star) VALUES (?, ?, 1)").bind(k).bind(&me).execute(&self.st.db).await.unwrap();
        }
        for k in ["dz-t-2", "dz-t-5"] {
            sqlx::query("INSERT INTO request_want (key, user_id, playlist_id) VALUES (?, ?, 'pl1')").bind(k).bind(&them).execute(&self.st.db).await.unwrap();
        }
        sqlx::query("UPDATE request SET trial_user = ? WHERE key IN ('dz-t-8', 'dz-t-9')").bind(&them).execute(&self.st.db).await.unwrap();
        // Lidarr's albums: one downloading (half done), one done, one it gave up on
        sqlx::query(
            "INSERT INTO lidarr_grab (album_id, title, artist, status, slsk_user, folder, files_json, searches, since, error) VALUES
             (31, 'Big', 'Gamma', 'downloading', 'albums', 'Share\\Gamma - Big', '[\"Share\\\\Gamma - Big\\\\01.flac\", \"Share\\\\Gamma - Big\\\\02.flac\"]', 1, 2000, ''),
             (32, 'Small', 'Gamma', 'done', 'albums', 'Share\\Gamma - Small', '[]', 1, 50, ''),
             (33, 'Lost', 'Delta', 'notfound', NULL, NULL, '[]', 6, 0, 'Lidarr refused the import')",
        )
        .execute(&self.st.db)
        .await
        .unwrap();
    }

    fn url(&self, user: &str, method: &str, params: &[(&str, &str)]) -> String {
        let pass = if user == OTHER { OTHER_PASS } else { PASS };
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{pass}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", user), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        format!("{}/rest/{method}.view?{}", self.base, q.finish())
    }

    async fn call_as(&self, user: &str, method: &str, params: &[(&str, &str)]) -> Value {
        let r: Value = self.http.get(self.url(user, method, params)).send().await.unwrap().json().await.unwrap();
        let r = r["subsonic-response"].clone();
        contract::observe(method, &r);
        r
    }

    async fn ok_as(&self, user: &str, method: &str, params: &[(&str, &str)]) -> Value {
        let r = self.call_as(user, method, params).await;
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }

    async fn ok(&self, method: &str, params: &[(&str, &str)]) -> Value {
        self.ok_as(USER, method, params).await
    }

    /// The error code a call answers with.
    async fn fails_as(&self, user: &str, method: &str, params: &[(&str, &str)]) -> i64 {
        let r = self.call_as(user, method, params).await;
        assert_eq!(r["status"], "failed", "{method} should fail: {r}");
        r["error"]["code"].as_i64().unwrap()
    }

    async fn list(&self, user: &str, params: &[(&str, &str)]) -> Value {
        self.ok_as(user, "getRequests", params).await["requests"].clone()
    }
}

fn ids(v: &Value) -> Vec<String> {
    v["request"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
}

fn find<'a>(v: &'a Value, id: &str) -> &'a Value {
    v["request"].as_array().unwrap().iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("{id} not listed: {v}"))
}

#[tokio::test]
async fn listed_as_podium_shows_them() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let exts = s.ok("getOpenSubsonicExtensions", &[]).await;
    let ext = exts["openSubsonicExtensions"].as_array().unwrap().iter().find(|e| e["name"] == "encore.requests").cloned();
    assert_eq!(ext.expect("encore.requests listed")["versions"], json!([1]));

    let all = s.list(USER, &[("count", "500")]).await;
    assert_eq!(all["total"], 14, "11 songs and 3 albums: {all}");
    // newest first: songs by when they were asked for, albums by their last download
    assert_eq!(ids(&all)[..3], ["lidarr-31", "dz-t-11", "dz-t-10"]);
    let state = |id: &str| find(&all, id)["state"].as_str().unwrap().to_string();
    for (id, want) in [
        ("dz-t-1", "searching"),
        ("dz-t-2", "downloading"),
        ("dz-t-3", "queued"),
        ("dz-t-4", "done"),
        ("dz-t-5", "notfound"),
        ("dz-t-6", "failed"),
        ("dz-t-7", "setAside"),
        ("lidarr-31", "downloading"),
        ("lidarr-32", "done"),
        ("lidarr-33", "failed"),
    ] {
        assert_eq!(state(id), want, "{id}");
    }
    assert_eq!(
        all["counts"],
        json!({ "searching": 4, "queued": 1, "downloading": 2, "done": 2, "notfound": 1, "failed": 3, "setAside": 1 })
    );
    assert_eq!((all["onTrial"].as_i64(), all["canPick"].as_bool(), all["downloadsPaused"].as_bool()), (Some(2), Some(true), Some(false)));

    // a download under way, with slskd's progress; one waiting in the peer's queue
    let moving = find(&all, "dz-t-2");
    assert_eq!((moving["progress"].as_f64(), moving["transfer"].as_str()), (Some(40.0), Some("InProgress")));
    assert_eq!((moving["from"].as_str(), moving["file"].as_str()), (Some("fast"), Some("Moving.flac")));
    assert_eq!(find(&all, "dz-t-3")["transfer"], "Queued, Remotely");
    assert_eq!(find(&all, "lidarr-31")["progress"], 75.0, "one file done, one half: {all}");
    // who asked and why, trial or kept
    let why = |id: &str| -> (Value, Value) {
        let r = find(&all, id);
        (r["reasons"].clone(), r["users"].clone())
    };
    assert_eq!(why("dz-t-2"), (json!(["heart", "playlist"]), json!([OTHER, USER])));
    assert_eq!(why("dz-t-8"), (json!(["trial"]), json!([OTHER])));
    assert_eq!(why("dz-t-9"), (json!(["mix"]), json!([OTHER])));
    assert_eq!(why("dz-t-10"), (json!(["discovery"]), json!([])));
    assert_eq!(why("lidarr-33"), (json!(["lidarr"]), json!([])));
    let trying = find(&all, "dz-t-8");
    assert_eq!((trying["trial"].as_bool(), trying["kept"].as_bool(), trying["folder"].as_str()), (Some(true), Some(false), Some("Trial")));
    assert_eq!(find(&all, "dz-t-9")["mix"], "discover");
    let here = find(&all, "dz-t-4");
    assert_eq!((here["kept"].as_bool(), here["folder"].as_str(), here["path"].as_str()), (Some(true), Some("Songs"), Some("Songs/Beta/Beta - Here.flac")));
    assert!(here["songId"].is_string() && here["arrived"] == "1970-01-01T00:07:30Z", "{here}");
    assert_eq!((here["artist"].as_str(), here["artists"].clone()), (Some("Beta, Guest"), json!(["Beta", "Guest"])));
    assert_eq!(here["actions"], json!([]), "nothing to do: it's here");
    assert_eq!(find(&all, "dz-t-5")["actions"], json!(["retry", "cancel", "setAside", "search", "pick"]));
    assert_eq!(find(&all, "dz-t-7")["actions"], json!(["retry", "cancel", "bringBack", "search", "pick"]));
    assert_eq!(find(&all, "lidarr-33")["note"], "Lidarr refused the import");
    assert_eq!(find(&all, "dz-t-11")["query"], "Alpha Four");

    // filters: by state (repeated or comma-separated), by type; paging
    let f = s.list(USER, &[("state", "failed,notfound"), ("state", "setAside")]).await;
    assert_eq!(ids(&f), ["dz-t-11", "dz-t-7", "dz-t-6", "dz-t-5", "lidarr-33"]);
    assert_eq!(f["total"], 5);
    assert_eq!(f["counts"], all["counts"], "the counts are of everything, not of the page");
    assert_eq!(ids(&s.list(USER, &[("type", "album")]).await), ["lidarr-31", "lidarr-32", "lidarr-33"]);
    let songs = s.list(USER, &[("type", "song"), ("count", "0")]).await;
    assert_eq!((songs["total"].as_i64(), songs["request"].as_array().map(Vec::len)), (Some(11), Some(0)));
    let p1 = s.list(USER, &[("type", "song"), ("count", "4")]).await;
    let p2 = s.list(USER, &[("type", "song"), ("count", "4"), ("offset", "4")]).await;
    let p3 = s.list(USER, &[("type", "song"), ("count", "4"), ("offset", "8")]).await;
    assert_eq!((ids(&p1).len(), ids(&p2).len(), ids(&p3).len()), (4, 4, 3));
    let paged: BTreeSet<String> = [ids(&p1), ids(&p2), ids(&p3)].concat().into_iter().collect();
    assert_eq!(paged.len(), 11, "each song once");
    assert_eq!(p2["offset"], 4);
    assert_eq!(s.fails_as(USER, "getRequests", &[("state", "lost")]).await, 0);
    assert_eq!(s.fails_as(USER, "getRequests", &[("type", "video")]).await, 0);
}

#[tokio::test]
async fn a_user_sees_only_their_own() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let mine = s.list(OTHER, &[]).await;
    assert_eq!(ids(&mine), ["dz-t-9", "dz-t-8", "dz-t-5", "dz-t-2"], "their playlist's, their trial and mix songs; no albums");
    for r in mine["request"].as_array().unwrap() {
        assert_eq!(r["users"], json!([OTHER]), "only their own name: {r}");
        assert_eq!(r["actions"], json!([]));
        assert!(r.get("from").is_none() && r.get("file").is_none() && r.get("path").is_none(), "no peers or files: {r}");
    }
    assert_eq!(find(&mine, "dz-t-2")["state"], "downloading", "the progress, though");
    assert_eq!(mine["counts"]["searching"], 2);
    assert_eq!(mine["canPick"], false);
    // asking about someone else: only an admin
    assert_eq!(s.fails_as(OTHER, "getRequests", &[("username", USER)]).await, 50);
    assert_eq!(ids(&s.list(OTHER, &[("username", OTHER)]).await).len(), 4);
    let theirs = s.list(USER, &[("username", OTHER)]).await;
    assert_eq!(ids(&theirs), ids(&mine), "an admin asking about the user sees what they asked for");
    assert_eq!(find(&theirs, "dz-t-2")["users"], json!([OTHER, USER]));
    assert_eq!(s.fails_as(USER, "getRequests", &[("username", "nobody")]).await, 70);
}

#[tokio::test]
async fn only_admins_act() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    for (m, p) in [
        ("retryRequest", vec![("id", "dz-t-5")]),
        ("cancelRequest", vec![("id", "dz-t-5")]),
        ("setRequestAside", vec![("id", "dz-t-5")]),
        ("searchRequestFiles", vec![("id", "dz-t-5"), ("query", "beta nowhere")]),
        ("pickRequestFile", vec![("id", "dz-t-5"), ("user", "peer"), ("file", "x.flac"), ("size", "1")]),
    ] {
        assert_eq!(s.fails_as(OTHER, m, &p).await, 50, "{m}");
    }
    assert_eq!(s.list(USER, &[]).await["total"], 14, "nothing changed");
    assert!(s.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn admin_actions() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    // set aside and bring back
    let r = s.ok("setRequestAside", &[("id", "dz-t-5")]).await;
    assert_eq!((r["request"]["state"].as_str(), r["request"]["actions"].clone()), (Some("setAside"), json!(["retry", "cancel", "bringBack", "search", "pick"])));
    assert_eq!(s.fails_as(USER, "setRequestAside", &[("id", "dz-t-5")]).await, 0, "already set aside");
    assert_eq!(s.ok("setRequestAside", &[("id", "dz-t-5"), ("aside", "false")]).await["request"]["state"], "notfound");
    assert_eq!(s.fails_as(USER, "setRequestAside", &[("id", "dz-t-2")]).await, 0, "a download under way isn't set aside");
    // retry: from the start
    let r = s.ok("retryRequest", &[("id", "dz-t-6")]).await;
    assert_eq!((r["request"]["state"].as_str(), r["request"]["searches"].as_i64()), (Some("searching"), Some(0)));
    assert_eq!(s.fails_as(USER, "retryRequest", &[("id", "dz-t-4")]).await, 0, "it's here");
    assert_eq!(s.ok("retryRequest", &[("id", "lidarr-33")]).await["request"]["state"], "searching");
    assert_eq!(s.fails_as(USER, "retryRequest", &[("id", "lidarr-32")]).await, 0, "a done album");
    assert_eq!(s.fails_as(USER, "retryRequest", &[("id", "dz-t-404")]).await, 70);
    assert_eq!(s.fails_as(USER, "retryRequest", &[]).await, 10);
    // cancel
    s.ok("cancelRequest", &[("id", "dz-t-1")]).await;
    assert!(!ids(&s.list(USER, &[("count", "500")]).await).contains(&"dz-t-1".to_string()));
    assert_eq!(s.fails_as(USER, "cancelRequest", &[("id", "dz-t-1")]).await, 70);
    assert_eq!(s.fails_as(USER, "cancelRequest", &[("id", "dz-t-4")]).await, 0, "it's here");
    // search Soulseek (the song's own words by default; at least 3 letters), then pick a file
    let r = s.ok("searchRequestFiles", &[("id", "dz-t-11")]).await;
    let found = &r["requestFiles"];
    assert_eq!((found["query"].as_str(), found["total"].as_i64()), (Some("Alpha Four"), Some(2)), "no pictures: {found}");
    let best = &found["file"][0];
    assert_eq!((best["name"].as_str(), best["match"].as_bool(), best["titleOk"].as_bool()), (Some("04 Four.flac"), Some(true), Some(true)));
    assert_eq!(found["file"][1]["match"], false, "a live version isn't what was asked for, but may still be picked");
    assert_eq!(s.fails_as(USER, "searchRequestFiles", &[("id", "dz-t-11"), ("query", "al")]).await, 0);
    assert_eq!(s.fails_as(USER, "searchRequestFiles", &[("id", "dz-t-404"), ("query", "alpha")]).await, 70);
    let (user, file, size) = (best["user"].as_str().unwrap(), best["filename"].as_str().unwrap(), best["size"].to_string());
    let r = s.ok("pickRequestFile", &[("id", "dz-t-11"), ("user", user), ("file", file), ("size", &size)]).await;
    assert_eq!((r["request"]["state"].as_str(), r["request"]["manual"].as_bool()), (Some("queued"), Some(true)));
    assert_eq!(*s.sent.lock().unwrap(), [("peer".to_string(), file.to_string())]);
    assert_eq!(s.fails_as(USER, "pickRequestFile", &[("id", "dz-t-11"), ("user", user), ("file", file), ("size", &size)]).await, 0, "not waiting any more");
    // Podium's own list agrees (it's the same code)
    let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let api = |p: &str| format!("{}/admin/api/{p}", s.base);
    let login = json!({ "username": USER, "password": PASS });
    assert_eq!(http.post(api("login")).header("X-Conductor", "1").json(&login).send().await.unwrap().status(), 200);
    let pick: Value = http.get(api("pick")).send().await.unwrap().json().await.unwrap();
    let four = pick["songs"].as_array().unwrap().iter().find(|x| x["key"] == "dz-t-11").unwrap();
    assert_eq!((four["group"].as_str(), four["status"].as_str()), (Some("downloading"), Some("queued")));
}

/// The answers' shapes, recorded in docs/api/shapes/ and checked as tests/api.rs's `contract` module does (a field
/// may be added, never dropped or retyped). Record: CONDUCTOR_BLESS=1 cargo test --test requests.
mod contract {
    use super::*;

    type Shape = BTreeMap<String, BTreeSet<String>>;
    static SEEN: Mutex<BTreeMap<String, Shape>> = Mutex::new(BTreeMap::new());
    static COMMITTED: Mutex<BTreeMap<String, Shape>> = Mutex::new(BTreeMap::new());
    const ENVELOPE: &[&str] = &["status", "version", "type", "serverVersion", "openSubsonic", "error"];

    fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/api/shapes")
    }

    fn key(k: &str) -> &str {
        let id_like = k.len() >= 16 && k.chars().any(|c| c.is_ascii_digit()) && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if k.starts_with(|c: char| c.is_ascii_digit()) || id_like { "{key}" } else { k }
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
            Value::Object(o) => o.iter().for_each(|(k, x)| walk(x, &format!("{path}.{}", key(k)), out)),
            _ => {}
        }
    }

    /// Only this extension's calls: the others are recorded by tests/api.rs.
    const CALLS: &[&str] = &["getRequests", "retryRequest", "cancelRequest", "setRequestAside", "searchRequestFiles", "pickRequestFile"];

    pub fn observe(method: &str, r: &Value) {
        if !CALLS.contains(&method) || r["status"] != "ok" {
            return;
        }
        let mut shape = Shape::new();
        for (k, v) in r.as_object().into_iter().flatten() {
            if !ENVELOPE.contains(&k.as_str()) {
                walk(v, k, &mut shape);
            }
        }
        let file = dir().join(format!("{method}.json"));
        if std::env::var_os("CONDUCTOR_BLESS").is_some() {
            let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
            let mut committed = COMMITTED.lock().unwrap_or_else(|e| e.into_inner());
            let before = committed
                .entry(method.to_string())
                .or_insert_with(|| std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default());
            let all = seen.entry(method.to_string()).or_default();
            for (p, t) in shape {
                all.entry(p).or_default().extend(t);
            }
            let mut merged = before.clone();
            for (p, t) in all.iter() {
                merged.entry(p.clone()).or_default().extend(t.iter().cloned());
            }
            std::fs::write(&file, serde_json::to_string_pretty(&merged).unwrap() + "\n").unwrap();
            return;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            panic!("{method}: no recorded shape in docs/api/shapes/ (record it: CONDUCTOR_BLESS=1 cargo test --test requests)");
        };
        let known: Shape = serde_json::from_str(&text).unwrap();
        for (p, t) in &shape {
            match known.get(p) {
                None => panic!("{method}: the field {p} isn't recorded (a new field: CONDUCTOR_BLESS=1 cargo test --test requests, and document it)"),
                Some(k) if !t.is_subset(k) => panic!("{method}: {p} is {t:?}, recorded as {k:?}"),
                _ => {}
            }
        }
    }
}
