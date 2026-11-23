//! What each user does: stars, ratings, plays, now playing, playlists, play queue, user info, radio.

use serde_json::{Value, json};

use super::{ApiError, Ctx, NowPlaying, Reply, empty, items, ok, smart};
use crate::db::{self, iso, now};

async fn annotate(c: &Ctx, kind: &str, id: &str, set: &str, bind: Option<i64>) -> Result<(), ApiError> {
    let sql = format!(
        "INSERT INTO annotation (user_id, item_kind, item_id) VALUES (?, ?, ?) ON CONFLICT DO NOTHING;
         UPDATE annotation SET {set} WHERE user_id = ? AND item_kind = ? AND item_id = ?"
    );
    // two statements: run them one by one so the binds line up
    let (ins, upd) = sql.split_once(';').unwrap();
    sqlx::query(sqlx::AssertSqlSafe(ins.to_string())).bind(&c.user.id).bind(kind).bind(id).execute(&c.st.db).await?;
    let mut q = sqlx::query(sqlx::AssertSqlSafe(upd.to_string()));
    if let Some(b) = bind {
        q = q.bind(b);
    }
    q.bind(&c.user.id).bind(kind).bind(id).execute(&c.st.db).await?;
    Ok(())
}

pub async fn star(c: &Ctx, on: bool) -> Result<Reply, ApiError> {
    let mut targets: Vec<(&'static str, String)> = vec![];
    for id in c.p.all("id") {
        match items::kind_of(c, id).await? {
            Some(k) => targets.push((k, id.to_string())),
            None => return Err(ApiError::not_found("Item")),
        }
    }
    targets.extend(c.p.all("albumId").into_iter().map(|i| ("album", i.to_string())));
    targets.extend(c.p.all("artistId").into_iter().map(|i| ("artist", i.to_string())));
    if targets.is_empty() {
        return Err(ApiError::missing("id"));
    }
    for (k, id) in targets {
        if on {
            annotate(c, k, &id, "starred_at = COALESCE(starred_at, ?)", Some(now())).await?;
        } else {
            annotate(c, k, &id, "starred_at = NULL", None).await?;
        }
    }
    empty()
}

pub async fn set_rating(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    let r = c.p.int("rating").ok_or_else(|| ApiError::missing("rating"))?.clamp(0, 5);
    let k = items::kind_of(c, id).await?.ok_or_else(|| ApiError::not_found("Item"))?;
    annotate(c, k, id, "rating = ?", Some(r)).await?;
    empty()
}

pub async fn scrobble(c: &Ctx) -> Result<Reply, ApiError> {
    let ids = c.p.all("id");
    if ids.is_empty() {
        return Err(ApiError::missing("id"));
    }
    let times = c.p.all("time");
    let submission = c.p.bool("submission").unwrap_or(true);
    // what goes to the user's ListenBrainz and Last.fm, sent one after another once the request is done: an app
    // handing in plays made offline sends many at once, and they must arrive (or wait to be sent again) in order
    let mut out: Vec<(String, i64, bool)> = vec![];
    for (i, id) in ids.iter().enumerate() {
        let t = items::track(c, id).await?;
        if !submission {
            set_now_playing(c, &t.id).await;
            out.push((t.id.clone(), now(), true));
            continue;
        }
        let at = times.get(i).and_then(|v| v.parse::<i64>().ok()).map(|ms| ms / 1000).unwrap_or_else(now);
        if super::encore::is_duplicate_play(c, &t.id, at).await? {
            continue; // apps sometimes report a play twice
        }
        sqlx::query("INSERT INTO play (user_id, track_id, played_at, client) VALUES (?, ?, ?, ?)")
            .bind(&c.user.id)
            .bind(&t.id)
            .bind(at)
            .bind(&c.client)
            .execute(&c.st.db)
            .await?;
        for (k, item) in [("track", &t.id), ("album", &t.album_id), ("artist", &t.artist_id)] {
            annotate(c, k, item, "play_count = play_count + 1, last_played = MAX(COALESCE(last_played, 0), ?)", Some(at)).await?;
        }
        out.push((t.id.clone(), at, false));
    }
    if !out.is_empty() {
        let (st, uid) = (c.st.clone(), c.user.id.clone());
        tokio::spawn(async move {
            for (tid, at, playing_now) in out {
                crate::scrobble::scrobble(&st, &uid, &tid, at, playing_now).await;
            }
        });
    }
    empty()
}

async fn set_now_playing(c: &Ctx, track_id: &str) {
    let mut np = c.st.now_playing.lock().await;
    np.insert(
        (c.user.id.clone(), c.client.clone()),
        NowPlaying { track_id: track_id.to_string(), username: c.user.name.clone(), client: c.client.clone(), at: now() },
    );
}

/// OpenSubsonic playback reports: we only keep "now playing" from them for now.
pub async fn report_playback(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("mediaId")?;
    match c.p.get("state").unwrap_or("playing") {
        "stopped" => {
            c.st.now_playing.lock().await.remove(&(c.user.id.clone(), c.client.clone()));
        }
        _ => set_now_playing(c, id).await,
    }
    empty()
}

pub async fn now_playing(c: &Ctx) -> Result<Reply, ApiError> {
    let list: Vec<NowPlaying> = {
        let mut np = c.st.now_playing.lock().await;
        let cutoff = now() - 15 * 60;
        np.retain(|_, v| v.at > cutoff);
        np.values().cloned().collect()
    };
    let mut entries = vec![];
    for n in list {
        if let Ok(t) = items::track(c, &n.track_id).await {
            let mut v = items::song_json(&t);
            v["username"] = n.username.into();
            v["playerName"] = n.client.into();
            v["minutesAgo"] = ((now() - n.at) / 60).into();
            v["playerId"] = 0.into();
            entries.push(v);
        }
    }
    ok(json!({ "nowPlaying": { "entry": entries } }))
}

// ── playlists ──

#[derive(sqlx::FromRow)]
struct PlaylistRow {
    id: String,
    owner_id: String,
    owner_name: String,
    name: String,
    comment: String,
    public: bool,
    created_at: i64,
    updated_at: i64,
    song_count: i64,
    duration_ms: i64,
    cover: Option<String>,
    image_path: Option<String>,
    smart: bool,
}

const PLAYLIST_SELECT: &str = "SELECT p.id, p.owner_id, u.name AS owner_name, p.name, p.comment, p.public, p.created_at, p.updated_at,
    (SELECT COUNT(*) FROM playlist_entry e JOIN track t ON t.id = e.track_id WHERE e.playlist_id = p.id AND t.missing_since IS NULL) AS song_count,
    (SELECT COALESCE(SUM(t.duration_ms), 0) FROM playlist_entry e JOIN track t ON t.id = e.track_id WHERE e.playlist_id = p.id AND t.missing_since IS NULL) AS duration_ms,
    (SELECT t.album_id FROM playlist_entry e JOIN track t ON t.id = e.track_id WHERE e.playlist_id = p.id ORDER BY e.position LIMIT 1) AS cover,
    p.image_path, p.rules_json IS NOT NULL AS smart
  FROM playlist p JOIN user u ON u.id = p.owner_id";

fn playlist_json(p: &PlaylistRow) -> Value {
    let mut v = json!({
        "id": p.id, "name": p.name, "comment": p.comment, "owner": p.owner_name, "public": p.public,
        "songCount": p.song_count, "readonly": p.smart, "duration": p.duration_ms / 1000, "created": iso(p.created_at), "changed": iso(p.updated_at),
    });
    // a playlist's own picture: the id changes with the playlist, so apps fetch a new picture after a change
    if p.image_path.is_some() {
        v["coverArt"] = format!("pl-{}_{:x}", p.id, p.updated_at).into();
    } else if let Some(c) = &p.cover {
        v["coverArt"] = c.clone().into();
    }
    v
}

async fn load_playlist(c: &Ctx, id: &str) -> Result<PlaylistRow, ApiError> {
    let p: PlaylistRow = sqlx::query_as(sqlx::AssertSqlSafe(format!("{PLAYLIST_SELECT} WHERE p.id = ?")))
        .bind(id)
        .fetch_optional(&c.st.db)
        .await?
        .ok_or_else(|| ApiError::not_found("Playlist"))?;
    if p.owner_id != c.user.id && !p.public && !c.user.is_admin {
        return Err(ApiError::not_found("Playlist"));
    }
    Ok(p)
}

fn owns(c: &Ctx, p: &PlaylistRow) -> Result<(), ApiError> {
    if p.owner_id == c.user.id || c.user.is_admin {
        Ok(())
    } else {
        Err(ApiError::new(50, "Only the owner can change this playlist"))
    }
}

/// Smart playlists this user can see, re-filled if they're stale.
async fn refresh_smart(c: &Ctx) -> Result<(), ApiError> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM playlist WHERE rules_json IS NOT NULL AND (owner_id = ? OR public = 1)")
            .bind(&c.user.id)
            .fetch_all(&c.st.db)
            .await?;
    for id in ids {
        if let Err(e) = smart::refresh(&c.st, &id, smart::MAX_AGE).await {
            tracing::warn!(playlist = %id, "smart playlist rules: {e:#}");
        }
    }
    Ok(())
}

fn not_smart(p: &PlaylistRow) -> Result<(), ApiError> {
    if p.smart {
        Err(ApiError::new(50, "This playlist fills itself from its rules; its songs can't be changed by hand"))
    } else {
        Ok(())
    }
}

pub async fn playlists(c: &Ctx) -> Result<Reply, ApiError> {
    // another user's (`username`, as the Subsonic spec has it): only for an admin, and then the ones they see
    let whose = match c.p.get("username").filter(|n| !n.is_empty() && !n.eq_ignore_ascii_case(&c.user.name)) {
        None => c.user.id.clone(),
        Some(_) if !c.user.is_admin => return Err(ApiError::new(50, "Only admins can see other users' playlists")),
        Some(name) => super::users::find(c, name).await?.0,
    };
    refresh_smart(c).await?;
    let rows: Vec<PlaylistRow> =
        sqlx::query_as(sqlx::AssertSqlSafe(format!("{PLAYLIST_SELECT} WHERE p.owner_id = ? OR p.public = 1 ORDER BY p.name COLLATE NOCASE")))
            .bind(&whose)
            .fetch_all(&c.st.db)
            .await?;
    ok(json!({ "playlists": { "playlist": rows.iter().map(playlist_json).collect::<Vec<_>>() } }))
}

pub async fn playlist(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    if let Err(e) = smart::refresh(&c.st, id, smart::MAX_AGE).await {
        tracing::warn!(playlist = %id, "smart playlist rules: {e:#}");
    }
    let p = load_playlist(c, id).await?;
    let songs =
        items::tracks_where(c, "t.id IN (SELECT track_id FROM playlist_entry WHERE playlist_id = ?)", &[&p.id], "").await?;
    let order: Vec<String> = sqlx::query_scalar("SELECT track_id FROM playlist_entry WHERE playlist_id = ? ORDER BY position")
        .bind(&p.id)
        .fetch_all(&c.st.db)
        .await?;
    let by_id: std::collections::HashMap<&str, &items::TrackRow> = songs.iter().map(|t| (t.id.as_str(), t)).collect();
    let entries: Vec<Value> = order.iter().filter_map(|id| by_id.get(id.as_str())).map(|t| items::song_json(t)).collect();
    let mut v = playlist_json(&p);
    v["entry"] = entries.into();
    ok(json!({ "playlist": v }))
}

async fn set_entries(c: &Ctx, pid: &str, ids: &[String]) -> Result<(), ApiError> {
    let mut tx = c.st.db.begin().await?;
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
    tx.commit().await?;
    Ok(())
}

pub async fn create_playlist(c: &Ctx) -> Result<Reply, ApiError> {
    let songs: Vec<String> = c.p.all("songId").into_iter().map(String::from).collect();
    let pid = if let Some(existing) = c.p.get("playlistId") {
        let p = load_playlist(c, existing).await?;
        owns(c, &p)?;
        not_smart(&p)?;
        if let Some(n) = c.p.get("name") {
            sqlx::query("UPDATE playlist SET name = ? WHERE id = ?").bind(n).bind(&p.id).execute(&c.st.db).await?;
        }
        p.id
    } else {
        let name = c.p.req("name")?;
        let id = db::random_id();
        sqlx::query("INSERT INTO playlist (id, owner_id, name, created_at, updated_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(&c.user.id)
            .bind(name)
            .bind(now())
            .bind(now())
            .execute(&c.st.db)
            .await?;
        id
    };
    set_entries(c, &pid, &songs).await?;
    let p = load_playlist(c, &pid).await?;
    let mut v = playlist_json(&p);
    let rows = items::tracks_where(c, "t.id IN (SELECT track_id FROM playlist_entry WHERE playlist_id = ?)", &[&pid], "").await?;
    v["entry"] = songs.iter().filter_map(|id| rows.iter().find(|t| &t.id == id)).map(items::song_json).collect::<Vec<_>>().into();
    ok(json!({ "playlist": v }))
}

pub async fn update_playlist(c: &Ctx) -> Result<Reply, ApiError> {
    let p = load_playlist(c, c.p.req("playlistId")?).await?;
    owns(c, &p)?;
    if let Some(n) = c.p.get("name") {
        sqlx::query("UPDATE playlist SET name = ? WHERE id = ?").bind(n).bind(&p.id).execute(&c.st.db).await?;
    }
    if let Some(cm) = c.p.get("comment") {
        sqlx::query("UPDATE playlist SET comment = ? WHERE id = ?").bind(cm).bind(&p.id).execute(&c.st.db).await?;
    }
    if let Some(pb) = c.p.bool("public") {
        sqlx::query("UPDATE playlist SET public = ? WHERE id = ?").bind(pb).bind(&p.id).execute(&c.st.db).await?;
    }
    let remove: Vec<usize> = c.p.all("songIndexToRemove").iter().filter_map(|v| v.parse().ok()).collect();
    let add: Vec<&str> = c.p.all("songIdToAdd");
    if !remove.is_empty() || !add.is_empty() {
        not_smart(&p)?;
        let mut ids: Vec<String> =
            sqlx::query_scalar("SELECT track_id FROM playlist_entry WHERE playlist_id = ? ORDER BY position")
                .bind(&p.id)
                .fetch_all(&c.st.db)
                .await?;
        // the places are those getPlaylist shows: songs that are missing (or gone) aren't counted, and stay
        let shown: std::collections::HashSet<String> = sqlx::query_scalar(
            "SELECT id FROM track WHERE missing_since IS NULL
             AND id IN (SELECT track_id FROM playlist_entry WHERE playlist_id = ?)",
        )
        .bind(&p.id)
        .fetch_all(&c.st.db)
        .await?
        .into_iter()
        .collect();
        let mut keep = vec![];
        let mut place = 0;
        for id in ids.drain(..) {
            if !shown.contains(&id) {
                keep.push(id);
                continue;
            }
            if !remove.contains(&place) {
                keep.push(id);
            }
            place += 1;
        }
        keep.extend(add.into_iter().map(String::from));
        set_entries(c, &p.id, &keep).await?;
    } else {
        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(&p.id).execute(&c.st.db).await?;
    }
    empty()
}

pub async fn delete_playlist(c: &Ctx) -> Result<Reply, ApiError> {
    let p = load_playlist(c, c.p.req("id")?).await?;
    owns(c, &p)?;
    sqlx::query("DELETE FROM playlist WHERE id = ?").bind(&p.id).execute(&c.st.db).await?;
    empty()
}

// ── play queue ──

pub async fn get_play_queue(c: &Ctx) -> Result<Reply, ApiError> {
    play_queue(c, false).await
}

/// OpenSubsonic's index-based queue: the current song by its place (a song can be in the queue twice).
pub async fn get_play_queue_by_index(c: &Ctx) -> Result<Reply, ApiError> {
    play_queue(c, true).await
}

/// track ids (JSON), current id, current place, position (ms), changed by, changed at
type QueueRow = (String, Option<String>, Option<i64>, i64, String, i64);

async fn play_queue(c: &Ctx, by_index: bool) -> Result<Reply, ApiError> {
    let row: Option<QueueRow> = sqlx::query_as(
        "SELECT track_ids, current, current_index, position_ms, changed_by, changed_at FROM play_queue WHERE user_id = ?",
    )
    .bind(&c.user.id)
    .fetch_optional(&c.st.db)
    .await?;
    let Some((ids, current, index, pos, by, at)) = row else {
        return empty();
    };
    let ids: Vec<String> = serde_json::from_str(&ids).unwrap_or_default();
    let mut entries = vec![];
    // places in `ids` whose song couldn't be found (and so aren't in `entries`)
    let mut dropped = vec![];
    for (i, id) in ids.iter().enumerate() {
        // ⬇ songs and podcast episodes are queued too: keep them, or another device loses them
        let e = if crate::catalog::dz_id(id, 't').is_some() {
            super::catalog_api::queue_entry(c, id).await
        } else if super::podcasts::is_podcast_id(id) {
            super::podcasts::queue_entry(c, id).await
        } else {
            items::track(c, id).await.ok().map(|t| items::song_json(&t))
        };
        if e.is_none() {
            dropped.push(i as i64);
        }
        entries.extend(e);
    }
    let mut v = json!({ "username": c.user.name, "changed": iso(at), "changedBy": by, "position": pos, "entry": entries });
    if by_index {
        let i = index.or_else(|| current.as_ref().and_then(|cur| ids.iter().position(|x| x == cur).map(|p| p as i64)));
        // the stored place counts every id; the answer only has the songs found, so the place moves back by the
        // ones left out before it (and goes when the current song itself was left out)
        if let Some(i) = i.filter(|i| !dropped.contains(i)) {
            v["currentIndex"] = (i - dropped.iter().filter(|d| **d < i).count() as i64).into();
        }
        return ok(json!({ "playQueueByIndex": v }));
    }
    if let Some(cur) = current {
        v["current"] = cur.into();
    }
    ok(json!({ "playQueue": v }))
}

pub async fn save_play_queue(c: &Ctx) -> Result<Reply, ApiError> {
    let ids: Vec<&str> = c.p.all("id");
    let current = c.p.get("current");
    let index = current.and_then(|cur| ids.iter().position(|x| *x == cur)).map(|i| i as i64);
    store_play_queue(c, &ids, current, index).await
}

pub async fn save_play_queue_by_index(c: &Ctx) -> Result<Reply, ApiError> {
    let ids: Vec<&str> = c.p.all("id");
    let index = c.p.int("currentIndex");
    if let Some(i) = index
        && (i < 0 || i as usize >= ids.len())
    {
        return Err(ApiError::new(10, "currentIndex isn't a place in the queue"));
    }
    let current = index.map(|i| ids[i as usize]);
    store_play_queue(c, &ids, current, index).await
}

async fn store_play_queue(c: &Ctx, ids: &[&str], current: Option<&str>, index: Option<i64>) -> Result<Reply, ApiError> {
    if ids.is_empty() {
        sqlx::query("DELETE FROM play_queue WHERE user_id = ?").bind(&c.user.id).execute(&c.st.db).await?;
        return empty();
    }
    sqlx::query(
        "INSERT INTO play_queue (user_id, track_ids, current, current_index, position_ms, changed_by, changed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET track_ids = excluded.track_ids, current = excluded.current,
           current_index = excluded.current_index, position_ms = excluded.position_ms, changed_by = excluded.changed_by,
           changed_at = excluded.changed_at",
    )
    .bind(&c.user.id)
    .bind(serde_json::to_string(&ids).unwrap())
    .bind(current)
    .bind(index)
    .bind(c.p.int("position").unwrap_or(0))
    .bind(&c.client)
    .bind(now())
    .execute(&c.st.db)
    .await?;
    empty()
}

// ── internet radio ──

pub async fn radio_list(c: &Ctx) -> Result<Reply, ApiError> {
    let rows: Vec<(String, String, String, Option<String>)> =
        sqlx::query_as("SELECT id, name, stream_url, homepage_url FROM radio ORDER BY name COLLATE NOCASE")
            .fetch_all(&c.st.db)
            .await?;
    let list: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, s, h)| {
            let mut v = json!({ "id": id, "name": name, "streamUrl": s });
            if let Some(h) = h {
                v["homepageUrl"] = h.into();
            }
            v
        })
        .collect();
    ok(json!({ "internetRadioStations": { "internetRadioStation": list } }))
}

fn admin(c: &Ctx) -> Result<(), ApiError> {
    if c.user.is_admin { Ok(()) } else { Err(ApiError::new(50, "Only admins can change radio stations")) }
}

pub async fn radio_create(c: &Ctx) -> Result<Reply, ApiError> {
    admin(c)?;
    sqlx::query("INSERT INTO radio (id, name, stream_url, homepage_url) VALUES (?, ?, ?, ?)")
        .bind(db::random_id())
        .bind(c.p.req("name")?)
        .bind(c.p.req("streamUrl")?)
        .bind(c.p.get("homepageUrl"))
        .execute(&c.st.db)
        .await?;
    empty()
}

pub async fn radio_update(c: &Ctx) -> Result<Reply, ApiError> {
    admin(c)?;
    let r = sqlx::query("UPDATE radio SET name = ?, stream_url = ?, homepage_url = ? WHERE id = ?")
        .bind(c.p.req("name")?)
        .bind(c.p.req("streamUrl")?)
        .bind(c.p.get("homepageUrl"))
        .bind(c.p.req("id")?)
        .execute(&c.st.db)
        .await?;
    if r.rows_affected() == 0 { Err(ApiError::not_found("Radio station")) } else { empty() }
}

pub async fn radio_delete(c: &Ctx) -> Result<Reply, ApiError> {
    admin(c)?;
    sqlx::query("DELETE FROM radio WHERE id = ?").bind(c.p.req("id")?).execute(&c.st.db).await?;
    empty()
}
