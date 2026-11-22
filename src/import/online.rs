//! Sources read from a service's API, and lookups an import uses:
//! - Spotify (the account Spotify sync is signed in to): liked songs with when, saved albums, followed artists,
//!   your playlists, the last 50 plays, saved shows. Its ids → ISRCs for songs the names didn't match.
//! - Last.fm by username (needs `lastfm_api_key`): every scrobble with its time, and loved songs.
//! - Deezer by user id (a public profile): favourite songs, albums, artists, playlists and podcasts.
//! - iTunes search: the RSS feed of a podcast found by name. Deezer: the ⬇ key of a song you don't have.

use std::{collections::HashMap, time::Duration};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{Album, Parsed, Play, Playlist, Podcast, Song, parse_time};
use crate::{
    api::AppState,
    catalog::text::{norm, same_artist, tk},
};

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).user_agent(crate::wiki::UA).build().expect("http client")
}

// ───────────── lookups ─────────────

/// A podcast's RSS feed, found by its name (and publisher) in the iTunes directory.
pub async fn podcast_feed(itunes_url: &str, title: &str, publisher: &str) -> Option<String> {
    let url = format!("{}/search", itunes_url.trim_end_matches('/'));
    let r: Value = http()
        .get(url)
        .query(&[("media", "podcast"), ("entity", "podcast"), ("term", title), ("limit", "10")])
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let results = r["results"].as_array()?;
    let named: Vec<&Value> = results.iter().filter(|p| norm(p["collectionName"].as_str().unwrap_or("")) == norm(title)).collect();
    let best = named
        .iter()
        .find(|p| publisher.is_empty() || same_artist(publisher, p["artistName"].as_str().unwrap_or("")))
        .or(named.first())?;
    best["feedUrl"].as_str().filter(|f| f.starts_with("http")).map(String::from)
}

/// The Deezer song for an imported one (its id, its ISRC, else its names), as a ⬇ key.
pub async fn deezer_key(st: &AppState, s: &Song) -> Option<String> {
    if let Some(d) = s.deezer {
        return Some(format!("dz-t-{d}"));
    }
    if let Some(isrc) = &s.isrc {
        let t = st.catalog.dz.get(&format!("track/isrc:{isrc}"), &[]).await;
        if t["id"].as_i64().is_some_and(|i| i > 0) {
            return Some(format!("dz-t-{}", t["id"]));
        }
    }
    if s.title.is_empty() || s.artist.is_empty() {
        return None;
    }
    let q = format!(r#"artist:"{}" track:"{}""#, s.artist.replace('"', ""), crate::catalog::text::clean_title(&s.title).replace('"', ""));
    let hits = st.catalog.dz.list("search/track", &[("q", &q), ("limit", "5")]).await;
    hits.into_iter()
        .find(|t| same_artist(&s.artist, t["artist"]["name"].as_str().unwrap_or("")) && tk(t["title"].as_str().unwrap_or("")) == tk(&s.title))
        .map(|t| format!("dz-t-{}", t["id"]))
}

/// ISRCs of Spotify songs, from Spotify (when Spotify sync is signed in): Song::key → ISRC.
pub async fn spotify_isrcs(st: &AppState, songs: &[&Song]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(Some(c)) = crate::spotify::client(st).await else { return out };
    let ids: Vec<(&str, String)> = songs.iter().filter_map(|s| s.spotify.as_deref().map(|id| (id, s.key()))).take(20_000).collect();
    for chunk in ids.chunks(50) {
        let list: Vec<&str> = chunk.iter().map(|(id, _)| *id).collect();
        let Ok(r) = c.get(&format!("/tracks?ids={}", list.join(","))).await else { break };
        for (t, (_, key)) in r["tracks"].as_array().into_iter().flatten().zip(chunk) {
            if let Some(isrc) = t["external_ids"]["isrc"].as_str() {
                out.insert(key.clone(), isrc.to_uppercase());
            }
        }
    }
    out
}

// ───────────── Spotify ─────────────

fn sp_song(t: &Value) -> Option<Song> {
    if t.is_null() || t["is_local"] == true || t["type"].as_str().is_some_and(|x| x != "track") {
        return None;
    }
    let mut s = Song::named(
        t["name"].as_str().unwrap_or(""),
        t["artists"][0]["name"].as_str().unwrap_or(""),
        t["album"]["name"].as_str().unwrap_or(""),
    );
    s.spotify = t["id"].as_str().map(String::from);
    s.isrc = t["external_ids"]["isrc"].as_str().map(str::to_uppercase);
    s.duration_ms = t["duration_ms"].as_i64().unwrap_or(0);
    Some(s)
}

pub async fn spotify(st: &AppState) -> Result<Parsed> {
    let Some(c) = crate::spotify::client(st).await? else {
        bail!("Spotify isn't signed in here (Spotify sync's login comes over with import-music-lookup)");
    };
    let mut p = Parsed::default();
    p.sources.insert("Spotify (your account)".into());
    for i in c.pages("/me/tracks?limit=50").await? {
        if let Some(s) = sp_song(&i["track"]) {
            p.liked.push((s, i["added_at"].as_str().and_then(parse_time)));
        }
    }
    for i in c.pages("/me/albums?limit=50").await? {
        let a = &i["album"];
        p.albums.push(Album { title: a["name"].as_str().unwrap_or("").into(), artist: a["artists"][0]["name"].as_str().unwrap_or("").into() });
    }
    // followed artists page by a cursor
    let mut next = Some("/me/following?type=artist&limit=50".to_string());
    while let Some(u) = next {
        let r = c.get(&u).await?;
        p.artists.extend(r["artists"]["items"].as_array().into_iter().flatten().filter_map(|a| a["name"].as_str().map(String::from)));
        next = r["artists"]["next"].as_str().map(String::from);
    }
    for pl in c.pages("/me/playlists?limit=50").await? {
        let id = pl["id"].as_str().unwrap_or("");
        let items = match c.pages(&format!("/playlists/{id}/items?limit=100")).await {
            Ok(i) => i,
            Err(_) => c.pages(&format!("/playlists/{id}/tracks?limit=100")).await.unwrap_or_default(),
        };
        let songs = items.iter().filter_map(|i| sp_song(if i["item"].is_object() { &i["item"] } else { &i["track"] })).collect();
        p.playlists.push(Playlist {
            name: pl["name"].as_str().unwrap_or("").into(),
            description: pl["description"].as_str().unwrap_or("").into(),
            songs,
        });
    }
    if let Ok(r) = c.get("/me/player/recently-played?limit=50").await {
        for i in r["items"].as_array().into_iter().flatten() {
            if let (Some(s), Some(at)) = (sp_song(&i["track"]), i["played_at"].as_str().and_then(parse_time)) {
                let ms = s.duration_ms;
                p.plays.push(Play { song: s, at: at - ms / 1000, ms_played: None });
            }
        }
    }
    for i in c.pages("/me/shows?limit=50").await.unwrap_or_default() {
        let s = &i["show"];
        p.podcasts.push(Podcast { title: s["name"].as_str().unwrap_or("").into(), publisher: s["publisher"].as_str().unwrap_or("").into(), feed: None });
    }
    p.tidy();
    Ok(p)
}

// ───────────── Last.fm ─────────────

async fn lastfm_page(st: &AppState, key: &str, method: &str, user: &str, page: u32) -> Result<Value> {
    for attempt in 0..4 {
        let r = http()
            .get(&st.cfg.lastfm_url)
            .query(&[("method", method), ("user", user), ("api_key", key), ("format", "json"), ("limit", "200"), ("page", &page.to_string())])
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => {
                let v: Value = r.json().await?;
                if let Some(e) = v["message"].as_str().filter(|_| v.get("error").is_some()) {
                    bail!("Last.fm: {e}");
                }
                return Ok(v);
            }
            Ok(r) if r.status().as_u16() == 404 => bail!("Last.fm doesn't know the user {user}"),
            _ if attempt < 3 => tokio::time::sleep(Duration::from_secs(2 << attempt)).await,
            Ok(r) => bail!("Last.fm answered {}", r.status()),
            Err(e) => return Err(e).context("Last.fm"),
        }
    }
    unreachable!()
}

fn lfm_song(t: &Value) -> Song {
    let artist = t["artist"]["#text"].as_str().or(t["artist"]["name"].as_str()).unwrap_or("");
    let mut s = Song::named(t["name"].as_str().unwrap_or(""), artist, t["album"]["#text"].as_str().unwrap_or(""));
    s.mbid = t["mbid"].as_str().filter(|m| !m.is_empty()).map(String::from);
    s
}

/// Every scrobble and loved song of a Last.fm user (public data; `lastfm_api_key` needed).
pub async fn lastfm(st: &AppState, user: &str) -> Result<Parsed> {
    let Some(key) = st.cfg.lastfm_api_key.clone() else {
        bail!("Last.fm needs an API key: set lastfm_api_key (free at last.fm/api/account/create)");
    };
    let mut p = Parsed::default();
    p.sources.insert("Last.fm".into());
    for (method, list) in [("user.getlovedtracks", "lovedtracks"), ("user.getrecenttracks", "recenttracks")] {
        let mut page = 1;
        loop {
            let v = lastfm_page(st, &key, method, user, page).await?;
            let block = &v[list];
            for t in block["track"].as_array().into_iter().flatten() {
                if t["@attr"]["nowplaying"] == "true" {
                    continue;
                }
                let Some(at) = t["date"]["uts"].as_str().and_then(|u| u.parse::<i64>().ok()) else { continue };
                if list == "lovedtracks" {
                    p.liked.push((lfm_song(t), Some(at)));
                } else {
                    p.plays.push(Play { song: lfm_song(t), at, ms_played: None });
                }
            }
            let pages = block["@attr"]["totalPages"].as_str().and_then(|n| n.parse::<u32>().ok()).unwrap_or(1);
            if page >= pages || page >= 2000 {
                break;
            }
            page += 1;
            tokio::time::sleep(Duration::from_millis(250)).await; // Last.fm asks for at most 5 a second
        }
    }
    p.tidy();
    Ok(p)
}

// ───────────── Deezer ─────────────

fn dz_song(t: &Value) -> Song {
    let mut s = Song::named(t["title"].as_str().unwrap_or(""), t["artist"]["name"].as_str().unwrap_or(""), t["album"]["title"].as_str().unwrap_or(""));
    s.deezer = t["id"].as_i64();
    s.duration_ms = t["duration"].as_i64().unwrap_or(0) * 1000;
    s.isrc = t["isrc"].as_str().map(str::to_uppercase);
    s
}

async fn dz_all(st: &AppState, path: &str) -> Vec<Value> {
    let mut out = vec![];
    for index in (0..20_000).step_by(100) {
        let page = st.catalog.dz.get(path, &[("index", &index.to_string()), ("limit", "100")]).await;
        let data = page["data"].as_array().cloned().unwrap_or_default();
        let n = data.len();
        out.extend(data);
        if n < 100 || page["next"].is_null() {
            break;
        }
    }
    out
}

/// A Deezer user's favourites and playlists (their profile must be public). Deezer gives no play history.
pub async fn deezer(st: &AppState, user: &str) -> Result<Parsed> {
    let id: String = user.chars().filter(char::is_ascii_digit).collect();
    if id.is_empty() {
        bail!("give the Deezer user's number (deezer.com/profile/<number>)");
    }
    let who = st.catalog.dz.get(&format!("user/{id}"), &[]).await;
    if who["id"].is_null() {
        bail!("Deezer doesn't show that user (is the profile public?)");
    }
    let mut p = Parsed::default();
    p.sources.insert("Deezer".into());
    for t in dz_all(st, &format!("user/{id}/tracks")).await {
        let at = t["time_add"].as_i64();
        p.liked.push((dz_song(&t), at));
    }
    for a in dz_all(st, &format!("user/{id}/albums")).await {
        p.albums.push(Album { title: a["title"].as_str().unwrap_or("").into(), artist: a["artist"]["name"].as_str().unwrap_or("").into() });
    }
    p.artists.extend(dz_all(st, &format!("user/{id}/artists")).await.iter().filter_map(|a| a["name"].as_str().map(String::from)));
    for pl in dz_all(st, &format!("user/{id}/playlists")).await {
        if pl["is_loved_track"] == true {
            continue; // the same as the favourite songs
        }
        let songs = dz_all(st, &format!("playlist/{}/tracks", pl["id"])).await.iter().map(dz_song).collect();
        p.playlists.push(Playlist { name: pl["title"].as_str().unwrap_or("").into(), description: String::new(), songs });
    }
    for pod in dz_all(st, &format!("user/{id}/podcasts")).await {
        p.podcasts.push(Podcast { title: pod["title"].as_str().unwrap_or("").into(), publisher: String::new(), feed: None });
    }
    p.notes.push("Deezer doesn't share play history, so no plays come from it.".into());
    p.tidy();
    Ok(p)
}
