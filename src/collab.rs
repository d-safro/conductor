//! Collaborative playlists: friends add songs to one of your playlists through a link, like Jam.
//!
//!   Encore (signed in):   playlistShareStart?id=  playlistShareEnd?id=  playlistShares
//!   Friends (public, on the Jam listener):
//!     GET  /jam/pl/<token>               the page            GET /jam/pl/<token>/state
//!     GET  /jam/pl/<token>/search?q=     songs to add        GET /jam/pl/<token>/cover/<id>?s=
//!     POST /jam/pl/<token>/add {id, by, guest}   POST /jam/pl/<token>/remove {id, index, guest}
//!
//! Changes go to the real playlist, so every app sees them. Friends can take out only songs they added. A link
//! works until it's stopped in the app, or for 30 days.

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::LazyLock,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Mutex;

use crate::{
    api::{ApiError, Ctx, Reply, Shared, media, ok},
    db::{self, now},
};

const PAGE: &str = include_str!("../web/collab/index.html");
const LIFETIME: i64 = 30 * 86400;
const MAX_SONGS: usize = 500;
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; \
                   img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Per link: request times per address (rate limit) and the covers its page has shown (only those can be fetched).
#[derive(Default)]
struct Seen {
    hits: HashMap<(String, String), Vec<i64>>,
    covers: HashMap<String, HashSet<String>>,
}
static SEEN: LazyLock<Mutex<Seen>> = LazyLock::new(Default::default);

fn token_ok(t: &str) -> bool {
    (20..=64).contains(&t.len()) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn song_id_ok(t: &str) -> bool {
    (1..=64).contains(&t.len()) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') && !t.starts_with("dz-")
}

fn url_of(c_base: Option<&str>, token: &str) -> String {
    c_base.filter(|b| !b.is_empty()).map(|b| format!("{}/jam/pl/{token}", b.trim_end_matches('/'))).unwrap_or_default()
}

async fn added(db: &db::Db, token: &str, last: i64) -> Vec<Value> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT track_id, by_name, guest, added_at FROM (SELECT rowid, * FROM playlist_share_add WHERE token = ?
         ORDER BY rowid DESC LIMIT ?) ORDER BY rowid",
    )
    .bind(token)
    .bind(last)
    .fetch_all(db)
    .await
    .unwrap_or_default();
    rows.into_iter().map(|(id, by, guest, at)| json!({ "id": id, "by": by, "guest": guest, "at": at })).collect()
}

/// A live link (`token`) → (playlist id, owner id). Expired links are removed.
async fn live(db: &db::Db, token: &str) -> Option<(String, String)> {
    if !token_ok(token) {
        return None;
    }
    let row: Option<(String, String, i64)> =
        sqlx::query_as("SELECT playlist_id, owner_id, created_at FROM playlist_share WHERE token = ?")
            .bind(token)
            .fetch_optional(db)
            .await
            .ok()?;
    let (pid, owner, created) = row?;
    if now() - created > LIFETIME {
        let _ = sqlx::query("DELETE FROM playlist_share WHERE token = ?").bind(token).execute(db).await;
        return None;
    }
    Some((pid, owner))
}

// ───────────── the app ─────────────

pub async fn share_start(c: &Ctx) -> Result<Reply, ApiError> {
    let pid = c.p.req("id")?;
    let mine: Option<Option<String>> = sqlx::query_scalar("SELECT rules_json FROM playlist WHERE id = ? AND owner_id = ?")
        .bind(pid)
        .bind(&c.user.id)
        .fetch_optional(&c.st.db)
        .await?;
    match mine {
        None => return ok(json!({ "playlistShare": { "error": "That isn't one of your playlists" } })),
        Some(Some(_)) => {
            return ok(json!({ "playlistShare": { "error": "Smart playlists fill themselves: friends can't add to them" } }));
        }
        Some(None) => {}
    }
    let existing: Option<(String, i64)> = sqlx::query_as("SELECT token, created_at FROM playlist_share WHERE playlist_id = ?")
        .bind(pid)
        .fetch_optional(&c.st.db)
        .await?;
    let token = match existing {
        Some((t, created)) if now() - created <= LIFETIME => t,
        _ => {
            sqlx::query("DELETE FROM playlist_share WHERE playlist_id = ?").bind(pid).execute(&c.st.db).await?;
            let t = format!("{}{}", db::random_id(), &db::random_id()[..8]);
            sqlx::query("INSERT INTO playlist_share (token, playlist_id, owner_id, created_at) VALUES (?, ?, ?, ?)")
                .bind(&t)
                .bind(pid)
                .bind(&c.user.id)
                .bind(now())
                .execute(&c.st.db)
                .await?;
            t
        }
    };
    let url = url_of(c.st.cfg.jam_public_url.as_deref(), &token);
    let mut out = json!({ "playlist": pid, "url": url, "token": token, "added": added(&c.st.db, &token, 1000).await });
    if url.is_empty() {
        out["error"] = "Sharing isn't set up on the server yet (jam_public_url)".into();
    }
    ok(json!({ "playlistShare": out }))
}

pub async fn share_end(c: &Ctx) -> Result<Reply, ApiError> {
    let r = sqlx::query("DELETE FROM playlist_share WHERE playlist_id = ? AND owner_id = ?")
        .bind(c.p.get("id").unwrap_or(""))
        .bind(&c.user.id)
        .execute(&c.st.db)
        .await?;
    ok(json!({ "playlistShare": { "ended": r.rows_affected() > 0 } }))
}

pub async fn shares(c: &Ctx) -> Result<Reply, ApiError> {
    let rows: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT token, playlist_id, created_at FROM playlist_share WHERE owner_id = ?")
            .bind(&c.user.id)
            .fetch_all(&c.st.db)
            .await?;
    let mut out = vec![];
    for (t, pid, created) in rows {
        if now() - created <= LIFETIME {
            out.push(json!({ "playlist": pid, "url": url_of(c.st.cfg.jam_public_url.as_deref(), &t),
                "added": added(&c.st.db, &t, 50).await }));
        }
    }
    ok(json!({ "playlistShares": { "share": out } }))
}

// ───────────── friends ─────────────

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/jam/pl/{token}", get(page))
        .route("/jam/pl/{token}/state", get(state))
        .route("/jam/pl/{token}/search", get(search))
        .route("/jam/pl/{token}/cover/{id}", get(cover))
        .route("/jam/pl/{token}/{action}", post(action))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": msg }))).into_response()
}

fn ended() -> Response {
    err(StatusCode::GONE, "This link has been stopped")
}

fn limited(token: &str, ip: &str, n: usize) -> bool {
    let now = db::now_ms();
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let hits = g.hits.entry((token.to_string(), ip.to_string())).or_default();
    hits.retain(|t| now - t < 60_000);
    hits.push(now);
    let over = hits.len() > n;
    if g.hits.len() > 2000 {
        g.hits.retain(|_, v| v.last().is_some_and(|t| now - t < 60_000));
    }
    over
}

fn show_covers<'a>(token: &str, covers: impl Iterator<Item = &'a str>) {
    let mut g = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let set = g.covers.entry(token.to_string()).or_default();
    if set.len() > 5000 {
        set.clear();
    }
    set.extend(covers.filter(|c| !c.is_empty()).map(String::from));
}

/// The playlist's songs, in order, with who added each through the link.
async fn entries(st: &Shared, token: &str, pid: &str) -> Vec<Value> {
    let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT t.id, t.title, t.artist_name, t.album_id, t.duration_ms FROM playlist_entry e JOIN track t ON t.id = e.track_id
         WHERE e.playlist_id = ? ORDER BY e.position",
    )
    .bind(pid)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    let mut who: HashMap<String, (String, String)> = HashMap::new();
    let adds: Vec<(String, String, String)> =
        sqlx::query_as("SELECT track_id, by_name, guest FROM playlist_share_add WHERE token = ? ORDER BY rowid")
            .bind(token)
            .fetch_all(&st.db)
            .await
            .unwrap_or_default();
    for (id, by, g) in adds {
        who.entry(id).or_insert((by, g));
    }
    rows.into_iter()
        .enumerate()
        .map(|(i, (id, title, artist, cover, d))| {
            let (by, guest) = who.get(&id).cloned().unwrap_or_default();
            json!({ "index": i, "id": id, "title": title, "artist": artist, "cover": cover, "duration": d / 1000,
                "by": by, "guest": guest })
        })
        .collect()
}

async fn page(State(st): State<Shared>, Path(token): Path<String>) -> Response {
    if !token_ok(&token) {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, CSP),
    ];
    if live(&st.db, &token).await.is_some() {
        (StatusCode::OK, headers, PAGE).into_response()
    } else {
        (StatusCode::GONE, headers, PAGE.replace("<!--ENDED-->", r#"<meta name="share-ended" content="1">"#)).into_response()
    }
}

async fn state(State(st): State<Shared>, Path(token): Path<String>) -> Response {
    let Some((pid, _)) = live(&st.db, &token).await else { return ended() };
    let head: Option<(String, String, String)> =
        sqlx::query_as("SELECT p.name, p.comment, u.name FROM playlist p JOIN user u ON u.id = p.owner_id WHERE p.id = ?")
            .bind(&pid)
            .fetch_optional(&st.db)
            .await
            .unwrap_or_default();
    let Some((name, comment, owner)) = head else { return ended() };
    let songs = entries(&st, &token, &pid).await;
    show_covers(&token, songs.iter().filter_map(|s| s["cover"].as_str()));
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({ "name": name, "comment": comment, "owner": owner, "song": songs })),
    )
        .into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchQ {
    q: String,
}

async fn search(
    State(st): State<Shared>,
    Path(token): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    Query(q): Query<SearchQ>,
) -> Response {
    if live(&st.db, &token).await.is_none() {
        return ended();
    }
    if limited(&token, &crate::jam::client_ip(&h, peer), 40) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Slow down a little");
    }
    let text: String = q.q.trim().chars().take(100).collect();
    let songs = if text.chars().count() >= 2 { crate::jam::find_songs(&st, &text, 25).await } else { vec![] };
    show_covers(&token, songs.iter().filter_map(|s| s["cover"].as_str()));
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "songs": songs }))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CoverQ {
    s: Option<String>,
}

async fn cover(State(st): State<Shared>, Path((token, id)): Path<(String, String)>, Query(q): Query<CoverQ>) -> Response {
    let shown = SEEN.lock().unwrap_or_else(|e| e.into_inner()).covers.get(&token).is_some_and(|c| c.contains(&id));
    if !shown || !token_ok(&token) || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if live(&st.db, &token).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let size = q.s.and_then(|s| s.parse::<u32>().ok()).filter(|s| *s <= 600).unwrap_or(150);
    match media::cover_for(&st, &id, Some(size)).await {
        Ok(Some(b)) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=86400")], b)
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn set_entries(st: &Shared, pid: &str, ids: &[String]) -> Result<(), sqlx::Error> {
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(pid).execute(&mut *tx).await?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
            .bind(pid)
            .bind(i as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(pid).execute(&mut *tx).await?;
    tx.commit().await
}

async fn action(
    State(st): State<Shared>,
    Path((token, kind)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    if kind != "add" && kind != "remove" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some((pid, _)) = live(&st.db, &token).await else { return ended() };
    if limited(&token, &crate::jam::client_ip(&h, peer), 30) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Slow down a little");
    }
    if body.len() > 4096 {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "Too big");
    }
    let Ok(Value::Object(b)) = serde_json::from_slice::<Value>(if body.is_empty() { b"{}" } else { &body }) else {
        return err(StatusCode::BAD_REQUEST, "Bad request");
    };
    let text = |k: &str, n: usize| -> String {
        match b.get(k) {
            Some(Value::String(s)) => s.chars().take(n).collect(),
            Some(Value::Null) | None => String::new(),
            Some(o) => o.to_string().chars().take(n).collect(),
        }
    };
    let by = Some(text("by", 30)).filter(|s| !s.is_empty()).unwrap_or_else(|| "A friend".into());
    let guest = text("guest", 40);
    let sid = text("id", 80);
    let ids: Vec<String> = sqlx::query_scalar("SELECT track_id FROM playlist_entry WHERE playlist_id = ? ORDER BY position")
        .bind(&pid)
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
    if kind == "add" {
        let known: i64 = if song_id_ok(&sid) {
            sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE id = ? AND missing_since IS NULL")
                .bind(&sid)
                .fetch_one(&st.db)
                .await
                .unwrap_or(0)
        } else {
            0
        };
        if known == 0 {
            return err(StatusCode::BAD_REQUEST, "Unknown song");
        }
        if ids.len() >= MAX_SONGS {
            return err(StatusCode::CONFLICT, "The playlist is full");
        }
        if ids.contains(&sid) {
            return Json(json!({ "ok": true, "already": true })).into_response();
        }
        let r = sqlx::query(
            "INSERT INTO playlist_entry (playlist_id, position, track_id)
             SELECT ?, COALESCE(MAX(position), -1) + 1, ? FROM playlist_entry WHERE playlist_id = ?",
        )
        .bind(&pid)
        .bind(&sid)
        .bind(&pid)
        .execute(&st.db)
        .await;
        if r.is_err() {
            return err(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong");
        }
        let _ = sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(&pid).execute(&st.db).await;
        let _ = sqlx::query("INSERT INTO playlist_share_add (token, track_id, by_name, guest, added_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&token)
            .bind(&sid)
            .bind(&by)
            .bind(&guest)
            .bind(now())
            .execute(&st.db)
            .await;
        return Json(json!({ "ok": true })).into_response();
    }
    // remove: by its place, checked against the song (the playlist may have changed meanwhile)
    let index = b.get("index").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
    let Some(index) = index else { return err(StatusCode::BAD_REQUEST, "Bad request") };
    if index < 0 || index as usize >= ids.len() || ids[index as usize] != sid {
        return err(StatusCode::CONFLICT, "The playlist changed: try again");
    }
    let theirs: Option<String> =
        sqlx::query_scalar("SELECT guest FROM playlist_share_add WHERE token = ? AND track_id = ? ORDER BY rowid LIMIT 1")
            .bind(&token)
            .bind(&sid)
            .fetch_optional(&st.db)
            .await
            .unwrap_or_default();
    if guest.is_empty() || theirs.as_deref() != Some(guest.as_str()) {
        return err(StatusCode::FORBIDDEN, "You can only take out songs you added");
    }
    let mut keep = ids;
    keep.remove(index as usize);
    if set_entries(&st, &pid, &keep).await.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong");
    }
    let _ = sqlx::query("DELETE FROM playlist_share_add WHERE token = ? AND track_id = ? AND guest = ?")
        .bind(&token)
        .bind(&sid)
        .bind(&guest)
        .execute(&st.db)
        .await;
    Json(json!({ "ok": true })).into_response()
}
