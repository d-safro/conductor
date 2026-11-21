//! AudioMuse-AI (optional; its analysis is an AI app, off unless set up): sound-alike songs and paths
//! between two sounds, and the read-only recommendation calls Encore makes at `/rest/audiomuse/<endpoint>`.
//! AudioMuse knows songs by the ids the server gave them (Navidrome's, kept by the import), so its answers map
//! straight onto the library. When it isn't set up or doesn't answer, these say "not available" and apps fall back.
//!
//! AudioMuse's API (as documented for its Jellyfin plugin): similar_tracks?item_id=&n=&eliminate_duplicates= → a list
//! of {item_id, title, author, distance}; find_path?start_song_id=&end_song_id=&max_steps= → {path: [...], total_distance}.

use axum::{
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use super::{ApiError, Ctx, Reply, Shared, items, ok};
use crate::auth::{self, Credentials};

/// The recommendation calls an app may use (method, path under AudioMuse's /api/).
const ALLOWED: &[(&str, &str)] = &[
    ("GET", "similar_tracks"),
    ("GET", "sonic_fingerprint/generate"),
    ("GET", "similar_artists"),
    ("GET", "search_tracks"),
    ("GET", "find_path"),
    ("GET", "mood_centroids"),
    ("GET", "lyrics/axes"),
    ("POST", "alchemy"),
    ("POST", "clap/search"),
    ("POST", "lyrics/search/text"),
    ("POST", "lyrics/search/axes"),
    ("GET", "clap/top_queries"),
];
const AUTH_KEYS: &[&str] = &["u", "p", "t", "s", "apiKey", "c", "v", "f"];

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build().expect("http client")
}

fn json_err(code: StatusCode, msg: &str) -> Response {
    (code, [(header::CONTENT_TYPE, "application/json")], json!({ "error": msg }).to_string()).into_response()
}

/// `/rest/audiomuse/<endpoint>`: forwarded for a signed-in user, with AudioMuse's own token.
pub async fn passthrough(State(st): State<Shared>, Path(endpoint): Path<String>, req: Request) -> Response {
    let endpoint = endpoint.trim_matches('/').to_string();
    let method = req.method().clone();
    if !ALLOWED.contains(&(method.as_str(), endpoint.as_str())) {
        return json_err(StatusCode::NOT_FOUND, "not available");
    }
    let pairs: Vec<(String, String)> =
        req.uri().query().map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect()).unwrap_or_default();
    let get = |k: &str| pairs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
    let creds = Credentials { u: get("u"), p: get("p"), t: get("t"), s: get("s"), api_key: get("apiKey") };
    if auth::login(&st.db, &st.secrets, creds).await.is_err() {
        return json_err(StatusCode::UNAUTHORIZED, "wrong login");
    }
    let Some(base) = st.cfg.audiomuse_url.clone() else {
        return json_err(StatusCode::NOT_FOUND, "AudioMuse-AI isn't set up on this server");
    };
    let rest: Vec<(String, String)> = pairs.iter().filter(|(k, _)| !AUTH_KEYS.contains(&k.as_str())).cloned().collect();
    let body =
        if method == Method::POST { to_bytes(req.into_body(), 1 << 20).await.unwrap_or_default() } else { Default::default() };
    let mut r = client()
        .request(method.clone(), format!("{}/api/{endpoint}", base.trim_end_matches('/')))
        .query(&rest)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = &st.cfg.audiomuse_token {
        r = r.bearer_auth(t);
    }
    if method == Method::POST {
        r = r.body(body);
    }
    match r.send().await {
        Ok(resp) => {
            let code = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = resp.bytes().await.unwrap_or_default();
            (code, [(header::CONTENT_TYPE, "application/json")], Body::from(bytes)).into_response()
        }
        Err(e) => json_err(StatusCode::BAD_GATEWAY, &format!("AudioMuse-AI isn't answering ({e})")),
    }
}

fn not_available() -> ApiError {
    ApiError {
        http: StatusCode::OK,
        ..ApiError::not_found("Sound-alike songs (AudioMuse-AI isn't set up or hasn't analysed this song)")
    }
}

async fn am_get(c: &Ctx, endpoint: &str, q: &[(&str, String)]) -> Result<Value, ApiError> {
    let Some(base) = c.st.cfg.audiomuse_url.clone() else { return Err(not_available()) };
    let mut r = client().get(format!("{}/api/{endpoint}", base.trim_end_matches('/'))).query(q);
    if let Some(t) = &c.st.cfg.audiomuse_token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await.map_err(|_| not_available())?;
    if !resp.status().is_success() {
        return Err(not_available());
    }
    resp.json().await.map_err(|_| not_available())
}

/// The list of songs in an AudioMuse answer (a list, or one under a usual key), as (id, distance).
fn songs_in(v: &Value) -> Vec<(String, Option<f64>)> {
    let list = match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => ["tracks", "path", "results", "similar_tracks", "items"]
            .iter()
            .find_map(|k| o.get(*k).and_then(Value::as_array).cloned())
            .unwrap_or_default(),
        _ => vec![],
    };
    list.iter()
        .filter_map(|x| {
            let id = x
                .get("item_id")
                .or(x.get("id"))
                .and_then(|i| i.as_str().map(String::from).or_else(|| i.as_i64().map(|n| n.to_string())))?;
            Some((id, x.get("distance").and_then(Value::as_f64)))
        })
        .collect()
}

async fn entries(c: &Ctx, found: Vec<(String, Option<f64>)>, skip: &[&str]) -> Vec<Value> {
    let mut out = vec![];
    for (id, dist) in found {
        if skip.contains(&id.as_str()) {
            continue;
        }
        if let Ok(t) = items::track(c, &id).await {
            let mut e = json!({ "entry": items::song_json(&t) });
            if let Some(d) = dist {
                e["similarity"] = json!((1.0 - d).clamp(0.0, 1.0));
            }
            out.push(e);
        }
    }
    out
}

/// getSonicSimilarTracks?id=&count= → {"sonicMatch": [{"entry": song, "similarity": 0.9}, …]}
pub async fn similar(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?.to_string();
    let count = c.p.int("count").unwrap_or(50).clamp(1, 500);
    let v = am_get(
        c,
        "similar_tracks",
        &[("item_id", id.clone()), ("n", count.to_string()), ("eliminate_duplicates", "true".into())],
    )
    .await?;
    let list = entries(c, songs_in(&v), &[&id]).await;
    if list.is_empty() {
        return Err(not_available());
    }
    ok(json!({ "sonicMatch": list }))
}

/// findSonicPath?startSongId=&endSongId=&count= → {"sonicPath": [{"entry": song}, …]}
pub async fn path(c: &Ctx) -> Result<Reply, ApiError> {
    let (a, b) = (c.p.req("startSongId")?.to_string(), c.p.req("endSongId")?.to_string());
    let count = c.p.int("count").unwrap_or(25).clamp(2, 200);
    let v = am_get(c, "find_path", &[("start_song_id", a), ("end_song_id", b), ("max_steps", count.to_string())]).await?;
    ok(json!({ "sonicPath": entries(c, songs_in(&v), &[]).await }))
}
