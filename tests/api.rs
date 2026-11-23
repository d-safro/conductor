//! End-to-end: make a tiny music folder with ffmpeg, start the server in-process, talk Subsonic to it.

use std::{path::Path, process::Command, sync::Arc};

use conductor::{
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
        // a test that turns online info on without its own stand-in for Deezer still never reaches the real one
        c.deezer_url = "http://127.0.0.1:9".into();
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
        let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
        let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
        auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
        let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
        let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
        let rep = library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap();
        assert_eq!(rep.added, 3);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = api::router(st.clone());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
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
        let r = r["subsonic-response"].clone();
        contract::observe(method, &r);
        r
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
    // kept as a file name, so it's still found once the data folder moves; a whole path as older versions kept
    // it (here under the old folder name) finds the file of that name in this data folder
    let kept: String =
        sqlx::query_scalar("SELECT image_path FROM playlist WHERE id = ?").bind(&sp).fetch_one(&s.st.db).await.unwrap();
    assert!(!kept.contains(['/', '\\']), "{kept}");
    let old_path = s.st.cfg.data_dir.parent().unwrap().join("maestro").join("playlist-images").join(&kept);
    sqlx::query("UPDATE playlist SET image_path = ? WHERE id = ?")
        .bind(old_path.to_string_lossy().to_string())
        .bind(&sp)
        .execute(&s.st.db)
        .await
        .unwrap();
    assert_eq!(s.http.get(s.url("getCoverArt", &[("id", &cover)])).send().await.unwrap().status(), 200);
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
async fn a_song_ffmpeg_cant_read_is_an_error_not_silence() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    std::fs::create_dir_all(s.music.join("Broken")).unwrap();
    std::fs::write(s.music.join("Broken/Damaged.flac"), b"fLaC this is not really a flac file at all").unwrap();
    library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let id = s.ok("search3", &[("query", "Damaged")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let r = s.http.get(s.url("stream", &[("id", &id), ("format", "mp3")])).send().await.unwrap();
    assert_ne!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["subsonic-response"]["status"], "failed");
    // a good one still streams
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    // started part way in a format it can't make: still from that point (mp3), not the whole file from the start
    let r = s.http.get(s.url("stream", &[("id", &one), ("format", "wav"), ("timeOffset", "1")])).send().await.unwrap();
    assert_eq!(r.headers()["x-transcoded"], "mp3@320k");
    let r = s.http.get(s.url("stream", &[("id", &one), ("format", "mp3")])).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.bytes().await.unwrap().len() > 1000);
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
    contract::observe("pairStart", &start["subsonic-response"]);
    let pair = &start["subsonic-response"]["pair"];
    let (code, token) = (pair["code"].as_str().unwrap().to_string(), pair["token"].as_str().unwrap().to_string());
    assert_eq!(code.len(), 6);
    let w: Value = s.http.get(open("pairPoll", &format!("&token={token}&wait=0"))).send().await.unwrap().json().await.unwrap();
    contract::observe("pairPoll", &w["subsonic-response"]);
    assert_eq!(w["subsonic-response"]["pair"]["status"], "waiting");
    let wrong = serde_json::json!({"server": "https://x", "user": "someone-else", "pass": "p"}).to_string();
    assert_eq!(s.call("pairApprove", &[("code", &code), ("login", &wrong)]).await["error"]["code"], 10);
    let login = serde_json::json!({"server": "https://music.example", "user": USER, "pass": PASS}).to_string();
    assert_eq!(s.ok("pairApprove", &[("code", &code), ("login", &login)]).await["pair"]["name"], "Laptop");
    let done: Value = s.http.get(open("pairPoll", &format!("&token={token}&wait=0"))).send().await.unwrap().json().await.unwrap();
    contract::observe("pairPoll", &done["subsonic-response"]);
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
    let app = conductor::jam::router(s.st.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
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
    // a Jam counts at most 1000 guests: made-up ones can't fill the memory
    {
        let mut g = s.st.live.lock();
        let j = g.jams.get_mut(&tok).unwrap();
        for i in 0..999 {
            j.guests.insert(format!("made-up-{i}"), (String::new(), conductor::db::now_ms()));
        }
    }
    let st: Value = s.http.get(format!("{jbase}/jam/{tok}/state?g=one-more")).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["guests"], 1000);
    s.st.live.lock().jams.get_mut(&tok).unwrap().guests.retain(|k, _| k == "guest1");
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
    // the state is limited too (generously: the page asks every 2.5 s, and a party may share one address)
    let mut n = 0;
    while s.http.get(format!("{jbase}/jam/{tok}/state")).send().await.unwrap().status() == 200 {
        n += 1;
        assert!(n <= 600, "no limit on the Jam's state");
    }
    assert!(n >= 590, "limited after {n}");
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
    // the test's podcast server is on this computer
    let s = Arc::new(
        Server::start_with(|c| {
            c.catalog = false;
            c.podcast_local = true;
        })
        .await,
    );
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
    tokio::spawn(conductor::api::podcasts::worker(s.st.clone()));

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

#[tokio::test]
async fn podcast_episodes_with_the_same_name_keep_their_own_files() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.podcast_local = true;
        c.podcast_keep = 0;
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    tone(&dir.path().join("a.mp3"), "A", "Host", "Show", 1, 2);
    tone(&dir.path().join("b.mp3"), "B", "Host", "Show", 2, 4);
    let (a, b) = (std::fs::read(dir.path().join("a.mp3")).unwrap(), std::fs::read(dir.path().join("b.mp3")).unwrap());
    let fl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fbase = format!("http://{}", fl.local_addr().unwrap());
    // two episodes on one day with one title (a daily show's "News")
    let feed = format!(
        r#"<?xml version="1.0"?><rss><channel><title>Daily</title>
        <item><title>News</title><guid>morning</guid><pubDate>Wed, 11 Nov 2026 08:00:00 +0000</pubDate><enclosure url="{fbase}/a.mp3" type="audio/mpeg"/></item>
        <item><title>News</title><guid>evening</guid><pubDate>Wed, 11 Nov 2026 18:00:00 +0000</pubDate><enclosure url="{fbase}/b.mp3" type="audio/mpeg"/></item>
        </channel></rss>"#
    );
    let app = axum::Router::new()
        .route("/feed.xml", axum::routing::get(move || { let f = feed.clone(); async move { ([("content-type", "application/rss+xml")], f) } }))
        .nest_service("/a.mp3", tower_http::services::ServeFile::new(dir.path().join("a.mp3")))
        .nest_service("/b.mp3", tower_http::services::ServeFile::new(dir.path().join("b.mp3")));
    tokio::spawn(async move { axum::serve(fl, app).await.unwrap() });
    tokio::spawn(conductor::api::podcasts::worker(s.st.clone()));
    s.ok("createPodcastChannel", &[("url", &format!("{fbase}/feed.xml"))]).await;
    let eps = s.ok("getPodcasts", &[]).await["podcasts"]["channel"][0]["episode"].as_array().unwrap().clone();
    let ids: Vec<String> = eps.iter().map(|e| e["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids.len(), 2);
    for id in &ids {
        s.ok("downloadPodcastEpisode", &[("id", id)]).await;
    }
    wait_for(
        async || {
            let mut all = true;
            for id in &ids {
                all &= s.ok("getPodcastEpisode", &[("id", id)]).await["podcastEpisode"]["downloaded"] == true;
            }
            all
        },
        "both episodes saved",
    )
    .await;
    let body = async |id: &str| s.http.get(s.url("stream", &[("id", id)])).send().await.unwrap().bytes().await.unwrap().to_vec();
    let (first, second) = (body(&ids[0]).await, body(&ids[1]).await);
    assert_ne!(first, second, "each has its own file");
    assert!([&a, &b].contains(&&first) && [&a, &b].contains(&&second));
    // deleting one leaves the other's file
    s.ok("deletePodcastEpisode", &[("id", &ids[0])]).await;
    assert_eq!(body(&ids[1]).await, second);
    let files: Vec<Option<String>> = sqlx::query_scalar("SELECT file FROM episode").fetch_all(&s.st.db).await.unwrap();
    let left = files.into_iter().flatten().next().unwrap();
    assert!(conductor::api::podcasts::folder(&s.st).join(left).is_file());
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
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));

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
    // (and the catalog's lists, for the API contract)
    s.ok("getDiscoveries", &[]).await;
    s.ok("getSongDiscoveries", &[("id", "dz-t-900")]).await;
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

#[tokio::test]
async fn a_download_never_replaces_a_file_you_have() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|_| {}).await;
    // another song already where the download would be filed
    let there = s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer.flac");
    tone(&there, "Heaven", "Talking Heads", "Fear of Music", 7, 2);
    let before = std::fs::read(&there).unwrap();
    library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    s.ok("star", &[("id", "dz-t-900")]).await;
    let mut local = None;
    for _ in 0..100 {
        let r = s.ok("lookupSongs", &[("id", "dz-t-900")]).await;
        if r["lookupSongs"]["song"][0]["status"] == "ready" {
            local = r["lookupSongs"]["song"][0]["songId"].as_str().map(String::from);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let local = local.expect("the song never arrived");
    assert_eq!(std::fs::read(&there).unwrap(), before, "the song you had is still there, unchanged");
    assert!(s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer (2).flac").is_file());
    assert_eq!(s.ok("getSong", &[("id", &local)]).await["song"]["title"], "Psycho Killer");
    let heaven = s.ok("search3", &[("query", "Heaven")]).await;
    assert_eq!(heaven["searchResult3"]["song"][0]["title"], "Heaven");
    // nothing half-copied left beside them
    let names: Vec<String> = std::fs::read_dir(s.music.join("Songs/Talking Heads")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names.len(), 2, "{names:?}");
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
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
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
async fn a_trial_song_someone_else_hearted_is_kept() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|_| {}).await;
    auth::create_user(&s.st.db, &s.st.secrets, "ann", "ann pass 1", false).await.unwrap();
    s.ok("lookupSongs", &[("id", "dz-t-900"), ("fetch", "true")]).await;
    let mut local = String::new();
    wait_for(
        async || {
            let r = s.ok("lookupSongs", &[("id", "dz-t-900")]).await;
            local = r["lookupSongs"]["song"][0]["songId"].as_str().unwrap_or("").to_string();
            !local.is_empty()
        },
        "the trial song",
    )
    .await;
    // Ann hearts it, not the one who played it
    let r: Value = s
        .http
        .get(format!("{}/rest/star.view?u=ann&p=ann%20pass%201&v=1.16.1&c=test&f=json&id={local}", s.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["subsonic-response"]["status"], "ok", "{r}");
    wait_for(async || s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer.flac").is_file(), "the song to be kept").await;
}

#[tokio::test]
async fn a_sync_doesnt_refile_or_search_again_soon() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let me = s.st_user().await;
    // asked for by someone (filed in Songs), and a song Soulseek didn't have two hours ago
    for (key, status) in [("dz-t-5", "retry"), ("dz-t-6", "notfound")] {
        sqlx::query("INSERT INTO request (key, title, artists_json, status, last_search, asked_at) VALUES (?, 'Five', '[\"Alpha\"]', ?, ?, 1)")
            .bind(key)
            .bind(status)
            .bind(conductor::db::now() - 7200)
            .execute(&s.st.db)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO request_want (key, user_id, star) VALUES ('dz-t-5', 'someone', 1)").execute(&s.st.db).await.unwrap();
    let row = async |key: &str| -> (String, String) {
        sqlx::query_as("SELECT folder, status FROM request WHERE key = ?").bind(key).fetch_one(&s.st.db).await.unwrap()
    };
    // a discovery playlist wants them too: they stay where they're filed, and nothing is searched again yet
    for key in ["dz-t-5", "dz-t-6"] {
        assert!(conductor::catalog::ask_synced(&s.st, key, &me, false, Some("pl-x"), Some("Discovery")).await.unwrap());
    }
    assert_eq!(row("dz-t-5").await, ("Songs".into(), "retry".into()));
    assert_eq!(row("dz-t-6").await, ("Songs".into(), "notfound".into()));
    assert!(conductor::catalog::ask_synced(&s.st, "dz-t-6", &me, true, None, None).await.unwrap());
    assert_eq!(row("dz-t-6").await.1, "notfound", "a Spotify sync doesn't either");
    // someone asking for it now: looked for again
    assert!(conductor::catalog::ask(&s.st, "dz-t-6", &me, true, None, false).await.unwrap());
    assert_eq!(row("dz-t-6").await.1, "retry");
}

#[tokio::test]
async fn a_kept_trial_song_never_replaces_or_loses_a_file() {
    if !have_ffmpeg() {
        return;
    }
    let (s, _tmp) = trial_server(|_| {}).await;
    s.ok("lookupSongs", &[("id", "dz-t-900"), ("fetch", "true")]).await;
    let mut local = String::new();
    wait_for(
        async || {
            let r = s.ok("lookupSongs", &[("id", "dz-t-900")]).await;
            local = r["lookupSongs"]["song"][0]["songId"].as_str().unwrap_or("").to_string();
            !local.is_empty()
        },
        "the trial song",
    )
    .await;
    // meanwhile another song you have sits where the kept one would go in Songs/
    let there = s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer.flac");
    tone(&there, "Heaven", "Talking Heads", "Fear of Music", 7, 2);
    let before = std::fs::read(&there).unwrap();
    library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    s.ok("star", &[("id", &local)]).await;
    let kept = s.music.join("Songs/Talking Heads/Talking Heads - Psycho Killer (2).flac");
    wait_for(async || kept.is_file(), "the kept song to move beside it").await;
    assert_eq!(std::fs::read(&there).unwrap(), before, "the other song is untouched");
    wait_for(
        async || s.ok("getSong", &[("id", &local)]).await["song"]["path"] == "Songs/Talking Heads/Talking Heads - Psycho Killer (2).flac",
        "the kept song's new place",
    )
    .await;
    assert!(s.ok("getSong", &[("id", &local)]).await["song"]["starred"].is_string());
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
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
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
    let saved = conductor::online::covers_once(&s.st).await.unwrap();
    assert_eq!(saved, 1);
    assert!(s.music.join("Albums/Gamma/Third/cover.jpg").is_file());
    assert!(!s.music.join("The Beta Band/Second/cover.jpg").exists());
    assert_eq!(conductor::online::covers_once(&s.st).await.unwrap(), 0); // never twice
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
    // wrong passwords are limited here as on /rest: guessing gets nowhere
    let guess = format!("{}/rest/audiomuse/clap/top_queries?u={USER}&p=guess", s.base);
    for _ in 0..auth::APP_TRIES {
        assert_eq!(s.http.get(&guess).send().await.unwrap().status(), 401);
    }
    assert_eq!(s.http.get(&guess).send().await.unwrap().status(), 429);
    assert_eq!(s.http.get(s.url("audiomuse/clap/top_queries", &[])).send().await.unwrap().status(), 429);
    // not set up: "not available", so apps fall back (and no login is tried)
    let s2 = Server::start().await;
    assert_eq!(s2.call("getSonicSimilarTracks", &[("id", "x")]).await["error"]["code"], 70);
    assert_eq!(s2.http.get(format!("{}/rest/audiomuse/clap/top_queries?u=x&p=y", s2.base)).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn admin_page() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let page = http.get(format!("{}/admin", s.base)).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("<title>Podium · Conductor</title>") && page.contains("href=\"/favicon.svg\""));
    let icon = http.get(format!("{}/favicon.svg", s.base)).send().await.unwrap();
    assert_eq!(icon.headers()["content-type"], "image/svg+xml");
    assert!(icon.text().await.unwrap().contains("<svg"));
    let api = |p: &str| format!("{}/admin/api/{p}", s.base);
    assert_eq!(http.get(api("status")).send().await.unwrap().status(), 401);
    // sign-in needs the header (other sites can't send it), and the right password
    let no_header = http.post(api("login")).json(&serde_json::json!({"username": USER, "password": PASS})).send().await.unwrap();
    assert_eq!(no_header.status(), 403);
    let wrong = http
        .post(api("login"))
        .header("X-Conductor", "1")
        .json(&serde_json::json!({"username": USER, "password": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);
    let ok = http
        .post(api("login"))
        .header("X-Conductor", "1")
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
        .header("X-Conductor", "1")
        .json(&serde_json::json!({"name": "guest", "password": "secret1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let users: Value = http.get(api("users")).send().await.unwrap().json().await.unwrap();
    assert_eq!(users.as_array().unwrap().len(), 2);
    let k: Value = http.post(api("users/guest/apikey")).header("X-Conductor", "1").send().await.unwrap().json().await.unwrap();
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
        .header("X-Conductor", "1")
        .json(&serde_json::json!({"username": "guest", "password": "secret1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(http.post(api("scan")).header("X-Conductor", "1").send().await.unwrap().status(), 200);
    http.post(api("logout")).header("X-Conductor", "1").send().await.unwrap();
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
    let jams = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let listens = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lb = format!("http://{}", l.local_addr().unwrap());
    let (w2, j2, l2) = (week.clone(), jams.clone(), listens.clone());
    let app = axum::Router::new().fallback(move |req: Request| {
        let (week, jams, listens) = (w2.clone(), j2.clone(), l2.clone());
        async move {
            let w = week.load(std::sync::atomic::Ordering::SeqCst);
            let j = jams.load(std::sync::atomic::Ordering::SeqCst);
            let path = req.uri().path().to_string();
            let body = match path.as_str() {
                "/1/user/lbuser/playlists/createdfor" => {
                    let mut lists = vec![json!({ "playlist": { "title": format!("Weekly Exploration for lbuser, week {w}"), "identifier": format!("https://listenbrainz.org/playlist/pl-{w}"), "date": format!("2026-11-{:02}T00:00:00", 10 + w) } })];
                    if j > 0 {
                        lists.push(json!({ "playlist": { "title": "Weekly Jams for lbuser", "identifier": format!("https://listenbrainz.org/playlist/jams-{j}"), "date": "2026-11-01T00:00:00" } }));
                    }
                    json!({ "playlists": lists })
                }
                "/1/playlist/pl-1" => json!({ "playlist": { "track": [
                    { "title": "One", "creator": "Alpha", "album": "First Album" },
                    { "title": "Psycho Killer", "creator": "Talking Heads", "album": "Talking Heads: 77", "duration": 3000 }] } }),
                "/1/playlist/pl-2" => json!({ "playlist": { "track": [{ "title": "Two", "creator": "Alpha" }] } }),
                "/1/playlist/jams-1" => json!({ "playlist": { "track": [{ "title": "Two", "creator": "Alpha" }] } }),
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
        c.discovery = vec!["weekly-exploration".into(), "weekly-jams".into()];
    })
    .await;
    tokio::spawn(conductor::listenbrainz::worker(s.st.clone()));

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
    // both in it as songs of yours: until the download is scanned in, it's listed as on its way ("⏳ …", its ⬇ id),
    // and two entries alone don't mean it has arrived
    wait_for(
        async || {
            s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"]
                .as_array()
                .is_some_and(|e| e.len() == 2 && e.iter().all(|x| !x["id"].as_str().unwrap_or("dz-").starts_with("dz-")))
        },
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

    // another discovery playlist's new list doesn't delete what this one still has
    jams.store(1, std::sync::atomic::Ordering::SeqCst);
    wait_for(
        async || {
            let r = s.ok("getPlaylists", &[]).await;
            let Some(p) = r["playlists"]["playlist"].as_array().unwrap().iter().find(|p| p["name"] == "Weekly Jams").cloned() else {
                return false;
            };
            let e = s.ok("getPlaylist", &[("id", p["id"].as_str().unwrap())]).await["playlist"]["entry"].clone();
            e.as_array().is_some_and(|e| e.len() == 1 && e[0]["title"] == "Two")
        },
        "Weekly Jams",
    )
    .await;
    assert!(file.is_file(), "Weekly Exploration still has it");
    assert_eq!(s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"].as_array().map(|e| e.len()), Some(2));

    // week 2: a new list; last week's song nobody kept is deleted
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
    let broken = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let b2 = broken.clone();
    let app = axum::Router::new().fallback(move |req: Request| {
        let broken = b2.load(std::sync::atomic::Ordering::SeqCst);
        async move {
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
                "/v1/playlists/car/items" if !broken => json!({ "items": [{ "item": t("sp-two", "Zwei (renamed on Spotify)", "Alpha", None) }], "next": null }),
                _ => return (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            };
            axum::Json(body).into_response()
        }
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
    assert_eq!(conductor::spotify::import_from(&s.st.db, dir.path(), Some(lib)).await.unwrap(), (true, 1));

    let (lists, have, asked) = conductor::spotify::sync_once(&s.st).await.unwrap();
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
    let (_, have, asked) = conductor::spotify::sync_once(&s.st).await.unwrap();
    assert_eq!((have, asked), (3, 0));
    let order: Vec<String> = s.ok("getPlaylist", &[("id", &liked)]).await["playlist"]["entry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["title"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(order, ["Psycho Killer", "One"]);
    // Spotify can't give a playlist's songs: it's left as it is, not emptied
    broken.store(true, std::sync::atomic::Ordering::SeqCst);
    conductor::spotify::sync_once(&s.st).await.unwrap();
    assert_eq!(s.ok("getPlaylist", &[("id", &car)]).await["playlist"]["entry"][0]["title"], "Two");
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
    let now = conductor::db::now();
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
    let app = conductor::jam::router(s.st.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>();
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
    let st: Value = s.http.get(format!("{jbase}/state?g=g-ann")).send().await.unwrap().json().await.unwrap();
    let songs = st["song"].as_array().unwrap();
    assert_eq!(songs.len(), 2);
    assert_eq!(songs[1]["by"], "Ann");
    // each friend sees which songs are theirs, never the others' guest ids (they'd let anyone take those out)
    assert_eq!((songs[0]["mine"].as_bool(), songs[1]["mine"].as_bool()), (Some(false), Some(true)));
    assert!(songs.iter().all(|x| x.get("guest").is_none()), "{st}");
    let st: Value = s.http.get(format!("{jbase}/state")).send().await.unwrap().json().await.unwrap();
    assert!(st["song"].as_array().unwrap().iter().all(|x| x["mine"] == false));
    let r = s.ok("playlistShares", &[]).await;
    assert_eq!(r["playlistShares"]["share"][0]["added"].as_array().unwrap().len(), 1);
    // the owner takes Ann's song out in the app, then Bob adds it again: it's Bob's now, not Ann's
    s.ok("updatePlaylist", &[("playlistId", &pid), ("songIndexToRemove", "1")]).await;
    assert_eq!(post("add", serde_json::json!({"id": three, "by": "Bob", "guest": "g-bob"})).await.status(), 200);
    let st: Value = s.http.get(format!("{jbase}/state")).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["song"][1]["by"], "Bob");
    assert_eq!(post("remove", serde_json::json!({"id": three, "index": 1, "guest": "g-ann"})).await.status(), 403);
    assert_eq!(post("remove", serde_json::json!({"id": three, "index": 1, "guest": "g-bob"})).await.status(), 200);
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
async fn deezer_failures_arent_kept() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
    let up = Arc::new(AtomicBool::new(false));
    let u2 = up.clone();
    let (dz, _) = stand_in(move |path, _q, _| match path {
        _ if !u2.load(SeqCst) => None, // Deezer is down: an error page
        "/search/artist" => Some(json!({ "data": [{ "id": 8, "name": "Alpha", "nb_fan": 50 }] })),
        "/artist/8/related" => Some(json!({ "data": [] })),
        _ => Some(json!({ "error": { "code": 800, "message": "no data" } })),
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = true;
        c.deezer_url = dz.clone();
    })
    .await;
    assert_eq!(conductor::catalog::artist_dz_id(&s.st, "Alpha").await, None);
    assert!(conductor::catalog::artist_info(&s.st, "Alpha").await.is_none());
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_cache WHERE kind IN ('artist', 'artist_info')")
        .fetch_one(&s.st.db)
        .await
        .unwrap();
    assert_eq!(kept, 0, "a failed call isn't kept as \"Deezer has nothing\"");
    // Deezer's own "nothing" is an answer, and kept
    up.store(true, SeqCst);
    assert_eq!(conductor::catalog::match_album(&s.st, "Nobody", "Nothing", &[]).await, None);
    let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM catalog_cache WHERE kind = 'album'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(kept, 1);
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
            contract::observe("identifySong", &v["subsonic-response"]);
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
            conductor::mixes::tick(&s.st).await;
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
    conductor::catalog::trial::tick(&s.st).await.unwrap();
    let on_trial = conductor::catalog::trial::playlist_id(&s.st, &s.st_user().await, false).await.unwrap();
    if let Some(p) = on_trial {
        let pl = s.ok("getPlaylist", &[("id", &p)]).await;
        assert!(pl["playlist"]["entry"].as_array().is_none_or(|e| e.iter().all(|x| x["id"] != pk.as_str())));
    }
    let still: String = sqlx::query_scalar("SELECT rel FROM request WHERE key = 'dz-t-900'").fetch_one(&s.st.db).await.unwrap();
    assert!(still.starts_with("Trial/"));
    // heart it: kept (moved to Songs/)
    s.ok("star", &[("id", &pk)]).await;
    conductor::catalog::trial::tick(&s.st).await.unwrap();
    let kept: String = sqlx::query_scalar("SELECT rel FROM request WHERE key = 'dz-t-900'").fetch_one(&s.st.db).await.unwrap();
    assert!(kept.starts_with("Songs/"), "{kept}");
    // a song that hasn't arrived is let go when the next mix comes
    conductor::catalog::let_go_of_mix_song(&s.st, "dz-t-901", radar.as_deref().unwrap()).await.unwrap();
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
    // Deezer's related artist you have first, then Conductor's own picks; only artists you have
    assert!(sim.iter().all(|a| !a["id"].as_str().unwrap().starts_with("dz-")), "only artists you have: {sim:?}");
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
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
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
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
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
    let rep = conductor::import_nd::music_lookup(&s.st.db, data.path(), Some(state.path())).await.unwrap();
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
    let pid = conductor::catalog::trial::playlist_id(&s.st, &uid, false).await.unwrap();
    assert_eq!(pid.as_deref(), Some(on_trial.as_str()));
    let wants: Vec<(String, String, bool, bool)> =
        sqlx::query_as("SELECT key, playlist_id, star, done FROM request_want ORDER BY key").fetch_all(&s.st.db).await.unwrap();
    assert_eq!(wants, [("dz-t-1".into(), "".into(), true, true), ("dz-t-3".into(), mine.clone(), false, false)]);
    // a second run leaves what's there alone
    let again = conductor::import_nd::music_lookup(&s.st.db, data.path(), Some(state.path())).await.unwrap();
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
    assert_eq!(http.post(api("login")).header("X-Conductor", "1").json(&login).send().await.unwrap().status(), 200);
    let get = async |p: &str| -> Value { http.get(api(p)).send().await.unwrap().json().await.unwrap() };
    let post = async |p: &str, b: Value| http.post(api(p)).header("X-Conductor", "1").json(&b).send().await.unwrap();
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
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
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
    // two songs Conductor downloaded earlier as MP3 from the right album
    tone(&s.music.join("Songs/Alpha/Alpha - Four.mp3"), "Four", "Alpha", "First Album", 4, 2);
    tone(&s.music.join("Songs/Alpha/Alpha - Five.mp3"), "Five", "Alpha", "First Album", 5, 2);
    // and a song of yours where Four's better copy would go
    let heaven = s.music.join("Songs/Alpha/Alpha - Four.flac");
    tone(&heaven, "Heaven", "Alpha", "Fear of Music", 7, 2);
    let heaven_was = std::fs::read(&heaven).unwrap();
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
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
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
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
    // Four: the FLAC is in (beside your song with its name, untouched), the MP3 gone, the same song id (and heart)
    assert!(s.music.join("Songs/Alpha/Alpha - Four (2).flac").is_file());
    assert_eq!(std::fs::read(&heaven).unwrap(), heaven_was, "your song is still there, unchanged");
    assert!(!s.music.join("Songs/Alpha/Alpha - Four.mp3").exists());
    let song = s.ok("getSong", &[("id", &four)]).await["song"].clone();
    assert_eq!((song["suffix"].as_str(), song["path"].as_str().map(|p| p.ends_with("Four (2).flac"))), (Some("flac"), Some(true)), "{song}");
    assert!(song["starred"].is_string());
    let rel: String = sqlx::query_scalar("SELECT rel FROM request WHERE key = 'dz-t-4'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(rel, "Songs/Alpha/Alpha - Four (2).flac");
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
    assert_eq!(s.call("djAudio", &[("id", "../../conductor.db")]).await["error"]["code"], 70);
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
async fn old_uploads_go_but_what_was_sent_elsewhere_stays() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let dir = s.st.cfg.data_dir.join("imports");
    std::fs::create_dir_all(&dir).unwrap();
    let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    let old = |name: &str| {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(two_days_ago).unwrap();
        p
    };
    let upload = old("0123456789abcdef0123456789abcdef.zip");
    let state = old("sent-httpnavidromelocal-me.json");
    let fresh = dir.join("fedcba9876543210fedcba9876543210.json");
    std::fs::write(&fresh, b"x").unwrap();
    conductor::import::jobs::tidy_uploads(&s.st).await;
    assert!(!upload.exists(), "an upload left behind for a day goes");
    assert!(fresh.exists(), "a new one stays");
    assert!(state.exists(), "import --to's record of what it sent stays (it's needed to run it again, and to undo)");
}

#[tokio::test]
async fn an_import_stopped_part_way_can_be_undone() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let uid = s.st_user().await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    // what an import had done when Conductor stopped (or it failed): a play and a heart
    for (id, status) in [("b-stopped", "running"), ("b-failed", "failed")] {
        sqlx::query("INSERT INTO import_batch (id, user_id, input_json, status, created_at) VALUES (?, ?, '{}', ?, 0)")
            .bind(id).bind(&uid).bind(status).execute(&s.st.db).await.unwrap();
        sqlx::query("INSERT INTO play (user_id, track_id, played_at, client) VALUES (?, ?, ?, ?)")
            .bind(&uid).bind(&one).bind(1_000_000).bind(format!("import:{id}")).execute(&s.st.db).await.unwrap();
    }
    sqlx::query("INSERT INTO annotation (user_id, item_kind, item_id, starred_at) VALUES (?, 'track', ?, 5)").bind(&uid).bind(&one).execute(&s.st.db).await.unwrap();
    sqlx::query("INSERT INTO import_change (batch_id, kind, item) VALUES ('b-stopped', 'star', ?)").bind(&one).execute(&s.st.db).await.unwrap();
    let plays = || sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM play WHERE client LIKE 'import:%'").fetch_one(&s.st.db);
    // the next start: the one left running is failed, and both can be taken back
    conductor::import::jobs::recover(&s.st).await;
    assert_eq!(s.ok("getImport", &[("id", "b-stopped")]).await["import"]["status"], "failed");
    assert_eq!(s.ok("undoImport", &[("id", "b-stopped")]).await["import"]["status"], "undone");
    assert_eq!(plays().await.unwrap(), 1);
    assert!(s.ok("getSong", &[("id", &one)]).await["song"]["starred"].is_null(), "its heart is taken back");
    // forgetting a failed one takes back what it changed too
    s.ok("deleteImport", &[("id", "b-failed")]).await;
    assert_eq!(plays().await.unwrap(), 0);
}

#[tokio::test]
async fn undo_keeps_what_others_want_and_skips_deleted_playlists() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let uid = s.st_user().await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let run = |q: &'static str| sqlx::query(q);
    sqlx::query("INSERT INTO import_batch (id, user_id, input_json, status, created_at) VALUES ('b1', ?, '{}', 'done', 0)").bind(&uid).execute(&s.st.db).await.unwrap();
    sqlx::query("INSERT INTO play (user_id, track_id, played_at, client) VALUES (?, ?, 1000000, 'import:b1')").bind(&uid).bind(&one).execute(&s.st.db).await.unwrap();
    sqlx::query("INSERT INTO annotation (user_id, item_kind, item_id, starred_at) VALUES (?, 'track', ?, 5)").bind(&uid).bind(&one).execute(&s.st.db).await.unwrap();
    // it hearted a song, filled a playlist deleted since, and asked for two songs (someone else wants one of them now)
    sqlx::query("INSERT INTO import_change (batch_id, kind, item) VALUES ('b1', 'star', ?)").bind(&one).execute(&s.st.db).await.unwrap();
    run("INSERT INTO import_change (batch_id, kind, item, before) VALUES ('b1', 'playlist_songs', 'gone-pl', '[\"x\"]')").execute(&s.st.db).await.unwrap();
    for key in ["dz-t-5", "dz-t-6"] {
        sqlx::query("INSERT INTO request (key, title, artists_json, asked_at) VALUES (?, 'Five', '[\"Alpha\"]', 1)").bind(key).execute(&s.st.db).await.unwrap();
        sqlx::query("INSERT INTO request_want (key, user_id, star) VALUES (?, ?, 1)").bind(key).bind(&uid).execute(&s.st.db).await.unwrap();
        sqlx::query("INSERT INTO import_change (batch_id, kind, item) VALUES ('b1', 'request', ?)").bind(key).execute(&s.st.db).await.unwrap();
    }
    run("INSERT INTO request_want (key, user_id, star) VALUES ('dz-t-5', 'ann', 1)").execute(&s.st.db).await.unwrap();
    assert_eq!(s.ok("undoImport", &[("id", "b1")]).await["import"]["status"], "undone");
    let plays: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM play").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(plays, 0);
    assert!(s.ok("getSong", &[("id", &one)]).await["song"]["starred"].is_null(), "its heart is taken back");
    let left: Vec<(String, String)> = sqlx::query_as("SELECT r.key, w.user_id FROM request r JOIN request_want w ON w.key = r.key")
        .fetch_all(&s.st.db)
        .await
        .unwrap();
    assert_eq!(left, [("dz-t-5".to_string(), "ann".to_string())], "Ann's song stays asked for; only the import's want goes");
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
    let parsed = conductor::import::parse_path(&dir.path().join("Library.xml")).unwrap();
    // this test's server stands in for any Subsonic server (Navidrome before a switch)
    let server = conductor::import::subsonic::Server::new(&s.base, USER, PASS);
    let library = server.library().await.unwrap();
    assert_eq!(library.len(), 3);
    let plan = conductor::import::apply::plan(&parsed, &library, &Default::default(), &Default::default());
    let state_path = dir.path().join("sent.json");
    let mut state = conductor::import::subsonic::State::default();
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
    let mut state = conductor::import::subsonic::State::load(&state_path);
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
    assert_eq!(conductor::import::online::podcast_feed(&format!("{base}/itunes"), "Song Exploder", "").await.as_deref(), Some("http://feeds.example/se"));
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
async fn online_imports_keep_keys_and_spotify_to_themselves() {
    if !have_ffmpeg() {
        return;
    }
    // Last.fm can't be reached: the error doesn't show the address with the key in it
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let lfm = format!("http://{}/lfm", l.local_addr().unwrap());
    drop(l);
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.lastfm_url = lfm.clone();
        c.lastfm_api_key = Some("secret-lfm-key-123".into());
    })
    .await;
    let id = s.ok("importOnline", &[("source", "lastfm"), ("account", "bob")]).await["import"]["id"].as_str().unwrap().to_string();
    let mut error = String::new();
    for _ in 0..400 {
        let b = s.ok("getImport", &[("id", &id)]).await["import"].clone();
        if b["status"] == "failed" {
            error = b["error"].as_str().unwrap_or("").to_string();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(!error.is_empty(), "no failure");
    assert!(!error.contains("secret-lfm-key-123") && !error.contains("api_key"), "{error}");
    // the server's Spotify login is the admin's: someone else can't import from it
    auth::create_user(&s.st.db, &s.st.secrets, "ann", "ann pass 1", false).await.unwrap();
    let r: Value = s
        .http
        .get(format!("{}/rest/importOnline.view?u=ann&p=ann%20pass%201&v=1.16.1&c=test&f=json&source=spotify", s.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["subsonic-response"]["error"]["code"], 50, "{r}");
    let r: Value = s
        .http
        .get(format!("{}/rest/importOnline.view?u=ann&p=ann%20pass%201&v=1.16.1&c=test&f=json&source=deezer&account=42", s.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["subsonic-response"]["status"], "ok", "other sources are for everyone: {r}");
    s.ok("importOnline", &[("source", "spotify")]).await;
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
async fn kill_switch_pauses_downloads_while_the_vpn_is_down() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    let up = Arc::new(AtomicBool::new(false));
    let searches = Arc::new(AtomicUsize::new(0));
    let (u2, s2) = (up.clone(), searches.clone());
    let (base, _) = stand_in(move |path, _q, body| {
        Some(match path {
            "/check" if u2.load(SeqCst) => json!({ "public_ip": "10.64.0.2", "country": "Netherlands" }),
            "/check" => return None, // the tunnel's control server doesn't answer
            "/home" => json!({ "ip": "198.51.100.7" }),
            "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
            "/api/v0/searches" if !body.is_empty() => {
                s2.fetch_add(1, SeqCst);
                json!({})
            }
            p if p.ends_with("/responses") => json!([]),
            p if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
            "/api/v0/transfers/downloads" => json!([]),
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.slskd_fast_test = true;
        c.vpn = "existing".into();
        c.vpn_check_url = Some(format!("{base}/check"));
        c.vpn_ip_url = format!("{base}/home");
        c.downloads_require_vpn = true;
        c.vpn_check_secs = 1;
    })
    .await;
    sqlx::query("INSERT INTO request (key, title, artists_json, album, status, asked_at) VALUES ('dz-t-5', 'Five', '[\"Alpha\"]', '', 'retry', 1)")
        .execute(&s.st.db)
        .await
        .unwrap();
    let health = async || -> Value { s.http.get(format!("{}/health", s.base)).send().await.unwrap().json().await.unwrap() };
    // paused before the first check, and while it fails
    assert!(conductor::vpn::paused(&s.st));
    tokio::spawn(conductor::vpn::worker(s.st.clone()));
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    assert_eq!(searches.load(SeqCst), 0, "nothing searched while the VPN is down");
    let h = health().await;
    assert_eq!((h["vpn"]["up"].as_bool(), h["vpn"]["downloadsPaused"].as_bool()), (Some(false), Some(true)), "{h}");
    // up again: downloads go on
    up.store(true, SeqCst);
    wait_for(async || searches.load(SeqCst) > 0, "a search once the VPN is up").await;
    let h = health().await;
    assert_eq!((h["vpn"]["up"].as_bool(), h["vpn"]["country"].as_str(), h["vpn"]["downloadsPaused"].as_bool()), (Some(true), Some("Netherlands"), Some(false)));
    assert!(!h.to_string().contains("10.64.0.2") && !h.to_string().contains("198.51.100.7"), "addresses are never shown");
    // down again: paused again
    up.store(false, SeqCst);
    wait_for(async || conductor::vpn::paused(&s.st), "the pause").await;
}

#[tokio::test]
async fn kill_switch_stops_a_round_when_the_vpn_drops() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    let state: Arc<std::sync::OnceLock<Arc<AppState>>> = Arc::default();
    let (searches, queued) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (st2, s2, q2) = (state.clone(), searches.clone(), queued.clone());
    let (base, _) = stand_in(move |path, _q, body| {
        Some(match path {
            "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
            "/api/v0/searches" if !body.is_empty() => {
                s2.fetch_add(1, SeqCst);
                // the tunnel drops while the first search runs
                st2.get().unwrap().vpn.write().unwrap().up = Some(false);
                json!({})
            }
            p if p.ends_with("/responses") => json!([{ "username": "peer", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
                "files": [{ "filename": r"Music\Alpha\Album\01 Five.flac", "size": 1, "bitRate": 0, "extension": "flac" }] }]),
            p if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
            p if p.starts_with("/api/v0/transfers/downloads/") && !body.is_empty() => {
                q2.fetch_add(1, SeqCst);
                json!({})
            }
            "/api/v0/transfers/downloads" => json!([]),
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.slskd_fast_test = true;
        c.downloads_require_vpn = true;
    })
    .await;
    state.set(s.st.clone()).ok();
    s.st.vpn.write().unwrap().up = Some(true);
    for key in ["dz-t-5", "dz-t-6"] {
        sqlx::query("INSERT INTO request (key, title, artists_json, album, status, asked_at) VALUES (?, 'Five', '[\"Alpha\"]', 'Album', 'retry', 1)")
            .bind(key)
            .execute(&s.st.db)
            .await
            .unwrap();
    }
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
    wait_for(async || searches.load(SeqCst) > 0, "the first search").await;
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    assert_eq!((searches.load(SeqCst), queued.load(SeqCst)), (1, 0), "nothing more is searched or fetched once the VPN is down");
}

#[tokio::test]
async fn a_filed_song_whose_file_is_gone_stops_the_scans() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let (base, _) = stand_in(|path, _q, _| {
        Some(match path {
            "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
            "/api/v0/transfers/downloads" => json!([]),
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.slskd_fast_test = true;
    })
    .await;
    // filed, then the file went before it was scanned in
    sqlx::query("INSERT INTO request (key, title, artists_json, status, rel, arrived_at, asked_at) VALUES ('dz-t-5', 'Five', '[\"Alpha\"]', 'done', 'Songs/Alpha/Alpha - Five.flac', 1, 1)")
        .execute(&s.st.db)
        .await
        .unwrap();
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
    wait_for(
        async || sqlx::query_scalar::<_, String>("SELECT status FROM request WHERE key = 'dz-t-5'").fetch_one(&s.st.db).await.unwrap() == "notfound",
        "the song to be given up on",
    )
    .await;
    let generation = || s.st.catalog.generation.load(std::sync::atomic::Ordering::SeqCst);
    let before = generation();
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    assert_eq!(generation(), before, "no more scans for it");
}

#[tokio::test]
async fn a_filed_song_waits_for_a_long_scan() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let (base, _) = stand_in(|path, _q, _| {
        Some(match path {
            "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
            "/api/v0/transfers/downloads" => json!([]),
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.slskd_fast_test = true;
    })
    .await;
    // filed long ago, its file there, while a scan of the whole library (hours, on a slow drive) is running: the
    // song isn't given up on because that scan hasn't reached it yet
    let rel = "Songs/Alpha/Alpha - Six.flac";
    std::fs::create_dir_all(s.music.join("Songs/Alpha")).unwrap();
    std::fs::write(s.music.join(rel), b"not really a song").unwrap();
    sqlx::query("INSERT INTO request (key, title, artists_json, status, rel, arrived_at, asked_at) VALUES ('dz-t-6', 'Six', '[\"Alpha\"]', 'done', ?, 1, 1)")
        .bind(rel)
        .execute(&s.st.db)
        .await
        .unwrap();
    s.st.scan.scanning.store(true, std::sync::atomic::Ordering::SeqCst);
    tokio::spawn(conductor::catalog::acquire::worker(s.st.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    let status: String = sqlx::query_scalar("SELECT status FROM request WHERE key = 'dz-t-6'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(status, "done", "still waiting for the scan");
    s.st.scan.scanning.store(false, std::sync::atomic::Ordering::SeqCst);
}

#[tokio::test]
async fn proxy_carries_the_download_traffic() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    // a stand-in proxy: whatever it's asked for, it answers (and counts)
    let (proxy, through) = stand_in(|path, _q, _| {
        Some(match path {
            "/search" => json!({ "data": [{ "id": 77, "title": "Proxied Song", "duration": 3, "artist": { "id": 1, "name": "Via Proxy" },
                "album": { "id": 2, "title": "P" } }] }),
            "/ip" => json!({ "ip": "203.0.113.50", "country": "Sweden" }),
            _ => json!({ "data": [] }),
        })
    })
    .await;
    let (home, _) = stand_in(|path, _, _| (path == "/ip").then(|| json!({ "ip": "198.51.100.7" }))).await;
    let s = Server::start_with(|c| {
        c.catalog = true;
        c.deezer_url = "http://deezer.invalid".into(); // only reachable through the proxy
        c.proxy = Some(proxy.clone());
        c.vpn = "proxy".into();
        c.vpn_ip_url = format!("{home}/ip");
    })
    .await;
    let r = s.ok("search3", &[("query", "proxied")]).await;
    assert!(r.to_string().contains("Proxied Song"), "the catalog answered through the proxy: {r}");
    assert!(through.load(std::sync::atomic::Ordering::SeqCst) > 0);
    // the VPN check in proxy mode: the address seen through the proxy differs from this computer's
    let st = conductor::vpn::check(&s.st.cfg).await;
    assert_eq!((st.up, st.country.as_deref()), (Some(true), Some("Sweden")), "{st:?}");
    // a proxy that can't be read fails closed
    let mut bad = s.st.cfg.clone();
    bad.proxy = Some("not a proxy".into());
    assert_eq!(conductor::vpn::check(&bad).await.up, Some(false));
}

#[tokio::test]
async fn setup_page_only_before_anyone_has_an_account() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    let s = Server::start().await;
    let url = |p: &str| format!("{}{p}", s.base);
    let token = s.st.setup_token.clone();
    // someone has an account: closed, token or not
    assert_eq!(s.http.get(url("/setup")).send().await.unwrap().status(), 404);
    assert_eq!(s.http.get(url(&format!("/setup?token={token}"))).send().await.unwrap().status(), 404);
    // nobody yet: open from this computer
    sqlx::query("DELETE FROM user").execute(&s.st.db).await.unwrap();
    let page = s.http.get(url("/setup")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    assert!(page.text().await.unwrap().contains("Prelude"));
    let st: Value = s.http.get(url("/setup/api/status")).send().await.unwrap().json().await.unwrap();
    assert_eq!(st["libraries"][0]["connected"], true);
    // from this computer only in name too: not through a proxy here, not a web page pointed at 127.0.0.1
    let rebound = s.http.get(url("/setup")).header("Host", "evil.example:4533").send().await.unwrap();
    assert_eq!(rebound.status(), 404, "DNS rebinding: another site's name");
    let proxied = s.http.get(url("/setup")).header("X-Forwarded-For", "203.0.113.9").send().await.unwrap();
    assert_eq!(proxied.status(), 404, "passed on by a proxy on this computer");
    let rebound = s.http.post(url("/setup/api/admin")).header("Host", "evil.example").header("X-Conductor", "1")
        .json(&json!({ "name": "evil", "password": "longenough" })).send().await.unwrap();
    assert_eq!(rebound.status(), 404);
    let with_token = s.http.get(url(&format!("/setup?token={token}"))).header("X-Forwarded-For", "203.0.113.9").send().await.unwrap();
    assert_eq!(with_token.status(), 200, "the token still opens it");
    let body = json!({ "name": "first", "password": "longenough" });
    assert_eq!(s.http.post(url("/setup/api/admin")).json(&body).send().await.unwrap().status(), 403, "needs the header");
    let short = json!({ "name": "first", "password": "123" });
    assert_eq!(s.http.post(url("/setup/api/admin")).header("X-Conductor", "1").json(&short).send().await.unwrap().status(), 400);
    // several at once: one admin is made
    let tries = (0..8).map(|i| {
        let b = json!({ "name": format!("first{i}"), "password": "longenough" });
        s.http.post(url("/setup/api/admin")).header("X-Conductor", "1").json(&b).send()
    });
    let made = futures_util::future::join_all(tries).await.into_iter().filter(|r| r.as_ref().unwrap().status() == 200).count();
    assert_eq!(made, 1);
    // closed for good
    assert_eq!(s.http.get(url("/setup")).send().await.unwrap().status(), 404);
    let again = s.http.post(url("/setup/api/admin")).header("X-Conductor", "1").json(&json!({ "name": "second", "password": "longenough" })).send().await.unwrap();
    assert_eq!(again.status(), 404);
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user WHERE is_admin = 1").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(admins, 1);
}

#[tokio::test]
async fn slskd_follows_the_vpns_forwarded_port() {
    if !have_ffmpeg() {
        return;
    }
    use serde_json::json;
    use std::sync::Mutex;
    let written: Arc<Mutex<Option<String>>> = Arc::default();
    let reconnects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (w2, r2) = (written.clone(), reconnects.clone());
    let (base, _) = stand_in(move |path, _q, body| {
        Some(match path {
            "/v1/publicip/ip" => json!({ "public_ip": "10.64.0.2", "country": "Switzerland" }),
            "/home" => json!({ "ip": "198.51.100.7" }),
            "/v1/portforward" => json!({ "port": 41234 }),
            "/api/v0/options/yaml" if body.is_empty() => json!("soulseek:\n  username: me\n  listen_port: 50300\n"),
            "/api/v0/options/yaml" => {
                *w2.lock().unwrap() = serde_json::from_slice::<String>(body).ok();
                json!({})
            }
            "/api/v0/server" => {
                r2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                json!({})
            }
            _ => json!({}),
        })
    })
    .await;
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.slskd_url = Some(base.clone());
        c.slskd_api_key = Some("k".into());
        c.vpn = "gluetun".into();
        c.vpn_check_url = Some(format!("{base}/v1/publicip/ip"));
        c.vpn_check_key = Some("control-key".into());
        c.vpn_ip_url = format!("{base}/home");
        c.vpn_port_sync = true;
        c.vpn_check_secs = 1;
    })
    .await;
    tokio::spawn(conductor::vpn::worker(s.st.clone()));
    wait_for(async || written.lock().unwrap().is_some(), "slskd's configuration to be written").await;
    assert_eq!(written.lock().unwrap().as_deref(), Some("soulseek:\n  username: me\n  listen_port: 41234\n"));
    wait_for(async || reconnects.load(std::sync::atomic::Ordering::SeqCst) >= 2, "slskd to reconnect").await;
    wait_for(async || conductor::vpn::status(&s.st).forwarded_port == Some(41234), "the status").await;
}

/// A Navidrome database with just what the importer reads, and two users whose passwords a real Navidrome encrypted
/// (test-only passwords): one with its default key, one with its own key.
async fn navidrome_db(path: &Path) {
    let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(path).create_if_missing(true);
    let nd = sqlx::SqlitePool::connect_with(opts).await.unwrap();
    for sql in [
        "CREATE TABLE library (id INTEGER, name TEXT)",
        "INSERT INTO library VALUES (1, 'Music Library')",
        "CREATE TABLE user (id TEXT, user_name TEXT, is_admin BOOL, created_at TEXT, password TEXT)",
        "INSERT INTO user VALUES ('nd-u1', 'nd-default', 1, '2024-01-01 00:00:00', '51Sxz9molEEEoDcirGPBQkUe5GtQQ43ss2tnvqJ5WEXzYWXvGLcKbRfl1Rwo')",
        "INSERT INTO user VALUES ('nd-u2', 'nd-custom', 0, '2024-01-01 00:00:00', 'aKRq7AgwSj43+IBFsufgk+3D0bpQESEx8pqC+qdXjUnBK0WAd7Z+X0QbdbY=')",
        "CREATE TABLE media_file (id TEXT, path TEXT, library_id INTEGER, album_artist TEXT, album TEXT, album_id TEXT, created_at TEXT)",
        "CREATE TABLE album (id TEXT, name TEXT, album_artist TEXT, missing BOOL, song_count INTEGER, created_at TEXT)",
        "CREATE TABLE artist (id TEXT, name TEXT, missing BOOL)",
        "CREATE TABLE annotation (user_id TEXT, item_id TEXT, item_type TEXT, play_count INTEGER, play_date TEXT, rating INTEGER, starred BOOL, starred_at TEXT)",
        "CREATE TABLE scrobbles (media_file_id TEXT, user_id TEXT, submission_time INTEGER)",
        "CREATE TABLE playlist (id TEXT, name TEXT, comment TEXT, public BOOL, owner_id TEXT, rules TEXT, uploaded_image TEXT, created_at TEXT, updated_at TEXT)",
        "CREATE TABLE playlist_tracks (id INTEGER, playlist_id TEXT, media_file_id TEXT)",
        "CREATE TABLE playqueue (user_id TEXT, current TEXT, changed_by TEXT, items TEXT, position INTEGER, updated_at TEXT)",
        "CREATE TABLE radio (id TEXT, name TEXT, stream_url TEXT, home_page_url TEXT)",
    ] {
        sqlx::query(sql).execute(&nd).await.unwrap();
    }
    nd.close().await;
}

async fn signs_in(s: &Server, user: &str, pw: &str) -> (bool, bool) {
    let salt = "s4lt";
    let t = format!("{:x}", md5::compute(format!("{pw}{salt}")));
    let ping = async |q: String| -> bool {
        let r: Value = s.http.get(format!("{}/rest/ping?{q}&v=1.16.1&c=test&f=json", s.base)).send().await.unwrap().json().await.unwrap();
        r["subsonic-response"]["status"] == "ok"
    };
    let user = form_urlencoded::byte_serialize(user.as_bytes()).collect::<String>();
    let p = form_urlencoded::byte_serialize(pw.as_bytes()).collect::<String>();
    (ping(format!("u={user}&t={t}&s={salt}")).await, ping(format!("u={user}&p={p}")).await)
}

#[tokio::test]
async fn navidrome_passwords_come_over() {
    if !have_ffmpeg() {
        return;
    }
    for (key, carried, reset) in [(None, "nd-default", "nd-custom"), (Some("test-only-custom-key"), "nd-custom", "nd-default")] {
        let s = Server::start().await;
        let tmp = tempfile::tempdir().unwrap();
        let nd = tmp.path().join("navidrome.db");
        navidrome_db(&nd).await;
        let rep = conductor::import_nd::run(&s.st.cfg, &s.st.db, &s.st.secrets, &s.st.libs, &nd, None, key, false).await.unwrap();
        assert_eq!((rep.users_imported, rep.passwords_carried), (2, 1));
        assert_eq!(rep.users, [reset.to_string()], "the other one is to be set again");
        let shown = serde_json::to_string(&rep).unwrap();
        assert!(!shown.contains("test-pw-") && !shown.contains("51Sxz9") && !shown.contains("aKRq7"), "no password in the report: {shown}");
        let pw = if carried == "nd-default" { "test-pw-default-1" } else { "test-pw-custom-2" };
        assert_eq!(signs_in(&s, carried, pw).await, (true, true), "{carried} signs in with Navidrome's password (token and p=)");
        assert_eq!(signs_in(&s, carried, "wrong-password").await, (false, false));
        let other = if reset == "nd-default" { "test-pw-default-1" } else { "test-pw-custom-2" };
        assert_eq!(signs_in(&s, reset, other).await, (false, false), "a password that didn't open isn't guessed");
    }
}

#[tokio::test]
async fn navidrome_again_only_with_replace() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let nd = tmp.path().join("navidrome.db");
    navidrome_db(&nd).await;
    let run = async |replace: bool| conductor::import_nd::run(&s.st.cfg, &s.st.db, &s.st.secrets, &s.st.libs, &nd, None, None, replace).await;
    run(false).await.unwrap();
    // used since: a playlist of yours
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    s.ok("createPlaylist", &[("name", "Made since"), ("songId", &one)]).await;
    let e = run(false).await.unwrap_err().to_string();
    assert!(e.contains("--replace"), "{e}");
    assert!(s.ok("getPlaylists", &[]).await.to_string().contains("Made since"), "nothing was replaced");
    run(true).await.unwrap();
    assert!(!s.ok("getPlaylists", &[]).await.to_string().contains("Made since"), "asked for: replaced");
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
    let rep = conductor::import_nd::music_lookup(&s.st.db, data.path(), None).await.unwrap();
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

/// The web player: served at / and /web, self-contained, and every Subsonic call it makes works.
#[tokio::test]
async fn web_player() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let mut page = String::new();
    for path in ["/", "/web", "/web/"] {
        let r = s.http.get(format!("{}{path}", s.base)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
        assert!(r.headers()["content-security-policy"].to_str().unwrap().contains("default-src 'self'"));
        page = r.text().await.unwrap();
        assert!(page.contains("<title>Recital</title>"), "{path}");
    }
    // small, and nothing to load from elsewhere: no outside scripts, styles, fonts or pictures
    assert!(page.len() < 60_000, "page is {} bytes", page.len());
    let outside = regex::Regex::new(r#"(?:src|href)=["']?(?:https?:)?//"#).unwrap();
    assert!(!outside.is_match(&page), "the page links outside: {:?}", outside.find(&page));
    assert!(!page.contains("@import") && !page.contains("url(http"));
    // its icons come from this server (Conductor's logo, served by brand.rs)
    let icons = regex::Regex::new(r#"<link rel="(?:icon|apple-touch-icon)"[^>]* href="(/[^"]+)""#).unwrap();
    let hrefs: Vec<&str> = icons.captures_iter(&page).map(|c| c.get(1).unwrap().as_str()).collect();
    assert_eq!(hrefs, ["/favicon.svg", "/favicon-32.png", "/icon-192.png"]);
    for href in hrefs {
        let r = s.http.get(format!("{}{href}", s.base)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{href}");
        assert!(r.headers()["content-type"].to_str().unwrap().starts_with("image/"), "{href}");
    }

    // the methods the page calls, all checked below
    let calls = regex::Regex::new(r#"(?:api|url|cover)\('(\w+)'"#).unwrap();
    let mut used: Vec<&str> = calls.captures_iter(&page).map(|c| c.get(1).unwrap().as_str()).collect();
    used.sort();
    used.dedup();
    let checked = [
        "getAlbum",
        "getAlbumList2",
        "getArtist",
        "getArtists",
        "getCoverArt",
        "getPlaylist",
        "getPlaylists",
        "getStarred2",
        "ping",
        "scrobble",
        "search3",
        "star",
        "stream",
        "unstar",
    ];
    assert_eq!(used, checked);

    s.ok("ping", &[]).await;
    let al = s.ok("getAlbumList2", &[("type", "newest"), ("size", "60")]).await;
    let albums = al["albumList2"]["album"].as_array().unwrap();
    assert_eq!(albums.len(), 2);
    let more = s.ok("getAlbumList2", &[("type", "newest"), ("size", "60"), ("offset", "2")]).await;
    assert!(more["albumList2"]["album"].as_array().is_none_or(|a| a.is_empty()));
    let ar = s.ok("getArtists", &[]).await;
    let alpha = ar["artists"]["index"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| i["artist"].as_array().unwrap())
        .find(|a| a["name"] == "Alpha")
        .unwrap();
    assert_eq!(alpha["albumCount"], 1);
    let artist = s.ok("getArtist", &[("id", alpha["id"].as_str().unwrap())]).await;
    let album_id = artist["artist"]["album"][0]["id"].as_str().unwrap().to_string();
    let album = s.ok("getAlbum", &[("id", &album_id)]).await["album"].clone();
    assert_eq!(album["name"], "First Album");
    let songs = album["song"].as_array().unwrap();
    assert_eq!(songs.len(), 2);
    let one = &songs[0];
    let id = one["id"].as_str().unwrap();
    // what the page reads from a song
    for k in ["title", "artist", "artistId", "album", "albumId", "coverArt", "duration", "track", "suffix"] {
        assert!(!one[k].is_null(), "song has no {k}: {one}");
    }

    // cover and stream (with Range, which the audio element uses to seek)
    let r = s.http.get(s.url("getCoverArt", &[("id", one["coverArt"].as_str().unwrap()), ("size", "300")])).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "image/jpeg");
    let r = s.http.get(s.url("stream", &[("id", id)])).header("Range", "bytes=0-99").send().await.unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.bytes().await.unwrap().len(), 100);

    let found = s.ok("search3", &[("query", "Two"), ("artistCount", "12"), ("albumCount", "18"), ("songCount", "50")]).await;
    assert_eq!(found["searchResult3"]["song"][0]["title"], "Two");
    let found = s.ok("search3", &[("query", "alpha")]).await;
    assert_eq!(found["searchResult3"]["artist"][0]["name"], "Alpha");
    assert_eq!(found["searchResult3"]["album"][0]["name"], "First Album");

    // heart, starred songs, scrobble
    s.ok("star", &[("id", id)]).await;
    let st = s.ok("getStarred2", &[]).await;
    assert_eq!(st["starred2"]["song"][0]["id"], id);
    assert!(!st["starred2"]["song"][0]["starred"].is_null());
    s.ok("unstar", &[("id", id)]).await;
    assert!(s.ok("getStarred2", &[]).await["starred2"]["song"].as_array().is_none_or(|a| a.is_empty()));
    s.ok("scrobble", &[("id", id), ("submission", "false")]).await;
    s.ok("scrobble", &[("id", id), ("submission", "true"), ("time", "1700000000000")]).await;
    let plays: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM play").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(plays, 1, "the submission counts as one play");

    // playlists
    s.ok("createPlaylist", &[("name", "Web"), ("songId", id)]).await;
    let pl = s.ok("getPlaylists", &[]).await;
    let pid = pl["playlists"]["playlist"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(s.ok("getPlaylist", &[("id", &pid)]).await["playlist"]["entry"][0]["id"], id);

    // a wrong password is refused with code 40, which the page shows as "Wrong username or password"
    let salt = "x1";
    let bad = format!("{:x}", md5::compute(format!("nope{salt}")));
    let r: Value = s
        .http
        .get(format!("{}/rest/ping?u={USER}&t={bad}&s={salt}&v=1.16.1&c=recital&f=json", s.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["subsonic-response"]["error"]["code"], 40);
}

#[tokio::test]
async fn podcasts_internet_only_and_for_admins() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    // a feed on this computer or the home network isn't fetched (by name or by number)
    let fl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = fl.local_addr().unwrap().port();
    let app = axum::Router::new()
        .route("/feed.xml", axum::routing::get(|| async { "<rss><channel><title>Inside</title></channel></rss>" }));
    tokio::spawn(async move { axum::serve(fl, app).await.unwrap() });
    for url in [
        format!("http://127.0.0.1:{port}/feed.xml"),
        format!("http://localhost:{port}/feed.xml"),
        format!("http://[::1]:{port}/feed.xml"),
        "http://169.254.169.254/latest/meta-data/".to_string(),
        "http://192.168.1.1/feed.xml".to_string(),
    ] {
        let r = s.call("createPodcastChannel", &[("url", &url)]).await;
        assert_eq!(r["status"], "failed", "{url}: {r}");
        assert!(r["error"]["message"].as_str().unwrap().contains("Couldn't read that feed"), "{url}: {r}");
    }
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM podcast").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(n, 0);

    // someone who isn't an admin can listen, not change the shows
    auth::create_user(&s.st.db, &s.st.secrets, "guest", "guest pass", false).await.unwrap();
    for (m, extra) in [
        ("createPodcastChannel", ("url", "https://feeds.example.com/x.xml")),
        ("deletePodcastChannel", ("id", "pc-1")),
        ("refreshPodcasts", ("x", "")),
        ("downloadPodcastEpisode", ("id", "pe-1")),
        ("deletePodcastEpisode", ("id", "pe-1")),
    ] {
        let url = format!("{}/rest/{m}.view?u=guest&p=guest%20pass&v=1.16.1&c=test&f=json&{}={}", s.base, extra.0, extra.1);
        let r: Value = s.http.get(url).send().await.unwrap().json().await.unwrap();
        assert_eq!(r["subsonic-response"]["error"]["code"], 50, "{m}: {r}");
    }
    let url = format!("{}/rest/getPodcasts.view?u=guest&p=guest%20pass&v=1.16.1&c=test&f=json", s.base);
    let r: Value = s.http.get(url).send().await.unwrap().json().await.unwrap();
    assert_eq!(r["subsonic-response"]["status"], "ok");
}

#[tokio::test]
async fn smart_rules_see_only_readable_playlists() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    // the admin's private playlist, with one song
    let song = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let pl =
        s.ok("createPlaylist", &[("name", "Secret"), ("songId", &song)]).await["playlist"]["id"].as_str().unwrap().to_string();
    let other = auth::create_user(&s.st.db, &s.st.secrets, "other", "other pass", false).await.unwrap();
    let rule = serde_json::json!({"all": [{"inPlaylist": {"id": pl}}]});
    let not_rule = serde_json::json!({"all": [{"notInPlaylist": {"id": pl}}]});
    // its owner sees what's in it
    let mine = api::smart::evaluate(&s.st, &s.st_user().await, &rule).await.unwrap();
    assert_eq!(mine, vec![song.clone()]);
    // someone else learns nothing from it: it's as if it were empty
    assert!(api::smart::evaluate(&s.st, &other, &rule).await.unwrap().is_empty());
    assert_eq!(api::smart::evaluate(&s.st, &other, &not_rule).await.unwrap().len(), 3);
    // once it's public, they can use it
    sqlx::query("UPDATE playlist SET public = 1 WHERE id = ?").bind(&pl).execute(&s.st.db).await.unwrap();
    assert_eq!(api::smart::evaluate(&s.st, &other, &rule).await.unwrap(), vec![song]);
}

#[tokio::test]
async fn sign_in_codes_capped_per_address() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    let start = |from: &'static str| {
        let url = format!("{}/rest/pairStart?f=json&c=test&name=Laptop", s.base);
        // the test talks over loopback, so X-Forwarded-For stands for who's asking (as behind Caddy)
        let rq = s.http.get(url).header("x-forwarded-for", from);
        async move { rq.send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone() }
    };
    for _ in 0..conductor::live::MAX_PAIRS_PER_IP {
        assert_eq!(start("203.0.113.7").await["status"], "ok");
    }
    let r = start("203.0.113.7").await;
    assert_eq!(r["status"], "failed", "{r}");
    // someone else can still get a code
    assert_eq!(start("198.51.100.20").await["status"], "ok");
}

#[tokio::test]
async fn wrong_passwords_are_limited() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    let ping = |pass: &str, from: &'static str| {
        let url = format!("{}/rest/ping.view?u={USER}&p={}&v=1.16.1&c=test&f=json", s.base, pass.replace(' ', "%20"));
        let rq = s.http.get(url).header("x-forwarded-for", from);
        async move { rq.send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone() }
    };
    // a right password clears the count
    for _ in 0..auth::APP_TRIES - 1 {
        assert_eq!(ping("nope", "203.0.113.7").await["error"]["code"], 40);
    }
    assert_eq!(ping(PASS, "203.0.113.7").await["status"], "ok");
    for _ in 0..auth::APP_TRIES {
        assert_eq!(ping("nope", "203.0.113.7").await["error"]["code"], 40);
    }
    // past the limit even the right one is refused from there, for a while
    let r = ping(PASS, "203.0.113.7").await;
    assert!(r["error"]["message"].as_str().unwrap().contains("Too many"), "{r}");
    // not from anywhere else
    assert_eq!(ping(PASS, "198.51.100.20").await["status"], "ok");

    // the admin page has a smaller limit
    let admin = |pass: &str| {
        let rq = s
            .http
            .post(format!("{}/admin/api/login", s.base))
            .header("x-conductor", "1")
            .header("x-forwarded-for", "192.0.2.50")
            .json(&serde_json::json!({"username": USER, "password": pass}));
        async move { rq.send().await.unwrap().status().as_u16() }
    };
    for _ in 0..auth::ADMIN_TRIES {
        assert_eq!(admin("nope").await, 401);
    }
    assert_eq!(admin(PASS).await, 429);
}

#[tokio::test]
async fn shares_work_without_an_account() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    // the public side lives on the Jam listener
    let jl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let jam = format!("http://{}", jl.local_addr().unwrap());
    let app = conductor::jam::router(s.st.clone());
    tokio::spawn(async move { axum::serve(jl, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });

    let album = s.ok("search3", &[("query", "First Album")]).await["searchResult3"]["album"][0]["id"].as_str().unwrap().to_string();
    let r = s.ok("createShare", &[("id", &album), ("description", "For you"), ("expires", "0")]).await;
    let sh = &r["shares"]["share"][0];
    assert_eq!(sh["description"], "For you");
    assert_eq!(sh["entry"].as_array().unwrap().len(), 2, "{sh}");
    let url = sh["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("https://jam.example.test/jam/s/"), "{url}");
    let token = url.rsplit('/').next().unwrap().to_string();
    assert_eq!(s.ok("getShares", &[]).await["shares"]["share"].as_array().unwrap().len(), 1);

    // anyone with the link: the page, the list, a song and its cover
    let page = s.http.get(format!("{jam}/jam/s/{token}")).send().await.unwrap();
    assert_eq!(page.status(), 200);
    let st: Value = s.http.get(format!("{jam}/jam/s/{token}/state")).send().await.unwrap().json().await.unwrap();
    assert_eq!((st["owner"].as_str(), st["description"].as_str()), (Some(USER), Some("For you")));
    assert_eq!(st["song"][1]["title"], "Two");
    assert!(st["song"][0].get("id").is_none(), "visitors never see ids");
    let song = s.http.get(format!("{jam}/jam/s/{token}/stream/0")).header("range", "bytes=0-99").send().await.unwrap();
    assert_eq!(song.status(), 206);
    assert_eq!(song.bytes().await.unwrap().len(), 100);
    assert_eq!(s.http.get(format!("{jam}/jam/s/{token}/cover/0")).send().await.unwrap().status(), 200);
    assert_eq!(s.http.get(format!("{jam}/jam/s/{token}/stream/2")).send().await.unwrap().status(), 404, "only the shared songs");
    assert_eq!(s.ok("getShares", &[]).await["shares"]["share"][0]["visitCount"], 1);

    // expired, then deleted: gone
    s.ok("updateShare", &[("id", &token), ("expires", "1000")]).await;
    assert_eq!(s.http.get(format!("{jam}/jam/s/{token}/state")).send().await.unwrap().status(), 410);
    assert_eq!(s.http.get(format!("{jam}/jam/s/{token}/stream/0")).send().await.unwrap().status(), 404);
    s.ok("deleteShare", &[("id", &token)]).await;
    assert!(s.ok("getShares", &[]).await["shares"]["share"].as_array().unwrap().is_empty());
    assert_eq!(s.call("createShare", &[("id", "nope")]).await["error"]["code"], 70);
}

#[tokio::test]
async fn users_over_the_api() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    s.ok("createUser", &[("username", "sam"), ("password", "enc:73616d2070617373"), ("email", "x@example.com")]).await; // "sam pass"
    assert_eq!(s.call("createUser", &[("username", "sam"), ("password", "another1")]).await["error"]["code"], 0);
    assert_eq!(s.call("createUser", &[("username", "short"), ("password", "abc")]).await["error"]["code"], 10);
    let users = s.ok("getUsers", &[]).await;
    assert_eq!(users["users"]["user"].as_array().unwrap().len(), 2);
    let sam = s.ok("getUser", &[("username", "sam")]).await;
    assert_eq!((sam["user"]["adminRole"].as_bool(), sam["user"]["streamRole"].as_bool()), (Some(false), Some(true)));
    assert_eq!(s.call("getUser", &[("username", "nobody")]).await["error"]["code"], 70);

    // sam signs in, can't manage others, can change their own password
    let as_sam = |m: &str, pass: &str, q: &str| format!("{}/rest/{m}.view?u=sam&p={}&v=1.16.1&c=test&f=json{q}", s.base, pass.replace(' ', "%20"));
    let get = |u: String| {
        let rq = s.http.get(u);
        async move { rq.send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone() }
    };
    assert_eq!(get(as_sam("ping", "sam pass", "")).await["status"], "ok");
    assert_eq!(get(as_sam("getUsers", "sam pass", "")).await["error"]["code"], 50);
    assert_eq!(get(as_sam("deleteUser", "sam pass", &format!("&username={USER}"))).await["error"]["code"], 50);
    assert_eq!(get(as_sam("changePassword", "sam pass", "&username=sam&password=new%20sam%20pass")).await["status"], "ok");
    assert_eq!(get(as_sam("ping", "new sam pass", "")).await["status"], "ok");

    // the only admin stays one, and can't delete themself
    assert_eq!(s.call("updateUser", &[("username", USER), ("adminRole", "false")]).await["error"]["code"], 50);
    assert_eq!(s.call("deleteUser", &[("username", USER)]).await["error"]["code"], 50);
    s.ok("updateUser", &[("username", "sam"), ("adminRole", "true")]).await;
    assert_eq!(s.ok("getUser", &[("username", "sam")]).await["user"]["adminRole"], true);

    // a picture for each
    let pic = s.http.get(s.url("getAvatar", &[("username", "sam")])).send().await.unwrap();
    assert_eq!(pic.headers()["content-type"], "image/png");
    assert!(image::load_from_memory(&pic.bytes().await.unwrap()).is_ok());

    s.ok("deleteUser", &[("username", "sam")]).await;
    assert_eq!(get(as_sam("ping", "new sam pass", "")).await["error"]["code"], 40);
}

#[tokio::test]
async fn backup_and_restore() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    s.ok("createPlaylist", &[("name", "Keep me")]).await;
    let out = s._dir.path().join("b/backup.zip");
    let n = conductor::backup::make(&s.st.cfg, &s.st.db, &out).await.unwrap();
    assert!(n > 1000);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o077, 0, "only you can read it");
    }

    // somewhere new: it all comes back, and the password still works (the key came too)
    let fresh = tempfile::tempdir().unwrap();
    let cfg = Config { data_dir: fresh.path().join("data"), listen: "127.0.0.1:0".parse().unwrap(), ..Config::default() };
    let r = conductor::backup::restore(&cfg, &out, false).unwrap();
    assert!(r.files.contains(&"conductor.db".to_string()) && r.files.contains(&"secret.key".to_string()), "{r:?}");
    let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
    let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
    assert!(auth::verify_password(&pool, &secrets, USER, PASS).await.unwrap().is_some());
    let pls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist WHERE name = 'Keep me'").fetch_one(&pool).await.unwrap();
    let songs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track").fetch_one(&pool).await.unwrap();
    assert_eq!((pls, songs), (1, 3));
    pool.close().await;
    drop(pool);

    // over a database that's there: only with --force, and what was there is kept aside. (A folder this test never
    // opened: on Windows the database library holds the file a while after closing, and Conductor is stopped for a
    // real restore anyway.)
    let cfg2 = Config { data_dir: fresh.path().join("data2"), listen: "127.0.0.1:0".parse().unwrap(), ..Config::default() };
    conductor::backup::restore(&cfg2, &out, false).unwrap();
    assert!(conductor::backup::restore(&cfg2, &out, false).unwrap_err().to_string().contains("already a database"));
    let r = conductor::backup::restore(&cfg2, &out, true).unwrap();
    let aside = r.moved_aside.unwrap();
    assert!(aside.join("conductor.db").is_file() && aside.join("secret.key").is_file());

    // not a backup
    std::fs::write(fresh.path().join("x.zip"), b"nope").unwrap();
    assert!(conductor::backup::restore(&cfg, &fresh.path().join("x.zip"), true).is_err());

    // the daily ones: only the newest kept, other files left alone
    let d = fresh.path().join("daily");
    std::fs::create_dir_all(&d).unwrap();
    for n in ["conductor-20260101-000000.zip", "conductor-20260102-000000.zip", "conductor-20260103-000000.zip", "conductor-backup-mine.zip", "notes.txt"] {
        std::fs::write(d.join(n), b"x").unwrap();
    }
    conductor::backup::prune(&d, 2);
    let mut left: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    left.sort();
    assert_eq!(left, ["conductor-20260102-000000.zip", "conductor-20260103-000000.zip", "conductor-backup-mine.zip", "notes.txt"]);
}

#[tokio::test]
async fn whole_library_paging_gets_each_song_once() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    // apps that sync everything ask for "" a page at a time; every song once, in a stable order
    let mut seen = vec![];
    for off in 0..5 {
        let r = s.ok("search3", &[("query", ""), ("artistCount", "0"), ("albumCount", "0"), ("songCount", "1"), ("songOffset", &off.to_string())]).await;
        for song in r["searchResult3"]["song"].as_array().cloned().unwrap_or_default() {
            seen.push(song["id"].as_str().unwrap().to_string());
        }
    }
    let mut uniq = seen.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!((seen.len(), uniq.len()), (3, 3), "{seen:?}");
    // a word still finds, and starred lists come from the stars
    assert_eq!(s.ok("search3", &[("query", "Three")]).await["searchResult3"]["song"].as_array().unwrap().len(), 1);
    s.ok("star", &[("id", &seen[0])]).await;
    let st = s.ok("getStarred2", &[]).await;
    assert_eq!(st["starred2"]["song"][0]["id"], seen[0].as_str());
    let g = s.ok("getGenres", &[]).await;
    assert_eq!(g["genres"]["genre"][0]["songCount"], 3);
}

#[tokio::test]
async fn play_queue_by_index_keeps_the_right_copy() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    let ids: Vec<String> = s.ok("search3", &[("query", "")]).await["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    let (a, b) = (ids[0].as_str(), ids[1].as_str());
    // the same song twice: the second copy is the one playing
    s.ok("savePlayQueueByIndex", &[("id", a), ("id", b), ("id", a), ("currentIndex", "2"), ("position", "1500")]).await;
    let q = s.ok("getPlayQueueByIndex", &[]).await;
    assert_eq!((q["playQueueByIndex"]["currentIndex"].as_i64(), q["playQueueByIndex"]["position"].as_i64()), (Some(2), Some(1500)));
    assert_eq!(q["playQueueByIndex"]["entry"].as_array().unwrap().len(), 3);
    assert_eq!(s.ok("getPlayQueue", &[]).await["playQueue"]["current"], a, "the id-based view still works");
    assert_eq!(s.call("savePlayQueueByIndex", &[("id", a), ("currentIndex", "5")]).await["error"]["code"], 10);
    // saved the old way, the place is the first copy
    s.ok("savePlayQueue", &[("id", b), ("id", a), ("current", a)]).await;
    assert_eq!(s.ok("getPlayQueueByIndex", &[]).await["playQueueByIndex"]["currentIndex"], 1);
    let ext = s.ok("getOpenSubsonicExtensions", &[]).await;
    assert!(ext["openSubsonicExtensions"].as_array().unwrap().iter().any(|e| e["name"] == "indexBasedQueue"));
}

#[tokio::test]
async fn web_apps_from_elsewhere_can_use_the_api() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    // a web app on another address (Feishin's web version): the browser asks first, then calls
    let pre = s
        .http
        .request(reqwest::Method::OPTIONS, format!("{}/rest/getUser.view", s.base))
        .header("origin", "http://player.example")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .unwrap();
    assert!(pre.status().is_success());
    assert_eq!(pre.headers()["access-control-allow-origin"], "*");
    let r = s.http.get(s.url("ping", &[])).header("origin", "http://player.example").send().await.unwrap();
    assert_eq!(r.headers()["access-control-allow-origin"], "*");
    // the admin page (signed in with a cookie) keeps the same-address rule
    let a = s.http.get(format!("{}/admin/api/status", s.base)).header("origin", "http://player.example").send().await.unwrap();
    assert!(a.headers().get("access-control-allow-origin").is_none());
}

#[tokio::test]
async fn a_service_account_doesnt_block_the_first_admin() {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::open(&dir.path().join("c.db")).await.unwrap();
    let secrets = Secrets::load_or_create(&dir.path().join("secret.key")).unwrap();
    // Prelude made AudioMuse's account before anyone had an admin
    auth::create_user(&pool, &secrets, "audiomuse", "service pass", false).await.unwrap();
    assert_eq!(auth::admin_count(&pool).await.unwrap(), 0);
    assert!(auth::create_first_admin(&pool, &secrets, "me", "my pass 1").await.unwrap().is_some());
    // once there's an admin, nobody else can claim it
    assert!(auth::create_first_admin(&pool, &secrets, "someone", "their pass").await.unwrap().is_none());
    assert_eq!(auth::admin_count(&pool).await.unwrap(), 1);
}

#[tokio::test]
async fn users_stay_apart() {
    if !have_ffmpeg() {
        eprintln!("skipping: ffmpeg not found");
        return;
    }
    let s = Server::start().await;
    // two people who aren't admins: Ann and Ben
    for (n, p) in [("ann", "ann pass 1"), ("ben", "ben pass 1")] {
        auth::create_user(&s.st.db, &s.st.secrets, n, p, false).await.unwrap();
    }
    let call = |who: &'static str, m: &'static str, params: Vec<(&'static str, String)>| {
        let pass = if who == "ann" { "ann pass 1" } else { "ben pass 1" };
        let mut q = form_urlencoded::Serializer::new(String::new());
        q.extend_pairs([("u", who), ("p", pass), ("v", "1.16.1"), ("c", "test"), ("f", "json")]);
        for (k, v) in &params {
            q.append_pair(k, v);
        }
        let rq = s.http.get(format!("{}/rest/{m}.view?{}", s.base, q.finish()));
        async move { rq.send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone() }
    };
    let ok = |r: Value| {
        assert_eq!(r["status"], "ok", "{r}");
        r
    };
    let songs: Vec<String> = ok(call("ann", "search3", vec![("query", String::new())]).await)["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    let (a, b) = (songs[0].clone(), songs[1].clone());

    // Ann's stars, rating, plays, queue, bookmark, sync values, device
    ok(call("ann", "star", vec![("id", a.clone())]).await);
    ok(call("ann", "setRating", vec![("id", a.clone()), ("rating", "5".into())]).await);
    ok(call("ann", "scrobble", vec![("id", a.clone()), ("submission", "true".into())]).await);
    ok(call("ann", "savePlayQueue", vec![("id", a.clone()), ("id", b.clone()), ("current", b.clone())]).await);
    ok(call("ann", "createBookmark", vec![("id", a.clone()), ("position", "1000".into())]).await);
    ok(call("ann", "syncSet", vec![("key", "pins".into()), ("value", "[1]".into()), ("at", "1000".into())]).await);
    ok(call("ann", "deviceHello", vec![("id", "ann-phone".into()), ("name", "Ann's phone".into()), ("kind", "phone".into())]).await);
    // her private playlist and a public one
    let private = ok(call("ann", "createPlaylist", vec![("name", "Ann only".into()), ("songId", a.clone())]).await)["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let public = ok(call("ann", "createPlaylist", vec![("name", "For all".into()), ("songId", b.clone())]).await)["playlist"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    ok(call("ann", "updatePlaylist", vec![("playlistId", public.clone()), ("public", "true".into())]).await);

    // Ben sees none of it
    let st = ok(call("ben", "getStarred2", vec![]).await);
    assert!(st["starred2"]["song"].as_array().is_none_or(|x| x.is_empty()), "stars: {st}");
    let song = ok(call("ben", "getSong", vec![("id", a.clone())]).await);
    assert_eq!((song["song"]["userRating"].as_i64(), song["song"]["playCount"].as_i64()), (Some(0), Some(0)), "rating and plays");
    assert!(song["song"].get("starred").is_none());
    assert!(ok(call("ben", "getPlayQueue", vec![]).await).get("playQueue").is_none(), "play queue");
    let bm = ok(call("ben", "getBookmarks", vec![]).await);
    assert!(bm["bookmarks"]["bookmark"].as_array().is_none_or(|x| x.is_empty()), "bookmarks: {bm}");
    let sync = ok(call("ben", "syncGet", vec![]).await);
    assert!(sync["sync"]["values"].get("pins").is_none(), "sync values: {sync}");
    let dev = ok(call("ben", "deviceList", vec![]).await);
    assert!(dev["devices"]["device"].as_array().is_none_or(|x| x.is_empty()), "devices: {dev}");
    let stats = ok(call("ben", "getListeningStats", vec![("period", "all".into())]).await);
    assert_eq!(stats["listeningStats"]["plays"], 0, "Ben's listening stats: {stats}");
    let ann_stats = ok(call("ann", "getListeningStats", vec![("period", "all".into())]).await);
    assert_eq!(ann_stats["listeningStats"]["plays"], 1, "Ann's: {ann_stats}");
    let alb = ok(call("ben", "getAlbumList2", vec![("type", "frequent".into())]).await);
    assert!(alb["albumList2"]["album"].as_array().is_none_or(|x| x.is_empty()), "frequent albums: {alb}");

    // playlists: the private one isn't there for Ben, the public one is, and neither is his to change
    let names: Vec<String> = ok(call("ben", "getPlaylists", vec![]).await)["playlists"]["playlist"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|p| p["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["For all"], "{names:?}");
    assert_ne!(call("ben", "getPlaylist", vec![("id", private.clone())]).await["status"], "ok", "Ann's private playlist");
    assert_eq!(call("ben", "updatePlaylist", vec![("playlistId", public.clone()), ("name", "Mine now".into())]).await["error"]["code"], 50);
    assert_eq!(call("ben", "deletePlaylist", vec![("id", public.clone())]).await["error"]["code"], 50);
    assert_ne!(call("ben", "deletePlaylist", vec![("id", private.clone())]).await["status"], "ok");
    // nor her bookmark, her account, or the user list
    ok(call("ben", "deleteBookmark", vec![("id", a.clone())]).await);
    let still = ok(call("ann", "getBookmarks", vec![]).await);
    assert_eq!(still["bookmarks"]["bookmark"].as_array().map(|x| x.len()), Some(1), "Ben's delete didn't touch Ann's bookmark");
    assert_eq!(call("ben", "getUser", vec![("username", "ann".into())]).await["error"]["code"], 50);
    assert_eq!(call("ben", "changePassword", vec![("username", "ann".into()), ("password", "taken over".into())]).await["error"]["code"], 50);
    assert_eq!(call("ben", "getUsers", vec![]).await["error"]["code"], 50);
    // and Ann's own view is intact
    assert_eq!(ok(call("ann", "getSong", vec![("id", a.clone())]).await)["song"]["userRating"], 5);
    assert_eq!(ok(call("ann", "getPlayQueue", vec![]).await)["playQueue"]["current"], b.as_str());
}

#[tokio::test]
async fn helpers_see_every_music_folder() {
    if !have_ffmpeg() {
        return;
    }
    // two music folders with the same path inside each: only the "lib<n>/" prefix tells them apart
    let dir = tempfile::tempdir().unwrap();
    let (m0, m1) = (dir.path().join("m0"), dir.path().join("m1"));
    tone(&m0.join("A/B/01 Same.flac"), "First Copy", "A", "B", 1, 2);
    tone(&m1.join("A/B/01 Same.flac"), "Second Copy", "A", "B", 2, 2);
    let (sid, _) = stand_in(|path, _, _| match path {
        "/identify" => Some(serde_json::json!({ "path": "lib1/A/B/01 Same.flac", "offset": 1.0, "score": 30 })),
        _ => None,
    })
    .await;
    let mix = dir.path().join("mix");
    std::fs::create_dir_all(&mix).unwrap();
    std::fs::write(mix.join("info.jsonl"), "{\"path\": \"lib0/A/B/01 Same.flac\", \"dur\": 2.0, \"bpm\": 99.0}\n").unwrap();
    let mut cfg = Config {
        data_dir: dir.path().join("data"),
        libraries: vec![
            LibraryConfig { name: "One".into(), path: m0, sentinel: None, scan_threads: None },
            LibraryConfig { name: "Two".into(), path: m1, sentinel: None, scan_threads: None },
        ],
        songid_url: Some(sid),
        mixinfo_dir: Some(mix.clone()),
        catalog: false,
        ..Config::default()
    };
    Server::no_network(&mut cfg);
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
    let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
    auth::create_user(&pool, &secrets, USER, PASS, true).await.unwrap();
    let libs = library::ensure_libraries(&pool, &cfg.libraries).await.unwrap();
    let st = Arc::new(AppState::new(pool, secrets, cfg, libs));
    assert_eq!(library::scan(&st.db, &st.libs, &st.scan, false).await.unwrap().added, 2);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = api::router(st.clone());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let s = Server { base, http: reqwest::Client::new(), st, _dir: dir, music: std::path::PathBuf::new() };

    let songs = s.ok("search3", &[("query", "")]).await["searchResult3"]["song"].as_array().unwrap().clone();
    let id = |t: &str| songs.iter().find(|x| x["title"] == t).unwrap()["id"].as_str().unwrap().to_string();
    // Pitch's "lib1/…" is the second folder's song, not the first's (same path inside)
    let v: Value = s.http.post(s.url("identifySong", &[])).body("audio").send().await.unwrap().json().await.unwrap();
    assert_eq!(v["subsonic-response"]["songIdentification"]["song"]["title"], "Second Copy", "{v}");
    // Metronome's "lib0/…" is the first folder's; the second's isn't measured yet, so it's asked for with its prefix
    let r = s.ok("getMixInfo", &[("id", &id("First Copy")), ("id", &id("Second Copy"))]).await;
    let got = r["mixInfo"]["song"].as_array().unwrap();
    assert_eq!((got.len(), got[0]["id"].as_str()), (1, Some(id("First Copy").as_str())), "{r}");
    assert_eq!(std::fs::read_to_string(mix.join("wanted.txt")).unwrap().trim(), "lib1/A/B/01 Same.flac");
}

#[tokio::test]
async fn waiting_imports_are_capped() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let body = r#"[{"ts":"2024-03-01T10:00:00Z","ms_played":180000,"master_metadata_track_name":"One","master_metadata_album_artist_name":"Alpha","spotify_track_uri":"spotify:track:1111111111111111111111"}]"#;
    let upload = || async {
        let r: Value = s
            .http
            .post(s.url("importUpload", &[("name", "Streaming_History_Audio_2024.json")]))
            .body(body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        r["subsonic-response"].clone()
    };
    let mut ids = vec![];
    for _ in 0..conductor::import::jobs::MAX_WAITING {
        let r = upload().await;
        assert_eq!(r["status"], "ok", "{r}");
        ids.push(r["import"]["id"].as_str().unwrap().to_string());
    }
    // a sixth waits for room: refused before anything is taken
    let r = upload().await;
    assert_eq!(r["status"], "failed", "{r}");
    assert!(r["error"]["message"].as_str().unwrap().contains("waiting already"), "{r}");
    // discarding one makes room again
    s.ok("deleteImport", &[("id", &ids[0])]).await;
    assert_eq!(upload().await["status"], "ok");
}

#[tokio::test]
async fn limits_on_what_apps_send() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();

    // stats far back: refused at once, not a stuck worker or a crash
    for p in ["week", "month", "year"] {
        let r = s.call("getListeningStats", &[("period", p), ("offset", "9223372036854775807")]).await;
        assert_eq!(r["error"]["code"], 10, "{p}: {r}");
        let r = s.call("getListeningStats", &[("period", p), ("offset", "300000")]).await;
        assert_eq!(r["error"]["code"], 10, "{p}: {r}");
    }
    s.ok("getListeningStats", &[("period", "month"), ("offset", "14")]).await;

    // a form past the 1 MB that used to be the limit is read; one past the limit is refused, and the queue stays
    s.ok("savePlayQueue", &[("id", &one)]).await;
    let pad = "x".repeat(1_500_000);
    let r: Value = s
        .http
        .post(s.url("savePlayQueue", &[]))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("id={one}&current={one}&pad={pad}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(r["subsonic-response"]["status"], "ok", "{r}");
    assert_eq!(s.ok("getPlayQueue", &[]).await["playQueue"]["current"], one.as_str());
    let huge = "x".repeat(9 << 20);
    let sent = s
        .http
        .post(s.url("savePlayQueue", &[]))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(format!("pad={huge}"))
        .send()
        .await;
    if let Ok(r) = sent {
        let r: Value = r.json().await.unwrap();
        assert_eq!(r["subsonic-response"]["status"], "failed", "{r}");
    }
    let q = s.ok("getPlayQueue", &[]).await;
    assert_eq!(q["playQueue"]["entry"].as_array().map(Vec::len), Some(1), "the queue wasn't emptied: {q}");

    // devices: at most 20 per account, the one heard from longest ago makes room
    for i in 0..25 {
        s.ok("deviceHello", &[("id", &format!("device-{i:04}"))]).await;
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    let list = s.ok("deviceList", &[]).await;
    let ids: Vec<&str> = list["devices"]["device"].as_array().unwrap().iter().map(|d| d["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 20);
    assert!(!ids.contains(&"device-0000") && ids.contains(&"device-0024"), "{ids:?}");
    // and the commands waiting for one device are kept to 512 KB, newest kept
    let big = format!(r#"{{"type":"queue","pad":"{}"}}"#, "y".repeat(60_000));
    for _ in 0..12 {
        let r: Value = s
            .http
            .post(s.url("deviceCommand", &[]))
            .form(&[("id", "device-0023"), ("target", "device-0024"), ("command", &big)])
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(r["subsonic-response"]["status"], "ok", "{r}");
    }
    let r = s.ok("devicePoll", &[("id", "device-0024"), ("ack", "0"), ("since", "0"), ("wait", "0")]).await;
    let cmds = r["devicePoll"]["commands"].as_array().unwrap();
    let bytes: usize = cmds.iter().map(|c| c.to_string().len()).sum();
    assert!(bytes <= 512 * 1024 && cmds.len() < 12, "{} commands, {bytes} bytes", cmds.len());
    assert_eq!(cmds.last().unwrap()["seq"], 12);

    // sign-in codes: 10 wrong ones an account, then even the right one waits
    let start: Value =
        s.http.get(format!("{}/rest/pairStart?f=json&c=test&name=Laptop", s.base)).send().await.unwrap().json().await.unwrap();
    let code = start["subsonic-response"]["pair"]["code"].as_str().unwrap().to_string();
    let wrong = if code == "000001" { "000002" } else { "000001" };
    let login = serde_json::json!({"server": "https://music.example", "user": USER, "pass": PASS}).to_string();
    for _ in 0..10 {
        assert_eq!(s.call("pairApprove", &[("code", wrong), ("login", &login)]).await["error"]["code"], 70);
    }
    let r = s.call("pairApprove", &[("code", &code), ("login", &login)]).await;
    assert!(r["error"]["message"].as_str().unwrap_or("").contains("Too many"), "{r}");

    // API keys: an app with a revoked key that keeps trying doesn't lock out the other apps at the same address
    let key = auth::create_api_key(&s.st.db, USER, "phone").await.unwrap();
    let ping = |k: String| {
        let u = format!("{}/rest/ping?apiKey={k}&f=json", s.base);
        let h = s.http.clone();
        async move { h.get(u).send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone() }
    };
    for _ in 0..auth::APP_TRIES {
        assert_eq!(ping("enc_revoked".into()).await["error"]["code"], 44);
    }
    assert_eq!(ping(key.clone()).await["status"], "ok", "a right key still gets in");
    assert_eq!(ping("enc_revoked".into()).await["error"]["code"], 40, "the wrong one is held back");
}

#[tokio::test]
async fn places_count_only_songs_shown() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let found = s.ok("search3", &[("query", "\"\""), ("songCount", "10")]).await;
    let songs = found["searchResult3"]["song"].as_array().unwrap().clone();
    let id = |t: &str| songs.iter().find(|x| x["title"] == t).unwrap()["id"].as_str().unwrap().to_string();
    let (one, two, three) = (id("One"), id("Two"), id("Three"));
    // a playlist whose first song is missing (its drive is out): getPlaylist shows Two, Three
    let pl = s.ok("createPlaylist", &[("name", "P"), ("songId", &one), ("songId", &two), ("songId", &three)]).await;
    let pid = pl["playlist"]["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE track SET missing_since = 1 WHERE id = ?").bind(&one).execute(&s.st.db).await.unwrap();
    let shown = |r: Value| -> Vec<String> {
        r["playlist"]["entry"].as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(shown(s.ok("getPlaylist", &[("id", &pid)]).await), [two.clone(), three.clone()]);
    // taking out place 0 (as shown: Two) takes out Two, and the missing song stays for when it's back
    s.ok("updatePlaylist", &[("playlistId", &pid), ("songIndexToRemove", "0")]).await;
    assert_eq!(shown(s.ok("getPlaylist", &[("id", &pid)]).await), std::slice::from_ref(&three));
    sqlx::query("UPDATE track SET missing_since = NULL WHERE id = ?").bind(&one).execute(&s.st.db).await.unwrap();
    assert_eq!(shown(s.ok("getPlaylist", &[("id", &pid)]).await), [one.clone(), three.clone()]);

    // a queue with a song that's gone: the place given back is the current song's among those sent
    s.ok("savePlayQueueByIndex", &[("id", "gone-song"), ("id", &two), ("id", &three), ("currentIndex", "2")]).await;
    let q = s.ok("getPlayQueueByIndex", &[]).await["playQueueByIndex"].clone();
    let i = q["currentIndex"].as_u64().unwrap() as usize;
    assert_eq!(q["entry"][i]["id"], three.as_str(), "{q}");
    // the current one itself gone: no place rather than the wrong song
    s.ok("savePlayQueueByIndex", &[("id", &two), ("id", "gone-song"), ("currentIndex", "1")]).await;
    assert!(s.ok("getPlayQueueByIndex", &[]).await["playQueueByIndex"]["currentIndex"].is_null());

    // an album that's emptied (all its songs gone) is "not found", not a crash
    let album = s.ok("getSong", &[("id", &three)]).await["song"]["albumId"].as_str().unwrap().to_string();
    sqlx::query("UPDATE album SET song_count = 0 WHERE id = ?").bind(&album).execute(&s.st.db).await.unwrap();
    assert_eq!(s.call("getMusicDirectory", &[("id", &album)]).await["error"]["code"], 70);
    assert_eq!(s.call("getSimilarSongs", &[("id", &album)]).await["error"]["code"], 70);
    s.ok("ping", &[]).await;
}

#[tokio::test]
async fn a_library_keeps_its_id_when_its_folder_is_written_differently_or_moves() {
    let dir = tempfile::tempdir().unwrap();
    let pool = db::open(&dir.path().join("conductor.db")).await.unwrap();
    let lib =
        |name: &str, p: std::path::PathBuf| LibraryConfig { name: name.into(), path: p, sentinel: None, scan_threads: None };
    let (old, moved) = (dir.path().join("old"), dir.path().join("moved"));
    std::fs::create_dir_all(&old).unwrap();
    let first = library::ensure_libraries(&pool, &[lib("Music", old.clone())]).await.unwrap()[0].0;
    // with a separator at the end: the same folder
    let slash = std::path::PathBuf::from(format!("{}/", old.display()));
    assert_eq!(library::ensure_libraries(&pool, &[lib("Music", slash)]).await.unwrap()[0].0, first);
    // still there: another folder is another library
    std::fs::create_dir_all(&moved).unwrap();
    let other = library::ensure_libraries(&pool, &[lib("Music", old.clone()), lib("Music", moved.clone())]).await.unwrap();
    assert_eq!(other[0].0, first);
    assert_ne!(other[1].0, first);
    sqlx::query("DELETE FROM library WHERE id = ?").bind(other[1].0).execute(&pool).await.unwrap();
    // the folder moved (the old one is gone): the same library, so the songs keep their ids
    std::fs::remove_dir_all(&old).unwrap();
    assert_eq!(library::ensure_libraries(&pool, &[lib("Music", moved.clone())]).await.unwrap()[0].0, first);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library").fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    // a gone folder of another name isn't taken over
    let gone = dir.path().join("gone");
    let podcasts = library::ensure_libraries(&pool, &[lib("Music", moved.clone()), lib("Audiobooks", gone)]).await.unwrap();
    assert_eq!(podcasts[0].0, first);
    assert_ne!(podcasts[1].0, first);
}

#[tokio::test]
async fn uploads_sent_at_once_still_wait_their_turn() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let body = r#"[{"ts":"2024-03-01T10:00:00Z","ms_played":180000,"master_metadata_track_name":"One","master_metadata_album_artist_name":"Alpha","spotify_track_uri":"spotify:track:1111111111111111111111"}]"#;
    let addr = s.base.trim_start_matches("http://").to_string();
    let path = s.url("importUpload", &[("name", "Streaming_History_Audio_2024.json")]).trim_start_matches(&s.base).to_string();
    // every upload is under way (its first byte sent) before any of them is done
    let mut conns = vec![];
    for _ in 0..conductor::import::jobs::MAX_WAITING + 3 {
        let mut c = tokio::net::TcpStream::connect(&addr).await.unwrap();
        let head = format!("POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
        c.write_all(head.as_bytes()).await.unwrap();
        c.write_all(&body.as_bytes()[..1]).await.unwrap();
        conns.push(c);
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let mut taken = 0;
    for mut c in conns {
        let _ = c.write_all(&body.as_bytes()[1..]).await;
        let mut out = String::new();
        let _ = c.read_to_string(&mut out).await;
        if out.contains(r#""status":"ok""#) {
            taken += 1;
        }
    }
    assert_eq!(taken, conductor::import::jobs::MAX_WAITING);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM import_batch").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(n, conductor::import::jobs::MAX_WAITING);
}

#[tokio::test]
async fn every_user_scrobbles_to_their_own_accounts() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering::SeqCst},
    };
    if !have_ffmpeg() {
        return;
    }
    #[derive(Default)]
    struct Fake {
        listens: Mutex<Vec<(String, Value)>>,      // (authorization, body)
        lastfm: Mutex<Vec<HashMap<String, String>>>, // track calls' parameters
        approved: AtomicBool,
        down: AtomicBool,
        revoked: AtomicBool,
    }
    use std::collections::HashMap;
    let fake = Arc::new(Fake::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let f2 = fake.clone();
    let app = axum::Router::new().fallback(move |req: Request| {
        let fake = f2.clone();
        async move {
            let path = req.uri().path().to_string();
            let auth = req.headers().get("authorization").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let body = axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap();
            let body = match path.as_str() {
                "/1/validate-token" => match auth.as_str() {
                    "Token samtok" => json!({ "valid": true, "user_name": "sam_lb" }),
                    _ => json!({ "valid": false }),
                },
                "/1/submit-listens" => {
                    fake.listens.lock().unwrap().push((auth, serde_json::from_slice(&body).unwrap()));
                    json!({ "status": "ok" })
                }
                "/lfm/" => {
                    let p: HashMap<String, String> = form_urlencoded::parse(&body).into_owned().collect();
                    // signed as Last.fm wants, with this server's shared secret
                    let mut v: Vec<(String, String)> =
                        p.iter().filter(|(k, _)| *k != "format" && *k != "api_sig").map(|(k, v)| (k.clone(), v.clone())).collect();
                    v.sort();
                    let sig = format!("{:x}", md5::compute(format!("{}lfmsecret", v.iter().map(|(k, v)| format!("{k}{v}")).collect::<String>())));
                    assert_eq!(p["api_sig"], sig, "signature");
                    assert_eq!(p["api_key"], "lfmkey");
                    assert_eq!(p["format"], "json");
                    match p["method"].as_str() {
                        "auth.getToken" => json!({ "token": "lfmtok" }),
                        "auth.getSession" if !fake.approved.load(SeqCst) => json!({ "error": 14, "message": "Unauthorized Token" }),
                        "auth.getSession" => json!({ "session": { "name": "samfm", "key": "sk-sam", "subscriber": 0 } }),
                        _ if fake.down.load(SeqCst) => json!({ "error": 16, "message": "Temporarily unavailable" }),
                        _ if fake.revoked.load(SeqCst) => json!({ "error": 9, "message": "Invalid session key" }),
                        _ => {
                            assert_eq!(p["sk"], "sk-sam");
                            fake.lastfm.lock().unwrap().push(p);
                            json!({ "scrobbles": {} })
                        }
                    }
                }
                _ => return (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            };
            axum::Json(body).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let (lb, lfm) = (base.clone(), format!("{base}/lfm"));
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.listenbrainz_url = lb;
        c.listenbrainz_token = Some("lbtok".into());
        c.listenbrainz_user = Some("owner_lb".into());
        c.lastfm_url = lfm;
        c.lastfm_api_key = Some("lfmkey".into());
        c.lastfm_secret = Some("lfmsecret".into());
    })
    .await;
    s.ok("createUser", &[("username", "sam"), ("password", "sam pass")]).await;
    let get = async |u: String| -> Value {
        let r = s.http.get(&u).send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone();
        contract::observe_url(&u, &r);
        r
    };
    let sam = |m: &str, q: &str| format!("{}/rest/{m}.view?u=sam&p=sam%20pass&v=1.16.1&c=test&f=json{q}", s.base);
    let mut ids = Vec::new();
    for t in ["One", "Two", "Three"] {
        ids.push(s.ok("search3", &[("query", t)]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string());
    }
    let (one, two, three) = (ids[0].clone(), ids[1].clone(), ids[2].clone());

    // it's offered to apps
    let ext = s.ok("getOpenSubsonicExtensions", &[]).await;
    assert!(ext["openSubsonicExtensions"].as_array().unwrap().iter().any(|e| e["name"] == "encore.scrobbling"));

    // the owner has the settings' token; sam has nothing yet, and Last.fm can be linked
    let me = s.ok("getScrobbling", &[]).await["scrobbling"].clone();
    assert_eq!((me["listenbrainz"]["linked"].as_bool(), me["listenbrainz"]["fromSettings"].as_bool()), (Some(true), Some(true)));
    assert_eq!(me["lastfm"]["available"], true);
    let r = get(sam("getScrobbling", "")).await;
    assert_eq!((r["scrobbling"]["listenbrainz"]["linked"].as_bool(), r["scrobbling"]["lastfm"]["linked"].as_bool()), (Some(false), Some(false)));

    // sam links ListenBrainz: a token it doesn't accept isn't kept
    let r = get(sam("setListenBrainz", "&token=wrong")).await;
    assert_eq!((r["status"].as_str(), r["error"]["code"].as_i64()), (Some("failed"), Some(0)));
    let r = get(sam("setListenBrainz", "&token=samtok")).await;
    assert_eq!(r["scrobbling"]["listenbrainz"]["name"], "sam_lb");
    // kept encrypted, never as it is
    let stored: Vec<String> = sqlx::query_scalar("SELECT secret FROM scrobble_account").fetch_all(&s.st.db).await.unwrap();
    assert!(stored.iter().all(|x| !x.contains("samtok")));

    // and Last.fm: the page to approve on, then waiting until it's approved; nobody else can finish sam's link
    let r = get(sam("lastfmLinkStart", "")).await;
    let url = r["lastfmLink"]["url"].as_str().unwrap().to_string();
    assert!(url.starts_with("https://www.last.fm/api/auth/?api_key=lfmkey&token=lfmtok"), "{url}");
    fake.approved.store(true, SeqCst);
    assert_eq!(s.ok("lastfmLinkFinish", &[("token", "lfmtok")]).await["lastfmLink"]["status"], "expired");
    fake.approved.store(false, SeqCst);
    assert_eq!(get(sam("lastfmLinkFinish", "&token=lfmtok")).await["lastfmLink"]["status"], "waiting");
    fake.approved.store(true, SeqCst);
    let r = get(sam("lastfmLinkFinish", "&token=lfmtok")).await;
    assert_eq!((r["lastfmLink"]["status"].as_str(), r["lastfmLink"]["name"].as_str()), (Some("linked"), Some("samfm")));
    assert_eq!(get(sam("getScrobbling", "")).await["scrobbling"]["lastfm"]["name"], "samfm");

    // sam's plays go to sam's accounts, the owner's to the settings' token only
    get(sam("scrobble", &format!("&id={one}&submission=false"))).await;
    get(sam("scrobble", &format!("&id={one}&time=1700000000000"))).await;
    s.ok("scrobble", &[("id", &two)]).await;
    wait_for(async || fake.listens.lock().unwrap().len() >= 3 && fake.lastfm.lock().unwrap().len() >= 2, "the plays").await;
    {
        let listens = fake.listens.lock().unwrap();
        let by = |who: &str, kind: &str| listens.iter().filter(|(a, b)| a == who && b["listen_type"] == kind).count();
        assert_eq!((by("Token samtok", "playing_now"), by("Token samtok", "single"), by("Token lbtok", "single")), (1, 1, 1));
        let lfm = fake.lastfm.lock().unwrap();
        // (two requests from the app: which reaches Last.fm first isn't fixed)
        assert!(lfm.iter().any(|p| p["method"] == "track.updateNowPlaying"), "{lfm:?}");
        let sc = lfm.iter().find(|p| p["method"] == "track.scrobble").unwrap();
        assert_eq!((sc["track"].as_str(), sc["artist"].as_str()), ("One", "Alpha"));
        assert_eq!((sc["timestamp"].as_str(), sc["album"].as_str()), ("1700000000", "First Album"));
    }

    // Last.fm down: plays are kept, and later ones wait behind them, then all go in order
    fake.down.store(true, SeqCst);
    get(sam("scrobble", &format!("&id={two}&time=1700000100000"))).await;
    wait_for(
        async || sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM lb_queue WHERE service = 'lastfm'").fetch_one(&s.st.db).await.unwrap() == 1,
        "the kept play",
    )
    .await;
    fake.down.store(false, SeqCst);
    get(sam("scrobble", &format!("&id={three}&time=1700000200000"))).await;
    wait_for(
        async || sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM lb_queue WHERE service = 'lastfm'").fetch_one(&s.st.db).await.unwrap() == 2,
        "the later play kept behind it",
    )
    .await;
    conductor::scrobble::resend(&s.st).await;
    let titles: Vec<String> = fake.lastfm.lock().unwrap()[2..].iter().map(|p| p["track"].clone()).collect();
    assert_eq!(titles, ["Two", "Three"]);
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM lb_queue").fetch_one(&s.st.db).await.unwrap(), 0);

    // a session Last.fm no longer takes: shown as needing a new link
    fake.revoked.store(true, SeqCst);
    get(sam("scrobble", &format!("&id={one}"))).await;
    wait_for(async || get(sam("getScrobbling", "")).await["scrobbling"]["lastfm"]["broken"] == true, "the broken link").await;

    // unlinking
    let r = get(sam("lastfmUnlink", "")).await;
    assert_eq!(r["scrobbling"]["lastfm"]["linked"], false);
    let r = get(sam("setListenBrainz", "&token=")).await;
    assert_eq!(r["scrobbling"]["listenbrainz"]["linked"], false);
    // and deleting a user takes their accounts with them
    get(sam("setListenBrainz", "&token=samtok")).await;
    s.ok("deleteUser", &[("username", "sam")]).await;
    assert_eq!(sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM scrobble_account").fetch_one(&s.st.db).await.unwrap(), 0);
}

#[tokio::test]
async fn albums_without_a_cover_get_a_picture_except_in_apps_that_draw_their_own() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let album = s.ok("search3", &[("query", "Three")]).await["searchResult3"]["song"][0]["albumId"].as_str().unwrap().to_string();
    // most apps: a picture, the size asked for
    let r = s.http.get(s.url("getCoverArt", &[("id", &album), ("size", "200")])).send().await.unwrap();
    assert_eq!((r.status().as_u16(), r.headers()["content-type"].to_str().unwrap().to_string()), (200, "image/png".into()));
    let img = image::load_from_memory(&r.bytes().await.unwrap()).unwrap();
    assert_eq!((img.width(), img.height()), (200, 200));
    // Encore (phone or desktop) and Recital: "not found", as before
    for app in ["Encore", "Encore Desktop", "recital"] {
        let u = s.url("getCoverArt", &[("id", &album)]).replace("c=test", &format!("c={}", app.replace(' ', "+")));
        assert_eq!(s.http.get(u).send().await.unwrap().status(), 404, "{app}");
    }
    // ...and Recital does draw its own: a picture that fails loses its src (else browsers show a broken image)
    let page = s.http.get(format!("{}/", s.base)).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("document.addEventListener('error'") && page.contains("removeAttribute('src')"));
    // an album with a cover still gets its own
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["albumId"].as_str().unwrap().to_string();
    let r = s.http.get(s.url("getCoverArt", &[("id", &one)])).send().await.unwrap();
    assert_eq!(r.headers()["content-type"], "image/jpeg");
}

#[tokio::test]
async fn cyrillic_tags_written_as_latin1_are_read_as_cyrillic_also_in_songs_scanned_before() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    // Windows-1251 bytes that a tagger marked as Latin-1: «Лучшее часть 2 диск 2» by «Аквариум»
    let mp3 = s.music.join("Akvarium/02.mp3");
    tone_tags(&mp3, 2, &[("title", "Ãîðîä"), ("artist", "Àêâàðèóì"), ("album", "Ëó÷øåå ÷àñòü 2 äèñê 2")]);
    tone_tags(&s.music.join("Beyonce/01.mp3"), 1, &[("title", "Halo"), ("artist", "Beyoncé"), ("album", "I Am... Sasha Fierce")]);
    library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let song = async |q: &str| s.ok("search3", &[("query", q)]).await["searchResult3"]["song"][0].clone();
    let t = song("Город").await;
    assert_eq!((t["artist"].as_str(), t["album"].as_str()), (Some("Аквариум"), Some("Лучшее часть 2 диск 2")));
    assert_eq!(song("Halo").await["artist"], "Beyoncé");

    // a library scanned by an older version has the garbled names: the upgrade has them read again, once
    sqlx::query("UPDATE track SET title = 'Ãîðîä', artist_name = 'Àêâàðèóì' WHERE title = 'Город'").execute(&s.st.db).await.unwrap();
    for stmt in include_str!("../migrations/0022_reread_latin1_tags.sql").split(';').filter(|x| x.lines().any(|l| !l.trim().is_empty() && !l.trim_start().starts_with("--"))) {
        sqlx::query(stmt).execute(&s.st.db).await.unwrap();
    }
    let rep = library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    assert_eq!(rep.updated, 2, "the garbled song and Beyoncé's are read again; the plain ASCII ones aren't");
    let title: String = sqlx::query_scalar("SELECT title FROM track WHERE artist_name = 'Аквариум'").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(title, "Город");
}

#[tokio::test]
async fn plays_handed_in_together_reach_listenbrainz_in_order_and_a_refused_token_needs_a_new_link() {
    use axum::{extract::Request, response::IntoResponse};
    use serde_json::json;
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering::SeqCst},
    };
    if !have_ffmpeg() {
        return;
    }
    #[derive(Default)]
    struct Fake {
        listens: Mutex<Vec<(String, String)>>, // (authorization, track)
        revoked: AtomicBool,
        busy: AtomicBool,
    }
    let fake = Arc::new(Fake::default());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let f2 = fake.clone();
    let app = axum::Router::new().fallback(move |req: Request| {
        let fake = f2.clone();
        async move {
            let path = req.uri().path().to_string();
            let auth = req.headers().get("authorization").map(|v| v.to_str().unwrap().to_string()).unwrap_or_default();
            let body: Value = serde_json::from_slice(&axum::body::to_bytes(req.into_body(), 1 << 16).await.unwrap()).unwrap_or_default();
            match path.as_str() {
                "/1/validate-token" => axum::Json(json!({ "valid": true, "user_name": "sam_lb" })).into_response(),
                "/1/submit-listens" if fake.revoked.load(SeqCst) && auth == "Token samtok" => {
                    (axum::http::StatusCode::UNAUTHORIZED, "{}").into_response()
                }
                "/1/submit-listens" if fake.busy.load(SeqCst) && auth == "Token samtok" => {
                    (axum::http::StatusCode::TOO_MANY_REQUESTS, "{}").into_response()
                }
                "/1/submit-listens" => {
                    let title = body["payload"][0]["track_metadata"]["track_name"].as_str().unwrap_or("").to_string();
                    // the first one is slow to be taken: sent all at once, the others would overtake it
                    if title == "One" {
                        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                    }
                    fake.listens.lock().unwrap().push((auth, title));
                    axum::Json(json!({ "status": "ok" })).into_response()
                }
                _ => (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.listenbrainz_url = base;
        c.listenbrainz_token = Some("lbtok".into());
    })
    .await;
    let mut ids = Vec::new();
    for t in ["One", "Two", "Three"] {
        ids.push(s.ok("search3", &[("query", t)]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string());
    }
    // three plays made offline, handed in with one call
    s.ok("scrobble", &[("id", &ids[0]), ("id", &ids[1]), ("id", &ids[2]), ("time", "1700000000000"), ("time", "1700000200000"), ("time", "1700000400000")])
        .await;
    wait_for(async || fake.listens.lock().unwrap().len() >= 3, "the plays").await;
    let titles: Vec<String> = fake.listens.lock().unwrap().iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(titles, ["One", "Two", "Three"]);

    // sam's own token, which ListenBrainz later stops taking: sam is told to link it again
    s.ok("createUser", &[("username", "sam"), ("password", "sam pass")]).await;
    let sam = |m: &str, q: &str| format!("{}/rest/{m}.view?u=sam&p=sam%20pass&v=1.16.1&c=test&f=json{q}", s.base);
    let get = async |u: String| -> Value {
        let r = s.http.get(&u).send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone();
        contract::observe_url(&u, &r);
        r
    };
    assert_eq!(get(sam("setListenBrainz", "&token=samtok")).await["scrobbling"]["listenbrainz"]["broken"], false);
    fake.revoked.store(true, SeqCst);
    get(sam("scrobble", &format!("&id={}", ids[1]))).await;
    wait_for(async || get(sam("getScrobbling", "")).await["scrobbling"]["listenbrainz"]["broken"] == true, "the broken link").await;
    // linking again mends it
    fake.revoked.store(false, SeqCst);
    assert_eq!(get(sam("setListenBrainz", "&token=samtok")).await["scrobbling"]["listenbrainz"]["broken"], false);

    // sam's token turned away for now (too many), with a long wait of plays: the owner's waiting play still goes
    fake.busy.store(true, SeqCst);
    let sam_id: String = sqlx::query_scalar("SELECT id FROM user WHERE name = 'sam'").fetch_one(&s.st.db).await.unwrap();
    let body = json!({ "listen_type": "single", "payload": [{ "listened_at": 1, "track_metadata": { "artist_name": "A", "track_name": "Sam's" } }] });
    for _ in 0..600 {
        sqlx::query("INSERT INTO lb_queue (payload, added_at, user_id, service) VALUES (?, 0, ?, 'listenbrainz')")
            .bind(body.to_string())
            .bind(&sam_id)
            .execute(&s.st.db)
            .await
            .unwrap();
    }
    // (queued by an older version: no user, the settings' token)
    let mine = json!({ "listen_type": "single", "payload": [{ "listened_at": 2, "track_metadata": { "artist_name": "A", "track_name": "Owner's" } }] });
    sqlx::query("INSERT INTO lb_queue (payload, added_at) VALUES (?, 0)").bind(mine.to_string()).execute(&s.st.db).await.unwrap();
    conductor::scrobble::resend(&s.st).await;
    assert!(fake.listens.lock().unwrap().iter().any(|(a, t)| a == "Token lbtok" && t == "Owner's"));
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lb_queue").fetch_one(&s.st.db).await.unwrap();
    assert_eq!(left, 600, "sam's wait for later, in order");
}

#[tokio::test]
async fn song_radio_without_audiomuse_stays_close() {
    // no AudioMuse, no online info: the artists that go with a song come from the library and its plays
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start_with(|c| c.catalog = false).await;
    let m = &s.music;
    tone_genre(&m.join("Kino/Gruppa/01.mp3"), "Группа крови", "Кино", "Группа крови", 1, 2, "Russian Rock");
    tone_genre(&m.join("Kino/Gruppa/02.mp3"), "Кукушка", "Кино", "Группа крови", 2, 2, "Russian Rock");
    tone_genre(&m.join("DDT/Osen/01.mp3"), "Осень", "ДДТ", "Актриса Весна", 1, 2, "Rock; Russian Rock");
    tone_genre(&m.join("Chaif/Arg/01.mp3"), "Аргентина", "Чайф", "Шекогали", 1, 2, "Rock");
    tone_genre(&m.join("Zemfira/Z/01.mp3"), "Искала", "Земфира", "Земфира", 1, 2, "Pop");
    tone_genre(&m.join("Oasis/Morning/01.mp3"), "Wonderwall", "Oasis", "Morning Glory", 1, 2, "Britpop");
    tone_genre(&m.join("Rimsky/Sch/01.mp3"), "Scheherazade", "Rimsky-Korsakov", "Scheherazade", 1, 2, "Classical");
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let id = |t: &'static str| {
        let s = &s;
        async move { s.ok("search3", &[("query", t)]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string() }
    };
    let (kino, zemfira) = (id("Группа крови").await, id("Искала").await);
    // Кино and Земфира are played in the same sittings
    for day in 0..3i64 {
        let t = 1_700_000_000_000 + day * 86_400_000;
        s.ok("scrobble", &[("id", &kino), ("time", &t.to_string())]).await;
        s.ok("scrobble", &[("id", &zemfira), ("time", &(t + 240_000).to_string())]).await;
    }
    let r = s.ok("getSimilarSongs2", &[("id", &kino), ("count", "20")]).await;
    let artists: Vec<String> =
        r["similarSongs2"]["song"].as_array().unwrap().iter().map(|x| x["artist"].as_str().unwrap().to_string()).collect();
    let at = |a: &str| artists.iter().position(|x| x == a);
    // Кино's other song, the artist played with it, then Russian rock before Britpop; classical not at all
    assert!(artists.contains(&"Кино".to_string()), "{artists:?}");
    assert!(at("Земфира").is_some_and(|z| at("Oasis").is_none_or(|o| z < o)), "{artists:?}");
    assert!(at("ДДТ").is_some_and(|d| at("Oasis").is_none_or(|o| d < o)), "{artists:?}");
    assert!(at("Чайф").is_some(), "Rock shares a word with Russian Rock: {artists:?}");
    assert!(at("Rimsky-Korsakov").is_none(), "{artists:?}");
    // and the same artists as Кино's similar artists, without any online info
    let a = s.ok("getSong", &[("id", &kino)]).await["song"]["artistId"].as_str().unwrap().to_string();
    let info = s.ok("getArtistInfo2", &[("id", &a), ("count", "3")]).await;
    let names: Vec<&str> = info["artistInfo2"]["similarArtist"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names.len(), 3, "{names:?}");
    assert!(names.contains(&"Земфира") && names.contains(&"ДДТ"), "{names:?}");
}


/// The API's contract: the shape of every answer (its fields and their types) is recorded in docs/api/shapes/, one
/// file per call. Every call a test makes is checked against it: a field that isn't recorded, or has another type,
/// fails the test. `CONDUCTOR_BLESS=1 cargo test --test api` records them again; CI does that and fails if the files
/// change, so a dropped or changed field is seen (STABILITY.md: fields may be added, never taken away).
mod contract {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
        sync::Mutex,
    };

    use serde_json::Value;

    type Shape = BTreeMap<String, BTreeSet<String>>;
    static SEEN: Mutex<BTreeMap<String, Shape>> = Mutex::new(BTreeMap::new());
    /// What was committed, read before this run writes over it: a field's types are kept (a field that's sometimes
    /// null, a list that's sometimes empty, seen one way in one run and the other way in another), while the fields
    /// themselves come from this run alone (so one that's gone shows).
    static COMMITTED: Mutex<BTreeMap<String, Shape>> = Mutex::new(BTreeMap::new());
    /// The envelope every answer has (checked by Subsonic apps themselves, and the same for every call).
    const ENVELOPE: &[&str] = &["status", "version", "type", "serverVersion", "openSubsonic", "error"];

    fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/api/shapes")
    }

    /// A key that's data, not a field name (a date, an id, a number): one name for all of them.
    fn key(k: &str) -> &str {
        // (ids are long and have digits; long field names like displayAlbumArtist don't)
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
            Value::Array(a) => {
                for x in a {
                    walk(x, &format!("{path}[]"), out);
                }
            }
            Value::Object(o) => {
                for (k, x) in o {
                    walk(x, &format!("{path}.{}", key(k)), out);
                }
            }
            _ => {}
        }
    }

    /// The same, for an answer to a request made by hand (its method from the address).
    pub fn observe_url(url: &str, r: &Value) {
        let path = url.split('?').next().unwrap_or("");
        if let Some(m) = path.split("/rest/").nth(1) {
            observe(m, r);
        }
    }

    pub fn observe(method: &str, r: &Value) {
        let method = method.trim_end_matches(".view");
        if method.contains('/') || r["status"] != "ok" {
            return; // a passed-through answer (AudioMuse's), or an error (its shape is Subsonic's)
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
                let e = all.entry(p.clone()).or_default();
                e.extend(t);
                if let Some(k) = before.get(&p) {
                    e.extend(k.iter().cloned());
                }
            }
            std::fs::create_dir_all(dir()).unwrap();
            std::fs::write(&file, serde_json::to_string_pretty(all).unwrap() + "\n").unwrap();
            return;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            panic!("{method}: no recorded shape in docs/api/shapes/ (record it: CONDUCTOR_BLESS=1 cargo test --test api)");
        };
        let known: Shape = serde_json::from_str(&text).unwrap();
        for (p, t) in &shape {
            match known.get(p) {
                None => panic!("{method}: the field {p} isn't recorded (a new field: CONDUCTOR_BLESS=1 cargo test --test api, and document it)"),
                Some(k) if !t.is_subset(k) => panic!("{method}: {p} is {t:?}, recorded as {k:?}"),
                _ => {}
            }
        }
    }
}


#[tokio::test]
async fn every_call_has_a_recorded_shape() {
    // the calls no other test makes, made here; then every call the server answers (but those that send files) must
    // have its shape recorded in docs/api/shapes/ (see the contract module)
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let one = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    for (m, p) in [
        ("getLicense", vec![]),
        ("getMusicFolders", vec![]),
        ("getScanStatus", vec![]),
        ("startScan", vec![]),
        ("getAlbumList", vec![("type", "newest")]),
        ("search2", vec![("query", "One")]),
        ("getStarred", vec![]),
        ("getTopSongs", vec![("artist", "Alpha")]),
        ("getSimilarSongs", vec![("id", one.as_str())]),
        ("getLyrics", vec![("artist", "Alpha"), ("title", "One")]),
        ("reportPlayback", vec![("mediaId", one.as_str()), ("mediaType", "song"), ("positionMs", "1000"), ("state", "playing")]),
        ("createInternetRadioStation", vec![("streamUrl", "http://radio.example/s"), ("name", "Test FM")]),
        ("createBookmark", vec![("id", one.as_str()), ("position", "1000")]),
        ("deleteBookmark", vec![("id", one.as_str())]),
        ("refreshPodcasts", vec![]),
        ("changePassword", vec![("username", USER), ("password", PASS)]),
        ("refreshMix", vec![("kind", "daylist")]),
        ("refreshConcerts", vec![]),
        ("getScrobbling", vec![]),
        ("setListenBrainz", vec![("token", "")]),
        ("lastfmUnlink", vec![]),
    ] {
        let r = s.call(m, &p).await;
        assert!(r["status"] == "ok" || r["error"]["code"].as_i64().is_some(), "{m}: {r}");
    }
    let radio = s.ok("getInternetRadioStations", &[]).await["internetRadioStations"]["internetRadioStation"][0]["id"].as_str().unwrap().to_string();
    s.ok("updateInternetRadioStation", &[("id", &radio), ("streamUrl", "http://radio.example/s2"), ("name", "Test FM 2")]).await;
    s.ok("deleteInternetRadioStation", &[("id", &radio)]).await;

    let src = [include_str!("../src/api/mod.rs"), include_str!("../src/api/catalog_api.rs")].concat();
    let mut calls = std::collections::BTreeSet::new();
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('"') && t.contains("=>") {
            for part in t.split("=>").next().unwrap().split('|') {
                let name = part.trim().trim_matches('"');
                if name.len() > 2 && name.chars().all(|c| c.is_ascii_alphanumeric()) && name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) {
                    calls.insert(name.to_string());
                }
            }
        }
    }
    calls.extend(["pairStart".to_string(), "pairPoll".to_string()]); // answered before login (src/api/mod.rs)
    let files = ["stream", "download", "getCoverArt", "getAvatar", "djAudio"];
    // calls that only answer with the catalog, a downloader or a service (their shapes come from the tests that have those)
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/api/shapes");
    if std::env::var_os("CONDUCTOR_BLESS").is_some() {
        return; // recording: the other tests may still be writing theirs
    }
    let missing: Vec<&String> = calls.iter().filter(|c| !files.contains(&c.as_str()) && !dir.join(format!("{c}.json")).is_file()).collect();
    assert!(missing.is_empty(), "calls without a recorded shape (make them in a test, then CONDUCTOR_BLESS=1): {missing:?}");
}

#[tokio::test]
async fn conversions_are_cached_and_take_turns() {
    if !have_ffmpeg() {
        return;
    }
    // one ffmpeg at a time: four conversions asked for together all come, in turn
    let s = Server::start_with(|c| {
        c.catalog = false;
        c.transcode_slots = 1;
    })
    .await;
    let ids: Vec<String> = s.ok("search3", &[("query", "")]).await["searchResult3"]["song"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|x| x["suffix"] == "flac") // (the MP3 is small enough to be sent as it is)
        .map(|x| x["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 2);
    let get = |id: String, kbps: &'static str| {
        let u = s.url("stream", &[("id", &id), ("format", "mp3"), ("maxBitRate", kbps)]);
        let http = s.http.clone();
        async move {
            let r = http.get(u).send().await.unwrap();
            let cache = r.headers().get("x-transcode-cache").map(|v| v.to_str().unwrap().to_string());
            (r.status().as_u16(), r.headers()["content-type"].to_str().unwrap().to_string(), cache, r.bytes().await.unwrap().len())
        }
    };
    let all = futures_util::future::join_all([get(ids[0].clone(), "96"), get(ids[1].clone(), "96"), get(ids[0].clone(), "128"), get(ids[1].clone(), "128")]).await;
    for (status, ct, cache, len) in &all {
        assert_eq!((*status, ct.as_str(), cache.as_deref()), (200, "audio/mpeg", Some("miss")), "converted now");
        assert!(*len > 10_000, "{len} bytes");
    }
    // the same again: from the cache, the same length, and seeking works on it
    let (status, _, cache, len) = get(ids[0].clone(), "96").await;
    assert_eq!((status, cache.as_deref(), len), (200, Some("hit"), all[0].3));
    let r = s.http.get(s.url("stream", &[("id", &ids[0]), ("format", "mp3"), ("maxBitRate", "96")])).header("Range", "bytes=100-199").send().await.unwrap();
    assert_eq!((r.status().as_u16(), r.bytes().await.unwrap().len()), (206, 100));
    let dir = s.st.cfg.cache_dir().join("transcode");
    let files = std::fs::read_dir(&dir).unwrap().count();
    assert_eq!(files, 4, "one file each, no parts left");

    // the song's file changes: not the old conversion
    let one = s.music.join("Alpha/First Album/01 One.flac");
    tone(&one, "One", "Alpha", "First Album", 1, 5);
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let one_id = s.ok("search3", &[("query", "One")]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string();
    let before = all[ids.iter().position(|i| *i == one_id).unwrap()].3;
    let (_, _, cache, len) = get(one_id, "96").await;
    assert_eq!(cache.as_deref(), Some("miss"), "the file changed: converted again");
    assert!(len > before, "the longer song: {len} > {before}");
}

#[tokio::test]
async fn pages_and_answers_carry_their_protections() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let get = |p: &'static str| s.http.get(format!("{}{p}", s.base)).send();
    // Podium and Recital: only their own script (by hash), never framed by another site
    for (p, framed_by_header) in [("/admin", true), ("/", false)] {
        let r = get(p).await.unwrap();
        let csp = r.headers()["content-security-policy"].to_str().unwrap().to_string();
        let script = csp.split(';').map(str::trim).find(|d| d.starts_with("script-src")).unwrap().to_string();
        assert!(script.contains("'sha256-") && !script.contains("unsafe-inline"), "{p}: {csp}");
        assert!(csp.contains("frame-ancestors 'none'"), "{p}: {csp}");
        if framed_by_header {
            assert_eq!(r.headers()["x-frame-options"], "DENY");
        }
        assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    }
    // API answers aren't to be read as anything else
    let r = s.http.get(s.url("ping", &[])).send().await.unwrap();
    assert_eq!(r.headers()["x-content-type-options"], "nosniff");
    // Podium's sign-in cookie: Secure when it came over HTTPS through a proxy on this computer, not otherwise
    let login = |proto: Option<&'static str>| {
        let mut b = s
            .http
            .post(format!("{}/admin/api/login", s.base))
            .header("X-Conductor", "1")
            .json(&serde_json::json!({ "username": USER, "password": PASS }));
        if let Some(p) = proto {
            b = b.header("X-Forwarded-Proto", p);
        }
        b.send()
    };
    let plain = login(None).await.unwrap();
    let cookie = plain.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict") && !cookie.contains("Secure"), "{cookie}");
    let tls = login(Some("https")).await.unwrap();
    assert!(tls.headers()["set-cookie"].to_str().unwrap().contains("; Secure"));
}

#[tokio::test]
async fn settings_from_podium() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let api = |p: &str| format!("{}/admin/api/{p}", s.base);
    assert_eq!(http.get(api("settings")).send().await.unwrap().status(), 401, "signed-in admins only");
    http.post(api("login")).header("X-Conductor", "1").json(&serde_json::json!({"username": USER, "password": PASS})).send().await.unwrap();
    let get = async || http.get(api("settings")).send().await.unwrap().json::<Value>().await.unwrap();
    let find = |all: &Value, name: &str| {
        all["sections"].as_array().unwrap().iter().flat_map(|s| s["settings"].as_array().unwrap().iter()).find(|x| x["name"] == name).cloned()
    };
    let all = get().await;
    // every documented setting but the two that move data
    assert!(find(&all, "data_dir").is_none() && find(&all, "libraries").is_none());
    let lyr = find(&all, "lyrics_online").unwrap();
    assert_eq!((lyr["kind"].as_str(), lyr["set"].as_bool()), (Some("bool"), Some(false)));
    assert_eq!(find(&all, "lastfm_secret").unwrap()["kind"], "secret");
    let set = |name: &'static str, value: Value| {
        let http = http.clone();
        let u = api("settings");
        async move { http.post(u).header("X-Conductor", "1").json(&serde_json::json!({ "name": name, "value": value })).send().await.unwrap() }
    };
    // changes need the header (another site can't send it)
    let no_header = http.post(api("settings")).json(&serde_json::json!({"name": "scan_interval_min", "value": 5})).send().await.unwrap();
    assert_eq!(no_header.status(), 403);
    assert_eq!(set("lyrics_online", Value::Bool(false)).await.status(), 200);
    assert_eq!(set("scan_interval_min", "30".into()).await.status(), 200);
    assert_eq!(set("discovery", "weekly-jams, daily-jams".into()).await.status(), 200);
    assert_eq!(set("lastfm_secret", "s3cr3t-value".into()).await.status(), 200);
    // settings with a decimal default take decimals (as JSON numbers or as typed)
    assert_eq!(set("lidarr_match_ratio", "0.9".into()).await.status(), 200);
    assert_eq!(set("trial_hours", serde_json::json!(12.5)).await.status(), 200);
    assert_eq!(set("trial_per_day", "2.5".into()).await.status(), 400, "a whole number");
    // a value Conductor can't take, a number that isn't, a setting that isn't one here
    assert_eq!(set("scan_interval_min", "soon".into()).await.status(), 400);
    assert_eq!(set("data_dir", "/tmp/x".into()).await.status(), 400);
    let file = s.st.cfg.data_dir.join("config.toml");
    let cfg = conductor::config::Config::load(Some(file.clone())).unwrap();
    assert_eq!((cfg.lyrics_online, cfg.scan_interval_min, cfg.discovery.clone()), (false, 30, vec!["weekly-jams".to_string(), "daily-jams".into()]));
    assert_eq!(cfg.lastfm_secret.as_deref(), Some("s3cr3t-value"));
    assert_eq!((cfg.lidarr_match_ratio, cfg.trial_hours), (0.9, 12.5));
    // the secret is in secrets.toml, not config.toml, and never sent back
    assert!(!std::fs::read_to_string(&file).unwrap().contains("s3cr3t"));
    assert!(std::fs::read_to_string(file.with_file_name("secrets.toml")).unwrap().contains("s3cr3t-value"));
    let all = get().await;
    let sec = find(&all, "lastfm_secret").unwrap();
    assert_eq!((sec["set"].as_bool(), sec["value"].is_null()), (Some(true), true));
    assert!(!all.to_string().contains("s3cr3t"));
    // back to the default
    assert_eq!(set("lyrics_online", Value::Null).await.status(), 200);
    assert!(find(&get().await, "lyrics_online").unwrap()["set"] == false);
}

#[tokio::test]
async fn made_for_you_without_audiomuse() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start_with(|c| c.catalog = false).await;
    let m = &s.music;
    tone_genre(&m.join("Kino/G/01.mp3"), "Группа крови", "Кино", "Группа крови", 1, 2, "Russian Rock");
    tone_genre(&m.join("Kino/G/02.mp3"), "Кукушка", "Кино", "Группа крови", 2, 2, "Russian Rock");
    tone_genre(&m.join("Kino/G/03.mp3"), "Звезда", "Кино", "Группа крови", 3, 2, "Russian Rock");
    tone_genre(&m.join("DDT/O/01.mp3"), "Осень", "ДДТ", "Актриса Весна", 1, 2, "Rock; Russian Rock");
    tone_genre(&m.join("DDT/O/02.mp3"), "Родина", "ДДТ", "Актриса Весна", 2, 2, "Rock; Russian Rock");
    tone_genre(&m.join("Chaif/A/01.mp3"), "Аргентина", "Чайф", "Шекогали", 1, 2, "Russian Rock");
    tone_genre(&m.join("Rimsky/S/01.mp3"), "Scheherazade", "Rimsky-Korsakov", "Scheherazade", 1, 2, "Classical");
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let ext = s.ok("getOpenSubsonicExtensions", &[]).await;
    assert!(ext["openSubsonicExtensions"].as_array().unwrap().iter().any(|e| e["name"] == "encore.forYou"));
    // nothing played or hearted yet: nothing to make it from
    assert_eq!(s.ok("getForYou", &[]).await["forYou"]["song"].as_array().map(|a| a.len()).unwrap_or(0), 0);
    let id = |t: &'static str| {
        let s = &s;
        async move { s.ok("search3", &[("query", t)]).await["searchResult3"]["song"][0]["id"].as_str().unwrap().to_string() }
    };
    let blood = id("Группа крови").await;
    for i in 0..5i64 {
        s.ok("scrobble", &[("id", &blood), ("time", &((conductor::db::now() - 3600 * (i + 1)) * 1000).to_string())]).await;
    }
    let mix = s.ok("getForYou", &[("count", "20")]).await;
    let songs = mix["forYou"]["song"].as_array().unwrap().clone();
    let artists: Vec<&str> = songs.iter().map(|x| x["artist"].as_str().unwrap()).collect();
    let titles: Vec<&str> = songs.iter().map(|x| x["title"].as_str().unwrap()).collect();
    assert!(!titles.contains(&"Группа крови"), "played in the last two weeks: {titles:?}");
    assert!(artists.contains(&"ДДТ") || artists.contains(&"Чайф"), "the artists that go with yours: {artists:?}");
    assert!(artists.contains(&"Кино"), "and yours, the songs not played lately: {artists:?}");
    assert!(!artists.contains(&"Rimsky-Korsakov"), "{artists:?}");
    assert!(artists.windows(2).all(|w| w[0] != w[1]) || artists.iter().collect::<std::collections::HashSet<_>>().len() == 1, "never twice in a row: {artists:?}");
    // the same all day
    assert_eq!(s.ok("getForYou", &[("count", "20")]).await["forYou"], mix["forYou"]);
}

#[tokio::test]
async fn search_ignores_case_in_any_alphabet() {
    if !have_ffmpeg() {
        return;
    }
    let s = Server::start().await;
    tone(&s.music.join("Kino/G/01.mp3"), "Группа крови", "Кино", "Группа крови", 1, 2);
    tone(&s.music.join("Kino/G/02.mp3"), "Звезда по имени Солнце", "Кино", "Группа крови", 2, 2);
    conductor::library::scan(&s.st.db, &s.st.libs, &s.st.scan, false).await.unwrap();
    let titles = |r: &Value| -> Vec<String> {
        let mut t: Vec<String> = r["searchResult3"]["song"].as_array().unwrap().iter().map(|x| x["title"].as_str().unwrap().into()).collect();
        t.sort();
        t
    };
    // as typed on a phone (lower case), all capitals, or as written
    for q in ["кино", "КИНО", "Кино"] {
        let r = s.ok("search3", &[("query", q)]).await;
        assert_eq!(titles(&r).len(), 2, "{q}: {r}");
        assert_eq!(r["searchResult3"]["artist"][0]["name"], "Кино", "{q}");
    }
    assert_eq!(titles(&s.ok("search3", &[("query", "звезда солнце")]).await), ["Звезда по имени Солнце"]);
    assert_eq!(titles(&s.ok("search3", &[("query", "СОЛНЦЕ кино")]).await), ["Звезда по имени Солнце"]);
    assert_eq!(titles(&s.ok("search3", &[("query", "группа КРОВИ")]).await).len(), 2, "the album's name");
    // the guest pages (Jam, Chorus) search the same way
    let found = conductor::jam::find_songs(&s.st, "кино звезда", 10).await;
    assert_eq!(found.len(), 1, "{found:?}");
    // A-Z as before, and a backslash or % is just a character
    assert_eq!(titles(&s.ok("search3", &[("query", "alpha ONE")]).await), ["One"]);
    assert!(titles(&s.ok("search3", &[("query", "on\\")]).await).is_empty());
    assert!(titles(&s.ok("search3", &[("query", "%")]).await).is_empty());
}

#[tokio::test]
async fn a_backup_restored_into_another_data_folder_is_used_there() {
    // made on one computer (its settings name its data folder)...
    let old = tempfile::tempdir().unwrap();
    let music = old.path().join("music");
    std::fs::create_dir_all(&music).unwrap();
    let toml_for = |data: &Path, port: u16| {
        format!(
            "data_dir = {:?}\nlisten = \"127.0.0.1:{port}\"\nlyrics_online = false\n\n[[libraries]]\nname = \"Music\"\npath = {:?}\n",
            data.to_string_lossy(),
            music.to_string_lossy()
        )
    };
    let old_cfg_path = old.path().join("config.toml");
    std::fs::write(&old_cfg_path, toml_for(&old.path().join("data"), 1)).unwrap();
    let cfg = Config::load(Some(old_cfg_path)).unwrap();
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
    let out = old.path().join("backup.zip");
    conductor::backup::make(&cfg, &pool, &out).await.unwrap();
    pool.close().await;
    // ...restored where the data folder is somewhere else: the settings come back, but keep this data folder
    let new = tempfile::tempdir().unwrap();
    let new_cfg_path = new.path().join("config.toml");
    std::fs::write(&new_cfg_path, toml_for(&new.path().join("data"), 0).replace("lyrics_online = false\n", "")).unwrap();
    let cfg2 = Config::load(Some(new_cfg_path.clone())).unwrap();
    let r = conductor::backup::restore(&cfg2, &out, false).unwrap();
    assert!(r.files.contains(&"config.toml".to_string()), "{r:?}");
    let after = Config::load(Some(new_cfg_path)).unwrap();
    assert_eq!(after.data_dir, new.path().join("data"), "where the database was restored");
    assert!(after.data_dir.join("conductor.db").is_file());
    assert!(!after.lyrics_online, "the rest of the settings came back");
}
