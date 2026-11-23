//! Artist and album info and similar songs, from what the server knows: Deezer (the artist's picture and related
//! artists, when the catalog is on), Wikipedia through MusicBrainz (biographies, album notes, album pictures from
//! the Cover Art Archive; see wiki.rs) and AudioMuse (songs that sound alike, when it's set up; else Conductor's own
//! analysis of the audio, crate::audio). Without any of them,
//! similar songs come from the same artist and genre.
//!
//!   getArtistInfo2?id=&count=&includeNotPresent=   biography, images, MusicBrainz id, similar artists (yours;
//!                                                  with includeNotPresent, ⬇ ones too for apps that get the catalog)
//!   getAlbumInfo2?id=                              notes, MusicBrainz id, images (getAlbumInfo also takes a song id)
//!   getSimilarSongs2?id=&count=                    songs like this song / album / artist, from your library

use std::{collections::HashSet, time::Duration};

use serde_json::{Value, json};

use super::{ApiError, Ctx, Reply, items, ok};
use crate::{catalog, wiki};

/// How long an app waits for a first lookup; a slower one finishes in the background and is there next time.
const WAIT: Duration = Duration::from_secs(4);

/// The first of a joined album artist ("A • B" → "A"), for looking it up.
fn first_artist(name: &str) -> &str {
    name.split(crate::library::ARTIST_JOINER).next().unwrap_or(name)
}

fn catalog_for(c: &Ctx) -> bool {
    c.st.cfg.catalog && !c.st.cfg.catalog_skip_clients.iter().any(|s| c.client.to_lowercase().contains(&s.to_lowercase()))
}

/// Your artists among Deezer's related artists for `name`, in Deezer's order, and the ones you don't have.
async fn related(c: &Ctx, name: &str) -> (Option<Value>, Vec<items::ArtistRow>, Vec<Value>) {
    // with online info, or the catalog (which asks Deezer anyway): related artists you have are worth knowing
    // without a downloader too (the ones you don't have are only offered with the catalog; see artist_info)
    if !c.st.cfg.info_online && !c.st.cfg.catalog {
        return (None, vec![], vec![]);
    }
    let Some(info) = catalog::artist_info(&c.st, name).await else { return (None, vec![], vec![]) };
    let (mut mine, mut theirs, mut seen) = (vec![], vec![], HashSet::new());
    for r in info["related"].as_array().into_iter().flatten() {
        let n = r["name"].as_str().unwrap_or("");
        if n.is_empty() || !seen.insert(n.to_lowercase()) {
            continue;
        }
        match catalog::library_artist_id(&c.st, n).await {
            Some(id) => {
                if let Ok(mut a) = items::artists_where(c, "ar.id = ?", &[&id], "").await
                    && !a.is_empty()
                {
                    mine.push(a.remove(0));
                }
            }
            None => theirs.push(r.clone()),
        }
    }
    (Some(info["artist"].clone()), mine, theirs)
}

pub async fn artist_info(c: &Ctx, id3: bool) -> Result<Reply, ApiError> {
    let key = if id3 { "artistInfo2" } else { "artistInfo" };
    let id = c.p.req("id")?;
    let count = c.p.int("count").unwrap_or(20).clamp(0, 100) as usize;
    let Some(a) = items::artists_where(c, "ar.id = ?", &[id], "").await?.into_iter().next() else {
        return ok(json!({ key: {} }));
    };
    let ((dz, mine, theirs), w) = tokio::join!(related(c, &a.name), wiki::artist(&c.st, &a.name, a.mbid.as_deref(), WAIT));
    let mut out = json!({});
    if let Some(m) = a.mbid.as_deref().filter(|m| !m.is_empty()).or(w.as_ref().and_then(|w| w["mbid"].as_str())) {
        out["musicBrainzId"] = m.into();
    }
    if let Some(b) = w.as_ref().and_then(wiki::as_html) {
        out["biography"] = b.into();
    }
    if let Some(d) = dz {
        for (k, f) in [("smallImageUrl", "picture_small"), ("mediumImageUrl", "picture_medium"), ("largeImageUrl", "picture_big")]
        {
            if let Some(u) = d[f].as_str().filter(|u| !u.is_empty() && !u.contains("/artist//")) {
                out[k] = u.into();
            }
        }
    }
    if out.get("largeImageUrl").is_none()
        && let Some(img) = w.as_ref().and_then(|w| w["image"].as_str())
    {
        for k in ["smallImageUrl", "mediumImageUrl", "largeImageUrl"] {
            out[k] = img.into();
        }
    }
    let mut similar: Vec<Value> = mine.iter().filter(|x| x.id != a.id).take(count).map(items::artist_json).collect();
    // and Conductor's own picks from the library (played together, genres, script, era) to fill the list, leaving
    // room (up to half) for the artists you don't have when the app asks for them too
    let include_theirs = c.p.bool("includeNotPresent") == Some(true) && catalog_for(c);
    let room_for_theirs = if include_theirs { theirs.len().min(count / 2) } else { 0 };
    let count_own = count.saturating_sub(room_for_theirs);
    if similar.len() < count_own {
        let related: Vec<String> = mine.iter().map(|x| x.id.clone()).collect();
        for (id, _) in crate::similar::artists(&c.st, &a.id, &related, count).await {
            if similar.len() >= count_own {
                break;
            }
            if similar.iter().any(|s| s["id"] == id.as_str()) {
                continue;
            }
            if let Some(x) = items::artists_where(c, "ar.id = ?", &[&id], "").await?.into_iter().next() {
                similar.push(items::artist_json(&x));
            }
        }
    }
    if include_theirs {
        let room = count.saturating_sub(similar.len());
        similar.extend(theirs.iter().take(room).map(catalog::ss_artist));
    }
    if !similar.is_empty() {
        out["similarArtist"] = similar.into();
    }
    ok(json!({ key: out }))
}

pub async fn album_info(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?.to_string();
    let album_id = match items::kind_of(c, &id).await? {
        Some("album") => id,
        Some("track") => items::track(c, &id).await?.album_id.clone(),
        _ => return Err(ApiError::not_found("Album")),
    };
    let Some(a) = items::albums_where(c, "al.id = ?", &[&album_id], "").await?.into_iter().next() else {
        return Err(ApiError::not_found("Album"));
    };
    let w = wiki::album(&c.st, first_artist(&a.artist_name), &a.name, a.mbid.as_deref(), WAIT).await;
    let mut out = json!({});
    if let Some(n) = w.as_ref().and_then(wiki::as_html) {
        out["notes"] = n.into();
    }
    if let Some(m) = a.mbid.as_deref().filter(|m| !m.is_empty()).or(w.as_ref().and_then(|w| w["mbid"].as_str())) {
        out["musicBrainzId"] = m.into();
    }
    if let Some(w) = &w {
        for (k, f) in [("smallImageUrl", "small"), ("mediumImageUrl", "medium"), ("largeImageUrl", "large")] {
            if let Some(u) = w["images"][f].as_str() {
                out[k] = u.into();
            }
        }
    }
    ok(json!({ "albumInfo": out }))
}

pub async fn similar_songs(c: &Ctx, id3: bool) -> Result<Reply, ApiError> {
    let key = if id3 { "similarSongs2" } else { "similarSongs" };
    let id = c.p.req("id")?.to_string();
    let count = c.p.int("count").unwrap_or(50).clamp(1, 500) as usize;
    let (artist_id, artist_name, genre, seed_track) = match items::kind_of(c, &id).await? {
        Some("track") => {
            let t = items::track(c, &id).await?;
            (t.artist_id.clone(), t.artist_name.clone(), t.genre.clone().unwrap_or_default(), Some(t.id))
        }
        Some("album") => {
            let a = items::albums_where(c, "al.id = ?", &[&id], "")
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::not_found("Album"))?;
            (a.artist_id.clone(), a.artist_name.clone(), a.genre.clone().unwrap_or_default(), None)
        }
        Some("artist") => {
            let a = items::artists_where(c, "ar.id = ?", &[&id], "")
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::not_found("Artist"))?;
            (a.id.clone(), a.name.clone(), String::new(), None)
        }
        _ => return Err(ApiError::not_found("Item")),
    };
    let mut ids: Vec<String> = vec![];
    let push = |ids: &mut Vec<String>, x: String| {
        if x != id && !ids.contains(&x) {
            ids.push(x);
        }
    };
    // 1. songs that sound like it and go with it (AudioMuse, else Conductor's own analysis blended with the artists
    //    that go with it: crate::audio)
    if let Some(t) = &seed_track
        && let Some(found) = super::sonic::similar_songs(&c.st, t, count).await
    {
        for x in found {
            push(&mut ids, x);
        }
    }
    // 2. the artist's own songs, in turns with those of the artists that go with it: played together, related
    //    online, the same genres, the same script, the same era (crate::similar). Within each artist, the songs
    //    you've hearted or played come first.
    if ids.len() < count {
        let (_, mine, _) = related(c, &artist_name).await;
        let related_ids: Vec<String> = mine.into_iter().map(|a| a.id).collect();
        let mut artist_ids: Vec<String> = vec![artist_id.clone()];
        artist_ids.extend(crate::similar::artists(&c.st, &artist_id, &related_ids, 25).await.into_iter().map(|(id, _)| id));
        let marks = vec!["?"; artist_ids.len()].join(",");
        let binds: Vec<&str> = artist_ids.iter().map(String::as_str).collect();
        let rows = items::tracks_where(
            c,
            &format!("(t.artist_id IN ({marks}) OR al.artist_id IN ({marks}))"),
            &[binds.as_slice(), binds.as_slice()].concat(),
            "ORDER BY RANDOM() LIMIT 3000",
        )
        .await?;
        let mut groups: Vec<Vec<&items::TrackRow>> = vec![vec![]; artist_ids.len()];
        for t in &rows {
            if let Some(i) = artist_ids.iter().position(|a| *a == t.artist_id || *a == t.album_artist_id) {
                groups[i].push(t);
            }
        }
        for g in &mut groups {
            // hearted, then played, then the rest (each kept in its random order)
            g.sort_by_key(|t| (t.starred_at.is_none(), t.play_count.unwrap_or(0) == 0));
        }
        // in turns, the closest artists first: the first round takes the artist and its 8 closest, the next 16, …
        // so what plays first goes best with it, and the list still reaches further as it goes on
        let mut next = vec![0usize; groups.len()];
        for r in 0..groups.iter().map(Vec::len).max().unwrap_or(0) + groups.len() / 8 + 1 {
            for (i, g) in groups.iter().enumerate().take(8 * (r + 1) + 1) {
                if let Some(t) = g.get(next[i]) {
                    push(&mut ids, t.id.clone());
                    next[i] += 1;
                }
            }
        }
    }
    // 3. the same genre
    if ids.len() < count && !genre.is_empty() {
        let first = crate::library::split_genres(&genre).into_iter().next().unwrap_or_default();
        let cond = crate::library::has_genre_sql("t.genre");
        for t in items::tracks_where(c, &cond, &[&first], &format!("ORDER BY RANDOM() LIMIT {count}")).await? {
            push(&mut ids, t.id);
        }
    }
    ids.truncate(count);
    let mut songs = vec![];
    if !ids.is_empty() {
        let marks = vec!["?"; ids.len()].join(",");
        let binds: Vec<&str> = ids.iter().map(String::as_str).collect();
        let rows = items::tracks_where(c, &format!("t.id IN ({marks})"), &binds, "").await?;
        let mut by: std::collections::HashMap<&str, &items::TrackRow> = rows.iter().map(|t| (t.id.as_str(), t)).collect();
        for x in &ids {
            if let Some(t) = by.remove(x.as_str()) {
                songs.push(items::song_json(t));
            }
        }
    }
    ok(json!({ key: { "song": songs } }))
}
