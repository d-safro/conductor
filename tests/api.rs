//! End-to-end: make a tiny music folder with ffmpeg, start the server in-process, talk Subsonic to it.

use std::{path::Path, process::Command, sync::Arc};

use maestro::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
};
use serde_json::Value;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

fn tone(path: &Path, title: &str, artist: &str, album: &str, track: u32, secs: u32) {
    tone_genre(path, title, artist, album, track, secs, "Test")
}

fn tone_genre(path: &Path, title: &str, artist: &str, album: &str, track: u32, secs: u32, genre: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={}:duration={secs}", 220 * track)])
        .args(["-metadata", &format!("title={title}"), "-metadata", &format!("artist={artist}")])
        .args(["-metadata", &format!("album={album}"), "-metadata", &format!("track={track}"), "-metadata", "date=2021"])
        .args([
            "-metadata",
            &format!("genre={genre}"),
            "-metadata",
            "REPLAYGAIN_TRACK_GAIN=-6.50 dB",
            "-metadata",
            "ISRC=USABC2100001",
        ])
        .arg(path)
        .status()
        .unwrap()
        .success();
    assert!(ok, "ffmpeg failed for {}", path.display());
}

fn tone_tags(path: &Path, track: u32, tags: &[(&str, &str)]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={}:duration=2", 220 * track)]);
    for (k, v) in tags {
        cmd.args(["-metadata", &format!("{k}={v}")]);
    }
    assert!(cmd.arg(path).status().unwrap().success(), "ffmpeg failed for {}", path.display());
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
        Self::start_with(|c| c.catalog = false).await
    }

    fn no_network(c: &mut Config) {
        c.lyrics_online = false;
        c.covers_fetch = false;
        c.concerts = false;
        c.info_online = false;
    }

    async fn start_with(tune: impl FnOnce(&mut Config)) -> Self {
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
            jam_public_url: Some("https://jam.example.test".into()),
            ..Config::default()
        };
        let mut cfg = cfg;
        Self::no_network(&mut cfg);
        tune(&mut cfg);
        std::fs::create_dir_all(&cfg.data_dir).unwrap();
        let pool = db::open(&cfg.data_dir.join("maestro.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        let rep = library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        assert_eq!(rep.added, 3);
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
        if method.contains('/') {
            format!("{}/rest/{method}?{}", self.base, q.finish())
        } else {
            format!("{}/rest/{method}.view?{}", self.base, q.finish())
        }
    }

    async fn call(&self, method: &str, params: &[(&str, &str)]) -> Value {
        let r: Value = self.http.get(self.url(method, params)).send().await.unwrap().json().await.unwrap();
        r["subsonic-response"].clone()
    }

    async fn st_user(&self) -> String {
        sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(USER).fetch_one(&self.st.db).await.unwrap()
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
    assert_eq!(songs[0]["replayGain"]["trackGain"], -6.5);
    assert_eq!(songs[0]["isrc"][0], "USABC2100001");
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

    // Navidrome stand-in (Encore 1.7.9): sign in, smart playlists, playlist pictures
    let nd = |path: &str| format!("{}{path}", s.base);
    let bad =
        s.http.post(nd("/auth/login")).json(&serde_json::json!({"username": USER, "password": "nope"})).send().await.unwrap();
    assert_eq!(bad.status(), 401);
    let login: Value = s
        .http
        .post(nd("/auth/login"))
        .json(&serde_json::json!({"username": USER, "password": PASS}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let auth = format!("Bearer {}", login["token"].as_str().unwrap());
    assert_eq!(s.http.get(nd("/api/playlist")).header("x-nd-authorization", "Bearer wrong").send().await.unwrap().status(), 401);
    s.ok("star", &[("id", &two)]).await;
    let created: Value = s.http.post(nd("/api/playlist")).header("x-nd-authorization", &auth)
        .json(&serde_json::json!({"name": "Hearted", "comment": "", "public": false, "rules": {"all": [{"is": {"loved": true}}], "sort": "title", "limit": 10}}))
        .send().await.unwrap().json().await.unwrap();
    let sp = created["id"].as_str().unwrap().to_string();
    let r = s.ok("getPlaylist", &[("id", &sp)]).await;
    assert_eq!(r["playlist"]["readonly"], true);
    let ids: Vec<&str> = r["playlist"]["entry"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [two.as_str()]);
    assert_eq!(s.call("updatePlaylist", &[("playlistId", &sp), ("songIdToAdd", &one)]).await["error"]["code"], 50);
    // Encore edits: GET, then PUT the same object with new rules
    let mut cur: Value = s
        .http
        .get(nd(&format!("/api/playlist/{sp}")))
        .header("x-nd-authorization", &auth)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cur["rules"]["limit"], 10);
    cur["rules"] = serde_json::json!({"all": [{"contains": {"title": "o"}}, {"gt": {"duration": 1}}], "sort": "title"});
    let put = s.http.put(nd(&format!("/api/playlist/{sp}"))).header("x-nd-authorization", &auth).json(&cur).send().await.unwrap();
    assert_eq!(put.status(), 200);
    let r = s.ok("getPlaylist", &[("id", &sp)]).await;
    let titles: Vec<&str> = r["playlist"]["entry"].as_array().unwrap().iter().map(|e| e["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["One", "Two"]);
    let bad_rules = s
        .http
        .post(nd("/api/playlist"))
        .header("x-nd-authorization", &auth)
        .json(&serde_json::json!({"name": "x", "rules": {"all": [{"is": {"nofield": 1}}]}}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_rules.status(), 400);
    // a picture: upload, it becomes the cover, take it off again
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(64, 64, image::Rgb([0, 128, 255])).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let form =
        reqwest::multipart::Form::new().part("image", reqwest::multipart::Part::bytes(png.into_inner()).file_name("cover.png"));
    let up = s
        .http
        .post(nd(&format!("/api/playlist/{sp}/image")))
        .header("x-nd-authorization", &auth)
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert_eq!(up.status(), 200);
    let cover = s.ok("getPlaylist", &[("id", &sp)]).await["playlist"]["coverArt"].as_str().unwrap().to_string();
    assert!(cover.starts_with(&format!("pl-{sp}_")), "{cover}");
    let img = s.http.get(s.url("getCoverArt", &[("id", &cover)])).send().await.unwrap();
    assert_eq!(img.status(), 200);
    assert_eq!(img.headers()["content-type"], "image/png");
    let del = s.http.delete(nd(&format!("/api/playlist/{sp}/image"))).header("x-nd-authorization", &auth).send().await.unwrap();
    assert_eq!(del.status(), 200);
    assert!(!s.ok("getPlaylist", &[("id", &sp)]).await["playlist"]["coverArt"].as_str().unwrap().starts_with("pl-"));
    let del = s.http.delete(nd(&format!("/api/playlist/{sp}"))).header("x-nd-authorization", &auth).send().await.unwrap();
    assert_eq!(del.status(), 200);
    s.ok("unstar", &[("id", &two)]).await;

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

#[tokio::test]
async fn encore_extensions() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Arc::new(Server::start().await);
    let exts = s.ok("getOpenSubsonicExtensions", &[]).await;
    assert!(exts["openSubsonicExtensions"].as_array().unwrap().iter().any(|e| e["name"] == "encore.devices"));

    // devices: two of them, a command goes from one to the other through a long-poll
    let (a, b) = ("desktop-aaaa1111", "phone-bbbb2222");
    let r = s.ok("deviceHello", &[("id", a), ("name", "Laptop"), ("kind", "computer")]).await;
    assert_eq!(r["devices"]["device"][0]["name"], "Laptop");
    s.ok("deviceHello", &[("id", b), ("name", "Pixel"), ("kind", "phone")]).await;
    assert_eq!(s.ok("deviceList", &[]).await["devices"]["device"].as_array().unwrap().len(), 2);
    let s2 = s.clone();
    let poll = tokio::spawn(async move { s2.ok("devicePoll", &[("id", b), ("ack", "0"), ("since", "0"), ("wait", "10")]).await });
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let t0 = std::time::Instant::now();
    let r = s.ok("deviceCommand", &[("id", a), ("target", b), ("command", r#"{"type":"pause"}"#)]).await;
    assert_eq!(r["deviceCommand"]["seq"], 1);
    let got = poll.await.unwrap();
    assert!(t0.elapsed().as_secs() < 3, "the long-poll didn't wake up");
    assert_eq!(got["devicePoll"]["commands"][0]["type"], "pause");
    assert_eq!(got["devicePoll"]["commands"][0]["sender"], a);
    s.ok("deviceState", &[("id", a), ("state", r#"{"playing":true,"songId":"x"}"#)]).await;
    let r = s.ok("devicePoll", &[("id", b), ("ack", "1"), ("since", "0"), ("wait", "0")]).await;
    let laptop = r["devicePoll"]["devices"].as_array().unwrap().iter().find(|d| d["id"] == a).unwrap().clone();
    assert_eq!(laptop["state"]["playing"], true);
    assert!(laptop["state"]["at"].as_i64().unwrap() > 0);
    assert_eq!(
        s.call("deviceCommand", &[("id", a), ("target", "nobody-123456"), ("command", r#"{"type":"x"}"#)]).await["error"]["code"],
        70
    );
    s.ok("deviceBye", &[("id", a)]).await;
    assert_eq!(s.ok("deviceList", &[]).await["devices"]["device"].as_array().unwrap().len(), 1);

    // sync: newest wins
    let r = s.ok("syncSet", &[("key", "pins"), ("value", "[1,2]"), ("at", "2000"), ("id", a)]).await;
    assert_eq!(r["sync"]["changed"], true);
    let r = s.ok("syncSet", &[("key", "pins"), ("value", "[9]"), ("at", "1000"), ("id", b)]).await;
    assert_eq!(r["sync"]["changed"], false);
    assert_eq!(r["sync"]["value"]["value"], serde_json::json!([1, 2]));
    let r = s.ok("syncGet", &[]).await;
    assert_eq!(r["sync"]["values"]["pins"]["by"], a);
    assert_eq!(s.call("syncSet", &[("key", "bad key!"), ("value", "1")]).await["error"]["code"], 10);

    // sign in with your phone: the new device has no login yet
    let open = |m: &str, q: &str| format!("{}/rest/{m}?f=json&c=test{q}", s.base);
    let start: Value = s.http.get(open("pairStart", "&name=Laptop")).send().await.unwrap().json().await.unwrap();
    let pair = &start["subsonic-response"]["pair"];
    let (code, token) = (pair["code"].as_str().unwrap().to_string(), pair["token"].as_str().unwrap().to_string());
    assert_eq!(code.len(), 6);
    let w: Value = s.http.get(open("pairPoll", &format!("&token={token}&wait=0"))).send().await.unwrap().json().await.unwrap();
    assert_eq!(w["subsonic-response"]["pair"]["status"], "waiting");
    let wrong = serde_json::json!({"server": "https://x", "user": "someone-else", "pass": "p"}).to_string();
    assert_eq!(s.call("pairApprove", &[("code", &code), ("login", &wrong)]).await["error"]["code"], 10);
    let login = serde_json::json!({"server": "https://music.example", "user": USER, "pass": PASS}).to_string();
    assert_eq!(s.ok("pairApprove", &[("code", &code), ("login", &login)]).await["pair"]["name"], "Laptop");
    let done: Value = s.http.get(open("pairPoll", &format!("&token={token}&wait=0"))).send().await.unwrap().json().await.unwrap();
    assert_eq!(done["subsonic-response"]["pair"]["status"], "done");
    assert_eq!(done["subsonic-response"]["pair"]["login"]["user"], USER);
    let again: Value =
        s.http.get(open("pairPoll", &format!("&token={token}&wait=0"))).send().await.unwrap().json().await.unwrap();
    assert_eq!(again["subsonic-response"]["error"]["code"], 70);

    // listening stats and real folders
    let album = s.ok("search3", &[("query", "One")]).await;
    let one = album["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let now_ms = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).to_string();
    s.ok("scrobble", &[("id", &one), ("time", &now_ms)]).await;
    s.ok("scrobble", &[("id", &one), ("time", &now_ms)]).await; // the same play reported twice
    let r = s.ok("getListeningStats", &[("period", "week")]).await;
    let st = &r["listeningStats"];
    assert_eq!(st["plays"], 1);
    assert_eq!(st["song"][0]["id"], one);
    assert_eq!(st["artist"][0]["name"], "Alpha");
    assert_eq!(st["newArtists"], 1);
    assert_eq!(st["hour"].as_array().unwrap().len(), 24);
    assert_eq!(s.ok("getListeningStats", &[("period", "all")]).await["listeningStats"]["plays"], 1);
    let r = s.ok("getFolder", &[("path", "")]).await;
    let dirs: Vec<&str> = r["folder"]["dir"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
    assert_eq!(dirs, ["Alpha", "The Beta Band"]);
    let r = s.ok("getFolder", &[("path", "Alpha/First Album")]).await;
    assert_eq!(r["folder"]["song"].as_array().unwrap().len(), 2);
    assert_eq!(r["folder"]["song"][0]["title"], "One");

    // Jam: the guest page on its own listener
    let jl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jbase = format!("http://{}", jl.local_addr().unwrap());
    let app = maestro::jam::router(s.st.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(jl, app).await.unwrap() });
    let r = s.ok("jamStart", &[("name", "Tester")]).await;
    let tok = r["jam"]["token"].as_str().unwrap().to_string();
    assert_eq!(r["jam"]["url"], format!("https://jam.example.test/jam/{tok}"));
    let page = s.http.get(format!("{jbase}/jam/{tok}")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("default-src 'none'"));
    let st: Value = s.http.get(format!("{jbase}/jam/{tok}/state?g=guest1&n=Bob")).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["guests"], 1);
    assert_eq!(st["host"], "Tester");
    let found: Value = s.http.get(format!("{jbase}/jam/{tok}/search?q=thr")).send().await.unwrap().json().await.unwrap();
    assert_eq!(found["songs"][0]["title"], "Three");
    let cover = found["songs"][0]["cover"].as_str().unwrap().to_string();
    assert_eq!(s.http.get(format!("{jbase}/jam/{tok}/cover/{cover}")).send().await.unwrap().status(), 404); // that album has no picture
    let r =
        s.http.post(format!("{jbase}/jam/{tok}/add")).json(&serde_json::json!({"id": one, "by": "Bob"})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let bad = s.http.post(format!("{jbase}/jam/{tok}/add")).json(&serde_json::json!({"id": "nope"})).send().await.unwrap();
    assert_eq!(bad.status(), 400);
    let r = s.ok("jamPoll", &[("token", &tok), ("ack", "0"), ("wait", "1")]).await;
    assert_eq!(r["jam"]["commands"][0]["type"], "add");
    assert_eq!(r["jam"]["commands"][0]["by"], "Bob");
    assert_eq!(r["jam"]["guests"], 1);
    // a cover in the queue the app sent may be loaded by guests; others may not
    let alpha_album = s.ok("getSong", &[("id", &one)]).await["song"]["albumId"].as_str().unwrap().to_string();
    let state = serde_json::json!({"now": {"id": one, "cover": alpha_album}, "queue": []}).to_string();
    s.ok("jamState", &[("token", &tok), ("state", &state)]).await;
    assert_eq!(s.http.get(format!("{jbase}/jam/{tok}/cover/{alpha_album}?s=120")).send().await.unwrap().status(), 200);
    assert_eq!(s.http.get(format!("{jbase}/jam/{tok}/cover/someother")).send().await.unwrap().status(), 404);
    assert_eq!(s.http.get(format!("{jbase}/rest/ping")).send().await.unwrap().status(), 404); // nothing else on this port
    s.ok("jamEnd", &[("token", &tok)]).await;
    assert_eq!(s.http.get(format!("{jbase}/jam/{tok}")).send().await.unwrap().status(), 410);
    assert_eq!(s.call("jamPoll", &[("token", &tok), ("ack", "0"), ("wait", "0")]).await["error"]["code"], 70);
}

#[tokio::test]
async fn podcasts() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Arc::new(Server::start().await);
    // a podcast's own server: a feed, two episodes, a picture
    let dir = tempfile::tempdir().unwrap();
    tone(&dir.path().join("ep1.mp3"), "Ep 1", "Host", "Show", 1, 4);
    let ep1 = std::fs::read(dir.path().join("ep1.mp3")).unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_pixel(80, 80, image::Rgb([200, 10, 10])).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let png = png.into_inner();
    let fl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fbase = format!("http://{}", fl.local_addr().unwrap());
    let feed = format!(
        r#"<?xml version="1.0"?><rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel><title>Test Show</title>
        <itunes:author>Host</itunes:author><itunes:image href="{fbase}/cover.png"/>
        <item><title>Newer</title><guid>b</guid><pubDate>Wed, 11 Nov 2026 08:00:00 +0000</pubDate><itunes:duration>4</itunes:duration>
          <enclosure url="{fbase}/ep1.mp3" length="{}" type="audio/mpeg"/></item>
        <item><title>Older</title><guid>a</guid><pubDate>Tue, 10 Nov 2026 08:00:00 +0000</pubDate><itunes:duration>4</itunes:duration>
          <enclosure url="{fbase}/ep1.mp3?older" type="audio/mpeg"/></item></channel></rss>"#,
        ep1.len()
    );
    let app = axum::Router::new()
        .route(
            "/feed.xml",
            axum::routing::get(move || {
                let f = feed.clone();
                async move { ([("content-type", "application/rss+xml")], f) }
            }),
        )
        .route(
            "/cover.png",
            axum::routing::get(move || {
                let p = png.clone();
                async move { ([("content-type", "image/png")], p) }
            }),
        )
        .nest_service("/ep1.mp3", tower_http::services::ServeFile::new(dir.path().join("ep1.mp3")));
    tokio::spawn(async move { axum::serve(fl, app).await.unwrap() });
    tokio::spawn(maestro::api::podcasts::worker(s.st.clone()));

    assert_eq!(s.call("createPodcastChannel", &[("url", "not a url")]).await["error"]["code"], 10);
    let r = s.ok("createPodcastChannel", &[("url", &format!("{fbase}/feed.xml"))]).await;
    let show = r["podcastChannel"]["id"].as_str().unwrap().to_string();
    assert!(show.starts_with("pc-"));
    assert_eq!(r["podcastChannel"]["episodeCount"], 2);
    let r = s.ok("getPodcasts", &[]).await;
    let eps = r["podcasts"]["channel"][0]["episode"].as_array().unwrap().clone();
    assert_eq!(eps[0]["title"], "Newer");
    assert_eq!(eps[0]["artist"], "Host");
    assert_eq!(eps[0]["downloaded"], false);
    let (newer, older) = (eps[0]["id"].as_str().unwrap().to_string(), eps[1]["id"].as_str().unwrap().to_string());
    assert_eq!(s.ok("getNewestPodcasts", &[("count", "1")]).await["newestPodcasts"]["episode"][0]["id"], newer);
    // an episode in the play queue stays there for the next device
    s.ok("savePlayQueue", &[("id", &newer), ("current", &newer), ("position", "5000")]).await;
    let q = s.ok("getPlayQueue", &[]).await;
    assert_eq!(q["playQueue"]["entry"][0]["id"], newer.as_str());

    // not saved here: streamed through from the podcast's server, seeking included
    let r = s.http.get(s.url("stream", &[("id", &older)])).header("Range", "bytes=10-19").send().await.unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.bytes().await.unwrap().len(), 10);
    // saved on the server when asked for
    s.ok("downloadPodcastEpisode", &[("id", &newer)]).await;
    let mut saved = false;
    for _ in 0..50 {
        if s.ok("getPodcastEpisode", &[("id", &newer)]).await["podcastEpisode"]["downloaded"] == true {
            saved = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(saved, "the episode wasn't downloaded");
    let r = s.http.get(s.url("stream", &[("id", &newer)])).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap().len(), ep1.len());
    // covers for the show and its episodes (they have none of their own)
    let r = s.http.get(s.url("getCoverArt", &[("id", &show), ("size", "40")])).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(image::load_from_memory(&r.bytes().await.unwrap()).unwrap().width(), 40);
    assert_eq!(s.http.get(s.url("getCoverArt", &[("id", &newer)])).send().await.unwrap().status(), 200);

    // where you are, per user; bookmarks; played
    s.ok("createBookmark", &[("id", &older), ("position", "1500")]).await;
    let r = s.ok("getBookmarks", &[]).await;
    assert_eq!(r["bookmarks"]["bookmark"][0]["position"], 1500);
    assert_eq!(r["bookmarks"]["bookmark"][0]["entry"]["id"], older);
    s.ok("setPodcastProgress", &[("id", &older), ("played", "true")]).await;
    let e = s.ok("getPodcastEpisode", &[("id", &older)]).await;
    assert_eq!(e["podcastEpisode"]["played"], true);
    assert_eq!(e["podcastEpisode"]["position"], 0);
    assert!(s.ok("getBookmarks", &[]).await["bookmarks"]["bookmark"].as_array().unwrap().is_empty());

    // delete the saved file, then the show
    s.ok("deletePodcastEpisode", &[("id", &newer)]).await;
    assert_eq!(s.ok("getPodcastEpisode", &[("id", &newer)]).await["podcastEpisode"]["downloaded"], false);
    s.ok("deletePodcastChannel", &[("id", &show)]).await;
    assert!(s.ok("getPodcasts", &[]).await["podcasts"]["channel"].as_array().unwrap().is_empty());
    assert_eq!(s.call("getPodcastEpisode", &[("id", &newer)]).await["error"]["code"], 70);
}

/// Stand-ins for Deezer (a few songs, albums, artists) and slskd (one peer who has the song).
async fn fake_services(downloads: std::path::PathBuf, flac: Vec<u8>) -> (String, String) {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    let dl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let dz_base = format!("http://{}", dl.local_addr().unwrap());
    let b = dz_base.clone();
    let deezer = axum::Router::new().fallback(move |req: Request| {
        let b = b.clone();
        async move {
            let path = req.uri().path().to_string();
            let q = req.uri().query().unwrap_or("").to_lowercase();
            let th = json!({ "id": 7, "name": "Talking Heads", "nb_fan": 1000, "nb_album": 1, "picture_big": format!("{b}/img.png") });
            let alpha = json!({ "id": 8, "name": "Alpha", "nb_fan": 50 });
            let al77 = json!({ "id": 50, "title": "Talking Heads: 77", "artist": th, "record_type": "album", "nb_tracks": 2, "release_date": "1977-09-16",
                               "cover_big": format!("{b}/img.png"), "cover_xl": format!("{b}/img.png"), "fans": 900 });
            let first = json!({ "id": 60, "title": "First Album", "artist": alpha, "record_type": "album", "nb_tracks": 3, "fans": 400 });
            let second = json!({ "id": 61, "title": "Second Album", "artist": alpha, "record_type": "album", "nb_tracks": 9, "fans": 300, "release_date": "2023-01-01" });
            let pk = json!({ "id": 900, "title": "Psycho Killer", "duration": 3, "isrc": "USSI10000001", "artist": th, "album": al77,
                             "preview": format!("{b}/preview.mp3"), "track_position": 1, "disk_number": 1, "contributors": [] });
            let uh = json!({ "id": 901, "title": "Uh-Oh, Love Comes to Town", "duration": 3, "artist": th, "album": al77, "track_position": 2, "disk_number": 1 });
            let one = json!({ "id": 801, "title": "One", "duration": 3, "artist": alpha, "album": first, "track_position": 1 });
            let half = json!({ "id": 803, "title": "Three and a Half", "duration": 3, "artist": alpha, "album": first, "track_position": 3 });
            let body = match path.as_str() {
                "/search" => json!({ "data": if q.contains("psycho") { vec![pk.clone(), one.clone()] } else { vec![] } }),
                "/search/album" if q.contains("alpha") => json!({ "data": [first] }),
                "/search/album" => json!({ "data": if q.contains("talking") || q.contains("psycho") { vec![al77.clone()] } else { vec![] } }),
                "/search/artist" if q.contains("alpha") => json!({ "data": [alpha] }),
                "/search/artist" => json!({ "data": if q.contains("talking") { vec![th.clone()] } else { vec![] } }),
                "/track/900" => pk,
                "/track/901" => uh,
                "/track/isrc:USSI10000001" => pk.clone(),
                "/album/50" => al77,
                "/album/50/tracks" => json!({ "data": [pk, uh] }),
                "/album/60/tracks" => json!({ "data": [one, half] }),
                "/artist/8/albums" => json!({ "data": [first, second] }),
                // for the mixes: related artists, top songs, radios, a release from today
                "/artist/8/related" => json!({ "data": [th, { "id": 9, "name": "The Beta Band", "picture_big": format!("{b}/img.png") }] }),
                "/artist/7/top" => json!({ "data": [pk, uh] }),
                "/artist/8/radio" => json!({ "data": [pk] }),
                "/search/track" if q.contains("alpha") => json!({ "data": [one] }),
                "/artist/7/albums" => json!({ "data": [{ "id": 51, "title": "New Single", "artist": th, "record_type": "single",
                    "release_date": chrono::Local::now().format("%Y-%m-%d").to_string() }] }),
                "/album/51/tracks" => json!({ "data": [uh] }),
                "/artist/7" => th,
                "/preview.mp3" => return ([("content-type", "audio/mpeg")], vec![0xffu8, 0xfb, 0x90, 0x00, 1, 2, 3]).into_response(),
                "/img.png" => {
                    let mut png = std::io::Cursor::new(Vec::new());
                    image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3])).write_to(&mut png, image::ImageFormat::Png).unwrap();
                    return ([("content-type", "image/png")], png.into_inner()).into_response();
                }
                _ => json!({ "error": { "code": 800, "message": "no data" } }),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(dl, deezer).await.unwrap() });

    let sl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sl_base = format!("http://{}", sl.local_addr().unwrap());
    let remote = r"Music\Talking Heads\Talking Heads 77\04 Psycho Killer.flac".to_string();
    let queued = Arc::new(std::sync::Mutex::new(false));
    let slskd = axum::Router::new().fallback(move |req: Request| {
        let (remote, queued, downloads, flac) = (remote.clone(), queued.clone(), downloads.clone(), flac.clone());
        async move {
            let (m, path) = (req.method().clone(), req.uri().path().to_string());
            let body = match (m.as_str(), path.as_str()) {
                (_, "/api/v0/application") => json!({ "server": { "isLoggedIn": true } }),
                ("POST", "/api/v0/searches") => json!({}),
                ("GET", p) if p.ends_with("/responses") => json!([{ "username": "peer", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
                    "files": [{ "filename": remote, "size": flac.len(), "bitRate": 0, "length": 3, "extension": "flac" },
                              { "filename": r"Music\Talking Heads\Live\Psycho Killer (Live).flac", "size": 1, "length": 3 }] }]),
                ("GET", p) if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
                ("POST", "/api/v0/transfers/downloads/peer") => {
                    // the peer sends it at once
                    let d = downloads.join("Talking Heads 77");
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("04 Psycho Killer.flac"), &flac).unwrap();
                    *queued.lock().unwrap() = true;
                    json!({})
                }
                ("GET", "/api/v0/transfers/downloads") => {
                    if *queued.lock().unwrap() {
                        json!([{ "username": "peer", "directories": [{ "files": [{ "id": "t1", "filename": remote, "state": "Completed, Succeeded" }] }] }])
                    } else {
                        json!([])
                    }
                }
                _ => json!({}),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(sl, slskd).await.unwrap() });
    (dz_base, sl_base)
}

#[tokio::test]
async fn songs_you_dont_have() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("complete");
    tone(&tmp.path().join("pk.flac"), "Psycho Killer", "Talking Heads", "Talking Heads: 77", 4, 3);
    let flac = std::fs::read(tmp.path().join("pk.flac")).unwrap();
    let (dz, sl) = fake_services(downloads.clone(), flac).await;
    let s = Arc::new(
        Server::start_with(|c| {
            c.deezer_url = dz.clone();
            c.slskd_url = Some(sl.clone());
            c.slskd_api_key = Some("test".into());
            c.slskd_downloads = Some(downloads.clone());
            c.slskd_fast_test = true;
        })
        .await,
    );
    tokio::spawn(maestro::catalog::acquire::worker(s.st.clone()));

    // search: ⬇ items you don't have; your own "One" isn't offered again
    let r = s.ok("search3", &[("query", "psycho")]).await;
    let songs = r["searchResult3"]["song"].as_array().unwrap();
    assert_eq!(songs.len(), 1, "{songs:?}");
    assert_eq!(songs[0]["id"], "dz-t-900");
    assert_eq!(songs[0]["title"], "⬇ Psycho Killer");
    assert_eq!(songs[0]["lookupState"], 0);
    assert!(r["searchResult3"]["album"].as_array().is_none_or(|a| a.is_empty())); // "psycho" isn't in the album's name
    let r = s.ok("search3", &[("query", "talking heads")]).await;
    assert_eq!(r["searchResult3"]["album"][0]["id"], "dz-a-50");
    assert_eq!(r["searchResult3"]["album"][0]["name"], "⬇ Talking Heads: 77");
    assert_eq!(r["searchResult3"]["artist"][0]["id"], "dz-r-7");
    // Symfonium never gets ⬇ items
    let plain: Value = s
        .http
        .get(s.url("search3", &[("query", "psycho")]).replace("c=test", "c=Symfonium"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(plain["subsonic-response"]["searchResult3"]["song"].as_array().is_none_or(|a| a.is_empty()));

    // a whole ⬇ album, a ⬇ song, its preview and picture
    let r = s.ok("getAlbum", &[("id", "dz-a-50")]).await;
    assert_eq!(r["album"]["song"].as_array().unwrap().len(), 2);
    assert_eq!(s.ok("getSong", &[("id", "dz-t-900")]).await["song"]["album"], "Talking Heads: 77");
    let pv = s.http.get(s.url("stream", &[("id", "dz-t-900")])).send().await.unwrap();
    assert_eq!(pv.headers()["content-type"], "audio/mpeg");
    assert_eq!(pv.bytes().await.unwrap().len(), 7);
    assert_eq!(s.http.get(s.url("getCoverArt", &[("id", "dz-a-50")])).send().await.unwrap().status(), 200);

    // your album shows its missing song; your artist the album you don't have
    let alpha = s.ok("search3", &[("query", "Alpha"), ("songCount", "0"), ("albumCount", "0")]).await["searchResult3"]["artist"]
        [0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let art = s.ok("getArtist", &[("id", &alpha)]).await;
    let names: Vec<&str> = art["artist"]["album"].as_array().unwrap().iter().map(|a| a["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["First Album", "⬇ Second Album"]);
    let first = art["artist"]["album"][0]["id"].as_str().unwrap().to_string();
    let al = s.ok("getAlbum", &[("id", &first)]).await;
    let titles: Vec<&str> = al["album"]["song"].as_array().unwrap().iter().map(|x| x["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["One", "⬇ Three and a Half", "Two"]);

    // heart it: found on Soulseek (the studio copy, not the live one), downloaded, filed, scanned, hearted
    s.ok("star", &[("id", "dz-t-900")]).await;
    let mut ready = Value::Null;
    for _ in 0..100 {
        let r = s.ok("lookupSongs", &[("id", "dz-t-900")]).await;
        if r["lookupSongs"]["song"][0]["status"] == "ready" {
            ready = r["lookupSongs"]["song"][0].clone();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let local = ready["songId"].as_str().expect("the song never arrived").to_string();
    assert!(s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer.flac").is_file());
    let st = s.ok("getStarred2", &[]).await;
    assert!(st["starred2"]["song"].as_array().unwrap().iter().any(|x| x["id"] == local));
    assert_eq!(s.ok("getSong", &[("id", &local)]).await["song"]["title"], "Psycho Killer");
    // now you have it: search doesn't offer it any more, and dz-t-900 plays your copy
    assert!(
        s.ok("search3", &[("query", "psycho")]).await["searchResult3"]["song"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["id"] != "dz-t-900")
    );
    let r = s.http.get(s.url("stream", &[("id", "dz-t-900")])).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "audio/flac");

    // ⬇ songs in the play queue stay there for the next device
    s.ok("savePlayQueue", &[("id", &local), ("id", "dz-t-901"), ("current", "dz-t-901")]).await;
    let q = s.ok("getPlayQueue", &[]).await;
    let ids: Vec<&str> = q["playQueue"]["entry"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [local.as_str(), "dz-t-901"]);

    // a ⬇ song added to a playlist shows there while it's on its way
    let r = s.ok("createPlaylist", &[("name", "Wishlist"), ("songId", "dz-t-901")]).await;
    let pid = r["playlist"]["id"].as_str().unwrap().to_string();
    let pl = s.ok("getPlaylist", &[("id", &pid)]).await;
    assert_eq!(pl["playlist"]["entry"][0]["id"], "dz-t-901");
    assert_eq!(pl["playlist"]["entry"][0]["title"].as_str().unwrap().chars().next(), Some('⏳'));
}

async fn trial_server(tune: impl FnOnce(&mut Config)) -> (Arc<Server>, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("complete");
    tone(&tmp.path().join("pk.flac"), "Psycho Killer", "Talking Heads", "Talking Heads: 77", 4, 3);
    let flac = std::fs::read(tmp.path().join("pk.flac")).unwrap();
    let (dz, sl) = fake_services(downloads.clone(), flac).await;
    let s = Arc::new(
        Server::start_with(|c| {
            c.deezer_url = dz.clone();
            c.slskd_url = Some(sl.clone());
            c.slskd_api_key = Some("test".into());
            c.slskd_downloads = Some(downloads.clone());
            c.slskd_fast_test = true;
            tune(c);
        })
        .await,
    );
    tokio::spawn(maestro::catalog::acquire::worker(s.st.clone()));
    (s, tmp)
}

async fn wait_for(mut f: impl AsyncFnMut() -> bool, what: &str) {
    for _ in 0..150 {
        if f().await {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn songs_on_trial_kept() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|_| {}).await;
    // Encore holds playback (silence, not the preview) while it waits for a song
    let r = s.http.get(s.url("stream", &[("id", "dz-t-901"), ("hold", "true")])).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "audio/mpeg");
    assert_eq!(r.bytes().await.unwrap().len(), 417 * (10 * 44_100 / 1152));

    // played / queued: on trial
    let r = s.ok("lookupSongs", &[("id", "dz-t-900"), ("fetch", "true")]).await;
    assert_eq!(r["lookupSongs"]["song"][0]["status"], "searching");
    assert_eq!(r["lookupSongs"]["song"][0]["temporary"], true);
    assert_eq!(r["lookupSongs"]["usedToday"], 1);
    let mut local = String::new();
    let mut pid = String::new();
    wait_for(
        async || {
            let r = s.ok("lookupSongs", &[("id", "dz-t-900")]).await;
            local = r["lookupSongs"]["song"][0]["songId"].as_str().unwrap_or("").to_string();
            pid = r["lookupSongs"]["playlistId"].as_str().unwrap_or("").to_string();
            !local.is_empty() && !pid.is_empty()
        },
        "the trial song and its playlist",
    )
    .await;
    assert!(s.music.join("Trial/Talking Heads/Talking Heads - Psycho Killer.flac").is_file());
    wait_for(async || s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"][0]["id"] == local, "On trial to list it")
        .await;
    let pl = s.ok("getPlaylist", &[("id", &pid)]).await;
    assert_eq!(pl["playlist"]["name"], "On trial");

    // hearted: kept, moved into Songs, same id, gone from On trial
    s.ok("star", &[("id", &local)]).await;
    wait_for(async || s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer.flac").is_file(), "the kept song to move")
        .await;
    assert!(!s.music.join("Trial").join("Talking Heads").exists(), "empty Trial folders are removed");
    wait_for(
        async || s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"].as_array().is_none_or(|e| e.is_empty()),
        "On trial to empty",
    )
    .await;
    let song = s.ok("getSong", &[("id", &local)]).await;
    assert_eq!(song["song"]["path"], "Songs/Talking Heads/Talking Heads - Psycho Killer.flac");
    assert!(song["song"]["starred"].is_string());
    let r = s.ok("lookupSongs", &[("id", "dz-t-900")]).await;
    assert_eq!(r["lookupSongs"]["song"][0]["temporary"], false);
}

#[tokio::test]
async fn songs_on_trial_expire() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|c| c.trial_hours = 2.0 / 3600.0).await;
    // playing a ⬇ song in any app puts it on trial
    s.ok("scrobble", &[("id", "dz-t-900"), ("submission", "false")]).await;
    let file = s.music.join("Trial/Talking Heads/Talking Heads - Psycho Killer.flac");
    wait_for(async || file.is_file(), "the trial download").await;
    // not kept within the (two-second) trial: deleted
    wait_for(async || !file.exists(), "the trial song to be deleted").await;
    wait_for(
        async || s.ok("lookupSongs", &[("id", "dz-t-900")]).await["lookupSongs"]["song"][0]["status"] == "none",
        "the request to go",
    )
    .await;

    // today's trial downloads used up
    let (s2, _tmp2) = trial_server(|c| c.trial_per_day = 0).await;
    let r = s2.ok("lookupSongs", &[("id", "dz-t-900"), ("fetch", "true")]).await;
    assert_eq!(r["lookupSongs"]["song"][0]["status"], "limit");
}

#[tokio::test]
async fn lidarr_wanted_albums() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    if !have_ffmpeg() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("complete");
    let lidarr_music = tmp.path().join("lidarr-music");
    tone(&tmp.path().join("a.flac"), "Born Under Punches", "Talking Heads", "Remain in Light", 1, 2);
    let flac = std::fs::read(tmp.path().join("a.flac")).unwrap();
    let dir = r"Music\Talking Heads\Remain in Light (1980)";
    let files = [format!(r"{dir}\01 - Born Under Punches.flac"), format!(r"{dir}\02 - Crosseyed and Painless.flac")];

    // slskd: one user with the whole album, one with half of it
    let sl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sl_base = format!("http://{}", sl.local_addr().unwrap());
    let sent = Arc::new(std::sync::Mutex::new(false));
    let (f2, s2, d2, flac2) = (files.clone(), sent.clone(), downloads.clone(), flac.clone());
    let slskd = axum::Router::new().fallback(move |req: Request| {
        let (files, sent, downloads, flac) = (f2.clone(), s2.clone(), d2.clone(), flac2.clone());
        async move {
            let (m, path) = (req.method().clone(), req.uri().path().to_string());
            let body = match (m.as_str(), path.as_str()) {
                (_, "/api/v0/application") => json!({ "server": { "isLoggedIn": true } }),
                ("GET", p) if p.ends_with("/responses") => json!([
                    { "username": "half", "hasFreeUploadSlot": true, "files": [{ "filename": files[0], "size": 1 }] },
                    { "username": "good", "hasFreeUploadSlot": true, "uploadSpeed": 900_000,
                      "files": [{ "filename": files[0], "size": flac.len() }, { "filename": files[1], "size": flac.len() }] }]),
                ("GET", p) if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
                ("POST", "/api/v0/transfers/downloads/good") => {
                    let d = downloads.join("Remain in Light (1980)");
                    std::fs::create_dir_all(&d).unwrap();
                    for f in &files {
                        std::fs::write(d.join(f.rsplit('\\').next().unwrap()), &flac).unwrap();
                    }
                    *sent.lock().unwrap() = true;
                    json!({})
                }
                ("GET", "/api/v0/transfers/downloads") if *sent.lock().unwrap() => {
                    json!([{ "username": "good", "directories": [{ "files": [
                    { "id": "a", "filename": files[0], "state": "Completed, Succeeded" },
                    { "id": "b", "filename": files[1], "state": "Completed, Succeeded" }] }] }])
                }
                ("GET", "/api/v0/transfers/downloads") => json!([]),
                _ => json!({}),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(sl, slskd).await.unwrap() });

    // Lidarr: wants one album; "imports" by moving the folder into its own music folder
    let ll = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ll_base = format!("http://{}", ll.local_addr().unwrap());
    let imported = Arc::new(std::sync::Mutex::new(None::<String>));
    let (imp, lm) = (imported.clone(), lidarr_music.clone());
    let lidarr = axum::Router::new().fallback(move |req: Request| {
        let (imported, music) = (imp.clone(), lm.clone());
        async move {
            let (m, path, query) =
                (req.method().clone(), req.uri().path().to_string(), req.uri().query().unwrap_or("").to_string());
            assert_eq!(req.headers()["x-api-key"], "lkey");
            let done = imported.lock().unwrap().is_some();
            let body = match (m.as_str(), path.as_str()) {
                ("GET", "/api/v1/wanted/missing") => json!({ "records": if done { json!([]) } else {
                    json!([{ "id": 42, "title": "Remain in Light", "artist": { "artistName": "Talking Heads" } }]) } }),
                ("GET", "/api/v1/track") if query.contains("albumId=42") => {
                    json!([{ "title": "Born Under Punches" }, { "title": "Crosseyed and Painless" }])
                }
                ("POST", "/api/v1/command") => {
                    let b = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap();
                    let v: Value = serde_json::from_slice(&b).unwrap();
                    assert_eq!(v["name"], "DownloadedAlbumsScan");
                    let from = std::path::PathBuf::from(v["path"].as_str().unwrap());
                    let to = music.join("Talking Heads/Remain in Light");
                    std::fs::create_dir_all(&to).unwrap();
                    for e in std::fs::read_dir(&from).unwrap().flatten() {
                        std::fs::rename(e.path(), to.join(e.file_name())).unwrap();
                    }
                    *imported.lock().unwrap() = Some(v["path"].as_str().unwrap().to_string());
                    json!({ "id": 7 })
                }
                ("GET", "/api/v1/command/7") => json!({ "status": "completed" }),
                ("GET", "/api/v1/album/42") => json!({ "statistics": { "trackFileCount": if done { 2 } else { 0 } } }),
                _ => json!({}),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(ll, lidarr).await.unwrap() });

    let s = Arc::new(
        Server::start_with(|c| {
            c.slskd_url = Some(sl_base.clone());
            c.slskd_api_key = Some("test".into());
            c.slskd_downloads = Some(downloads.clone());
            c.lidarr_url = Some(ll_base.clone());
            c.lidarr_api_key = Some("lkey".into());
            c.slskd_fast_test = true;
        })
        .await,
    );
    tokio::spawn(maestro::catalog::acquire::worker(s.st.clone()));
    wait_for(
        async || {
            sqlx::query_scalar::<_, String>("SELECT status FROM lidarr_grab WHERE album_id = 42")
                .fetch_optional(&s.st.db)
                .await
                .unwrap()
                .as_deref()
                == Some("done")
        },
        "the album to be imported",
    )
    .await;
    let path = imported.lock().unwrap().clone().unwrap();
    assert!(path.ends_with("Talking Heads - Remain in Light"), "{path}");
    assert!(lidarr_music.join("Talking Heads/Remain in Light/02 - Crosseyed and Painless.flac").is_file());
    assert!(!downloads.join("Talking Heads - Remain in Light").exists(), "the emptied folder is removed");
    let user: String =
        sqlx::query_scalar("SELECT slsk_user FROM lidarr_grab WHERE album_id = 42").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(user, "good"); // the whole album, not half of it
}

#[tokio::test]
async fn lyrics_and_covers_online() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    if !have_ffmpeg() {
        return;
    }
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let (b2, h2) = (base.clone(), hits.clone());
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::RgbImage::from_fn(400, 400, |x, y| image::Rgb([(x * 7 % 251) as u8, (y * 13 % 241) as u8, ((x * y) % 239) as u8]))
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .unwrap();
    let jpeg = jpeg.into_inner();
    let app = axum::Router::new().fallback(move |req: Request| {
        let (b, hits, jpeg) = (b2.clone(), h2.clone(), jpeg.clone());
        async move {
            let q = req.uri().query().unwrap_or("").to_string();
            match req.uri().path() {
                "/api/get" => {
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if q.contains("track_name=Two") {
                        axum::Json(json!({ "syncedLyrics": "[00:01.00]two, online\n[00:02.50]second line", "plainLyrics": "two, online" })).into_response()
                    } else {
                        (axum::http::StatusCode::NOT_FOUND, "none").into_response()
                    }
                }
                "/search" => axum::Json(json!({ "results": [{ "artistName": "Gamma", "collectionName": "Third",
                    "artworkUrl100": format!("{b}/art/100x100bb.jpg") }] })).into_response(),
                p if p.starts_with("/art/") => {
                    assert!(p.contains("1200x1200bb"), "asks for the big picture");
                    ([("content-type", "image/jpeg")], jpeg).into_response()
                }
                _ => axum::Json(json!({ "data": [] })).into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.lyrics_online = true;
        c.lrclib_url = base.clone();
        c.itunes_url = base.clone();
        c.deezer_url = base.clone();
        c.slskd_fast_test = true;
    })
    .await;
    tone(&s.music.join("Albums/Gamma/Third/01 Gamma Song.flac"), "Gamma Song", "Gamma", "Third", 1, 2);
    library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();

    // a song with no lyrics of its own: LRCLIB's, synced, asked for once
    let two = s.ok("search3", &[("query", "Two")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    for _ in 0..2 {
        let r = s.ok("getLyricsBySongId", &[("id", &two)]).await;
        let l = &r["lyricsList"]["structuredLyrics"][0];
        assert_eq!(l["synced"], true);
        assert_eq!(l["line"][1]["value"], "second line");
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    // a miss is remembered too
    let three = s.ok("search3", &[("query", "Three")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    for _ in 0..2 {
        assert!(
            s.ok("getLyricsBySongId", &[("id", &three)]).await["lyricsList"]["structuredLyrics"].as_array().unwrap().is_empty()
        );
    }
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);

    // a cover for the album folder that has none; Artist/Album folders (one level less) are left alone
    let saved = maestro::online::covers_once(&s.st).await.unwrap();
    assert_eq!(saved, 1);
    assert!(s.music.join("Albums/Gamma/Third/cover.jpg").is_file());
    assert!(!s.music.join("The Beta Band/Second/cover.jpg").exists());
    assert_eq!(maestro::online::covers_once(&s.st).await.unwrap(), 0); // never twice
}

#[tokio::test]
async fn audiomuse_bridge() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    if !have_ffmpeg() {
        return;
    }
    let ids = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let i2 = ids.clone();
    let am = axum::Router::new().fallback(move |req: Request| {
        let ids = i2.clone();
        async move {
            assert_eq!(req.headers()["authorization"], "Bearer am-token");
            let q = req.uri().query().unwrap_or("").to_string();
            assert!(!q.contains("apiKey") && !q.contains("u="), "logins aren't passed on: {q}");
            let ids = ids.lock().unwrap().clone();
            let body = match req.uri().path() {
                "/api/similar_tracks" => {
                    json!([{ "item_id": ids[1], "title": "Two", "distance": 0.1 }, { "item_id": "unknown", "distance": 0.2 },
                                                { "item_id": ids[2], "distance": 0.3 }])
                }
                "/api/find_path" => json!({ "path": [{ "item_id": ids[0] }, { "item_id": ids[2] }], "total_distance": 1.0 }),
                "/api/clap/top_queries" => json!({ "queries": ["rainy piano"] }),
                _ => return (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(l, am).await.unwrap() });
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.audiomuse_url = Some(base.clone());
        c.audiomuse_token = Some("am-token".into());
    })
    .await;
    let all = s.ok("search3", &[("query", "\"\""), ("songCount", "10")]).await;
    let mut v: Vec<String> =
        all["searchResult3"]["song"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect();
    v.sort();
    *ids.lock().unwrap() = v.clone();
    let r = s.ok("getSonicSimilarTracks", &[("id", &v[0]), ("count", "5")]).await;
    let m = r["sonicMatch"].as_array().unwrap();
    assert_eq!(m.len(), 2, "unknown ids are left out");
    assert_eq!(m[0]["entry"]["id"], v[1]);
    assert!((m[0]["similarity"].as_f64().unwrap() - 0.9).abs() < 1e-9);
    let r = s.ok("findSonicPath", &[("startSongId", &v[0]), ("endSongId", &v[2])]).await;
    assert_eq!(r["sonicPath"][1]["entry"]["id"], v[2]);
    // the passthrough: allowed calls with a login, nothing else
    let r: Value = s.http.get(s.url("audiomuse/clap/top_queries", &[])).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["queries"][0], "rainy piano");
    assert_eq!(s.http.get(s.url("audiomuse/admin/delete_everything", &[])).send().await.unwrap().status(), 404);
    assert_eq!(s.http.get(format!("{}/rest/audiomuse/clap/top_queries?u=x&p=y", s.base)).send().await.unwrap().status(), 401);
    // not set up: "not available", so apps fall back
    let s2 = Server::start().await;
    assert_eq!(s2.call("getSonicSimilarTracks", &[("id", "x")]).await["error"]["code"], 70);
}

#[tokio::test]
async fn admin_page() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let page = http.get(format!("{}/admin", s.base)).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("<title>Maestro admin</title>"));
    let api = |p: &str| format!("{}/admin/api/{p}", s.base);
    assert_eq!(http.get(api("status")).send().await.unwrap().status(), 401);
    // sign-in needs the header (other sites can't send it), and the right password
    let no_header = http.post(api("login")).json(&serde_json::json!({"username": USER, "password": PASS})).send().await.unwrap();
    assert_eq!(no_header.status(), 403);
    let wrong = http
        .post(api("login"))
        .header("X-Maestro", "1")
        .json(&serde_json::json!({"username": USER, "password": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let ok = http
        .post(api("login"))
        .header("X-Maestro", "1")
        .json(&serde_json::json!({"username": USER, "password": PASS}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert!(ok.headers()["set-cookie"].to_str().unwrap().contains("HttpOnly; SameSite=Strict"));
    let st: Value = http.get(api("status")).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["songs"], 3);
    assert_eq!(st["libraries"][0]["connected"], true);
    // changes need the header too
    assert_eq!(
        http.post(api("users")).json(&serde_json::json!({"name": "guest", "password": "secret1"})).send().await.unwrap().status(),
        403
    );
    let r = http
        .post(api("users"))
        .header("X-Maestro", "1")
        .json(&serde_json::json!({"name": "guest", "password": "secret1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let users: Value = http.get(api("users")).send().await.unwrap().json().await.unwrap();
    assert_eq!(users.as_array().unwrap().len(), 2);
    let k: Value = http.post(api("users/guest/apikey")).header("X-Maestro", "1").send().await.unwrap().json().await.unwrap();
    let ping: Value = s
        .http
        .get(format!("{}/rest/ping?f=json&apiKey={}", s.base, k["key"].as_str().unwrap()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ping["subsonic-response"]["status"], "ok");
    // a non-admin can't sign in here
    let guest = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let r = guest
        .post(api("login"))
        .header("X-Maestro", "1")
        .json(&serde_json::json!({"username": "guest", "password": "secret1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(http.post(api("scan")).header("X-Maestro", "1").send().await.unwrap().status(), 200);
    http.post(api("logout")).header("X-Maestro", "1").send().await.unwrap();
    assert_eq!(http.get(api("status")).send().await.unwrap().status(), 401);
}

#[tokio::test]
async fn listenbrainz_discovery_and_plays() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    if !have_ffmpeg() {
        return;
    }
    let week = Arc::new(std::sync::atomic::AtomicU8::new(1));
    let listens = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lb = format!("http://{}", l.local_addr().unwrap());
    let (w2, l2) = (week.clone(), listens.clone());
    let app = axum::Router::new().fallback(move |req: Request| {
        let (week, listens) = (w2.clone(), l2.clone());
        async move {
            let w = week.load(std::sync::atomic::Ordering::SeqCst);
            let path = req.uri().path().to_string();
            let body = match path.as_str() {
                "/1/user/lbuser/playlists/createdfor" => json!({ "playlists": [
                    { "playlist": { "title": format!("Weekly Exploration for lbuser, week {w}"), "identifier": format!("https://listenbrainz.org/playlist/pl-{w}"), "date": format!("2026-11-{:02}T00:00:00", 10 + w) } },
                    { "playlist": { "title": "Weekly Jams for lbuser", "identifier": "https://listenbrainz.org/playlist/jams", "date": "2026-11-01T00:00:00" } }] }),
                "/1/playlist/pl-1" => json!({ "playlist": { "track": [
                    { "title": "One", "creator": "Alpha", "album": "First Album" },
                    { "title": "Psycho Killer", "creator": "Talking Heads", "album": "Talking Heads: 77", "duration": 3000 }] } }),
                "/1/playlist/pl-2" => json!({ "playlist": { "track": [{ "title": "Two", "creator": "Alpha" }] } }),
                "/1/submit-listens" => {
                    assert_eq!(req.headers()["authorization"], "Token lbtok");
                    let b = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap();
                    listens.lock().unwrap().push(serde_json::from_slice(&b).unwrap());
                    json!({ "status": "ok" })
                }
                _ => return (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let (s, _tmp) = trial_server(|c| {
        c.listenbrainz_url = lb.clone();
        c.listenbrainz_user = Some("lbuser".into());
        c.listenbrainz_token = Some("lbtok".into());
        c.discovery = vec!["weekly-exploration".into()];
    })
    .await;
    tokio::spawn(maestro::listenbrainz::worker(s.st.clone()));

    // week 1: the song you have goes straight in; the other is downloaded into Discovery/ and joins
    let file = s.music.join("Discovery/Talking Heads/Talking Heads - Psycho Killer.flac");
    let pid = {
        let mut pid = String::new();
        wait_for(
            async || {
                let r = s.ok("getPlaylists", &[]).await;
                let p =
                    r["playlists"]["playlist"].as_array().unwrap().iter().find(|p| p["name"] == "Weekly Exploration").cloned();
                pid = p.map(|p| p["id"].as_str().unwrap().to_string()).unwrap_or_default();
                !pid.is_empty()
            },
            "the discovery playlist",
        )
        .await;
        pid
    };
    wait_for(async || file.is_file(), "the discovery download").await;
    wait_for(
        async || s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"].as_array().is_some_and(|e| e.len() == 2),
        "both songs in it",
    )
    .await;
    let titles: Vec<String> = s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(titles, ["One", "Psycho Killer"]);

    // plays are sent to ListenBrainz
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    s.ok("scrobble", &[("id", &one)]).await;
    wait_for(
        async || {
            listens
                .lock()
                .unwrap()
                .iter()
                .any(|l| l["listen_type"] == "single" && l["payload"][0]["track_metadata"]["track_name"] == "One")
        },
        "the play at ListenBrainz",
    )
    .await;

    // week 2: a new list; last week's song you didn't keep is deleted
    week.store(2, std::sync::atomic::Ordering::SeqCst);
    wait_for(async || !file.exists(), "last week's song to go").await;
    wait_for(
        async || {
            let e = s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"].clone();
            e.as_array().is_some_and(|e| e.len() == 1 && e[0]["title"] == "Two")
        },
        "this week's list",
    )
    .await;
}

#[tokio::test]
async fn spotify_sync() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    if !have_ffmpeg() {
        return;
    }
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sp = format!("http://{}", l.local_addr().unwrap());
    let app = axum::Router::new().fallback(move |req: Request| async move {
        let path = req.uri().path().to_string();
        if path == "/api/token" {
            let b = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap();
            let form: Vec<(String, String)> = form_urlencoded::parse(&b).into_owned().collect();
            // the saved one first, then the new one Spotify handed out
            let tok = form.iter().find(|(k, _)| k == "refresh_token").map(|(_, v)| v.as_str()).unwrap_or("");
            assert!((tok == "old-refresh" || tok == "new-refresh") && form.contains(&("client_id".into(), "cid".into())));
            return axum::Json(json!({ "access_token": "acc", "refresh_token": "new-refresh", "expires_in": 3600 })).into_response();
        }
        assert_eq!(req.headers()["authorization"], "Bearer acc");
        let t = |id: &str, name: &str, artist: &str, isrc: Option<&str>| {
            json!({ "id": id, "name": name, "type": "track", "artists": [{ "name": artist }], "duration_ms": 3000, "external_ids": { "isrc": isrc } })
        };
        let body = match path.as_str() {
            "/v1/me/tracks" => json!({ "items": [{ "track": t("sp-pk", "Psycho Killer", "Talking Heads", Some("USSI10000001")) },
                                                 { "track": t("sp-one", "One", "Alpha", None) }], "next": null }),
            "/v1/me/playlists" => json!({ "items": [{ "id": "car", "name": "Car" }, { "id": "x", "name": "Not chosen" }], "next": null }),
            "/v1/playlists/car/items" => json!({ "items": [{ "item": t("sp-two", "Zwei (renamed on Spotify)", "Alpha", None) }], "next": null }),
            _ => return (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
        };
        axum::Json(body).into_response()
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let (s, _tmp) = trial_server(|c| {
        c.spotify_api_url = format!("{sp}/v1");
        c.spotify_accounts_url = sp.clone();
    })
    .await;
    // spotify.py's files: login, choices, and which file each Spotify song became
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("token.json"), r#"{"client_id": "cid", "refresh_token": "old-refresh", "access_token": "x"}"#)
        .unwrap();
    std::fs::write(dir.path().join("settings.json"), r#"{"mode": "songs", "liked": true, "playlists": ["Car"]}"#).unwrap();
    std::fs::write(dir.path().join("songs.json"), r#"{"sp-two": {"rel": "Alpha/First Album/02 Two.flac", "status": "done"}}"#)
        .unwrap();
    let lib = s.st.libs[0].0;
    assert_eq!(maestro::spotify::import_from(&s.st.db, dir.path(), Some(lib)).await.unwrap(), (true, 1));

    let (lists, have, asked) = maestro::spotify::sync_once(&s.st).await.unwrap();
    assert_eq!((lists, have, asked), (2, 2, 1)); // One by title, Two by its file; Psycho Killer asked for (by ISRC)
    let tok: String =
        sqlx::query_scalar("SELECT value FROM setting WHERE key = 'spotify_token'").fetch_one(&s.st.db).await.unwrap();
    assert!(tok.contains("new-refresh"), "a new refresh token is kept");
    let pls = s.ok("getPlaylists", &[]).await;
    let id_of = |name: &str| {
        pls["playlists"]["playlist"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .map(|p| p["id"].as_str().unwrap().to_string())
    };
    let (liked, car) = (id_of("Liked Songs").unwrap(), id_of("Car").unwrap());
    assert!(id_of("Not chosen").is_none());
    assert_eq!(s.ok("getPlaylist", &[("id", &car)]).await["playlist"]["entry"][0]["title"], "Two");
    // the missing song arrives: it joins Liked Songs and is hearted
    wait_for(
        async || {
            s.ok("getPlaylist", &[("id", &liked)]).await["playlist"]["entry"]
                .as_array()
                .is_some_and(|e| e.iter().any(|x| x["title"] == "Psycho Killer" && x["starred"].is_string()))
        },
        "the liked song to arrive",
    )
    .await;
    assert!(s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer.flac").is_file());
    let starred = s.ok("getStarred2", &[]).await;
    assert!(starred["starred2"]["song"].as_array().unwrap().iter().any(|x| x["title"] == "One"));
    // a second sync: now everything is here, nothing asked for, Spotify's order
    let (_, have, asked) = maestro::spotify::sync_once(&s.st).await.unwrap();
    assert_eq!((have, asked), (3, 0));
    let order: Vec<String> = s.ok("getPlaylist", &[("id", &liked)]).await["playlist"]["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(order, ["Psycho Killer", "One"]);
}

#[tokio::test]
async fn listening_review_and_shared_playlists() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Arc::new(Server::start().await);
    let find = |r: &Value, title: &str| {
        r["searchResult3"]["song"].as_array().unwrap().iter().find(|x| x["title"] == title).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let r = s.ok("search3", &[("query", "")]).await;
    let (one, two, three) = (find(&r, "One"), find(&r, "Two"), find(&r, "Three"));
    let uid: String = sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(USER).fetch_one(&s.st.db).await.unwrap();

    // the year in review: One played 12 times over the last three days (first heard this year: a discovery),
    // Three heard last year and once now (not a discovery)
    let now = maestro::db::now();
    let year_start = {
        use chrono::{Datelike, TimeZone};
        let y = chrono::Local::now().year();
        chrono::Local.with_ymd_and_hms(y, 1, 1, 0, 0, 0).earliest().unwrap().timestamp()
    };
    let mut plays = vec![(three.clone(), year_start - 86400 * 30), (three.clone(), now - 60)];
    for i in 0..12 {
        plays.push((one.clone(), (now - (i % 3) * 86400 - i * 600).max(year_start + i)));
    }
    for (id, t) in &plays {
        sqlx::query("INSERT INTO play (user_id, track_id, played_at) VALUES (?, ?, ?)")
            .bind(&uid)
            .bind(id)
            .bind(t)
            .execute(&s.st.db)
            .await
            .unwrap();
    }
    let r = s.ok("getListeningReview", &[("period", "year")]).await;
    let rv = &r["listeningReview"];
    assert_eq!(rv["plays"], 13, "{rv}");
    assert_eq!(rv["song"][0]["id"], one);
    assert_eq!(rv["discoveryCount"], 1);
    assert_eq!(rv["discoveries"][0]["id"], one);
    assert_eq!(rv["discoveries"][0]["plays"], 12);
    assert!(rv["streak"]["days"].as_i64().unwrap() >= 1);
    assert!(rv["peakHour"].is_number());
    assert!(rv["firstSong"]["id"].is_string());
    assert!(rv["genre"].is_array());
    // an earlier year with nothing in it
    let r = s.ok("getListeningReview", &[("period", "year"), ("offset", "3")]).await;
    assert_eq!(r["listeningReview"]["plays"], 0);
    assert!(r["listeningReview"]["firstSong"].is_null());

    // a shared playlist: friends add songs through the link and can take out only their own
    let pl = s.ok("createPlaylist", &[("name", "Party"), ("songId", &one)]).await;
    let pid = pl["playlist"]["id"].as_str().unwrap().to_string();
    let r = s.ok("playlistShareStart", &[("id", &pid)]).await;
    let share = &r["playlistShare"];
    let tok = share["token"].as_str().unwrap().to_string();
    assert_eq!(share["url"], format!("https://jam.example.test/jam/pl/{tok}"));
    assert_eq!(s.ok("playlistShareStart", &[("id", &pid)]).await["playlistShare"]["token"], tok.as_str()); // same link again
    assert!(s.ok("playlistShareStart", &[("id", "nope")]).await["playlistShare"]["error"].is_string());

    let jl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jbase = format!("{}/jam/pl/{tok}", format_args!("http://{}", jl.local_addr().unwrap()));
    let app = maestro::jam::router(s.st.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
    tokio::spawn(async move { axum::serve(jl, app).await.unwrap() });
    let page = s.http.get(&jbase).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.headers()["content-security-policy"].to_str().unwrap().contains("default-src 'none'"));
    let st: Value = s.http.get(format!("{jbase}/state")).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["name"], "Party");
    assert_eq!(st["owner"], USER);
    assert_eq!(st["song"].as_array().unwrap().len(), 1);
    let found: Value = s.http.get(format!("{jbase}/search?q=two")).send().await.unwrap().json().await.unwrap();
    assert_eq!(found["songs"][0]["id"], two.as_str());
    let cover = found["songs"][0]["cover"].as_str().unwrap().to_string();
    assert_eq!(s.http.get(format!("{jbase}/cover/{cover}?s=100")).send().await.unwrap().status(), 200);
    assert_eq!(s.http.get(format!("{jbase}/cover/notshown")).send().await.unwrap().status(), 404);
    let post = |what: &'static str, body: Value| {
        let (h, u) = (s.http.clone(), format!("{jbase}/{what}"));
        async move { h.post(u).json(&body).send().await.unwrap() }
    };
    assert_eq!(post("add", serde_json::json!({"id": two, "by": "Bob", "guest": "g-bob"})).await.status(), 200);
    let again: Value = post("add", serde_json::json!({"id": two, "by": "Bob", "guest": "g-bob"})).await.json().await.unwrap();
    assert_eq!(again["already"], true);
    assert_eq!(post("add", serde_json::json!({"id": "dz-t-1", "guest": "g-bob"})).await.status(), 400);
    assert_eq!(post("add", serde_json::json!({"id": three, "by": "Ann", "guest": "g-ann"})).await.status(), 200);
    let pl = s.ok("getPlaylist", &[("id", &pid)]).await;
    let ids: Vec<&str> = pl["playlist"]["entry"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [one.as_str(), two.as_str(), three.as_str()]);
    // Ann can't take out Bob's song, nor the owner's; Bob can take out his
    assert_eq!(post("remove", serde_json::json!({"id": two, "index": 1, "guest": "g-ann"})).await.status(), 403);
    assert_eq!(post("remove", serde_json::json!({"id": one, "index": 0, "guest": "g-bob"})).await.status(), 403);
    assert_eq!(post("remove", serde_json::json!({"id": two, "index": 2, "guest": "g-bob"})).await.status(), 409);
    assert_eq!(post("remove", serde_json::json!({"id": two, "index": 1, "guest": "g-bob"})).await.status(), 200);
    let st: Value = s.http.get(format!("{jbase}/state")).send().await.unwrap().json().await.unwrap();
    let songs = st["song"].as_array().unwrap();
    assert_eq!(songs.len(), 2);
    assert_eq!(songs[1]["by"], "Ann");
    let r = s.ok("playlistShares", &[]).await;
    assert_eq!(r["playlistShares"]["share"][0]["added"].as_array().unwrap().len(), 1);
    assert_eq!(s.ok("playlistShareEnd", &[("id", &pid)]).await["playlistShare"]["ended"], true);
    assert_eq!(s.http.get(&jbase).send().await.unwrap().status(), 410);
    assert_eq!(post("add", serde_json::json!({"id": two, "guest": "g-bob"})).await.status(), 410);
}

/// A stand-in on a local port: `answer(path, body)` → JSON (404 when None). Counts the calls.
async fn stand_in(
    answer: impl Fn(&str, &str, &[u8]) -> Option<Value> + Clone + Send + Sync + 'static,
) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    use axum::{extract::Request, response::IntoResponse};
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let h2 = hits.clone();
    let app = axum::Router::new().fallback(move |req: Request| {
        let (answer, hits) = (answer.clone(), h2.clone());
        async move {
            hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let path = req.uri().path().to_string();
            let query = req.uri().query().unwrap_or("").to_string();
            let body = axum::body::to_bytes(req.into_body(), 50 << 20).await.unwrap();
            match answer(&path, &query, &body) {
                Some(v) => axum::Json(v).into_response(),
                None => (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, hits)
}

#[tokio::test]
async fn lyrics_translation() {
    if !have_ffmpeg() {
        return;
    }
    let (base, hits) = stand_in(|path, _, body| {
        assert_eq!(path, "/api/chat");
        let req: Value = serde_json::from_slice(body).unwrap();
        let text = req["messages"][0]["content"].as_str().unwrap();
        let lines: Vec<String> = serde_json::from_str(text.rsplit("Lines: ").next().unwrap()).unwrap();
        assert!(!lines.iter().any(|l| l.trim().is_empty()), "blank lines aren't sent");
        let out: Vec<String> = lines.iter().map(|l| l.to_uppercase()).collect();
        let source = if text.contains("Latin alphabet (romanization") { "en" } else { "es" };
        Some(serde_json::json!({ "message": { "content": serde_json::json!({ "source": source, "lines": out }).to_string() } }))
    })
    .await;
    let off = Server::start().await;
    let r = off.ok("translateLyrics", &[("id", "x"), ("lang", "en"), ("line", "hola")]).await;
    assert_eq!(r["lyricsTranslation"]["error"], "ai-off");

    let s = Server::start_with(move |c| {
        c.catalog = false;
        c.ollama_url = Some(base);
    })
    .await;
    let q = [("id", "song1"), ("lang", "en"), ("line", "hola"), ("line", ""), ("line", "adiós")];
    let r = s.ok("translateLyrics", &q).await;
    let t = &r["lyricsTranslation"];
    assert_eq!(t["source"], "es");
    assert_eq!(t["same"], false);
    assert_eq!(t["lines"], serde_json::json!(["HOLA", "", "ADIÓS"]));
    s.ok("translateLyrics", &q).await; // kept: not asked again
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    // romanizing English: nothing to do
    let r = s.ok("translateLyrics", &[("id", "song1"), ("lang", "en"), ("mode", "romanize"), ("line", "hello")]).await;
    assert_eq!(r["lyricsTranslation"]["same"], true);
}

#[tokio::test]
async fn song_id_and_automix() {
    if !have_ffmpeg() {
        return;
    }
    let (sid, _) = stand_in(|path, _, body| match path {
        "/identify" if body == b"RIFFfake-audio" => {
            Some(serde_json::json!({ "path": "Alpha/First Album/02 Two.flac", "offset": 12.5, "score": 40 }))
        }
        "/identify" => Some(serde_json::json!({ "path": null, "score": 2 })),
        "/status" => Some(serde_json::json!({ "songs": 3, "hashes": 100, "indexing": true })),
        _ => None,
    })
    .await;
    let mix = tempfile::tempdir().unwrap();
    let mixdir = mix.path().to_path_buf();
    std::fs::write(
        mixdir.join("info.jsonl"),
        concat!(
            r#"{"path": "Alpha/First Album/01 One.flac", "mtime": 1, "dur": 3.0, "start": 0.1, "introEnd": 0.5, "outroStart": 2.5, "end": 2.9, "bpm": 120.0, "beat": 0.2, "endDb": -30}"#,
            "\n",
            r#"{"path": "The Beta Band/Second/01 Three.mp3", "error": "couldn't decode"}"#,
            "\n"
        ),
    )
    .unwrap();
    let md = mixdir.clone();
    let s = Server::start_with(move |c| {
        c.catalog = false;
        c.songid_url = Some(sid);
        c.mixinfo_dir = Some(md);
    })
    .await;
    let r = s.ok("search3", &[("query", "")]).await;
    let id = |t: &str| {
        r["searchResult3"]["song"].as_array().unwrap().iter().find(|x| x["title"] == t).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let (one, two, three) = (id("One"), id("Two"), id("Three"));

    let post = |body: &'static [u8]| {
        let (h, u) = (s.http.clone(), s.url("identifySong", &[]));
        async move {
            let v: Value = h
                .post(u)
                .header("content-type", "application/octet-stream")
                .body(body)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["subsonic-response"]["songIdentification"].clone()
        }
    };
    let got = post(b"RIFFfake-audio").await;
    assert_eq!(got["found"], true, "{got}");
    assert_eq!(got["song"]["id"], two.as_str());
    assert_eq!(got["offset"], 12.5);
    let miss = post(b"something else").await;
    assert_eq!(miss["found"], false);
    assert_eq!(miss["indexing"], true);
    assert_eq!(miss["songs"], 3);

    let r = s.ok("getMixInfo", &[("id", &one), ("id", &two), ("id", &three)]).await;
    let songs = r["mixInfo"]["song"].as_array().unwrap();
    assert_eq!(songs.len(), 1, "only measured songs, not failed ones: {songs:?}");
    assert_eq!(songs[0]["id"], one.as_str());
    assert_eq!(songs[0]["introEnd"], 0.5);
    assert!(songs[0].get("endDb").is_none());
    // Two isn't measured yet: it's asked for
    let wanted = std::fs::read_to_string(mixdir.join("wanted.txt")).unwrap();
    assert_eq!(wanted.trim(), "Alpha/First Album/02 Two.flac");
    // the analyzer measures it: seen on the next call
    let mut f = std::fs::OpenOptions::new().append(true).open(mixdir.join("info.jsonl")).unwrap();
    std::io::Write::write_all(
        &mut f,
        br#"{"path": "Alpha/First Album/02 Two.flac", "dur": 3.0, "start": 0.0, "end": 3.0, "bpm": 90}"#,
    )
    .unwrap();
    std::io::Write::write_all(&mut f, b"\n").unwrap();
    drop(f);
    let r = s.ok("getMixInfo", &[("id", &two)]).await;
    assert_eq!(r["mixInfo"]["song"][0]["bpm"], 90);
}

#[tokio::test]
async fn concerts_near_you() {
    if !have_ffmpeg() {
        return;
    }
    let (mb, _) = stand_in(|path, query, _| {
        let q: std::collections::HashMap<String, String> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
        match path {
            "/artist" if q["query"] == "artist:\"Alpha\"" => {
                Some(serde_json::json!({ "artists": [{ "id": "mb-alpha", "name": "Alpha", "score": 100 }] }))
            }
            "/artist" => Some(serde_json::json!({ "artists": [] })),
            "/event" if q["query"] == "arid:mb-alpha" => Some(serde_json::json!({ "events": [
                { "id": "e1", "name": "Alpha live", "life-span": { "begin": "2099-05-01" }, "time": "20:00",
                  "relations": [{ "place": { "name": "Club" } }, { "area": { "name": "Berlin" } }] },
                { "id": "e2", "name": "Alpha far", "life-span": { "begin": "2099-05-03" },
                  "relations": [{ "area": { "name": "Paris" } }] },
                { "id": "e0", "name": "Alpha past", "life-span": { "begin": "2001-01-01" }, "relations": [{ "area": { "name": "Berlin" } }] }
            ] })),
            _ => None,
        }
    })
    .await;
    let s = Server::start_with(move |c| {
        c.catalog = false;
        c.concerts = true;
        c.musicbrainz_url = mb;
    })
    .await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let now_ms = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).to_string();
    s.ok("scrobble", &[("id", &one), ("time", &now_ms)]).await;
    s.ok("syncSet", &[("key", "showsPrefs"), ("value", r#"{"city":"berlin"}"#), ("at", "1000"), ("id", "phone-aaaa1111")]).await;
    let r = s.ok("getConcerts", &[]).await;
    assert!(r["concerts"]["show"].as_array().unwrap().is_empty()); // the first look starts now
    let mut shows = Value::Null;
    wait_for(
        async || {
            shows = s.ok("getConcerts", &[]).await["concerts"].clone();
            shows["updated"].as_i64().unwrap_or(0) > 0
        },
        "concerts looked up",
    )
    .await;
    assert_eq!(shows["source"], "MusicBrainz");
    assert_eq!(shows["city"], "berlin");
    let list = shows["show"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["venue"], "Club");
    assert_eq!(list[0]["artist"], "Alpha");
    assert!(list[0]["first"].as_i64().unwrap() > 0);
}

#[tokio::test]
async fn weekly_mixes() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|_| {}).await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["title"] == "One")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let now_ms = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).to_string();
    s.ok("scrobble", &[("id", &one), ("time", &now_ms)]).await;
    s.ok(
        "syncSet",
        &[("key", "followedArtists"), ("value", r#"[{"name":"Talking Heads"}]"#), ("at", "1000"), ("id", "phone-aaaa1111")],
    )
    .await;

    // the first ask makes them: Discover finds songs you don't have and fetches them on trial for a week
    let r = s.ok("getMixes", &[]).await;
    let kinds: Vec<&str> = r["mixes"]["kind"].as_array().unwrap().iter().map(|k| k["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["discover", "radar", "daily", "daylist"]);
    let mut mixes = Value::Null;
    wait_for(
        async || {
            maestro::mixes::tick(&s.st).await;
            mixes = s.ok("getMixes", &[]).await["mixes"].clone();
            mixes["mix"].as_array().unwrap().iter().any(|m| m["kind"] == "discover")
        },
        "Discover Weekly",
    )
    .await;
    let dw = mixes["mix"].as_array().unwrap().iter().find(|m| m["kind"] == "discover").unwrap().clone();
    assert_eq!(dw["name"], "Discover Weekly");
    let pl = s.ok("getPlaylist", &[("id", dw["playlistId"].as_str().unwrap())]).await;
    assert_eq!(pl["playlist"]["entry"][0]["title"], "Psycho Killer");
    let pk = pl["playlist"]["entry"][0]["id"].as_str().unwrap().to_string();
    let (mix, hours, rel): (Option<String>, Option<f64>, String) =
        sqlx::query_as("SELECT mix, trial_hours, rel FROM request WHERE key = 'dz-t-900'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!((mix.as_deref(), hours), (Some("discover"), Some(168.0)));
    assert!(rel.starts_with("Trial/"), "{rel}");
    // Release Radar: today's single by an artist you follow
    let radar: Option<String> =
        sqlx::query_scalar("SELECT mix FROM request WHERE key = 'dz-t-901'").fetch_optional(&s.st.db).await.unwrap().flatten();
    assert!(radar.is_some(), "the new single was asked for");
    // a mix's song isn't in "On trial", and being in the mix doesn't keep it
    maestro::catalog::trial::tick(&s.st).await.unwrap();
    let on_trial = maestro::catalog::trial::playlist_id(&s.st, &s.st_user().await, false).await.unwrap();
    if let Some(p) = on_trial {
        let pl = s.ok("getPlaylist", &[("id", &p)]).await;
        assert!(pl["playlist"]["entry"].as_array().is_none_or(|e| e.iter().all(|x| x["id"] != pk.as_str())));
    }
    let still: String = sqlx::query_scalar("SELECT rel FROM request WHERE key = 'dz-t-900'").fetch_one(&s.st.db).await.unwrap();
    assert!(still.starts_with("Trial/"));
    // heart it: kept (moved to Songs/)
    s.ok("star", &[("id", &pk)]).await;
    maestro::catalog::trial::tick(&s.st).await.unwrap();
    let kept: String = sqlx::query_scalar("SELECT rel FROM request WHERE key = 'dz-t-900'").fetch_one(&s.st.db).await.unwrap();
    assert!(kept.starts_with("Songs/"), "{kept}");
    // a song that hasn't arrived is let go when the next mix comes
    maestro::catalog::let_go_of_mix_song(&s.st, "dz-t-901", radar.as_deref().unwrap()).await.unwrap();
    let gone: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE key = 'dz-t-901'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(gone, 0);
    // off: its playlist goes
    let r = s.ok("setMix", &[("kind", "discover"), ("enabled", "false")]).await;
    assert!(r["mixes"]["mix"].as_array().unwrap().iter().all(|m| m["kind"] != "discover"));
    assert_eq!(r["mixes"]["kind"][0]["enabled"], false);
    assert_eq!(s.call("getPlaylist", &[("id", dw["playlistId"].as_str().unwrap())]).await["error"]["code"], 70);
}

#[tokio::test]
async fn artist_info_and_similar_songs() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|_| {}).await;
    let r = s.ok("search3", &[("query", "")]).await;
    let id = |t: &str| r["searchResult3"]["song"].as_array().unwrap().iter().find(|x| x["title"] == t).unwrap().clone();
    let (one, three) = (id("One"), id("Three"));
    let alpha = one["artistId"].as_str().unwrap().to_string();
    let info = &s.ok("getArtistInfo2", &[("id", &alpha), ("count", "12")]).await["artistInfo2"];
    let sim = info["similarArtist"].as_array().unwrap();
    assert_eq!(sim.len(), 1, "only artists you have: {sim:?}");
    assert_eq!(sim[0]["name"], "The Beta Band");
    assert_eq!(sim[0]["id"], three["artistId"]);
    let all = &s.ok("getArtistInfo2", &[("id", &alpha), ("includeNotPresent", "true")]).await["artistInfo2"];
    assert!(all["similarArtist"].as_array().unwrap().iter().any(|a| a["id"] == "dz-r-7"));
    // similar songs: the artist's own and the related artists' you have, not the song itself
    let r = s.ok("getSimilarSongs2", &[("id", one["id"].as_str().unwrap()), ("count", "10")]).await;
    let titles: Vec<&str> = r["similarSongs2"]["song"].as_array().unwrap().iter().map(|x| x["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Two") && titles.contains(&"Three") && !titles.contains(&"One"), "{titles:?}");
    let r = s.ok("getSimilarSongs2", &[("id", &alpha)]).await;
    assert_eq!(r["similarSongs2"]["song"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn artist_bios_and_album_notes() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let (base, hits) = stand_in(|path, query, _| {
        let b = "http://pics";
        Some(match path {
            "/mb/artist" if query.contains("Alpha") => json!({ "artists": [{ "id": "mb-alpha", "name": "Alpha", "score": 100 }] }),
            "/mb/artist/mb-alpha" => json!({ "relations": [{ "type": "wikidata", "url": { "resource": "https://www.wikidata.org/wiki/Q1" } }] }),
            "/mb/release-group" if query.contains("First") => {
                json!({ "release-groups": [{ "id": "rg1", "title": "First Album", "score": 100 }] })
            }
            "/mb/release-group/rg1" => json!({ "relations": [{ "type": "wikidata", "url": { "resource": "https://www.wikidata.org/wiki/Q2" } }] }),
            "/wd/wiki/Special:EntityData/Q1.json" => json!({ "entities": { "Q1": { "sitelinks": { "enwiki": { "title": "Alpha (band)" } } } } }),
            "/wd/wiki/Special:EntityData/Q2.json" => {
                json!({ "entities": { "Q2": { "sitelinks": { "enwiki": { "title": "First Album" }, "ruwiki": { "title": "Pervy" } } } } })
            }
            "/wp/en/api/rest_v1/page/summary/Alpha_(band)" => json!({
                "type": "standard", "extract": "Alpha are a test band.",
                "content_urls": { "desktop": { "page": "https://en.wikipedia.org/wiki/Alpha_(band)" } },
                "thumbnail": { "source": format!("{b}/alpha.jpg") } }),
            "/wp/ru/api/rest_v1/page/summary/Pervy" => json!({ "type": "standard", "extract": "Первый альбом.",
                "content_urls": { "desktop": { "page": "https://ru.wikipedia.org/wiki/Pervy" } } }),
            "/caa/release-group/rg1" => json!({ "images": [{ "front": true, "image": format!("{b}/full.jpg"),
                "thumbnails": { "250": format!("{b}/250.jpg"), "500": format!("{b}/500.jpg"), "1200": format!("{b}/1200.jpg") } }] }),
            _ => return None,
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.info_online = true;
        c.info_lang = "ru".into();
        c.musicbrainz_url = format!("{base}/mb");
        c.wikidata_url = format!("{base}/wd");
        c.wikipedia_url = format!("{base}/wp/{{lang}}");
        c.coverart_url = format!("{base}/caa");
    })
    .await;
    let r = s.ok("search3", &[("query", "One")]).await;
    let one = r["searchResult3"]["song"][0].clone();
    let alpha = one["artistId"].as_str().unwrap().to_string();
    let info = s.ok("getArtistInfo2", &[("id", &alpha)]).await["artistInfo2"].clone();
    let bio = info["biography"].as_str().unwrap();
    assert!(bio.starts_with("Alpha are a test band. <a") && bio.contains("Read more on Wikipedia"), "{bio}");
    assert_eq!(info["musicBrainzId"], "mb-alpha");
    assert_eq!(info["largeImageUrl"], "http://pics/alpha.jpg", "Wikipedia's picture when Deezer has none");
    // album notes in the chosen language, pictures from the Cover Art Archive; getAlbumInfo takes a song id too
    let a = s.ok("getAlbumInfo2", &[("id", one["albumId"].as_str().unwrap())]).await["albumInfo"].clone();
    assert!(a["notes"].as_str().unwrap().starts_with("Первый альбом."), "{a}");
    assert_eq!(a["musicBrainzId"], "rg1");
    assert_eq!(a["smallImageUrl"], "http://pics/250.jpg");
    assert_eq!(a["largeImageUrl"], "http://pics/1200.jpg");
    let asked = hits.load(std::sync::atomic::Ordering::SeqCst);
    let again = s.ok("getAlbumInfo", &[("id", one["id"].as_str().unwrap())]).await["albumInfo"].clone();
    assert_eq!(again, a);
    s.ok("getArtistInfo", &[("id", &alpha)]).await;
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), asked, "answers are kept");
    // an artist nobody knows: nothing, and that is kept too
    let beta = s.ok("search3", &[("query", "Three")]).await["searchResult3"]["song"][0]["artistId"].as_str().unwrap().to_string();
    let none = s.ok("getArtistInfo2", &[("id", &beta)]).await["artistInfo2"].clone();
    assert!(none.get("biography").is_none());
    let asked = hits.load(std::sync::atomic::Ordering::SeqCst);
    s.ok("getArtistInfo2", &[("id", &beta)]).await;
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), asked);
}

#[tokio::test]
async fn genres_split_like_navidrome() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    tone_genre(&s.music.join("Gamma/Mixed/01 Four.flac"), "Four", "Gamma", "Mixed", 1, 2, "Pop, Rock");
    // a song scanned before genres were split
    sqlx::query("UPDATE track SET genre = 'Folk/Rock' WHERE title = 'Two'").execute(&s.st.db).await.unwrap();
    maestro::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let r = s.ok("getGenres", &[]).await;
    let g: Vec<(String, i64)> = r["genres"]["genre"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (x["value"].as_str().unwrap().to_string(), x["songCount"].as_i64().unwrap()))
        .collect();
    assert_eq!(g, [("Folk".into(), 1), ("Pop".into(), 1), ("Rock".into(), 2), ("Test".into(), 2)], "{g:?}");
    let r = s.ok("getSongsByGenre", &[("genre", "rock")]).await;
    let mut titles: Vec<&str> =
        r["songsByGenre"]["song"].as_array().unwrap().iter().map(|x| x["title"].as_str().unwrap()).collect();
    titles.sort();
    assert_eq!(titles, ["Four", "Two"]);
    let four = &r["songsByGenre"]["song"].as_array().unwrap().iter().find(|x| x["title"] == "Four").unwrap().clone();
    assert_eq!(four["genre"], "Pop");
    assert_eq!(four["genres"], serde_json::json!([{ "name": "Pop" }, { "name": "Rock" }]));
    let r = s.ok("getAlbumList2", &[("type", "byGenre"), ("genre", "Pop")]).await;
    assert_eq!(r["albumList2"]["album"][0]["name"], "Mixed");
    let r = s.ok("getRandomSongs", &[("genre", "Folk")]).await;
    assert_eq!(r["randomSongs"]["song"][0]["title"], "Two");
}

#[tokio::test]
async fn albums_grouped_like_navidrome() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let m = &s.music;
    let t = |title: &'static str| ("title", title);
    // a second album artist tag ("ALBUM ARTIST", as some taggers write it) makes another album, as in Navidrome
    tone_tags(&m.join("Delta/Box/01.flac"), 1, &[t("D1"), ("artist", "Delta"), ("album_artist", "Delta"), ("album", "Box")]);
    tone_tags(
        &m.join("Delta/Box/02.flac"),
        2,
        &[t("D2"), ("artist", "Delta"), ("album_artist", "Delta"), ("ALBUM ARTIST", "Delta Band"), ("album", "Box")],
    );
    // typographic and plain apostrophes are one album; a trailing space is another (Navidrome keeps it)
    tone_tags(&m.join("Echo/a.flac"), 3, &[t("E1"), ("artist", "Echo"), ("album_artist", "Echo"), ("album", "Rock’s Way")]);
    tone_tags(&m.join("Echo/b.flac"), 4, &[t("E2"), ("artist", "Echo"), ("album_artist", "Echo"), ("album", "Rock's Way")]);
    tone_tags(&m.join("Echo/c.flac"), 5, &[t("E3"), ("artist", "Echo"), ("album_artist", "Echo"), ("album", "Rock's Way ")]);
    // no album artist: the (first) artist, trimmed
    tone_tags(&m.join("Echo/d.flac"), 6, &[t("E4"), ("artist", "Echo "), ("album", "Rock's Way")]);
    maestro::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let album_of = async |title: &str| {
        let r = s.ok("search3", &[("query", title), ("songCount", "5")]).await;
        let song = r["searchResult3"]["song"].as_array().unwrap().iter().find(|x| x["title"] == title).unwrap().clone();
        song["albumId"].as_str().unwrap().to_string()
    };
    let (d1, d2) = (album_of("D1").await, album_of("D2").await);
    assert_ne!(d1, d2);
    let r = s.ok("getAlbum", &[("id", &d2)]).await;
    assert_eq!(r["album"]["artist"], "Delta • Delta Band");
    let delta = s.ok("getAlbum", &[("id", &d1)]).await["album"]["artistId"].clone();
    assert_eq!(r["album"]["artistId"], delta, "linked to the first album artist");
    let r = s.ok("getArtists", &[]).await;
    assert!(!r.to_string().contains("Delta Band"), "no joined artists in the index");
    let (e1, e2, e3, e4) = (album_of("E1").await, album_of("E2").await, album_of("E3").await, album_of("E4").await);
    assert_eq!(e1, e2);
    assert_ne!(e1, e3);
    assert_eq!(e1, e4);
}

#[tokio::test]
async fn music_lookup_requests_come_over() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let s = Server::start().await;
    let song = async |q: &str| s.ok("search3", &[("query", q)]).await["searchResult3"]["song"][0].clone();
    let (one, three) = (song("One").await, song("Three").await);
    // Navidrome had music-lookup's "On trial" and a playlist of yours (ids kept by import-navidrome)
    let on_trial = s.ok("createPlaylist", &[("name", "On trial"), ("songId", three["id"].as_str().unwrap())]).await["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mine = s.ok("createPlaylist", &[("name", "Mine")]).await["playlist"]["id"].as_str().unwrap().to_string();
    let state = tempfile::tempdir().unwrap();
    let t = |name: &str, artist: &str| json!({ "name": name, "artists": [artist], "album": "First Album", "ms": 3000, "isrc": "X" });
    let req = json!({
        "tracks": { "dz-t-1": t("One", "Alpha"), "dz-t-2": t("Three", "The Beta Band"), "dz-t-3": t("Four", "Alpha"),
                    "dz-t-4": t("Five", "Alpha"), "1x2y3z": t("Spotify key", "Alpha") },
        "state": {
            "dz-t-1": { "status": "done", "user": "u1", "file": "Music\\Alpha\\First Album\\01 One.flac", "same_album": true,
                        "rel": "Alpha/First Album/01 One.flac", "searches": 1, "last": 5, "since": 5 },
            "dz-t-2": { "status": "done", "folder": "Trial", "rel": "The Beta Band/Second/01 Three.mp3", "same_album": false },
            "dz-t-3": { "status": "queued", "user": "u2", "file": "Alpha\\Four.flac", "since": 7, "tried": ["u0"], "found": true },
            "dz-t-4": { "status": "notfound", "searches": 8, "last": 9 } },
        "want": {
            "dz-t-1": { "star": true, "playlists": [], "asked": 100, "applied": 101 },
            "dz-t-2": { "star": false, "playlists": [], "asked": 200, "trial": true, "trial_at": 200, "arrived": 300, "listed": true },
            "dz-t-3": { "star": false, "playlists": [mine], "asked": 400 },
            "dz-t-4": { "star": false, "playlists": [], "asked": 500, "trial": true, "trial_at": 500, "mix": "radar", "trial_hours": 168 } },
        "local": { "dz-t-1": one["id"], "dz-t-2": "an-id-navidrome-had" },
        "meta": { "trial_playlist": on_trial } });
    std::fs::write(state.path().join("requests.json"), req.to_string()).unwrap();
    let data = tempfile::tempdir().unwrap();
    let rep = maestro::import_nd::music_lookup(&s.st.db, data.path(), Some(state.path())).await.unwrap();
    assert_eq!((rep.requests, rep.requests_waiting, rep.requests_on_trial), (4, 1, 2), "{rep:?}");
    type R = (String, String, Option<String>, bool, Option<i64>, Option<String>, Option<bool>, String);
    let rows: Vec<R> = sqlx::query_as(
        "SELECT key, status, local_id, trial, arrived_at, slsk_user, same_album, folder FROM request ORDER BY key",
    )
    .fetch_all(&s.st.db)
    .await
    .unwrap();
    assert_eq!(rows.len(), 4, "only Deezer keys");
    assert_eq!(rows[0].2.as_deref(), one["id"].as_str());
    assert_eq!(rows[0].6, Some(true));
    // found by the file it was filed as (the id music-lookup had is gone); on trial since it arrived
    assert_eq!(rows[1].2.as_deref(), three["id"].as_str());
    assert_eq!((rows[1].3, rows[1].4, rows[1].7.as_str()), (true, Some(300), "Trial"));
    assert_eq!((rows[2].1.as_str(), rows[2].5.as_deref()), ("queued", Some("u2")), "the download goes on");
    assert_eq!(rows[3].1, "notfound");
    // the same "On trial" playlist goes on; the wish to add Four to Mine waits for it
    let uid: String = sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(USER).fetch_one(&s.st.db).await.unwrap();
    let pid = maestro::catalog::trial::playlist_id(&s.st, &uid, false).await.unwrap();
    assert_eq!(pid.as_deref(), Some(on_trial.as_str()));
    let wants: Vec<(String, String, bool, bool)> =
        sqlx::query_as("SELECT key, playlist_id, star, done FROM request_want ORDER BY key").fetch_all(&s.st.db).await.unwrap();
    assert_eq!(wants, [("dz-t-1".into(), "".into(), true, true), ("dz-t-3".into(), mine.clone(), false, false)]);
    // a second run leaves what's there alone
    let again = maestro::import_nd::music_lookup(&s.st.db, data.path(), Some(state.path())).await.unwrap();
    assert_eq!((again.requests, again.requests_already_there), (0, 4));
}

#[tokio::test]
async fn pick_a_file_by_hand() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("complete");
    tone(&tmp.path().join("four.flac"), "Four", "Alpha", "First Album", 4, 2);
    let flac = std::fs::read(tmp.path().join("four.flac")).unwrap();
    let remote = r"Music\Alpha\First Album\04 Four.flac";
    let sent = Arc::new(AtomicBool::new(false));
    let (sent2, dl2) = (sent.clone(), downloads.clone());
    let (base, _) = stand_in(move |path, _q, body| {
        Some(match path {
            "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
            "/api/v0/searches" => json!({}),
            p if p.ends_with("/responses") => json!([{ "username": "peer", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000, "files": [
                { "filename": remote, "size": flac.len(), "bitRate": 0, "length": 2, "extension": "flac" },
                { "filename": r"Music\Alpha\Live\Four (Live).flac", "size": 9, "length": 2 },
                { "filename": r"Music\Alpha\First Album\cover.jpg", "size": 9 }] }]),
            p if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
            "/api/v0/transfers/downloads/peer" if !body.is_empty() => {
                let d = dl2.join("First Album");
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join("04 Four.flac"), &flac).unwrap();
                sent2.store(true, SeqCst);
                json!({})
            }
            "/api/v0/transfers/downloads" => {
                if sent2.load(SeqCst) {
                    json!([{ "username": "peer", "directories": [{ "files": [{ "id": "t1", "filename": remote, "state": "Completed, Succeeded" }] }] }])
                } else {
                    json!([])
                }
            }
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.slskd_downloads = Some(downloads.clone());
        c.slskd_fast_test = true;
    })
    .await;
    sqlx::query(
        "INSERT INTO request (key, title, artists_json, album, duration_ms, status, searches, found, asked_at)
         VALUES ('dz-t-9', 'Four', '[\"Alpha\"]', 'First Album', 2000, 'notfound', 8, 1, 1)",
    )
    .execute(&s.st.db)
    .await
    .unwrap();
    let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let api = |p: &str| format!("{}/admin/api/{p}", s.base);
    let login = json!({ "username": USER, "password": PASS });
    assert_eq!(http.post(api("login")).header("X-Maestro", "1").json(&login).send().await.unwrap().status(), 200);
    let get = async |p: &str| -> Value { http.get(api(p)).send().await.unwrap().json().await.unwrap() };
    let post = async |p: &str, b: Value| http.post(api(p)).header("X-Maestro", "1").json(&b).send().await.unwrap();
    let l = get("pick").await;
    let song = l["songs"].as_array().unwrap().iter().find(|x| x["key"] == "dz-t-9").unwrap().clone();
    assert_eq!((song["group"].as_str(), song["query"].as_str()), (Some("pick"), Some("Alpha Four")));
    // search in your own words: every audio file, the best first
    assert_eq!(post("pick/dz-t-9/search", json!({ "q": "al" })).await.status(), 400);
    let r: Value = post("pick/dz-t-9/search", json!({ "q": "alpha four" })).await.json().await.unwrap();
    let res = r["results"].as_array().unwrap();
    assert_eq!(res.len(), 2, "no pictures: {res:?}");
    assert_eq!((res[0]["sameAlbum"].as_bool(), res[0]["match"].as_bool()), (Some(true), Some(true)));
    assert_eq!(res[1]["match"], false, "a live version isn't what was asked for, but you may still pick it");
    // set aside and back
    post("pick/dz-t-9/skip", json!({ "on": true })).await;
    let g = get("pick").await;
    assert_eq!(g["songs"][0]["group"], "skipped");
    post("pick/dz-t-9/skip", json!({ "on": false })).await;
    assert_eq!(get("pick").await["songs"][0]["status"], "notfound");
    // pick it: downloaded, filed, scanned like any download
    let f = &res[0];
    let picked = post("pick/dz-t-9", json!({ "user": f["user"], "filename": f["filename"], "size": f["size"] })).await;
    assert_eq!(picked.status(), 200);
    assert!(sent.load(SeqCst));
    tokio::spawn(maestro::catalog::acquire::worker(s.st.clone()));
    wait_for(
        async || sqlx::query_scalar::<_, Option<String>>("SELECT local_id FROM request WHERE key = 'dz-t-9'").fetch_one(&s.st.db).await.unwrap().is_some(),
        "the picked file to be filed and scanned",
    )
    .await;
    assert!(s.music.join("Songs/Alpha/Alpha - Four.flac").is_file());
    let (manual, same): (bool, Option<bool>) =
        sqlx::query_as("SELECT manual, same_album FROM request WHERE key = 'dz-t-9'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!((manual, same), (true, Some(true)));
    let again = post("pick/dz-t-9", json!({ "user": "peer", "filename": remote, "size": 1 })).await;
    assert_eq!(again.status(), 409, "it isn't waiting for a file any more");
}

#[tokio::test]
async fn better_copy_swapped_in_same_id() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::Mutex;
    let tmp = tempfile::tempdir().unwrap();
    let downloads = tmp.path().join("complete");
    tone(&tmp.path().join("four.flac"), "Four", "Alpha", "First Album", 4, 2);
    tone(&tmp.path().join("five.flac"), "Five", "Alpha", "First Album", 5, 20); // not the song's length
    let files: std::collections::HashMap<&str, Vec<u8>> = [
        ("04 Four.flac", std::fs::read(tmp.path().join("four.flac")).unwrap()),
        ("05 Five.flac", std::fs::read(tmp.path().join("five.flac")).unwrap()),
    ]
    .into();
    let asked = Arc::new(Mutex::new(String::new()));
    let sent: Arc<Mutex<Vec<String>>> = Arc::default();
    let (a2, s2, dl2) = (asked.clone(), sent.clone(), downloads.clone());
    let (base, _) = stand_in(move |path, _q, body| {
        let b: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
        Some(match path {
            "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
            "/api/v0/searches" => {
                *a2.lock().unwrap() = b["searchText"].as_str().unwrap_or("").to_string();
                json!({})
            }
            p if p.ends_with("/responses") => {
                let name = if a2.lock().unwrap().contains("Four") { "04 Four.flac" } else { "05 Five.flac" };
                json!([{ "username": "peer", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000, "files": [
                    { "filename": format!(r"Music\Alpha\First Album\{name}"), "size": 1, "length": 2, "extension": "flac" }] }])
            }
            p if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
            "/api/v0/transfers/downloads/peer" if !body.is_empty() => {
                let remote = b[0]["filename"].as_str().unwrap().to_string();
                let name = remote.rsplit('\\').next().unwrap().to_string();
                std::fs::create_dir_all(dl2.join("First Album")).unwrap();
                std::fs::write(dl2.join("First Album").join(&name), &files[name.as_str()]).unwrap();
                s2.lock().unwrap().push(remote);
                json!({})
            }
            "/api/v0/transfers/downloads" => {
                let f: Vec<Value> = s2.lock().unwrap().iter().enumerate()
                    .map(|(i, r)| json!({ "id": format!("t{i}"), "filename": r, "state": "Completed, Succeeded" })).collect();
                json!([{ "username": "peer", "directories": [{ "files": f }] }])
            }
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.slskd_downloads = Some(downloads.clone());
        c.slskd_fast_test = true;
    })
    .await;
    // two songs Maestro downloaded earlier as MP3 from the right album
    tone(&s.music.join("Songs/Alpha/Alpha - Four.mp3"), "Four", "Alpha", "First Album", 4, 2);
    tone(&s.music.join("Songs/Alpha/Alpha - Five.mp3"), "Five", "Alpha", "First Album", 5, 2);
    maestro::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let id_of = async |t: &str| s.ok("search3", &[("query", t)]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let (four, five) = (id_of("Four").await, id_of("Five").await);
    s.ok("star", &[("id", &four)]).await;
    for (key, title, id, at) in [("dz-t-4", "Four", &four, 2), ("dz-t-5", "Five", &five, 1)] {
        sqlx::query(
            "INSERT INTO request (key, title, artists_json, album, duration_ms, status, rel, local_id, same_album, arrived_at, asked_at)
             VALUES (?, ?, '[\"Alpha\"]', 'First Album', 2000, 'done', ?, ?, 1, ?, 1)",
        )
        .bind(key)
        .bind(title)
        .bind(format!("Songs/Alpha/Alpha - {title}.mp3"))
        .bind(id)
        .bind(at)
        .execute(&s.st.db)
        .await
        .unwrap();
    }
    tokio::spawn(maestro::catalog::acquire::worker(s.st.clone()));
    wait_for(
        async || {
            let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE upgrade_tried = 1 AND status = 'done'")
                .fetch_one(&s.st.db)
                .await
                .unwrap();
            n == 2
        },
        "both songs to be looked at",
    )
    .await;
    // Four: the FLAC is in, the MP3 gone, the same song id (and heart)
    assert!(s.music.join("Songs/Alpha/Alpha - Four.flac").is_file());
    assert!(!s.music.join("Songs/Alpha/Alpha - Four.mp3").exists());
    let song = s.ok("getSong", &[("id", &four)]).await["song"].clone();
    assert_eq!((song["suffix"].as_str(), song["path"].as_str().map(|p| p.ends_with("Four.flac"))), (Some("flac"), Some(true)), "{song}");
    assert!(song["starred"].is_string());
    let rel: String = sqlx::query_scalar("SELECT rel FROM request WHERE key = 'dz-t-4'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(rel, "Songs/Alpha/Alpha - Four.flac");
    // Five: the download didn't check out, so the MP3 stays and nothing is left behind
    assert!(s.music.join("Songs/Alpha/Alpha - Five.mp3").is_file());
    assert!(!s.music.join("Songs/Alpha/Alpha - Five.flac").exists());
    assert_eq!(s.ok("getSong", &[("id", &five)]).await["song"]["suffix"], "mp3");
    let left: Vec<_> = std::fs::read_dir(s.music.join("Songs/Alpha")).unwrap().filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with('.')).collect();
    assert!(left.is_empty(), "temporary files left: {left:?}");
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE title IN ('Four', 'Five') AND missing_since IS NULL").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(n, 2, "no extra songs");
}

#[tokio::test]
async fn dj_lines() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::Mutex;
    // Ollama: writes the line (and keeps the prompt it was given)
    let prompts: Arc<Mutex<Vec<String>>> = Arc::default();
    let p2 = prompts.clone();
    let (ollama, _) = stand_in(move |path, _, body| {
        (path == "/api/chat").then(|| {
            let b: Value = serde_json::from_slice(body).unwrap();
            p2.lock().unwrap().push(b["messages"][0]["content"].as_str().unwrap().to_string());
            json!({ "message": { "content": json!({ "line": "Up next: \"One\" by Alpha." }).to_string() } })
        })
    })
    .await;
    // the voice: English only; one second of WAV
    let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
    for (v, n) in [(16u32, 4), (1, 2), (1, 2), (8000, 4), (16000, 4), (2, 2), (16, 2)] {
        wav.extend(&v.to_le_bytes()[..n]);
    }
    wav.extend(b"data");
    wav.extend(16000u32.to_le_bytes());
    wav.extend(vec![0u8; 16000]);
    let w2 = wav.clone();
    let vl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let voice = format!("http://{}", vl.local_addr().unwrap());
    let app = axum::Router::new().route(
        "/speak",
        axum::routing::post(move |axum::Json(b): axum::Json<Value>| {
            let w = w2.clone();
            async move {
                if b["voice"].as_str().unwrap().starts_with("en_") { (axum::http::StatusCode::OK, w) } else { (axum::http::StatusCode::NOT_FOUND, vec![]) }
            }
        }),
    );
    tokio::spawn(async move { axum::serve(vl, app).await.unwrap() });

    let off = Server::start().await;
    let one = off.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(off.ok("getDjLine", &[("next", &one)]).await["djLine"]["error"], "ai-off");

    let s = Server::start_with(|c| {
        c.catalog = false;
        c.ollama_url = Some(ollama.clone());
        c.dj_voice_url = Some(voice.clone());
    })
    .await;
    let song = async |q: &str| s.ok("search3", &[("query", q)]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let (one, two) = (song("One").await, song("Two").await);
    s.ok("star", &[("id", &one)]).await;
    // no Russian voice: English then
    let l = s.ok("getDjLine", &[("next", &one), ("prev", &two), ("lang", "ru")]).await["djLine"].clone();
    assert_eq!(l["text"], "Up next: One by Alpha.");
    assert_eq!(l["duration"], 1.0);
    let id = l["id"].as_str().unwrap().to_string();
    {
        let ps = prompts.lock().unwrap();
        assert_eq!(ps.len(), 2, "written again in English");
        assert!(ps[0].contains("spoken Russian") && ps[1].contains("spoken English"));
        assert!(ps[1].contains("\"title\":\"One\"") && ps[1].contains("\"you liked it\":true") && ps[1].contains("\"you have played it\":\"never\""), "{}", ps[1]);
        assert!(ps[1].contains("Previous song: {\"album\":\"First Album\",\"artist\":\"Alpha\""), "{}", ps[1]);
    }
    // kept: asking again is free; the audio plays
    assert_eq!(s.ok("getDjLine", &[("next", &one), ("prev", &two), ("lang", "ru")]).await["djLine"], l);
    assert_eq!(prompts.lock().unwrap().len(), 2);
    let a = s.http.get(s.url("djAudio", &[("id", &id)])).send().await.unwrap();
    assert_eq!(a.headers()["content-type"], "audio/wav");
    assert_eq!(a.bytes().await.unwrap().to_vec(), wav);
    assert_eq!(s.call("djAudio", &[("id", "../../maestro.db")]).await["error"]["code"], 70);
    assert_eq!(s.ok("getDjLine", &[("next", "nope")]).await["djLine"]["error"], "unknown song");
}

fn zip_of(files: &[(&str, String)]) -> Vec<u8> {
    use std::io::Write;
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in files {
        z.start_file(*name, o).unwrap();
        z.write_all(body.as_bytes()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

async fn wait_import(s: &Server, id: &str, until: &str) -> Value {
    for _ in 0..200 {
        let b = s.ok("getImport", &[("id", id)]).await["import"].clone();
        if b["status"] == until {
            return b;
        }
        assert_ne!(b["status"], "failed", "{b}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("import {id} never got to {until}");
}

#[tokio::test]
async fn import_a_spotify_export() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let s = Server::start().await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0].clone();
    let one_id = one["id"].as_str().unwrap().to_string();
    let t = |name: &str, artist: &str, album: &str, uri: &str| json!({ "trackName": name, "artistName": artist, "albumName": album, "trackUri": uri });
    let play = |ts: &str, ms: i64, name: &str, artist: &str| json!({ "ts": ts, "platform": "secret-phone", "ms_played": ms, "conn_country": "ZZ",
        "ip_addr": "203.0.113.9", "master_metadata_track_name": name, "master_metadata_album_artist_name": artist,
        "master_metadata_album_album_name": "", "spotify_track_uri": format!("spotify:track:{:0>22}", name.bytes().map(u64::from).sum::<u64>()), "skipped": false });
    let export = zip_of(&[
        ("Spotify Account Data/YourLibrary.json", json!({
            "tracks": [{ "artist": "Alpha", "album": "First Album", "track": "One", "uri": "spotify:track:1111111111111111111111" }],
            "albums": [{ "artist": "Alpha", "album": "First Album", "uri": "x" }],
            "artists": [{ "name": "Alpha", "uri": "x" }, { "name": "Nobody Here", "uri": "y" }], "shows": [] }).to_string()),
        ("Spotify Account Data/Playlist1.json", json!({ "playlists": [{ "name": "Road", "lastModifiedDate": "2024-01-01", "items": [
            { "track": t("Three", "The Beta Band", "Second", "spotify:track:3333333333333333333333") },
            { "track": t("One", "Alpha", "First Album", "spotify:track:1111111111111111111111") },
            { "track": t("Not Here", "Alpha", "First Album", "spotify:track:9999999999999999999999") }] }] }).to_string()),
        ("Spotify Account Data/SearchQueries.json", json!([{ "searchQuery": "a secret search" }]).to_string()),
        ("Spotify Extended Streaming History/Streaming_History_Audio_2024.json", json!([
            play("2024-03-01T10:00:00Z", 180000, "One", "Alpha"), play("2024-03-02T10:00:00Z", 180000, "One", "Alpha"),
            play("2024-03-02T11:00:00Z", 5000, "Two", "Alpha"), play("2024-03-03T10:00:00Z", 200000, "Not Here At All", "Alpha")]).to_string()),
    ]);
    let upload = async |body: Vec<u8>| -> Value {
        let r: Value = s.http.post(s.url("importUpload", &[("name", "my_spotify_data.zip")])).body(body).send().await.unwrap().json().await.unwrap();
        assert_eq!(r["subsonic-response"]["status"], "ok", "{r}");
        r["subsonic-response"]["import"].clone()
    };
    let b = upload(export.clone()).await;
    let id = b["id"].as_str().unwrap().to_string();
    // the preview: found, matched, what it would change; nothing changed yet
    let p = wait_import(&s, &id, "preview").await;
    let r = &p["report"];
    assert_eq!((r["found"]["liked"].as_i64(), r["matched"]["liked"].as_i64()), (Some(1), Some(1)), "{r}");
    assert_eq!((r["found"]["plays"].as_i64(), r["found"]["skips"].as_i64(), r["matched"]["plays"].as_i64()), (Some(3), Some(1), Some(2)));
    assert_eq!(r["changes"]["plays"], 2);
    assert_eq!(r["missingTotal"], 2, "the playlist's song and the played one");
    assert_eq!(r["artistsMissing"], json!(["Nobody Here"]));
    let dump = p.to_string();
    assert!(!dump.contains("203.0.113.9") && !dump.contains("secret") && !dump.contains("ZZ\""), "nothing private kept: {dump}");
    assert!(s.ok("getStarred2", &[]).await["starred2"]["song"].as_array().is_none_or(|a| a.is_empty()));
    // import it
    s.ok("startImport", &[("id", &id)]).await;
    let done = wait_import(&s, &id, "done").await;
    assert_eq!(done["report"]["changes"]["hearts"], 1, "{done}");
    let starred = s.ok("getStarred2", &[]).await["starred2"].clone();
    assert_eq!(starred["song"][0]["id"], one_id.as_str());
    assert_eq!(starred["album"][0]["name"], "First Album");
    let follows: String = sqlx::query_scalar("SELECT value_json FROM sync_kv WHERE key = 'followedArtists'").fetch_one(&s.st.db).await.unwrap();
    assert!(follows.contains("\"name\":\"Alpha\""), "{follows}");
    let lists = s.ok("getPlaylists", &[]).await["playlists"]["playlist"].clone();
    let road = lists.as_array().unwrap().iter().find(|p| p["name"] == "Road").unwrap().clone();
    assert!(road["comment"].as_str().unwrap().contains("Imported from Spotify"));
    let songs = s.ok("getPlaylist", &[("id", road["id"].as_str().unwrap())]).await["playlist"]["entry"].clone();
    let titles: Vec<&str> = songs.as_array().unwrap().iter().map(|x| x["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Three", "One"], "in Spotify's order");
    let song = s.ok("getSong", &[("id", &one_id)]).await["song"].clone();
    assert_eq!(song["playCount"], 2);
    assert_eq!(song["played"].as_str().map(|p| p.starts_with("2024-03-02")), Some(true), "{song}");
    let skips: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skip").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(skips, 1);
    let left = std::fs::read_dir(s.st.cfg.data_dir.join("imports")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(left, 0, "the upload is deleted once imported");
    // the same export again: nothing new
    let again = upload(export).await;
    let a = wait_import(&s, again["id"].as_str().unwrap(), "preview").await;
    let c = &a["report"]["changes"];
    assert_eq!((c["plays"].as_i64(), c["playsAlreadyThere"].as_i64(), c["hearts"].as_i64(), c["playlistsUpdated"].as_i64()), (Some(0), Some(2), Some(0), Some(1)), "{c}");
    s.ok("deleteImport", &[("id", again["id"].as_str().unwrap())]).await;
    assert_eq!(s.ok("getImports", &[]).await["imports"]["import"].as_array().unwrap().len(), 1);
    // undo: everything it added goes
    s.ok("undoImport", &[("id", &id)]).await;
    assert_eq!(s.ok("getImport", &[("id", &id)]).await["import"]["status"], "undone");
    assert!(s.ok("getStarred2", &[]).await["starred2"]["song"].as_array().is_none_or(|a| a.is_empty()));
    assert!(!s.ok("getPlaylists", &[]).await.to_string().contains("\"Road\""));
    let song = s.ok("getSong", &[("id", &one_id)]).await["song"].clone();
    assert!(song["playCount"].as_i64().unwrap_or(0) == 0, "{song}");
    let left: (i64, i64) = sqlx::query_as("SELECT (SELECT COUNT(*) FROM skip), (SELECT COUNT(*) FROM play)").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(left, (0, 0));
    let follows: String = sqlx::query_scalar("SELECT value_json FROM sync_kv WHERE key = 'followedArtists'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(follows, "[]");
    // someone else can't see or touch it
    assert_eq!(s.call("undoImport", &[("id", "nope")]).await["error"]["code"], 70);
}

#[tokio::test]
async fn import_into_any_subsonic_server() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let lib = r#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>Tracks</key><dict>
      <key>1</key><dict><key>Track ID</key><integer>1</integer><key>Name</key><string>Two</string><key>Artist</key><string>Alpha</string>
        <key>Album</key><string>First Album</string><key>Loved</key><true/><key>Rating</key><integer>100</integer>
        <key>Play Count</key><integer>7</integer><key>Play Date UTC</key><date>2021-06-01T12:00:00Z</date></dict>
      <key>2</key><dict><key>Track ID</key><integer>2</integer><key>Name</key><string>Three</string><key>Artist</key><string>The Beta Band</string></dict>
    </dict><key>Playlists</key><array><dict><key>Name</key><string>Old iPod</string><key>Playlist Items</key><array>
      <dict><key>Track ID</key><integer>2</integer></dict><dict><key>Track ID</key><integer>1</integer></dict></array></dict></array></dict></plist>"#;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Library.xml"), lib).unwrap();
    let parsed = maestro::import::parse_path(&dir.path().join("Library.xml")).unwrap();
    // this test's server stands in for any Subsonic server (Navidrome before a switch)
    let server = maestro::import::subsonic::Server::new(&s.base, USER, PASS);
    let library = server.library().await.unwrap();
    assert_eq!(library.len(), 3);
    let plan = maestro::import::apply::plan(&parsed, &library, &Default::default(), &Default::default());
    let state_path = dir.path().join("sent.json");
    let mut state = maestro::import::subsonic::State::default();
    let dry = server.apply(&plan, "Imported from iTunes", &mut state, &state_path, true).await.unwrap();
    assert_eq!((dry.hearts, dry.plays, dry.playlists_new), (1, 1, 1));
    assert!(s.ok("getStarred2", &[]).await["starred2"]["song"].as_array().is_none_or(|a| a.is_empty()), "a dry run sends nothing");
    let c = server.apply(&plan, "Imported from iTunes", &mut state, &state_path, false).await.unwrap();
    assert_eq!((c.hearts, c.ratings, c.playlists_new), (1, 1, 1));
    let two = s.ok("search3", &[("query", "Two")]).await["searchResult3"]["song"][0].clone();
    assert!(two["starred"].is_string());
    assert_eq!(two["userRating"], 5);
    assert_eq!(two["played"].as_str().map(|p| p.starts_with("2021-06-01")), Some(true), "the last play, at its time: {two}");
    let lists = s.ok("getPlaylists", &[]).await.to_string();
    assert!(lists.contains("Old iPod") && lists.contains("Imported from iTunes"));
    // again: nothing new is sent; undo takes the hearts and the playlist back
    let mut state = maestro::import::subsonic::State::load(&state_path);
    let again = server.apply(&plan, "Imported from iTunes", &mut state, &state_path, false).await.unwrap();
    assert_eq!((again.hearts, again.plays, again.playlists_new, again.playlists_updated), (0, 0, 0, 1));
    server.undo(&mut state, &state_path).await.unwrap();
    assert!(s.ok("getStarred2", &[]).await["starred2"]["song"].as_array().is_none_or(|a| a.is_empty()));
    assert!(!s.ok("getPlaylists", &[]).await.to_string().contains("Old iPod"));
}

#[tokio::test]
async fn import_from_lastfm_and_deezer() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let (base, _) = stand_in(|path, q, _| {
        let tr = |name: &str, uts: &str| json!({ "name": name, "artist": { "#text": "Alpha", "name": "Alpha" }, "album": { "#text": "First Album" }, "date": { "uts": uts } });
        let dz = |id: i64, title: &str, artist: &str| json!({ "id": id, "title": title, "duration": 3, "artist": { "name": artist }, "album": { "title": "x" }, "time_add": 1650000000 });
        Some(match path {
            "/lfm" if q.contains("getlovedtracks") => json!({ "lovedtracks": { "track": [tr("One", "1600000000")], "@attr": { "totalPages": "1" } } }),
            "/lfm" if q.contains("page=1") => json!({ "recenttracks": { "@attr": { "totalPages": "2" }, "track": [
                { "name": "Two", "artist": { "#text": "Alpha" }, "@attr": { "nowplaying": "true" } }, tr("One", "1700000000")] } }),
            "/lfm" => json!({ "recenttracks": { "@attr": { "totalPages": "2" }, "track": [tr("Two", "1700000500")] } }),
            "/dz/user/42" => json!({ "id": 42, "name": "x" }),
            "/dz/user/42/tracks" => json!({ "data": [dz(801, "One", "Alpha")] }),
            "/dz/user/42/playlists" => json!({ "data": [{ "id": 5, "title": "Dz list", "is_loved_track": false }, { "id": 6, "title": "Loved", "is_loved_track": true }] }),
            "/dz/playlist/5/tracks" => json!({ "data": [dz(901, "Three", "The Beta Band"), dz(902, "Missing", "Nobody")] }),
            "/dz/user/42/podcasts" => json!({ "data": [{ "id": 1, "title": "Song Exploder" }] }),
            p if p.starts_with("/dz/user/42/") => json!({ "data": [] }),
            "/itunes/search" => json!({ "results": [{ "collectionName": "Song Exploder", "artistName": "Hrishikesh Hirway", "feedUrl": "http://feeds.example/se" }] }),
            _ => return None,
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.lastfm_url = format!("{base}/lfm");
        c.lastfm_api_key = Some("k".into());
        c.deezer_url = format!("{base}/dz");
        c.itunes_url = format!("{base}/itunes");
    })
    .await;
    let lfm = s.ok("importOnline", &[("source", "lastfm"), ("account", "bob")]).await["import"]["id"].as_str().unwrap().to_string();
    let r = wait_import(&s, &lfm, "preview").await["report"].clone();
    assert_eq!((r["found"]["liked"].as_i64(), r["found"]["plays"].as_i64(), r["matched"]["plays"].as_i64()), (Some(1), Some(2), Some(2)), "{r}");
    assert_eq!(r["sources"], json!(["Last.fm"]));
    let dz = s.ok("importOnline", &[("source", "deezer"), ("account", "https://www.deezer.com/profile/42")]).await["import"]["id"].as_str().unwrap().to_string();
    let r = wait_import(&s, &dz, "preview").await["report"].clone();
    assert_eq!((r["found"]["liked"].as_i64(), r["found"]["playlists"].as_i64(), r["matched"]["playlistSongs"].as_i64(), r["found"]["podcasts"].as_i64()), (Some(1), Some(1), Some(1), Some(1)), "{r}");
    assert_eq!(maestro::import::online::podcast_feed(&format!("{base}/itunes"), "Song Exploder", "").await.as_deref(), Some("http://feeds.example/se"));
    // without a key, Last.fm says why
    let off = Server::start().await;
    let id = off.ok("importOnline", &[("source", "lastfm"), ("account", "bob")]).await["import"]["id"].as_str().unwrap().to_string();
    for _ in 0..100 {
        let b = off.ok("getImport", &[("id", &id)]).await["import"].clone();
        if b["status"] == "failed" {
            assert!(b["error"].as_str().unwrap().contains("lastfm_api_key"));
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("no failure");
}

#[tokio::test]
async fn import_upload_of_one_file_labelled_as_a_form() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    // curl --data-binary and some apps label any body as a form; the format is told from the name it was sent with
    let body = r#"[{"ts":"2024-03-01T10:00:00Z","ms_played":180000,"master_metadata_track_name":"One","master_metadata_album_artist_name":"Alpha","spotify_track_uri":"spotify:track:1111111111111111111111"}]"#;
    let r: Value = s
        .http
        .post(s.url("importUpload", &[("name", "Streaming_History_Audio_2024.json")]))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = r["subsonic-response"]["import"]["id"].as_str().unwrap_or_else(|| panic!("{r}")).to_string();
    let b = wait_import(&s, &id, "preview").await;
    assert_eq!(b["report"]["matched"]["plays"], 1, "{b}");
}

#[tokio::test]
async fn music_lookup_mixes_shares_and_concerts_come_over() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    // what Navidrome had: music-lookup's Daily Mix 1 and a shared playlist (ids kept by import-navidrome)
    let dm = s.ok("createPlaylist", &[("name", "Daily Mix 1"), ("songId", &one)]).await["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let party = s.ok("createPlaylist", &[("name", "Party")]).await["playlist"]["id"].as_str().unwrap().to_string();
    let data = tempfile::tempdir().unwrap();
    let w = |p: &str, v: Value| {
        let f = data.path().join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, v.to_string()).unwrap();
    };
    w(
        "mixes/state.json",
        serde_json::json!({ "enabled": { "radar": false }, "playlists": { "daily-1": dm, "daily-2": "gone" },
        "last": { "daily": 1700000000.5 }, "current": {}, "history": { "discover": ["dz-t-1"] }, "about": { "daily-1": { "count": 1, "at": 1700000000 } } }),
    );
    w(
        "encore/playlist-shares.json",
        serde_json::json!({ "AbCdEfGhIjKlMnOpQrStUvWx": { "playlist": party, "owner": USER, "created": 2000000000.0,
        "added": [{ "id": one, "by": "Bob", "guest": "g1", "at": 1 }] } }),
    );
    w(
        "concerts/concerts.json",
        serde_json::json!({ "show": [{ "artist": "Alpha", "date": "2099-01-01", "venue": "Club" }], "first": { "alpha|2099-01-01|club": 5 },
        "at": 1700000000, "city": "Berlin", "source": "MusicBrainz", "prefs": "{}" }),
    );
    let rep = maestro::import_nd::music_lookup(&s.st.db, data.path(), None).await.unwrap();
    assert_eq!((rep.mix_playlists, rep.playlist_shares, rep.concerts), (1, 1, 1));
    let r = s.ok("getMixes", &[]).await;
    let mixes = r["mixes"]["mix"].as_array().unwrap();
    assert_eq!(mixes.len(), 1);
    assert_eq!(mixes[0]["playlistId"], dm.as_str()); // the same playlist goes on being Daily Mix 1
    assert!(r["mixes"]["kind"].as_array().unwrap().iter().any(|k| k["kind"] == "radar" && k["enabled"] == false));
    let sh = s.ok("playlistShares", &[]).await;
    assert_eq!(sh["playlistShares"]["share"][0]["url"], "https://jam.example.test/jam/pl/AbCdEfGhIjKlMnOpQrStUvWx");
    assert_eq!(sh["playlistShares"]["share"][0]["added"][0]["by"], "Bob");
    let c = s.ok("getConcerts", &[]).await;
    assert_eq!(c["concerts"]["show"][0]["venue"], "Club");
    assert_eq!(c["concerts"]["city"], "Berlin");
}
