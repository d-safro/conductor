//! The admin page at /admin (admins only): status, scan, downloads (retry / cancel), Lidarr albums, users.
//! Sign-in sets an HttpOnly, SameSite=Strict cookie (kept in memory for 12 hours); every change also needs the
//! `X-Maestro: 1` header, which other sites can't send.

#![allow(clippy::result_large_err)] // handlers return axum Responses as errors

use std::sync::atomic::Ordering;

use axum::{
    Json, Router,
    extract::{Path, State},
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
const COOKIE: &str = "maestro_admin";
const HOURS: i64 = 12;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/admin", get(|| async { Html(PAGE) }))
        .route("/admin/", get(|| async { Html(PAGE) }))
        .route("/admin/api/login", post(login))
        .route("/admin/api/logout", post(logout))
        .route("/admin/api/status", get(status))
        .route("/admin/api/scan", post(scan))
        .route("/admin/api/requests", get(requests))
        .route("/admin/api/requests/{key}/retry", post(retry))
        .route("/admin/api/requests/{key}", delete(cancel))
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

/// The signed-in admin. Changes (`write`) also need the X-Maestro header.
async fn admin(st: &Shared, h: &HeaderMap, write: bool) -> Result<User, Response> {
    if write && h.get("x-maestro").and_then(|v| v.to_str().ok()) != Some("1") {
        return Err(err(StatusCode::FORBIDDEN, "missing X-Maestro header"));
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

async fn login(State(st): State<Shared>, h: HeaderMap, Json(b): Json<Login>) -> Response {
    if h.get("x-maestro").and_then(|v| v.to_str().ok()) != Some("1") {
        return err(StatusCode::FORBIDDEN, "missing X-Maestro header");
    }
    let Ok(Some(u)) = auth::verify_password(&st.db, &st.secrets, &b.username, &b.password).await else {
        return err(StatusCode::UNAUTHORIZED, "Wrong username or password");
    };
    if !u.is_admin {
        return err(StatusCode::FORBIDDEN, "This page is for admins");
    }
    let tok = format!("{}{}", db::random_id(), db::random_id());
    st.native_tokens.lock().await.insert(format!("admin:{tok}"), (u.id, now() + HOURS * 3600));
    let c = format!("{COOKIE}={tok}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={}", HOURS * 3600);
    let mut r = Json(json!({ "ok": true, "name": u.name })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&c).unwrap());
    r
}

async fn logout(State(st): State<Shared>, h: HeaderMap) -> Response {
    if let Some(t) = cookie_of(&h) {
        st.native_tokens.lock().await.remove(&format!("admin:{t}"));
    }
    let mut r = Json(json!({ "ok": true })).into_response();
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("maestro_admin=; Path=/admin; Max-Age=0"));
    r
}

async fn count(st: &Shared, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(&st.db).await.unwrap_or(0)
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
