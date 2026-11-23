//! A read-only server doesn't search Soulseek for a song (`searchRequestFiles`, Podium's search): nothing found
//! could be picked there. A stand-in for slskd that counts what it's asked, a request put straight into the database.
//! (Searching on a server that downloads is in tests/requests.rs.)

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use conductor::{
    api::AppState,
    auth::Secrets,
    catalog::requests::{self, Refusal},
    config::Config,
    db,
};
use serde_json::json;

/// slskd, logged in; every call counted.
async fn fake_slskd(calls: Arc<AtomicUsize>) -> String {
    use axum::{extract::Request, response::IntoResponse};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let app = axum::Router::new().fallback(move |req: Request| {
        let calls = calls.clone();
        async move {
            calls.fetch_add(1, Ordering::SeqCst);
            let v = match req.uri().path() {
                "/api/v0/application" => json!({ "server": { "isLoggedIn": true } }),
                p if p.ends_with("/responses") => json!([]),
                p if p.starts_with("/api/v0/searches/") => json!({ "isComplete": true }),
                _ => json!({}),
            };
            axum::Json(v).into_response()
        }
    });
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    base
}

async fn state(read_only: bool, calls: Arc<AtomicUsize>) -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config { data_dir: dir.path().join("data"), ..Config::default() };
    cfg.catalog = false;
    cfg.deezer_url = "http://127.0.0.1:9".into();
    cfg.read_only = read_only;
    cfg.slskd_url = Some(fake_slskd(calls).await);
    cfg.slskd_api_key = Some("k".into());
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    let pool = db::open(&cfg.data_dir.join("conductor.db")).await.unwrap();
    let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key")).unwrap();
    sqlx::query("INSERT INTO request (key, title, artists_json, status, asked_at) VALUES ('dz-t-1', 'Four', '[\"Alpha\"]', 'notfound', 1)")
        .execute(&pool)
        .await
        .unwrap();
    (Arc::new(AppState::new(pool, secrets, cfg, vec![])), dir)
}

#[tokio::test]
async fn read_only_refuses_before_asking_soulseek() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (st, _dir) = state(true, calls.clone()).await;
    match requests::search_files(&st, "dz-t-1", "alpha four").await {
        Err(Refusal::Conflict(m)) => assert!(m.contains("read-only"), "{m}"),
        other => panic!("refused as read-only, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0, "slskd isn't asked anything");
    // pick_file has always refused: the two agree
    assert!(matches!(requests::pick_file(&st, "dz-t-1", "peer", "x.flac", 1).await, Err(Refusal::Conflict(_))));
}
