//! Concerts near you, for Encore: once a week, the upcoming shows of the artists you play most (top 150 over a
//! year) and the ones you follow in Encore, near the town you set there (Encore's synced `showsPrefs`: city and
//! Ticketmaster key). Ticketmaster's Discovery API with a key (more shows, ticket links); MusicBrainz events
//! without one (fewer, no key). Official APIs only. Nothing is looked up for a user until their app asks.
//!
//!   getConcerts     → {"concerts": {"city", "source", "updated", "show": [{artist, date, time, name, venue, city,
//!                      region, country, url, source, first}]}}   (first: when it was first seen, ms)
//!   refreshConcerts → look again now (also done by itself when the town or the key changes)

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use chrono::Local;
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::{
    api::{ApiError, AppState, Ctx, Reply, ok},
    db::{now, now_ms},
};

const TOP: usize = 150;
const UA: &str = "Maestro/0.1 (self-hosted music server; https://github.com/d-safro/maestro)";
static RUNNING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

// ───────────── pure helpers ─────────────

pub fn norm(name: &str) -> String {
    let ascii: String = name.nfkd().filter(|c| c.is_ascii()).collect::<String>().to_lowercase();
    let s = ascii.strip_prefix("the ").unwrap_or(&ascii);
    let out: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if out.is_empty() { name.trim().to_lowercase() } else { out }
}

/// The town you typed appears in the show's city, region or country (the same rule as Encore).
fn near(show: &Value, town: &str) -> bool {
    let w = town.trim().to_lowercase();
    w.is_empty() || ["city", "region", "country"].iter().any(|k| show[*k].as_str().unwrap_or("").to_lowercase().contains(&w))
}

fn show_key(s: &Value) -> String {
    format!(
        "{}|{}|{}",
        norm(s["artist"].as_str().unwrap_or("")),
        s["date"].as_str().unwrap_or(""),
        norm(s["venue"].as_str().unwrap_or(""))
    )
}

fn s<'a>(v: &'a Value, path: &[&str]) -> &'a str {
    let mut v = v;
    for p in path {
        v = &v[*p];
    }
    v.as_str().unwrap_or("")
}

pub fn parse_ticketmaster(j: &Value, artist: &str, today: &str) -> Vec<Value> {
    let n = norm(artist);
    let mut out = vec![];
    for e in j["_embedded"]["events"].as_array().into_iter().flatten() {
        let acts = e["_embedded"]["attractions"].as_array().cloned().unwrap_or_default();
        if !acts.iter().any(|a| norm(a["name"].as_str().unwrap_or("")) == n) {
            continue; // tribute bands and namesakes
        }
        let date = s(e, &["dates", "start", "localDate"]);
        if date.is_empty() || date < today {
            continue;
        }
        let v = &e["_embedded"]["venues"][0];
        let time = s(e, &["dates", "start", "localTime"]);
        out.push(json!({
            "artist": artist, "date": date, "time": time.strip_suffix(":00").unwrap_or(time), "name": s(e, &["name"]),
            "venue": s(v, &["name"]), "city": s(v, &["city", "name"]), "region": s(v, &["state", "name"]),
            "country": s(v, &["country", "name"]), "url": s(e, &["url"]), "source": "Ticketmaster",
        }));
    }
    out
}

pub fn parse_mb_events(j: &Value, artist: &str, today: &str) -> Vec<Value> {
    let mut out = vec![];
    for e in j["events"].as_array().into_iter().flatten() {
        if e["cancelled"].as_bool() == Some(true) {
            continue;
        }
        let date = s(e, &["life-span", "begin"]);
        if date.len() < 10 || date < today {
            continue;
        }
        let (mut venue, mut city) = ("", "");
        for r in e["relations"].as_array().into_iter().flatten() {
            if r["place"].is_object() {
                venue = s(r, &["place", "name"]);
            }
            if r["area"].is_object() {
                city = s(r, &["area", "name"]);
            }
        }
        out.push(json!({
            "artist": artist, "date": date, "time": s(e, &["time"]), "name": s(e, &["name"]), "venue": venue, "city": city,
            "region": "", "country": "", "url": format!("https://musicbrainz.org/event/{}", s(e, &["id"])), "source": "MusicBrainz",
        }));
    }
    out
}

/// Found shows, without doubles and by date; `first` (key → first seen, ms) gets the new ones; past ones go.
pub fn merge(mut found: Vec<Value>, first: &mut HashMap<String, i64>, now_ms: i64, today: &str) -> Vec<Value> {
    found.sort_by(|a, b| (s(a, &["date"]), s(a, &["time"])).cmp(&(s(b, &["date"]), s(b, &["time"]))));
    let (mut out, mut seen) = (vec![], HashSet::new());
    for mut x in found {
        let k = show_key(&x);
        if !seen.insert(k.clone()) {
            continue;
        }
        let f = *first.entry(k).or_insert(now_ms);
        x["first"] = f.into();
        out.push(x);
    }
    first.retain(|k, _| seen.contains(k) || k.split('|').nth(1).is_some_and(|d| d >= today));
    out
}

// ───────────── the server's side ─────────────

async fn synced(st: &AppState, uid: &str, key: &str) -> Value {
    let raw: Option<String> = sqlx::query_scalar("SELECT value_json FROM sync_kv WHERE user_id = ? AND key = ?")
        .bind(uid)
        .bind(key)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    let v: Value = raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
    match v {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null), // some apps store JSON as text
        v => v,
    }
}

async fn prefs(st: &AppState, uid: &str) -> (String, String) {
    let p = synced(st, uid, "showsPrefs").await;
    (s(&p, &["city"]).trim().to_string(), s(&p, &["tmKey"]).trim().to_string())
}

fn prefs_tag(city: &str, key: &str) -> String {
    // the key itself isn't stored here, only whether it changed
    use sha1::{Digest, Sha1};
    format!("{city}|{:x}", Sha1::digest(key.as_bytes()))
}

/// Who to look for: the ones you follow, then your most played over a year.
async fn artists(st: &AppState, uid: &str) -> Vec<String> {
    let mut names: Vec<String> = synced(st, uid, "followedArtists")
        .await
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["name"].as_str().filter(|n| !n.is_empty()).map(String::from))
        .collect();
    let since = now() - 365 * 86400;
    let top: Vec<(String, i64)> = sqlx::query_as(
        "SELECT CASE WHEN lower(t.artist_name) IN ('', 'various artists') THEN al.artist_name ELSE t.artist_name END AS a,
                COUNT(*) AS n
         FROM play p JOIN track t ON t.id = p.track_id JOIN album al ON al.id = t.album_id
         WHERE p.user_id = ? AND p.played_at >= ? GROUP BY a ORDER BY n DESC LIMIT ?",
    )
    .bind(uid)
    .bind(since)
    .bind(TOP as i64 + 20)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    names.extend(
        top.into_iter()
            .map(|(a, _)| a)
            .filter(|a| !a.is_empty() && !["various artists", "[unknown artist]"].contains(&a.to_lowercase().as_str()))
            .take(TOP),
    );
    let mut seen = HashSet::new();
    names.retain(|n| seen.insert(n.clone()));
    names.truncate(TOP + 50);
    names
}

async fn get_json(http: &reqwest::Client, url: &str, q: &[(&str, &str)]) -> Result<Value, String> {
    let r = http.get(url).query(q).header("accept", "application/json").send().await.map_err(|e| e.to_string())?;
    if !r.status().is_success() {
        return Err(format!("HTTP {}", r.status().as_u16()));
    }
    r.json().await.map_err(|e| e.to_string())
}

async fn tm_shows(st: &AppState, http: &reqwest::Client, key: &str, artist: &str, today: &str) -> Result<Vec<Value>, String> {
    let url = format!("{}/events.json", st.cfg.ticketmaster_url.trim_end_matches('/'));
    let q = [("apikey", key), ("keyword", artist), ("classificationName", "music"), ("sort", "date,asc"), ("size", "100")];
    Ok(parse_ticketmaster(&get_json(http, &url, &q).await?, artist, today))
}

async fn mb_shows(st: &AppState, http: &reqwest::Client, artist: &str, today: &str) -> Result<Vec<Value>, String> {
    let _ = http;
    let query = format!("artist:\"{}\"", artist.replace('"', ""));
    let j = crate::wiki::mb_get(st, "artist", &[("query", &query), ("limit", "5")]).await?.unwrap_or_default();
    let mbid = j["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["score"].as_i64().unwrap_or(0) >= 95 && norm(a["name"].as_str().unwrap_or("")) == norm(artist))
        .and_then(|a| a["id"].as_str())
        .map(String::from);
    let Some(mbid) = mbid else { return Ok(vec![]) };
    let j = crate::wiki::mb_get(st, "event", &[("query", &format!("arid:{mbid}")), ("limit", "100")]).await?.unwrap_or_default();
    Ok(parse_mb_events(&j, artist, today))
}

/// Look for this user's concerts now (one run per user at a time).
pub async fn run(st: Arc<AppState>, uid: String) {
    if !RUNNING.lock().unwrap_or_else(|e| e.into_inner()).insert(uid.clone()) {
        return;
    }
    let (city, key) = prefs(&st, &uid).await;
    let today = Local::now().format("%Y-%m-%d").to_string();
    let http = reqwest::Client::builder().user_agent(UA).timeout(Duration::from_secs(20)).build().expect("http client");
    let (mut found, mut errors) = (vec![], 0);
    for a in artists(&st, &uid).await {
        let shows =
            if key.is_empty() { mb_shows(&st, &http, &a, &today).await } else { tm_shows(&st, &http, &key, &a, &today).await };
        match shows {
            Ok(v) => found.extend(v.into_iter().filter(|s| near(s, &city))),
            Err(e) => {
                errors += 1;
                if errors == 1 || errors == 10 {
                    tracing::warn!(artist = %a, "concerts: {e}");
                }
                if !key.is_empty() && e.contains("401") {
                    break; // a wrong key: stop asking
                }
            }
        }
        if !key.is_empty() {
            tokio::time::sleep(Duration::from_millis(250)).await; // Ticketmaster: 5 requests a second
        }
    }
    let row: Option<String> = sqlx::query_scalar("SELECT first_json FROM concerts WHERE user_id = ?")
        .bind(&uid)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten();
    let mut first: HashMap<String, i64> = row.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
    let shows = merge(found, &mut first, now_ms(), &today);
    let source = if key.is_empty() { "MusicBrainz" } else { "Ticketmaster" };
    tracing::info!(shows = shows.len(), near = %if city.is_empty() { "anywhere" } else { &city }, source, "concerts");
    let _ = sqlx::query(
        "INSERT INTO concerts (user_id, shows_json, first_json, city, source, prefs, at) VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(user_id) DO UPDATE SET shows_json = excluded.shows_json, first_json = excluded.first_json,
           city = excluded.city, source = excluded.source, prefs = excluded.prefs, at = excluded.at",
    )
    .bind(&uid)
    .bind(Value::from(shows).to_string())
    .bind(serde_json::to_string(&first).unwrap_or_default())
    .bind(&city)
    .bind(source)
    .bind(prefs_tag(&city, &key))
    .bind(now())
    .execute(&st.db)
    .await;
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).remove(&uid);
}

async fn due(st: &AppState, uid: &str, at: i64, had: &str) -> bool {
    let (city, key) = prefs(st, uid).await;
    now() - at > 7 * 86400 || had != prefs_tag(&city, &key)
}

/// Every half hour: users whose apps have asked for concerts, when a week has passed or their town changed.
pub async fn worker(st: Arc<AppState>) {
    if !st.cfg.concerts {
        return;
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
    loop {
        let rows: Vec<(String, i64, String)> =
            sqlx::query_as("SELECT user_id, at, prefs FROM concerts").fetch_all(&st.db).await.unwrap_or_default();
        for (uid, at, had) in rows {
            if due(&st, &uid, at, &had).await {
                run(st.clone(), uid).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(1800)).await;
    }
}

pub async fn get_concerts(c: &Ctx, refresh: bool) -> Result<Reply, ApiError> {
    let row: Option<(String, String, String, i64, String)> =
        sqlx::query_as("SELECT shows_json, city, source, at, prefs FROM concerts WHERE user_id = ?")
            .bind(&c.user.id)
            .fetch_optional(&c.st.db)
            .await?;
    let first_time = row.is_none();
    if first_time {
        sqlx::query("INSERT OR IGNORE INTO concerts (user_id) VALUES (?)").bind(&c.user.id).execute(&c.st.db).await?;
    }
    let (shows, city, source, at, had) = row.unwrap_or_else(|| ("[]".into(), String::new(), String::new(), 0, String::new()));
    if c.st.cfg.concerts && (refresh || first_time || due(&c.st, &c.user.id, at, &had).await) {
        tokio::spawn(run(c.st.clone(), c.user.id.clone()));
    }
    let today = Local::now().format("%Y-%m-%d").to_string();
    let shows: Vec<Value> = serde_json::from_str::<Vec<Value>>(&shows)
        .unwrap_or_default()
        .into_iter()
        .filter(|x| s(x, &["date"]) >= today.as_str())
        .collect();
    ok(json!({ "concerts": { "city": city, "source": source, "updated": at * 1000, "show": shows } }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_merging() {
        assert_eq!(norm("The Beatles"), "beatles");
        assert_eq!(norm("Beyoncé"), "beyonce");
        assert_eq!(norm("!!!"), "!!!");
        let today = "2026-06-01";
        let tm = json!({ "_embedded": { "events": [
            { "name": "Tour", "url": "https://t/1", "dates": { "start": { "localDate": "2026-07-01", "localTime": "20:00:00" } },
              "_embedded": { "attractions": [{ "name": "The Beatles" }], "venues": [{ "name": "Hall", "city": { "name": "Boston" },
                "state": { "name": "Massachusetts" }, "country": { "name": "United States Of America" } }] } },
            { "name": "Tribute", "dates": { "start": { "localDate": "2026-07-02" } },
              "_embedded": { "attractions": [{ "name": "Beatles Tribute" }] } },
            { "name": "Old", "dates": { "start": { "localDate": "2026-01-01" } }, "_embedded": { "attractions": [{ "name": "Beatles" }] } }
        ] } });
        let shows = parse_ticketmaster(&tm, "Beatles", today);
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0]["time"], "20:00");
        assert!(near(&shows[0], "massach"));
        assert!(!near(&shows[0], "Paris"));
        let mut first = HashMap::from([("gone|2026-01-01|x".to_string(), 1), ("later|2026-12-01|y".to_string(), 2)]);
        let twice = vec![shows[0].clone(), shows[0].clone()];
        let m = merge(twice, &mut first, 99, today);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0]["first"], 99);
        assert!(!first.contains_key("gone|2026-01-01|x") && first.contains_key("later|2026-12-01|y"));
        let mb = json!({ "events": [{ "id": "e1", "name": "Live", "life-span": { "begin": "2026-08-01" }, "time": "19:30",
            "relations": [{ "place": { "name": "Club" } }, { "area": { "name": "Berlin" } }] }, { "cancelled": true }] });
        let e = parse_mb_events(&mb, "X", today);
        assert_eq!((e.len(), e[0]["venue"].as_str(), e[0]["city"].as_str()), (1, Some("Club"), Some("Berlin")));
    }
}
