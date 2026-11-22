//! Encore's own calls (OpenSubsonic-style extensions), answered the way music-lookup answered them:
//! devices and remote control, sync, signing in with your phone, listening stats, real folders, and Jam (the app's
//! side; guests use the Jam listener, `crate::jam`).

use std::collections::{HashMap, HashSet};

use chrono::{Datelike, Duration as CDuration, Local, NaiveDate, TimeZone, Timelike};
use rand::Rng;
use serde_json::{Map, Value, json};

use super::{ApiError, Ctx, Reply, empty, ok};
use crate::{
    db,
    live::{self, Device, Pair, now_ms},
};

fn bad(msg: &str) -> ApiError {
    ApiError::new(10, msg)
}

fn clean(s: Option<&str>, n: usize) -> String {
    s.unwrap_or("").chars().filter(|c| !c.is_control()).take(n).collect()
}

fn json_param(c: &Ctx, k: &str) -> Option<Value> {
    c.p.get(k).and_then(|s| serde_json::from_str(s).ok())
}

fn device_id_ok(id: &str) -> bool {
    (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

const MAX_STATE: usize = 64 * 1024;

fn listing(inner: &mut live::Inner, owner: &str) -> Vec<Value> {
    let now = now_ms();
    inner.devices.retain(|_, d| now - d.seen_ms < 24 * 3600 * 1000);
    let mut v: Vec<(bool, String, Value)> = inner
        .devices
        .iter()
        .filter(|((o, _), _)| o == owner)
        .map(|((_, id), d)| (now - d.seen_ms >= live::ONLINE_SECS * 1000, d.name.to_lowercase(), d.public(id, now)))
        .collect();
    v.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    v.into_iter().map(|x| x.2).collect()
}

// ── devices ──

pub async fn device_hello(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.get("id").unwrap_or("");
    if !device_id_ok(id) {
        return Err(bad("bad device id"));
    }
    let state = json_param(c, "state")
        .and_then(|v| v.as_object().cloned())
        .filter(|s| Value::Object(s.clone()).to_string().len() <= MAX_STATE);
    let devices = {
        let mut g = c.st.live.lock();
        let d = g.devices.entry((c.user.id.clone(), id.to_string())).or_insert_with(|| Device {
            name: "Encore".into(),
            kind: "phone".into(),
            client: String::new(),
            state: Map::new(),
            seen_ms: 0,
            cmds: vec![],
            seq: 0,
        });
        d.name = Some(clean(c.p.get("name"), 60)).filter(|s| !s.is_empty()).unwrap_or_else(|| "Encore".into());
        let kind = c.p.get("kind").unwrap_or("phone");
        d.kind = if ["phone", "computer", "tablet"].contains(&kind) { kind.into() } else { "phone".into() };
        d.client = clean(Some(&c.client), 40);
        if let Some(s) = state {
            d.state = s;
        }
        d.seen_ms = now_ms();
        listing(&mut g, &c.user.id)
    };
    c.st.live.bump();
    ok(json!({ "devices": { "device": devices } }))
}

pub async fn device_state(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.get("id").unwrap_or("");
    let Some(Value::Object(mut state)) = json_param(c, "state") else { return Err(bad("state too big")) };
    if Value::Object(state.clone()).to_string().len() > MAX_STATE {
        return Err(bad("state too big"));
    }
    {
        let mut g = c.st.live.lock();
        let Some(d) = g.devices.get_mut(&(c.user.id.clone(), id.to_string())) else { return Err(gone()) };
        state.insert("at".into(), now_ms().into()); // server time, so devices with wrong clocks still compare
        d.state = state;
        d.seen_ms = now_ms();
    }
    c.st.live.bump();
    empty()
}

fn gone() -> ApiError {
    ApiError::new(70, "No device or code like that (it may have gone)")
}

pub async fn device_list(c: &Ctx) -> Result<Reply, ApiError> {
    let devices = listing(&mut c.st.live.lock(), &c.user.id);
    ok(json!({ "devices": { "device": devices } }))
}

pub async fn device_command(c: &Ctx) -> Result<Reply, ApiError> {
    let target = c.p.get("target").unwrap_or("");
    let Some(Value::Object(cmd)) = json_param(c, "command") else { return Err(bad("bad command")) };
    if !cmd.get("type").is_some_and(Value::is_string) {
        return Err(bad("bad command"));
    }
    if Value::Object(cmd.clone()).to_string().len() > MAX_STATE {
        return Err(bad("command too big"));
    }
    let out = {
        let mut g = c.st.live.lock();
        let Some(d) = g.devices.get_mut(&(c.user.id.clone(), target.to_string())) else { return Err(gone()) };
        d.seq += 1;
        let mut cmd = Value::Object(cmd);
        cmd["seq"] = d.seq.into();
        cmd["sender"] = c.p.get("id").unwrap_or("").into();
        cmd["at"] = now_ms().into();
        d.cmds.push(cmd);
        let n = d.cmds.len();
        if n > 100 {
            d.cmds.drain(..n - 100);
        }
        json!({ "seq": d.seq, "online": now_ms() - d.seen_ms < live::ONLINE_SECS * 1000 })
    };
    c.st.live.bump();
    ok(json!({ "deviceCommand": out }))
}

/// Commands newer than `ack`, and the account's devices when one changed after `since` (ms). Waits up to `wait`
/// seconds for either. Also the device's heartbeat.
pub async fn device_poll(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.get("id").unwrap_or("").to_string();
    let ack = c.p.int("ack").unwrap_or(0);
    let since = c.p.int("since").unwrap_or(0);
    let wait = c.p.int("wait").unwrap_or(20).clamp(0, 25) as u64;
    let owner = c.user.id.clone();
    if !c.st.live.lock().devices.contains_key(&(owner.clone(), id.clone())) {
        return Err(gone());
    }
    let out =
        c.st.live
            .wait(wait, |g, last| {
                let key = (owner.clone(), id.clone());
                let Some(d) = g.devices.get_mut(&key) else { return Some(None) };
                d.seen_ms = now_ms();
                let new: Vec<Value> = d.cmds.iter().filter(|x| x["seq"].as_i64().unwrap_or(0) > ack).cloned().collect();
                let changed = g.devices.iter().any(|((o, did), x)| *o == owner && *did != id && x.state_at() > since);
                if new.is_empty() && !changed && !last {
                    return None;
                }
                Some(Some(json!({ "commands": new, "devices": listing(g, &owner), "now": now_ms() })))
            })
            .await;
    match out {
        Some(v) => ok(json!({ "devicePoll": v })),
        None => Err(gone()),
    }
}

pub async fn device_bye(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.get("id").unwrap_or("").to_string();
    c.st.live.lock().devices.remove(&(c.user.id.clone(), id));
    c.st.live.bump();
    empty()
}

// ── sync: small values, newest wins ──

const MAX_SYNC_VALUE: usize = 64 * 1024;
const MAX_SYNC_KEYS: i64 = 60;

fn sync_key_ok(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.chars().all(|c| c.is_ascii_alphanumeric() || "_.:-".contains(c))
}

pub async fn sync_get(c: &Ctx) -> Result<Reply, ApiError> {
    let rows: Vec<(String, String, i64, String)> =
        sqlx::query_as("SELECT key, value_json, at, by_device FROM sync_kv WHERE user_id = ?")
            .bind(&c.user.id)
            .fetch_all(&c.st.db)
            .await?;
    let mut values = Map::new();
    for (k, v, at, by) in rows {
        values.insert(k, json!({ "value": serde_json::from_str::<Value>(&v).unwrap_or(Value::Null), "at": at, "by": by }));
    }
    ok(json!({ "sync": { "values": values } }))
}

pub async fn sync_set(c: &Ctx) -> Result<Reply, ApiError> {
    let key = c.p.get("key").unwrap_or("");
    if !sync_key_ok(key) {
        return Err(bad("bad key"));
    }
    let value = json_param(c, "value").unwrap_or(Value::Null);
    let text = value.to_string();
    if text.len() > MAX_SYNC_VALUE {
        return Err(bad("value too big"));
    }
    let now = now_ms();
    let at = c.p.int("at").unwrap_or(now).min(now + 60_000); // a clock far in the future can't win forever
    let by = clean(c.p.get("id"), 64);
    let cur: Option<(String, i64, String)> =
        sqlx::query_as("SELECT value_json, at, by_device FROM sync_kv WHERE user_id = ? AND key = ?")
            .bind(&c.user.id)
            .bind(key)
            .fetch_optional(&c.st.db)
            .await?;
    if cur.is_none() {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sync_kv WHERE user_id = ?").bind(&c.user.id).fetch_one(&c.st.db).await?;
        if n >= MAX_SYNC_KEYS {
            return Err(bad("too many keys"));
        }
    }
    let changed = cur.as_ref().is_none_or(|(_, cur_at, _)| at >= *cur_at);
    let out = if changed {
        sqlx::query(
            "INSERT INTO sync_kv (user_id, key, value_json, at, by_device) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(user_id, key) DO UPDATE SET value_json = excluded.value_json, at = excluded.at, by_device = excluded.by_device",
        )
        .bind(&c.user.id)
        .bind(key)
        .bind(&text)
        .bind(at)
        .bind(&by)
        .execute(&c.st.db)
        .await?;
        json!({ "value": value, "at": at, "by": by })
    } else {
        let (v, a, b) = cur.unwrap();
        json!({ "value": serde_json::from_str::<Value>(&v).unwrap_or(Value::Null), "at": a, "by": b })
    };
    ok(json!({ "sync": { "value": out, "changed": changed } }))
}

// ── sign in with your phone ──

fn expire_pairs(g: &mut live::Inner) {
    let now = now_ms();
    g.pairs.retain(|_, p| now - p.created_ms < live::PAIR_SECS * 1000);
}

/// No login: the new device asks for a code to show.
pub async fn pair_start(st: &super::Shared, name: Option<&str>, ip: &str) -> Result<Value, ApiError> {
    let mut g = st.live.lock();
    expire_pairs(&mut g);
    if g.pairs.len() >= live::MAX_PAIRS || g.pairs.values().filter(|p| p.ip == ip).count() >= live::MAX_PAIRS_PER_IP {
        return Err(bad("Too many sign-ins waiting. Try again in a few minutes."));
    }
    let live_codes: HashSet<String> = g.pairs.values().map(|p| p.code.clone()).collect();
    let mut rng = rand::thread_rng();
    let code = loop {
        let c = format!("{:06}", rng.gen_range(0..1_000_000));
        if !live_codes.contains(&c) {
            break c;
        }
    };
    let token = format!("{}{}", db::random_id(), &db::random_id()[..8]);
    let name = Some(clean(name, 60)).filter(|s| !s.is_empty()).unwrap_or_else(|| "a computer".into());
    g.pairs.insert(token.clone(), Pair { code: code.clone(), ip: ip.to_string(), created_ms: now_ms(), name, login: None });
    Ok(json!({ "code": code, "token": token, "expires": live::PAIR_SECS }))
}

/// No login: the new device waits for the phone (once handed over, the code is gone).
pub async fn pair_poll(st: &super::Shared, token: &str, wait: u64) -> Result<Value, ApiError> {
    let token = token.to_string();
    st.live
        .wait(wait, |g, last| {
            expire_pairs(g);
            let Some(p) = g.pairs.get(&token) else {
                return Some(Err(ApiError::new(70, "This code has run out. Ask for a new one.")));
            };
            if let Some(login) = p.login.clone() {
                g.pairs.remove(&token);
                return Some(Ok(json!({ "status": "done", "login": login })));
            }
            if last { Some(Ok(json!({ "status": "waiting" }))) } else { None }
        })
        .await
}

/// The phone (signed in) hands its login to the device showing `code`.
pub async fn pair_approve(c: &Ctx) -> Result<Reply, ApiError> {
    let code: String = c.p.get("code").unwrap_or("").chars().filter(char::is_ascii_digit).collect();
    let login = json_param(c, "login").unwrap_or(Value::Null);
    let field = |k: &str| login.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    if !field("user").eq_ignore_ascii_case(&c.user.name) || field("pass").is_empty() || field("server").is_empty() {
        return Err(bad("That isn't this account's login"));
    }
    let name = {
        let mut g = c.st.live.lock();
        expire_pairs(&mut g);
        let Some(p) = g.pairs.values_mut().find(|p| p.code == code && p.login.is_none()) else {
            return Err(ApiError::new(70, "That code isn't right, or it has run out. Check the code on the computer."));
        };
        let mut l = Map::new();
        for k in ["server", "user", "pass", "localAddress"] {
            l.insert(k.into(), field(k).chars().take(300).collect::<String>().into());
        }
        p.login = Some(Value::Object(l));
        p.name.clone()
    };
    c.st.live.bump();
    ok(json!({ "pair": { "name": name } }))
}

// ── listening stats ──

#[derive(sqlx::FromRow, Clone)]
struct StatSong {
    id: String,
    title: String,
    artist_id: String,
    artist_name: String,
    album_id: String,
    album_name: String,
    album_artist_id: String,
    album_artist_name: String,
    duration_ms: i64,
    year: Option<i64>,
    number: Option<i64>,
    disc: Option<i64>,
    suffix: String,
    bitrate: i64,
    rel_path: String,
    starred_at: Option<i64>,
    play_count: Option<i64>,
}

const STAT_SELECT: &str =
    "SELECT t.id, t.title, t.artist_id, t.artist_name, t.album_id, t.album_name, al.artist_id AS album_artist_id,
    al.artist_name AS album_artist_name, t.duration_ms, t.year, t.number, t.disc, t.suffix, t.bitrate, t.rel_path,
    an.starred_at, an.play_count
  FROM track t JOIN album al ON al.id = t.album_id
  LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'track' AND an.item_id = t.id";

fn stat_song(s: &StatSong, plays: i64) -> Value {
    let mut v = json!({
        "id": s.id, "title": s.title, "artist": s.artist_name, "artistId": s.artist_id, "album": s.album_name,
        "albumId": s.album_id, "duration": s.duration_ms / 1000, "year": s.year, "track": s.number, "coverArt": s.album_id,
        "isDir": false, "type": "music", "plays": plays, "suffix": s.suffix, "bitRate": s.bitrate,
    });
    if let Some(t) = s.starred_at {
        v["starred"] = db::iso(t).into();
    }
    v
}

/// [start, end) of this week / month / year (offset 1 = the one before), local time, in seconds.
fn period_range(period: &str, offset: i64) -> (i64, i64) {
    let today = Local::now().date_naive();
    let (start, end): (NaiveDate, NaiveDate) = match period {
        "week" => {
            let s = today - CDuration::days(today.weekday().num_days_from_monday() as i64) - CDuration::weeks(offset);
            (s, s + CDuration::weeks(1))
        }
        "month" => {
            let mut y = today.year();
            let mut m = today.month() as i64 - offset;
            while m <= 0 {
                y -= 1;
                m += 12;
            }
            let s = NaiveDate::from_ymd_opt(y, m as u32, 1).unwrap();
            let e =
                if m == 12 { NaiveDate::from_ymd_opt(y + 1, 1, 1) } else { NaiveDate::from_ymd_opt(y, m as u32 + 1, 1) }.unwrap();
            (s, e)
        }
        _ => {
            let y = today.year() - offset as i32;
            (NaiveDate::from_ymd_opt(y, 1, 1).unwrap(), NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap())
        }
    };
    let ts =
        |d: NaiveDate| Local.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap()).earliest().map(|t| t.timestamp()).unwrap_or(0);
    (ts(start), ts(end))
}

pub async fn listening_stats(c: &Ctx) -> Result<Reply, ApiError> {
    let period = match c.p.get("period").unwrap_or("month") {
        p @ ("week" | "month" | "year" | "all") => p,
        _ => "month",
    };
    let offset = c.p.int("offset").unwrap_or(0).max(0);
    ok(json!({ "listeningStats": compute_stats(c, period, offset).await? }))
}

async fn compute_stats(c: &Ctx, period: &str, offset: i64) -> Result<Value, ApiError> {
    let uid = &c.user.id;
    let mut counted: Vec<(StatSong, i64)> = vec![];
    let mut times: Vec<(i64, StatSong)> = vec![];
    let mut range = None;
    if period == "all" {
        let rows: Vec<StatSong> =
            sqlx::query_as(&format!("{STAT_SELECT} WHERE an.play_count > 0")).bind(uid).fetch_all(&c.st.db).await?;
        counted = rows.into_iter().map(|s| (s.clone(), s.play_count.unwrap_or(0))).collect();
    } else {
        let (start, end) = period_range(period, offset);
        range = Some((start, end));
        let rows: Vec<(i64, String)> = sqlx::query_as(
            "SELECT played_at, track_id FROM play WHERE user_id = ? AND played_at >= ? AND played_at < ? ORDER BY played_at",
        )
        .bind(uid)
        .bind(start)
        .bind(end)
        .fetch_all(&c.st.db)
        .await?;
        let ids: HashSet<&str> = rows.iter().map(|(_, id)| id.as_str()).collect();
        let songs = songs_by_id(c, ids.into_iter().collect()).await?;
        let mut n: HashMap<&str, i64> = HashMap::new();
        for (t, id) in &rows {
            if let Some(s) = songs.get(id) {
                *n.entry(id.as_str()).or_default() += 1;
                times.push((*t, s.clone()));
            }
        }
        counted = n.into_iter().map(|(id, k)| (songs[id].clone(), k)).collect();
    }
    let plays: i64 = counted.iter().map(|(_, n)| n).sum();
    let seconds: f64 = counted.iter().map(|(s, n)| *n as f64 * s.duration_ms as f64 / 1000.0).sum();
    // top artists and albums: (plays, seconds, a song of theirs)
    let mut artists: HashMap<String, (i64, f64, StatSong)> = HashMap::new();
    let mut albums: HashMap<String, (i64, f64, StatSong)> = HashMap::new();
    for (s, n) in &counted {
        let secs = *n as f64 * s.duration_ms as f64 / 1000.0;
        for (key, table) in [(s.artist_id.clone(), &mut artists), (s.album_id.clone(), &mut albums)] {
            let e = table.entry(key).or_insert((0, 0.0, s.clone()));
            e.0 += n;
            e.1 += secs;
        }
    }
    let mut top_songs = counted.clone();
    top_songs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.title.cmp(&b.0.title)));
    let mut top_artists: Vec<_> = artists.values().collect();
    top_artists.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut top_albums: Vec<_> = albums.values().collect();
    top_albums.sort_by_key(|a| std::cmp::Reverse(a.0));
    let mut out = json!({
        "period": period, "offset": offset, "plays": plays, "minutes": (seconds / 60.0).round() as i64,
        "songs": counted.len(), "artistCount": artists.len(), "albumCount": albums.len(),
        "song": top_songs.iter().take(50).map(|(s, n)| stat_song(s, *n)).collect::<Vec<_>>(),
        "artist": top_artists.iter().take(30).map(|(n, sec, s)| json!({ "id": s.artist_id, "name": s.artist_name, "plays": n,
            "minutes": (sec / 60.0).round() as i64, "coverArt": s.artist_id })).collect::<Vec<_>>(),
        "album": top_albums.iter().take(30).map(|(n, sec, s)| json!({ "id": s.album_id, "name": s.album_name, "artist": s.album_artist_name,
            "artistId": s.album_artist_id, "plays": n, "minutes": (sec / 60.0).round() as i64, "coverArt": s.album_id, "year": s.year })).collect::<Vec<_>>(),
    });
    if let Some((start, end)) = range {
        out["from"] = (start * 1000).into();
        out["to"] = (end * 1000).into();
        let mut days: std::collections::BTreeMap<String, f64> = Default::default();
        let mut hours = [0f64; 24];
        for (t, s) in &times {
            let Some(dt) = Local.timestamp_opt(*t, 0).single() else { continue };
            let mins = s.duration_ms as f64 / 60_000.0;
            *days.entry(dt.format("%Y-%m-%d").to_string()).or_default() += mins;
            hours[dt.hour() as usize] += mins;
        }
        out["day"] = days.into_iter().map(|(d, m)| json!({ "date": d, "minutes": m.round() as i64 })).collect::<Vec<_>>().into();
        out["hour"] = hours.iter().map(|h| h.round() as i64).collect::<Vec<_>>().into();
        let before: HashSet<String> = sqlx::query_scalar(
            "SELECT DISTINCT t.artist_id FROM play p JOIN track t ON t.id = p.track_id WHERE p.user_id = ? AND p.played_at < ?",
        )
        .bind(uid)
        .bind(start)
        .fetch_all(&c.st.db)
        .await?
        .into_iter()
        .collect();
        let heard: HashSet<&String> = times.iter().map(|(_, s)| &s.artist_id).collect();
        out["newArtists"] = heard.iter().filter(|a| !before.contains(a.as_str())).count().into();
    }
    let since: Option<i64> =
        sqlx::query_scalar("SELECT MIN(played_at) FROM play WHERE user_id = ?").bind(uid).fetch_one(&c.st.db).await?;
    out["since"] = (since.unwrap_or(0) * 1000).into();
    Ok(out)
}

/// The year (or month) in review ("Your 2026"): the stats' totals and top 5s, plus top genres, the first song,
/// the longest run of days with music, the peak hour and discoveries (first heard in the period, 10+ plays in it).
pub async fn listening_review(c: &Ctx) -> Result<Reply, ApiError> {
    let period = if c.p.get("period") == Some("month") { "month" } else { "year" };
    let offset = c.p.int("offset").unwrap_or(0).max(0);
    let mut out = compute_stats(c, period, offset).await?;
    for k in ["song", "artist", "album"] {
        if let Some(a) = out[k].as_array_mut() {
            a.truncate(5);
        }
    }
    let (start, end) = period_range(period, offset);
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT played_at, track_id FROM play WHERE user_id = ? AND played_at >= ? AND played_at < ? ORDER BY played_at",
    )
    .bind(&c.user.id)
    .bind(start)
    .bind(end)
    .fetch_all(&c.st.db)
    .await?;
    let songs = songs_by_id(c, rows.iter().map(|(_, id)| id.as_str()).collect::<HashSet<_>>().into_iter().collect()).await?;
    let genres_of: HashMap<String, Option<String>> = {
        let mut m = HashMap::new();
        let ids: Vec<&String> = songs.keys().collect();
        for chunk in ids.chunks(400) {
            let sql = format!("SELECT id, genre FROM track WHERE id IN ({})", vec!["?"; chunk.len()].join(","));
            let mut q = sqlx::query_as::<_, (String, Option<String>)>(&sql);
            for id in chunk {
                q = q.bind(*id);
            }
            m.extend(q.fetch_all(&c.st.db).await?);
        }
        m
    };
    let rows: Vec<(i64, &StatSong)> = rows.iter().filter_map(|(t, id)| songs.get(id).map(|s| (*t, s))).collect();
    let mut genres: HashMap<String, f64> = HashMap::new();
    for (_, s) in &rows {
        for g in crate::library::split_genres(&genres_of.get(&s.id).cloned().flatten().unwrap_or_default()) {
            *genres.entry(g).or_default() += s.duration_ms as f64 / 60_000.0;
        }
    }
    let mut genres: Vec<_> = genres.into_iter().collect();
    genres.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out["genre"] =
        genres.iter().take(5).map(|(g, m)| json!({ "name": g, "minutes": m.round() as i64 })).collect::<Vec<_>>().into();
    out["firstSong"] = rows.first().map(|(_, s)| stat_song(s, 1)).unwrap_or(Value::Null);
    // the longest run of days in a row with music
    let days: std::collections::BTreeSet<NaiveDate> =
        rows.iter().filter_map(|(t, _)| Local.timestamp_opt(*t, 0).single().map(|d| d.date_naive())).collect();
    let (mut best, mut run, mut best_from, mut run_from, mut prev) = (0, 0, None, None, None::<NaiveDate>);
    for d in &days {
        if prev.is_some_and(|p| (*d - p).num_days() == 1) {
            run += 1;
        } else {
            run = 1;
            run_from = Some(*d);
        }
        if run > best {
            best = run;
            best_from = run_from;
        }
        prev = Some(*d);
    }
    out["streak"] = json!({ "days": best, "from": best_from.map(|d| d.format("%Y-%m-%d").to_string()) });
    // the busiest hour of the day, by unrounded listening time (the first of equals)
    let mut hours = [0f64; 24];
    for (t, s) in &rows {
        if let Some(dt) = Local.timestamp_opt(*t, 0).single() {
            hours[dt.hour() as usize] += s.duration_ms as f64;
        }
    }
    out["peakHour"] = if hours.iter().any(|h| *h > 0.0) {
        let top = hours.iter().cloned().fold(0.0, f64::max);
        hours.iter().position(|h| *h == top).into()
    } else {
        Value::Null
    };
    let heard_before: HashSet<String> =
        sqlx::query_scalar("SELECT DISTINCT track_id FROM play WHERE user_id = ? AND played_at < ?")
            .bind(&c.user.id)
            .bind(start)
            .fetch_all(&c.st.db)
            .await?
            .into_iter()
            .collect();
    let mut n: HashMap<&str, i64> = HashMap::new();
    for (_, s) in &rows {
        if !heard_before.contains(&s.id) {
            *n.entry(s.id.as_str()).or_default() += 1;
        }
    }
    let mut found: Vec<(&str, i64)> = n.into_iter().filter(|(_, k)| *k >= 10).collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    out["discoveryCount"] = found.len().into();
    out["discoveries"] = found.iter().take(10).map(|(id, k)| stat_song(&songs[*id], *k)).collect::<Vec<_>>().into();
    ok(json!({ "listeningReview": out }))
}

async fn songs_by_id(c: &Ctx, ids: Vec<&str>) -> Result<HashMap<String, StatSong>, ApiError> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("{STAT_SELECT} WHERE t.id IN ({marks})");
        let mut q = sqlx::query_as::<_, StatSong>(&sql).bind(&c.user.id);
        for id in chunk {
            q = q.bind(*id);
        }
        for s in q.fetch_all(&c.st.db).await? {
            out.insert(s.id.clone(), s);
        }
    }
    Ok(out)
}

/// The real folders on disk: what's directly in `path` ("" = the top).
pub async fn folder(c: &Ctx) -> Result<Reply, ApiError> {
    let path = c.p.get("path").unwrap_or("").trim_matches('/').to_string();
    let pre = if path.is_empty() { String::new() } else { format!("{path}/") };
    let pat = format!("{}%", pre.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let rows: Vec<StatSong> =
        sqlx::query_as(&format!("{STAT_SELECT} WHERE t.missing_since IS NULL AND t.rel_path LIKE ? ESCAPE '\\'"))
            .bind(&c.user.id)
            .bind(&pat)
            .fetch_all(&c.st.db)
            .await?;
    let mut dirs: HashMap<String, (i64, String)> = HashMap::new();
    let mut songs = vec![];
    for s in rows {
        let rest = &s.rel_path[pre.len()..];
        match rest.split_once('/') {
            Some((name, _)) => {
                let e = dirs.entry(name.to_string()).or_insert((0, s.album_id.clone()));
                e.0 += 1;
            }
            None => songs.push(s),
        }
    }
    songs.sort_by(|a, b| {
        (a.disc.unwrap_or(0), a.number.unwrap_or(0), &a.rel_path).cmp(&(b.disc.unwrap_or(0), b.number.unwrap_or(0), &b.rel_path))
    });
    let mut d: Vec<(String, i64, String)> = dirs.into_iter().map(|(n, (k, cov))| (n, k, cov)).collect();
    d.sort_by_key(|x| x.0.to_lowercase());
    ok(json!({ "folder": {
        "path": path,
        "dir": d.into_iter().map(|(name, count, cover)| json!({ "name": name, "path": format!("{pre}{name}"), "count": count, "coverArt": cover })).collect::<Vec<_>>(),
        "song": songs.iter().map(|s| stat_song(s, s.play_count.unwrap_or(0))).collect::<Vec<_>>(),
    } }))
}

// ── Jam: the app's side ──

fn jam_gone() -> ApiError {
    ApiError::new(70, "This Jam has ended")
}

pub async fn jam_start(c: &Ctx) -> Result<Reply, ApiError> {
    let Some(base) = c.st.cfg.jam_public_url.clone().filter(|u| !u.is_empty()) else {
        return Err(ApiError::new(0, "Jam isn't set up on the server yet (jam_public_url)"));
    };
    let host = Some(clean(c.p.get("name"), 40)).filter(|s| !s.is_empty()).unwrap_or_else(|| c.user.name.clone());
    let token = {
        let mut g = c.st.live.lock();
        let existing = g.jams.values().find(|j| j.owner_id == c.user.id).map(|j| j.token.clone()); // one Jam per account
        let token = existing.unwrap_or_else(|| format!("{}{}", db::random_id(), &db::random_id()[..8]));
        let j = g.jams.entry(token.clone()).or_insert_with(|| live::Jam {
            token: token.clone(),
            owner_id: c.user.id.clone(),
            host_name: host.clone(),
            ..Default::default()
        });
        j.seen_ms = now_ms();
        token
    };
    crate::jam::save(&c.st).await;
    ok(json!({ "jam": { "token": token, "url": format!("{}/jam/{token}", base.trim_end_matches('/')) } }))
}

pub async fn jam_end(c: &Ctx) -> Result<Reply, ApiError> {
    let token = c.p.get("token").unwrap_or("").to_string();
    {
        let mut g = c.st.live.lock();
        if g.jams.get(&token).is_some_and(|j| j.owner_id == c.user.id) {
            g.jams.remove(&token);
        }
    }
    c.st.live.bump();
    crate::jam::save(&c.st).await;
    ok(json!({ "jam": {} }))
}

pub async fn jam_state(c: &Ctx) -> Result<Reply, ApiError> {
    let token = c.p.get("token").unwrap_or("");
    let state = json_param(c, "state").and_then(|v| v.as_object().cloned()).unwrap_or_default();
    let mut g = c.st.live.lock();
    let Some(j) = g.jams.get_mut(token).filter(|j| j.owner_id == c.user.id) else { return Err(jam_gone()) };
    let mut songs: Vec<&Value> = vec![];
    if let Some(n) = state.get("now") {
        songs.push(n);
    }
    if let Some(Value::Array(q)) = state.get("queue") {
        songs.extend(q.iter());
    }
    for s in songs {
        if let Some(cv) = s.get("cover").and_then(Value::as_str) {
            j.covers.insert(cv.to_string());
        }
    }
    if j.covers.len() > 6000 {
        j.covers.clear();
    }
    j.state = state;
    j.seen_ms = now_ms();
    drop(g);
    ok(json!({ "jam": {} }))
}

pub async fn jam_poll(c: &Ctx) -> Result<Reply, ApiError> {
    let token = c.p.get("token").unwrap_or("").to_string();
    let ack = c.p.int("ack").unwrap_or(0);
    let wait = c.p.int("wait").unwrap_or(20).clamp(0, 25) as u64;
    let owner = c.user.id.clone();
    let out =
        c.st.live
            .wait(wait, |g, last| {
                let Some(j) = g.jams.get_mut(&token).filter(|j| j.owner_id == owner) else { return Some(None) };
                j.seen_ms = now_ms();
                let new: Vec<Value> = j.cmds.iter().filter(|x| x["seq"].as_i64().unwrap_or(0) > ack).cloned().collect();
                if new.is_empty() && !last {
                    return None;
                }
                Some(Some(json!({ "commands": new, "guests": j.guest_count(now_ms()) })))
            })
            .await;
    match out {
        Some(v) => ok(json!({ "jam": v })),
        None => Err(jam_gone()),
    }
}

/// A scrobble is a play for the stats, unless the same song was logged by this user in the last 30 seconds.
pub async fn is_duplicate_play(c: &Ctx, track: &str, at: i64) -> Result<bool, ApiError> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM play WHERE user_id = ? AND track_id = ? AND played_at > ? AND played_at <= ?")
            .bind(&c.user.id)
            .bind(track)
            .bind(at - 30)
            .bind(at + 30)
            .fetch_one(&c.st.db)
            .await?;
    Ok(n > 0)
}
