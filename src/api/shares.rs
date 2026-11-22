//! Shares (Subsonic `getShares` `createShare` `updateShare` `deleteShare`): a link anyone can open to listen to some
//! songs, without an account.
//!
//!   Apps (signed in):  createShare?id=…&id=…&description=&expires=   (songs, albums or playlists you can see)
//!   Anyone (public, on the Jam listener, at `jam_public_url`):
//!     GET /jam/s/<token>               the page               GET /jam/s/<token>/state
//!     GET /jam/s/<token>/stream/<n>    the n-th song's file   GET /jam/s/<token>/cover/<n>?s=
//!
//! The songs are fixed when the share is made. Visitors get only those songs, by their place in the list (never an
//! id), with a rate limit per address. A share works until it expires or is deleted.

use std::{collections::HashMap, net::SocketAddr, sync::LazyLock, sync::Mutex};

use axum::{
    Json, Router,
    body::Body,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::{ApiError, Ctx, Reply, Shared, empty, items, media, ok};
use crate::db::{self, now};

const PAGE: &str = include_str!("../../web/share/index.html");
const MAX_SONGS: usize = 500;
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; \
                   img-src 'self' data:; media-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    user_id: String,
    username: String,
    description: String,
    track_ids: String,
    created_at: i64,
    expires_at: Option<i64>,
    visited_at: Option<i64>,
    visits: i64,
}

const SELECT: &str = "SELECT s.id, s.user_id, u.name AS username, s.description, s.track_ids, s.created_at, s.expires_at,
                             s.visited_at, s.visits
                      FROM share s JOIN user u ON u.id = s.user_id";

fn ids_of(r: &Row) -> Vec<String> {
    serde_json::from_str(&r.track_ids).unwrap_or_default()
}

fn link(c: &Ctx, token: &str) -> Result<String, ApiError> {
    let base = c.st.cfg.jam_public_url.as_deref().map(|u| u.trim_end_matches('/')).filter(|u| !u.is_empty());
    let base = base.ok_or_else(|| {
        ApiError::new(0, "Sharing needs the public address guests open (jam_public_url, the one Jam uses)")
    })?;
    Ok(format!("{base}/jam/s/{token}"))
}

/// Expires is in milliseconds (Subsonic); stored in seconds. 0 or missing: never.
fn expires_param(c: &Ctx) -> Option<i64> {
    c.p.int("expires").filter(|e| *e > 0).map(|e| e / 1000)
}

async fn share_json(c: &Ctx, r: &Row) -> Result<Value, ApiError> {
    let ids = ids_of(r);
    let mut songs = vec![];
    if !ids.is_empty() {
        let marks = vec!["?"; ids.len()].join(",");
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let rows = items::tracks_where(c, &format!("t.id IN ({marks})"), &refs, "").await?;
        let by_id: HashMap<&str, &items::TrackRow> = rows.iter().map(|t| (t.id.as_str(), t)).collect();
        songs = ids.iter().filter_map(|i| by_id.get(i.as_str())).map(|t| items::song_json(t)).collect();
    }
    let mut v = json!({
        "id": r.id, "url": link(c, &r.id).unwrap_or_default(), "description": r.description, "username": r.username,
        "created": db::iso(r.created_at), "visitCount": r.visits, "entry": songs,
    });
    if let Some(e) = r.expires_at {
        v["expires"] = db::iso(e).into();
    }
    if let Some(t) = r.visited_at {
        v["lastVisited"] = db::iso(t).into();
    }
    Ok(v)
}

/// The songs behind the ids: songs as they are, albums in order, playlists you can see in order.
async fn expand(c: &Ctx, ids: &[&str]) -> Result<Vec<String>, ApiError> {
    let mut out: Vec<String> = vec![];
    for id in ids {
        let one: Option<String> = sqlx::query_scalar("SELECT id FROM track WHERE id = ? AND missing_since IS NULL")
            .bind(id)
            .fetch_optional(&c.st.db)
            .await?;
        if let Some(t) = one {
            out.push(t);
            continue;
        }
        let album: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM track WHERE album_id = ? AND missing_since IS NULL ORDER BY disc, number, sort_title",
        )
        .bind(id)
        .fetch_all(&c.st.db)
        .await?;
        if !album.is_empty() {
            out.extend(album);
            continue;
        }
        let pl: Option<(String, bool)> = sqlx::query_as("SELECT owner_id, public FROM playlist WHERE id = ?")
            .bind(id)
            .fetch_optional(&c.st.db)
            .await?;
        match pl {
            Some((owner, public)) if owner == c.user.id || public || c.user.is_admin => {
                let songs: Vec<String> = sqlx::query_scalar(
                    "SELECT e.track_id FROM playlist_entry e JOIN track t ON t.id = e.track_id
                     WHERE e.playlist_id = ? AND t.missing_since IS NULL ORDER BY e.position",
                )
                .bind(id)
                .fetch_all(&c.st.db)
                .await?;
                out.extend(songs);
            }
            _ => return Err(ApiError::not_found("Song, album or playlist")),
        }
    }
    out.truncate(MAX_SONGS);
    Ok(out)
}

async fn load(c: &Ctx, id: &str) -> Result<Row, ApiError> {
    let r: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE s.id = ?")))
        .bind(id)
        .fetch_optional(&c.st.db)
        .await?
        .ok_or_else(|| ApiError::not_found("Share"))?;
    if r.user_id != c.user.id && !c.user.is_admin {
        return Err(ApiError::not_found("Share"));
    }
    Ok(r)
}

pub async fn get_shares(c: &Ctx) -> Result<Reply, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE s.user_id = ? ORDER BY s.created_at DESC")))
        .bind(&c.user.id)
        .fetch_all(&c.st.db)
        .await?;
    let mut list = vec![];
    for r in &rows {
        list.push(share_json(c, r).await?);
    }
    ok(json!({ "shares": { "share": list } }))
}

pub async fn create(c: &Ctx) -> Result<Reply, ApiError> {
    let ids = c.p.all("id");
    if ids.is_empty() {
        return Err(ApiError::new(10, "Required parameter is missing: id"));
    }
    let token = format!("{}{}", db::random_id(), &db::random_id()[..8]);
    link(c, &token)?;
    let songs = expand(c, &ids).await?;
    if songs.is_empty() {
        return Err(ApiError::new(70, "Nothing to share: no songs there"));
    }
    let desc: String = c.p.get("description").unwrap_or("").chars().take(300).collect();
    sqlx::query("INSERT INTO share (id, user_id, description, track_ids, created_at, expires_at) VALUES (?,?,?,?,?,?)")
        .bind(&token)
        .bind(&c.user.id)
        .bind(&desc)
        .bind(serde_json::to_string(&songs).unwrap_or_default())
        .bind(now())
        .bind(expires_param(c))
        .execute(&c.st.db)
        .await?;
    let r = load(c, &token).await?;
    ok(json!({ "shares": { "share": [share_json(c, &r).await?] } }))
}

pub async fn update(c: &Ctx) -> Result<Reply, ApiError> {
    let r = load(c, c.p.req("id")?).await?;
    if let Some(d) = c.p.get("description") {
        let d: String = d.chars().take(300).collect();
        sqlx::query("UPDATE share SET description = ? WHERE id = ?").bind(d).bind(&r.id).execute(&c.st.db).await?;
    }
    if c.p.get("expires").is_some() {
        sqlx::query("UPDATE share SET expires_at = ? WHERE id = ?").bind(expires_param(c)).bind(&r.id).execute(&c.st.db).await?;
    }
    empty()
}

pub async fn delete(c: &Ctx) -> Result<Reply, ApiError> {
    let r = load(c, c.p.req("id")?).await?;
    sqlx::query("DELETE FROM share WHERE id = ?").bind(&r.id).execute(&c.st.db).await?;
    empty()
}

// ───────────── anyone with the link ─────────────

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/jam/s/{token}", get(page))
        .route("/jam/s/{token}/state", get(state))
        .route("/jam/s/{token}/stream/{n}", get(stream))
        .route("/jam/s/{token}/cover/{n}", get(cover))
}

fn token_ok(t: &str) -> bool {
    (20..=64).contains(&t.len()) && t.chars().all(|c| c.is_ascii_alphanumeric())
}

type Hits = Mutex<HashMap<(String, String), Vec<i64>>>;

/// At most `n` requests a minute from one address to one share.
fn limited(token: &str, ip: &str, n: usize) -> bool {
    static HITS: LazyLock<Hits> = LazyLock::new(Default::default);
    let now = db::now_ms();
    let mut g = HITS.lock().unwrap_or_else(|e| e.into_inner());
    let hits = g.entry((token.to_string(), ip.to_string())).or_default();
    hits.retain(|t| now - t < 60_000);
    hits.push(now);
    let over = hits.len() > n;
    crate::jam::trim_hits(&mut g, now, 2000);
    over
}

/// A share that's still open: (owner's name, description, songs).
async fn open(st: &Shared, token: &str) -> Option<(String, String, Vec<String>)> {
    if !token_ok(token) {
        return None;
    }
    let r: Row = sqlx::query_as(sqlx::AssertSqlSafe(format!("{SELECT} WHERE s.id = ?"))).bind(token).fetch_optional(&st.db).await.ok()??;
    if r.expires_at.is_some_and(|e| e <= now()) {
        return None;
    }
    let ids = ids_of(&r);
    Some((r.username, r.description, ids))
}

fn gone() -> Response {
    (StatusCode::GONE, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": "This link has run out or was stopped" })))
        .into_response()
}

async fn page(State(st): State<Shared>, Path(token): Path<String>) -> Response {
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, CSP),
    ];
    let status = if open(&st, &token).await.is_some() { StatusCode::OK } else { StatusCode::GONE };
    (status, headers, PAGE).into_response()
}

async fn state(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    Path(token): Path<String>,
) -> Response {
    // a link that can't be one is turned away before it's counted (or kept in memory)
    if !token_ok(&token) {
        return gone();
    }
    if limited(&token, &crate::jam::client_ip(&h, peer), 30) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Some((owner, description, ids)) = open(&st, &token).await else { return gone() };
    let _ = sqlx::query("UPDATE share SET visits = visits + 1, visited_at = ? WHERE id = ?")
        .bind(now())
        .bind(&token)
        .execute(&st.db)
        .await;
    let mut songs = vec![];
    for (n, id) in ids.iter().enumerate() {
        let t: Option<(String, String, String, i64)> = sqlx::query_as(
            "SELECT title, artist_name, album_name, duration_ms FROM track WHERE id = ? AND missing_since IS NULL",
        )
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
        if let Some((title, artist, album, ms)) = t {
            songs.push(json!({ "n": n, "title": title, "artist": artist, "album": album, "duration": ms / 1000 }));
        }
    }
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "owner": owner, "description": description, "song": songs })))
        .into_response()
}

/// The n-th song: (library, path in it, content type, album).
async fn nth(st: &Shared, token: &str, n: usize) -> Option<(i64, String, String, String)> {
    let (_, _, ids) = open(st, token).await?;
    let id = ids.get(n)?;
    sqlx::query_as("SELECT library_id, rel_path, content_type, album_id FROM track WHERE id = ? AND missing_since IS NULL")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()?
}

async fn stream(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((token, n)): Path<(String, usize)>,
    req: Request,
) -> Response {
    if !token_ok(&token) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // a player asks for a song in a few pieces (seeking): generous, but not a firehose
    if limited(&format!("{token}/stream"), &crate::jam::client_ip(req.headers(), peer), 120) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Some((lib, rel, content_type, _)) = nth(&st, &token, n).await else { return StatusCode::NOT_FOUND.into_response() };
    let Some(path) = crate::library::track_path(&st.libs, lib, &rel).filter(|p| p.is_file()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut fwd = axum::http::Request::builder().uri("/");
    for k in [header::RANGE, header::IF_RANGE] {
        if let Some(v) = req.headers().get(&k) {
            fwd = fwd.header(&k, v);
        }
    }
    let Ok(fwd) = fwd.body(Body::empty()) else { return StatusCode::BAD_REQUEST.into_response() };
    let resp = match ServeFile::new(path).oneshot(fwd).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let (mut parts, body) = resp.into_parts();
    if parts.status.is_success()
        && let Ok(v) = content_type.parse()
    {
        parts.headers.insert(header::CONTENT_TYPE, v);
    }
    parts.headers.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("private, no-store"));
    Response::from_parts(parts, Body::new(body))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CoverQ {
    s: Option<String>,
}

async fn cover(State(st): State<Shared>, Path((token, n)): Path<(String, usize)>, Query(q): Query<CoverQ>) -> Response {
    let Some((_, _, _, album)) = nth(&st, &token, n).await else { return StatusCode::NOT_FOUND.into_response() };
    let size = q.s.and_then(|s| s.parse::<u32>().ok()).filter(|s| *s <= 600).unwrap_or(150);
    match media::cover_for(&st, &album, Some(size)).await {
        Ok(Some(b)) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=86400")], b)
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
