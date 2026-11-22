//! Songs on trial: songs you played (or queued) but don't have are downloaded into the music folder's `Trial/`
//! and listed in your "On trial" playlist. Heart one, add it to another playlist or rate it 4+ to keep it (it moves
//! to `Songs/` and keeps its id, hearts and playlists); take it out of "On trial" to delete it now; the rest are
//! deleted `trial_hours` after they arrive.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;

use crate::{
    api::AppState,
    db::{self, now},
    library,
};

pub const PLAYLIST: &str = "On trial";
pub const FOLDER: &str = "Trial";

pub fn note(hours: f64) -> String {
    format!(
        "Songs you played that weren't in your library. Heart one or add it to a playlist to keep it; remove it from here \
         to delete it now. The rest are deleted {hours} hours after they arrive."
    )
}

fn setting_key(user: &str) -> String {
    format!("trial_playlist:{user}")
}

/// The user's "On trial" playlist (made when `create` and it doesn't exist).
pub async fn playlist_id(st: &AppState, user: &str, create: bool) -> Result<Option<String>> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT value FROM setting WHERE key = ?").bind(setting_key(user)).fetch_optional(&st.db).await?;
    if let Some(id) = id {
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist WHERE id = ?").bind(&id).fetch_one(&st.db).await?;
        if exists > 0 {
            return Ok(Some(id));
        }
    }
    if !create {
        return Ok(None);
    }
    let id = db::random_id();
    sqlx::query("INSERT INTO playlist (id, owner_id, name, comment, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(user)
        .bind(PLAYLIST)
        .bind(note(st.cfg.trial_hours))
        .bind(now())
        .bind(now())
        .execute(&st.db)
        .await?;
    sqlx::query("INSERT OR REPLACE INTO setting (key, value) VALUES (?, ?)")
        .bind(setting_key(user))
        .bind(&id)
        .execute(&st.db)
        .await?;
    Ok(Some(id))
}

fn root(st: &AppState) -> Option<(i64, PathBuf)> {
    let pick = st.cfg.import_library.as_deref();
    st.libs.iter().find(|(_, l)| pick.is_none_or(|p| l.name.eq_ignore_ascii_case(p))).map(|(id, l)| (*id, l.path.clone()))
}

/// Remove empty folders from `dir` up to (not including) the Trial folder.
fn prune(trial_root: &Path, mut dir: PathBuf) {
    while dir.starts_with(trial_root) && dir != trial_root {
        if std::fs::remove_dir(&dir).is_err() {
            return; // not empty
        }
        if !dir.pop() {
            return;
        }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    key: String,
    title: String,
    local_id: String,
    rel: Option<String>,
    trial_user: Option<String>,
    keep: bool,
    arrived_at: Option<i64>,
    listed: bool,
    /// fetched for one of the user's mixes: its own time on trial, not listed in "On trial"
    mix: Option<String>,
    trial_hours: Option<f64>,
}

/// Move a kept song from `folder`/ (Trial, Discovery) to Songs/; the library keeps its id, hearts and playlists.
/// Returns its new place.
pub async fn move_into_songs(st: &AppState, local_id: &str, rel: &str, folder: &str) -> Result<String> {
    let Some((lib, root)) = root(st) else { return Ok(rel.to_string()) };
    let Some(rest) = rel.strip_prefix(&format!("{folder}/")) else { return Ok(rel.to_string()) };
    let new_rel = format!("Songs/{rest}");
    let (src, dst) = (root.join(rel), root.join(&new_rel));
    if src.is_file() {
        if let Some(d) = dst.parent() {
            tokio::fs::create_dir_all(d).await?;
        }
        if dst.exists() {
            tokio::fs::remove_file(&src).await?; // already have that file: drop this copy
        } else if tokio::fs::rename(&src, &dst).await.is_err() {
            tokio::fs::copy(&src, &dst).await?;
            tokio::fs::remove_file(&src).await?;
        }
        if let Some(p) = src.parent() {
            prune(&root.join(folder), p.to_path_buf());
        }
    }
    let mut tx = st.db.begin().await?;
    sqlx::query("UPDATE track SET rel_path = ? WHERE id = ?").bind(&new_rel).bind(local_id).execute(&mut *tx).await?;
    sqlx::query("INSERT OR REPLACE INTO id_override (kind, key, id) VALUES ('track', ?, ?)")
        .bind(library::track_key(lib, &new_rel))
        .bind(local_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(new_rel)
}

/// Delete a downloaded file, but never anything outside `folder`/; empty folders go too.
pub async fn delete_inside(st: &AppState, rel: Option<&str>, folder: &str) {
    if let (Some((_, root)), Some(rel)) = (root(st), rel)
        && rel.starts_with(&format!("{folder}/"))
        && !rel.contains("..")
    {
        let f = root.join(rel);
        let _ = tokio::fs::remove_file(&f).await;
        if let Some(p) = f.parent() {
            prune(&root.join(folder), p.to_path_buf());
        }
    }
}

async fn keep(st: &AppState, r: &Row) -> Result<()> {
    let new_rel = move_into_songs(st, &r.local_id, r.rel.as_deref().unwrap_or(""), FOLDER).await?;
    sqlx::query("UPDATE request SET trial = 0, keep = 0, kept_at = ?, folder = 'Songs', rel = ? WHERE key = ?")
        .bind(now())
        .bind(&new_rel)
        .bind(&r.key)
        .execute(&st.db)
        .await?;
    info!(song = %r.title, "kept from trial");
    Ok(())
}

async fn drop_it(st: &AppState, r: &Row, why: &str) -> Result<()> {
    delete_inside(st, r.rel.as_deref(), FOLDER).await;
    sqlx::query("DELETE FROM request WHERE key = ?").bind(&r.key).execute(&st.db).await?;
    info!(song = %r.title, "deleted from trial ({why})");
    Ok(())
}

/// Keep the liked ones, delete the expired or rejected ones, list the rest in each user's "On trial".
pub async fn tick(st: &AppState) -> Result<()> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT key, title, local_id, rel, trial_user, keep, arrived_at, listed, mix, trial_hours FROM request
         WHERE trial = 1 AND local_id IS NOT NULL ORDER BY arrived_at",
    )
    .fetch_all(&st.db)
    .await?;
    let mut users: Vec<String> =
        sqlx::query_scalar("SELECT substr(key, 16) FROM setting WHERE key LIKE 'trial_playlist:%'").fetch_all(&st.db).await?;
    users.extend(rows.iter().filter_map(|r| r.trial_user.clone()));
    users.sort();
    users.dedup();
    let mut changed = false;
    let limit = (st.cfg.trial_hours * 3600.0) as i64;
    for user in users {
        let pid = playlist_id(st, &user, false).await?;
        let listed_now: Vec<String> = match &pid {
            Some(p) => {
                sqlx::query_scalar("SELECT track_id FROM playlist_entry WHERE playlist_id = ?").bind(p).fetch_all(&st.db).await?
            }
            None => vec![],
        };
        let mut stay: Vec<&Row> = vec![];
        for r in rows.iter().filter(|r| r.trial_user.as_deref() == Some(user.as_str())) {
            // in a playlist of someone's own (not "On trial", not one a mix made)
            let other_lists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM playlist_entry WHERE track_id = ? AND playlist_id != ?
                 AND playlist_id NOT IN (SELECT playlist_id FROM mix_playlist)",
            )
            .bind(&r.local_id)
            .bind(pid.as_deref().unwrap_or(""))
            .fetch_one(&st.db)
            .await?;
            let limit = r.trial_hours.map(|h| (h * 3600.0) as i64).unwrap_or(limit);
            let liked: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM annotation WHERE user_id = ? AND item_kind = 'track' AND item_id = ? AND (starred_at IS NOT NULL OR rating >= 4)",
            )
            .bind(&user)
            .bind(&r.local_id)
            .fetch_one(&st.db)
            .await?;
            if r.keep || other_lists > 0 || liked > 0 {
                keep(st, r).await?;
                changed = true;
            } else if now() - r.arrived_at.unwrap_or_else(now) > limit || (r.trial_hours == Some(0.0)) {
                drop_it(st, r, "not kept in time").await?;
                changed = true;
            } else if r.listed && r.mix.is_none() && pid.is_some() && !listed_now.contains(&r.local_id) {
                drop_it(st, r, "taken out of On trial").await?; // you didn't like it
                changed = true;
            } else if r.mix.is_none() {
                stay.push(r);
            }
        }
        // "On trial" holds exactly the songs still on trial
        let want: Vec<&str> = stay.iter().map(|r| r.local_id.as_str()).collect();
        if want.is_empty() && pid.is_none() {
            continue;
        }
        let pid = match pid {
            Some(p) => p,
            None => playlist_id(st, &user, true).await?.unwrap(),
        };
        if listed_now != want {
            let mut tx = st.db.begin().await?;
            sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(&pid).execute(&mut *tx).await?;
            for (i, id) in want.iter().enumerate() {
                sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
                    .bind(&pid)
                    .bind(i as i64)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(&pid).execute(&mut *tx).await?;
            tx.commit().await?;
        }
        for r in &stay {
            if !r.listed {
                sqlx::query("UPDATE request SET listed = 1 WHERE key = ?").bind(&r.key).execute(&st.db).await?;
            }
        }
    }
    if changed {
        library::scan(&st.db, &st.libs, &st.scan, false).await?;
        st.catalog.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}
