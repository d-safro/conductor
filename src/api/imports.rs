//! Transpose (imports from other services) for apps: `encore.import`. Each user sees and changes only their own.
//!
//!   POST importUpload?name=<file name>            body: the export (zip, json, csv, xml, m3u) → {import}
//!   importOnline?source=spotify|lastfm|deezer&account=   (Last.fm user name, Deezer user id) → {import}
//!                                                 (spotify: the server's Spotify login, for admins and the user
//!                                                 Spotify sync is for)
//!   getImports                                    → {imports: {import: [...]}} (newest first)
//!   getImport?id=                                 → {import: {id, status, progress, source, report, …}}
//!   startImport?id=&kinds=liked,albums,artists,playlists,plays,ratings,counts,podcasts&askMissing=N
//!   undoImport?id=   deleteImport?id=
//!
//! status: reading → preview (report: what was found, matchRate, changes it would make) → running → done
//! (report.changes: what it changed) → undone; or failed (error).

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use serde_json::json;

use super::{ApiError, Ctx, Reply, ok};
use crate::{
    auth::User,
    import::{apply::Options, jobs},
};

async fn mine(c: &Ctx) -> Result<jobs::BatchRow, ApiError> {
    let id = c.p.req("id")?;
    match jobs::get(&c.st, id).await? {
        Some(b) if b.user_id == c.user.id => Ok(b),
        _ => Err(ApiError::not_found("Import")),
    }
}

fn failed(e: anyhow::Error) -> ApiError {
    ApiError::new(0, format!("{e:#}"))
}

/// Uploads still arriving, per user: they count as waiting imports, so several sent at once can't all pass the
/// check before any of them is made an import.
static ARRIVING: LazyLock<Mutex<HashMap<String, i64>>> = LazyLock::new(Default::default);

/// One upload's place among the arriving ones, given back when it's dropped (made an import, or failed).
struct Arriving(String);

impl Arriving {
    /// Takes a place: how many others were arriving already.
    fn take(user: &str) -> (Self, i64) {
        let mut g = ARRIVING.lock().unwrap_or_else(|e| e.into_inner());
        let n = g.entry(user.to_string()).or_default();
        *n += 1;
        (Self(user.to_string()), *n - 1)
    }
}

impl Drop for Arriving {
    fn drop(&mut self) {
        let mut g = ARRIVING.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = g.get_mut(&self.0) {
            *n -= 1;
            if *n <= 0 {
                g.remove(&self.0);
            }
        }
    }
}

pub async fn upload(st: &crate::api::Shared, user: &User, name: &str, body: axum::body::Body) -> Result<Reply, ApiError> {
    let (_place, others) = Arriving::take(&user.id);
    jobs::room_besides(st, &user.id, others).await.map_err(failed)?; // before taking the upload
    let file = jobs::save_upload_stream(st, name, body).await.map_err(failed)?;
    let id = match jobs::create(st, &user.id, json!({ "file": file, "name": name })).await {
        Ok(id) => id,
        Err(e) => {
            let _ = tokio::fs::remove_file(crate::import::upload_dir(&st.cfg.data_dir).join(&file)).await;
            return Err(failed(e));
        }
    };
    let b = jobs::get(st, &id).await?.ok_or_else(|| ApiError::not_found("Import"))?;
    ok(json!({ "import": jobs::batch_json(&b) }))
}

pub async fn online(c: &Ctx) -> Result<Reply, ApiError> {
    let source = c.p.req("source")?;
    if !["spotify", "lastfm", "deezer"].contains(&source) {
        return Err(ApiError::new(10, "source is spotify, lastfm or deezer"));
    }
    // Spotify is the server's own Spotify login: only for the one it syncs for, or an admin
    if source == "spotify" && !c.user.is_admin && crate::listenbrainz::owner(&c.st).await.as_deref() != Some(c.user.id.as_str()) {
        return Err(ApiError::new(50, "Spotify here is someone else's account: only an admin can import from it"));
    }
    let input = json!({ "online": source, "user": c.p.get("account").unwrap_or("") });
    jobs::room_for_another(&c.st, &c.user.id).await.map_err(failed)?;
    let id = jobs::create(&c.st, &c.user.id, input).await.map_err(failed)?;
    let b = jobs::get(&c.st, &id).await?.ok_or_else(|| ApiError::not_found("Import"))?;
    ok(json!({ "import": jobs::batch_json(&b) }))
}

pub async fn list(c: &Ctx) -> Result<Reply, ApiError> {
    let all = jobs::list(&c.st, Some(&c.user.id)).await.map_err(failed)?;
    ok(json!({ "imports": { "import": all } }))
}

pub async fn get(c: &Ctx) -> Result<Reply, ApiError> {
    let b = mine(c).await?;
    ok(json!({ "import": jobs::batch_json(&b) }))
}

pub async fn start(c: &Ctx) -> Result<Reply, ApiError> {
    let b = mine(c).await?;
    let mut opts = c.p.get("kinds").map(Options::only).unwrap_or_default();
    opts.ask_missing = c.p.int("askMissing").unwrap_or(0).clamp(0, 500) as usize;
    jobs::start(&c.st, &b.id, opts).await.map_err(failed)?;
    get(c).await
}

pub async fn undo(c: &Ctx) -> Result<Reply, ApiError> {
    let b = mine(c).await?;
    jobs::undo(&c.st, &b.id).await.map_err(failed)?;
    get(c).await
}

pub async fn delete(c: &Ctx) -> Result<Reply, ApiError> {
    let b = mine(c).await?;
    jobs::discard(&c.st, &b.id).await.map_err(failed)?;
    ok(json!({}))
}
