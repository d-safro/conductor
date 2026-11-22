//! Artist info and similar songs, from what the server knows: Deezer (the artist's picture and related artists,
//! when the catalog is on) and AudioMuse (songs that sound alike, when it's set up). Without either, similar songs
//! come from the same artist and genre.
//!
//!   getArtistInfo2?id=&count=&includeNotPresent=   images, MusicBrainz id, similar artists (yours; with
//!                                                  includeNotPresent, ⬇ ones too for apps that get the catalog)
//!   getSimilarSongs2?id=&count=                    songs like this song / album / artist, from your library

use std::collections::HashSet;

use serde_json::{Value, json};

use super::{ApiError, Ctx, Reply, items, ok};
use crate::catalog;

fn catalog_for(c: &Ctx) -> bool {
    c.st.cfg.catalog && !c.st.cfg.catalog_skip_clients.iter().any(|s| c.client.to_lowercase().contains(&s.to_lowercase()))
}

/// Your artists among Deezer's related artists for `name`, in Deezer's order, and the ones you don't have.
async fn related(c: &Ctx, name: &str) -> (Option<Value>, Vec<items::ArtistRow>, Vec<Value>) {
    if !c.st.cfg.catalog {
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
    let (dz, mine, theirs) = related(c, &a.name).await;
    let mut out = json!({});
    if let Some(m) = a.mbid.as_deref().filter(|m| !m.is_empty()) {
        out["musicBrainzId"] = m.into();
    }
    if let Some(d) = dz {
        for (k, f) in [("smallImageUrl", "picture_small"), ("mediumImageUrl", "picture_medium"), ("largeImageUrl", "picture_big")]
        {
            if let Some(u) = d[f].as_str().filter(|u| !u.is_empty() && !u.contains("/artist//")) {
                out[k] = u.into();
            }
        }
    }
    let mut similar: Vec<Value> = mine.iter().filter(|x| x.id != a.id).take(count).map(items::artist_json).collect();
    if c.p.bool("includeNotPresent") == Some(true) && catalog_for(c) {
        let room = count.saturating_sub(similar.len());
        similar.extend(theirs.iter().take(room).map(catalog::ss_artist));
    }
    if !similar.is_empty() {
        out["similarArtist"] = similar.into();
    }
    ok(json!({ key: out }))
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
            let a = items::albums_where(c, "al.id = ?", &[&id], "").await?.remove(0);
            (a.artist_id.clone(), a.artist_name.clone(), a.genre.clone().unwrap_or_default(), None)
        }
        Some("artist") => {
            let a = items::artists_where(c, "ar.id = ?", &[&id], "").await?.remove(0);
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
    // 1. songs that sound like it (AudioMuse)
    if let Some(t) = &seed_track
        && let Some(found) = super::sonic::similar_ids(&c.st, t, count).await
    {
        for x in found {
            push(&mut ids, x);
        }
    }
    // 2. songs by related artists you have (Deezer), mixed with the artist's own
    if ids.len() < count {
        let (_, mine, _) = related(c, &artist_name).await;
        let mut artist_ids: Vec<String> = vec![artist_id.clone()];
        artist_ids.extend(mine.into_iter().map(|a| a.id).take(10));
        let marks = vec!["?"; artist_ids.len()].join(",");
        let binds: Vec<&str> = artist_ids.iter().map(String::as_str).collect();
        let rows = items::tracks_where(
            c,
            &format!("(t.artist_id IN ({marks}) OR al.artist_id IN ({marks}))"),
            &[binds.as_slice(), binds.as_slice()].concat(),
            "ORDER BY RANDOM() LIMIT 1000",
        )
        .await?;
        // in turns: one song from each artist, then the next round
        let mut groups: Vec<Vec<String>> = vec![vec![]; artist_ids.len()];
        for t in rows {
            if let Some(i) = artist_ids.iter().position(|a| *a == t.artist_id || *a == t.album_artist_id) {
                groups[i].push(t.id);
            }
        }
        let rounds = groups.iter().map(Vec::len).max().unwrap_or(0);
        for r in 0..rounds {
            for g in &groups {
                if let Some(x) = g.get(r) {
                    push(&mut ids, x.clone());
                }
            }
        }
    }
    // 3. the same genre
    if ids.len() < count && !genre.is_empty() {
        for t in items::tracks_where(c, "t.genre = ?", &[&genre], &format!("ORDER BY RANDOM() LIMIT {count}")).await? {
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
