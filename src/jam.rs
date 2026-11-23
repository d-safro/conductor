//! Jam: friends add songs to your queue from a web page. The guest side has its own listener (`jam_listen`,
//! default 127.0.0.1:4535) that serves nothing but `/jam/...`; only that port should be put on the internet
//! (e.g. Tailscale Funnel → `jam_public_url`). The app's side is in `api::encore` (jamStart / jamState / jamPoll / jamEnd).
//!
//!   GET  /jam/<token>                 the page          GET /jam/<token>/state?g=<guest>&n=<name>
//!   GET  /jam/<token>/search?q=       songs to add      GET /jam/<token>/cover/<id>?s=<size>
//!   POST /jam/<token>/add | move | remove | skip        (JSON body, becomes a command the app picks up)
//!   /jam/pl/...: collaborative playlists (`collab`)

use std::{collections::HashMap, net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    api::{AppState, Shared, media},
    db::{self, now_ms},
    live,
};

const PAGE: &str = include_str!("../web/jam/index.html");
const LIFETIME_MS: i64 = 12 * 3600 * 1000;
/// (its own script by hash: crate::page)
fn csp() -> &'static str {
    static C: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| crate::page::csp(PAGE, CSP));
    &C
}

const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; \
                   img-src 'self' https://*.dzcdn.net data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

pub fn router(st: Shared) -> Router {
    Router::new()
        .route("/jam/{token}", get(page))
        .route("/jam/{token}/state", get(state))
        .route("/jam/{token}/search", get(search))
        .route("/jam/{token}/cover/{id}", get(cover))
        .route("/jam/{token}/{action}", post(action))
        .merge(crate::collab::router())
        .merge(crate::api::shares::router())
        .merge(crate::brand::routes())
        .fallback(|| async { (StatusCode::NOT_FOUND, "Not found") })
        .layer(axum::middleware::map_response(|mut r: Response| async move {
            let h = r.headers_mut();
            h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
            h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
            r
        }))
        .with_state(st)
}

/// Jams survive a restart (the app sends its queue again); guests and commands don't.
pub async fn load(st: &AppState) {
    let rows: Vec<(String, String, String, i64)> =
        sqlx::query_as("SELECT token, owner_id, host_name, seen_at FROM jam").fetch_all(&st.db).await.unwrap_or_default();
    let mut g = st.live.lock();
    for (token, owner_id, host_name, seen) in rows {
        if now_ms() - seen < LIFETIME_MS {
            g.jams.insert(token.clone(), crate::live::Jam { token, owner_id, host_name, seen_ms: seen, ..Default::default() });
        }
    }
}

pub async fn save(st: &AppState) {
    let rows: Vec<(String, String, String, i64)> = {
        let mut g = st.live.lock();
        let n = now_ms();
        g.jams.retain(|_, j| n - j.seen_ms < LIFETIME_MS);
        g.jams.values().map(|j| (j.token.clone(), j.owner_id.clone(), j.host_name.clone(), j.seen_ms)).collect()
    };
    let Ok(mut tx) = st.db.begin().await else { return };
    let _ = sqlx::query("DELETE FROM jam").execute(&mut *tx).await;
    for (t, o, h, s) in rows {
        let _ = sqlx::query("INSERT INTO jam (token, owner_id, host_name, created_at, seen_at) VALUES (?, ?, ?, ?, ?)")
            .bind(t)
            .bind(o)
            .bind(h)
            .bind(db::now())
            .bind(s)
            .execute(&mut *tx)
            .await;
    }
    let _ = tx.commit().await;
}

fn token_ok(t: &str) -> bool {
    (20..=64).contains(&t.len()) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn id_ok(t: &str) -> bool {
    (1..=64).contains(&t.len()) && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "error": msg }))).into_response()
}

/// Is there such a Jam? (and touch nothing else)
fn exists(st: &AppState, token: &str) -> bool {
    token_ok(token) && st.live.lock().jams.contains_key(token)
}

/// Who's asking, for the rate limits. X-Forwarded-For counts only when the connection comes from a proxy on this
/// computer (Tailscale Funnel, Caddy…), and then only its last address: the one that proxy added. The ones before it
/// are whatever the guest sent, so a guest could otherwise be someone new on every request. Behind a proxy on
/// another computer the header isn't believed: everyone has the proxy's address, and shares its limits.
pub fn client_ip(h: &HeaderMap, peer: SocketAddr) -> String {
    if !peer.ip().is_loopback() {
        return peer.ip().to_string();
    }
    h.get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .rfind(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(|| peer.ip().to_string())
}

/// Something happened in the Jam of `owner` (a guest's command, a new guest): the host's jamPoll and event stream.
fn jam_changed(st: &AppState, owner: &str) {
    st.live.publish("jam", live::Audience::User(owner.to_string()), json!({}));
}

/// At most `n` requests a minute from one address to one Jam.
fn limited(st: &AppState, token: &str, ip: &str, n: usize) -> bool {
    let now = now_ms();
    let mut g = st.live.lock();
    let Some(j) = g.jams.get_mut(token) else { return false };
    let hits = j.hits.entry(ip.to_string()).or_default();
    hits.retain(|t| now - t < 60_000);
    hits.push(now);
    let over = hits.len() > n;
    trim_hits(&mut j.hits, now, 500);
    over
}

/// Keeps a table of requests a minute to about `cap` entries: past it, those not heard from in a minute go, then
/// the ones heard from longest ago (so many made-up addresses can't fill the memory).
pub(crate) fn trim_hits<K: Clone + Eq + std::hash::Hash>(m: &mut HashMap<K, Vec<i64>>, now: i64, cap: usize) {
    if m.len() <= cap {
        return;
    }
    m.retain(|_, v| v.last().is_some_and(|t| now - t < 60_000));
    if m.len() > cap {
        let mut by_age: Vec<(i64, K)> = m.iter().map(|(k, v)| (v.last().copied().unwrap_or(0), k.clone())).collect();
        by_age.sort_by_key(|x| x.0);
        let extra = m.len() - cap / 2;
        for (_, k) in by_age.into_iter().take(extra) {
            m.remove(&k);
        }
    }
}

/// Guests one Jam keeps count of.
const MAX_GUESTS: usize = 1000;

async fn page(State(st): State<Shared>, Path(token): Path<String>) -> Response {
    if !token_ok(&token) {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }
    let headers = [
        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, csp()),
    ];
    if exists(&st, &token) {
        (StatusCode::OK, headers, PAGE).into_response()
    } else {
        (StatusCode::GONE, headers, PAGE.replace("<!--ENDED-->", r#"<meta name="jam-ended" content="1">"#)).into_response()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StateQ {
    g: String,
    n: String,
}

async fn state(
    State(st): State<Shared>,
    Path(token): Path<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    Query(q): Query<StateQ>,
) -> Response {
    if !exists(&st, &token) {
        return err(StatusCode::GONE, "This Jam has ended");
    }
    // the page asks every 2.5 s; generous, for a party of guests behind one address
    if limited(&st, &token, &format!("{} state", client_ip(&h, peer)), 600) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Slow down a little");
    }
    let now = now_ms();
    let mut g = st.live.lock();
    let Some(j) = g.jams.get_mut(&token) else { return err(StatusCode::GONE, "This Jam has ended") };
    if !q.g.is_empty() {
        let id: String = q.g.chars().take(40).collect();
        if !j.guests.contains_key(&id) && j.guests.len() >= MAX_GUESTS {
            j.guests.retain(|_, (_, t)| now - *t < live::JAM_GUEST_SECS * 1000);
        }
        // past the limit a new guest still sees the Jam, just isn't counted
        if j.guests.contains_key(&id) || j.guests.len() < MAX_GUESTS {
            let new = j.guests.insert(id, (q.n.chars().take(30).collect(), now)).is_none();
            if new {
                let (owner, out) = (j.owner_id.clone(), j.public(now));
                drop(g);
                jam_changed(&st, &owner); // a new guest: the host's app shows how many
                return (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(out)).into_response();
            }
        }
    }
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(j.public(now))).into_response()
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
    if !exists(&st, &token) {
        return err(StatusCode::GONE, "This Jam has ended");
    }
    if limited(&st, &token, &client_ip(&h, peer), 40) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Slow down a little");
    }
    let text: String = q.q.trim().chars().take(100).collect();
    if text.chars().count() < 2 {
        return Json(json!({ "songs": [] })).into_response();
    }
    let songs = find_songs(&st, &text, 20).await;
    if let Some(j) = st.live.lock().jams.get_mut(&token) {
        j.covers.extend(songs.iter().filter_map(|s| s["cover"].as_str().map(String::from)));
    }
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(json!({ "songs": songs }))).into_response()
}

/// Library songs whose title / artist / album hold every word (for the guest pages).
pub async fn find_songs(st: &AppState, text: &str, limit: i64) -> Vec<Value> {
    let (cond, words) = crate::api::word_conditions(text, "t.title || ' ' || t.artist_name || ' ' || t.album_name");
    if words.is_empty() {
        return vec![];
    }
    let sql = format!(
        "SELECT t.id, t.title, t.artist_name, t.album_name, t.album_id, t.duration_ms FROM track t
         WHERE t.missing_since IS NULL AND {cond} ORDER BY t.sort_title LIMIT {limit}"
    );
    let mut qq = sqlx::query_as::<_, (String, String, String, String, String, i64)>(sqlx::AssertSqlSafe(sql.to_string()));
    for w in &words {
        qq = qq.bind(w);
    }
    qq.fetch_all(&st.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|(id, title, artist, album, cover, d)| {
            json!({ "id": id, "title": title, "artist": artist, "album": album, "cover": cover, "duration": d / 1000, "lookup": false })
        })
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CoverQ {
    s: Option<String>,
}

async fn cover(State(st): State<Shared>, Path((token, id)): Path<(String, String)>, Query(q): Query<CoverQ>) -> Response {
    let allowed = token_ok(&token) && st.live.lock().jams.get(&token).is_some_and(|j| j.covers.contains(&id));
    if !allowed || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let size = q.s.and_then(|s| s.parse::<u32>().ok()).filter(|s| *s <= 600).unwrap_or(300);
    match media::cover_for(&st, &id, Some(size)).await {
        Ok(Some(b)) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=86400")], b)
                .into_response()
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn action(
    State(st): State<Shared>,
    Path((token, kind)): Path<(String, String)>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    if !["add", "move", "remove", "skip"].contains(&kind.as_str()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !exists(&st, &token) {
        return err(StatusCode::GONE, "This Jam has ended");
    }
    if limited(&st, &token, &client_ip(&h, peer), 60) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Slow down a little");
    }
    if body.len() > 8192 {
        return err(StatusCode::PAYLOAD_TOO_LARGE, "Too big");
    }
    let Ok(Value::Object(b)) = serde_json::from_slice::<Value>(if body.is_empty() { b"{}" } else { &body }) else {
        return err(StatusCode::BAD_REQUEST, "Bad request");
    };
    let by: String =
        b.get("by").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or("A guest").chars().take(30).collect();
    let sid = b.get("id").map(|v| match v {
        Value::String(s) => s.clone(),
        o => o.to_string(),
    });
    let cmd = match kind.as_str() {
        "add" => {
            let Some(id) = sid.filter(|s| id_ok(s)) else { return err(StatusCode::BAD_REQUEST, "Unknown song") };
            let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE id = ? AND missing_since IS NULL")
                .bind(&id)
                .fetch_one(&st.db)
                .await
                .unwrap_or(0);
            if known == 0 {
                return err(StatusCode::BAD_REQUEST, "Unknown song");
            }
            json!({ "type": "add", "id": id, "by": by })
        }
        "move" | "remove" => {
            let num = |k: &str| b.get(k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
            let Some(a) = num("from").or_else(|| num("index")) else { return err(StatusCode::BAD_REQUEST, "Bad request") };
            let to = num("to").unwrap_or(a);
            let Some(id) = sid.filter(|s| id_ok(s)) else { return err(StatusCode::BAD_REQUEST, "Bad request") };
            if !(0..100_000).contains(&a) || !(0..100_000).contains(&to) {
                return err(StatusCode::BAD_REQUEST, "Bad request");
            }
            json!({ "type": kind, "id": id, "from": a, "to": to, "by": by })
        }
        _ => {
            let mut g = st.live.lock();
            let Some(j) = g.jams.get_mut(&token) else { return err(StatusCode::GONE, "This Jam has ended") };
            if now_ms() - j.last_skip_ms < 2000 {
                return Json(json!({ "ok": true })).into_response(); // two people tapping at once: one skip
            }
            j.last_skip_ms = now_ms();
            json!({ "type": "skip", "id": sid.unwrap_or_default().chars().take(64).collect::<String>(), "by": by })
        }
    };
    let owner = {
        let mut g = st.live.lock();
        let Some(j) = g.jams.get_mut(&token) else { return err(StatusCode::GONE, "This Jam has ended") };
        j.push(cmd);
        j.owner_id.clone()
    };
    jam_changed(&st, &owner);
    Json(json!({ "ok": true })).into_response()
}

/// Serve the guest listener until the program stops.
pub async fn serve(st: Arc<AppState>, addr: SocketAddr) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "Jam guest page listening");
    axum::serve(listener, router(st).into_make_service_with_connect_info::<SocketAddr>()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{client_ip, trim_hits};
    use axum::http::HeaderMap;

    #[test]
    fn rate_tables_stay_small() {
        let now = 1_000_000;
        let mut m: std::collections::HashMap<String, Vec<i64>> = Default::default();
        for i in 0..5000 {
            m.insert(format!("a{i}"), vec![now - 1000 + i / 10]);
            trim_hits(&mut m, now, 500);
            assert!(m.len() <= 501);
        }
        assert!(m.contains_key("a4999"), "the latest stay");
        assert!(!m.contains_key("a0"), "the oldest go");
    }

    #[test]
    fn who_is_asking() {
        let local: std::net::SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let far: std::net::SocketAddr = "198.51.100.7:5000".parse().unwrap();
        let mut h = HeaderMap::new();
        assert_eq!(client_ip(&h, far), "198.51.100.7");
        assert_eq!(client_ip(&h, local), "127.0.0.1");
        // the guest's own header, then what the proxy here added
        h.insert("x-forwarded-for", "1.2.3.4, 203.0.113.5".parse().unwrap());
        assert_eq!(client_ip(&h, local), "203.0.113.5", "the proxy's address, not the one the guest made up");
        assert_eq!(client_ip(&h, far), "198.51.100.7", "not through a proxy here: the header is the guest's own");
        h.append("x-forwarded-for", "203.0.113.6".parse().unwrap());
        assert_eq!(client_ip(&h, local), "203.0.113.6");
    }
}
