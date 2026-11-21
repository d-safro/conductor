//! Looked up online, for every app: lyrics from LRCLIB for songs that have none, and cover pictures saved into
//! album folders that have none (iTunes, then Deezer; same rules as music-lookup's upkeep).

use std::{collections::HashMap, path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use lofty::prelude::*;
use serde_json::Value;
use tracing::{info, warn};

use crate::{api::AppState, db::now, library};

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(concat!("Maestro/", env!("CARGO_PKG_VERSION"), " (self-hosted music server)"))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("http client")
}

// ── lyrics ──

/// Lyrics for a song from LRCLIB (synced when it has them), remembered; misses are asked again after 30 days.
pub async fn lyrics(st: &AppState, track_id: &str, title: &str, artist: &str, album: &str, duration_s: i64) -> Option<String> {
    if !st.cfg.lyrics_online {
        return None;
    }
    let hit: Option<(Option<String>, i64)> = sqlx::query_as("SELECT body, fetched_at FROM lyrics_cache WHERE track_id = ?")
        .bind(track_id)
        .fetch_optional(&st.db)
        .await
        .ok()?;
    if let Some((body, at)) = hit
        && (body.is_some() || now() - at < 30 * 86_400)
    {
        return body;
    }
    let d = duration_s.to_string();
    let url = format!("{}/api/get", st.cfg.lrclib_url.trim_end_matches('/'));
    let q = [("track_name", title), ("artist_name", artist), ("album_name", album), ("duration", d.as_str())];
    let r = http().get(&url).query(&q).send().await.ok()?;
    let body = if r.status().is_success() {
        let v: Value = r.json().await.ok()?;
        v["syncedLyrics"].as_str().or(v["plainLyrics"].as_str()).filter(|s| !s.trim().is_empty()).map(String::from)
    } else if r.status().as_u16() == 404 {
        None
    } else {
        return None; // LRCLIB is having a moment: don't remember a miss
    };
    let synced = body.as_deref().is_some_and(|b| b.trim_start().starts_with('['));
    let _ = sqlx::query("INSERT OR REPLACE INTO lyrics_cache (track_id, body, synced, fetched_at) VALUES (?, ?, ?, ?)")
        .bind(track_id)
        .bind(&body)
        .bind(synced)
        .bind(now())
        .execute(&st.db)
        .await;
    body
}

// ── covers into album folders ──

fn cnorm(s: &str) -> String {
    let p = regex::Regex::new(r"[\(\[].*?[\)\]]").unwrap();
    p.replace_all(&s.to_lowercase(), "").chars().filter(|c| c.is_alphanumeric()).collect()
}

/// A big cover's address, or None. The artist must match (or, for a name in another alphabet, the title exactly).
pub async fn find_cover(st: &AppState, artist: &str, album: &str) -> Option<String> {
    let title = regex::Regex::new(r"\s*[\(\[].*?[\)\]]").unwrap().replace_all(album, "").trim().to_string();
    let (na, nt) = (cnorm(artist), cnorm(&title));
    let latin = artist.is_ascii();
    let fits = |a: &str, t: &str| {
        let same_artist = !na.is_empty() && (cnorm(a).contains(&na) || na.contains(&cnorm(a)));
        let same_title = !nt.is_empty() && cnorm(t) == nt;
        if latin { same_artist } else { same_artist || (same_title && nt.chars().count() >= 4) }
    };
    let term = format!("{artist} {title}");
    let it: Value = http()
        .get(format!("{}/search", st.cfg.itunes_url.trim_end_matches('/')))
        .query(&[("term", term.as_str()), ("entity", "album"), ("limit", "5")])
        .send()
        .await
        .ok()?
        .json()
        .await
        .unwrap_or(Value::Null);
    for r in it["results"].as_array().into_iter().flatten() {
        let (a, t) = (r["artistName"].as_str().unwrap_or(""), r["collectionName"].as_str().unwrap_or(""));
        if fits(a, t)
            && cnorm(t) == nt
            && let Some(u) = r["artworkUrl100"].as_str().filter(|u| !u.is_empty())
        {
            return Some(u.replace("100x100bb", "1200x1200bb"));
        }
    }
    for d in st.catalog.dz.list("search/album", &[("q", &term), ("limit", "5")]).await {
        let (a, t) = (d["artist"]["name"].as_str().unwrap_or(""), d["title"].as_str().unwrap_or(""));
        if fits(a, t)
            && cnorm(t) == nt
            && let Some(u) = d["cover_xl"].as_str().or(d["cover_big"].as_str()).filter(|u| !u.is_empty())
        {
            return Some(u.to_string());
        }
    }
    None
}

fn has_image(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|d| {
            d.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_lowercase();
                [".jpg", ".jpeg", ".png", ".webp"].iter().any(|x| n.ends_with(x))
            })
        })
        .unwrap_or(true)
}

fn has_embedded(file: &Path) -> bool {
    lofty::read_from_path(file).map(|t| t.tags().iter().any(|t| !t.pictures().is_empty())).unwrap_or(true)
}

/// Albums with no picture get a cover.jpg in their folder. Only real album folders (Albums/Artist/Album) that hold
/// just that album; nothing is ever overwritten; misses are tried again after 14 days.
pub async fn covers_once(st: &AppState) -> Result<usize> {
    let rows: Vec<(i64, String, String, String, String)> = sqlx::query_as(
        "SELECT t.library_id, t.rel_path, t.album_id, al.artist_name, al.name FROM track t JOIN album al ON al.id = t.album_id WHERE t.missing_since IS NULL",
    )
    .fetch_all(&st.db)
    .await?;
    let mut dirs: HashMap<(i64, String), std::collections::HashSet<String>> = HashMap::new();
    let mut folders_of: HashMap<String, std::collections::HashSet<(i64, String)>> = HashMap::new();
    let mut info_of: HashMap<String, (String, String, String)> = HashMap::new();
    for (lib, rel, album, artist, name) in rows {
        let dir = rel.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        dirs.entry((lib, dir.clone())).or_default().insert(album.clone());
        folders_of.entry(album.clone()).or_default().insert((lib, dir));
        info_of.entry(album).or_insert((artist, name, rel));
    }
    let mut saved = 0;
    for (album, folders) in folders_of {
        if folders.len() != 1 {
            continue;
        }
        let (lib, rel) = folders.into_iter().next().unwrap();
        // a cover there can't land on songs of other albums (loose songs share their artist's folder)
        if rel.matches('/').count() < 2 || dirs.get(&(lib, rel.clone())).is_none_or(|a| a.len() != 1) {
            continue;
        }
        let Some(dir) = library::track_path(&st.libs, lib, &rel) else { continue };
        let (artist, name, first) = info_of[&album].clone();
        let first = library::track_path(&st.libs, lib, &first);
        let (d2, f2) = (dir.clone(), first.clone());
        let skip = tokio::task::spawn_blocking(move || !d2.is_dir() || has_image(&d2) || f2.is_some_and(|f| has_embedded(&f)))
            .await
            .unwrap_or(true);
        if skip {
            continue;
        }
        let miss: Option<i64> = sqlx::query_scalar("SELECT at FROM catalog_cache WHERE kind = 'cover-miss' AND key = ?")
            .bind(&album)
            .fetch_optional(&st.db)
            .await?;
        if miss.is_some_and(|at| now() - at < 14 * 86_400) {
            continue;
        }
        let url = find_cover(st, &artist, &name).await;
        if !st.cfg.slskd_fast_test {
            tokio::time::sleep(Duration::from_millis(1500)).await; // gentle with the free directories
        }
        let Some(url) = url else {
            sqlx::query("INSERT OR REPLACE INTO catalog_cache (kind, key, value, at) VALUES ('cover-miss', ?, NULL, ?)")
                .bind(&album)
                .bind(now())
                .execute(&st.db)
                .await?;
            continue;
        };
        let res: Result<()> = async {
            let data = http().get(&url).timeout(Duration::from_secs(30)).send().await?.bytes().await?;
            if data.len() < 5000 || !data.starts_with(&[0xff, 0xd8, 0xff]) {
                anyhow::bail!("not a JPEG");
            }
            let part = dir.join(".cover.jpg.part");
            tokio::fs::write(&part, &data).await?;
            tokio::fs::rename(&part, dir.join("cover.jpg")).await?;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => {
                saved += 1;
                info!(album = %format!("{artist} - {name}"), "cover saved");
                // resized copies made before have to go
                if let Ok(rd) = std::fs::read_dir(st.cfg.cache_dir().join("covers")) {
                    for e in rd.flatten() {
                        if e.file_name().to_string_lossy().starts_with(&format!("{album}_")) {
                            let _ = std::fs::remove_file(e.path());
                        }
                    }
                }
            }
            Err(e) => warn!(album = %name, "couldn't save a cover: {e:#}"),
        }
    }
    Ok(saved)
}

/// Every 6 hours (and soon after start): covers for albums that have none.
pub async fn worker(st: Arc<AppState>) {
    if !st.cfg.covers_fetch {
        return;
    }
    tokio::time::sleep(Duration::from_secs(if st.cfg.slskd_fast_test { 0 } else { 120 })).await;
    loop {
        match covers_once(&st).await {
            Ok(n) if n > 0 => {
                let _ = library::scan(&st.db, &st.libs, &st.scan, false).await;
            }
            Ok(_) => {}
            Err(e) => warn!("covers: {e:#}"),
        }
        tokio::time::sleep(Duration::from_secs(6 * 3600)).await;
    }
}
