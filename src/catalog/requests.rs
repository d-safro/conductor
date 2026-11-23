//! The songs asked for (Stagehand's requests, Audition's songs on trial) and Lidarr's albums, as Podium's admin page
//! and the public `encore.requests` calls show them, and what an admin can do with them: retry, cancel, set aside or
//! bring back, search Soulseek in their own words and pick a file. One place for both, so they never disagree.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Value, json};

use super::{acquire, text};
use crate::{
    api::AppState,
    auth::User,
    db::{iso, now},
};

/// Requests the automatic search couldn't get (or is still looking for): an admin may pick a file for them.
pub const TO_PICK: &[&str] = &["retry", "notfound"];

/// What a request's state is called in `encore.requests`, in the order they're listed.
pub const STATES: &[&str] = &["searching", "queued", "downloading", "done", "notfound", "failed", "setAside"];

/// Why an action was refused. Podium answers it with an HTTP status, the Subsonic API with its own codes.
#[derive(Debug)]
pub enum Refusal {
    /// no such request
    NotFound(String),
    /// the request (a search's words) can't be used
    Bad(String),
    /// it doesn't apply to this request now (it's already here, it isn't waiting for a file, read-only)
    Conflict(String),
    /// slskd isn't logged in to Soulseek
    Unavailable(String),
    /// slskd refused or failed
    Upstream(String),
}

impl Refusal {
    pub fn message(&self) -> &str {
        match self {
            Refusal::NotFound(m) | Refusal::Bad(m) | Refusal::Conflict(m) | Refusal::Unavailable(m) | Refusal::Upstream(m) => m,
        }
    }
}

pub fn slskd(st: &AppState) -> Option<acquire::Slskd> {
    Some(acquire::Slskd::new(st.cfg.slskd_url.as_deref()?, st.cfg.slskd_api_key.as_deref()?))
}

/// How slskd's downloads are going: (peer, remote file) → (slskd's state, percent done). Empty without slskd.
pub async fn transfers(st: &AppState) -> HashMap<(String, String), (String, f64)> {
    let mut live = HashMap::new();
    let Some(sl) = slskd(st) else { return live };
    let Ok(t) = sl.get("/transfers/downloads").await else { return live };
    for u in t.as_array().into_iter().flatten() {
        for d in u["directories"].as_array().into_iter().flatten() {
            for f in d["files"].as_array().into_iter().flatten() {
                live.insert(
                    (u["username"].as_str().unwrap_or("").to_string(), f["filename"].as_str().unwrap_or("").to_string()),
                    (f["state"].as_str().unwrap_or("").to_string(), f["percentComplete"].as_f64().unwrap_or(0.0)),
                );
            }
        }
    }
    live
}

/// A remote file's own name, without its folders (peers use `\` or `/`).
pub fn file_name(f: &str) -> String {
    f.replace('\\', "/").rsplit('/').next().unwrap_or("").to_string()
}

/// The words to search Soulseek with for a song: its main artist and its title without "(Remastered)" and the like.
pub fn pick_query(title: &str, artists: &[String]) -> String {
    format!("{} {}", artists.first().map(String::as_str).unwrap_or(""), text::clean_title(title)).trim().to_string()
}

/// A line for an admin about where a request stands, or "".
pub fn note(status: &str, manual: bool, tried: bool) -> &'static str {
    if TO_PICK.contains(&status) && manual && tried {
        "the last file you picked didn't arrive; pick another"
    } else if status == "upgrading" {
        "swapping in a better copy"
    } else {
        ""
    }
}

// ───────────── what an admin can do ─────────────

/// Look for a song again from the start (any file, any peer). False when it's already here or unknown.
pub async fn retry(st: &AppState, key: &str) -> bool {
    let r = sqlx::query(
        "UPDATE request SET status = 'retry', searches = 0, last_search = 0, tried_json = '[]' WHERE key = ? AND local_id IS NULL",
    )
    .bind(key)
    .execute(&st.db)
    .await;
    st.acquire_wake.notify_one();
    r.is_ok_and(|r| r.rows_affected() > 0)
}

/// Stop asking for a song that isn't here yet. False when it's already here or unknown.
pub async fn cancel(st: &AppState, key: &str) -> bool {
    let r = sqlx::query("DELETE FROM request WHERE key = ? AND local_id IS NULL").bind(key).execute(&st.db).await;
    r.is_ok_and(|r| r.rows_affected() > 0)
}

/// Set a song aside (no more automatic searches, out of the list to pick), or bring it back. False when it isn't
/// one that can be (set aside: only one that's being looked for or wasn't found; brought back: only one set aside).
pub async fn set_aside(st: &AppState, key: &str, on: bool) -> bool {
    let q = if on {
        "UPDATE request SET skipped_from = status, status = 'skipped' WHERE key = ? AND status IN ('retry', 'notfound')"
    } else {
        "UPDATE request SET status = COALESCE(skipped_from, 'notfound'), skipped_from = NULL WHERE key = ? AND status = 'skipped'"
    };
    sqlx::query(q).bind(key).execute(&st.db).await.is_ok_and(|r| r.rows_affected() > 0)
}

/// Look for a Lidarr album again. False when it's done or unknown.
pub async fn retry_album(st: &AppState, album_id: i64) -> bool {
    sqlx::query("UPDATE lidarr_grab SET status = 'retry', searches = 0, last_search = 0 WHERE album_id = ? AND status != 'done'")
        .bind(album_id)
        .execute(&st.db)
        .await
        .is_ok_and(|r| r.rows_affected() > 0)
}

async fn request(st: &AppState, key: &str) -> Option<acquire::Req> {
    sqlx::query_as("SELECT * FROM request WHERE key = ?").bind(key).fetch_optional(&st.db).await.ok().flatten()
}

/// Search Soulseek for a song in an admin's own words: every audio file found, the likeliest first (at most 120),
/// each with what fits (title, artist, album, the automatic search's own score). `{results, total}`.
pub async fn search_files(st: &AppState, key: &str, q: &str) -> Result<Value, Refusal> {
    if st.cfg.read_only {
        // nothing found could be picked (pick_file refuses too), so Soulseek isn't asked
        return Err(Refusal::Conflict("read-only: nothing is downloaded, so there's nothing to search for".into()));
    }
    let Some(r) = request(st, key).await else { return Err(Refusal::NotFound("unknown song".into())) };
    let q = q.trim();
    if q.chars().count() < 3 {
        return Err(Refusal::Bad("type at least 3 letters".into()));
    }
    let Some(sl) = slskd(st) else { return Err(Refusal::Conflict("slskd isn't set up (slskd_url, slskd_api_key)".into())) };
    if !sl.logged_in().await {
        return Err(Refusal::Unavailable("slskd isn't logged in to Soulseek right now (is its VPN up?)".into()));
    }
    let found = acquire::search(st, &sl, q).await.map_err(|e| Refusal::Upstream(format!("Soulseek search failed: {e}")))?;
    let title = text::norm(&text::clean_title(&r.title));
    let mut rows = vec![];
    for resp in &found {
        for f in resp["files"].as_array().into_iter().flatten() {
            let name = f["filename"].as_str().unwrap_or("").replace('\\', "/");
            let base = name.rsplit('/').next().unwrap_or(&name).to_string();
            let ext = f["extension"]
                .as_str()
                .filter(|e| !e.is_empty())
                .map(String::from)
                .unwrap_or_else(|| base.rsplit('.').next().unwrap_or("").to_string())
                .trim_start_matches('.')
                .to_lowercase();
            if !acquire::is_audio(&ext) {
                continue;
            }
            let score = acquire::score_file(&r, resp, f, &[]);
            let title_ok = !title.is_empty() && (text::norm(&base).contains(&title) || text::norm(&text::translit(&base)).contains(&title));
            rows.push(json!({
                "user": resp["username"], "filename": f["filename"], "size": f["size"].as_i64().unwrap_or(0),
                "folder": name.rsplit('/').nth(1).unwrap_or(""), "name": base, "ext": ext,
                "bitrate": f["bitRate"].as_i64().unwrap_or(0), "length": f["length"].as_f64().unwrap_or(0.0),
                "free": resp["hasFreeUploadSlot"] == true, "queue": resp["queueLength"].as_i64().unwrap_or(0),
                "speed": resp["uploadSpeed"].as_i64().unwrap_or(0), "match": score.is_some(), "titleOk": title_ok,
                "artistOk": acquire::artist_in(&r.artists(), &name), "sameAlbum": acquire::same_album(&r, &name) == Some(true),
                "score": score.unwrap_or(-1.0),
            }));
        }
    }
    let rank = |x: &Value| {
        let q = match x["ext"].as_str() {
            Some("flac") => 3,
            Some("mp3") => 1,
            _ => 0,
        };
        (
            x["match"] == true,
            x["titleOk"] == true && x["artistOk"] == true,
            x["sameAlbum"] == true,
            (x["score"].as_f64().unwrap_or(-1.0) * 100.0) as i64,
            q,
            x["bitrate"].as_i64().unwrap_or(0),
            x["free"] == true,
        )
    };
    rows.sort_by_key(|x| std::cmp::Reverse(rank(x)));
    let total = rows.len();
    rows.truncate(120);
    Ok(json!({ "results": rows, "total": total }))
}

/// Download this file (a search result) for a song that's waiting for one; it's filed like any download.
pub async fn pick_file(st: &AppState, key: &str, user: &str, filename: &str, size: i64) -> Result<(), Refusal> {
    if st.cfg.read_only {
        return Err(Refusal::Conflict("read-only: nothing is downloaded".into()));
    }
    let Some(r) = request(st, key).await else { return Err(Refusal::NotFound("unknown song".into())) };
    if !matches!(r.status.as_str(), "retry" | "notfound" | "skipped") {
        return Err(Refusal::Conflict("this song isn't waiting for a file".into()));
    }
    let Some(sl) = slskd(st) else { return Err(Refusal::Conflict("slskd isn't set up".into())) };
    let path = format!("/transfers/downloads/{}", acquire::urlencode(user));
    if let Err(e) = sl.call(reqwest::Method::POST, &path, Some(json!([{ "filename": filename, "size": size }]))).await {
        return Err(Refusal::Upstream(format!("slskd refused it ({e}); that person may be offline, try another file")));
    }
    let same = acquire::same_album(&r, filename);
    let _ = sqlx::query(
        "UPDATE request SET status = 'queued', slsk_user = ?, slsk_file = ?, since = ?, manual = 1, same_album = ?, skipped_from = NULL
         WHERE key = ?",
    )
    .bind(user)
    .bind(filename)
    .bind(now())
    .bind(same)
    .bind(key)
    .execute(&st.db)
    .await;
    st.acquire_wake.notify_one();
    Ok(())
}

// ───────────── the list (encore.requests) ─────────────

/// What to list.
#[derive(Default)]
pub struct Filter<'a> {
    /// only these states (empty: all)
    pub states: Vec<&'a str>,
    /// "song" or "album" (None: both)
    pub kind: Option<&'a str>,
    /// only what this user asked for (their id); a user who isn't an admin always sees only their own
    pub user: Option<&'a str>,
    /// only this one request (its id)
    pub id: Option<&'a str>,
}

#[derive(sqlx::FromRow)]
struct SongRow {
    key: String,
    title: String,
    artists_json: String,
    album: String,
    duration_ms: i64,
    status: String,
    found: bool,
    searches: i64,
    manual: bool,
    trial: bool,
    keep: bool,
    trial_at: Option<i64>,
    trial_user: Option<String>,
    mix: Option<String>,
    folder: String,
    rel: Option<String>,
    local_id: Option<String>,
    asked_at: i64,
    arrived_at: Option<i64>,
    slsk_user: Option<String>,
    slsk_file: Option<String>,
    up_user: Option<String>,
    up_file: Option<String>,
    tried: bool,
}

#[derive(sqlx::FromRow)]
struct AlbumRow {
    album_id: i64,
    title: String,
    artist: String,
    status: String,
    slsk_user: Option<String>,
    folder: Option<String>,
    files_json: String,
    searches: i64,
    last_search: i64,
    tried_json: String,
    since: i64,
    error: String,
}

/// What a song's progress looks like: (state, percent, slskd's word for it).
fn song_state(status: &str, found: bool, live: Option<&(String, f64)>) -> (&'static str, f64, String) {
    let (word, pc) = live.cloned().unwrap_or_default();
    let moving = |word: &str| word.contains("InProgress") || word.contains("Succeeded");
    match status {
        "queued" | "upgrading" if moving(&word) => ("downloading", if word.contains("Succeeded") { 100.0 } else { pc.round() }, word),
        "queued" | "upgrading" => ("queued", 0.0, word),
        "done" => ("done", 100.0, String::new()),
        "notfound" if found => ("failed", 0.0, String::new()),
        "notfound" => ("notfound", 0.0, String::new()),
        "skipped" => ("setAside", 0.0, String::new()),
        _ => ("searching", 0.0, String::new()),
    }
}

/// The list as `encore.requests` gives it, newest first (songs by when they were asked for, Lidarr's albums by their
/// last search or download), and a page of it. `viewer` decides what's in it: an admin sees everything (and who
/// asked, the peers, the files); anyone else only what they asked for, and only their own name.
pub async fn list(st: &AppState, viewer: &User, f: &Filter<'_>, offset: usize, count: usize) -> Result<Value> {
    let admin = viewer.is_admin;
    let only = if admin { f.user } else { Some(viewer.id.as_str()) };
    let mut songs: Vec<SongRow> = vec![];
    if f.kind != Some("album") && f.id.is_none_or(|id| !id.starts_with("lidarr-")) {
        songs = sqlx::query_as(
            "SELECT r.key, r.title, r.artists_json, r.album, r.duration_ms, r.status, r.found, r.searches, r.manual, r.trial,
                    r.keep, r.trial_at, r.trial_user, r.mix, r.folder, r.rel, r.local_id, r.asked_at, r.arrived_at,
                    r.slsk_user, r.slsk_file, r.up_user, r.up_file, r.tried_json != '[]' AS tried
             FROM request r
             WHERE (?1 IS NULL OR r.trial_user = ?1 OR EXISTS (SELECT 1 FROM request_want w WHERE w.key = r.key AND w.user_id = ?1))
               AND (?2 IS NULL OR r.key = ?2)
             ORDER BY r.asked_at DESC",
        )
        .bind(only)
        .bind(f.id)
        .fetch_all(&st.db)
        .await?;
    }
    // Lidarr's albums are the server's own wants: an admin's (and only without a user asked about)
    let mut albums: Vec<AlbumRow> = vec![];
    if admin && only.is_none() && f.kind != Some("song") && f.id.is_none_or(|id| id.starts_with("lidarr-")) {
        let one = f.id.and_then(|id| id.strip_prefix("lidarr-")).map(|n| n.parse::<i64>().unwrap_or(-1));
        albums = sqlx::query_as(
            "SELECT album_id, title, artist, status, slsk_user, folder, files_json, searches, last_search, tried_json, since, error
             FROM lidarr_grab WHERE ?1 IS NULL OR album_id = ?1",
        )
        .bind(one)
        .fetch_all(&st.db)
        .await?;
    }
    let names: HashMap<String, String> =
        sqlx::query_as("SELECT id, name FROM user").fetch_all(&st.db).await?.into_iter().collect();
    // who wants each song (hearted, in a playlist)
    let mut wants: HashMap<String, Vec<(String, bool, String)>> = HashMap::new();
    for (key, uid, star, pl) in sqlx::query_as::<_, (String, String, bool, String)>("SELECT key, user_id, star, playlist_id FROM request_want")
        .fetch_all(&st.db)
        .await?
    {
        wants.entry(key).or_default().push((uid, star, pl));
    }
    let busy = songs.iter().any(|r| r.status == "queued" || r.status == "upgrading") || albums.iter().any(|a| a.status == "downloading");
    let live = if busy { transfers(st).await } else { HashMap::new() };
    let can_pick = admin && slskd(st).is_some() && !st.cfg.read_only;

    // (sort key, state, item)
    let mut all: Vec<(i64, &'static str, Value)> = vec![];
    for r in songs {
        let artists: Vec<String> = serde_json::from_str(&r.artists_json).unwrap_or_default();
        let (u, file) = if r.status == "upgrading" { (r.up_user.clone(), r.up_file.clone()) } else { (r.slsk_user.clone(), r.slsk_file.clone()) };
        let (state, progress, transfer) =
            song_state(&r.status, r.found, live.get(&(u.clone().unwrap_or_default(), file.clone().unwrap_or_default())));
        let w = wants.get(&r.key).map(Vec::as_slice).unwrap_or(&[]);
        let mut reasons = vec![];
        if w.iter().any(|x| x.1) {
            reasons.push("heart");
        }
        if w.iter().any(|x| !x.2.is_empty()) {
            reasons.push("playlist");
        }
        if r.trial_at.is_some() && r.mix.is_none() {
            reasons.push("trial");
        }
        if r.mix.is_some() {
            reasons.push("mix");
        }
        let place = r.rel.as_deref().and_then(|p| p.split('/').next()).filter(|p| !p.is_empty()).unwrap_or(&r.folder).to_string();
        if place == "Discovery" {
            reasons.push("discovery");
        }
        let users: Vec<String> = if admin {
            let mut ids: Vec<&str> = r.trial_user.as_deref().into_iter().collect();
            ids.extend(w.iter().map(|x| x.0.as_str()));
            let mut out: Vec<String> = vec![];
            for id in ids {
                if let Some(n) = names.get(id)
                    && !out.contains(n)
                {
                    out.push(n.clone());
                }
            }
            out.sort();
            out
        } else {
            vec![viewer.name.clone()]
        };
        let here = r.local_id.is_some();
        let mut actions = vec![];
        if admin {
            if !here {
                actions.extend(["retry", "cancel"]);
            }
            if TO_PICK.contains(&r.status.as_str()) {
                actions.push("setAside");
            }
            if r.status == "skipped" {
                actions.push("bringBack");
            }
            if can_pick && matches!(r.status.as_str(), "retry" | "notfound" | "skipped") {
                actions.extend(["search", "pick"]);
            }
        }
        let mut v = json!({
            "id": r.key, "type": "song", "title": r.title, "artist": artists.join(", "), "artists": artists.clone(),
            "album": r.album, "duration": r.duration_ms / 1000, "state": state, "progress": progress, "transfer": transfer,
            "reasons": reasons, "users": users, "trial": r.trial, "kept": !r.trial || r.keep, "folder": place,
            "asked": iso(r.asked_at), "searches": r.searches, "found": r.found, "manual": r.manual,
            "upgrading": r.status == "upgrading", "note": note(&r.status, r.manual, r.tried),
            "query": pick_query(&r.title, &artists), "actions": actions,
        });
        if let Some(t) = r.arrived_at {
            v["arrived"] = iso(t).into();
        }
        if let Some(l) = &r.local_id {
            v["songId"] = l.clone().into();
        }
        if let Some(m) = &r.mix {
            v["mix"] = m.clone().into();
        }
        if admin {
            v["from"] = u.unwrap_or_default().into();
            v["file"] = file.as_deref().map(file_name).unwrap_or_default().into();
            v["path"] = r.rel.clone().unwrap_or_default().into();
        }
        all.push((r.asked_at, state, v));
    }
    for a in albums {
        let files: Vec<String> = serde_json::from_str(&a.files_json).unwrap_or_default();
        let peer = a.slsk_user.clone().unwrap_or_default();
        let here: Vec<&(String, f64)> = files.iter().filter_map(|f| live.get(&(peer.clone(), f.clone()))).collect();
        let (state, progress, transfer) = match a.status.as_str() {
            "downloading" if here.iter().any(|x| x.0.contains("InProgress") || x.0.contains("Succeeded")) => {
                let done: f64 = here.iter().map(|x| if x.0.contains("Succeeded") { 100.0 } else { x.1 }).sum();
                ("downloading", (done / files.len().max(1) as f64).round(), "InProgress".to_string())
            }
            "downloading" => ("queued", 0.0, here.first().map(|x| x.0.clone()).unwrap_or_default()),
            "importing" => ("downloading", 100.0, "Importing".to_string()),
            "done" => ("done", 100.0, String::new()),
            "notfound" if !a.error.is_empty() || a.tried_json != "[]" => ("failed", 0.0, String::new()),
            "notfound" => ("notfound", 0.0, String::new()),
            _ => ("searching", 0.0, String::new()),
        };
        let v = json!({
            "id": format!("lidarr-{}", a.album_id), "type": "album", "title": a.title, "artist": a.artist,
            "artists": [a.artist], "album": a.title, "duration": 0, "state": state, "progress": progress, "transfer": transfer,
            "reasons": ["lidarr"], "users": [], "trial": false, "kept": true, "folder": "",
            "searches": a.searches, "found": a.tried_json != "[]" || matches!(a.status.as_str(), "downloading" | "importing" | "done"),
            "manual": false, "upgrading": false, "note": a.error, "query": format!("{} {}", a.artist, a.title).trim(),
            "actions": if a.status != "done" { vec!["retry"] } else { vec![] },
            "from": peer, "file": a.folder.as_deref().map(file_name).unwrap_or_default(), "path": "",
        });
        all.push((a.since.max(a.last_search), state, v));
    }
    all.sort_by_key(|x| std::cmp::Reverse(x.0));
    let mut counts = serde_json::Map::new();
    for s in STATES {
        counts.insert(s.to_string(), all.iter().filter(|x| x.1 == *s).count().into());
    }
    let on_trial = all.iter().filter(|x| x.2["trial"] == true).count();
    let chosen: Vec<Value> = all.into_iter().filter(|x| f.states.is_empty() || f.states.contains(&x.1)).map(|x| x.2).collect();
    let total = chosen.len();
    let page: Vec<Value> = chosen.into_iter().skip(offset).take(count).collect();
    Ok(json!({
        "total": total, "offset": offset, "counts": counts, "onTrial": on_trial,
        "downloadsPaused": crate::vpn::paused(st), "canPick": can_pick, "request": page,
    }))
}

// ───────────── what changed (encore.events) ─────────────

#[derive(sqlx::FromRow)]
struct PrintRow {
    key: String,
    status: String,
    found: bool,
    trial: bool,
    keep: bool,
    mix: Option<String>,
    folder: String,
    local_id: Option<String>,
    arrived_at: Option<i64>,
    trial_user: Option<String>,
    slsk_user: Option<String>,
    slsk_file: Option<String>,
    up_user: Option<String>,
    up_file: Option<String>,
}

/// What each account's list (`list`) shows, as one number per account (user id → it), to tell when it changed
/// without sending the list: a song's state, its progress in steps of 10%, why it was asked for, on trial or kept,
/// here or not. An admin's covers every song and Lidarr's albums; anyone else's only what they asked for. Asks
/// slskd how its downloads are going only while one is under way.
pub async fn fingerprints(st: &AppState) -> Result<HashMap<String, u64>> {
    use std::hash::{Hash, Hasher};
    fn hash(v: impl Hash) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        v.hash(&mut h);
        h.finish()
    }
    let rows: Vec<PrintRow> = sqlx::query_as(
        "SELECT key, status, found, trial, keep, mix, folder, local_id, arrived_at, trial_user, slsk_user, slsk_file,
                up_user, up_file FROM request",
    )
    .fetch_all(&st.db)
    .await?;
    let albums: Vec<(i64, String)> = sqlx::query_as("SELECT album_id, status FROM lidarr_grab").fetch_all(&st.db).await?;
    let mut wants: HashMap<String, Vec<(String, bool, String)>> = HashMap::new();
    for (key, uid, star, pl) in sqlx::query_as::<_, (String, String, bool, String)>("SELECT key, user_id, star, playlist_id FROM request_want")
        .fetch_all(&st.db)
        .await?
    {
        wants.entry(key).or_default().push((uid, star, pl));
    }
    let users: Vec<(String, bool)> = sqlx::query_as("SELECT id, is_admin FROM user").fetch_all(&st.db).await?;
    let busy = rows.iter().any(|r| r.status == "queued" || r.status == "upgrading") || albums.iter().any(|a| a.1 == "downloading");
    let live = if busy { transfers(st).await } else { HashMap::new() };

    // (the sum of the rows' numbers: the same whatever order they come in)
    let mut all = 0u64;
    let mut mine: HashMap<&str, u64> = HashMap::new();
    for r in &rows {
        let (u, f) = if r.status == "upgrading" { (&r.up_user, &r.up_file) } else { (&r.slsk_user, &r.slsk_file) };
        let (state, pc, _) = song_state(&r.status, r.found, live.get(&(u.clone().unwrap_or_default(), f.clone().unwrap_or_default())));
        let w = wants.get(&r.key).map(Vec::as_slice).unwrap_or(&[]);
        let why = (w.iter().any(|x| x.1), w.iter().any(|x| !x.2.is_empty()), r.mix.as_deref(), r.folder.as_str());
        let n = hash((&r.key, state, (pc / 10.0) as i64, r.status == "upgrading", why, r.trial, r.keep, &r.local_id, r.arrived_at));
        all = all.wrapping_add(n);
        let mut who: Vec<&str> = r.trial_user.as_deref().into_iter().chain(w.iter().map(|x| x.0.as_str())).collect();
        who.sort_unstable();
        who.dedup();
        for id in who {
            let e = mine.entry(id).or_default();
            *e = e.wrapping_add(n);
        }
    }
    for a in &albums {
        all = all.wrapping_add(hash(("lidarr", a.0, &a.1)));
    }
    Ok(users
        .into_iter()
        .map(|(id, admin)| {
            let n = if admin { all } else { mine.get(id.as_str()).copied().unwrap_or(0) };
            (id, n)
        })
        .collect())
}
