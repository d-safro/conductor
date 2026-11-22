//! Prelude, the setup wizard: scripted answers for every path (the VPN choices included), `--yes` with flags, and
//! secrets kept out of config.toml and off the screen.

use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

use conductor::prelude::{
    self,
    ui::{Auto, Term},
};
use serde_json::{Value, json};

/// A stand-in HTTP service: path → JSON (404 when None).
async fn stand_in(answer: impl Fn(&str) -> Option<Value> + Clone + Send + Sync + 'static) -> String {
    use axum::{extract::Request, response::IntoResponse};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
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
    let input = lines.join("\n") + "\n";
    let mut out = Vec::new();
    {
        let mut ui = Term { input: Cursor::new(input.into_bytes()), out: &mut out, hide: false };
        prelude::run(Some(config.to_path_buf()), args, &mut ui).await.unwrap();
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
            "1", &base, "the-slskd-key", p.root.join("dl").to_str().unwrap(), // my slskd
            "1", &format!("{base}/check"), "", "y", // already behind a VPN; the kill switch
            "n", "n", "n", "n", "n", "n", "n", "n", // no other services, no AI
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
    let dir = p.root.join("data/downloads-vpn");
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
        assert!(read(&p.root.join("data/downloads-vpn/compose.yml")).contains("network_mode: service:gluetun"));
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
