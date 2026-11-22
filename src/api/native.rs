//! A small stand-in for the parts of Navidrome's own API that Encore 1.7.9 uses (smart playlists and playlist
//! pictures). It goes away once Encore uses the `encore.smartPlaylists` / `encore.playlistImages` extensions.
//!
//!   POST   /auth/login                    {"username", "password"} → {"token", …}
//!   GET    /api/playlist[/{id}]           POST /api/playlist   PUT /api/playlist/{id}   DELETE /api/playlist/{id}
//!   POST   /api/playlist/{id}/image       (multipart field "image")     DELETE /api/playlist/{id}/image
//! Requests carry `x-nd-authorization: Bearer <token>`. Tokens live in memory for 48 hours; after a restart the
//! app gets 401 and signs in again.

#![allow(clippy::result_large_err)] // handlers return axum Responses as errors

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Shared, smart};
use crate::{
    auth::{self, User},
    db::{self, iso, now},
};

const TOKEN_HOURS: i64 = 48;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/api/playlist", get(list).post(create))
        .route("/api/playlist/{id}", get(read).put(update).delete(remove))
        .route("/api/playlist/{id}/image", post(set_image).delete(delete_image))
        .layer(DefaultBodyLimit::max(15 << 20))
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

#[derive(Deserialize)]
struct LoginBody {
    username: String,
    password: String,
}

async fn login(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    h: HeaderMap,
    Json(b): Json<LoginBody>,
) -> Response {
    let ip = crate::jam::client_ip(&h, peer);
    if st.login_fails.blocked("app", &ip, &b.username, auth::APP_TRIES) {
        return err(StatusCode::TOO_MANY_REQUESTS, "Too many wrong passwords. Try again in a few minutes.");
    }
    let Ok(Some(user)) = auth::verify_password(&st.db, &st.secrets, &b.username, &b.password).await else {
        st.login_fails.failed("app", &ip, &b.username);
        return err(StatusCode::UNAUTHORIZED, "Invalid username or password");
    };
    st.login_fails.clear("app", &ip, &b.username);
    let token = format!("{}{}", db::random_id(), db::random_id());
    {
        let mut t = st.native_tokens.lock().await;
        let n = now();
        t.retain(|_, (_, exp)| *exp > n);
        t.insert(token.clone(), (user.id.clone(), n + TOKEN_HOURS * 3600));
    }
    Json(json!({ "id": user.id, "name": user.name, "username": user.name, "isAdmin": user.is_admin, "token": token }))
        .into_response()
}

async fn user_of(st: &Shared, h: &HeaderMap) -> Option<User> {
    let v = h.get("x-nd-authorization").or_else(|| h.get("authorization"))?.to_str().ok()?;
    let tok = v.strip_prefix("Bearer ").unwrap_or(v).trim();
    let uid = {
        let t = st.native_tokens.lock().await;
        t.get(tok).filter(|(_, exp)| *exp > now()).map(|(u, _)| u.clone())?
    };
    sqlx::query_as("SELECT id, name, password_enc, is_admin FROM user WHERE id = ?")
        .bind(uid)
        .fetch_optional(&st.db)
        .await
        .ok()?
}

macro_rules! need_user {
    ($st:expr, $h:expr) => {
        match user_of(&$st, &$h).await {
            Some(u) => u,
            None => return err(StatusCode::UNAUTHORIZED, "Not signed in"),
        }
    };
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    owner_id: String,
    owner_name: String,
    name: String,
    comment: String,
    public: bool,
    rules_json: Option<String>,
    created_at: i64,
    updated_at: i64,
    image_path: Option<String>,
    song_count: i64,
    duration_ms: i64,
}

const SELECT: &str = "SELECT p.id, p.owner_id, u.name AS owner_name, p.name, p.comment, p.public, p.rules_json, p.created_at,
    p.updated_at, p.image_path,
    (SELECT COUNT(*) FROM playlist_entry e WHERE e.playlist_id = p.id) AS song_count,
    (SELECT COALESCE(SUM(t.duration_ms), 0) FROM playlist_entry e JOIN track t ON t.id = e.track_id WHERE e.playlist_id = p.id) AS duration_ms
  FROM playlist p JOIN user u ON u.id = p.owner_id";

fn to_json(r: &Row) -> Value {
    let rules = r.rules_json.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()).unwrap_or(Value::Null);
    json!({
        "id": r.id, "name": r.name, "comment": r.comment, "public": r.public, "ownerId": r.owner_id, "ownerName": r.owner_name,
        "songCount": r.song_count, "duration": r.duration_ms as f64 / 1000.0, "rules": rules, "sync": false, "path": "",
        "createdAt": iso(r.created_at), "updatedAt": iso(r.updated_at), "evaluatedAt": iso(r.updated_at),
        "uploadedImage": r.image_path.as_deref().map(|p| p.rsplit(['/', '\\']).next().unwrap_or("")).unwrap_or(""),
    })
}

async fn load(st: &Shared, id: &str) -> Option<Row> {
    sqlx::query_as(&format!("{SELECT} WHERE p.id = ?")).bind(id).fetch_optional(&st.db).await.ok().flatten()
}

async fn list(State(st): State<Shared>, h: HeaderMap) -> Response {
    let u = need_user!(st, h);
    let rows: Vec<Row> =
        match sqlx::query_as(&format!("{SELECT} WHERE p.owner_id = ? OR p.public = 1 ORDER BY p.name COLLATE NOCASE"))
            .bind(&u.id)
            .fetch_all(&st.db)
            .await
        {
            Ok(r) => r,
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "database"),
        };
    Json(rows.iter().map(to_json).collect::<Vec<_>>()).into_response()
}

async fn read(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> Response {
    let u = need_user!(st, h);
    let _ = smart::refresh(&st, &id, smart::MAX_AGE).await;
    match load(&st, &id).await {
        Some(r) if r.public || r.owner_id == u.id || u.is_admin => Json(to_json(&r)).into_response(),
        _ => err(StatusCode::NOT_FOUND, "not found"),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Body {
    name: Option<String>,
    comment: Option<String>,
    public: Option<bool>,
    rules: Option<Value>,
}

fn rules_text(rules: &Option<Value>) -> Result<Option<String>, Response> {
    match rules {
        None | Some(Value::Null) => Ok(None),
        Some(r) if r.as_object().is_some_and(|o| o.is_empty()) => Ok(None),
        Some(r) => match smart::compile(r) {
            Ok(_) => Ok(Some(r.to_string())),
            Err(e) => Err(err(StatusCode::BAD_REQUEST, &format!("rules: {e}"))),
        },
    }
}

async fn create(State(st): State<Shared>, h: HeaderMap, Json(b): Json<Body>) -> Response {
    let u = need_user!(st, h);
    let Some(name) = b.name.filter(|n| !n.trim().is_empty()) else { return err(StatusCode::BAD_REQUEST, "name is required") };
    let rules = match rules_text(&b.rules) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let id = db::random_id();
    let r = sqlx::query(
        "INSERT INTO playlist (id, owner_id, name, comment, public, rules_json, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&u.id)
    .bind(&name)
    .bind(b.comment.unwrap_or_default())
    .bind(b.public.unwrap_or(false))
    .bind(rules)
    .bind(now())
    .bind(now())
    .execute(&st.db)
    .await;
    if r.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "database");
    }
    let _ = smart::refresh(&st, &id, 0).await;
    match load(&st, &id).await {
        Some(r) => Json(to_json(&r)).into_response(),
        None => err(StatusCode::INTERNAL_SERVER_ERROR, "database"),
    }
}

async fn owned(st: &Shared, u: &User, id: &str) -> Result<Row, Response> {
    match load(st, id).await {
        None => Err(err(StatusCode::NOT_FOUND, "not found")),
        Some(r) if r.owner_id != u.id && !u.is_admin => {
            Err(err(StatusCode::FORBIDDEN, "only the owner can change this playlist"))
        }
        Some(r) => Ok(r),
    }
}

async fn update(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>, Json(b): Json<Body>) -> Response {
    let u = need_user!(st, h);
    let cur = match owned(&st, &u, &id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let rules = match rules_text(&b.rules) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let r = sqlx::query(
        "UPDATE playlist SET name = ?, comment = ?, public = ?, rules_json = ?, updated_at = ?, evaluated_at = NULL WHERE id = ?",
    )
    .bind(b.name.filter(|n| !n.trim().is_empty()).unwrap_or(cur.name))
    .bind(b.comment.unwrap_or(cur.comment))
    .bind(b.public.unwrap_or(cur.public))
    .bind(rules)
    .bind(now())
    .bind(&id)
    .execute(&st.db)
    .await;
    if r.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "database");
    }
    let _ = smart::refresh(&st, &id, 0).await;
    match load(&st, &id).await {
        Some(r) => Json(to_json(&r)).into_response(),
        None => err(StatusCode::NOT_FOUND, "not found"),
    }
}

async fn remove(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> Response {
    let u = need_user!(st, h);
    let cur = match owned(&st, &u, &id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(p) = cur.image_path {
        let _ = tokio::fs::remove_file(p).await;
    }
    let _ = sqlx::query("DELETE FROM playlist WHERE id = ?").bind(&id).execute(&st.db).await;
    Json(json!({ "id": id })).into_response()
}

async fn set_image(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>, mut form: Multipart) -> Response {
    let u = need_user!(st, h);
    let cur = match owned(&st, &u, &id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    let mut data = None;
    while let Ok(Some(f)) = form.next_field().await {
        if f.name() == Some("image") {
            data = f.bytes().await.ok();
            break;
        }
    }
    let Some(data) = data else { return err(StatusCode::BAD_REQUEST, "no image") };
    let ext = match image::guess_format(&data) {
        Ok(image::ImageFormat::Jpeg) => "jpg",
        Ok(image::ImageFormat::Png) => "png",
        Ok(image::ImageFormat::WebP) => "webp",
        _ => return err(StatusCode::BAD_REQUEST, "the picture must be JPEG, PNG or WebP"),
    };
    let dir = st.cfg.data_dir.join("playlist-images");
    let path = dir.join(format!("{id}_{}.{ext}", &db::random_id()[..8]));
    if tokio::fs::create_dir_all(&dir).await.is_err() || tokio::fs::write(&path, &data).await.is_err() {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "can't save the picture");
    }
    let _ = sqlx::query("UPDATE playlist SET image_path = ?, updated_at = ? WHERE id = ?")
        .bind(path.to_string_lossy().to_string())
        .bind(now())
        .bind(&id)
        .execute(&st.db)
        .await;
    if let Some(old) = cur.image_path {
        let _ = tokio::fs::remove_file(old).await;
    }
    Json(json!({ "id": id })).into_response()
}

async fn delete_image(State(st): State<Shared>, h: HeaderMap, Path(id): Path<String>) -> Response {
    let u = need_user!(st, h);
    let cur = match owned(&st, &u, &id).await {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(old) = cur.image_path {
        let _ = tokio::fs::remove_file(old).await;
    }
    let _ = sqlx::query("UPDATE playlist SET image_path = NULL, updated_at = ? WHERE id = ?")
        .bind(now())
        .bind(&id)
        .execute(&st.db)
        .await;
    Json(json!({ "id": id })).into_response()
}
