//! Songs, albums and artists you don't have yet (from Deezer), shown inside the apps marked ⬇, and the requests
//! for them (heart one or add it to a playlist: it's downloaded, filed, and hearted / added once it's here).
//!
//! Ids: `dz-t-<id>` songs, `dz-a-<id>` albums, `dz-r-<id>` artists (Encore builds these itself too).
//! Marks at the start of titles (for apps that don't read `lookupState`): ⬇ not asked for · ⏳ on its way · ✕ not found.

pub mod acquire;
pub mod deezer;
pub mod lidarr;
pub mod text;
pub mod trial;

use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

use anyhow::Result;
use serde_json::{Value, json};

use crate::{
    api::AppState,
    db::{self, now},
};
use text::{JUNK, ak, alk, edition_tier, mentions, ratio, same_artist, tk};

pub fn dz_id(x: &str, kind: char) -> Option<&str> {
    let (k, n) = x.strip_prefix("dz-")?.split_once('-')?;
    (k.len() == 1 && k.starts_with(kind) && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).then_some(n)
}

pub fn is_dz(x: &str) -> bool {
    x.starts_with("dz-")
}

/// A song in the library, for matching.
#[derive(Clone, Debug)]
pub struct LibSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub album_id: String,
    pub artist_id: String,
    pub album_artist_id: String,
    pub duration: i64,
}

#[derive(Default)]
struct LibIndex {
    built_for: i64,
    by_title: HashMap<String, Vec<LibSong>>,
    albums: HashMap<(String, String), String>,
    artists: HashMap<String, String>,
}

pub struct Catalog {
    pub dz: deezer::Deezer,
    lib: Mutex<LibIndex>,
    /// bumped when a requested song arrives, so the index is rebuilt
    pub generation: std::sync::atomic::AtomicI64,
}

impl Catalog {
    pub fn new(deezer_url: &str, proxy: Option<&str>) -> Self {
        Self { dz: deezer::Deezer::new(deezer_url, proxy), lib: Mutex::default(), generation: Default::default() }
    }
}

async fn refresh_index(st: &AppState) -> Result<()> {
    let generation = st.scan.last_scan.load(std::sync::atomic::Ordering::Relaxed) * 1000
        + st.catalog.generation.load(std::sync::atomic::Ordering::Relaxed);
    if st.catalog.lib.lock().unwrap().built_for == generation && generation != 0 {
        return Ok(());
    }
    type Row = (String, String, String, String, String, String, String, String, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT t.id, t.title, t.artist_name, al.artist_name, t.album_name, t.album_id, t.artist_id, al.artist_id, t.duration_ms / 1000
         FROM track t JOIN album al ON al.id = t.album_id WHERE t.missing_since IS NULL",
    )
    .fetch_all(&st.db)
    .await?;
    let mut idx = LibIndex { built_for: generation, ..Default::default() };
    for (id, title, artist, album_artist, album, album_id, artist_id, album_artist_id, duration) in rows {
        for (name, aid) in [(&album_artist, &album_artist_id), (&artist, &artist_id)] {
            idx.albums.entry((ak(name), alk(&album))).or_insert_with(|| album_id.clone());
            idx.artists.entry(ak(name)).or_insert_with(|| aid.clone());
        }
        idx.by_title.entry(tk(&title)).or_default().push(LibSong {
            id,
            title,
            artist,
            album_artist,
            album,
            album_id,
            artist_id,
            album_artist_id,
            duration,
        });
    }
    *st.catalog.lib.lock().unwrap() = idx;
    Ok(())
}

/// Your copy of this song, if you have one (any album).
pub async fn find(st: &AppState, title: &str, artist: &str, duration: Option<i64>) -> Option<LibSong> {
    refresh_index(st).await.ok()?;
    let idx = st.catalog.lib.lock().unwrap();
    let mut best = None;
    for s in idx.by_title.get(&tk(title))? {
        if [&s.artist, &s.album_artist].iter().any(|n| same_artist(artist, n)) {
            if let Some(d) = duration
                && d > 0
                && (s.duration - d).abs() > 25
            {
                best = best.or(Some(s.clone())); // same name, other length (live / edit): only if nothing closer
                continue;
            }
            return Some(s.clone());
        }
    }
    best
}

pub async fn has_album(st: &AppState, artist: &str, title: &str) -> bool {
    if refresh_index(st).await.is_err() {
        return false;
    }
    st.catalog.lib.lock().unwrap().albums.contains_key(&(ak(artist), alk(title)))
}

pub async fn library_artist_id(st: &AppState, name: &str) -> Option<String> {
    refresh_index(st).await.ok()?;
    st.catalog.lib.lock().unwrap().artists.get(&ak(name)).cloned()
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}
fn year_of(d: &str) -> i64 {
    d.get(..4).and_then(|y| y.parse().ok()).unwrap_or(0)
}

// ── requests: status of a ⬇ song ──

#[derive(Debug, Clone, Default)]
pub struct Status {
    pub status: Option<String>,
    pub local_id: Option<String>,
    pub trial: bool,
}

pub async fn statuses(st: &AppState, keys: &[String]) -> HashMap<String, Status> {
    let mut out = HashMap::new();
    for chunk in keys.chunks(400) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("SELECT key, status, local_id, trial FROM request WHERE key IN ({marks})");
        let mut q = sqlx::query_as::<_, (String, String, Option<String>, bool)>(sqlx::AssertSqlSafe(sql.to_string()));
        for k in chunk {
            q = q.bind(k);
        }
        for (k, status, local_id, trial) in q.fetch_all(&st.db).await.unwrap_or_default() {
            out.insert(k, Status { status: Some(status), local_id, trial });
        }
    }
    out
}

/// The mark in front of a ⬇ song's title, and Encore's lookupState (0 none, 1 on its way, 2 not found).
fn mark(s: Option<&Status>) -> (&'static str, i64) {
    match s.and_then(|s| s.status.as_deref()) {
        None => ("⬇ ", 0),
        Some("notfound") | Some("skipped") => ("✕ ", 2), // skipped: set aside on the admin page
        Some(_) => ("⏳ ", 1),
    }
}

/// A Deezer track as a Subsonic song.
pub fn ss_song(
    t: &Value,
    status: Option<&Status>,
    album: Option<&Value>,
    album_id: Option<&str>,
    artist_id: Option<&str>,
    cover: Option<&str>,
) -> Value {
    let key = format!("dz-t-{}", n(t, "id"));
    let album = album.or(t.get("album")).cloned().unwrap_or(json!({}));
    let art = t.get("artist").cloned().unwrap_or(json!({}));
    let aid = artist_id.map(String::from).unwrap_or_else(|| format!("dz-r-{}", n(&art, "id")));
    let alid = album_id.map(String::from).unwrap_or_else(|| format!("dz-a-{}", n(&album, "id")));
    let cover = cover
        .map(String::from)
        .unwrap_or_else(|| if n(&album, "id") > 0 { format!("dz-a-{}", n(&album, "id")) } else { key.clone() });
    let who = json!([{ "id": aid, "name": s(&art, "name") }]);
    let (m, state) = mark(status);
    json!({
        "id": key, "parent": alid, "isDir": false, "title": format!("{m}{}", s(t, "title")), "album": s(&album, "title"),
        "artist": s(&art, "name"), "track": n(t, "track_position"), "discNumber": n(t, "disk_number").max(1),
        "year": year_of(s(&album, "release_date")), "genre": "", "coverArt": cover, "size": 0, "contentType": "audio/mpeg",
        "suffix": "mp3", "duration": n(t, "duration"), "bitRate": 128, "path": "", "albumId": alid, "artistId": aid,
        "type": "music", "mediaType": "song", "isVideo": false, "created": db::iso(now()),
        "isrc": if s(t, "isrc").is_empty() { json!([]) } else { json!([s(t, "isrc")]) }, "genres": [], "artists": who,
        "displayArtist": s(&art, "name"), "albumArtists": who, "displayAlbumArtist": s(&art, "name"), "sortName": s(t, "title").to_lowercase(),
        "lookupState": state, "comment": "Not downloaded yet. Heart it or add it to a playlist to download it.",
    })
}

pub fn ss_album(a: &Value, artist_id: Option<&str>, songs: Option<Vec<Value>>) -> Value {
    let art = a.get("artist").cloned().unwrap_or(json!({}));
    let aid = artist_id.map(String::from).unwrap_or_else(|| format!("dz-r-{}", n(&art, "id")));
    let id = format!("dz-a-{}", n(a, "id"));
    let count = if n(a, "nb_tracks") > 0 { n(a, "nb_tracks") } else { songs.as_ref().map(|s| s.len() as i64).unwrap_or(0) };
    let duration = if n(a, "duration") > 0 { n(a, "duration") } else { songs.iter().flatten().map(|s| n(s, "duration")).sum() };
    let mut v = json!({
        "id": id, "name": format!("⬇ {}", s(a, "title")), "artist": s(&art, "name"), "artistId": aid, "coverArt": id,
        "songCount": count, "duration": duration, "created": db::iso(now()), "year": year_of(s(a, "release_date")),
        "genre": "", "genres": [], "isCompilation": s(a, "record_type") == "compile",
        "releaseTypes": [if s(a, "record_type").is_empty() { "album" } else { s(a, "record_type") }],
        "artists": [{ "id": aid, "name": s(&art, "name") }], "displayArtist": s(&art, "name"), "sortName": s(a, "title").to_lowercase(),
        "userRating": 0, "title": s(a, "title"), "album": s(a, "title"), "isDir": true, "isVideo": false, "parent": aid,
    });
    if let Some(songs) = songs {
        v["song"] = songs.into();
    }
    v
}

pub fn ss_artist(a: &Value) -> Value {
    let pic = if s(a, "picture_xl").is_empty() { s(a, "picture_big") } else { s(a, "picture_xl") };
    json!({ "id": format!("dz-r-{}", n(a, "id")), "name": format!("⬇ {}", s(a, "name")), "coverArt": format!("dz-r-{}", n(a, "id")),
            "albumCount": n(a, "nb_album"), "artistImageUrl": pic, "sortName": s(a, "name").to_lowercase() })
}

/// What you typed, on Deezer, that you don't have (junk and loose matches dropped).
pub struct Found {
    pub songs: Vec<Value>,
    pub albums: Vec<Value>,
    pub artists: Vec<Value>,
}

pub async fn lookup(
    st: &AppState,
    q: &str,
    counts: (usize, usize, usize),
    have_songs: &[(String, String)],
    have_albums: &[(String, String)],
    have_artists: &[String],
) -> Found {
    let dz = &st.catalog.dz;
    let (sc, alc, arc) = (counts.0.min(25).to_string(), counts.1.min(12).to_string(), counts.2.min(6).to_string());
    let (songs, albums, artists) = tokio::join!(
        async { if counts.0 > 0 { dz.list("search", &[("q", q), ("limit", &sc)]).await } else { vec![] } },
        async { if counts.1 > 0 { dz.list("search/album", &[("q", q), ("limit", &alc)]).await } else { vec![] } },
        async { if counts.2 > 0 { dz.list("search/artist", &[("q", q), ("limit", &arc)]).await } else { vec![] } },
    );
    let junk_ok = JUNK.is_match(q);
    let mut out = Found { songs: vec![], albums: vec![], artists: vec![] };
    let mut seen: HashSet<(String, String)> = have_songs.iter().map(|(a, t)| (ak(a), tk(t))).collect();
    for t in songs {
        let art = s(&t["artist"], "name").to_string();
        let k = (ak(&art), tk(s(&t, "title")));
        if seen.contains(&k)
            || (JUNK.is_match(&format!("{art} {}", s(&t, "title"))) && !junk_ok)
            || !mentions(q, &[&art, s(&t, "title"), s(&t["album"], "title")])
            || find(st, s(&t, "title"), &art, Some(n(&t, "duration"))).await.is_some()
        {
            continue;
        }
        seen.insert(k);
        out.songs.push(t);
    }
    let have: HashSet<String> = have_albums.iter().flat_map(|(a, nm)| [format!("{}|{}", ak(a), alk(nm)), alk(nm)]).collect();
    let mut picked: Vec<(String, Value)> = vec![];
    for a in albums {
        let art = s(&a["artist"], "name").to_string();
        let full = format!("{}|{}", ak(&art), alk(s(&a, "title")));
        if have.contains(&full)
            || (JUNK.is_match(&format!("{art} {}", s(&a, "title"))) && !junk_ok)
            || !mentions(q, &[&art, s(&a, "title")])
            || has_album(st, &art, s(&a, "title")).await
        {
            continue;
        }
        let key = if mentions(q, &[&art]) && !mentions(q, &[s(&a, "title")]) { full } else { alk(s(&a, "title")) };
        if have.contains(&key) {
            continue;
        }
        match picked.iter_mut().find(|(k, _)| *k == key) {
            None => picked.push((key, a)),
            Some((_, cur)) => {
                if same_artist(&art, s(&cur["artist"], "name")) && edition_tier(s(&a, "title")) < edition_tier(s(cur, "title")) {
                    *cur = a;
                }
            }
        }
    }
    out.albums = picked.into_iter().map(|(_, a)| a).collect();
    let mut names: HashSet<String> = have_artists.iter().map(|x| ak(x)).collect();
    let top = artists.iter().map(|a| n(a, "nb_fan")).max().unwrap_or(0);
    for a in artists {
        if (n(&a, "nb_fan") as f64) < top as f64 * 0.02 || JUNK.is_match(s(&a, "name")) {
            continue;
        }
        if names.contains(&ak(s(&a, "name")))
            || !mentions(q, &[s(&a, "name")])
            || library_artist_id(st, s(&a, "name")).await.is_some()
        {
            continue;
        }
        names.insert(ak(s(&a, "name")));
        out.artists.push(a);
    }
    out
}

async fn cache_get(st: &AppState, kind: &str, key: &str, days: i64) -> Option<Option<String>> {
    sqlx::query_as::<_, (Option<String>, i64)>("SELECT value, at FROM catalog_cache WHERE kind = ? AND key = ?")
        .bind(kind)
        .bind(key)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .filter(|(_, at)| now() - at < days * 86_400)
        .map(|(v, _)| v)
}

async fn cache_put(st: &AppState, kind: &str, key: &str, value: Option<&str>) {
    let _ = sqlx::query("INSERT OR REPLACE INTO catalog_cache (kind, key, value, at) VALUES (?, ?, ?, ?)")
        .bind(kind)
        .bind(key)
        .bind(value)
        .bind(now())
        .execute(&st.db)
        .await;
}

pub async fn album_tracks(st: &AppState, album_id: &str) -> Vec<Value> {
    st.catalog.dz.list(&format!("album/{album_id}/tracks"), &[("limit", "500")]).await
}

/// The Deezer album that is this library album (same name, same artist, most of your songs on it).
pub async fn match_album(st: &AppState, artist: &str, name: &str, titles: &[String]) -> Option<String> {
    let key = format!("{}|{}", ak(artist), alk(name));
    if let Some(hit) = cache_get(st, "album", &key, 7).await {
        return hit;
    }
    let mut cands: Vec<Value> = vec![];
    let clean = text::clean_album(name);
    let mine = edition_tier(name);
    for q in [format!(r#"artist:"{artist}" album:"{clean}""#), format!("{artist} {clean}")] {
        let Some(found) = st.catalog.dz.try_list("search/album", &[("q", &q), ("limit", "15")]).await else {
            // Deezer didn't answer: nothing is kept (it's asked again next time, not a week later), unless the
            // first search already found something to go on
            if cands.is_empty() {
                return None;
            }
            break;
        };
        for a in found {
            if same_artist(artist, s(&a["artist"], "name")) && !cands.iter().any(|c| n(c, "id") == n(&a, "id")) {
                cands.push(a);
            }
        }
        // the exact search can find only a bigger edition ("Sticky Fingers (Super Deluxe)") while the album itself
        // ("Sticky Fingers (Remastered)") comes up only in the plain one: look there too unless it's here already
        if cands.iter().any(|c| edition_tier(s(c, "title")) <= mine.max(1)) {
            break;
        }
    }
    let want = alk(name);
    let mut scored: Vec<(f64, Value)> = vec![];
    for a in cands {
        let sim = ratio(&want, &alk(s(&a, "title")));
        if sim < 0.85 {
            continue;
        }
        let mut sc = sim * 2.0 + if s(&a, "title").trim().eq_ignore_ascii_case(name.trim()) { 1.0 } else { 0.0 };
        sc += if s(&a, "record_type") == "album" { 0.5 } else { 0.0 };
        // the plain album first, then a remaster of it (the same songs), and only then a deluxe edition (unless
        // yours is one): Deezer often has only "(Remastered)" and "(Super Deluxe)", and lists the deluxe one first
        match edition_tier(s(&a, "title")) {
            t if t <= mine => {}
            1 => sc -= 0.5,
            _ => sc -= 1.5,
        }
        scored.push((sc, a));
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let have: HashSet<String> = titles.iter().map(|t| tk(t)).collect();
    let mut best: Option<(f64, String)> = None;
    for (sc, a) in scored.into_iter().take(4) {
        let tracks = album_tracks(st, &n(&a, "id").to_string()).await;
        let cover = tracks.iter().filter(|t| have.contains(&tk(s(t, "title")))).count() as f64 / have.len().max(1) as f64;
        // (all else the same: the one with fewer songs, nearer the album as it came out)
        let total = sc + cover * 3.0 - tracks.len() as f64 * 0.001;
        if best.as_ref().is_none_or(|(b, _)| total > *b) {
            best = Some((total, n(&a, "id").to_string()));
        }
    }
    let best = best.map(|(_, id)| id);
    cache_put(st, "album", &key, best.as_deref()).await;
    best
}

pub async fn artist_dz_id(st: &AppState, name: &str) -> Option<String> {
    let key = ak(name);
    if let Some(hit) = cache_get(st, "artist", &key, 30).await {
        return hit;
    }
    let mut best: Option<Value> = None;
    // Deezer didn't answer: nothing is kept (it's asked again next time, not a month later)
    for a in st.catalog.dz.try_list("search/artist", &[("q", name), ("limit", "10")]).await? {
        if ak(s(&a, "name")) == key && best.as_ref().is_none_or(|b| n(&a, "nb_fan") > n(b, "nb_fan")) {
            best = Some(a);
        }
    }
    let id = best.map(|b| n(&b, "id").to_string());
    cache_put(st, "artist", &key, id.as_deref()).await;
    id
}

/// The artist's albums on Deezer that you have none of (one per title, the plain edition first).
pub async fn missing_albums(st: &AppState, artist_name: &str, have_names: &[String]) -> Vec<Value> {
    let Some(did) = artist_dz_id(st, artist_name).await else { return vec![] };
    missing_albums_of(st, &did, have_names).await
}

pub async fn missing_albums_of(st: &AppState, did: &str, have_names: &[String]) -> Vec<Value> {
    let have: HashSet<String> = have_names.iter().map(|x| alk(x)).collect();
    let mut groups: Vec<(String, Vec<Value>)> = vec![];
    for a in st.catalog.dz.list(&format!("artist/{did}/albums"), &[("limit", "200")]).await {
        if ["album", "ep"].contains(&s(&a, "record_type")) && !JUNK.is_match(s(&a, "title")) {
            let k = alk(s(&a, "title"));
            match groups.iter_mut().find(|(g, _)| *g == k) {
                Some((_, g)) => g.push(a),
                None => groups.push((k, vec![a])),
            }
        }
    }
    let fans = |g: &Vec<Value>| g.iter().map(|a| n(a, "fans")).sum::<i64>();
    let top = groups.iter().map(|(_, g)| fans(g)).max().unwrap_or(0);
    let mut out: Vec<Value> = vec![];
    for (k, g) in &groups {
        if have.contains(k) || (fans(g) as f64) < top as f64 * 0.015 {
            continue; // radio broadcasts, obscure reissues
        }
        // the plain edition, else a remaster, else any
        let low = g.iter().map(|a| edition_tier(s(a, "title"))).min().unwrap_or(0);
        if let Some(best) = g.iter().filter(|a| edition_tier(s(a, "title")) == low).max_by_key(|a| n(a, "fans")) {
            out.push(best.clone());
        }
    }
    out.sort_by(|a, b| s(b, "release_date").cmp(s(a, "release_date")));
    out
}

/// [(Deezer track, your song or None)] in album order, then your songs that aren't on it.
pub async fn full_album(
    st: &AppState,
    dz_album: &str,
    local: &[(String, String)],
) -> (Vec<(Value, Option<String>)>, Vec<String>) {
    let mut used: HashSet<String> = HashSet::new();
    let mut plan = vec![];
    for t in album_tracks(st, dz_album).await {
        let mut mine =
            local.iter().find(|(id, title)| !used.contains(id) && tk(title) == tk(s(&t, "title"))).map(|(id, _)| id.clone());
        if mine.is_none() {
            mine = find(st, s(&t, "title"), s(&t["artist"], "name"), Some(n(&t, "duration")))
                .await
                .map(|x| x.id)
                .filter(|id| !used.contains(id));
        }
        if let Some(m) = &mine {
            used.insert(m.clone());
        }
        plan.push((t, mine));
    }
    let rest = local.iter().filter(|(id, _)| !used.contains(id)).map(|(id, _)| id.clone()).collect();
    (plan, rest)
}

pub fn not_in_library_key(t: &Value) -> (String, String) {
    (tk(s(t, "title")), ak(s(&t["artist"], "name")))
}

/// Deezer tracks you don't have (and haven't seen yet in this list).
pub async fn not_in_library(st: &AppState, tracks: Vec<Value>, seen: &mut HashSet<(String, String)>) -> Vec<Value> {
    let mut out = vec![];
    for t in tracks {
        let k = not_in_library_key(&t);
        if n(&t, "id") == 0
            || seen.contains(&k)
            || find(st, s(&t, "title"), s(&t["artist"], "name"), Some(n(&t, "duration"))).await.is_some()
        {
            continue;
        }
        seen.insert(k);
        out.push(t);
    }
    out
}

pub async fn dz_artist(st: &AppState, name: &str) -> Option<Value> {
    let hits = st.catalog.dz.list("search/artist", &[("q", name), ("limit", "1")]).await;
    hits.into_iter().next().filter(|a| same_artist(name, s(a, "name")))
}

/// This artist on Deezer and Deezer's related artists ({artist, related}), kept for a week.
pub async fn artist_info(st: &AppState, name: &str) -> Option<Value> {
    let key = ak(name);
    if let Some(v) = cache_get(st, "artist_info", &key, 7).await {
        return v.and_then(|v| serde_json::from_str(&v).ok());
    }
    // Deezer didn't answer: nothing is kept (it's asked again next time)
    let hits = st.catalog.dz.try_list("search/artist", &[("q", name), ("limit", "1")]).await?;
    let found = match hits.into_iter().next().filter(|a| same_artist(name, s(a, "name"))) {
        Some(a) => {
            let related = st.catalog.dz.try_list(&format!("artist/{}/related", n(&a, "id")), &[("limit", "20")]).await?;
            Some(json!({ "artist": a, "related": related }))
        }
        None => None,
    };
    cache_put(st, "artist_info", &key, found.as_ref().map(|v| v.to_string()).as_deref()).await;
    found
}

/// "Because you like X": top songs of Deezer's related artists for the artists you play and heart most.
pub async fn discoveries(st: &AppState, user_id: &str) -> Vec<(String, String, Vec<Value>)> {
    let rows: Vec<(String, i64)> = sqlx::query_as(
        "SELECT al.artist_name, SUM(COALESCE(an.play_count, 0) + CASE WHEN an.starred_at IS NOT NULL THEN 5 ELSE 0 END) AS score
         FROM track t JOIN album al ON al.id = t.album_id
         LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'track' AND an.item_id = t.id
         WHERE t.missing_since IS NULL AND lower(al.artist_name) NOT IN ('various artists', '[unknown artist]')
         GROUP BY al.artist_name HAVING score > 0 ORDER BY score DESC LIMIT 12",
    )
    .bind(user_id)
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    let mut sections = vec![];
    let mut seen = HashSet::new();
    for (name, _) in rows {
        if sections.len() >= 6 {
            break;
        }
        let Some(a) = dz_artist(st, &name).await else { continue };
        let mut songs = vec![];
        for rel in st.catalog.dz.list(&format!("artist/{}/related", n(&a, "id")), &[("limit", "8")]).await.into_iter().take(6) {
            let top = st.catalog.dz.list(&format!("artist/{}/top", n(&rel, "id")), &[("limit", "6")]).await;
            songs.extend(not_in_library(st, top, &mut seen).await.into_iter().take(3));
            if songs.len() >= 15 {
                break;
            }
        }
        if !songs.is_empty() {
            sections.push((format!("Because you like {name}"), name, songs));
        }
    }
    sections
}

/// Songs related to one of your songs (Deezer's radio for its artist), only ones you don't have.
pub async fn song_discoveries(st: &AppState, title: &str, artist: &str, count: usize) -> Vec<Value> {
    let q = format!(r#"artist:"{artist}" track:"{title}""#);
    let hits = st.catalog.dz.list("search/track", &[("q", &q), ("limit", "1")]).await;
    let aid = match hits.first() {
        Some(h) => n(&h["artist"], "id"),
        None => dz_artist(st, artist).await.map(|a| n(&a, "id")).unwrap_or(0),
    };
    if aid == 0 {
        return vec![];
    }
    let radio = st.catalog.dz.list(&format!("artist/{aid}/radio"), &[("limit", "60")]).await;
    let mut seen = HashSet::from([(tk(title), ak(artist))]);
    not_in_library(st, radio, &mut seen).await.into_iter().take(count).collect()
}

// ── asking for songs ──

/// Songs that went on trial in the last 24 hours (at most `trial_per_day`).
pub async fn trials_today(st: &AppState) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE trial_at > ? AND mix IS NULL")
        .bind(now() - 86_400)
        .fetch_one(&st.db)
        .await
        .unwrap_or(0)
}

/// Add a request for this Deezer track (false when Deezer doesn't know it).
async fn new_request(st: &AppState, key: &str, user_id: &str, trial: bool, folder: Option<&str>) -> Result<bool> {
    let Some(tid) = dz_id(key, 't') else { return Ok(false) };
    let t = st.catalog.dz.get(&format!("track/{tid}"), &[]).await;
    if s(&t, "title").is_empty() {
        return Ok(false);
    }
    let main = s(&t["artist"], "name").to_string();
    let mut names = vec![main.clone()];
    for c in t["contributors"].as_array().into_iter().flatten() {
        let nm = s(c, "name");
        if !nm.is_empty() && nm != main && names.len() < 4 {
            names.push(nm.to_string());
        }
    }
    sqlx::query(
        "INSERT INTO request (key, title, artists_json, album, duration_ms, isrc, folder, trial, trial_at, trial_user, asked_at)
         VALUES (?,?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(key)
    .bind(s(&t, "title"))
    .bind(serde_json::to_string(&names)?)
    .bind(s(&t["album"], "title"))
    .bind(n(&t, "duration") * 1000)
    .bind(Some(s(&t, "isrc")).filter(|x| !x.is_empty()))
    .bind(folder.unwrap_or(if trial { "Trial" } else { "Songs" }))
    .bind(trial)
    .bind(if trial { Some(now()) } else { None })
    .bind(if trial { Some(user_id) } else { None })
    .bind(now())
    .execute(&st.db)
    .await?;
    tracing::info!(song = %format!("{main} - {}", s(&t, "title")), trial, "asked for");
    Ok(true)
}

/// Ask for this Deezer track. False when Deezer doesn't know it (or, on trial, today's trials are used up).
/// trial: you played / queued it; it goes to the Trial folder and is deleted unless you keep it.
pub async fn ask(st: &AppState, key: &str, user_id: &str, star: bool, playlist: Option<&str>, trial: bool) -> Result<bool> {
    ask_into(st, key, user_id, star, playlist, trial, By::Someone).await
}

/// `ask` for a sync that runs by itself (Spotify, ListenBrainz). A new request is filed into `folder` (Discovery/)
/// instead of Songs/, set before the download worker can see it, so it can't be filed anywhere else; one asked for
/// before stays where it is filed (someone else may want it). A song Soulseek didn't have is looked for again only
/// after a week (each sync asks again).
pub async fn ask_synced(
    st: &AppState,
    key: &str,
    user_id: &str,
    star: bool,
    playlist: Option<&str>,
    folder: Option<&str>,
) -> Result<bool> {
    ask_into(st, key, user_id, star, playlist, false, By::Sync(folder)).await
}

/// Who asks: someone now (a heart, a play, an import), or a sync (with the folder for what it brings).
#[derive(Clone, Copy)]
enum By<'a> {
    Someone,
    Sync(Option<&'a str>),
}

async fn ask_into(
    st: &AppState,
    key: &str,
    user_id: &str,
    star: bool,
    playlist: Option<&str>,
    trial: bool,
    by: By<'_>,
) -> Result<bool> {
    let (folder, again_after) = match by {
        By::Someone => (None, 3600),
        By::Sync(f) => (f, 7 * 86_400),
    };
    let known: Option<String> =
        sqlx::query_scalar("SELECT status FROM request WHERE key = ?").bind(key).fetch_optional(&st.db).await?;
    if trial && known.is_some() && known.as_deref() != Some("notfound") {
        return Ok(true); // already asked for (on trial, wanted or kept)
    }
    if trial && known.is_none() && trials_today(st).await >= st.cfg.trial_per_day {
        return Ok(false); // enough trials today; it keeps playing the preview
    }
    if known.is_none() && !new_request(st, key, user_id, trial, folder).await? {
        return Ok(false);
    } else if known.as_deref() == Some("notfound") {
        // asked again later: look once more
        sqlx::query("UPDATE request SET status = 'retry', searches = 0, last_search = 0 WHERE key = ? AND last_search < ?")
            .bind(key)
            .bind(now() - again_after)
            .execute(&st.db)
            .await?;
    }
    if (star || playlist.is_some()) && !trial {
        // kept before or after it arrived: not arrived yet, it's filed in Songs; arrived, the trial check moves it there
        sqlx::query("UPDATE request SET keep = 1 WHERE key = ? AND trial = 1 AND rel IS NOT NULL")
            .bind(key)
            .execute(&st.db)
            .await?;
        if folder.is_none() {
            // (a new one for a folder was filed there already)
            sqlx::query("UPDATE request SET trial = 0, folder = 'Songs' WHERE key = ? AND rel IS NULL")
                .bind(key)
                .execute(&st.db)
                .await?;
        }
        sqlx::query("INSERT OR IGNORE INTO request_want (key, user_id, star, playlist_id) VALUES (?, ?, ?, ?)")
            .bind(key)
            .bind(user_id)
            .bind(star)
            .bind(playlist.unwrap_or(""))
            .execute(&st.db)
            .await?;
        sqlx::query("UPDATE request_want SET done = 0 WHERE key = ? AND user_id = ? AND playlist_id = ?")
            .bind(key)
            .bind(user_id)
            .bind(playlist.unwrap_or(""))
            .execute(&st.db)
            .await?;
    }
    st.acquire_wake.notify_one();
    if let Some(local) =
        sqlx::query_scalar::<_, Option<String>>("SELECT local_id FROM request WHERE key = ?").bind(key).fetch_one(&st.db).await?
    {
        acquire::apply_wants(st, key, &local).await?;
    }
    Ok(true)
}

/// Unheart a ⬇ song. One you played (on trial) and hearted, but that isn't kept yet, goes back on trial (deleted
/// after `trial_hours` like the rest); one you only hearted is cancelled if nobody else wants it.
/// Returns your copy's id, if it has arrived.
pub async fn unheart(st: &AppState, key: &str, user_id: &str) -> Result<Option<String>> {
    sqlx::query("DELETE FROM request_want WHERE key = ? AND user_id = ? AND playlist_id = '' AND star = 1")
        .bind(key)
        .bind(user_id)
        .execute(&st.db)
        .await?;
    type Row = (String, Option<String>, Option<i64>, Option<i64>, Option<String>);
    let row: Option<Row> = sqlx::query_as("SELECT status, local_id, trial_at, kept_at, rel FROM request WHERE key = ?")
        .bind(key)
        .fetch_optional(&st.db)
        .await?;
    let Some((status, local, trial_at, kept_at, rel)) = row else { return Ok(None) };
    let in_trial = rel.as_deref().is_none_or(|r| r.starts_with("Trial/"));
    if trial_at.is_some() && kept_at.is_none() && in_trial {
        // played before: back on trial (if it hasn't arrived yet, it lands in Trial again)
        sqlx::query("UPDATE request SET trial = 1, keep = 0, folder = 'Trial' WHERE key = ?").bind(key).execute(&st.db).await?;
        return Ok(local);
    }
    let wanted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_want WHERE key = ?").bind(key).fetch_one(&st.db).await?;
    if local.is_none() && wanted == 0 && (status == "retry" || status == "notfound") {
        sqlx::query("DELETE FROM request WHERE key = ?").bind(key).execute(&st.db).await?;
    }
    Ok(local)
}

/// Ask for this Deezer track for one of a user's mixes: on trial for `hours` (not listed in "On trial", not
/// counted in the day's trials). Kept like any trial song: heart it or add it to a playlist of yours.
pub async fn ask_for_mix(st: &AppState, key: &str, user_id: &str, mix: &str, hours: f64) -> Result<bool> {
    let known: Option<String> =
        sqlx::query_scalar("SELECT status FROM request WHERE key = ?").bind(key).fetch_optional(&st.db).await?;
    match known.as_deref() {
        None => {
            if !new_request(st, key, user_id, true, None).await? {
                return Ok(false);
            }
            sqlx::query("UPDATE request SET mix = ?, trial_hours = ? WHERE key = ?")
                .bind(mix)
                .bind(hours)
                .bind(key)
                .execute(&st.db)
                .await?;
        }
        Some("notfound") => {
            sqlx::query("UPDATE request SET status = 'retry', searches = 0, last_search = 0 WHERE key = ? AND last_search < ?")
                .bind(key)
                .bind(now() - 3600)
                .execute(&st.db)
                .await?;
        }
        Some(_) => {}
    }
    st.acquire_wake.notify_one();
    Ok(true)
}

/// Let a mix's song go: not arrived, it's no longer asked for; arrived and not kept, it's deleted.
/// Songs someone kept or wants (hearted, in a playlist) aren't touched.
pub async fn let_go_of_mix_song(st: &AppState, key: &str, mix: &str) -> Result<()> {
    type Row = (Option<String>, Option<String>, bool, Option<i64>, bool);
    let row: Option<Row> = sqlx::query_as("SELECT mix, local_id, trial, kept_at, keep FROM request WHERE key = ?")
        .bind(key)
        .fetch_optional(&st.db)
        .await?;
    let Some((m, local, trial, kept_at, keep)) = row else { return Ok(()) };
    let wanted: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request_want WHERE key = ?").bind(key).fetch_one(&st.db).await?;
    if m.as_deref() != Some(mix) || !trial || kept_at.is_some() || keep || wanted > 0 {
        return Ok(());
    }
    if local.is_some() {
        // the trial check decides (it keeps a song liked in the meantime): make it due now
        sqlx::query("UPDATE request SET trial_hours = 0 WHERE key = ?").bind(key).execute(&st.db).await?;
    } else {
        sqlx::query("DELETE FROM request WHERE key = ? AND local_id IS NULL").bind(key).execute(&st.db).await?;
    }
    Ok(())
}

/// The library album with this name by this artist, if you have it.
pub async fn library_album(st: &AppState, artist: &str, title: &str) -> Option<String> {
    refresh_index(st).await.ok()?;
    st.catalog.lib.lock().unwrap().albums.get(&(ak(artist), alk(title))).cloned()
}

/// Songs asked for to go into this playlist that haven't arrived yet.
pub async fn pending_for_playlist(st: &AppState, pid: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT w.key FROM request_want w JOIN request r ON r.key = w.key WHERE w.playlist_id = ? AND r.local_id IS NULL ORDER BY r.asked_at")
        .bind(pid)
        .fetch_all(&st.db)
        .await
        .unwrap_or_default()
}
