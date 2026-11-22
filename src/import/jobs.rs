//! Imports as batches: read and match in the background (a preview: what was found, the match rate, what would
//! change), then, when started, applied with progress; undone later if wanted. The same calls serve the admin page,
//! Encore (`encore.import`) and the CLI.
//!
//! An uploaded export is kept in `<data_dir>/imports` only until it's imported (or a day at most), then deleted.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{
    Parsed,
    apply::{self, Options, Plan},
};
use crate::{
    api::AppState,
    db::{self, now},
};

/// One import runs at a time (they write a lot).
static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const KEEP_UPLOADS_SECS: u64 = 24 * 3600;

fn upload_path(st: &AppState, name: &str) -> Option<PathBuf> {
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') && !name.starts_with('.');
    ok.then(|| super::upload_dir(&st.cfg.data_dir).join(name))
}

/// Keep an upload until it's imported: its file name in the imports folder. `hint`: the name it was sent with
/// (only its extension is kept, so the format can be told).
pub async fn save_upload(st: &AppState, hint: &str, body: &[u8]) -> Result<String> {
    let ext: String = hint.rsplit_once('.').map(|(_, e)| e.to_lowercase()).filter(|e| e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or_else(|| "zip".into());
    let name = format!("{}.{ext}", db::random_id());
    let dir = super::upload_dir(&st.cfg.data_dir);
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::write(dir.join(&name), body).await.context("saving the upload")?;
    Ok(name)
}

/// The largest export taken (a Google Takeout can be big).
pub const MAX_UPLOAD: u64 = 2 << 30;

/// Like `save_upload`, written as it arrives.
pub async fn save_upload_stream(st: &AppState, hint: &str, body: axum::body::Body) -> Result<String> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let name = save_upload(st, hint, b"").await?;
    let path = super::upload_dir(&st.cfg.data_dir).join(&name);
    let res = async {
        let mut f = tokio::fs::File::create(&path).await?;
        let mut total = 0u64;
        let mut s = body.into_data_stream();
        while let Some(chunk) = s.next().await {
            let c = chunk.context("receiving the upload")?;
            total += c.len() as u64;
            if total > MAX_UPLOAD {
                bail!("that file is bigger than 2 GB");
            }
            f.write_all(&c).await?;
        }
        f.flush().await?;
        if total == 0 {
            bail!("the upload was empty");
        }
        anyhow::Ok(())
    }
    .await;
    if let Err(e) = res {
        let _ = tokio::fs::remove_file(&path).await;
        return Err(e);
    }
    Ok(name)
}

/// Uploads left behind (never imported) go after a day.
pub async fn tidy_uploads(st: &AppState) {
    let dir = super::upload_dir(&st.cfg.data_dir);
    let Ok(mut rd) = tokio::fs::read_dir(&dir).await else { return };
    while let Ok(Some(e)) = rd.next_entry().await {
        let old = e.metadata().await.ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|a| a.as_secs() > KEEP_UPLOADS_SECS);
        if old {
            let _ = tokio::fs::remove_file(e.path()).await;
        }
    }
}

/// What an input names: {"file": …} or {"online": "spotify" | "lastfm" | "deezer", "user": …}.
pub async fn read(st: &AppState, input: &Value) -> Result<Parsed> {
    if let Some(name) = input["file"].as_str() {
        let path = upload_path(st, name).context("bad file name")?;
        let sent = input["name"].as_str().map(String::from);
        return tokio::task::spawn_blocking(move || super::parse_upload(&path, sent.as_deref())).await?;
    }
    if let Some(path) = input["path"].as_str() {
        let path = PathBuf::from(path);
        return tokio::task::spawn_blocking(move || super::parse_path(&path)).await?;
    }
    let user = input["user"].as_str().unwrap_or("");
    match input["online"].as_str() {
        Some("spotify") => super::online::spotify(st).await,
        Some("lastfm") => super::online::lastfm(st, user).await,
        Some("deezer") => super::online::deezer(st, user).await,
        _ => bail!("nothing to import"),
    }
}

/// Match to Conductor's library, with ISRCs from Spotify for Spotify songs the names didn't match.
pub async fn plan(st: &AppState, p: &Parsed, opts: &Options) -> Result<Plan> {
    let lib = apply::conductor_library(&st.db).await?;
    let none = std::collections::HashMap::new();
    let first = apply::plan(p, &lib, opts, &none);
    let unmatched: Vec<&super::Song> = first.missing.iter().map(|m| &m.song).filter(|s| s.spotify.is_some() && s.isrc.is_none()).collect();
    if unmatched.is_empty() {
        return Ok(first);
    }
    let isrcs = super::online::spotify_isrcs(st, &unmatched).await;
    if isrcs.is_empty() {
        return Ok(first);
    }
    Ok(apply::plan(p, &lib, opts, &isrcs))
}

async fn set(st: &AppState, id: &str, status: &str, report: Option<&Value>, error: Option<&str>) {
    let _ = sqlx::query(
        "UPDATE import_batch SET status = ?, report_json = COALESCE(?, report_json), error = ?,
            done_at = CASE WHEN ? IN ('done', 'failed', 'undone') THEN ? ELSE done_at END WHERE id = ?",
    )
    .bind(status)
    .bind(report.map(Value::to_string))
    .bind(error)
    .bind(status)
    .bind(now())
    .bind(id)
    .execute(&st.db)
    .await;
}

async fn drop_upload(st: &AppState, input: &Value) {
    if let Some(p) = input["file"].as_str().and_then(|n| upload_path(st, n)) {
        let _ = tokio::fs::remove_file(p).await;
    }
}

/// A new import for `user`: read and matched in the background; its preview is in the batch's report.
pub async fn create(st: &Arc<AppState>, user: &str, input: Value) -> Result<String> {
    tidy_uploads(st).await;
    let id = db::random_id()[..16].to_string();
    sqlx::query("INSERT INTO import_batch (id, user_id, input_json, status, created_at) VALUES (?, ?, ?, 'reading', ?)")
        .bind(&id)
        .bind(user)
        .bind(input.to_string())
        .bind(now())
        .execute(&st.db)
        .await?;
    let (st, id2, user) = (st.clone(), id.clone(), user.to_string());
    tokio::spawn(async move {
        let res = async {
            let p = read(&st, &input).await?;
            let plan = plan(&st, &p, &Options::default()).await?;
            let changes = apply::preview(&st.db, &user, &plan).await?;
            let mut report = plan.report;
            report.changes = Some(changes);
            anyhow::Ok((plan.source, report))
        }
        .await;
        match res {
            Ok((source, report)) => {
                let _ = sqlx::query("UPDATE import_batch SET source = ? WHERE id = ?").bind(&source).bind(&id2).execute(&st.db).await;
                set(&st, &id2, "preview", Some(&serde_json::to_value(&report).unwrap_or_default()), None).await;
            }
            Err(e) => {
                warn!(batch = %id2, "import couldn't be read: {e:#}");
                drop_upload(&st, &input).await;
                set(&st, &id2, "failed", None, Some(&format!("{e:#}"))).await;
            }
        }
    });
    Ok(id)
}

/// Import a previewed batch (in the background) with these options.
pub async fn start(st: &Arc<AppState>, id: &str, opts: Options) -> Result<()> {
    let row: Option<(String, String, String)> =
        sqlx::query_as("SELECT user_id, input_json, status FROM import_batch WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    let Some((user, input, status)) = row else { bail!("no such import") };
    if status != "preview" {
        bail!("this import is {status}");
    }
    sqlx::query("UPDATE import_batch SET status = 'running', progress = 0, options_json = ? WHERE id = ?")
        .bind(serde_json::to_string(&opts)?)
        .bind(id)
        .execute(&st.db)
        .await?;
    let input: Value = serde_json::from_str(&input).unwrap_or_default();
    let (st, id) = (st.clone(), id.to_string());
    tokio::spawn(async move {
        let res = run(&st, &id, &user, &input, &opts).await;
        drop_upload(&st, &input).await;
        if let Err(e) = res {
            warn!(batch = %id, "import failed: {e:#}");
            set(&st, &id, "failed", None, Some(&format!("{e:#}"))).await;
        }
    });
    Ok(())
}

/// Read, match and apply now (the CLI, and `start`'s background work).
pub async fn run(st: &AppState, id: &str, user: &str, input: &Value, opts: &Options) -> Result<Value> {
    let _one = ONE.lock().await;
    let p = read(st, input).await?;
    let plan = plan(st, &p, opts).await?;
    let changes = apply::apply(st, id, user, &plan, opts).await?;
    let mut report = plan.report;
    info!(batch = %id, plays = changes.plays, hearts = changes.hearts, playlists = changes.playlists_new + changes.playlists_updated, "import done");
    report.changes = Some(changes);
    let report = serde_json::to_value(&report)?;
    sqlx::query("UPDATE import_batch SET source = ? WHERE id = ?").bind(&plan.source).bind(id).execute(&st.db).await?;
    set(st, id, "done", Some(&report), None).await;
    Ok(report)
}

pub async fn undo(st: &AppState, id: &str) -> Result<()> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM import_batch WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    match status.as_deref() {
        Some("done") => {
            let _one = ONE.lock().await;
            apply::undo(st, id).await
        }
        Some(s) => bail!("this import is {s}: only a finished one can be undone"),
        None => bail!("no such import"),
    }
}

/// Forget a preview (or a failed import); its upload goes.
pub async fn discard(st: &AppState, id: &str) -> Result<()> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT input_json, status FROM import_batch WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    let Some((input, status)) = row else { bail!("no such import") };
    if !["preview", "failed", "reading"].contains(&status.as_str()) {
        bail!("this import is {status}: undo it instead");
    }
    drop_upload(st, &serde_json::from_str(&input).unwrap_or_default()).await;
    sqlx::query("DELETE FROM import_batch WHERE id = ?").bind(id).execute(&st.db).await?;
    Ok(())
}

pub fn batch_json(r: &BatchRow) -> Value {
    let input: Value = serde_json::from_str(&r.input_json).unwrap_or_default();
    json!({
        "id": r.id, "userId": r.user_id, "source": r.source, "status": r.status, "progress": r.progress,
        "created": r.created_at, "done": r.done_at, "error": r.error,
        "from": input["online"].as_str().unwrap_or(if input["file"].is_string() || input["path"].is_string() { "file" } else { "" }),
        "report": serde_json::from_str::<Value>(&r.report_json).unwrap_or_default(),
        "options": serde_json::from_str::<Value>(&r.options_json).unwrap_or_default(),
    })
}

#[derive(sqlx::FromRow)]
pub struct BatchRow {
    pub id: String,
    pub user_id: String,
    pub source: String,
    pub input_json: String,
    pub options_json: String,
    pub status: String,
    pub progress: f64,
    pub report_json: String,
    pub error: Option<String>,
    pub created_at: i64,
    pub done_at: Option<i64>,
}

pub async fn get(st: &AppState, id: &str) -> Result<Option<BatchRow>> {
    Ok(sqlx::query_as("SELECT * FROM import_batch WHERE id = ?").bind(id).fetch_optional(&st.db).await?)
}

/// Imports, newest first (one user's, or everyone's).
pub async fn list(st: &AppState, user: Option<&str>) -> Result<Vec<Value>> {
    let rows: Vec<BatchRow> = match user {
        Some(u) => sqlx::query_as("SELECT * FROM import_batch WHERE user_id = ? ORDER BY created_at DESC LIMIT 100").bind(u).fetch_all(&st.db).await?,
        None => sqlx::query_as("SELECT * FROM import_batch ORDER BY created_at DESC LIMIT 100").fetch_all(&st.db).await?,
    };
    Ok(rows.iter().map(|r| {
        let mut v = batch_json(r);
        // the list leaves out the long parts of the report
        if let Some(rep) = v["report"].as_object_mut() {
            rep.remove("missing");
            rep.remove("albumsMissing");
            rep.remove("artistsMissing");
        }
        v
    }).collect())
}
