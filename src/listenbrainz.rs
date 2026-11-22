//! ListenBrainz: your plays are sent there (as Navidrome did; it's what its recommendations are made from), and its
//! playlists made for you become discovery playlists (what Explo did): "Weekly Exploration", "Weekly Jams",
//! "Daily Jams". Songs you have go straight in; songs you don't are downloaded into Discovery/ and join the playlist
//! as they arrive. When the next list comes, last time's songs nobody kept (heart, rate 4+, or add to another
//! playlist) are deleted, unless another discovery playlist still has them; kept ones move to Songs/.

use std::{sync::Arc, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{
    api::AppState,
    catalog::{self, trial},
    db::{self, now},
};

pub const FOLDER: &str = "Discovery";

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("http client")
}

fn title_of(kind: &str) -> &'static str {
    match kind {
        "weekly-jams" => "Weekly Jams",
        "daily-jams" => "Daily Jams",
        _ => "Weekly Exploration",
    }
}

/// The Conductor user whose plays are sent and who owns the discovery playlists.
pub async fn owner(st: &AppState) -> Option<String> {
    match &st.cfg.listenbrainz_for {
        Some(name) => {
            sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(name).fetch_optional(&st.db).await.ok().flatten()
        }
        None => sqlx::query_scalar("SELECT id FROM user WHERE is_admin = 1 ORDER BY created_at LIMIT 1")
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten(),
    }
}

async fn submit(st: &AppState, body: &Value) -> Result<()> {
    let Some(token) = &st.cfg.listenbrainz_token else { return Ok(()) };
    let r = http()
        .post(format!("{}/1/submit-listens", st.cfg.listenbrainz_url.trim_end_matches('/')))
        .header("Authorization", format!("Token {token}"))
        .json(body)
        .send()
        .await?;
    if !r.status().is_success() {
        bail!("ListenBrainz answered {}", r.status());
    }
    Ok(())
}

/// A play (or "playing now") for ListenBrainz, if it's the user plays are sent for. Failed plays are kept and sent later.
pub async fn scrobble(st: &AppState, user_id: &str, track_id: &str, at: i64, playing_now: bool) {
    if st.cfg.listenbrainz_token.is_none() || owner(st).await.as_deref() != Some(user_id) {
        return;
    }
    type Row = (String, String, String, i64, Option<String>);
    let row: Option<Row> = sqlx::query_as("SELECT title, artist_name, album_name, duration_ms, mbid FROM track WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    let Some((title, artist, album, ms, mbid)) = row else { return };
    let mut info =
        json!({ "duration_ms": ms, "submission_client": "Conductor", "submission_client_version": env!("CARGO_PKG_VERSION") });
    if let Some(m) = mbid {
        info["recording_mbid"] = m.into();
    }
    let meta = json!({ "artist_name": artist, "track_name": title, "release_name": album, "additional_info": info });
    let body = if playing_now {
        json!({ "listen_type": "playing_now", "payload": [{ "track_metadata": meta }] })
    } else {
        json!({ "listen_type": "single", "payload": [{ "listened_at": at, "track_metadata": meta }] })
    };
    if let Err(e) = submit(st, &body).await
        && !playing_now
    {
        warn!("ListenBrainz: {e:#}; the play is kept and sent later");
        let _ = sqlx::query("INSERT INTO lb_queue (payload, added_at) VALUES (?, ?)")
            .bind(body.to_string())
            .bind(now())
            .execute(&st.db)
            .await;
    }
}

async fn resend(st: &AppState) {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, payload FROM lb_queue ORDER BY id LIMIT 200").fetch_all(&st.db).await.unwrap_or_default();
    for (id, p) in rows {
        let ok = match serde_json::from_str::<Value>(&p) {
            Ok(v) => submit(st, &v).await.is_ok(),
            Err(_) => true,
        };
        if ok {
            let _ = sqlx::query("DELETE FROM lb_queue WHERE id = ?").bind(id).execute(&st.db).await;
        } else {
            let _ = sqlx::query("UPDATE lb_queue SET tries = tries + 1 WHERE id = ?").bind(id).execute(&st.db).await;
            break; // still unreachable: try again later
        }
    }
}

// ── discovery playlists ──

async fn lb_get(st: &AppState, path: &str) -> Result<Value> {
    let mut r = http().get(format!("{}/1/{path}", st.cfg.listenbrainz_url.trim_end_matches('/')));
    if let Some(t) = &st.cfg.listenbrainz_token {
        r = r.header("Authorization", format!("Token {t}"));
    }
    let r = r.send().await?;
    if !r.status().is_success() {
        bail!("ListenBrainz answered {}", r.status());
    }
    Ok(r.json().await?)
}

/// The newest ListenBrainz playlist made for the user of this kind: (its id, [(title, artist, album, duration s)]).
async fn newest(st: &AppState, lb_user: &str, kind: &str) -> Result<Option<(String, Vec<(String, String, String, i64)>)>> {
    let list = lb_get(st, &format!("user/{lb_user}/playlists/createdfor?count=100")).await?;
    let want = title_of(kind).to_lowercase();
    let mut best: Option<(String, String)> = None; // (date, id)
    for p in list["playlists"].as_array().into_iter().flatten() {
        let p = &p["playlist"];
        let title = p["title"].as_str().unwrap_or("").to_lowercase();
        if !title.starts_with(&want) {
            continue;
        }
        let id = p["identifier"].as_str().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
        let date = p["date"].as_str().unwrap_or("").to_string();
        if !id.is_empty() && best.as_ref().is_none_or(|(d, _)| date > *d) {
            best = Some((date, id));
        }
    }
    let Some((_, id)) = best else { return Ok(None) };
    let pl = lb_get(st, &format!("playlist/{id}")).await?;
    let tracks = pl["playlist"]["track"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            (
                t["title"].as_str().unwrap_or("").to_string(),
                t["creator"].as_str().unwrap_or("").to_string(),
                t["album"].as_str().unwrap_or("").to_string(),
                t["duration"].as_i64().unwrap_or(0) / 1000,
            )
        })
        .filter(|t| !t.0.is_empty())
        .collect();
    Ok(Some((id, tracks)))
}

async fn playlist_for(st: &AppState, kind: &str, owner: &str) -> Result<String> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT d.playlist_id FROM discovery d JOIN playlist p ON p.id = d.playlist_id WHERE d.kind = ?")
            .bind(kind)
            .fetch_optional(&st.db)
            .await?;
    if let Some(id) = id {
        return Ok(id);
    }
    let id = db::random_id();
    sqlx::query("INSERT INTO playlist (id, owner_id, name, comment, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(owner)
        .bind(title_of(kind))
        .bind("From ListenBrainz. Songs you don't keep (heart, rate 4+ or add to another playlist) go when the next list comes.")
        .bind(now())
        .bind(now())
        .execute(&st.db)
        .await?;
    sqlx::query("INSERT OR REPLACE INTO discovery (kind, playlist_id) VALUES (?, ?)")
        .bind(kind)
        .bind(&id)
        .execute(&st.db)
        .await?;
    Ok(id)
}

/// Fill one discovery playlist from the newest ListenBrainz list (when it's a new one).
pub async fn refresh(st: &AppState, kind: &str) -> Result<()> {
    let (Some(lb_user), Some(owner)) = (st.cfg.listenbrainz_user.clone(), owner(st).await) else { return Ok(()) };
    let Some((source, tracks)) = newest(st, &lb_user, kind).await? else { return Ok(()) };
    let pid = playlist_for(st, kind, &owner).await?;
    let last: String = sqlx::query_scalar("SELECT source FROM discovery WHERE kind = ?").bind(kind).fetch_one(&st.db).await?;
    if last == source {
        return Ok(());
    }
    let mut have = vec![];
    let mut wanted = vec![];
    sqlx::query("DELETE FROM request_want WHERE playlist_id = ?").bind(&pid).execute(&st.db).await?;
    for (title, artist, _album, secs) in &tracks {
        if let Some(mine) = catalog::find(st, title, artist, Some(*secs).filter(|s| *s > 0)).await {
            have.push(mine.id);
            continue;
        }
        let q = format!(r#"artist:"{artist}" track:"{title}""#);
        let hit = st.catalog.dz.list("search/track", &[("q", &q), ("limit", "1")]).await.into_iter().next();
        let hit = match hit {
            Some(h) => Some(h),
            None => st.catalog.dz.list("search", &[("q", &format!("{artist} {title}")), ("limit", "1")]).await.into_iter().next(),
        };
        let Some(t) = hit.filter(|t| catalog::text::same_artist(artist, t["artist"]["name"].as_str().unwrap_or(""))) else {
            continue;
        };
        wanted.push(format!("dz-t-{}", t["id"]));
    }
    // the songs you have go in first; only then are the others asked for. (Asked for first, one could arrive and
    // join the playlist before it was filled, and the filling dropped it again, or put it in the same place as
    // another song.)
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(&pid).execute(&mut *tx).await?;
    for (i, id) in have.iter().enumerate() {
        sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
            .bind(&pid)
            .bind(i as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(&pid).execute(&mut *tx).await?;
    tx.commit().await?;
    // last time's songs you didn't keep go (unless another discovery playlist still has them); kept ones move into Songs
    clean_up(st).await?;
    let mut asked = 0;
    for key in &wanted {
        // filed into Discovery/ from the start (set afterwards, a quick download could land in Songs/ and stay)
        if catalog::ask_synced(st, key, &owner, false, Some(&pid), Some(FOLDER)).await? {
            asked += 1;
        }
    }
    sqlx::query("UPDATE discovery SET source = ?, filled_at = ? WHERE kind = ?").bind(&source).bind(now()).bind(kind).execute(&st.db).await?;
    info!(playlist = title_of(kind), have = have.len(), asked, "discovery playlist filled from ListenBrainz");
    Ok(())
}

/// Discovery songs that arrived: kept ones (hearted or rated 4+ by anyone, in a playlist that isn't a discovery one)
/// move into Songs; the rest are deleted (only ever inside Discovery/), once no discovery playlist has them.
async fn clean_up(st: &AppState) -> Result<()> {
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT key, local_id, rel FROM request WHERE folder = ? AND local_id IS NOT NULL")
            .bind(FOLDER)
            .fetch_all(&st.db)
            .await?;
    let mut changed = false;
    for (key, local, rel) in rows {
        let liked: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM annotation WHERE item_kind = 'track' AND item_id = ? AND (starred_at IS NOT NULL OR rating >= 4))
                  + (SELECT COUNT(*) FROM playlist_entry WHERE track_id = ? AND playlist_id NOT IN (SELECT playlist_id FROM discovery)
                       AND playlist_id NOT IN (SELECT value FROM setting WHERE key LIKE 'trial_playlist:%'))",
        )
        .bind(&local)
        .bind(&local)
        .fetch_one(&st.db)
        .await?;
        let listed: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM playlist_entry WHERE track_id = ? AND playlist_id IN (SELECT playlist_id FROM discovery)",
        )
        .bind(&local)
        .fetch_one(&st.db)
        .await?;
        if liked > 0 {
            let new_rel = trial::move_into_songs(st, &local, rel.as_deref().unwrap_or(""), FOLDER).await?;
            sqlx::query("UPDATE request SET folder = 'Songs', rel = ?, kept_at = ? WHERE key = ?")
                .bind(new_rel)
                .bind(now())
                .bind(&key)
                .execute(&st.db)
                .await?;
        } else if listed > 0 {
            continue; // still in this or another discovery playlist
        } else {
            trial::delete_inside(st, rel.as_deref(), FOLDER).await;
            sqlx::query("DELETE FROM request WHERE key = ?").bind(&key).execute(&st.db).await?;
        }
        changed = true;
    }
    if changed {
        crate::library::scan(&st.db, &st.libs, &st.scan, false).await?;
    }
    Ok(())
}

/// Hourly: resend plays ListenBrainz missed, and fill discovery playlists when new lists are out.
pub async fn worker(st: Arc<AppState>) {
    if st.cfg.listenbrainz_user.is_none() && st.cfg.listenbrainz_token.is_none() {
        return;
    }
    loop {
        resend(&st).await;
        if !st.cfg.read_only {
            for kind in st.cfg.discovery.clone() {
                if let Err(e) = refresh(&st, &kind).await {
                    warn!(playlist = %kind, "discovery: {e:#}");
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(if st.cfg.slskd_fast_test { 1 } else { 3600 })).await;
    }
}
