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
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let ok = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("sine=frequency={}:duration={secs}", 220 * track)])
        .args(["-metadata", &format!("title={title}"), "-metadata", &format!("artist={artist}")])
        .args(["-metadata", &format!("album={album}"), "-metadata", &format!("track={track}"), "-metadata", "date=2021"])
        .args(["-metadata", "genre=Test", "-metadata", "REPLAYGAIN_TRACK_GAIN=-6.50 dB", "-metadata", "ISRC=USABC2100001"])
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
            jam_public_url: Some("https://jam.example.test".into()),
            ..Config::default()
        };
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
