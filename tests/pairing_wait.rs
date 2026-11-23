//! Signing in with your phone (`encore.pairing`): the new device's `pairPoll` is a long-poll, so it hears the
//! approval at once, not on its next look (it has no login, so no event stream: this is why it doesn't need one).
//! A server in-process, an account to approve with.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::Config,
    db,
};
use serde_json::{Value, json};

const USER: &str = "tester";
const PASS: &str = "s3cret pass";

async fn server() -> (String, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config { data_dir: dir.path().join("data"), ..Config::default() };
    cfg.catalog = false;
    cfg.deezer_url = "http://127.0.0.1:9".into();
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
    let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
    auth::create_user(&pool, &secrets, USER, PASS, false).await.unwrap();
    let st = Arc::new(AppState::new(pool, secrets, cfg, vec![]));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = api::router(st);
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.unwrap() });
    (base, dir)
}

#[tokio::test]
async fn a_waiting_device_hears_the_approval_at_once() {
    let (base, _dir) = server().await;
    let http = reqwest::Client::new();
    let get = async |url: String| -> Value { http.get(url).send().await.unwrap().json::<Value>().await.unwrap()["subsonic-response"].clone() };
    let start = get(format!("{base}/rest/pairStart?f=json&c=test&name=Laptop")).await;
    let (code, token) = (start["pair"]["code"].as_str().unwrap().to_string(), start["pair"]["token"].as_str().unwrap().to_string());

    // the new device waits (as Encore does, up to 20 s); the phone approves a moment later
    let waiting = tokio::spawn({
        let (http, base, token) = (http.clone(), base.clone(), token.clone());
        async move {
            let r: Value =
                http.get(format!("{base}/rest/pairPoll?f=json&c=test&token={token}&wait=20")).send().await.unwrap().json().await.unwrap();
            (r["subsonic-response"].clone(), Instant::now())
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let login = json!({ "server": "https://music.example", "user": USER, "pass": PASS }).to_string();
    let mut q = form_urlencoded::Serializer::new(String::new());
    q.extend_pairs([("u", USER), ("p", PASS), ("v", "1.16.1"), ("c", "test"), ("f", "json"), ("code", &code), ("login", &login)]);
    let approved = get(format!("{base}/rest/pairApprove.view?{}", q.finish())).await;
    assert_eq!(approved["status"], "ok", "{approved}");
    let at = Instant::now();
    let (done, answered) = tokio::time::timeout(Duration::from_secs(5), waiting).await.expect("answered well before its 20 s").unwrap();
    assert_eq!(done["pair"]["status"], "done", "{done}");
    assert_eq!(done["pair"]["login"]["user"], USER);
    assert!(answered.saturating_duration_since(at) < Duration::from_secs(2), "woken by the approval");
}
