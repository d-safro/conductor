//! Moving over from Navidrome: `encore-server import-navidrome <navidrome.db> [--artwork <navidrome data>/artwork]`.
//!
//! Reads a Navidrome database (opened read-only; use a copy) and brings over, keeping Navidrome's ids so apps
//! that remember ids keep working: users (names and admin flag; passwords have to be set again), plays, ratings,
//! stars, play history, playlists (with their smart rules and uploaded pictures), play queues and radio stations.
//! The library itself is rescanned from the music folder. Running it again replaces what it brought over before.

use std::{collections::HashMap, path::Path, str::FromStr, sync::Arc};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqlx::{
    Row,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use tracing::info;

use crate::{
    auth::Secrets,
    config::{Config, LibraryConfig},
    db::{self, Db},
    library::{self, ScanStatus, album_key, artist_key, track_key},
};

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub users: Vec<String>,
    pub song_ids: usize,
    pub album_ids: usize,
    pub artist_ids: usize,
    pub songs_matched_after_scan: usize,
    pub songs_not_found: usize,
    pub annotations: usize,
    pub plays: usize,
    pub playlists: usize,
    pub playlist_pictures: usize,
    pub play_queues: usize,
    pub radio_stations: usize,
}

/// Navidrome's times ("2026-11-12 19:17:06.918+00:00") as Unix seconds.
fn nd_time(s: Option<String>) -> Option<i64> {
    s.and_then(|s| db::parse_iso(&s))
}

pub async fn run(
    cfg: &Config,
    ours: &Db,
    secrets: &Secrets,
    libs: &[(i64, LibraryConfig)],
    nd_path: &Path,
    artwork: Option<&Path>,
) -> Result<ImportReport> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", nd_path.display()))?.read_only(true);
    let nd = SqlitePoolOptions::new().max_connections(1).connect_with(opts).await.context("opening the Navidrome database")?;
    let mut rep = ImportReport::default();

    // Navidrome libraries → ours: by name, or the only one on each side.
    let nd_libs: Vec<(i64, String)> = sqlx::query_as("SELECT id, name FROM library").fetch_all(&nd).await?;
    let mut lib_map: HashMap<i64, i64> = HashMap::new();
    for (nid, name) in &nd_libs {
        let ours = libs.iter().find(|(_, l)| l.name.eq_ignore_ascii_case(name)).or(if nd_libs.len() == 1 && libs.len() == 1 {
            libs.first()
        } else {
            None
        });
        match ours {
            Some((oid, _)) => {
                lib_map.insert(*nid, *oid);
            }
            None => bail!("no music folder here matches Navidrome's library \"{name}\" (give one the same name in config.toml)"),
        }
    }

    let mut tx = ours.begin().await?;
    for t in ["id_override", "annotation", "play", "playlist_entry", "playlist", "play_queue", "radio"] {
        sqlx::query(&format!("DELETE FROM {t}")).execute(&mut *tx).await?;
    }
    // the library is rescanned below, now with the old ids
    for t in ["track", "album", "artist"] {
        sqlx::query(&format!("DELETE FROM {t}")).execute(&mut *tx).await?;
    }

    // users: same ids; a random password until it's set again
    let users = sqlx::query("SELECT id, user_name, is_admin, created_at FROM user").fetch_all(&nd).await?;
    let mut user_map: HashMap<String, String> = HashMap::new();
    for u in users {
        let (id, name): (String, String) = (u.get("id"), u.get("user_name"));
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(&name).fetch_optional(&mut *tx).await?;
        let mine = match existing {
            Some(e) => e,
            None => {
                sqlx::query("INSERT INTO user (id, name, password_enc, is_admin, created_at) VALUES (?, ?, ?, ?, ?)")
                    .bind(&id)
                    .bind(&name)
                    .bind(secrets.encrypt(&db::random_id()))
                    .bind(u.get::<bool, _>("is_admin"))
                    .bind(nd_time(u.get("created_at")).unwrap_or_else(db::now))
                    .execute(&mut *tx)
                    .await?;
                rep.users.push(name.clone());
                id.clone()
            }
        };
        user_map.insert(id, mine);
    }

    // ids: songs by path, albums by album artist + name, artists by name
    for r in sqlx::query("SELECT id, path, library_id FROM media_file").fetch_all(&nd).await? {
        let Some(lib) = lib_map.get(&r.get::<i64, _>("library_id")) else { continue };
        let path: String = r.get("path");
        sqlx::query("INSERT OR IGNORE INTO id_override (kind, key, id) VALUES ('track', ?, ?)")
            .bind(track_key(*lib, &path.replace('\\', "/")))
            .bind(r.get::<String, _>("id"))
            .execute(&mut *tx)
            .await?;
        rep.song_ids += 1;
    }
    for r in
        sqlx::query("SELECT id, name, album_artist FROM album WHERE missing = 0 ORDER BY song_count DESC").fetch_all(&nd).await?
    {
        let key = album_key(&r.get::<String, _>("album_artist"), &r.get::<String, _>("name"));
        let done = sqlx::query("INSERT OR IGNORE INTO id_override (kind, key, id) VALUES ('album', ?, ?)")
            .bind(key)
            .bind(r.get::<String, _>("id"))
            .execute(&mut *tx)
            .await?;
        rep.album_ids += done.rows_affected() as usize;
    }
    for r in sqlx::query("SELECT id, name FROM artist WHERE missing = 0").fetch_all(&nd).await? {
        let done = sqlx::query("INSERT OR IGNORE INTO id_override (kind, key, id) VALUES ('artist', ?, ?)")
            .bind(artist_key(&r.get::<String, _>("name")))
            .bind(r.get::<String, _>("id"))
            .execute(&mut *tx)
            .await?;
        rep.artist_ids += done.rows_affected() as usize;
    }

    // plays, ratings, stars
    for r in sqlx::query("SELECT user_id, item_id, item_type, play_count, play_date, rating, starred, starred_at FROM annotation")
        .fetch_all(&nd)
        .await?
    {
        let Some(uid) = user_map.get(&r.get::<String, _>("user_id")) else { continue };
        let kind = match r.get::<String, _>("item_type").as_str() {
            "media_file" => "track",
            "album" => "album",
            "artist" => "artist",
            _ => continue,
        };
        let starred: bool = r.get::<Option<bool>, _>("starred").unwrap_or(false);
        sqlx::query("INSERT OR REPLACE INTO annotation (user_id, item_kind, item_id, play_count, last_played, rating, starred_at) VALUES (?,?,?,?,?,?,?)")
            .bind(uid)
            .bind(kind)
            .bind(r.get::<String, _>("item_id"))
            .bind(r.get::<Option<i64>, _>("play_count").unwrap_or(0))
            .bind(nd_time(r.get("play_date")))
            .bind(r.get::<Option<i64>, _>("rating").unwrap_or(0))
            .bind(if starred { nd_time(r.get("starred_at")).or(Some(db::now())) } else { None })
            .execute(&mut *tx)
            .await?;
        rep.annotations += 1;
    }
    for r in sqlx::query("SELECT media_file_id, user_id, submission_time FROM scrobbles").fetch_all(&nd).await? {
        let Some(uid) = user_map.get(&r.get::<String, _>("user_id")) else { continue };
        sqlx::query("INSERT INTO play (user_id, track_id, played_at, client) VALUES (?, ?, ?, 'navidrome')")
            .bind(uid)
            .bind(r.get::<String, _>("media_file_id"))
            .bind(r.get::<i64, _>("submission_time"))
            .execute(&mut *tx)
            .await?;
        rep.plays += 1;
    }

    // playlists, with pictures copied into our data folder
    let pics = cfg.data_dir.join("playlist-images");
    for r in
        sqlx::query("SELECT id, name, comment, public, owner_id, rules, uploaded_image, created_at, updated_at FROM playlist")
            .fetch_all(&nd)
            .await?
    {
        let Some(owner) = user_map.get(&r.get::<String, _>("owner_id")) else { continue };
        let id: String = r.get("id");
        let mut image = None;
        if let (Some(dir), Some(file)) = (artwork, r.get::<Option<String>, _>("uploaded_image").filter(|f| !f.is_empty())) {
            let src = dir.join("playlist").join(&file);
            if src.is_file() {
                std::fs::create_dir_all(&pics)?;
                let dst = pics.join(&file);
                std::fs::copy(&src, &dst)?;
                image = Some(dst.to_string_lossy().to_string());
                rep.playlist_pictures += 1;
            }
        }
        let rules: Option<String> = r.get::<Option<String>, _>("rules").filter(|s| !s.is_empty() && s != "null");
        sqlx::query("INSERT INTO playlist (id, owner_id, name, comment, public, image_path, rules_json, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?)")
            .bind(&id)
            .bind(owner)
            .bind(r.get::<String, _>("name"))
            .bind(r.get::<Option<String>, _>("comment").unwrap_or_default())
            .bind(r.get::<bool, _>("public"))
            .bind(image)
            .bind(rules)
            .bind(nd_time(r.get("created_at")).unwrap_or_else(db::now))
            .bind(nd_time(r.get("updated_at")).unwrap_or_else(db::now))
            .execute(&mut *tx)
            .await?;
        let tracks: Vec<String> =
            sqlx::query_scalar("SELECT media_file_id FROM playlist_tracks WHERE playlist_id = ? ORDER BY id")
                .bind(&id)
                .fetch_all(&nd)
                .await?;
        for (pos, t) in tracks.iter().enumerate() {
            sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
                .bind(&id)
                .bind(pos as i64)
                .bind(t)
                .execute(&mut *tx)
                .await?;
        }
        rep.playlists += 1;
    }

    // play queues: Navidrome keeps the current song as an index into the list
    for r in sqlx::query("SELECT user_id, current, changed_by, items, position, updated_at FROM playqueue").fetch_all(&nd).await?
    {
        let Some(uid) = user_map.get(&r.get::<String, _>("user_id")) else { continue };
        let items: Vec<String> = r
            .get::<Option<String>, _>("items")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let cur_raw: Option<String> = r
            .try_get::<Option<String>, _>("current")
            .ok()
            .flatten()
            .or_else(|| r.try_get::<Option<i64>, _>("current").ok().flatten().map(|i| i.to_string()));
        let current = cur_raw.and_then(|c| match c.parse::<usize>() {
            Ok(i) if c.len() < 10 => items.get(i).cloned(),
            _ => Some(c),
        });
        sqlx::query("INSERT OR REPLACE INTO play_queue (user_id, track_ids, current, position_ms, changed_by, changed_at) VALUES (?,?,?,?,?,?)")
            .bind(uid)
            .bind(serde_json::to_string(&items)?)
            .bind(current)
            .bind(r.get::<Option<i64>, _>("position").unwrap_or(0))
            .bind(r.get::<Option<String>, _>("changed_by").unwrap_or_default())
            .bind(nd_time(r.get("updated_at")).unwrap_or_else(db::now))
            .execute(&mut *tx)
            .await?;
        rep.play_queues += 1;
    }
    for r in sqlx::query("SELECT id, name, stream_url, home_page_url FROM radio").fetch_all(&nd).await? {
        sqlx::query("INSERT INTO radio (id, name, stream_url, homepage_url) VALUES (?, ?, ?, ?)")
            .bind(r.get::<String, _>("id"))
            .bind(r.get::<String, _>("name"))
            .bind(r.get::<String, _>("stream_url"))
            .bind(r.get::<Option<String>, _>("home_page_url"))
            .execute(&mut *tx)
            .await?;
        rep.radio_stations += 1;
    }
    tx.commit().await?;
    nd.close().await;

    info!("ids carried over; scanning the music folders with them");
    let status = Arc::new(ScanStatus::default());
    library::scan(ours, libs, &status, true).await?;
    rep.songs_matched_after_scan =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track t JOIN id_override o ON o.kind = 'track' AND o.id = t.id")
            .fetch_one(ours)
            .await? as usize;
    rep.songs_not_found = rep.song_ids.saturating_sub(rep.songs_matched_after_scan);
    Ok(rep)
}
