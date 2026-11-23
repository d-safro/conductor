//! The admin page at /admin (admins only): status, scan, downloads (retry / cancel), picking a Soulseek file by
//! hand for songs the automatic search couldn't get, Lidarr albums, users.
//! Sign-in sets an HttpOnly, SameSite=Strict cookie (kept in memory for 12 hours); every change also needs the
//! `X-Conductor: 1` header, which other sites can't send.

#![allow(clippy::result_large_err)] // handlers return axum Responses as errors

use std::sync::atomic::Ordering;

use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    api::Shared,
    auth::{self, User},
    db::{self, now},
    library,
};

const PAGE: &str = include_str!("../web/admin/index.html");
const COOKIE: &str = "conductor_admin";
const HOURS: i64 = 12;

/// Podium itself: only its own script, never inside another site's frame, never cached.
async fn page() -> Response {
    static C: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| crate::page::csp(PAGE, crate::page::ADMIN_POLICY));
    ([(header::CONTENT_SECURITY_POLICY, C.as_str()), (header::X_FRAME_OPTIONS, "DENY"), (header::CACHE_CONTROL, "no-store")], Html(PAGE))
        .into_response()
}

/// Whether this request came over HTTPS: its own, or (from a proxy on this computer, as Prelude's Caddy, a Cloudflare
/// Tunnel or Tailscale serve are) what the proxy says it was.
fn https(h: &HeaderMap, peer: std::net::SocketAddr) -> bool {
    peer.ip().is_loopback()
        && (h.get("x-forwarded-proto").and_then(|v| v.to_str().ok()).is_some_and(|p| p.eq_ignore_ascii_case("https"))
            || h.get("forwarded").and_then(|v| v.to_str().ok()).is_some_and(|f| f.to_ascii_lowercase().contains("proto=https")))
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/admin", get(page))
        .route("/admin/", get(page))
        .route("/admin/api/login", post(login))
        .route("/admin/api/logout", post(logout))
        .route("/admin/api/settings", get(settings).post(set_setting))
        .route("/admin/api/status", get(status))
        .route("/admin/api/scan", post(scan))
        .route("/admin/api/requests", get(requests))
        .route("/admin/api/requests/{key}/retry", post(retry))
        .route("/admin/api/requests/{key}", delete(cancel))
        .route("/admin/api/pick", get(pick_list))
        .route("/admin/api/pick/{key}/search", post(pick_search))
        .route("/admin/api/pick/{key}/skip", post(pick_skip))
        .route("/admin/api/pick/{key}", post(pick_file))
        .route("/admin/api/import", get(import_list))
        .route("/admin/api/import/upload", post(import_upload))
        .route("/admin/api/import/online", post(import_online))
        .route("/admin/api/import/{id}", get(import_get).delete(import_discard))
        .route("/admin/api/import/{id}/start", post(import_start))
        .route("/admin/api/import/{id}/undo", post(import_undo))
        .route("/admin/api/lidarr", get(lidarr))
        .route("/admin/api/lidarr/{id}/retry", post(lidarr_retry))
        .route("/admin/api/users", get(users).post(add_user))
        .route("/admin/api/users/{name}/password", post(set_password))
        .route("/admin/api/users/{name}/apikey", post(new_api_key))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

fn cookie_of(h: &HeaderMap) -> Option<String> {
    h.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == COOKIE).then(|| v.to_string())
    })
}

/// The signed-in admin. Changes (`write`) also need the X-Conductor header.
async fn admin(st: &Shared, h: &HeaderMap, write: bool) -> Result<User, Response> {
    if write && h.get("x-conductor").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err(err(StatusCode::FORBIDDEN, "missing X-Conductor header"));
    }
    let tok = cookie_of(h).ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sign in"))?;
    let uid = {
        let t = st.native_tokens.lock().await;
        t.get(&format!("admin:{tok}")).filter(|(_, exp)| *exp > now()).map(|(u, _)| u.clone())
    }
    .ok_or_else(|| err(StatusCode::UNAUTHORIZED, "sign in"))?;
    let u: Option<User> = sqlx::query_as("SELECT id, name, password_enc, is_admin FROM user WHERE id = ?")
        .bind(uid)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    match u {
        Some(u) if u.is_admin => Ok(u),
        _ => Err(err(StatusCode::FORBIDDEN, "admins only")),
    }
}

macro_rules! need_admin {
    ($st:expr, $h:expr, $w:expr) => {
        match admin(&$st, &$h, $w).await {
            Ok(u) => u,
            Err(r) => return r,
        }
    };
}

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

async fn login(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    h: HeaderMap,
    Json(b): Json<Login>,
) -> Response {
    if h.get("x-conductor").and_then(|v| v.to_str().ok()) != Some("1") {
        return err(StatusCode::FORBIDDEN, "missing X-Conductor header");
    }
    let ip = crate::jam::client_ip(&h, peer);
    if st.login_fails.blocked("admin", &ip, &b.username, auth::ADMIN_TRIES) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Too many wrong passwords. Try again in a few minutes.");
    }
    let Ok(Some(u)) = auth::verify_password(&st.db, &st.secrets, &b.username, &b.password).await else {
        st.login_fails.failed("admin", &ip, &b.username);
        return err(StatusCode::UNAUTHORIZED, "Wrong username or password");
    };
    st.login_fails.clear("admin", &ip, &b.username);
    if !u.is_admin {
        return err(StatusCode::FORBIDDEN, "This page is for admins");
    }
    let tok = format!("{}{}", db::random_id(), db::random_id());
    st.native_tokens.lock().await.insert(format!("admin:{tok}"), (u.id, now() + HOURS * 3600));
    // Secure when it came over HTTPS (a domain through Prelude's proxy): the browser then never sends it in the clear
    let secure = if https(&h, peer) { "; Secure" } else { "" };
    let c = format!("{COOKIE}={tok}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={}{secure}", HOURS * 3600);
    let mut r = Json(json!({ "ok": true, "name": u.name })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&c).unwrap());
    r
}

async fn logout(State(st): State<Shared>, h: HeaderMap) -> Response {
    if let Some(t) = cookie_of(&h) {
        st.native_tokens.lock().await.remove(&format!("admin:{t}"));
    }
    let mut r = Json(json!({ "ok": true })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("conductor_admin=; Path=/admin; Max-Age=0"));
    r
}

async fn count(st: &Shared, sql: &str) -> i64 {
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string())).fetch_one(&st.db).await.unwrap_or(0)
}

async fn status(State(st): State<Shared>, h: HeaderMap) -> Response {
    let u = need_admin!(st, h, false);
    let libs: Vec<Value> = st
        .libs
        .iter()
        .map(|(id, l)| json!({ "id": id, "name": l.name, "path": l.path.to_string_lossy(), "connected": library::is_online(l) }))
        .collect();
    let by_status: Vec<(String, i64)> =
        sqlx::query_as("SELECT status, COUNT(*) FROM request GROUP BY status").fetch_all(&st.db).await.unwrap_or_default();
    let set = |o: &Option<String>| o.as_deref().is_some_and(|s| !s.is_empty());
    Json(json!({
        "you": u.name, "version": env!("CARGO_PKG_VERSION"), "libraries": libs,
        "songs": count(&st, "SELECT COUNT(*) FROM track WHERE missing_since IS NULL").await,
        "missing": count(&st, "SELECT COUNT(*) FROM track WHERE missing_since IS NOT NULL").await,
        "albums": count(&st, "SELECT COUNT(*) FROM album WHERE song_count > 0").await,
        "artists": count(&st, "SELECT COUNT(*) FROM artist WHERE album_count > 0").await,
        "playlists": count(&st, "SELECT COUNT(*) FROM playlist").await,
        "podcasts": count(&st, "SELECT COUNT(*) FROM podcast").await,
        "scanning": st.scan.scanning.load(Ordering::Relaxed),
        "lastScan": st.scan.last_scan.load(Ordering::Relaxed),
        "requests": by_status.into_iter().map(|(s, n)| (s, json!(n))).collect::<serde_json::Map<_, _>>(),
        "onTrial": count(&st, "SELECT COUNT(*) FROM request WHERE trial = 1").await,
        "lidarr": count(&st, "SELECT COUNT(*) FROM lidarr_grab WHERE status != 'done'").await,
        "vpn": crate::vpn::status(&st), "downloadsPaused": crate::vpn::paused(&st), "requireVpn": st.cfg.downloads_require_vpn,
        "services": { "slskd": set(&st.cfg.slskd_url), "lidarr": set(&st.cfg.lidarr_url), "audiomuse": set(&st.cfg.audiomuse_url),
                      "jam": set(&st.cfg.jam_public_url), "catalog": st.cfg.catalog, "lyrics": st.cfg.lyrics_online, "covers": st.cfg.covers_fetch },
    }))
    .into_response()
}

async fn scan(State(st): State<Shared>, h: HeaderMap) -> Response {
    need_admin!(st, h, true);
    let s = st.clone();
    tokio::spawn(async move {
        let _ = library::scan(&s.db, &s.libs, &s.scan, false).await;
    });
    Json(json!({ "ok": true })).into_response()
}

#[derive(sqlx::FromRow, serde::Serialize)]
struct ReqRow {
    key: String,
    title: String,
    artists_json: String,
    status: String,
    searches: i64,
    trial: bool,
    rel: Option<String>,
    local_id: Option<String>,
    asked_at: i64,
    slsk_user: Option<String>,
}

async fn requests(State(st): State<Shared>, h: HeaderMap) -> Response {
    need_admin!(st, h, false);
    let rows: Vec<ReqRow> = sqlx::query_as(
        "SELECT key, title, artists_json, status, searches, trial, rel, local_id, asked_at, slsk_user FROM request ORDER BY asked_at DESC LIMIT 500",
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    Json(rows).into_response()
}

async fn retry(State(st): State<Shared>, h: HeaderMap, Path(key): Path<String>) -> Response {
    need_admin!(st, h, true);
    let _ = sqlx::query("UPDATE request SET status = 'retry', searches = 0, last_search = 0, tried_json = '[]' WHERE key = ? AND local_id IS NULL")
        .bind(&key)
        .execute(&st.db)
        .await;
    st.acquire_wake.notify_one();
    Json(json!({ "ok": true })).into_response()
}

async fn cancel(State(st): State<Shared>, h: HeaderMap, Path(key): Path<String>) -> Response {
    need_admin!(st, h, true);
    let _ = sqlx::query("DELETE FROM request WHERE key = ? AND local_id IS NULL").bind(&key).execute(&st.db).await;
    Json(json!({ "ok": true })).into_response()
}

// ───────────── picking a file by hand ─────────────

/// Requests the automatic search couldn't get (or that were set aside), and hand-picked ones on their way.
const TO_PICK: &[&str] = &["retry", "notfound"];

fn slskd(st: &Shared) -> Option<crate::catalog::acquire::Slskd> {
    Some(crate::catalog::acquire::Slskd::new(st.cfg.slskd_url.as_deref()?, st.cfg.slskd_api_key.as_deref()?))
}

async fn pick_list(State(st): State<Shared>, h: HeaderMap) -> Response {
    need_admin!(st, h, false);
    type Row = (String, String, String, String, i64, String, bool, i64, bool, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT key, title, artists_json, album, duration_ms, status, found, searches, manual, slsk_user, slsk_file, rel,
                up_user, up_file, tried_json != '[]'
         FROM request WHERE local_id IS NULL AND (status IN ('retry', 'notfound', 'skipped') OR (status = 'queued' AND manual = 1))
            OR status = 'upgrading'
            OR (manual = 1 AND status = 'done' AND arrived_at > ?)
         ORDER BY asked_at DESC LIMIT 500",
    )
    .bind(now() - 7 * 86_400)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    // how the downloads are going, from slskd
    let mut live = std::collections::HashMap::new();
    if rows.iter().any(|r| r.5 == "queued" || r.5 == "upgrading")
        && let Some(sl) = slskd(&st)
        && let Ok(t) = sl.get("/transfers/downloads").await
    {
        for u in t.as_array().into_iter().flatten() {
            for d in u["directories"].as_array().into_iter().flatten() {
                for f in d["files"].as_array().into_iter().flatten() {
                    live.insert(
                        (u["username"].as_str().unwrap_or("").to_string(), f["filename"].as_str().unwrap_or("").to_string()),
                        (f["state"].as_str().unwrap_or("").to_string(), f["percentComplete"].as_f64().unwrap_or(0.0)),
                    );
                }
            }
        }
    }
    let mut out = vec![];
    for (key, title, artists, album, ms, status, found, searches, manual, user, file, rel, up_user, up_file, tried) in rows {
        let names: Vec<String> = serde_json::from_str(&artists).unwrap_or_default();
        let first = names.first().cloned().unwrap_or_default();
        let group = match status.as_str() {
            s if TO_PICK.contains(&s) => "pick",
            "skipped" => "skipped",
            "queued" | "upgrading" => "downloading",
            _ => "done",
        };
        let (u, f) = if status == "upgrading" { (up_user, up_file) } else { (user, file) };
        let (state, percent) = live.get(&(u.clone().unwrap_or_default(), f.clone().unwrap_or_default())).cloned().unwrap_or(("waiting".into(), 0.0));
        out.push(json!({
            "key": key, "title": title, "artist": names.join(", "), "album": album, "ms": ms, "status": status,
            "group": group, "found": found, "searches": searches,
            "query": format!("{first} {}", crate::catalog::text::clean_title(&title)).trim(),
            "note": if group == "pick" && manual && tried > 0 { "the last file you picked didn't arrive; pick another" }
                    else if status == "upgrading" { "swapping in a better copy" } else { "" },
            "user": u, "file": f.map(|f| f.replace('\\', "/").rsplit('/').next().unwrap_or("").to_string()),
            "rel": rel, "state": state, "percent": percent.round(),
        }));
    }
    let count = |g: &str| out.iter().filter(|x| x["group"] == g).count();
    let counts = json!({ "pick": count("pick"), "downloading": count("downloading"), "done": count("done"), "skipped": count("skipped") });
    Json(json!({ "songs": out, "counts": counts, "slskd": slskd(&st).is_some(), "readOnly": st.cfg.read_only })).into_response()
}

async fn pick_req(st: &Shared, key: &str) -> Option<crate::catalog::acquire::Req> {
    sqlx::query_as("SELECT * FROM request WHERE key = ?").bind(key).fetch_optional(&st.db).await.ok().flatten()
}

#[derive(Deserialize)]
struct PickSearch {
    q: String,
}

async fn pick_search(State(st): State<Shared>, h: HeaderMap, Path(key): Path<String>, Json(b): Json<PickSearch>) -> Response {
    need_admin!(st, h, true);
    use crate::catalog::{acquire, text};
    let Some(r) = pick_req(&st, &key).await else { return err(StatusCode::NOT_FOUND, "unknown song") };
    let q = b.q.trim();
    if q.chars().count() < 3 {
        return err(StatusCode::BAD_REQUEST, "type at least 3 letters");
    }
    let Some(sl) = slskd(&st) else { return err(StatusCode::CONFLICT, "slskd isn't set up (slskd_url, slskd_api_key)") };
    if !sl.logged_in().await {
        return err(StatusCode::SERVICE_UNAVAILABLE, "slskd isn't logged in to Soulseek right now (is its VPN up?)");
    }
    let found = match acquire::search(&st, &sl, q).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::BAD_GATEWAY, &format!("Soulseek search failed: {e}")),
    };
    let title = text::norm(&text::clean_title(&r.title));
    let mut rows = vec![];
    for resp in &found {
        for f in resp["files"].as_array().into_iter().flatten() {
            let name = f["filename"].as_str().unwrap_or("").replace('\\', "/");
            let base = name.rsplit('/').next().unwrap_or(&name).to_string();
            let ext = f["extension"].as_str().filter(|e| !e.is_empty()).map(String::from)
                .unwrap_or_else(|| base.rsplit('.').next().unwrap_or("").to_string()).trim_start_matches('.').to_lowercase();
            if !acquire::is_audio(&ext) {
                continue;
            }
            let score = acquire::score_file(&r, resp, f, &[]);
            let title_ok = !title.is_empty() && (text::norm(&base).contains(&title) || text::norm(&text::translit(&base)).contains(&title));
            rows.push(json!({
                "user": resp["username"], "filename": f["filename"], "size": f["size"].as_i64().unwrap_or(0),
                "folder": name.rsplit('/').nth(1).unwrap_or(""), "name": base, "ext": ext,
                "bitrate": f["bitRate"].as_i64().unwrap_or(0), "length": f["length"].as_f64().unwrap_or(0.0),
                "free": resp["hasFreeUploadSlot"] == true, "queue": resp["queueLength"].as_i64().unwrap_or(0),
                "speed": resp["uploadSpeed"].as_i64().unwrap_or(0), "match": score.is_some(), "titleOk": title_ok,
                "artistOk": acquire::artist_in(&r.artists(), &name), "sameAlbum": acquire::same_album(&r, &name) == Some(true),
                "score": score.unwrap_or(-1.0),
            }));
        }
    }
    let rank = |x: &Value| {
        let q = match x["ext"].as_str() { Some("flac") => 3, Some("mp3") => 1, _ => 0 };
        (x["match"] == true, x["titleOk"] == true && x["artistOk"] == true, x["sameAlbum"] == true,
         (x["score"].as_f64().unwrap_or(-1.0) * 100.0) as i64, q, x["bitrate"].as_i64().unwrap_or(0), x["free"] == true)
    };
    rows.sort_by_key(|x| std::cmp::Reverse(rank(x)));
    let total = rows.len();
    rows.truncate(120);
    Json(json!({ "results": rows, "total": total })).into_response()
}

#[derive(Deserialize)]
struct PickFile {
    user: String,
    filename: String,
    size: i64,
}

async fn pick_file(State(st): State<Shared>, h: HeaderMap, Path(key): Path<String>, Json(b): Json<PickFile>) -> Response {
    need_admin!(st, h, true);
    if st.cfg.read_only {
        return err(StatusCode::CONFLICT, "read-only: nothing is downloaded");
    }
    let Some(r) = pick_req(&st, &key).await else { return err(StatusCode::NOT_FOUND, "unknown song") };
    if !matches!(r.status.as_str(), "retry" | "notfound" | "skipped") {
        return err(StatusCode::CONFLICT, "this song isn't waiting for a file");
    }
    let Some(sl) = slskd(&st) else { return err(StatusCode::CONFLICT, "slskd isn't set up") };
    let path = format!("/transfers/downloads/{}", crate::catalog::acquire::urlencode(&b.user));
    if let Err(e) = sl.call(reqwest::Method::POST, &path, Some(json!([{ "filename": b.filename, "size": b.size }]))).await {
        return err(StatusCode::BAD_GATEWAY, &format!("slskd refused it ({e}); that person may be offline, try another file"));
    }
    let same = crate::catalog::acquire::same_album(&r, &b.filename);
    let _ = sqlx::query(
        "UPDATE request SET status = 'queued', slsk_user = ?, slsk_file = ?, since = ?, manual = 1, same_album = ?, skipped_from = NULL
         WHERE key = ?",
    )
    .bind(&b.user)
    .bind(&b.filename)
    .bind(now())
    .bind(same)
    .bind(&key)
    .execute(&st.db)
    .await;
    st.acquire_wake.notify_one();
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct Skip {
    on: bool,
}

/// Set a song aside (no more automatic searches, out of the list to pick), or bring it back.
async fn pick_skip(State(st): State<Shared>, h: HeaderMap, Path(key): Path<String>, Json(b): Json<Skip>) -> Response {
    need_admin!(st, h, true);
    let q = if b.on {
        "UPDATE request SET skipped_from = status, status = 'skipped' WHERE key = ? AND status IN ('retry', 'notfound')"
    } else {
        "UPDATE request SET status = COALESCE(skipped_from, 'notfound'), skipped_from = NULL WHERE key = ? AND status = 'skipped'"
    };
    let _ = sqlx::query(q).bind(&key).execute(&st.db).await;
    Json(json!({ "ok": true })).into_response()
}

// ───────────── imports from other services ─────────────

async fn user_by_name(st: &Shared, name: &str) -> Option<String> {
    sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(name).fetch_optional(&st.db).await.ok().flatten()
}

async fn import_list(State(st): State<Shared>, h: HeaderMap) -> Response {
    need_admin!(st, h, false);
    let names: std::collections::HashMap<String, String> =
        sqlx::query_as("SELECT id, name FROM user").fetch_all(&st.db).await.unwrap_or_default().into_iter().collect();
    let mut all = crate::import::jobs::list(&st, None).await.unwrap_or_default();
    for b in &mut all {
        b["user"] = names.get(b["userId"].as_str().unwrap_or("")).cloned().unwrap_or_default().into();
    }
    let spotify: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM setting WHERE key = 'spotify_token'").fetch_one(&st.db).await.unwrap_or(0);
    Json(json!({ "imports": all, "spotify": spotify > 0, "lastfm": st.cfg.lastfm_api_key.is_some(), "readOnly": st.cfg.read_only })).into_response()
}

#[derive(Deserialize)]
struct UploadQ {
    user: String,
    name: Option<String>,
}

async fn import_upload(
    State(st): State<Shared>,
    h: HeaderMap,
    axum::extract::Query(q): axum::extract::Query<UploadQ>,
    body: axum::body::Body,
) -> Response {
    need_admin!(st, h, true);
    let Some(uid) = user_by_name(&st, &q.user).await else { return err(StatusCode::NOT_FOUND, "no such user") };
    if let Err(e) = crate::import::jobs::room_for_another(&st, &uid).await {
        return err(StatusCode::TOO_MANY_REQUESTS, &format!("{e:#}"));
    }
    let file = match crate::import::jobs::save_upload_stream(&st, q.name.as_deref().unwrap_or("export.zip"), body).await {
        Ok(f) => f,
        Err(e) => return err(StatusCode::BAD_REQUEST, &format!("{e:#}")),
    };
    match crate::import::jobs::create(&st, &uid, json!({ "file": file, "name": q.name })).await {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct OnlineB {
    user: String,
    source: String,
    #[serde(default)]
    account: String,
}

async fn import_online(State(st): State<Shared>, h: HeaderMap, Json(b): Json<OnlineB>) -> Response {
    need_admin!(st, h, true);
    let Some(uid) = user_by_name(&st, &b.user).await else { return err(StatusCode::NOT_FOUND, "no such user") };
    if !["spotify", "lastfm", "deezer"].contains(&b.source.as_str()) {
        return err(StatusCode::BAD_REQUEST, "source is spotify, lastfm or deezer");
    }
    if let Err(e) = crate::import::jobs::room_for_another(&st, &uid).await {
        return err(StatusCode::TOO_MANY_REQUESTS, &format!("{e:#}"));
    }
    match crate::import::jobs::create(&st, &uid, json!({ "online": b.source, "user": b.account.trim() })).await {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    }
}

async fn import_get(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> Response {
    need_admin!(st, h, false);
    match crate::import::jobs::get(&st, &id).await {
        Ok(Some(b)) => Json(crate::import::jobs::batch_json(&b)).into_response(),
        _ => err(StatusCode::NOT_FOUND, "no such import"),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartB {
    kinds: Vec<String>,
    #[serde(default)]
    ask_missing: usize,
}

async fn import_start(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>, Json(b): Json<StartB>) -> Response {
    need_admin!(st, h, true);
    let mut opts = crate::import::apply::Options::only(&b.kinds.join(","));
    opts.ask_missing = b.ask_missing.min(500);
    match crate::import::jobs::start(&st, &id, opts).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, &format!("{e:#}")),
    }
}

async fn import_undo(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> Response {
    need_admin!(st, h, true);
    match crate::import::jobs::undo(&st, &id).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, &format!("{e:#}")),
    }
}

async fn import_discard(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> Response {
    need_admin!(st, h, true);
    match crate::import::jobs::discard(&st, &id).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, &format!("{e:#}")),
    }
}

async fn lidarr(State(st): State<Shared>, h: HeaderMap) -> Response {
    need_admin!(st, h, false);
    type Row = (i64, String, String, String, Option<String>, i64, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT album_id, title, artist, status, slsk_user, searches, error FROM lidarr_grab ORDER BY status, artist",
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    Json(rows.into_iter().map(|(id, t, a, s, u, n, e)| json!({ "id": id, "title": t, "artist": a, "status": s, "user": u, "searches": n, "error": e })).collect::<Vec<_>>())
        .into_response()
}

async fn lidarr_retry(State(st): State<Shared>, h: HeaderMap, Path(id): Path<i64>) -> Response {
    need_admin!(st, h, true);
    let _ = sqlx::query(
        "UPDATE lidarr_grab SET status = 'retry', searches = 0, last_search = 0 WHERE album_id = ? AND status != 'done'",
    )
    .bind(id)
    .execute(&st.db)
    .await;
    Json(json!({ "ok": true })).into_response()
}

async fn users(State(st): State<Shared>, h: HeaderMap) -> Response {
    need_admin!(st, h, false);
    let rows: Vec<(String, bool, i64)> =
        sqlx::query_as("SELECT name, is_admin, created_at FROM user ORDER BY name").fetch_all(&st.db).await.unwrap_or_default();
    Json(rows.into_iter().map(|(n, a, c)| json!({ "name": n, "admin": a, "created": c })).collect::<Vec<_>>()).into_response()
}

#[derive(Deserialize)]
struct NewUser {
    name: String,
    password: String,
    #[serde(default)]
    admin: bool,
}

async fn add_user(State(st): State<Shared>, h: HeaderMap, Json(b): Json<NewUser>) -> Response {
    need_admin!(st, h, true);
    if b.name.trim().is_empty() || b.password.len() < 6 {
        return err(StatusCode::BAD_REQUEST, "A name, and a password of at least 6 characters");
    }
    match auth::create_user(&st.db, &st.secrets, b.name.trim(), &b.password, b.admin).await {
        Ok(_) => Json(json!({ "ok": true })).into_response(),
        Err(_) => err(StatusCode::CONFLICT, "There's already a user with that name"),
    }
}

#[derive(Deserialize)]
struct Password {
    password: String,
}

async fn set_password(State(st): State<Shared>, h: HeaderMap, Path(name): Path<String>, Json(b): Json<Password>) -> Response {
    need_admin!(st, h, true);
    if b.password.len() < 6 {
        return err(StatusCode::BAD_REQUEST, "At least 6 characters");
    }
    match auth::set_password(&st.db, &st.secrets, &name, &b.password).await {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        _ => err(StatusCode::NOT_FOUND, "No such user"),
    }
}

async fn new_api_key(State(st): State<Shared>, h: HeaderMap, Path(name): Path<String>) -> Response {
    need_admin!(st, h, true);
    match auth::create_api_key(&st.db, &name, "admin page").await {
        Ok(k) => Json(json!({ "key": k })).into_response(),
        Err(_) => err(StatusCode::NOT_FOUND, "No such user"),
    }
}

// ── Settings ──

/// The settings reference (docs/CONFIG.md): Podium's Settings tab lists what it lists, as it describes them.
const REFERENCE: &str = include_str!("../docs/CONFIG.md");
/// Changed with `conductor setup`, not here: they move data or music.
const NOT_HERE: &[&str] = &["data_dir", "libraries"];

struct Setting {
    section: String,
    name: String,
    default: String,
    about: String,
}

fn reference() -> Vec<Setting> {
    let mut out = vec![];
    let mut section = String::new();
    for line in REFERENCE.lines() {
        if let Some(t) = line.strip_prefix("## ") {
            section = t.trim().to_string();
        } else if let Some(row) = line.strip_prefix("| `") {
            let cells: Vec<&str> = row.splitn(3, " | ").collect();
            if let [name, default, about] = cells.as_slice() {
                let name = name.trim_end_matches('`').to_string();
                if !NOT_HERE.contains(&name.as_str()) {
                    out.push(Setting { section: section.clone(), name, default: default.trim().into(), about: about.trim_end_matches(" |").trim().into() });
                }
            }
        }
    }
    out
}

/// What kind of value a setting takes, from its default.
fn kind(s: &Setting) -> &'static str {
    if crate::config::SECRET_KEYS.contains(&s.name.as_str()) {
        "secret"
    } else if s.default == "true" || s.default == "false" {
        "bool"
    } else if s.default.parse::<f64>().is_ok() {
        "number"
    } else if s.default.starts_with('[') {
        "list"
    } else {
        "text"
    }
}

fn config_file(st: &Shared) -> std::path::PathBuf {
    st.cfg.config_path.clone().unwrap_or_else(|| st.cfg.data_dir.join("config.toml"))
}

/// Set by the environment (CONDUCTOR_<NAME>, or the old MAESTRO_<NAME>), which wins over the files.
fn from_env(name: &str) -> bool {
    ["CONDUCTOR_", "MAESTRO_"].iter().any(|p| std::env::var(format!("{p}{}", name.to_uppercase())).is_ok_and(|v| !v.trim().is_empty()))
}

async fn settings(State(st): State<Shared>, h: HeaderMap) -> Response {
    if let Err(r) = admin(&st, &h, false).await {
        return r;
    }
    let path = config_file(&st);
    let d = match crate::prelude::Draft::load(&path) {
        Ok(d) => d,
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    };
    let mut sections: Vec<(String, Vec<Value>)> = vec![];
    for s in reference() {
        let k = kind(&s);
        let set = d.cfg.contains_key(&s.name) || d.secrets.contains_key(&s.name);
        // a secret's value is never sent, only whether there is one
        let value = if k == "secret" { Value::Null } else { d.cfg.get(&s.name).and_then(|v| serde_json::to_value(v).ok()).unwrap_or(Value::Null) };
        let row = json!({ "name": s.name, "default": s.default, "about": s.about, "kind": k, "set": set, "value": value, "env": from_env(&s.name) });
        match sections.iter_mut().find(|(t, _)| *t == s.section) {
            Some((_, rows)) => rows.push(row),
            None => sections.push((s.section, vec![row])),
        }
    }
    let sections: Vec<Value> = sections.into_iter().map(|(title, settings)| json!({ "title": title, "settings": settings })).collect();
    Json(json!({ "file": path.display().to_string(), "sections": sections })).into_response()
}

#[derive(Deserialize)]
struct SetSetting {
    name: String,
    /// null (or "") takes it out: the default again
    value: Option<Value>,
}

async fn set_setting(State(st): State<Shared>, h: HeaderMap, Json(b): Json<SetSetting>) -> Response {
    let u = match admin(&st, &h, true).await {
        Ok(u) => u,
        Err(r) => return r,
    };
    let Some(s) = reference().into_iter().find(|s| s.name == b.name) else {
        return err(StatusCode::BAD_REQUEST, "That isn't a setting that can be changed here (music folders and the data folder: conductor setup)");
    };
    let path = config_file(&st);
    let (before, mut d) = match (crate::prelude::Draft::load(&path), crate::prelude::Draft::load(&path)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(e), _) | (_, Err(e)) => return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}")),
    };
    let bad = |m: &str| err(StatusCode::BAD_REQUEST, m);
    match b.value.filter(|v| !v.is_null() && v.as_str() != Some("")) {
        None => d.unset(&s.name),
        Some(v) => match kind(&s) {
            "bool" => match v.as_bool().or_else(|| v.as_str().and_then(|t| t.parse().ok())) {
                Some(x) => d.set(&s.name, x),
                None => return bad("on or off"),
            },
            // a decimal default (trial_hours, lidarr_match_ratio) takes decimals; the rest whole numbers
            "number" if s.default.contains('.') => match v.as_f64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())) {
                Some(x) => d.set(&s.name, x),
                None => return bad("a number"),
            },
            "number" => match v.as_i64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok())) {
                Some(x) => d.set(&s.name, x),
                None => return bad("a whole number"),
            },
            "list" => {
                let items: Vec<String> = match &v {
                    Value::Array(a) => a.iter().filter_map(|x| x.as_str().map(|t| t.trim().to_string())).collect(),
                    Value::String(t) => t.split(',').map(|x| x.trim().to_string()).collect(),
                    _ => return bad("a list"),
                };
                d.set(&s.name, toml::Value::Array(items.into_iter().filter(|x| !x.is_empty()).map(toml::Value::String).collect()));
            }
            _ => match v.as_str() {
                Some(t) => d.set(&s.name, t.trim().to_string()),
                None => return bad("text"),
            },
        },
    }
    // saved, then read back as Conductor reads it at start: a value it can't take goes back to what was there
    if let Err(e) = d.save() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, &format!("{e:#}"));
    }
    if let Err(e) = crate::config::Config::load(Some(path.clone())) {
        let _ = before.save();
        return bad(&format!("Conductor can't use that: {e:#}"));
    }
    tracing::info!(setting = %s.name, by = %u.name, "setting changed (used after a restart)");
    Json(json!({ "ok": true, "restart": true, "env": from_env(&s.name) })).into_response()
}
