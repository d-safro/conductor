//! How Conductor runs and stops, as the real program: started on a free port with its own scratch folder, stopped
//! the way systemd and `docker stop` stop it.

use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const EXE: &str = env!("CARGO_BIN_EXE_conductor");

/// A free port on this computer (asked of the system, then let go for the server to take).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// conductor with only these settings: nothing from the environment it was started in.
fn conductor(args: &[&str]) -> Command {
    let mut c = Command::new(EXE);
    c.args(args).stdin(Stdio::null());
    for (k, _) in std::env::vars() {
        if k.starts_with("CONDUCTOR_") || k.starts_with("MAESTRO_") {
            c.env_remove(k);
        }
    }
    c
}

struct Server {
    child: Option<Child>,
    port: u16,
    config: PathBuf,
    data: PathBuf,
    _dir: tempfile::TempDir,
}

impl Server {
    /// A settings file (the listen address only in it) and a server started with it, not yet asked anything.
    fn new() -> Server {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let music = dir.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        let port = free_port();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            format!(
                "data_dir = {:?}\nlisten = \"127.0.0.1:{port}\"\njam_listen = \"\"\ncatalog = false\nlyrics_online = false\n\
                 covers_fetch = false\nconcerts = false\ninfo_online = false\nbackup_keep = 0\n\n\
                 [[libraries]]\nname = \"Music\"\npath = {:?}\n",
                data.to_str().unwrap(),
                music.to_str().unwrap()
            ),
        )
        .unwrap();
        Server { child: None, port, config, data, _dir: dir }
    }

    fn start(&mut self) {
        let child = conductor(&["--config", self.config.to_str().unwrap(), "serve"])
            .env("RUST_LOG", "info")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.up() {
            if let Some(st) = self.child.as_mut().unwrap().try_wait().unwrap() {
                let out = self.child.take().unwrap().wait_with_output().unwrap();
                panic!("conductor stopped at start ({st}): {}", String::from_utf8_lossy(&out.stderr));
            }
            assert!(Instant::now() < deadline, "conductor didn't answer on port {}", self.port);
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn up(&self) -> bool {
        get(self.port, "/health", &[]).is_some_and(|(code, _)| code == 200)
    }

    /// Stopped as systemd and `docker stop` do (SIGTERM; elsewhere, killed), then what it wrote.
    fn stop(&mut self) -> (std::process::ExitStatus, String) {
        let mut child = self.child.take().unwrap();
        #[cfg(unix)]
        unsafe {
            libc::kill(child.id() as i32, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("conductor didn't stop within 20 s of being asked to");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = child.wait_with_output().unwrap();
        (out.status, String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// A plain HTTP GET (status and body), so these tests need no async runtime.
fn get(port: u16, path: &str, headers: &[(&str, &str)]) -> Option<(u16, String)> {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    let extra: String = headers.iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
    write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{extra}Connection: close\r\n\r\n").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    let code = buf.split(' ').nth(1)?.parse().ok()?;
    Some((code, buf.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default()))
}

fn wal(data: &Path) -> u64 {
    std::fs::metadata(data.join("conductor.db-wal")).map(|m| m.len()).unwrap_or(0)
}

#[cfg(unix)]
#[test]
fn sigterm_stops_it_cleanly() {
    // systemd and `docker stop` send SIGTERM: it was killed at once (only Ctrl-C stopped it cleanly); now it stops
    // as for Ctrl-C: it stops listening, finishes what it's doing, and closes the database
    let mut s = Server::new();
    s.start();
    // a request that never ends (half its headers sent) doesn't keep it from stopping: a few seconds, then it goes
    let mut slow = std::net::TcpStream::connect(("127.0.0.1", s.port)).unwrap();
    std::io::Write::write_all(&mut slow, b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let t = Instant::now();
    let (status, out) = s.stop();
    assert!(t.elapsed() < Duration::from_secs(9), "within docker stop's 10 s: {:?}", t.elapsed());
    drop(slow);
    assert!(status.success(), "stopped cleanly, not killed: {status} {out}");
    assert!(out.contains("Conductor stopped"), "{out}");
    assert_eq!(wal(&s.data), 0, "the database was closed: everything is in conductor.db");
    assert!(get(s.port, "/health", &[]).is_none(), "nothing listens any more");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn settings_saved_at_once_are_all_kept() {
    // two admins saving in Podium at the same moment: each save read config.toml, changed one thing and wrote it all
    // back (truncated first), so one change could be lost, or a save could read a file half written
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let config = dir.path().join("config.toml");
    std::fs::write(&config, format!("data_dir = {:?}\njam_listen = \"\"\ncatalog = false\n", data.to_str().unwrap())).unwrap();
    let cfg = conductor::config::Config::load(Some(config.clone())).unwrap();
    let pool = conductor::db::open(&data.join("conductor.db")).await.unwrap();
    let secrets = conductor::auth::Secrets::load_or_create(&data.join("secret.key")).unwrap();
    conductor::auth::create_user(&pool, &secrets, "boss", "boss-pass-1", true).await.unwrap();
    let st = std::sync::Arc::new(conductor::api::AppState::new(pool, secrets, cfg, vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = conductor::api::router(st);
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    let http = reqwest::Client::builder().cookie_store(true).build().unwrap();
    let r = http
        .post(format!("{base}/admin/api/login"))
        .header("x-conductor", "1")
        .json(&serde_json::json!({"username": "boss", "password": "boss-pass-1"}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.text().await.unwrap());
    let numbers = [
        "scan_interval_min",
        "transcode_slots",
        "transcode_cache_mb",
        "backup_keep",
        "podcast_keep",
        "podcast_clean_days",
        "trial_per_day",
        "lidarr_interval_min",
        "lidarr_albums_per_run",
        "vpn_check_secs",
    ];
    let secrets = ["lastfm_api_key", "listenbrainz_token", "audiomuse_token"];
    let mut saves = vec![];
    for round in 0..4 {
        for (i, name) in numbers.iter().chain(&secrets).enumerate() {
            let value = if secrets.contains(name) { serde_json::json!(format!("key-{round}-{i}")) } else { serde_json::json!(round * 100 + i + 1) };
            let rq = http
                .post(format!("{base}/admin/api/settings"))
                .header("x-conductor", "1")
                .json(&serde_json::json!({"name": name, "value": value}));
            saves.push(tokio::spawn(async move { rq.send().await.unwrap() }));
        }
        for s in saves.drain(..) {
            let r = s.await.unwrap();
            let code = r.status();
            assert!(code.is_success(), "{code} {}", r.text().await.unwrap());
        }
        let c = conductor::config::Config::load(Some(config.clone())).unwrap();
        let t: toml::Table = toml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        for (i, name) in numbers.iter().enumerate() {
            assert_eq!(t.get(*name).and_then(|v| v.as_integer()), Some((round * 100 + i + 1) as i64), "{name} kept (round {round})");
        }
        assert_eq!(c.lastfm_api_key.as_deref(), Some(format!("key-{round}-{}", numbers.len()).as_str()), "a secret kept");
        assert_eq!(c.data_dir, data, "what was there stays");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sp = conductor::config::secrets_path(&config);
        assert_eq!(std::fs::metadata(&sp).unwrap().permissions().mode() & 0o777, 0o600, "secrets.toml stays private");
    }
    let left: Vec<_> = std::fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).filter(|n| n.to_string_lossy().ends_with(".tmp")).collect();
    assert!(left.is_empty(), "no temporary files left: {left:?}");
}

#[test]
fn the_health_check_asks_the_address_the_settings_give() {
    // Docker's health check asked port 4533 (or CONDUCTOR_LISTEN's): with the address only in config.toml (Prelude's
    // compose file clears CONDUCTOR_LISTEN) it asked the wrong port. `conductor health` reads the settings as serve does
    let mut s = Server::new();
    let cfg = s.config.to_str().unwrap().to_string();
    let o = conductor(&["--config", &cfg, "health"]).output().unwrap();
    assert!(!o.status.success(), "nothing running there: unhealthy");
    assert!(!s.data.exists(), "the check opens nothing (no data folder or database made)");
    s.start();
    let o = conductor(&["--config", &cfg, "health"]).output().unwrap();
    assert!(o.status.success(), "{o:?}");
    // as in Prelude's compose file: the settings from CONDUCTOR_CONFIG, the image's CONDUCTOR_LISTEN cleared
    let o = conductor(&["health"]).env("CONDUCTOR_CONFIG", &cfg).env("CONDUCTOR_LISTEN", "").env("CONDUCTOR_DATA_DIR", "").output().unwrap();
    assert!(o.status.success(), "{o:?}");
    // CONDUCTOR_LISTEN wins over the file, for the check as for the server
    let o = conductor(&["health"]).env("CONDUCTOR_CONFIG", &cfg).env("CONDUCTOR_LISTEN", format!("127.0.0.1:{}", free_port())).output().unwrap();
    assert!(!o.status.success(), "{o:?}");
    s.stop();
    let o = conductor(&["--config", &cfg, "health"]).output().unwrap();
    assert!(!o.status.success(), "stopped: unhealthy");
}

#[test]
fn the_setup_link_is_shown_but_not_written_to_logs() {
    // the one-time /setup link (its token lets whoever has it make the first admin) went to the journal, docker logs
    // and conductor.log. Not on a terminal (as here, and under systemd, Docker or the Windows service) only where to
    // find it is written; `conductor setup link` shows it
    let mut s = Server::new();
    let cfg = s.config.to_str().unwrap().to_string();
    s.start();
    let o = conductor(&["--config", &cfg, "setup", "link"]).output().unwrap();
    let out = String::from_utf8_lossy(&o.stdout).to_string();
    assert!(o.status.success(), "{o:?}");
    let token = out.split("token=").nth(1).map(|t| t.chars().take_while(char::is_ascii_alphanumeric).collect::<String>()).unwrap_or_default();
    assert!(token.len() >= 16, "{out}");
    // the link opens /setup from elsewhere (here through a proxy: not "this computer"); another token doesn't
    let away = [("X-Forwarded-For", "192.0.2.9")];
    assert_eq!(get(s.port, &format!("/setup?token={token}"), &away).map(|r| r.0), Some(200));
    assert_eq!(get(s.port, "/setup?token=0123456789abcdef0123456789abcdef", &away).map(|r| r.0), Some(404));
    let (_, log) = s.stop();
    assert!(log.contains("conductor setup link"), "it says where the link is: {log}");
    assert!(!log.contains(&token), "the token isn't in the log: {log}");
    // stopped: there's no link to show
    let o = conductor(&["--config", &cfg, "setup", "link"]).output().unwrap();
    assert!(!o.status.success(), "{o:?}");
}
