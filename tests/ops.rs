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
