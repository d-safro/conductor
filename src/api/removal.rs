//! `encore.removeSongs`: take songs out of every library for good (admins only). `getSongRemoval` says what would
//! go: each song and its other copies (the same title by the same artist, as long within 3 s, in any library), with
//! their files. `removeSongs` deletes exactly the songs it's given (the apps send the ones `getSongRemoval` listed,
//! after asking you), and only with `confirm=permanently`: the files (and a `.lrc` beside one), then each song from
//! every playlist and play queue; a song Conductor downloaded is set aside, so it isn't fetched again (bring it back
//! on the requests page). Every app hears it (the `songsRemoved` event) and drops its own download of it.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::{ApiError, Ctx, Reply, ok};
use crate::{catalog::text::same_artist, db::now, library, live::Audience};

fn admins_only(c: &Ctx) -> Result<(), ApiError> {
    if c.user.is_admin { Ok(()) } else { Err(ApiError::new(50, "Only admins can remove songs")) }
}

type Track = (String, String, String, String, i64, String, i64, i64);
const COLS: &str = "id, title, artist_name, album_name, library_id, rel_path, size, duration_ms";

async fn track(c: &Ctx, id: &str) -> Result<Option<Track>, ApiError> {
    Ok(sqlx::query_as(sqlx::AssertSqlSafe(format!("SELECT {COLS} FROM track WHERE id = ? AND missing_since IS NULL")))
        .bind(id)
        .fetch_optional(&c.st.db)
        .await?)
}

fn same_song(a: &Track, b: &Track) -> bool {
    a.1.trim().to_lowercase() == b.1.trim().to_lowercase() && same_artist(&a.2, &b.2) && (a.7 - b.7).abs() <= 3000
}

fn entry(c: &Ctx, t: &Track) -> Value {
    let lib = c.st.libs.iter().find(|(id, _)| *id == t.4).map(|(_, l)| l.name.clone()).unwrap_or_default();
    json!({ "id": t.0, "title": t.1, "artist": t.2, "album": t.3, "library": lib, "path": t.5, "size": t.6,
            "duration": t.7 / 1000 })
}

/// What removing these songs (`id`, once each) would take away: each one and its other copies.
pub async fn get_song_removal(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    let ids = c.p.all("id");
    if ids.is_empty() {
        return Err(ApiError::new(10, "Missing parameter: id"));
    }
    let mut seen = HashSet::new();
    let mut out = vec![];
    for id in ids {
        let Some(t) = track(c, id).await? else { return Err(ApiError::not_found("Song")) };
        let near: Vec<Track> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {COLS} FROM track WHERE missing_since IS NULL AND duration_ms BETWEEN ? AND ? ORDER BY id = ? DESC, library_id, rel_path"
        )))
        .bind(t.7 - 3000)
        .bind(t.7 + 3000)
        .bind(&t.0)
        .fetch_all(&c.st.db)
        .await?;
        for x in near.iter().filter(|x| x.0 == t.0 || same_song(&t, x)) {
            if seen.insert(x.0.clone()) {
                out.push(entry(c, x));
            }
        }
    }
    let size: i64 = out.iter().map(|e| e["size"].as_i64().unwrap_or(0)).sum();
    ok(json!({ "songRemoval": { "song": out, "count": out.len(), "size": size } }))
}

/// Delete these songs (`id`, once each) for good. `confirm=permanently` is required.
pub async fn remove_songs(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    if c.p.get("confirm") != Some("permanently") {
        return Err(ApiError::new(10, "Removing songs deletes their files: send confirm=permanently"));
    }
    if c.st.cfg.read_only {
        return Err(ApiError::new(0, "The server is read-only: nothing is deleted"));
    }
    let mut ids: Vec<String> = vec![];
    for id in c.p.all("id") {
        if !ids.iter().any(|x| x == id) {
            ids.push(id.to_string());
        }
    }
    if ids.is_empty() {
        return Err(ApiError::new(10, "Missing parameter: id"));
    }
    // every file first: none is deleted unless all of them can be
    let mut files = vec![];
    for id in &ids {
        let Some(t) = track(c, id).await? else { return Err(ApiError::not_found("Song")) };
        let lib = c.st.libs.iter().find(|(lid, _)| *lid == t.4).map(|(_, l)| l.clone());
        let Some(lib) = lib.filter(library::is_online) else {
            return Err(ApiError::new(0, format!("The music drive with \"{}\" isn't connected", t.1)));
        };
        let path = library::track_path(&c.st.libs, t.4, &t.5).ok_or_else(|| ApiError::not_found("Song"))?;
        if !path.starts_with(&lib.path) {
            return Err(ApiError::not_found("Song"));
        }
        files.push((t, path));
    }
    let mut removed = vec![];
    for (t, path) in &files {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(file = %path.display(), "couldn't remove a song: {e}");
                continue;
            }
        }
        let _ = tokio::fs::remove_file(path.with_extension("lrc")).await;
        if let Some(dir) = path.parent() {
            let _ = tokio::fs::remove_dir(dir).await; // only when nothing else is in it
        }
        tracing::info!(song = %format!("{} - {}", t.2, t.1), file = %t.5, by = %c.user.name, "removed for good");
        removed.push(t.0.clone());
    }
    let changed = forget(c, &removed).await?;
    for (owner, pid) in changed {
        c.st.live.playlist_changed(&owner, &pid, false);
    }
    if !removed.is_empty() {
        library::refresh_counts(&c.st.db).await?;
        c.st.catalog.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        c.st.live.publish("songsRemoved", Audience::Everyone, json!({ "ids": removed }));
    }
    let failed: Vec<&str> = ids.iter().filter(|i| !removed.contains(i)).map(String::as_str).collect();
    ok(json!({ "songsRemoved": { "removed": removed, "failed": failed } }))
}

/// The library forgets these songs: gone (as a deleted file is), out of every playlist and play queue, and a
/// download set aside. Returns the playlists changed, (owner, id).
async fn forget(c: &Ctx, ids: &[String]) -> Result<Vec<(String, String)>, ApiError> {
    let gone: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let mut tx = c.st.db.begin().await?;
    let mut changed = vec![];
    for id in ids {
        sqlx::query("UPDATE track SET missing_since = ? WHERE id = ?").bind(now()).bind(id).execute(&mut *tx).await?;
        // downloaded by Conductor: not fetched again (unless it's brought back from "set aside"), and not hearted or
        // added to a playlist when it would arrive
        let keys: Vec<String> =
            sqlx::query_scalar("SELECT key FROM request WHERE local_id = ?").bind(id).fetch_all(&mut *tx).await?;
        for key in keys {
            sqlx::query(
                "UPDATE request SET skipped_from = 'notfound', status = 'skipped', local_id = NULL, rel = NULL, keep = 0
                  WHERE key = ?",
            )
            .bind(&key)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM request_want WHERE key = ?").bind(&key).execute(&mut *tx).await?;
        }
        let pls: Vec<(String, String)> = sqlx::query_as(
            "SELECT DISTINCT p.id, p.owner_id FROM playlist_entry e JOIN playlist p ON p.id = e.playlist_id WHERE e.track_id = ?",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await?;
        for (pid, owner) in pls {
            if !changed.iter().any(|(_, p)| *p == pid) {
                changed.push((owner, pid));
            }
        }
    }
    for (_, pid) in &changed {
        let tracks: Vec<String> = sqlx::query_scalar("SELECT track_id FROM playlist_entry WHERE playlist_id = ? ORDER BY position")
            .bind(pid)
            .fetch_all(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(pid).execute(&mut *tx).await?;
        for (i, t) in tracks.iter().filter(|t| !gone.contains(t.as_str())).enumerate() {
            sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
                .bind(pid)
                .bind(i as i64)
                .bind(t)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(pid).execute(&mut *tx).await?;
    }
    let queues: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT user_id, track_ids, current FROM play_queue").fetch_all(&mut *tx).await?;
    for (user, json_ids, current) in queues {
        let q: Vec<String> = serde_json::from_str(&json_ids).unwrap_or_default();
        if !q.iter().any(|t| gone.contains(t.as_str())) && !current.as_deref().is_some_and(|t| gone.contains(t)) {
            continue;
        }
        let kept: Vec<&String> = q.iter().filter(|t| !gone.contains(t.as_str())).collect();
        let cur = current.filter(|t| !gone.contains(t.as_str()));
        sqlx::query("UPDATE play_queue SET track_ids = ?, current = ? WHERE user_id = ?")
            .bind(serde_json::to_string(&kept).unwrap_or_else(|_| "[]".into()))
            .bind(cur)
            .bind(user)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(changed)
}
