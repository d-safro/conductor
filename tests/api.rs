//! End-to-end: make a tiny music folder with ffmpeg, start the server in-process, talk Subsonic to it.

use std::{collections::HashMap, path::Path, process::Command, sync::Arc};

use encore_server::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
};
use serde_json::Value;
use tokio::sync::Mutex;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn tone(path: &Path, title: &str, artist: &str, album: &str, track: u32, secs: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={}:duration={secs}", 220 * track)])
        .args(["-metadata", &format!("title={title}"), "-metadata", &format!("artist={artist}")])
        .args(["-metadata", &format!("album={album}"), "-metadata", &format!("track={track}"), "-metadata", "date=2021"])
        .args(["-metadata", "genre=Test"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg failed for {}", path.display());
}

fn cover(path: &Path) {
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", "color=c=red:s=600x600", "-frames:v", "1"])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok);
}

struct Server {
    base: String,
    http: reqwest::Client,
    st: Arc<AppState>,
    _dir: tempfile::TempDir,
    music: std::path::PathBuf,
}

const USER: &str = "tester";
const PASS: &str = "s3cret pass";

impl Server {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        tone(&music.join("Alpha/First Album/01 One.flac"), "One", "Alpha", "First Album", 1, 3);
        tone(&music.join("Alpha/First Album/02 Two.flac"), "Two", "Alpha", "First Album", 2, 3);
        tone(&music.join("The Beta Band/Second/01 Three.mp3"), "Three", "The Beta Band", "Second", 1, 3);
        cover(&music.join("Alpha/First Album/cover.jpg"));
        std::fs::write(music.join("Alpha/First Album/01 One.lrc"), "[00:00.50]hello\n[00:01.75]world\n").unwrap();

        let cfg = Config {
            data_dir: dir.path().join("data"),
            libraries: vec![LibraryConfig { name: "Music".into(), path: music.clone(), sentinel: None, scan_threads: None }],
            ..Config::default()
        };
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("encore.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let scan = Arc::new(library::ScanStatus::default());
        let rep = library::scan(&pool, &libs, &scan, false).await.unwrap();
        assert_eq!(rep.added, 3);
        let st = Arc::new(AppState { db: pool, secrets, cfg, libs, scan, now_playing: Mutex::new(HashMap::new()) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Server { base, http: reqwest::Client::new(), st, _dir: dir, music }
    }

    fn url(&self, method: &str, params: &[(&str, &str)]) -> String {
        let salt = "abc123";
        let token = format!("{:x}", md5::compute(format!("{PASS}{salt}")));
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", USER), ("t", token.as_str()), ("s", salt), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        q.extend_pairs(params.iter().copied());
        format!("{}/rest/{method}.view?{}", self.base, q.finish())
    }

    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let r: Value = self.http.get(self.url(method, params)).send().await.unwrap().json().await.unwrap();
        r["subsonic-response"].clone()
    }

    async fn ok(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let r = self.call(method, params).await;
        assert_eq!(r["status"], "ok", "{method}: {r}");
        r
    }
}

#[tokio::test]
async fn subsonic_end_to_end() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;

    // login
    let r = s.ok("ping", &[]).await;
    assert_eq!(r["openSubsonic"], true);
    let bad: Value =
        s.http.get(format!("{}/rest/ping?u={USER}&p=wrong&f=json", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(bad["subsonic-response"]["error"]["code"], 40);
    let plain: Value = s
        .http
        .get(format!("{}/rest/ping?u={USER}&p=enc:{}&f=json", s.base, hex::encode(PASS)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(plain["subsonic-response"]["status"], "ok");
    let key = auth::create_api_key(&s.st.db, USER, "test").await.unwrap();
    let k: Value = s.http.get(format!("{}/rest/ping?apiKey={key}&f=json", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(k["subsonic-response"]["status"], "ok");
    let xml = s.http.get(s.url("ping", &[("f", "xml")]).replace("f=json&", "")).send().await.unwrap().text().await.unwrap();
    assert!(xml.contains(r#"<subsonic-response xmlns="http://subsonic.org/restapi""#) && xml.contains(r#"status="ok""#), "{xml}");

    // browse
    let r = s.ok("getArtists", &[]).await;
    let idx = r["artists"]["index"].as_array().unwrap();
    let names: Vec<&str> =
        idx.iter().flat_map(|i| i["artist"].as_array().unwrap()).map(|a| a["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Alpha", "The Beta Band"]); // "The" ignored for sorting: B after A
    assert_eq!(idx[1]["name"], "B");
    let alpha = idx[0]["artist"][0]["id"].as_str().unwrap().to_string();
    let r = s.ok("getArtist", &[("id", &alpha)]).await;
    let album_id = r["artist"]["album"][0]["id"].as_str().unwrap().to_string();
    let r = s.ok("getAlbum", &[("id", &album_id)]).await;
    let songs = r["album"]["song"].as_array().unwrap();
    assert_eq!(songs.len(), 2);
    assert_eq!(songs[0]["title"], "One");
    assert_eq!(songs[0]["year"], 2021);
    assert_eq!(r["album"]["songCount"], 2);
    let one = songs[0]["id"].as_str().unwrap().to_string();
    let two = songs[1]["id"].as_str().unwrap().to_string();

    let r = s.ok("search3", &[("query", "thre")]).await;
    assert_eq!(r["searchResult3"]["song"][0]["title"], "Three");
    let r = s.ok("search3", &[("query", "\"\""), ("songCount", "100")]).await;
    assert_eq!(r["searchResult3"]["song"].as_array().unwrap().len(), 3);
    let r = s.ok("getAlbumList2", &[("type", "alphabeticalByName")]).await;
    assert_eq!(r["albumList2"]["album"].as_array().unwrap().len(), 2);
    let r = s.ok("getGenres", &[]).await;
    assert_eq!(r["genres"]["genre"][0]["value"], "Test");
    assert_eq!(r["genres"]["genre"][0]["songCount"], 3);
    s.ok("getRandomSongs", &[("size", "2")]).await;
    s.ok("getIndexes", &[]).await;
    s.ok("getMusicDirectory", &[("id", &album_id)]).await;

    // stars, ratings, plays
    s.ok("star", &[("id", &one), ("albumId", &album_id)]).await;
    s.ok("setRating", &[("id", &one), ("rating", "4")]).await;
    s.ok("scrobble", &[("id", &one), ("submission", "true")]).await;
    s.ok("scrobble", &[("id", &two), ("submission", "false")]).await;
    let r = s.ok("getStarred2", &[]).await;
    assert_eq!(r["starred2"]["song"][0]["id"], one);
    assert_eq!(r["starred2"]["song"][0]["userRating"], 4);
    assert_eq!(r["starred2"]["song"][0]["playCount"], 1);
    assert_eq!(r["starred2"]["album"][0]["id"], album_id);
    let r = s.ok("getNowPlaying", &[]).await;
    assert_eq!(r["nowPlaying"]["entry"][0]["id"], two);
    s.ok("unstar", &[("id", &one)]).await;
    let r = s.ok("getStarred2", &[]).await;
    assert!(r["starred2"]["song"].as_array().unwrap().is_empty());
    let r = s.ok("getAlbumList2", &[("type", "frequent")]).await;
    assert_eq!(r["albumList2"]["album"][0]["id"], album_id);

    // playlists
    let r = s.ok("createPlaylist", &[("name", "Mix"), ("songId", &two), ("songId", &one)]).await;
    let pid = r["playlist"]["id"].as_str().unwrap().to_string();
    assert_eq!(r["playlist"]["entry"][0]["id"], two);
    s.ok("updatePlaylist", &[("playlistId", &pid), ("songIndexToRemove", "0"), ("songIdToAdd", &two), ("comment", "hi")]).await;
    let r = s.ok("getPlaylist", &[("id", &pid)]).await;
    let order: Vec<&str> = r["playlist"]["entry"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(order, [one.as_str(), two.as_str()]);
    assert_eq!(r["playlist"]["comment"], "hi");
    assert_eq!(s.ok("getPlaylists", &[]).await["playlists"]["playlist"].as_array().unwrap().len(), 1);
    s.ok("deletePlaylist", &[("id", &pid)]).await;

    // play queue
    s.ok("savePlayQueue", &[("id", &one), ("id", &two), ("current", &two), ("position", "1500")]).await;
    let r = s.ok("getPlayQueue", &[]).await;
    assert_eq!(r["playQueue"]["current"], two);
    assert_eq!(r["playQueue"]["position"], 1500);
    assert_eq!(r["playQueue"]["entry"].as_array().unwrap().len(), 2);

    // lyrics from the .lrc next to the file
    let r = s.ok("getLyricsBySongId", &[("id", &one)]).await;
    let l = &r["lyricsList"]["structuredLyrics"][0];
    assert_eq!(l["synced"], true);
    assert_eq!(l["line"][1]["start"], 1750);
    assert_eq!(l["line"][1]["value"], "world");

    // files: direct with Range, transcoded, cover art resized
    let resp = s.http.get(s.url("stream", &[("id", &one)])).header("Range", "bytes=0-99").send().await.unwrap();
    assert_eq!(resp.status(), 206);
    assert_eq!(resp.headers()["content-type"], "audio/flac");
    assert_eq!(resp.bytes().await.unwrap().len(), 100);
    let resp = s.http.get(s.url("stream", &[("id", &one), ("format", "mp3"), ("maxBitRate", "128")])).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.headers()["content-type"], "audio/mpeg");
    let b = resp.bytes().await.unwrap();
    assert!(b.len() > 10_000, "transcoded {} bytes", b.len());
    let resp = s.http.get(s.url("getCoverArt", &[("id", &album_id), ("size", "100")])).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    let img = image::load_from_memory(&resp.bytes().await.unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (100, 100));
    let resp = s.http.get(s.url("getCoverArt", &[("id", "nope")])).send().await.unwrap();
    assert_eq!(resp.status(), 404);

    // the drive goes away: nothing is marked missing, streams say so clearly
    let away = s.music.with_extension("away");
    std::fs::rename(&s.music, &away).unwrap();
    let rep = library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    assert_eq!(rep.libraries_offline, ["Music"]);
    assert_eq!(rep.missing, 0);
    assert_eq!(s.ok("getAlbum", &[("id", &album_id)]).await["album"]["song"].as_array().unwrap().len(), 2);
    let resp = s.http.get(s.url("stream", &[("id", &one)])).send().await.unwrap();
    assert_eq!(resp.status(), 503);
    let health: Value = s.http.get(format!("{}/health", s.base)).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["status"], "degraded");
    // ...and an empty mount point counts as not connected either
    std::fs::create_dir(&s.music).unwrap();
    let rep = library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    assert_eq!(rep.missing, 0);
    std::fs::remove_dir(&s.music).unwrap();
    std::fs::rename(&away, &s.music).unwrap();

    // a song really deleted is marked missing (and disappears from its album)
    std::fs::remove_file(s.music.join("Alpha/First Album/02 Two.flac")).unwrap();
    let rep = library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    assert_eq!(rep.missing, 1);
    assert_eq!(s.ok("getAlbum", &[("id", &album_id)]).await["album"]["songCount"], 1);

    // ids stay the same across scans
    let rep = library::scan(&s.st.db, &s.st.libs, &s.st.scan, true).await.unwrap();
    assert_eq!(rep.added, 0);
    assert_eq!(s.ok("getSong", &[("id", &one)]).await["song"]["title"], "One");
}
