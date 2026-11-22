//! Your mixes, as real playlists (so every app sees them), per user:
//!
//! - **Discover Weekly** (Mondays from 04:00): 30 songs you don't have, from Deezer artists related to the ones
//!   you play and radios of your top songs. Fetched on trial for a week (not listed in "On trial"): heart one or
//!   add it to a playlist to keep it; the rest go when the next one comes.
//! - **Release Radar** (Fridays from 04:00): new releases (last 14 days) from the artists you follow in Encore and
//!   your top artists. Ones you have are added; ones you don't are fetched on trial.
//! - **Daily Mix 1–6** (daily from 04:00): your library only. Each starts from one of your top artists and adds
//!   songs that sound like them (AudioMuse; the same genre without it): about 70% songs you play, 30% new to you.
//! - **Daylist** (through the day): what you play at this time of day (and on this weekday), named after it.
//!
//!   getMixes → {"mixes": {"kind": [{kind, name, enabled, updated}], "mix": [{kind, slot, playlistId, name,
//!   comment, songCount, updated}]}}   setMix?kind=&enabled=true|false (off deletes its playlists)   refreshMix?kind=
//!
//! Nothing is made for a user until their app asks for their mixes. Discover and Radar need the catalog (Deezer).

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use rand::{SeedableRng, seq::SliceRandom};
use serde_json::{Value, json};

use crate::{
    api::{ApiError, AppState, Ctx, Reply, ok},
    catalog,
    db::{self, now},
};

pub const KINDS: &[&str] = &["discover", "radar", "daily", "daylist"];
const WEEK_HOURS: f64 = 7.0 * 24.0;
const DISCOVER_SIZE: usize = 30;
const RADAR_SIZE: usize = 30;
const DAILY_SIZE: usize = 50;
const DAILY_COUNT: usize = 6;
const DISCOVER_NOTE: &str = "New to you, every Monday: songs you don't have yet, picked from what you play. They're fetched \
                             for a week: heart one or add it to a playlist to keep it.";
const RADAR_NOTE: &str = "New releases from the artists you follow and play most, every Friday. Heart what you want to keep.";

static RUNNING: LazyLock<Mutex<HashSet<(String, String)>>> = LazyLock::new(Default::default);

pub fn label(kind: &str) -> &'static str {
    match kind {
        "discover" => "Discover Weekly",
        "radar" => "Release Radar",
        "daily" => "Daily mixes",
        _ => "Daylist",
    }
}

// ───────────── pure helpers ─────────────

/// early morning 5–9, morning 9–12, afternoon 12–17, evening 17–21, night 21–1, late night 1–5.
pub fn part_of_day(hour: u32) -> &'static str {
    match hour {
        1..=4 => "late night",
        5..=8 => "early morning",
        9..=11 => "morning",
        12..=16 => "afternoon",
        17..=20 => "evening",
        _ => "night",
    }
}

pub fn daylist_name(genres: &[String], now: DateTime<Local>) -> String {
    let words = genres.iter().take(2).filter(|g| !g.is_empty()).map(|g| g.to_lowercase()).collect::<Vec<_>>().join(" ");
    let day = if now.hour() >= 1 { now } else { now - chrono::Duration::days(1) };
    let day = day.format("%A").to_string().to_lowercase();
    [words.as_str(), day.as_str(), part_of_day(now.hour())]
        .iter()
        .filter(|x| !x.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

fn seed_of(s: &str) -> u64 {
    use sha1::{Digest, Sha1};
    let d = Sha1::digest(s.as_bytes());
    u64::from_le_bytes(d[..8].try_into().unwrap())
}

#[derive(Clone, Debug)]
pub struct Cand {
    pub key: String, // "dz-t-…" or "lib:<id>"
    pub artist: String,
}

/// Up to n of `cands`, none in `skip`, at most `per_artist` each, shuffled by `seed`.
pub fn pick_spread(cands: &[Cand], n: usize, per_artist: usize, skip: &HashSet<String>, seed: u64) -> Vec<Cand> {
    let mut pool: Vec<&Cand> = cands.iter().filter(|c| !c.key.is_empty() && !skip.contains(&c.key)).collect();
    pool.shuffle(&mut rand::rngs::StdRng::seed_from_u64(seed));
    let (mut out, mut per, mut ids) = (vec![], HashMap::<String, usize>::new(), HashSet::new());
    for c in pool {
        let a = c.artist.to_lowercase();
        if ids.contains(&c.key) || per.get(&a).copied().unwrap_or(0) >= per_artist {
            continue;
        }
        ids.insert(c.key.clone());
        *per.entry(a).or_default() += 1;
        out.push(c.clone());
        if out.len() >= n {
            break;
        }
    }
    out
}

pub fn is_new_release(release_date: &str, today: NaiveDate, days: i64) -> bool {
    NaiveDate::parse_from_str(release_date.get(..10).unwrap_or(""), "%Y-%m-%d")
        .is_ok_and(|d| today - chrono::Duration::days(days) <= d && d <= today)
}

/// About 70% familiar, 30% new, interleaved, no artist more than `per_artist` times. Items: (id, artist).
pub fn blend<'a>(
    familiar: &[(&'a str, &'a str)],
    new: &[(&'a str, &'a str)],
    size: usize,
    share_new: f64,
    per_artist: usize,
) -> Vec<&'a str> {
    let want_new = (size as f64 * share_new).round() as usize;
    let (mut per, mut ids) = (HashMap::<String, usize>::new(), HashSet::<&str>::new());
    let mut take = |pool: &[(&'a str, &'a str)], limit: usize| -> Vec<&'a str> {
        let mut got = vec![];
        for (id, artist) in pool {
            let a = artist.to_lowercase();
            if ids.contains(id) || per.get(&a).copied().unwrap_or(0) >= per_artist {
                continue;
            }
            got.push(*id);
            ids.insert(id);
            *per.entry(a).or_default() += 1;
            if got.len() >= limit {
                break;
            }
        }
        got
    };
    let mut n = take(new, want_new);
    let f = take(familiar, size.saturating_sub(n.len()));
    let step = (f.len() / n.len().max(1)).max(1);
    let mut out = vec![];
    for (i, id) in f.iter().enumerate() {
        out.push(*id);
        if !n.is_empty() && (i + 1) % step == 0 {
            out.push(n.remove(0));
        }
    }
    out.extend(n);
    out
}

/// Is it time to make this mix again? `last`: when it was made (seconds) or None.
pub fn due(kind: &str, last: Option<i64>, now: DateTime<Local>) -> bool {
    let Some(then) = last.and_then(|t| Local.timestamp_opt(t, 0).single()) else { return true };
    let at4 = |d: NaiveDate| Local.from_local_datetime(&d.and_hms_opt(4, 0, 0).unwrap()).earliest().unwrap_or(now);
    match kind {
        "daylist" => (then.date_naive(), part_of_day(then.hour())) != (now.date_naive(), part_of_day(now.hour())),
        "daily" => {
            let mut start = at4(now.date_naive());
            if now < start {
                start = at4(now.date_naive() - chrono::Duration::days(1));
            }
            then < start
        }
        _ => {
            let weekday = if kind == "discover" { 0 } else { 4 }; // Monday / Friday
            let back = (now.weekday().num_days_from_monday() as i64 - weekday + 7) % 7;
            let mut start = at4(now.date_naive() - chrono::Duration::days(back));
            if start > now {
                start = at4(start.date_naive() - chrono::Duration::days(7));
            }
            then < start
        }
    }
}

// ───────────── the library, as one user sees it ─────────────

#[derive(Clone, sqlx::FromRow)]
struct Song {
    id: String,
    title: String,
    artist: String,
    album_artist: String,
    album_id: String,
    genre: Option<String>,
    plays: i64,
}

impl Song {
    /// The artist a song counts for (the album's artist on compilations).
    fn credited(&self) -> &str {
        if self.artist.is_empty() || self.artist.eq_ignore_ascii_case("various artists") {
            &self.album_artist
        } else {
            &self.artist
        }
    }
}

struct Lib {
    songs: Vec<Song>,
    by_id: HashMap<String, usize>,
    /// (time, song index) of the user's plays, oldest first
    plays: Vec<(i64, usize)>,
}

impl Lib {
    async fn load(st: &AppState, uid: &str) -> Lib {
        let songs: Vec<Song> = sqlx::query_as(
            "SELECT t.id, t.title, t.artist_name AS artist, al.artist_name AS album_artist, t.album_id, t.genre,
                    COALESCE(an.play_count, 0) AS plays
             FROM track t JOIN album al ON al.id = t.album_id
             LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'track' AND an.item_id = t.id
             WHERE t.missing_since IS NULL AND t.rel_path NOT LIKE 'Trial/%'",
        )
        .bind(uid)
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
        let by_id: HashMap<String, usize> = songs.iter().enumerate().map(|(i, s)| (s.id.clone(), i)).collect();
        let rows: Vec<(i64, String)> =
            sqlx::query_as("SELECT played_at, track_id FROM play WHERE user_id = ? AND played_at >= ? ORDER BY played_at")
                .bind(uid)
                .bind(now() - 365 * 86400)
                .fetch_all(&st.db)
                .await
                .unwrap_or_default();
        let plays = rows.into_iter().filter_map(|(t, id)| by_id.get(&id).map(|i| (t, *i))).collect();
        Lib { songs, by_id, plays }
    }

    fn plays_since(&self, days: i64) -> impl Iterator<Item = &(i64, usize)> {
        let since = now() - days * 86400;
        self.plays.iter().filter(move |(t, _)| *t >= since)
    }

    /// Most played in the last `days` days (by play count when there's no play log yet).
    fn top_songs(&self, days: i64, n: usize) -> Vec<usize> {
        let mut count: HashMap<usize, (i64, usize)> = HashMap::new(); // song → (plays, first seen order)
        for (order, (_, i)) in self.plays_since(days).enumerate() {
            count.entry(*i).or_insert((0, order)).0 += 1;
        }
        let mut v: Vec<(usize, (i64, usize))> = count.into_iter().collect();
        if v.is_empty() {
            let mut all: Vec<usize> = (0..self.songs.len()).filter(|i| self.songs[*i].plays > 0).collect();
            all.sort_by_key(|i| std::cmp::Reverse(self.songs[*i].plays));
            all.truncate(n);
            return all;
        }
        v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.1.1.cmp(&b.1.1)));
        v.into_iter().take(n).map(|(i, _)| i).collect()
    }

    fn top_artists(&self, days: i64, n: usize) -> Vec<String> {
        let mut count: Vec<(String, usize)> = vec![];
        let mut at: HashMap<String, usize> = HashMap::new();
        for i in self.top_songs(days, 1000) {
            let name = self.songs[i].credited();
            if name.is_empty() || ["various artists", "[unknown artist]"].contains(&name.to_lowercase().as_str()) {
                continue;
            }
            match at.get(name) {
                Some(k) => count[*k].1 += 1,
                None => {
                    at.insert(name.to_string(), count.len());
                    count.push((name.to_string(), 1));
                }
            }
        }
        count.sort_by_key(|x| std::cmp::Reverse(x.1)); // stable: first seen wins ties
        count.into_iter().take(n).map(|(a, _)| a).collect()
    }

    /// Library songs that sound like this one (AudioMuse), else ones of the same genre.
    async fn sounds_like(&self, st: &AppState, i: usize, n: usize) -> Vec<usize> {
        let s = &self.songs[i];
        if let Some(ids) = crate::api::sonic::similar_ids(st, &s.id, n).await {
            return ids.iter().filter_map(|id| self.by_id.get(id).copied()).filter(|j| *j != i).collect();
        }
        let first =
            |g: Option<&str>| g.and_then(|g| crate::library::split_genres(g).into_iter().next()).map(|g| g.to_lowercase());
        let Some(g) = first(s.genre.as_deref()) else { return vec![] };
        let mut same: Vec<usize> =
            (0..self.songs.len()).filter(|j| *j != i && first(self.songs[*j].genre.as_deref()).as_ref() == Some(&g)).collect();
        same.shuffle(&mut rand::rngs::StdRng::seed_from_u64(seed_of(&s.id)));
        same.truncate(n);
        same
    }

    fn pair(&self, i: usize) -> (&str, &str) {
        (self.songs[i].id.as_str(), self.songs[i].artist.as_str())
    }
}

// ───────────── state and playlists ─────────────

async fn ensure_rows(st: &AppState, uid: &str) -> Result<(), sqlx::Error> {
    for k in KINDS {
        sqlx::query("INSERT OR IGNORE INTO mix_state (user_id, kind) VALUES (?, ?)").bind(uid).bind(k).execute(&st.db).await?;
    }
    Ok(())
}

async fn list_of(st: &AppState, uid: &str, kind: &str, col: &str) -> Vec<String> {
    let raw: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT {col} FROM mix_state WHERE user_id = ? AND kind = ?")))
        .bind(uid)
        .bind(kind)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default()
}

/// Make the playlist for `slot` (e.g. "daily-2") hold exactly these songs, named `name`.
async fn set_playlist(st: &AppState, uid: &str, slot: &str, name: &str, ids: &[String], comment: &str) -> anyhow::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let pid: Option<String> = sqlx::query_scalar(
        "SELECT m.playlist_id FROM mix_playlist m JOIN playlist p ON p.id = m.playlist_id WHERE m.user_id = ? AND m.slot = ?",
    )
    .bind(uid)
    .bind(slot)
    .fetch_optional(&st.db)
    .await?;
    let mut tx = st.db.begin().await?;
    let pid = match pid {
        Some(p) => {
            sqlx::query("UPDATE playlist SET name = ?, comment = ?, updated_at = ? WHERE id = ?")
                .bind(name)
                .bind(comment)
                .bind(now())
                .bind(&p)
                .execute(&mut *tx)
                .await?;
            p
        }
        None => {
            // made again if it was deleted in some app
            let p = db::random_id();
            sqlx::query("INSERT INTO playlist (id, owner_id, name, comment, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
                .bind(&p)
                .bind(uid)
                .bind(name)
                .bind(comment)
                .bind(now())
                .bind(now())
                .execute(&mut *tx)
                .await?;
            p
        }
    };
    sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(&pid).execute(&mut *tx).await?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
            .bind(&pid)
            .bind(i as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("INSERT OR REPLACE INTO mix_playlist (user_id, slot, playlist_id, song_count, made_at) VALUES (?, ?, ?, ?, ?)")
        .bind(uid)
        .bind(slot)
        .bind(&pid)
        .bind(ids.len() as i64)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Delete the playlists of `prefix` ("daily" deletes daily-1 … daily-6; "daily-5" just that one).
async fn delete_playlists(st: &AppState, uid: &str, prefix: &str) {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT slot, playlist_id FROM mix_playlist WHERE user_id = ?")
        .bind(uid)
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
    for (slot, pid) in rows {
        if slot == prefix || slot.starts_with(&format!("{prefix}-")) {
            let _ = sqlx::query("DELETE FROM playlist WHERE id = ?").bind(&pid).execute(&st.db).await;
            let _ = sqlx::query("DELETE FROM mix_playlist WHERE user_id = ? AND slot = ?")
                .bind(uid)
                .bind(&slot)
                .execute(&st.db)
                .await;
        }
    }
}

// ───────────── the mixes ─────────────

/// Ask for this mix's Deezer songs on trial for a week; let last time's unkept ones go.
async fn fetch_on_trial(st: &AppState, uid: &str, kind: &str, picked: &[Cand]) -> Vec<String> {
    let keys: Vec<String> = picked.iter().map(|c| c.key.clone()).collect();
    for old in list_of(st, uid, kind, "current_json").await {
        if old.starts_with("dz-") && !keys.contains(&old) {
            let _ = catalog::let_go_of_mix_song(st, &old, kind).await;
        }
    }
    let mut asked = vec![];
    for k in keys {
        if !k.starts_with("dz-") || catalog::ask_for_mix(st, &k, uid, kind, WEEK_HOURS).await.unwrap_or(false) {
            asked.push(k);
        }
    }
    let mut hist = list_of(st, uid, kind, "history_json").await;
    for k in &asked {
        if !hist.contains(k) {
            hist.push(k.clone());
        }
    }
    let cut = hist.len().saturating_sub(400);
    hist.drain(..cut);
    let _ = sqlx::query("UPDATE mix_state SET current_json = ?, history_json = ? WHERE user_id = ? AND kind = ?")
        .bind(json!(asked).to_string())
        .bind(json!(hist).to_string())
        .bind(uid)
        .bind(kind)
        .execute(&st.db)
        .await;
    asked
}

/// The playlist of a fetching mix: its songs that have arrived (or that you had), in order.
async fn fill_fetched(st: &AppState, uid: &str, kind: &str) {
    let mut ids: Vec<String> = vec![];
    for k in list_of(st, uid, kind, "current_json").await {
        let id = match k.strip_prefix("lib:") {
            Some(id) => Some(id.to_string()),
            None => sqlx::query_scalar::<_, Option<String>>("SELECT local_id FROM request WHERE key = ?")
                .bind(&k)
                .fetch_optional(&st.db)
                .await
                .ok()
                .flatten()
                .flatten(),
        };
        if let Some(id) = id
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    let had: Option<(i64, i64)> = sqlx::query_as(
        "SELECT m.song_count, (SELECT COUNT(*) FROM playlist p WHERE p.id = m.playlist_id) FROM mix_playlist m
         WHERE m.user_id = ? AND m.slot = ?",
    )
    .bind(uid)
    .bind(kind)
    .fetch_optional(&st.db)
    .await
    .ok()
    .flatten();
    if !ids.is_empty() && had.is_none_or(|(n, exists)| n != ids.len() as i64 || exists == 0) {
        let note = if kind == "discover" { DISCOVER_NOTE } else { RADAR_NOTE };
        let _ = set_playlist(st, uid, kind, label(kind), &ids, note).await;
    }
}

async fn followed(st: &AppState, uid: &str) -> Vec<String> {
    let raw: Option<String> = sqlx::query_scalar("SELECT value_json FROM sync_kv WHERE user_id = ? AND key = 'followedArtists'")
        .bind(uid)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    let mut v: Value = raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
    if let Value::String(s) = &v {
        v = serde_json::from_str(s).unwrap_or_default();
    }
    v.as_array().into_iter().flatten().filter_map(|a| a["name"].as_str().filter(|n| !n.is_empty()).map(String::from)).collect()
}

/// A Deezer track (as the catalog's discovery helpers give them) as a candidate.
fn cand_of(v: &Value) -> Option<Cand> {
    let id = v["id"].as_i64().filter(|i| *i > 0)?;
    Some(Cand { key: format!("dz-t-{id}"), artist: v["artist"]["name"].as_str().unwrap_or("").to_string() })
}

async fn make_discover(st: &AppState, uid: &str, lib: &Lib) -> usize {
    if !st.cfg.catalog {
        return 0;
    }
    let mut cands: Vec<Cand> = vec![];
    for (_, _, songs) in catalog::discoveries(st, uid).await.into_iter().take(8) {
        cands.extend(songs.iter().filter_map(cand_of));
    }
    for i in lib.top_songs(30, 5) {
        let s = &lib.songs[i];
        cands.extend(catalog::song_discoveries(st, &s.title, &s.artist, 10).await.iter().filter_map(cand_of));
    }
    let skip: HashSet<String> = list_of(st, uid, "discover", "history_json").await.into_iter().collect();
    let week = Local::now().iso_week();
    let picked = pick_spread(&cands, DISCOVER_SIZE, 2, &skip, (week.year() * 100 + week.week() as i32) as u64);
    fetch_on_trial(st, uid, "discover", &picked).await;
    fill_fetched(st, uid, "discover").await;
    picked.len()
}

async fn make_radar(st: &AppState, uid: &str, lib: &Lib) -> usize {
    if !st.cfg.catalog {
        return 0;
    }
    let today = Local::now().date_naive();
    let mut names = followed(st, uid).await;
    names.extend(lib.top_artists(180, 50));
    let mut seen = HashSet::new();
    names.retain(|n| seen.insert(n.to_lowercase()));
    names.truncate(100);
    let mut out: Vec<Cand> = vec![];
    for name in names {
        let Some(a) = catalog::dz_artist(st, &name).await else { continue };
        let aid = a["id"].as_i64().unwrap_or(0);
        for al in st.catalog.dz.list(&format!("artist/{aid}/albums"), &[("limit", "30")]).await {
            if !is_new_release(al["release_date"].as_str().unwrap_or(""), today, 14) {
                continue;
            }
            let title = al["title"].as_str().unwrap_or("");
            if let Some(album_id) = catalog::library_album(st, &name, title).await {
                let mine = lib.songs.iter().filter(|s| s.album_id == album_id).take(3);
                out.extend(mine.map(|s| Cand { key: format!("lib:{}", s.id), artist: name.clone() }));
            } else {
                let n = if al["record_type"].as_str() == Some("single") { 1 } else { 2 };
                let tracks = catalog::album_tracks(st, &al["id"].as_i64().unwrap_or(0).to_string()).await;
                out.extend(
                    tracks
                        .iter()
                        .take(n)
                        .filter_map(|t| t["id"].as_i64())
                        .map(|id| Cand { key: format!("dz-t-{id}"), artist: name.clone() }),
                );
            }
        }
        if out.len() >= RADAR_SIZE * 2 {
            break;
        }
    }
    let picked = pick_spread(&out, RADAR_SIZE, 3, &HashSet::new(), today.num_days_from_ce() as u64);
    fetch_on_trial(st, uid, "radar", &picked).await;
    fill_fetched(st, uid, "radar").await;
    picked.len()
}

async fn make_daily(st: &AppState, uid: &str, lib: &Lib) -> usize {
    let songs = lib.top_songs(90, 1000);
    let played: HashSet<usize> = songs.iter().copied().collect();
    let mut by_artist: HashMap<&str, Vec<usize>> = HashMap::new();
    for i in &songs {
        by_artist.entry(lib.songs[*i].artist.as_str()).or_default().push(*i);
    }
    let (mut used, mut made) = (HashSet::<String>::new(), 0);
    for artist in lib.top_artists(90, 40) {
        if made >= DAILY_COUNT {
            break;
        }
        let Some(mine) = by_artist.get(artist.as_str()).filter(|_| !used.contains(&artist)) else { continue };
        let seeds: Vec<usize> = mine.iter().take(3).copied().collect();
        let mut near = vec![];
        for sd in &seeds {
            near.extend(lib.sounds_like(st, *sd, 50).await);
        }
        let mut familiar: Vec<usize> = seeds.clone();
        familiar.extend(mine.iter().skip(3).take(7));
        familiar.extend(near.iter().filter(|i| played.contains(i) || lib.songs[**i].plays > 0));
        let new: Vec<usize> = near.iter().copied().filter(|i| !played.contains(i) && lib.songs[*i].plays == 0).collect();
        let f: Vec<(&str, &str)> = familiar.iter().map(|i| lib.pair(*i)).collect();
        let n: Vec<(&str, &str)> = new.iter().map(|i| lib.pair(*i)).collect();
        let mix = blend(&f, &n, DAILY_SIZE, 0.3, 4);
        if mix.len() < 15 {
            continue;
        }
        made += 1;
        let mut who: Vec<(String, usize)> = vec![];
        for id in &mix {
            let a = &lib.songs[lib.by_id[*id]].artist;
            match who.iter_mut().find(|(x, _)| x == a) {
                Some(w) => w.1 += 1,
                None => who.push((a.clone(), 1)),
            }
        }
        who.sort_by_key(|x| std::cmp::Reverse(x.1));
        let who: Vec<String> = who.into_iter().filter(|(a, _)| !a.is_empty()).take(3).map(|(a, _)| a).collect();
        used.extend(who.iter().cloned());
        let ids: Vec<String> = mix.iter().map(|s| s.to_string()).collect();
        let note = format!("{} and more. Mostly songs you play, some new to you from your library.", who.join(", "));
        let _ = set_playlist(st, uid, &format!("daily-{made}"), &format!("Daily Mix {made}"), &ids, &note).await;
    }
    for k in made + 1..=DAILY_COUNT {
        delete_playlists(st, uid, &format!("daily-{k}")).await; // fewer mixes than last time
    }
    made
}

async fn make_daylist(st: &AppState, uid: &str, lib: &Lib) -> usize {
    let now = Local::now();
    let part = part_of_day(now.hour());
    let mut score: Vec<(usize, i64)> = vec![];
    let mut at: HashMap<usize, usize> = HashMap::new();
    for (t, i) in lib.plays_since(120) {
        let Some(d) = Local.timestamp_opt(*t, 0).single() else { continue };
        if part_of_day(d.hour()) != part {
            continue;
        }
        let w = if d.weekday() == now.weekday() { 2 } else { 1 };
        match at.get(i) {
            Some(k) => score[*k].1 += w,
            None => {
                at.insert(*i, score.len());
                score.push((*i, w));
            }
        }
    }
    if score.len() < 10 {
        return 0; // not enough plays at this time of day yet
    }
    score.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mine: Vec<usize> = score.iter().take(35).map(|(i, _)| *i).collect();
    let mut near = vec![];
    for i in mine.iter().take(3) {
        near.extend(lib.sounds_like(st, *i, 20).await.into_iter().filter(|j| !at.contains_key(j)));
    }
    let f: Vec<(&str, &str)> = mine.iter().map(|i| lib.pair(*i)).collect();
    let n: Vec<(&str, &str)> = near.iter().map(|i| lib.pair(*i)).collect();
    let mix = blend(&f, &n, 50, 0.3, 5);
    let mut genres: Vec<(String, usize)> = vec![];
    for id in &mix {
        if let Some(g) = lib.songs[lib.by_id[*id]].genre.as_deref().filter(|g| !g.is_empty()) {
            match genres.iter_mut().find(|(x, _)| x == g) {
                Some(e) => e.1 += 1,
                None => genres.push((g.to_string(), 1)),
            }
        }
    }
    genres.sort_by_key(|x| std::cmp::Reverse(x.1));
    let genres: Vec<String> = genres.into_iter().take(2).map(|(g, _)| g).collect();
    let name = daylist_name(&genres, now);
    let ids: Vec<String> = mix.iter().map(|s| s.to_string()).collect();
    let note = format!("What you play on {} {part}s, and songs like them. Changes through the day.", now.format("%A"));
    let _ = set_playlist(st, uid, "daylist", &format!("Daylist · {name}"), &ids, &note).await;
    ids.len()
}

/// Make this mix now (one at a time per user and kind).
pub async fn run(st: Arc<AppState>, uid: String, kind: String) {
    let key = (uid.clone(), kind.clone());
    if !RUNNING.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone()) {
        return;
    }
    let lib = Lib::load(&st, &uid).await;
    let n = match kind.as_str() {
        "discover" => make_discover(&st, &uid, &lib).await,
        "radar" => make_radar(&st, &uid, &lib).await,
        "daily" => make_daily(&st, &uid, &lib).await,
        _ => make_daylist(&st, &uid, &lib).await,
    };
    tracing::info!(mix = label(&kind), songs = n, "mix made");
    let _ = sqlx::query("UPDATE mix_state SET last_at = ? WHERE user_id = ? AND kind = ?")
        .bind(now())
        .bind(&uid)
        .bind(&kind)
        .execute(&st.db)
        .await;
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
}

/// Every 10 minutes: the mixes that are due, for users whose apps have asked for mixes; and the fetching mixes'
/// playlists, as their songs arrive over the hours after they're made.
pub async fn worker(st: Arc<AppState>) {
    tokio::time::sleep(Duration::from_secs(90)).await;
    loop {
        tick(&st).await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

pub async fn tick(st: &Arc<AppState>) {
    let rows: Vec<(String, String, bool, Option<i64>)> =
        sqlx::query_as("SELECT user_id, kind, enabled, last_at FROM mix_state").fetch_all(&st.db).await.unwrap_or_default();
    for (uid, kind, on, last) in rows {
        if !on {
            continue;
        }
        if due(&kind, last, Local::now()) {
            run(st.clone(), uid.clone(), kind.clone()).await;
        }
        if kind == "discover" || kind == "radar" {
            fill_fetched(st, &uid, &kind).await;
        }
    }
}

// ───────────── for the apps ─────────────

async fn listing(st: &AppState, uid: &str) -> Result<Value, ApiError> {
    let states: Vec<(String, bool, Option<i64>)> =
        sqlx::query_as("SELECT kind, enabled, last_at FROM mix_state WHERE user_id = ?").bind(uid).fetch_all(&st.db).await?;
    let on = |k: &str| states.iter().find(|s| s.0 == k).is_none_or(|s| s.1);
    let kinds: Vec<Value> = KINDS
        .iter()
        .map(|k| {
            let last = states.iter().find(|s| s.0 == *k).and_then(|s| s.2).unwrap_or(0);
            json!({ "kind": k, "name": label(k), "enabled": on(k), "updated": last * 1000 })
        })
        .collect();
    let rows: Vec<(String, String, String, String, i64, i64)> = sqlx::query_as(
        "SELECT m.slot, m.playlist_id, p.name, p.comment, m.song_count, m.made_at FROM mix_playlist m
         JOIN playlist p ON p.id = m.playlist_id WHERE m.user_id = ?",
    )
    .bind(uid)
    .fetch_all(&st.db)
    .await?;
    let mut mixes: Vec<(usize, String, Value)> = rows
        .into_iter()
        .filter_map(|(slot, pid, name, comment, count, at)| {
            let kind = slot.split('-').next().unwrap_or("").to_string();
            let order = KINDS.iter().position(|k| *k == kind)?;
            on(&kind).then(|| {
                (
                    order,
                    slot.clone(),
                    json!({ "kind": kind, "slot": slot, "playlistId": pid, "name": name, "comment": comment,
                    "songCount": count, "updated": at * 1000 }),
                )
            })
        })
        .collect();
    mixes.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    Ok(json!({ "kind": kinds, "mix": mixes.into_iter().map(|m| m.2).collect::<Vec<_>>() }))
}

pub async fn get_mixes(c: &Ctx) -> Result<Reply, ApiError> {
    let first = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM mix_state WHERE user_id = ?")
        .bind(&c.user.id)
        .fetch_one(&c.st.db)
        .await?
        == 0;
    ensure_rows(&c.st, &c.user.id).await?;
    if first {
        // the first time an app asks: make them now rather than at the next tick
        let st = c.st.clone();
        let uid = c.user.id.clone();
        tokio::spawn(async move {
            for k in KINDS {
                run(st.clone(), uid.clone(), k.to_string()).await;
            }
        });
    }
    ok(json!({ "mixes": listing(&c.st, &c.user.id).await? }))
}

pub async fn set_mix(c: &Ctx, refresh: bool) -> Result<Reply, ApiError> {
    ensure_rows(&c.st, &c.user.id).await?;
    let kind = c.p.get("kind").unwrap_or("");
    if KINDS.contains(&kind) {
        if refresh {
            tokio::spawn(run(c.st.clone(), c.user.id.clone(), kind.to_string()));
        } else {
            let on = c.p.get("enabled").unwrap_or("true").to_lowercase() != "false";
            sqlx::query("UPDATE mix_state SET enabled = ? WHERE user_id = ? AND kind = ?")
                .bind(on)
                .bind(&c.user.id)
                .bind(kind)
                .execute(&c.st.db)
                .await?;
            if on {
                tokio::spawn(run(c.st.clone(), c.user.id.clone(), kind.to_string()));
            } else {
                delete_playlists(&c.st, &c.user.id, kind).await;
            }
        }
    }
    ok(json!({ "mixes": listing(&c.st, &c.user.id).await? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_of_the_day_and_names() {
        assert_eq!(part_of_day(0), "night");
        assert_eq!(part_of_day(3), "late night");
        assert_eq!(part_of_day(6), "early morning");
        assert_eq!(part_of_day(13), "afternoon");
        assert_eq!(part_of_day(22), "night");
        let sun_evening = Local.with_ymd_and_hms(2026, 11, 22, 19, 0, 0).unwrap();
        assert_eq!(daylist_name(&["Indie Rock".into()], sun_evening), "indie rock sunday evening");
        let after_midnight = Local.with_ymd_and_hms(2026, 11, 23, 0, 30, 0).unwrap(); // still Sunday night
        assert_eq!(daylist_name(&[], after_midnight), "sunday night");
    }

    #[test]
    fn when_mixes_are_due() {
        let mon_10 = Local.with_ymd_and_hms(2026, 11, 23, 10, 0, 0).unwrap();
        let sun = Local.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap().timestamp();
        let mon_5 = Local.with_ymd_and_hms(2026, 11, 23, 5, 0, 0).unwrap().timestamp();
        assert!(due("discover", None, mon_10));
        assert!(due("discover", Some(sun), mon_10));
        assert!(!due("discover", Some(mon_5), mon_10));
        assert!(!due("radar", Some(sun), mon_10)); // last Friday 04:00 was before Sunday
        assert!(due("daily", Some(sun), mon_10));
        assert!(!due("daily", Some(mon_5), mon_10));
        assert!(due("daylist", Some(mon_5), mon_10)); // early morning → morning
    }

    #[test]
    fn picking_and_blending() {
        let c = |k: &str, a: &str| Cand { key: k.into(), artist: a.into() };
        let cands = vec![c("1", "A"), c("2", "A"), c("3", "A"), c("4", "B"), c("5", "B"), c("6", "C")];
        let skip = HashSet::from(["6".to_string()]);
        let p = pick_spread(&cands, 10, 2, &skip, 7);
        assert_eq!(p.len(), 4);
        assert_eq!(p.iter().filter(|x| x.artist == "A").count(), 2);
        assert_eq!(
            pick_spread(&cands, 10, 2, &skip, 7).iter().map(|x| &x.key).collect::<Vec<_>>(),
            p.iter().map(|x| &x.key).collect::<Vec<_>>()
        );
        let today = NaiveDate::from_ymd_opt(2026, 11, 21).unwrap();
        assert!(is_new_release("2026-11-10", today, 14));
        assert!(!is_new_release("2026-10-01", today, 14));
        assert!(!is_new_release("soon", today, 14));
        let fam: Vec<(&str, &str)> =
            (0..10).map(|i| (["f0", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9"][i], "X")).collect();
        let new = [("n0", "Y"), ("n1", "Z"), ("n2", "W")];
        let m = blend(&fam, &new, 10, 0.3, 4);
        assert_eq!(m.len(), 7); // 3 new + 4 of X (at most 4 per artist)
        assert_eq!(m.iter().filter(|x| x.starts_with('n')).count(), 3);
        assert_ne!(m[0], "n0"); // new songs come in between familiar ones
    }
}
