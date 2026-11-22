//! From what was found to what changes: the plan (matched songs, what's missing, a report), what it would change in
//! Conductor (the preview), applying it as a batch, and undoing a batch.
//!
//! Liked → hearts (with the date liked when known); saved albums → starred albums; followed artists → Encore's
//! follows (they feed Release Radar and concerts); playlists → playlists in the same order (marked "Imported from
//! …"; importing again updates them); plays → plays with their times (counts, stats, year in review and the mixes
//! all use them), plays under 30 s → skips; ratings; play counts (iTunes / Apple) where there are no single plays;
//! podcasts → subscriptions when a feed is found. Plays already there (within 100 s) aren't added again, so an
//! import can be run twice. Songs you don't have can be asked for (⬇), off unless a number is given.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    Album, Parsed, Podcast, Song,
    matching::{How, LibSong, Library},
};
use crate::{
    api::AppState,
    db::{self, Db, now, now_ms},
};

/// What to bring in.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    pub liked: bool,
    pub albums: bool,
    pub artists: bool,
    pub playlists: bool,
    pub plays: bool,
    pub ratings: bool,
    pub counts: bool,
    pub podcasts: bool,
    /// ask for (download) up to this many songs you don't have; 0: none
    pub ask_missing: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options { liked: true, albums: true, artists: true, playlists: true, plays: true, ratings: true, counts: true, podcasts: true, ask_missing: 0 }
    }
}

impl Options {
    /// "liked,playlists,plays" → only those.
    pub fn only(kinds: &str) -> Options {
        let has = |k: &str| kinds.split(',').any(|x| x.trim().eq_ignore_ascii_case(k));
        Options {
            liked: has("liked"),
            albums: has("albums"),
            artists: has("artists"),
            playlists: has("playlists"),
            plays: has("plays"),
            ratings: has("ratings"),
            counts: has("counts"),
            podcasts: has("podcasts"),
            ask_missing: 0,
        }
    }
}

/// Plays of the same song this close together are the same play (one export says when it started, another when
/// it ended, to the minute).
pub const SAME_PLAY_SECS: i64 = 100;

#[derive(Debug, Default, Serialize, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub liked: usize,
    pub albums: usize,
    pub artists: usize,
    pub playlists: usize,
    pub playlist_songs: usize,
    pub plays: usize,
    pub skips: usize,
    pub ratings: usize,
    pub counts: usize,
    pub podcasts: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Missing {
    #[serde(flatten)]
    pub song: Song,
    pub liked: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub playlists: Vec<String>,
    pub plays: usize,
}

impl Missing {
    fn weight(&self) -> usize {
        (if self.liked { 1_000_000 } else { 0 }) + self.playlists.len() * 10_000 + self.plays
    }
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub sources: Vec<String>,
    pub notes: Vec<String>,
    pub found: Counts,
    pub matched: Counts,
    /// different songs in the export, how many are in your library, and how they were found
    pub songs: usize,
    pub songs_matched: usize,
    pub by: HashMap<How, usize>,
    pub match_rate: f64,
    /// songs you don't have, most wanted first (the first 200)
    pub missing: Vec<Missing>,
    pub missing_total: usize,
    pub albums_missing: Vec<Album>,
    pub artists_missing: Vec<String>,
    /// what it changes (filled in by the preview or the import)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changes: Option<Changes>,
}

#[derive(Debug, Clone)]
pub struct PlanList {
    pub name: String,
    pub description: String,
    pub ids: Vec<String>,
    /// songs of it you don't have
    pub missing: Vec<Song>,
}

/// The import, matched to your library.
#[derive(Debug, Default)]
pub struct Plan {
    pub source: String,
    pub stars: Vec<(String, Option<i64>)>,
    pub album_stars: Vec<String>,
    pub follows: Vec<(String, String)>,
    pub playlists: Vec<PlanList>,
    pub plays: Vec<(String, i64)>,
    pub skips: Vec<(String, i64)>,
    pub ratings: Vec<(String, u8)>,
    pub counts: Vec<(String, i64, Option<i64>)>,
    pub podcasts: Vec<Podcast>,
    pub missing: Vec<Missing>,
    /// album and artist of each matched song (for play counts)
    pub of: HashMap<String, (String, String)>,
    pub report: Report,
}

/// Match everything once per song and work out what would be brought in. `isrcs`: ISRCs found online for songs
/// (by `Song::key`), tried when the names didn't match.
pub fn plan(p: &Parsed, lib: &Library, opts: &Options, isrcs: &HashMap<String, String>) -> Plan {
    let mut cache: HashMap<String, Option<(usize, How)>> = HashMap::new();
    let mut missing: HashMap<String, Missing> = HashMap::new();
    let mut out = Plan { source: p.sources.iter().cloned().collect::<Vec<_>>().join(", "), ..Default::default() };
    let r = &mut out.report;
    r.sources = p.sources.iter().cloned().collect();
    r.notes = p.notes.clone();
    let mut find = |s: &Song| -> Option<usize> {
        let k = s.key();
        let hit = *cache.entry(k.clone()).or_insert_with(|| {
            lib.find(s).or_else(|| {
                let isrc = isrcs.get(&k)?;
                let mut s2 = s.clone();
                s2.isrc = Some(isrc.clone());
                lib.find(&s2)
            })
        });
        hit.map(|(i, _)| i)
    };
    let mut want = |s: &Song, f: &dyn Fn(&mut Missing)| {
        let m = missing.entry(s.key()).or_insert_with(|| Missing { song: s.clone(), liked: false, playlists: vec![], plays: 0 });
        f(m);
    };
    let note = |out: &mut Plan, i: usize| {
        let s: &LibSong = &lib.songs[i];
        out.of.entry(s.id.clone()).or_insert_with(|| (s.album_id.clone(), s.artist_id.clone()));
        s.id.clone()
    };

    out.report.found.liked = p.liked.len();
    if opts.liked {
        let mut seen = HashSet::new();
        for (s, at) in &p.liked {
            match find(s) {
                Some(i) => {
                    let id = note(&mut out, i);
                    if seen.insert(id.clone()) {
                        out.stars.push((id, *at));
                    }
                }
                None => want(s, &|m| m.liked = true),
            }
        }
    }
    out.report.found.albums = p.albums.len();
    if opts.albums {
        for a in &p.albums {
            match lib.album(a) {
                Some(id) if !out.album_stars.contains(&id) => out.album_stars.push(id),
                Some(_) => {}
                None => out.report.albums_missing.push(a.clone()),
            }
        }
    }
    out.report.found.artists = p.artists.len();
    if opts.artists {
        for a in &p.artists {
            match lib.artist(a) {
                Some(x) if !out.follows.iter().any(|f| f.0 == x.0) => out.follows.push(x),
                Some(_) => {}
                None => out.report.artists_missing.push(a.clone()),
            }
        }
    }
    out.report.found.playlists = p.playlists.len();
    if opts.playlists {
        for pl in &p.playlists {
            out.report.found.playlist_songs += pl.songs.len();
            let mut l = PlanList { name: pl.name.clone(), description: pl.description.clone(), ids: vec![], missing: vec![] };
            for s in &pl.songs {
                match find(s) {
                    Some(i) => {
                        let id = note(&mut out, i);
                        if !l.ids.contains(&id) {
                            l.ids.push(id);
                        }
                    }
                    None => {
                        let name = pl.name.clone();
                        want(s, &|m| {
                            if !m.playlists.contains(&name) {
                                m.playlists.push(name.clone())
                            }
                        });
                        l.missing.push(s.clone());
                    }
                }
            }
            out.playlists.push(l);
        }
    }
    out.report.found.plays = p.plays.iter().filter(|x| !x.is_skip()).count();
    out.report.found.skips = p.plays.len() - out.report.found.plays;
    if opts.plays {
        for pl in &p.plays {
            match find(&pl.song) {
                Some(i) => {
                    let id = note(&mut out, i);
                    if pl.is_skip() { out.skips.push((id, pl.at)) } else { out.plays.push((id, pl.at)) }
                }
                None if !pl.is_skip() => want(&pl.song, &|m| m.plays += 1),
                None => {}
            }
        }
    }
    out.report.found.ratings = p.ratings.len();
    if opts.ratings {
        for (s, stars) in &p.ratings {
            if let Some(i) = find(s) {
                let id = note(&mut out, i);
                out.ratings.push((id, *stars));
            }
        }
    }
    out.report.found.counts = p.counts.len();
    if opts.counts {
        // only for songs without single plays in this import (they'd be counted twice)
        let played: HashMap<&str, i64> = out.plays.iter().fold(HashMap::new(), |mut m, (id, _)| {
            *m.entry(id.as_str()).or_insert(0) += 1;
            m
        });
        let mut adds = vec![];
        for (s, n, last) in &p.counts {
            if let Some(i) = find(s) {
                let id = lib.songs[i].id.clone();
                let extra = n - played.get(id.as_str()).copied().unwrap_or(0);
                if extra > 0 {
                    adds.push((id, extra, *last));
                }
            }
        }
        for (id, _, _) in &adds {
            if let Some(i) = lib.songs.iter().position(|s| &s.id == id) {
                note(&mut out, i);
            }
        }
        out.counts = adds;
    }
    out.report.found.podcasts = p.podcasts.len();
    if opts.podcasts {
        out.podcasts = p.podcasts.clone();
    }

    let r = &mut out.report;
    r.matched = Counts {
        liked: out.stars.len(),
        albums: out.album_stars.len(),
        artists: out.follows.len(),
        playlists: out.playlists.len(),
        playlist_songs: out.playlists.iter().map(|l| l.ids.len()).sum(),
        plays: out.plays.len(),
        skips: out.skips.len(),
        ratings: out.ratings.len(),
        counts: out.counts.len(),
        podcasts: out.podcasts.len(),
    };
    r.songs = cache.len();
    r.songs_matched = cache.values().filter(|v| v.is_some()).count();
    for (_, how) in cache.values().flatten() {
        *r.by.entry(*how).or_insert(0) += 1;
    }
    r.match_rate = if r.songs == 0 { 1.0 } else { (r.songs_matched as f64 / r.songs as f64 * 1000.0).round() / 1000.0 };
    let mut miss: Vec<Missing> = missing.into_values().collect();
    miss.sort_by_key(|m| std::cmp::Reverse(m.weight()));
    r.missing_total = miss.len();
    r.missing = miss.iter().take(200).cloned().collect();
    r.albums_missing.truncate(50);
    r.artists_missing.truncate(50);
    out.missing = miss;
    out
}

// ───────────── Conductor: the library, the preview, the import, undo ─────────────

pub async fn conductor_library(db: &Db) -> Result<Library> {
    type Row = (String, String, String, String, String, String, String, i64, Option<String>, Option<String>, String);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT t.id, t.title, t.artist_name, al.artist_name, t.album_name, t.album_id, t.artist_id, t.duration_ms / 1000,
                t.isrc, t.mbid, t.rel_path
         FROM track t JOIN album al ON al.id = t.album_id WHERE t.missing_since IS NULL",
    )
    .fetch_all(db)
    .await?;
    let songs = rows
        .into_iter()
        .map(|(id, title, artist, album_artist, album, album_id, artist_id, duration_s, isrc, mbid, path)| LibSong {
            id,
            title,
            artist,
            album_artist: album_artist.split(crate::library::ARTIST_JOINER).next().unwrap_or("").to_string(),
            album,
            album_id,
            artist_id,
            duration_s,
            isrc: isrc.into_iter().flat_map(|i| i.split([';', ',', '/']).map(|x| x.trim().to_uppercase()).collect::<Vec<_>>()).filter(|i| i.len() == 12).collect(),
            mbid: mbid.filter(|m| !m.is_empty()),
            path,
        })
        .collect();
    let spotify: HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT spotify_id, track_id FROM spotify_map").fetch_all(db).await?.into_iter().collect();
    let artists: Vec<(String, String)> = sqlx::query_as("SELECT id, name FROM artist").fetch_all(db).await?;
    Ok(Library::new(songs, spotify, artists))
}

/// What an import changes (or would change).
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Changes {
    pub hearts: usize,
    pub albums: usize,
    pub follows: usize,
    pub playlists_new: usize,
    pub playlists_updated: usize,
    pub plays: usize,
    pub plays_already_there: usize,
    pub skips: usize,
    pub ratings: usize,
    pub counts: usize,
    pub podcasts: usize,
    pub requests: usize,
}

const MARK: &str = "Imported from";

async fn plays_of(db: &Db, table: &str, col: &str, user: &str) -> Result<HashMap<String, Vec<i64>>> {
    let rows: Vec<(String, i64)> =
        sqlx::query_as(&format!("SELECT track_id, {col} FROM {table} WHERE user_id = ?")).bind(user).fetch_all(db).await?;
    let mut m: HashMap<String, Vec<i64>> = HashMap::new();
    for (t, at) in rows {
        m.entry(t).or_default().push(at);
    }
    Ok(m)
}

fn seen_near(have: &mut HashMap<String, Vec<i64>>, id: &str, at: i64) -> bool {
    let v = have.entry(id.to_string()).or_default();
    if v.iter().any(|x| (x - at).abs() <= SAME_PLAY_SECS) {
        return true;
    }
    v.push(at);
    false
}

async fn followed(db: &Db, user: &str) -> Result<Vec<Value>> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value_json FROM sync_kv WHERE user_id = ? AND key = 'followedArtists'")
        .bind(user)
        .fetch_optional(db)
        .await?;
    Ok(raw.and_then(|r| serde_json::from_str::<Vec<Value>>(&r).ok()).unwrap_or_default())
}

async fn set_followed(db: &Db, user: &str, list: &[Value]) -> Result<()> {
    sqlx::query(
        "INSERT INTO sync_kv (user_id, key, value_json, at, by_device) VALUES (?, 'followedArtists', ?, ?, 'conductor-import')
         ON CONFLICT(user_id, key) DO UPDATE SET value_json = excluded.value_json, at = excluded.at, by_device = excluded.by_device",
    )
    .bind(user)
    .bind(Value::from(list.to_vec()).to_string())
    .bind(now_ms())
    .execute(db)
    .await?;
    Ok(())
}

async fn imported_playlist(db: &Db, user: &str, name: &str) -> Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT id FROM playlist WHERE owner_id = ? AND name = ? AND comment LIKE ? ORDER BY created_at LIMIT 1")
        .bind(user)
        .bind(name)
        .bind(format!("{MARK} %"))
        .fetch_optional(db)
        .await?)
}

/// What importing the plan would change for this user (nothing is written).
pub async fn preview(db: &Db, user: &str, plan: &Plan) -> Result<Changes> {
    let mut c = Changes::default();
    let starred: HashSet<(String, String)> =
        sqlx::query_as::<_, (String, String)>("SELECT item_kind, item_id FROM annotation WHERE user_id = ? AND starred_at IS NOT NULL")
            .bind(user)
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    c.hearts = plan.stars.iter().filter(|(id, _)| !starred.contains(&("track".into(), id.clone()))).count();
    c.albums = plan.album_stars.iter().filter(|id| !starred.contains(&("album".into(), (*id).clone()))).count();
    let have: HashSet<String> = followed(db, user).await?.iter().filter_map(|f| f["id"].as_str().map(String::from)).collect();
    c.follows = plan.follows.iter().filter(|(id, _)| !have.contains(id)).count();
    for l in &plan.playlists {
        if imported_playlist(db, user, &l.name).await?.is_some() {
            c.playlists_updated += 1;
        } else {
            c.playlists_new += 1;
        }
    }
    let mut plays = plays_of(db, "play", "played_at", user).await?;
    for (id, at) in &plan.plays {
        if seen_near(&mut plays, id, *at) {
            c.plays_already_there += 1;
        } else {
            c.plays += 1;
        }
    }
    let mut skips = plays_of(db, "skip", "skipped_at", user).await?;
    c.skips = plan.skips.iter().filter(|(id, at)| !seen_near(&mut skips, id, *at)).count();
    let ratings: HashMap<String, i64> =
        sqlx::query_as::<_, (String, i64)>("SELECT item_id, rating FROM annotation WHERE user_id = ? AND item_kind = 'track'")
            .bind(user)
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    c.ratings = plan.ratings.iter().filter(|(id, r)| ratings.get(id).copied().unwrap_or(0) != *r as i64).count();
    let counted: HashSet<String> = sqlx::query_scalar(
        "SELECT ch.item FROM import_change ch JOIN import_batch b ON b.id = ch.batch_id WHERE ch.kind = 'count' AND b.user_id = ? AND b.status = 'done'",
    )
    .bind(user)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    c.counts = plan.counts.iter().filter(|(id, _, _)| !counted.contains(id)).count();
    c.podcasts = plan.podcasts.len();
    Ok(c)
}

async fn record(db: &Db, batch: &str, kind: &str, item: &str, before: Option<String>) -> Result<()> {
    sqlx::query("INSERT INTO import_change (batch_id, kind, item, before) VALUES (?, ?, ?, ?)")
        .bind(batch)
        .bind(kind)
        .bind(item)
        .bind(before)
        .execute(db)
        .await?;
    Ok(())
}

async fn set_progress(db: &Db, batch: &str, p: f64) {
    let _ = sqlx::query("UPDATE import_batch SET progress = ? WHERE id = ?").bind(p).bind(batch).execute(db).await;
}

async fn star(db: &Db, batch: &str, user: &str, kind: &str, id: &str, at: Option<i64>) -> Result<bool> {
    sqlx::query("INSERT OR IGNORE INTO annotation (user_id, item_kind, item_id) VALUES (?, ?, ?)")
        .bind(user)
        .bind(kind)
        .bind(id)
        .execute(db)
        .await?;
    let done = sqlx::query("UPDATE annotation SET starred_at = ? WHERE user_id = ? AND item_kind = ? AND item_id = ? AND starred_at IS NULL")
        .bind(at.unwrap_or_else(now))
        .bind(user)
        .bind(kind)
        .bind(id)
        .execute(db)
        .await?;
    if done.rows_affected() > 0 {
        record(db, batch, if kind == "track" { "star" } else { "album_star" }, id, None).await?;
        return Ok(true);
    }
    Ok(false)
}

/// Import the plan as batch `batch` (already in import_batch) for `user`. Progress goes into the batch row.
pub async fn apply(st: &AppState, batch: &str, user: &str, plan: &Plan, opts: &Options) -> Result<Changes> {
    let db = &st.db;
    let mut c = Changes::default();
    let total = (plan.stars.len() + plan.album_stars.len() + plan.playlists.len() + plan.plays.len() + plan.skips.len() + 10) as f64;
    let mut done = 0f64;
    for (id, at) in &plan.stars {
        if star(db, batch, user, "track", id, *at).await? {
            c.hearts += 1;
        }
    }
    for id in &plan.album_stars {
        if star(db, batch, user, "album", id, None).await? {
            c.albums += 1;
        }
    }
    done += (plan.stars.len() + plan.album_stars.len()) as f64;
    set_progress(db, batch, done / total).await;
    if !plan.follows.is_empty() {
        let mut list = followed(db, user).await?;
        for (id, name) in &plan.follows {
            if list.iter().any(|f| f["id"].as_str() == Some(id)) {
                continue;
            }
            list.push(json!({ "id": id, "name": name, "mbid": "", "cover": "", "since": now_ms() }));
            record(db, batch, "follow", id, None).await?;
            c.follows += 1;
        }
        if c.follows > 0 {
            set_followed(db, user, &list).await?;
        }
    }
    let label = format!("{MARK} {} on {}", if plan.source.is_empty() { "another service" } else { &plan.source }, chrono::Local::now().format("%Y-%m-%d"));
    let mut list_ids: HashMap<String, String> = HashMap::new();
    for l in &plan.playlists {
        let id = match imported_playlist(db, user, &l.name).await? {
            Some(id) => {
                let before: Vec<String> =
                    sqlx::query_scalar("SELECT track_id FROM playlist_entry WHERE playlist_id = ? ORDER BY position").bind(&id).fetch_all(db).await?;
                record(db, batch, "playlist_songs", &id, Some(Value::from(before).to_string())).await?;
                c.playlists_updated += 1;
                id
            }
            None => {
                let clash: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM playlist WHERE owner_id = ? AND name = ?").bind(user).bind(&l.name).fetch_one(db).await?;
                let id = db::random_id();
                let name = if clash > 0 { format!("{} (imported)", l.name) } else { l.name.clone() };
                let comment = if l.description.is_empty() { label.clone() } else { format!("{}\n{label}", l.description) };
                sqlx::query("INSERT INTO playlist (id, owner_id, name, comment, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
                    .bind(&id)
                    .bind(user)
                    .bind(&name)
                    .bind(&comment)
                    .bind(now())
                    .bind(now())
                    .execute(db)
                    .await?;
                record(db, batch, "playlist", &id, None).await?;
                c.playlists_new += 1;
                id
            }
        };
        let mut tx = db.begin().await?;
        sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(&id).execute(&mut *tx).await?;
        for (i, t) in l.ids.iter().enumerate() {
            sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)").bind(&id).bind(i as i64).bind(t).execute(&mut *tx).await?;
        }
        sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(&id).execute(&mut *tx).await?;
        tx.commit().await?;
        list_ids.insert(l.name.clone(), id);
    }
    done += plan.playlists.len() as f64;
    set_progress(db, batch, done / total).await;

    // plays: in chunks, counts added up per song / album / artist
    let client = format!("import:{batch}");
    let mut have = plays_of(db, "play", "played_at", user).await?;
    let mut bump: HashMap<(&str, String), (i64, i64)> = HashMap::new();
    for chunk in plan.plays.chunks(2000) {
        let mut tx = db.begin().await?;
        for (id, at) in chunk {
            if seen_near(&mut have, id, *at) {
                c.plays_already_there += 1;
                continue;
            }
            sqlx::query("INSERT INTO play (user_id, track_id, played_at, client) VALUES (?, ?, ?, ?)")
                .bind(user)
                .bind(id)
                .bind(at)
                .bind(&client)
                .execute(&mut *tx)
                .await?;
            c.plays += 1;
            let (album, artist) = plan.of.get(id).cloned().unwrap_or_default();
            for key in [("track", id.clone()), ("album", album), ("artist", artist)] {
                if key.1.is_empty() {
                    continue;
                }
                let e = bump.entry(key).or_insert((0, 0));
                e.0 += 1;
                e.1 = e.1.max(*at);
            }
        }
        tx.commit().await?;
        done += chunk.len() as f64;
        set_progress(db, batch, done / total).await;
    }
    let mut tx = db.begin().await?;
    for ((kind, id), (n, last)) in &bump {
        sqlx::query("INSERT OR IGNORE INTO annotation (user_id, item_kind, item_id) VALUES (?, ?, ?)").bind(user).bind(kind).bind(id).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE annotation SET play_count = play_count + ?, last_played = MAX(COALESCE(last_played, 0), ?)
             WHERE user_id = ? AND item_kind = ? AND item_id = ?",
        )
        .bind(n)
        .bind(last)
        .bind(user)
        .bind(kind)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let mut skipped = plays_of(db, "skip", "skipped_at", user).await?;
    let mut tx = db.begin().await?;
    for (id, at) in &plan.skips {
        if seen_near(&mut skipped, id, *at) {
            continue;
        }
        sqlx::query("INSERT INTO skip (user_id, track_id, skipped_at, client) VALUES (?, ?, ?, ?)").bind(user).bind(id).bind(at).bind(&client).execute(&mut *tx).await?;
        c.skips += 1;
    }
    tx.commit().await?;
    done += plan.skips.len() as f64;
    set_progress(db, batch, done / total).await;

    for (id, stars) in &plan.ratings {
        let before: Option<i64> =
            sqlx::query_scalar("SELECT rating FROM annotation WHERE user_id = ? AND item_kind = 'track' AND item_id = ?").bind(user).bind(id).fetch_optional(db).await?;
        if before == Some(*stars as i64) {
            continue;
        }
        sqlx::query("INSERT OR IGNORE INTO annotation (user_id, item_kind, item_id) VALUES (?, 'track', ?)").bind(user).bind(id).execute(db).await?;
        sqlx::query("UPDATE annotation SET rating = ? WHERE user_id = ? AND item_kind = 'track' AND item_id = ?").bind(*stars as i64).bind(user).bind(id).execute(db).await?;
        record(db, batch, "rating", id, Some(before.unwrap_or(0).to_string())).await?;
        c.ratings += 1;
    }
    let counted: HashSet<String> = sqlx::query_scalar(
        "SELECT ch.item FROM import_change ch JOIN import_batch b ON b.id = ch.batch_id WHERE ch.kind = 'count' AND b.user_id = ? AND b.status = 'done'",
    )
    .bind(user)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect();
    for (id, n, last) in &plan.counts {
        if counted.contains(id) {
            continue; // counted by an earlier import
        }
        let before: Option<(i64, Option<i64>)> = sqlx::query_as("SELECT play_count, last_played FROM annotation WHERE user_id = ? AND item_kind = 'track' AND item_id = ?")
            .bind(user)
            .bind(id)
            .fetch_optional(db)
            .await?;
        sqlx::query("INSERT OR IGNORE INTO annotation (user_id, item_kind, item_id) VALUES (?, 'track', ?)").bind(user).bind(id).execute(db).await?;
        sqlx::query("UPDATE annotation SET play_count = play_count + ?, last_played = MAX(COALESCE(last_played, 0), ?) WHERE user_id = ? AND item_kind = 'track' AND item_id = ?")
            .bind(n)
            .bind(last.unwrap_or(0))
            .bind(user)
            .bind(id)
            .execute(db)
            .await?;
        record(db, batch, "count", id, Some(json!({ "n": n, "last": before.and_then(|b| b.1) }).to_string())).await?;
        c.counts += 1;
    }
    if !plan.podcasts.is_empty() && !st.cfg.read_only {
        for p in &plan.podcasts {
            let feed = match &p.feed {
                Some(f) => Some(f.clone()),
                None => super::online::podcast_feed(&st.cfg.itunes_url, &p.title, &p.publisher).await,
            };
            let Some(feed) = feed else { continue };
            match crate::api::podcasts::subscribe(st, &feed).await {
                Ok((id, true)) => {
                    record(db, batch, "podcast", &id, None).await?;
                    c.podcasts += 1;
                }
                Ok((_, false)) => {}
                Err(e) => tracing::warn!(show = %p.title, "podcast not added: {e:#}"),
            }
        }
    } else {
        c.podcasts = 0;
    }
    // songs you don't have, most wanted first
    if opts.ask_missing > 0 && !st.cfg.read_only && st.cfg.catalog {
        for m in plan.missing.iter().take(opts.ask_missing) {
            let Some(key) = super::online::deezer_key(st, &m.song).await else { continue };
            let existed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE key = ?").bind(&key).fetch_one(db).await?;
            let pl = m.playlists.first().and_then(|n| list_ids.get(n)).map(String::as_str);
            if crate::catalog::ask(st, &key, user, m.liked, pl, false).await? && existed == 0 {
                record(db, batch, "request", &key, None).await?;
                c.requests += 1;
            }
        }
        st.acquire_wake.notify_one();
    }
    set_progress(db, batch, 1.0).await;
    Ok(c)
}

/// Put back what a batch changed. Plays you've played since stay; hearts, follows, playlists, ratings and counts
/// it added go; playlists it updated get their songs back; songs it asked for that haven't arrived are cancelled.
pub async fn undo(st: &AppState, batch: &str) -> Result<()> {
    let db = &st.db;
    let user: String = sqlx::query_scalar("SELECT user_id FROM import_batch WHERE id = ?").bind(batch).fetch_one(db).await?;
    let client = format!("import:{batch}");
    // plays: counts back down, then the plays go
    let mut tx = db.begin().await?;
    for (kind, col) in [("track", "p.track_id"), ("album", "t.album_id"), ("artist", "t.artist_id")] {
        sqlx::query(&format!(
            "UPDATE annotation SET play_count = MAX(0, play_count - (SELECT COUNT(*) FROM play p JOIN track t ON t.id = p.track_id
                 WHERE p.client = ?1 AND p.user_id = annotation.user_id AND {col} = annotation.item_id))
             WHERE user_id = ?2 AND item_kind = '{kind}'
               AND item_id IN (SELECT {col} FROM play p JOIN track t ON t.id = p.track_id WHERE p.client = ?1)"
        ))
        .bind(&client)
        .bind(&user)
        .execute(&mut *tx)
        .await?;
    }
    let touched: Vec<String> = sqlx::query_scalar("SELECT DISTINCT track_id FROM play WHERE client = ?").bind(&client).fetch_all(&mut *tx).await?;
    sqlx::query("DELETE FROM play WHERE client = ?").bind(&client).execute(&mut *tx).await?;
    for t in &touched {
        sqlx::query(
            "UPDATE annotation SET last_played = (SELECT MAX(played_at) FROM play WHERE user_id = ?1 AND track_id = ?2)
             WHERE user_id = ?1 AND item_kind = 'track' AND item_id = ?2",
        )
        .bind(&user)
        .bind(t)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM skip WHERE client = ?").bind(&client).execute(&mut *tx).await?;
    tx.commit().await?;
    let changes: Vec<(String, String, Option<String>)> =
        sqlx::query_as("SELECT kind, item, before FROM import_change WHERE batch_id = ? ORDER BY rowid DESC").bind(batch).fetch_all(db).await?;
    let mut unfollow = HashSet::new();
    for (kind, item, before) in changes {
        match kind.as_str() {
            "star" | "album_star" => {
                let k = if kind == "star" { "track" } else { "album" };
                sqlx::query("UPDATE annotation SET starred_at = NULL WHERE user_id = ? AND item_kind = ? AND item_id = ?").bind(&user).bind(k).bind(&item).execute(db).await?;
            }
            "follow" => {
                unfollow.insert(item);
            }
            "playlist" => {
                sqlx::query("DELETE FROM playlist WHERE id = ? AND owner_id = ?").bind(&item).bind(&user).execute(db).await?;
            }
            "playlist_songs" => {
                let ids: Vec<String> = before.and_then(|b| serde_json::from_str(&b).ok()).unwrap_or_default();
                let mut tx = db.begin().await?;
                sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(&item).execute(&mut *tx).await?;
                for (i, t) in ids.iter().enumerate() {
                    sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)").bind(&item).bind(i as i64).bind(t).execute(&mut *tx).await?;
                }
                tx.commit().await?;
            }
            "rating" => {
                let b: i64 = before.and_then(|b| b.parse().ok()).unwrap_or(0);
                sqlx::query("UPDATE annotation SET rating = ? WHERE user_id = ? AND item_kind = 'track' AND item_id = ?").bind(b).bind(&user).bind(&item).execute(db).await?;
            }
            "count" => {
                let b: Value = before.and_then(|b| serde_json::from_str(&b).ok()).unwrap_or_default();
                sqlx::query("UPDATE annotation SET play_count = MAX(0, play_count - ?), last_played = ? WHERE user_id = ? AND item_kind = 'track' AND item_id = ?")
                    .bind(b["n"].as_i64().unwrap_or(0))
                    .bind(b["last"].as_i64())
                    .bind(&user)
                    .bind(&item)
                    .execute(db)
                    .await?;
            }
            "request" => {
                sqlx::query("DELETE FROM request WHERE key = ? AND local_id IS NULL").bind(&item).execute(db).await?;
            }
            "podcast" => {
                let _ = crate::api::podcasts::unsubscribe(st, &item).await;
            }
            _ => {}
        }
    }
    if !unfollow.is_empty() {
        let list: Vec<Value> = followed(db, &user).await?.into_iter().filter(|f| !f["id"].as_str().is_some_and(|i| unfollow.contains(i))).collect();
        set_followed(db, &user, &list).await?;
    }
    sqlx::query("DELETE FROM import_change WHERE batch_id = ?").bind(batch).execute(db).await?;
    sqlx::query("UPDATE import_batch SET status = 'undone', done_at = ? WHERE id = ?").bind(now()).bind(batch).execute(db).await?;
    Ok(())
}
