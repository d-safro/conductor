//! Upgrades: databases made by older releases (tests/fixtures/upgrade/<tag>, made with tools/upgrade_fixture.py) are
//! opened by this version the way it starts (an old maestro.db renamed, the migrations run) and must answer as the
//! old release did: the same songs and ids, playlists in order (the smart one too), stars, ratings, plays, the play
//! queue, radio stations, bookmarks and both users' passwords. The music drive isn't there (its folders were
//! temporary), which an upgrade must also survive: nothing is marked missing.

use std::{path::Path, sync::Arc};

use conductor::{
    api::{self, AppState},
    auth::Secrets,
    config::{Config, LibraryConfig},
    db,
};
use serde_json::Value;

const USER: &str = "tester";
const PASS: &str = "s3cret pass";

struct Old {
    base: String,
    http: reqwest::Client,
    expected: Value,
    st: Arc<AppState>,
    _dir: tempfile::TempDir,
}

impl Old {
    async fn open(fixture: &Path) -> Old {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        for e in std::fs::read_dir(fixture).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), data.join(e.file_name())).unwrap();
        }
        let expected: Value = serde_json::from_str(&std::fs::read_to_string(data.join("expected.json")).unwrap()).unwrap();
        let cfg = Config { data_dir: data.clone(), catalog: false, lyrics_online: false, covers_fetch: false, concerts: false, info_online: false, ..Config::default() };
        // as at start: an old maestro.db gets the new name, then the migrations run
        conductor::config::move_old_names(&cfg);
        let pool = db::open(&data.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&data.join("secret.key")).unwrap();
        // the libraries as the old release had them (their folders are gone: the drive isn't connected)
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, root_path FROM library").fetch_all(&pool).await.unwrap();
        let libs: Vec<LibraryConfig> =
            rows.into_iter().map(|(name, root)| LibraryConfig { name, path: root.into(), sentinel: None, scan_threads: None }).collect();
        let cfg = Config { libraries: libs.clone(), ..cfg };
        let libs = conductor::library::ensure_libraries(&pool, &libs).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        let rep = conductor::library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        assert_eq!(rep.missing, 0, "a drive that isn't there marks nothing missing");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
        Old { base, http: reqwest::Client::new(), expected, st, _dir: dir }
    }

    async fn call_as(&self, user: &str, pass: &str, method: &str, params: &[(&str, &str)]) -> Value {
        let mut q = form_urlencoded::Serializer::new(String::new());
        let tok = format!("{:x}", md5::compute(format!("{pass}salt1")));
        q.extend_pairs([("u", user), ("t", tok.as_str()), ("s", "salt1"), ("v", "1.16.1"), ("c", "upgrade"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        let r: Value = self.http.get(format!("{}/rest/{method}?{}", self.base, q.finish())).send().await.unwrap().json().await.unwrap();
        r["subsonic-response"].clone()
    }

    async fn ok(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let r = self.call_as(USER, PASS, method, params).await;
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array().cloned().unwrap_or_default().iter().map(|x| x["id"].as_str().unwrap_or("").to_string()).collect()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()
}

async fn check(fixture: &Path) {
    let o = Old::open(fixture).await;
    let e = &o.expected;
    let from = e["made_by"].as_str().unwrap();

    // the same songs, with the same ids
    let mut songs: Vec<(String, String)> = o.ok("search3", &[("query", ""), ("songCount", "50")]).await["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["title"].as_str().unwrap().to_string(), s["id"].as_str().unwrap().to_string()))
        .collect();
    songs.sort();
    let mut want: Vec<(String, String)> = e["songs"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    want.sort();
    assert_eq!(songs, want, "{from}: songs and ids");

    // playlists, in order; the smart one still fills itself
    let pls = o.ok("getPlaylists", &[]).await["playlists"]["playlist"].clone();
    for (name, p) in e["playlists"].as_object().unwrap() {
        let id = pls.as_array().unwrap().iter().find(|x| x["name"] == name.as_str()).unwrap_or_else(|| panic!("{from}: playlist {name}"))["id"]
            .as_str()
            .unwrap()
            .to_string();
        let got = o.ok("getPlaylist", &[("id", &id)]).await;
        assert_eq!(ids(&got["playlist"]["entry"]), strings(&p["songs"]), "{from}: playlist {name}'s songs");
    }

    let st = o.ok("getStarred2", &[]).await;
    assert_eq!(ids(&st["starred2"]["song"]), strings(&e["starred_songs"]), "{from}: starred songs");
    assert_eq!(ids(&st["starred2"]["album"]), strings(&e["starred_albums"]), "{from}: starred albums");
    let two = e["songs"]["Two"].as_str().unwrap();
    let one = e["songs"]["One"].as_str().unwrap();
    assert_eq!(o.ok("getSong", &[("id", two)]).await["song"]["userRating"], e["rating_two"], "{from}: rating");
    assert_eq!(o.ok("getSong", &[("id", one)]).await["song"]["playCount"], e["plays_one"], "{from}: plays");

    let q = o.ok("getPlayQueue", &[]).await;
    assert_eq!(q["playQueue"]["current"], e["queue"]["current"], "{from}: queue's current song");
    assert_eq!(q["playQueue"]["position"], e["queue"]["position"], "{from}: queue's position");
    assert_eq!(ids(&q["playQueue"]["entry"]), strings(&e["queue_songs"]), "{from}: queue's songs");
    // and the newer index-based view of it works on the old queue
    assert_eq!(o.ok("getPlayQueueByIndex", &[]).await["playQueueByIndex"]["currentIndex"], 1, "{from}: queue by index");

    let radio: Vec<String> = o.ok("getInternetRadioStations", &[]).await["internetRadioStations"]["internetRadioStation"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(radio, strings(&e["radio"]), "{from}: radio stations");
    let bm = o.ok("getBookmarks", &[]).await;
    let bm: Vec<Value> =
        bm["bookmarks"]["bookmark"].as_array().unwrap().iter().map(|b| serde_json::json!([b["entry"]["id"], b["position"]])).collect();
    assert_eq!(Value::Array(bm), e["bookmarks"], "{from}: bookmarks");

    // both users still sign in with their passwords (the old key decrypts them)
    assert_eq!(o.call_as("second", "second pass", "ping", &[]).await["status"], "ok", "{from}: the second user");
    assert_eq!(o.call_as(USER, "wrong", "ping", &[]).await["error"]["code"], 40);

    // what's new works on the old data: a backup of it, users over the API
    let out = o._dir.path().join("b.zip");
    conductor::backup::make(&o.st.cfg, &o.st.db, &out).await.unwrap();
    assert_eq!(o.ok("getUsers", &[]).await["users"]["user"].as_array().unwrap().len(), 2, "{from}: users");
}

#[tokio::test]
async fn every_older_release_upgrades() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade");
    let mut n = 0;
    for e in std::fs::read_dir(&root).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            check(&p).await;
            n += 1;
        }
    }
    assert!(n >= 2, "fixtures in {}", root.display());
}
