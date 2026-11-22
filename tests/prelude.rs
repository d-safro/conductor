//! Prelude, the setup wizard: scripted answers for every path (the VPN choices included), `--yes` with flags, and
//! secrets kept out of config.toml and off the screen.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use conductor::prelude::{
    self,
    drives::Drive,
    ui::{Auto, Term},
};
use serde_json::{Value, json};

/// A stand-in HTTP service: path → JSON (404 when None).
async fn stand_in(answer: impl Fn(&str) -> Option<Value> + Clone + Send + Sync + 'static) -> String {
    serve_on(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap(), answer)
}

fn serve_on(l: tokio::net::TcpListener, answer: impl Fn(&str) -> Option<Value> + Clone + Send + Sync + 'static) -> String {
    use axum::{extract::Request, response::IntoResponse};
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = axum::Router::new().fallback(move |req: Request| {
        let answer = answer.clone();
        async move {
            match answer(req.uri().path()) {
                Some(v) => axum::Json(v).into_response(),
                None => (axum::http::StatusCode::NOT_FOUND, "{}").into_response(),
            }
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

/// The services the wizard talks to: slskd, the VPN check (an exit address) and "this computer's" address.
async fn services() -> String {
    stand_in(|p| match p {
        "/api/v0/application" => Some(json!({ "server": { "isLoggedIn": true }, "version": { "current": "0.22.0" } })),
        "/check" => Some(json!({ "public_ip": "10.64.0.9", "country": "Iceland" })),
        "/home" => Some(json!({ "ip": "198.51.100.3" })),
        "/ip" => Some(json!({ "ip": "203.0.113.77", "country": "Sweden" })), // what a proxy shows
        _ => None,
    })
    .await
}

/// A stand-in proxy: whatever it's asked through it, it answers with "its" address.
async fn proxy() -> String {
    stand_in(|_| Some(json!({ "ip": "203.0.113.77", "country": "Sweden" }))).await
}

struct Place {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    music: PathBuf,
    config: PathBuf,
}

/// A music folder with two "songs" and a settings file that points the address checks at the stand-in.
fn place(base: &str) -> Place {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let music = root.join("music/Alpha");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::write(music.join("01.flac"), b"x").unwrap();
    std::fs::write(music.join("02.mp3"), b"x").unwrap();
    let config = root.join("data/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, format!("vpn_ip_url = \"{base}/home\"\n")).unwrap();
    Place { _tmp: tmp, music: root.join("music"), config, root }
}

async fn scripted(config: &Path, args: &[&str], lines: &[&str]) -> String {
    // no drives to look at: this computer's own aren't part of the tests
    scripted_with(config, args, lines, vec![]).await
}

async fn scripted_with(config: &Path, args: &[&str], lines: &[&str], drives: Vec<Drive>) -> String {
    let input = lines.join("\n") + "\n";
    let mut out = Vec::new();
    {
        let mut ui = Term { input: Cursor::new(input.into_bytes()), out: &mut out, hide: false };
        prelude::run_with(Some(config.to_path_buf()), args, &mut ui, Some(drives)).await.unwrap();
    }
    String::from_utf8(out).unwrap()
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

#[cfg(unix)]
fn private(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o077 == 0
}
#[cfg(not(unix))]
fn private(_: &Path) -> bool {
    true
}

#[tokio::test]
async fn every_step_in_a_terminal() {
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    let out = scripted(
        &p.config,
        &[],
        &[
            "1",                                // everything
            p.music.to_str().unwrap(), "", "", // music folder, no sentinel, no more folders
            data.to_str().unwrap(), "127.0.0.1:45931", // data folder, address
            "me", "secret12", "secret12",      // the admin
            "2", &base, "the-slskd-key", p.root.join("dl").to_str().unwrap(), // my own slskd
            "1", &format!("{base}/check"), "", "y", // already behind a VPN; the kill switch
            "", "n", "n", "", "", "", "n", "n", // no other services (Enter: "No"), no AI
            "",                                // not on a domain now
            "", "", "",                        // nothing to import
            "4", "n",                          // no service; no scan now
        ],
    )
    .await;
    assert!(out.contains("✓ 2 songs"), "{out}");
    assert!(out.contains("✓ slskd answers: version 0.22.0, on Soulseek"), "{out}");
    assert!(out.contains("✓ the VPN works") && out.contains("Iceland"), "{out}");
    assert!(out.contains("✓ me can sign in"), "{out}");
    assert!(!out.contains("the-slskd-key") && !out.contains("10.64.0.9") && !out.contains("198.51.100.3"), "no secrets or addresses shown");
    let cfg = read(&p.config);
    assert!(cfg.contains("vpn = \"existing\"") && cfg.contains("downloads_require_vpn = true") && cfg.contains("listen = \"127.0.0.1:45931\""), "{cfg}");
    assert!(!cfg.contains("the-slskd-key"), "secrets aren't in config.toml");
    let secrets = p.config.with_file_name("secrets.toml");
    assert!(read(&secrets).contains("slskd_api_key = \"the-slskd-key\""));
    assert!(private(&secrets));
    // Conductor reads both together
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.slskd_api_key.as_deref(), c.libraries.len(), c.vpn.as_str()), (Some("the-slskd-key"), 1, "existing"));
    let db = conductor::db::open(&data.join("conductor.db")).await.unwrap();
    let admins: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user WHERE is_admin = 1").fetch_one(&db).await.unwrap();
    assert_eq!(admins, 1);
}

#[tokio::test]
async fn each_vpn_choice_again_later() {
    let base = services().await;
    let p = place(&base);
    // a proxy
    let proxy = proxy().await;
    let out = scripted(&p.config, &["vpn"], &["3", &proxy, "n", "y"]).await;
    assert!(out.contains("✓ the VPN works") && out.contains("Sweden"), "{out}");
    assert!(read(&p.config).contains("vpn = \"proxy\""));
    assert!(read(&p.config.with_file_name("secrets.toml")).contains(&format!("proxy = \"{proxy}\"")));
    // no VPN
    scripted(&p.config, &["vpn"], &["4"]).await;
    let cfg = read(&p.config);
    assert!(cfg.contains("vpn = \"none\"") && cfg.contains("downloads_require_vpn = false"), "{cfg}");
    // already behind one, but the check says it isn't (the same address as this computer's)
    let same = stand_in(|path| (path == "/check" || path == "/home").then(|| json!({ "ip": "198.51.100.3" }))).await;
    std::fs::write(&p.config, format!("vpn_ip_url = \"{same}/home\"\n")).unwrap();
    let out = scripted(&p.config, &["vpn"], &["1", &format!("{same}/check"), "", "y"]).await;
    assert!(out.contains("✗ downloads would use this computer's own address"), "{out}");
    // set one up: Linux with Docker only (on Windows Prelude says so and sets nothing up)
    if cfg!(windows) {
        return;
    }
    // a WireGuard .conf (a fake one), written but not started
    let conf = p.root.join("fake.conf");
    std::fs::write(&conf, "[Interface]\nPrivateKey = TestOnlyPrivateKeyNotReal00000000000000000=\nAddress = 10.2.0.2/32\n[Peer]\nPublicKey = TestOnlyPublicKeyNotReal000000000000000000=\nEndpoint = 127.0.0.1:51820\n").unwrap();
    let out = scripted(
        &p.config,
        &["vpn"],
        &["2", "1", conf.to_str().unwrap(), "n", "someone", "soulseek-pw", p.root.join("dl").to_str().unwrap(), "n", "y"],
    )
    .await;
    assert!(out.contains("✓ written"), "{out}");
    assert!(!out.contains("TestOnlyPrivateKey") && !out.contains("soulseek-pw"), "{out}");
    let dir = p.root.join("data/services");
    assert!(read(&dir.join("vpn.env")).contains("WIREGUARD_PRIVATE_KEY=TestOnlyPrivateKeyNotReal"));
    assert!(private(&dir.join("vpn.env")) && private(&dir.join("slskd/slskd.yml")) && private(&dir.join("gluetun/auth/config.toml")));
    let cfg = read(&p.config);
    assert!(cfg.contains("vpn = \"gluetun\"") && cfg.contains("vpn_check_url = \"http://127.0.0.1:") && !cfg.contains("TestOnly"), "{cfg}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.slskd_api_key.as_ref().map(|k| k.len()), Some(64), "a key made for slskd");
    assert!(read(&dir.join("slskd/slskd.yml")).contains(c.slskd_api_key.as_deref().unwrap()), "slskd knows the same key");
    assert!(read(&dir.join("gluetun/auth/config.toml")).contains(c.vpn_check_key.as_deref().unwrap()));
    if prelude::gluetun::has_docker() {
        prelude::gluetun::validate(&dir).expect("docker compose accepts the file");
    }
}

#[tokio::test]
async fn yes_mode_from_flags() {
    let base = services().await;
    let p = place(&base);
    let conf = p.root.join("fake.conf");
    std::fs::write(&conf, "[Interface]\nPrivateKey = FlagTestPrivateKey000000000000000000000000=\nAddress = 10.9.0.2/32\n[Peer]\nPublicKey = FlagTestPublicKey0000000000000000000000000=\nEndpoint = 127.0.0.1:51820\n").unwrap();
    let mut out = Vec::new();
    let dl = p.root.join("dl");
    let data = p.root.join("data");
    let args = [
        "--yes", "--music", p.music.to_str().unwrap(), "--data", data.to_str().unwrap(), "--listen", "127.0.0.1:45932", "--vpn", "wireguard", "--wg-conf", conf.to_str().unwrap(),
        "--slsk-user", "someone", "--slsk-pass", "pw-from-flag", "--downloads-dir", dl.to_str().unwrap(), "--require-vpn",
    ];
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &args, &mut ui).await.unwrap();
    }
    let out = String::from_utf8(out).unwrap();
    assert!(!out.contains("FlagTestPrivateKey") && !out.contains("pw-from-flag"), "{out}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    // a VPN set up with Docker is Linux-only: on Windows Prelude leaves it off (the kill switch still asked for)
    let made = if cfg!(windows) { "none" } else { "gluetun" };
    assert_eq!((c.vpn.as_str(), c.downloads_require_vpn, c.libraries.len()), (made, true, 1));
    if !cfg!(windows) {
        assert!(c.slskd_url.as_deref().unwrap().starts_with("http://127.0.0.1:"));
        assert!(read(&p.root.join("data/services/downloads.yml")).contains("network_mode: service:gluetun"));
    }
    // and a proxy, the same way
    let px = proxy().await;
    let mut out = Vec::new();
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &["--yes", "vpn", "--vpn", "proxy", "--proxy", &px], &mut ui).await.unwrap();
    }
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.vpn.as_str(), c.proxy.as_deref()), ("proxy", Some(px.as_str())));
    assert!(String::from_utf8(out).unwrap().contains("✓ the VPN works"));
}

#[tokio::test]
async fn downloader_container_on_its_own() {
    let base = services().await;
    let p = place(&base);
    let dl = p.root.join("dl");
    let data = p.root.join("data");
    let mut out = Vec::new();
    let args = [
        "--yes", "--music", p.music.to_str().unwrap(), "--data", data.to_str().unwrap(), "--listen", "127.0.0.1:45933", "--downloads", "docker",
        "--slsk-user", "someone", "--slsk-pass", "pw-alone", "--downloads-dir", dl.to_str().unwrap(),
    ];
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &args, &mut ui).await.unwrap();
    }
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("✓ written") && !out.contains("pw-alone"), "{out}");
    let dir = data.join("services");
    let compose = read(&dir.join("downloads.yml"));
    assert!(compose.contains("conductor-slskd") && !compose.contains("gluetun"), "{compose}");
    assert!(!dir.join("vpn.env").exists());
    assert!(private(&dir.join("slskd/slskd.yml")));
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.vpn.as_str(), c.downloads_require_vpn), ("none", false));
    assert!(c.slskd_url.as_deref().unwrap().starts_with("http://127.0.0.1:"));
    assert!(read(&dir.join("slskd/slskd.yml")).contains(c.slskd_api_key.as_deref().unwrap()), "slskd knows the key");
    assert!(c.catalog, "with a downloader, songs you don't have show");
    if prelude::gluetun::has_docker() && !cfg!(windows) {
        prelude::gluetun::validate(&dir).expect("docker compose accepts the file");
    }

    // and without downloads, nothing that can't be played shows
    let mut out = Vec::new();
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &["--yes", "downloads", "--no-downloads"], &mut ui).await.unwrap();
    }
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert!(c.slskd_url.is_none() && !c.catalog);
}

#[tokio::test]
async fn docker_file_uses_the_same_settings() {
    if cfg!(windows) {
        return; // the Docker choice is for Linux
    }
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    let mut out = Vec::new();
    let args = ["--yes", "--music", p.music.to_str().unwrap(), "--data", data.to_str().unwrap(), "--listen", "127.0.0.1:45934", "--service", "docker"];
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &args, &mut ui).await.unwrap();
    }
    let compose = p.config.with_file_name("conductor-compose.yml");
    let text = read(&compose);
    assert!(text.contains("ghcr.io/d-safro/conductor:latest") && text.contains("network_mode: host"), "{text}");
    let music = p.music.to_str().unwrap();
    assert!(text.contains(&format!("\"{music}:{music}")), "the music at the same place: {text}");
    assert!(text.contains(&format!("CONDUCTOR_CONFIG: \"{}\"", p.config.display())), "{text}");
    if prelude::gluetun::has_docker() {
        let ok = std::process::Command::new("docker").args(["compose", "-f"]).arg(&compose).args(["config", "-q"]).status().unwrap().success();
        assert!(ok, "docker compose accepts it");
    }
}

#[tokio::test]
async fn sets_up_the_whole_music_lab() {
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    let dl = p.root.join("dl");
    let mut out = Vec::new();
    let args = [
        "--yes", "--music", p.music.to_str().unwrap(), "--data", data.to_str().unwrap(), "--listen", "0.0.0.0:45935",
        "--downloads", "docker", "--slsk-user", "someone", "--slsk-pass", "pw-lab", "--downloads-dir", dl.to_str().unwrap(),
        "--docker", "lidarr,audiomuse,pitch,metronome,ai", "--ai",
    ];
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &args, &mut ui).await.unwrap();
    }
    let out = String::from_utf8(out).unwrap();
    assert!(!out.contains("pw-lab"), "{out}");
    assert!(out.contains("your 2 songs") && out.contains("its limit: 768 MB"), "Pitch's memory fits the library: {out}");
    let dir = data.join("services");
    let main = read(&dir.join("compose.yml"));
    for part in ["downloads", "lidarr", "audiomuse", "pitch", "metronome", "ai"] {
        assert!(main.contains(&format!("- {part}.yml")), "{part} in {main}");
    }
    assert!(dir.join("pitch/song_id.py").is_file() && dir.join("metronome/mix_analyzer.py").is_file() && dir.join("voice/dj_voice.py").is_file());
    assert!(private(&dir.join("lidarr.env")) && private(&dir.join("audiomuse.env")));
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    let local = |u: &Option<String>| u.as_deref().is_some_and(|u| u.starts_with("http://127.0.0.1:"));
    assert!(local(&c.slskd_url) && local(&c.lidarr_url) && local(&c.audiomuse_url) && local(&c.songid_url) && local(&c.ollama_url) && local(&c.dj_voice_url), "{c:?}");
    assert!(read(&dir.join("lidarr.env")).contains(c.lidarr_api_key.as_deref().unwrap()), "Lidarr and Conductor share its key");
    let am = read(&dir.join("audiomuse.env"));
    assert!(am.contains(&format!("API_TOKEN={}", c.audiomuse_token.as_deref().unwrap())) && am.contains("NAVIDROME_URL=http://host.docker.internal:45935"), "{am}");
    assert_eq!(c.mixinfo_dir.as_deref(), Some(data.join("mixinfo").as_path()));
    // AudioMuse reads the library with its own account, not an admin, and its password works
    let pass = am.lines().find_map(|l| l.strip_prefix("NAVIDROME_PASSWORD=")).unwrap().to_string();
    let db = conductor::db::open(&data.join("conductor.db")).await.unwrap();
    let secrets = conductor::auth::Secrets::load_or_create(&data.join("secret.key")).unwrap();
    let u = conductor::auth::verify_password(&db, &secrets, "audiomuse", &pass).await.unwrap().expect("its account");
    assert!(!u.is_admin);
    // every port its own
    let ports: Vec<u16> = [&c.slskd_url, &c.lidarr_url, &c.audiomuse_url, &c.songid_url, &c.dj_voice_url]
        .iter()
        .map(|u| u.as_deref().unwrap().rsplit(':').next().unwrap().parse().unwrap())
        .collect();
    let mut uniq = ports.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), ports.len(), "{ports:?}");
    if prelude::gluetun::has_docker() && !cfg!(windows) {
        prelude::stack::validate(&dir).expect("docker compose accepts it all");
    }

    // and taking one out again leaves the rest
    let mut out = Vec::new();
    {
        let mut ui = Auto { out: &mut out };
        prelude::run(Some(p.config.clone()), &["--yes", "services", "--docker", "lidarr,audiomuse,metronome,ai", "--ai"], &mut ui).await.unwrap();
    }
    let main = read(&dir.join("compose.yml"));
    assert!(!main.contains("pitch.yml") && main.contains("lidarr.yml") && main.contains("downloads.yml"), "{main}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert!(c.songid_url.is_none());
}

async fn yes(config: &Path, args: &[&str]) -> String {
    yes_with(config, args, vec![]).await
}

async fn yes_with(config: &Path, args: &[&str], drives: Vec<Drive>) -> String {
    let mut out = Vec::new();
    {
        let mut ui = Auto { out: &mut out };
        prelude::run_with(Some(config.to_path_buf()), args, &mut ui, Some(drives)).await.unwrap();
    }
    String::from_utf8(out).unwrap()
}

fn port_of(u: &Option<String>) -> u16 {
    let after = u.as_deref().unwrap().rsplit(':').next().unwrap();
    after.split('/').next().unwrap().parse().unwrap()
}

#[tokio::test]
async fn a_run_again_keeps_what_is_running() {
    if cfg!(windows) {
        return; // the VPN container is for Linux
    }
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    let dir = data.join("services");
    let conf = p.root.join("fake.conf");
    std::fs::write(&conf, "[Interface]\nPrivateKey = KeepTestPrivateKey000000000000000000000000=\nAddress = 10.9.0.2/32\n[Peer]\nPublicKey = KeepTestPublicKey0000000000000000000000000=\nEndpoint = 127.0.0.1:51820\n").unwrap();
    let dl = p.root.join("dl");
    yes(
        &p.config,
        &[
            "--yes", "--music", p.music.to_str().unwrap(), "--data", data.to_str().unwrap(), "--listen", "0.0.0.0:45936", "--vpn", "wireguard", "--wg-conf", conf.to_str().unwrap(),
            "--slsk-user", "someone", "--slsk-pass", "pw-keep", "--downloads-dir", dl.to_str().unwrap(), "--require-vpn", "--docker", "lidarr,audiomuse,pitch,ai", "--ai",
        ],
    )
    .await;
    let first = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(first.vpn, "gluetun");
    let files = |names: &[&str]| names.iter().map(|n| read(&dir.join(n))).collect::<Vec<_>>();
    let kept = ["vpn.env", "slskd/slskd.yml", "gluetun/auth/config.toml", "audiomuse.env", "lidarr.env"];
    let before = files(&kept);
    // their containers are running: each holds its port (Ollama answers, as it would)
    let urls = [&first.slskd_url, &first.vpn_check_url, &first.lidarr_url, &first.audiomuse_url, &first.songid_url, &first.dj_voice_url];
    let _held: Vec<_> = urls.iter().filter_map(|u| std::net::TcpListener::bind(("127.0.0.1", port_of(u))).ok()).collect();
    let ollama = port_of(&first.ollama_url);
    if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", ollama)).await {
        serve_on(l, |p| (p == "/api/tags").then(|| json!({ "models": [] })));
    }

    // only the downloads step, "set it up for me" again: it stays inside the VPN
    let out = yes(&p.config, &["--yes", "downloads", "--downloads", "docker"]).await;
    assert!(out.contains("stays inside your VPN"), "{out}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.vpn.as_str(), c.downloads_require_vpn, c.vpn_port_sync), ("gluetun", true, first.vpn_port_sync));
    assert!(read(&dir.join("downloads.yml")).contains("network_mode: service:gluetun"));
    assert_eq!((&c.slskd_url, &c.slskd_api_key, &c.vpn_check_url, &c.vpn_check_key), (&first.slskd_url, &first.slskd_api_key, &first.vpn_check_url, &first.vpn_check_key));
    assert_eq!(files(&kept), before, "the same keys and logins");

    // the VPN step again, asking nothing: the current VPN is kept (no .conf needed)
    yes(&p.config, &["--yes", "vpn"]).await;
    // and in a terminal: Enter keeps it; a .conf left blank doesn't end the wizard; blank Soulseek answers keep slskd's
    let out = scripted(&p.config, &["vpn"], &["", "", "", "", "", "", "n", ""]).await;
    assert!(out.contains("Keep the current VPN"), "{out}");
    let out = scripted(&p.config, &["vpn"], &["", "2", "", "", "", "", "", "n", ""]).await;
    assert!(out.contains("✗ no .conf given: the current VPN stays"), "{out}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.vpn.as_str(), &c.slskd_url, &c.slskd_api_key), ("gluetun", &first.slskd_url, &first.slskd_api_key));
    assert_eq!(files(&kept), before);
    assert!(read(&dir.join("slskd/slskd.yml")).contains("pw-keep"));

    // the services again: the same ports, the same keys, our own Ollama still ours
    yes(&p.config, &["--yes", "services", "--docker", "lidarr,audiomuse,pitch,ai", "--ai"]).await;
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(
        (&c.lidarr_url, &c.lidarr_api_key, &c.audiomuse_url, &c.audiomuse_token, &c.songid_url, &c.ollama_url, &c.dj_voice_url),
        (&first.lidarr_url, &first.lidarr_api_key, &first.audiomuse_url, &first.audiomuse_token, &first.songid_url, &first.ollama_url, &first.dj_voice_url)
    );
    assert_eq!(files(&kept), before, "AudioMuse's database password, its token and logins, Lidarr's key");
    assert!(read(&dir.join("ai.yml")).contains("conductor-ollama"), "our own Ollama stays in ai.yml");
    if prelude::gluetun::has_docker() {
        prelude::stack::validate(&dir).expect("docker compose accepts it");
    }
}

#[tokio::test]
async fn a_provider_that_needs_its_address() {
    if cfg!(windows) {
        return;
    }
    let base = services().await;
    let p = place(&base);
    let dir = p.root.join("data/services");
    let dl = p.root.join("dl");
    let args = ["--yes", "vpn", "--vpn", "provider", "--vpn-provider", "mullvad", "--vpn-key", "ProviderTestKey", "--slsk-user", "someone", "--slsk-pass", "pw", "--downloads-dir", dl.to_str().unwrap()];
    let out = yes(&p.config, &args).await;
    assert!(out.contains("mullvad needs its WireGuard address too (--vpn-address)"), "{out}");
    assert!(!dir.join("vpn.env").exists(), "nothing half set up");
    let mut with = args.to_vec();
    with.extend(["--vpn-address", "10.64.0.2/32"]);
    let out = yes(&p.config, &with).await;
    assert!(!out.contains("ProviderTestKey"), "{out}");
    let env = read(&dir.join("vpn.env"));
    assert!(env.contains("VPN_SERVICE_PROVIDER=mullvad") && env.contains("WIREGUARD_ADDRESSES=10.64.0.2/32"), "{env}");
    assert!(!read(&dir.join("downloads.yml")).contains("VPN_PORT_FORWARDING: \"on\""), "mullvad has no port forwarding");
}

#[tokio::test]
async fn yes_adds_the_admin_it_is_given() {
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    yes(&p.config, &["--yes", "server", "--data", data.to_str().unwrap(), "--listen", "127.0.0.1:45937"]).await;
    scripted(&p.config, &["admin"], &["me", "secret12", "secret12"]).await;
    // `conductor setup admin --yes --admin NAME`, the password from the environment
    let run = || {
        let o = std::process::Command::new(env!("CARGO_BIN_EXE_conductor"))
            .args(["--config", p.config.to_str().unwrap(), "setup", "admin", "--yes", "--admin", "second"])
            .env("CONDUCTOR_PASSWORD", "second-pw-1")
            .output()
            .unwrap();
        assert!(o.status.success(), "{o:?}");
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    let out = run();
    assert!(out.contains("✓ second can sign in") && !out.contains("second-pw-1"), "{out}");
    assert!(run().contains("✓ second is an admin already"));
    let db = conductor::db::open(&data.join("conductor.db")).await.unwrap();
    let admins: Vec<String> = sqlx::query_scalar("SELECT name FROM user WHERE is_admin = 1 ORDER BY name").fetch_all(&db).await.unwrap();
    assert_eq!(admins, ["me", "second"]);
}

#[tokio::test]
async fn user_add_if_missing_leaves_one_that_is_there() {
    // install.ps1 run again (an upgrade) with -Admin: the admin is there already, and that isn't an error
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(&config, format!("data_dir = {:?}\n", tmp.path().to_str().unwrap())).unwrap();
    let add = |pw: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_conductor"))
            .args(["--config", config.to_str().unwrap(), "user", "add", "me", "--admin", "--if-missing"])
            .env("CONDUCTOR_PASSWORD", pw)
            .output()
            .unwrap()
    };
    assert!(add("first-pw-1").status.success());
    let again = add("second-pw-2");
    assert!(again.status.success() && String::from_utf8_lossy(&again.stdout).contains("there already"), "{again:?}");
    let db = conductor::db::open(&tmp.path().join("conductor.db")).await.unwrap();
    let secrets = conductor::auth::Secrets::load_or_create(&tmp.path().join("secret.key")).unwrap();
    assert!(conductor::auth::verify_password(&db, &secrets, "me", "first-pw-1").await.unwrap().is_some(), "its password unchanged");
}

#[tokio::test]
async fn on_a_domain_with_caddy_then_a_tunnel() {
    let base = services().await;
    let p = place(&base);
    let dir = p.root.join("data/services");
    // (a) your own domain: Caddy in front, HTTPS, Jam at /jam/
    let out = yes(&p.config, &["--yes", "domain", "--domain", "music.invalid", "--domain-email", "me@example.com"]).await;
    assert!(out.contains("music.invalid doesn't point anywhere yet") && out.contains("forward those two"), "{out}");
    let main = read(&dir.join("compose.yml"));
    assert!(main.contains("- caddy.yml"), "{main}");
    let yml = read(&dir.join("caddy.yml"));
    assert!(yml.contains("image: caddy:2") && yml.contains("network_mode: host") && yml.contains("./caddy/data:/data"), "{yml}");
    let caddyfile = read(&dir.join("caddy/Caddyfile"));
    for want in ["email me@example.com", "music.invalid {", "handle /jam/*", "reverse_proxy 127.0.0.1:4535", "reverse_proxy 127.0.0.1:4533"] {
        assert!(caddyfile.contains(want), "{want} in {caddyfile}");
    }
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.jam_public_url.as_deref(), c.listen.to_string()), (Some("https://music.invalid"), "127.0.0.1:4533".to_string()), "only Caddy reaches it");
    if prelude::gluetun::has_docker() && !cfg!(windows) {
        prelude::stack::validate(&dir).expect("docker compose accepts it");
    }
    // a second name for Jam; and run again with nothing given, it stays
    yes(&p.config, &["--yes", "domain", "--domain", "music.invalid", "--jam-domain", "jam.music.invalid"]).await;
    yes(&p.config, &["--yes", "domain"]).await;
    let caddyfile = read(&dir.join("caddy/Caddyfile"));
    assert!(caddyfile.contains("jam.music.invalid {") && caddyfile.contains("music.invalid {") && caddyfile.contains("email me@example.com"), "{caddyfile}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.jam_public_url.as_deref(), Some("https://jam.music.invalid"));

    // (b) a Cloudflare Tunnel instead: the token only in tunnel.env
    let token = "TunnelTokenForTestsOnly0000";
    let out = yes(&p.config, &["--yes", "domain", "--tunnel-token", token, "--tunnel-host", "music.example.com"]).await;
    assert!(!out.contains(token), "{out}");
    assert!(out.contains("http://127.0.0.1:4535") && out.contains("http://127.0.0.1:4533"), "where the hostname goes: {out}");
    let env = dir.join("tunnel.env");
    assert!(read(&env).contains(&format!("TUNNEL_TOKEN={token}")) && private(&env));
    for f in [p.config.clone(), p.config.with_file_name("secrets.toml"), dir.join("tunnel.yml"), dir.join("compose.yml")] {
        assert!(!read(&f).contains(token), "the token isn't in {}", f.display());
    }
    let yml = read(&dir.join("tunnel.yml"));
    assert!(yml.contains("cloudflare/cloudflared") && yml.contains("tunnel --no-autoupdate run") && yml.contains("env_file: [tunnel.env]") && yml.contains("network_mode: host"), "{yml}");
    assert!(!dir.join("caddy.yml").exists(), "Caddy is out");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.jam_public_url.as_deref(), Some("https://music.example.com"));
    if prelude::gluetun::has_docker() && !cfg!(windows) {
        prelude::stack::validate(&dir).expect("docker compose accepts it");
    }
    // and out again
    yes(&p.config, &["--yes", "domain", "--no-domain"]).await;
    assert!(!dir.join("tunnel.yml").exists() && !read(&dir.join("compose.yml")).contains("tunnel"));
}

#[tokio::test]
async fn caddy_points_at_where_conductor_listens() {
    let base = services().await;
    let p = place(&base);
    let dir = p.root.join("data/services");
    // Conductor on one of this computer's addresses: "only Caddy" moves it to 127.0.0.1, and Caddy points there
    std::fs::write(&p.config, format!("{}listen = \"192.0.2.10:4533\"\n", read(&p.config))).unwrap();
    let out = yes(&p.config, &["--yes", "domain", "--domain", "music.invalid", "--domain-email", "me@example.com }\n:80 {"]).await;
    assert!(out.contains("isn't an email address"), "{out}");
    let caddyfile = read(&dir.join("caddy/Caddyfile"));
    assert!(caddyfile.contains("reverse_proxy 127.0.0.1:4533") && !caddyfile.contains("192.0.2.10"), "{caddyfile}");
    assert!(!caddyfile.contains("email") && !caddyfile.contains(":80"), "{caddyfile}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.listen.to_string(), "127.0.0.1:4533");
    // you open it up again yourself: run again, that stays
    let cfg = read(&p.config).replace("127.0.0.1:4533", "0.0.0.0:4533");
    std::fs::write(&p.config, cfg).unwrap();
    yes(&p.config, &["--yes", "domain"]).await;
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.listen.to_string(), "0.0.0.0:4533");
    assert!(read(&dir.join("caddy/Caddyfile")).contains("reverse_proxy 127.0.0.1:4533"));
}

/// A fake drive: a folder of the test's own.
fn drive(mount: &Path, label: &str, free_gb: u64, usb: bool) -> Drive {
    std::fs::create_dir_all(mount).unwrap();
    Drive {
        mount: std::fs::canonicalize(mount).unwrap(),
        label: Some(label.into()),
        size: (free_gb * 3) << 30,
        free: free_gb << 30,
        removable: usb,
        kind: usb.then(|| "USB".into()),
        system: false,
    }
}

#[tokio::test]
async fn music_auto_finds_the_music_on_the_drives() {
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    let big = drive(&p.root.join("drives/big"), "Data", 900, false);
    let usb = drive(&p.root.join("drives/usb"), "Seagate", 100, true);
    let music = usb.mount.join("Music");
    std::fs::create_dir_all(music.join("Alpha")).unwrap();
    for n in 0..3 {
        std::fs::write(music.join(format!("Alpha/{n}.flac")), b"x").unwrap();
    }
    let out = yes_with(&p.config, &["--yes", "music", "--music", "auto", "--data", data.to_str().unwrap()], vec![big, usb]).await;
    assert!(out.contains("Seagate, USB") && out.contains("drive guard"), "{out}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.libraries.len(), 1);
    assert_eq!(c.libraries[0].path, music, "the folder with the songs");
    assert_eq!(c.libraries[0].sentinel.as_deref(), Some(".library-id"), "on a USB drive, the drive guard");
    assert!(music.join(".library-id").is_file());
    // and --music PATH stays as it was: that folder, no drive guard asked for
    yes_with(&p.config, &["--yes", "music", "--music", p.music.to_str().unwrap()], vec![]).await;
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.libraries[0].path.as_path(), c.libraries[0].sentinel.as_deref()), (p.music.as_path(), None));
}

#[tokio::test]
async fn music_auto_makes_a_folder_on_the_biggest_drive() {
    let base = services().await;
    let p = place(&base);
    let data = p.root.join("data");
    let small = drive(&p.root.join("drives/small"), "Small", 40, false);
    let big = drive(&p.root.join("drives/big"), "Data", 900, false);
    let at = big.mount.clone();
    let args = [
        "--yes", "--music", "auto", "--data", data.to_str().unwrap(), "--listen", "127.0.0.1:45938", "--downloads", "docker", "--slsk-user", "someone", "--slsk-pass", "pw-drive",
    ];
    let out = yes_with(&p.config, &args, vec![small, big]).await;
    assert!(out.contains(&format!("✓ made {}", at.join("Music").display())), "{out}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!(c.libraries[0].path, at.join("Music"));
    assert_eq!(c.libraries[0].sentinel, None, "not a drive that's unplugged");
    // the downloads beside it, on the same drive (not in the data folder)
    assert_eq!(c.slskd_downloads.as_deref(), Some(at.join("Downloads").as_path()));
}

#[tokio::test]
async fn choosing_where_the_music_goes_in_a_terminal() {
    let base = services().await;
    let p = place(&base);
    let a = drive(&p.root.join("drives/a"), "Small", 40, false);
    let usb = drive(&p.root.join("drives/usb"), "Stick", 60, true);
    std::fs::create_dir_all(a.mount.join("Musik")).unwrap();
    std::fs::write(a.mount.join("Musik/1.mp3"), b"x").unwrap();
    let at = usb.mount.clone();
    // the found folder is offered first; choose "make a new one" (2), on the USB drive (the most free: 1), the
    // folder as offered, then the drive guard as offered, and made; no more folders
    let out = scripted_with(&p.config, &["music"], &["2", "1", "", "", "", ""], vec![a, usb]).await;
    assert!(out.contains("is ready for your songs") && !out.contains("Use it anyway"), "{out}");
    assert!(out.contains("Musik (Small) · 1 song · 43 GB free"), "{out}");
    assert!(out.contains("Stick, USB") && out.contains("drive guard"), "{out}");
    let c = conductor::config::Config::load(Some(p.config.clone())).unwrap();
    assert_eq!((c.libraries[0].path.as_path(), c.libraries[0].sentinel.as_deref()), (at.join("Music").as_path(), Some(".library-id")));
    assert!(at.join("Music/.library-id").is_file());
}
