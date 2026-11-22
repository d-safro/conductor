//! Songs you don't have, inside the normal Subsonic answers (what music-lookup added in front of Navidrome):
//! - search3 also lists songs, albums and artists you don't have (⬇)
//! - an album shows its whole tracklist (yours, plus the missing songs ⬇); an artist also lists the albums you don't have
//! - `dz-…` ids work in getSong / getAlbum / getArtist / getCoverArt / stream (a 30-second preview until it's here)
//! - heart a ⬇ song or album, or add a ⬇ song to a playlist: it's downloaded, then hearted / added
//! - lookupSongs, getDiscoveries, getSongDiscoveries (Encore)

use axum::{
    body::Body,
    http::{StatusCode, header},
};
use serde_json::{Value, json};

use super::{ApiError, Ctx, Params, Reply, items, ok, raw};
use crate::catalog::{self, Status, dz_id, is_dz};

fn enabled(c: &Ctx) -> bool {
    c.st.cfg.catalog && !c.st.cfg.catalog_skip_clients.iter().any(|s| c.client.to_lowercase().contains(&s.to_lowercase()))
}

fn sub(c: &Ctx, p: Params) -> Ctx {
    Ctx { st: c.st.clone(), user: c.user.clone(), p, headers: c.headers.clone(), client: c.client.clone(), body: c.body.clone() }
}

fn sv<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

async fn status_map(c: &Ctx, keys: &[String]) -> std::collections::HashMap<String, Status> {
    catalog::statuses(&c.st, keys).await
}

fn track_keys(ts: &[Value]) -> Vec<String> {
    ts.iter().map(|t| format!("dz-t-{}", t["id"].as_i64().unwrap_or(0))).collect()
}

/// Calls answered (or partly answered) here before the normal handler. None: not ours.
pub async fn intercept(c: &Ctx, method: &str) -> Option<Result<Reply, ApiError>> {
    let id = c.p.get("id").unwrap_or("");
    let r = match method {
        "getSong" if dz_id(id, 't').is_some() => dz_song(c, id).await,
        "getAlbum" if dz_id(id, 'a').is_some() => dz_album(c, id).await,
        "getArtist" if dz_id(id, 'r').is_some() => dz_artist(c, id).await,
        "getCoverArt" if is_dz(id) => cover(c, id).await,
        "stream" | "download" if dz_id(id, 't').is_some() => stream(c, id).await,
        "star" | "unstar" if c.p.0.iter().any(|(k, v)| ["id", "albumId", "artistId"].contains(&k.as_str()) && is_dz(v)) => {
            star(c, method == "star").await
        }
        "createPlaylist" if c.p.all("songId").iter().any(|v| is_dz(v)) => create_playlist(c).await,
        "updatePlaylist" if c.p.all("songIdToAdd").iter().any(|v| is_dz(v)) => update_playlist(c).await,
        "scrobble" if c.p.all("id").iter().any(|v| is_dz(v)) => scrobble(c).await,
        "lookupSongs" => lookup_songs(c).await,
        "getDiscoveries" => discoveries(c).await,
        "getSongDiscoveries" => song_discoveries(c).await,
        _ => return None,
    };
    Some(r)
}

/// ⬇ items added to the normal answers.
pub async fn augment(c: &Ctx, method: &str, r: Result<Reply, ApiError>) -> Result<Reply, ApiError> {
    let Ok(Reply::Ok(mut v)) = r else { return r };
    if !enabled(c) {
        return Ok(Reply::Ok(v));
    }
    match method {
        "search3" => search(c, &mut v).await,
        "getAlbum" => complete_album(c, &mut v).await,
        "getArtist" => missing_albums(c, &mut v).await,
        "getPlaylist" => pending(c, &mut v).await,
        _ => {}
    }
    Ok(Reply::Ok(v))
}

async fn search(c: &Ctx, v: &mut Value) {
    let q = c.p.get("query").unwrap_or("").trim().trim_matches('"').trim().to_string();
    let offsets = ["songOffset", "albumOffset", "artistOffset"].iter().any(|k| c.p.int(k).unwrap_or(0) > 0);
    if q.chars().count() < 2 || offsets {
        return;
    }
    let sr = &mut v["searchResult3"];
    let list = |k: &str| sr[k].as_array().cloned().unwrap_or_default();
    let (songs, albums, artists) = (list("song"), list("album"), list("artist"));
    let have_songs: Vec<(String, String)> = songs.iter().map(|s| (sv(s, "artist").into(), sv(s, "title").into())).collect();
    let have_albums: Vec<(String, String)> = albums.iter().map(|a| (sv(a, "artist").into(), sv(a, "name").into())).collect();
    let have_artists: Vec<String> = artists.iter().map(|a| sv(a, "name").into()).collect();
    let count = |k: &str| c.p.int(&format!("{k}Count")).unwrap_or(20).max(0) as usize;
    let found =
        catalog::lookup(&c.st, &q, (count("song"), count("album"), count("artist")), &have_songs, &have_albums, &have_artists)
            .await;
    let stat = status_map(c, &track_keys(&found.songs)).await;
    let push = |sr: &mut Value, k: &str, items: Vec<Value>| {
        if items.is_empty() {
            return;
        }
        if !sr[k].is_array() {
            sr[k] = json!([]);
        }
        sr[k].as_array_mut().unwrap().extend(items);
    };
    let songs_out: Vec<Value> =
        found.songs.iter().map(|t| catalog::ss_song(t, stat.get(&format!("dz-t-{}", t["id"])), None, None, None, None)).collect();
    push(sr, "song", songs_out);
    push(sr, "album", found.albums.iter().map(|a| catalog::ss_album(a, None, None)).collect());
    push(sr, "artist", found.artists.iter().map(catalog::ss_artist).collect());
}

async fn local_song(c: &Ctx, id: &str) -> Option<Value> {
    items::track(c, id).await.ok().map(|t| items::song_json(&t))
}

/// A ⬇ song as an entry of a play queue.
pub async fn queue_entry(c: &Ctx, id: &str) -> Option<Value> {
    match dz_song(c, id).await {
        Ok(Reply::Ok(v)) => Some(v["song"].clone()),
        _ => None,
    }
}

async fn dz_song(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let t = c.st.catalog.dz.get(&format!("track/{}", dz_id(id, 't').unwrap()), &[]).await;
    if sv(&t, "title").is_empty() {
        return Err(ApiError::not_found("Song"));
    }
    let stat = status_map(c, &[id.to_string()]).await;
    ok(json!({ "song": catalog::ss_song(&t, stat.get(id), None, None, None, None) }))
}

async fn dz_album(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let aid = dz_id(id, 'a').unwrap();
    let a = c.st.catalog.dz.get(&format!("album/{aid}"), &[]).await;
    if sv(&a, "title").is_empty() {
        return Err(ApiError::not_found("Album"));
    }
    let (plan, _) = catalog::full_album(&c.st, aid, &[]).await;
    let stat = status_map(c, &track_keys(&plan.iter().map(|p| p.0.clone()).collect::<Vec<_>>())).await;
    let mut songs = vec![];
    for (t, mine) in &plan {
        let local = match mine {
            Some(m) => local_song(c, m).await,
            None => None,
        };
        songs.push(
            local.unwrap_or_else(|| catalog::ss_song(t, stat.get(&format!("dz-t-{}", t["id"])), Some(&a), Some(id), None, None)),
        );
    }
    ok(json!({ "album": catalog::ss_album(&a, None, Some(songs)) }))
}

async fn complete_album(c: &Ctx, v: &mut Value) {
    let al = &v["album"];
    let Some(mine) = al["song"].as_array().cloned() else { return };
    let (artist, name) = (sv(al, "artist").to_string(), sv(al, "name").to_string());
    let titles: Vec<String> = mine.iter().map(|s| sv(s, "title").into()).collect();
    let Some(did) = catalog::match_album(&c.st, &artist, &name, &titles).await else { return };
    let local: Vec<(String, String)> = mine.iter().map(|s| (sv(s, "id").into(), sv(s, "title").into())).collect();
    let (plan, extra) = catalog::full_album(&c.st, &did, &local).await;
    if plan.iter().all(|(_, m)| m.is_some()) {
        return; // you have the whole album
    }
    let a = c.st.catalog.dz.get(&format!("album/{did}"), &[]).await;
    let stat = status_map(c, &track_keys(&plan.iter().map(|p| p.0.clone()).collect::<Vec<_>>())).await;
    let (alid, arid, cover) = (sv(al, "id").to_string(), sv(al, "artistId").to_string(), sv(al, "coverArt").to_string());
    let mut songs = vec![];
    for (t, m) in &plan {
        match m {
            Some(id) => {
                let mut s = match mine.iter().find(|s| sv(s, "id") == id) {
                    Some(s) => s.clone(),
                    None => match local_song(c, id).await {
                        Some(s) => s,
                        None => continue,
                    },
                };
                s["track"] = t["track_position"].clone();
                s["discNumber"] = json!(t["disk_number"].as_i64().unwrap_or(1));
                songs.push(s);
            }
            None => songs.push(catalog::ss_song(
                t,
                stat.get(&format!("dz-t-{}", t["id"])),
                Some(&a),
                Some(&alid),
                Some(&arid),
                Some(&cover),
            )),
        }
    }
    songs.extend(mine.iter().filter(|s| extra.iter().any(|e| e == sv(s, "id"))).cloned());
    let dur: i64 = songs.iter().map(|s| s["duration"].as_i64().unwrap_or(0)).sum();
    v["album"]["songCount"] = songs.len().into();
    v["album"]["duration"] = dur.into();
    v["album"]["song"] = songs.into();
}

async fn dz_artist(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let rid = dz_id(id, 'r').unwrap();
    let a = c.st.catalog.dz.get(&format!("artist/{rid}"), &[]).await;
    if sv(&a, "name").is_empty() {
        return Err(ApiError::not_found("Artist"));
    }
    let albums: Vec<Value> =
        catalog::missing_albums_of(&c.st, rid, &[]).await.iter().map(|x| catalog::ss_album(x, Some(id), None)).collect();
    let mut v = catalog::ss_artist(&a);
    v["albumCount"] = albums.len().into();
    v["album"] = albums.into();
    ok(json!({ "artist": v }))
}

async fn missing_albums(c: &Ctx, v: &mut Value) {
    let art = &v["artist"];
    let name = sv(art, "name").to_string();
    if name.is_empty() {
        return;
    }
    let have: Vec<String> = art["album"].as_array().into_iter().flatten().map(|a| sv(a, "name").to_string()).collect();
    let arid = sv(art, "id").to_string();
    let extra: Vec<Value> =
        catalog::missing_albums(&c.st, &name, &have).await.iter().map(|x| catalog::ss_album(x, Some(&arid), None)).collect();
    if extra.is_empty() {
        return;
    }
    if !v["artist"]["album"].is_array() {
        v["artist"]["album"] = json!([]);
    }
    v["artist"]["album"].as_array_mut().unwrap().extend(extra);
    let n = v["artist"]["album"].as_array().unwrap().len();
    v["artist"]["albumCount"] = n.into();
}

async fn pending(c: &Ctx, v: &mut Value) {
    let pid = sv(&v["playlist"], "id").to_string();
    let keys = catalog::pending_for_playlist(&c.st, &pid).await;
    if keys.is_empty() {
        return;
    }
    let stat = status_map(c, &keys).await;
    let mut add = vec![];
    for k in &keys {
        let t = c.st.catalog.dz.get(&format!("track/{}", dz_id(k, 't').unwrap_or("0")), &[]).await;
        if !sv(&t, "title").is_empty() {
            add.push(catalog::ss_song(&t, stat.get(k), None, None, None, None));
        }
    }
    if !v["playlist"]["entry"].is_array() {
        v["playlist"]["entry"] = json!([]);
    }
    v["playlist"]["entry"].as_array_mut().unwrap().extend(add);
}

/// The picture for a ⬇ song, album or artist (Deezer's), fetched once and kept.
async fn cover(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let size = c.p.int("size").unwrap_or(0);
    let big = size == 0 || size > 500;
    let dz = &c.st.catalog.dz;
    let url = if let Some(a) = dz_id(id, 'a') {
        let a = dz.get(&format!("album/{a}"), &[]).await;
        sv(&a, if big { "cover_xl" } else { "cover_big" }).to_string()
    } else if let Some(r) = dz_id(id, 'r') {
        let a = dz.get(&format!("artist/{r}"), &[]).await;
        sv(&a, if big { "picture_xl" } else { "picture_big" }).to_string()
    } else if let Some(t) = dz_id(id, 't') {
        let t = dz.get(&format!("track/{t}"), &[]).await;
        sv(&t["album"], if big { "cover_xl" } else { "cover_big" }).to_string()
    } else {
        String::new()
    };
    let nf = || ApiError { http: StatusCode::NOT_FOUND, ..ApiError::not_found("Cover art") };
    if url.is_empty() {
        return Err(nf());
    }
    let cache = c.st.cfg.cache_dir().join("deezer-covers").join(format!("{id}_{}", if big { "xl" } else { "big" }));
    let bytes = match tokio::fs::read(&cache).await {
        Ok(b) => b,
        Err(_) => {
            let r = dz.http().get(&url).send().await.map_err(|_| nf())?;
            let b = r.bytes().await.map_err(|_| nf())?.to_vec();
            if image::guess_format(&b).is_err() {
                return Err(nf());
            }
            if let Some(d) = cache.parent() {
                let _ = tokio::fs::create_dir_all(d).await;
            }
            let _ = tokio::fs::write(&cache, &b).await;
            b
        }
    };
    Ok(raw(
        StatusCode::OK,
        &[(header::CONTENT_TYPE, "image/jpeg".into()), (header::CACHE_CONTROL, "public, max-age=86400".into())],
        Body::from(bytes),
    ))
}

/// Your copy once it's here; until then Deezer's 30-second preview.
async fn stream(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let local: Option<String> =
        sqlx::query_scalar("SELECT local_id FROM request WHERE key = ?").bind(id).fetch_optional(&c.st.db).await?.flatten();
    if let Some(l) = local {
        let mut p = c.p.clone();
        p.0.retain(|(k, _)| k != "id" && k != "hold");
        p.0.push(("id".into(), l));
        return Box::pin(super::media::stream(&sub(c, p), false)).await;
    }
    if c.p.bool("hold") == Some(true) {
        // Encore is getting it: silence until it's here, not the preview
        return Ok(raw(
            StatusCode::OK,
            &[(header::CONTENT_TYPE, "audio/mpeg".into()), (header::CACHE_CONTROL, "no-store".into())],
            Body::from(silence(10)),
        ));
    }
    let t = c.st.catalog.dz.get(&format!("track/{}", dz_id(id, 't').unwrap()), &[]).await;
    let preview = sv(&t, "preview");
    if preview.is_empty() {
        return Err(ApiError::not_found("Preview"));
    }
    let r =
        c.st.catalog.dz.http().get(preview).send().await.map_err(|e| ApiError::new(0, format!("Deezer didn't answer: {e}")))?;
    let len = r.headers().get(header::CONTENT_LENGTH.as_str()).and_then(|v| v.to_str().ok()).map(String::from);
    let mut h = vec![(header::CONTENT_TYPE, "audio/mpeg".to_string())];
    if let Some(l) = len {
        h.push((header::CONTENT_LENGTH, l));
    }
    Ok(raw(StatusCode::OK, &h, Body::from_stream(r.bytes_stream())))
}

async fn star(c: &Ctx, on: bool) -> Result<Reply, ApiError> {
    let mut rest = Params::default();
    for (k, v) in &c.p.0 {
        let is_item = ["id", "albumId", "artistId"].contains(&k.as_str());
        if !(is_item && is_dz(v)) {
            rest.0.push((k.clone(), v.clone()));
            continue;
        }
        if dz_id(v, 't').is_some() {
            if on {
                catalog::ask(&c.st, v, &c.user.id, true, None, false).await?;
            } else if let Some(local) = catalog::unheart(&c.st, v, &c.user.id).await? {
                rest.0.push(("id".into(), local)); // it has arrived: unheart your copy as well
            }
        } else if let Some(a) = dz_id(v, 'a') {
            // a whole ⬇ album: every song on it you don't have
            for t in catalog::album_tracks(&c.st, a).await {
                if catalog::find(&c.st, sv(&t, "title"), sv(&t["artist"], "name"), t["duration"].as_i64()).await.is_some() {
                    continue;
                }
                let k = format!("dz-t-{}", t["id"]);
                if on {
                    catalog::ask(&c.st, &k, &c.user.id, true, None, false).await?;
                } else {
                    catalog::unheart(&c.st, &k, &c.user.id).await?;
                }
            }
        }
    }
    if rest.0.iter().any(|(k, _)| ["id", "albumId", "artistId"].contains(&k.as_str())) {
        return super::userdata::star(&sub(c, rest), on).await;
    }
    super::empty()
}

async fn create_playlist(c: &Ctx) -> Result<Reply, ApiError> {
    let theirs: Vec<String> = c.p.all("songId").into_iter().filter(|v| is_dz(v)).map(String::from).collect();
    let mut rest = c.p.clone();
    rest.0.retain(|(k, v)| !(k == "songId" && is_dz(v)));
    let r = super::userdata::create_playlist(&sub(c, rest)).await?;
    let Reply::Ok(v) = &r else { return Ok(r) };
    let pid = sv(&v["playlist"], "id").to_string();
    for k in theirs {
        catalog::ask(&c.st, &k, &c.user.id, false, Some(&pid), false).await?;
    }
    Ok(r)
}

async fn update_playlist(c: &Ctx) -> Result<Reply, ApiError> {
    let pid = c.p.req("playlistId")?.to_string();
    let theirs: Vec<String> = c.p.all("songIdToAdd").into_iter().filter(|v| is_dz(v)).map(String::from).collect();
    let mut rest = c.p.clone();
    rest.0.retain(|(k, v)| !(k == "songIdToAdd" && is_dz(v)));
    let r = super::userdata::update_playlist(&sub(c, rest)).await?;
    for k in theirs {
        catalog::ask(&c.st, &k, &c.user.id, false, Some(&pid), false).await?;
    }
    Ok(r)
}

/// Playing a ⬇ song: if you have it after all, the play counts for your copy.
async fn scrobble(c: &Ctx) -> Result<Reply, ApiError> {
    let mut rest = Params::default();
    for (k, v) in &c.p.0 {
        if k == "id" && is_dz(v) {
            let local: Option<String> = sqlx::query_scalar("SELECT local_id FROM request WHERE key = ?")
                .bind(v)
                .fetch_optional(&c.st.db)
                .await?
                .flatten();
            let mine = match local {
                Some(l) => Some(l),
                None => {
                    let t = c.st.catalog.dz.get(&format!("track/{}", dz_id(v, 't').unwrap_or("0")), &[]).await;
                    catalog::find(&c.st, sv(&t, "title"), sv(&t["artist"], "name"), t["duration"].as_i64()).await.map(|s| s.id)
                }
            };
            match mine {
                Some(m) => rest.0.push(("id".into(), m)), // you have it after all: the play counts for your copy
                None => {
                    // playing a ⬇ song (any app reports "now playing"): it's downloaded on trial
                    if let Err(e) = catalog::ask(&c.st, v, &c.user.id, false, None, true).await {
                        tracing::warn!("couldn't start a trial download for {v}: {e:#}");
                    }
                }
            }
        } else {
            rest.0.push((k.clone(), v.clone()));
        }
    }
    if rest.0.iter().any(|(k, _)| k == "id") {
        return super::userdata::scrobble(&sub(c, rest)).await;
    }
    super::empty()
}

/// Seconds of silence as an MP3 (MPEG-1 layer III, 128 kb/s, 44.1 kHz; every frame empty).
fn silence(seconds: usize) -> Vec<u8> {
    let mut frame = vec![0u8; 417];
    frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0xc0]);
    frame.repeat(seconds * 44_100 / 1152)
}

fn status_json(k: &str, s: Option<&Status>) -> Value {
    let status = match s {
        Some(Status { local_id: Some(_), .. }) => "ready",
        None => "none",
        Some(Status { status: Some(st), .. }) if st == "notfound" => "notfound",
        Some(Status { status: Some(st), .. }) if st == "queued" || st == "done" => "downloading",
        Some(_) => "searching",
    };
    let mut v = json!({ "id": k, "status": status, "temporary": s.is_some_and(|s| s.trial) });
    if let Some(l) = s.and_then(|s| s.local_id.clone()) {
        v["songId"] = l.into();
    }
    v
}

/// Encore: how getting these ⬇ songs is going (none · searching · downloading · ready · notfound · limit);
/// fetch=true first asks for each one on trial (you're playing it, queued it, or autoplay / radio picked it).
async fn lookup_songs(c: &Ctx) -> Result<Reply, ApiError> {
    let keys: Vec<String> = c.p.all("id").into_iter().filter(|k| dz_id(k, 't').is_some()).take(100).map(String::from).collect();
    let fetch = c.p.bool("fetch") == Some(true);
    let mut stat = status_map(c, &keys).await;
    let mut out = vec![];
    for k in &keys {
        let mut v = status_json(k, stat.get(k));
        if fetch && (v["status"] == "none" || v["status"] == "notfound") {
            let t = c.st.catalog.dz.get(&format!("track/{}", dz_id(k, 't').unwrap()), &[]).await;
            let mine = catalog::find(&c.st, sv(&t, "title"), sv(&t["artist"], "name"), t["duration"].as_i64()).await;
            if let Some(m) = mine {
                v = json!({ "id": k, "status": "ready", "temporary": false, "songId": m.id }); // you have it after all
            } else if v["status"] == "none" && catalog::trials_today(&c.st).await >= c.st.cfg.trial_per_day {
                v = json!({ "id": k, "status": "limit", "temporary": false });
            } else if catalog::ask(&c.st, k, &c.user.id, false, None, true).await? {
                stat = status_map(c, &keys).await;
                v = status_json(k, stat.get(k));
            } else {
                v = json!({ "id": k, "status": "notfound", "temporary": false });
            }
        }
        out.push(v);
    }
    let pid = crate::catalog::trial::playlist_id(&c.st, &c.user.id, false).await?.unwrap_or_default();
    ok(json!({ "lookupSongs": {
        "song": out, "trialHours": c.st.cfg.trial_hours, "perDay": c.st.cfg.trial_per_day,
        "usedToday": catalog::trials_today(&c.st).await, "playlistId": pid, "playlist": crate::catalog::trial::PLAYLIST,
    } }))
}

async fn discoveries(c: &Ctx) -> Result<Reply, ApiError> {
    let sections = catalog::discoveries(&c.st, &c.user.id).await;
    let keys: Vec<String> = sections.iter().flat_map(|(_, _, s)| track_keys(s)).collect();
    let stat = status_map(c, &keys).await;
    let out: Vec<Value> = sections
        .into_iter()
        .map(|(name, seed, songs)| {
            json!({ "name": name, "seed": seed, "song": songs.iter().map(|t| catalog::ss_song(t, stat.get(&format!("dz-t-{}", t["id"])), None, None, None, None)).collect::<Vec<_>>() })
        })
        .collect();
    ok(json!({ "discoveries": { "section": out } }))
}

async fn song_discoveries(c: &Ctx) -> Result<Reply, ApiError> {
    let count = c.p.int("count").unwrap_or(30).clamp(1, 100) as usize;
    let Ok(t) = items::track(c, c.p.req("id")?).await else { return ok(json!({ "songDiscoveries": { "song": [] } })) };
    let songs = catalog::song_discoveries(&c.st, &t.title, &t.artist_name, count).await;
    let stat = status_map(c, &track_keys(&songs)).await;
    let out: Vec<Value> =
        songs.iter().map(|t| catalog::ss_song(t, stat.get(&format!("dz-t-{}", t["id"])), None, None, None, None)).collect();
    ok(json!({ "songDiscoveries": { "song": out } }))
}
