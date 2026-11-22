//! Moving over from Navidrome: `conductor import-navidrome <navidrome.db> [--artwork <navidrome data>/artwork]`.
//!
//! Reads a Navidrome database (opened read-only; use a copy) and brings over, keeping Navidrome's ids so apps
//! that remember ids keep working: users (names, admin flag and passwords: Navidrome keeps them encrypted with its
//! default key unless it was given its own, --nd-key / CONDUCTOR_ND_PASSWORD_KEY), plays, ratings,
//! stars, play history, playlists (with their smart rules and uploaded pictures), play queues and radio stations.
//! The library itself is rescanned from the music folder. Running it again replaces what it brought over before.
//!
//! `import-music-lookup <DATA>` adds music-lookup's own records: the listening log (`listening/plays.jsonl`, what
//! the stats are made from) and the values Encore keeps in step across devices (`encore/sync.json`).

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
pub struct LookupReport {
    pub plays_added: usize,
    pub plays_already_there: usize,
    pub plays_unknown_user_or_song: usize,
    pub sync_values: usize,
    pub podcasts: usize,
    pub podcast_episodes: usize,
    pub podcast_progress: usize,
    pub mix_playlists: usize,
    pub playlist_shares: usize,
    pub concerts: usize,
    /// requests.json: songs asked for (⬇), with how getting each is going
    pub requests: usize,
    pub requests_waiting: usize,
    pub requests_on_trial: usize,
    pub requests_already_there: usize,
}

/// music-lookup's play log and sync values (DATA is music-lookup's data folder, e.g. /srv/music-lookup), and its
/// podcasts (`state`: the folder with podcasts.json, e.g. spotify.py's state folder). Progress goes to every user
/// (music-lookup kept one for everyone).
pub async fn music_lookup(ours: &Db, data: &Path, state: Option<&Path>) -> Result<LookupReport> {
    let mut rep = LookupReport::default();
    let users: HashMap<String, String> =
        sqlx::query_as::<_, (String, String)>("SELECT lower(name), id FROM user").fetch_all(ours).await?.into_iter().collect();
    let mut tx = ours.begin().await?;
    if let Ok(text) = std::fs::read_to_string(data.join("listening/plays.jsonl")) {
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let (Some(t), Some(u), Some(id)) = (v["t"].as_f64(), v["u"].as_str(), v["id"].as_str()) else { continue };
            let t = t as i64;
            let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE id = ?").bind(id).fetch_one(&mut *tx).await?;
            let Some(uid) = users.get(&u.to_lowercase()).filter(|_| known > 0) else {
                rep.plays_unknown_user_or_song += 1;
                continue;
            };
            let dup: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM play WHERE user_id = ? AND track_id = ? AND played_at BETWEEN ? AND ?")
                    .bind(uid)
                    .bind(id)
                    .bind(t - 30)
                    .bind(t + 30)
                    .fetch_one(&mut *tx)
                    .await?;
            if dup > 0 {
                rep.plays_already_there += 1;
                continue;
            }
            sqlx::query("INSERT INTO play (user_id, track_id, played_at, client) VALUES (?, ?, ?, 'music-lookup')")
                .bind(uid)
                .bind(id)
                .bind(t)
                .execute(&mut *tx)
                .await?;
            rep.plays_added += 1;
        }
    }
    if let Ok(text) = std::fs::read_to_string(data.join("encore/sync.json")) {
        let all: serde_json::Value = serde_json::from_str(&text).context("encore/sync.json")?;
        for (u, vals) in all.as_object().into_iter().flatten() {
            let Some(uid) = users.get(&u.to_lowercase()) else { continue };
            for (key, v) in vals.as_object().into_iter().flatten() {
                sqlx::query(
                    "INSERT INTO sync_kv (user_id, key, value_json, at, by_device) VALUES (?, ?, ?, ?, ?)
                     ON CONFLICT(user_id, key) DO UPDATE SET value_json = excluded.value_json, at = excluded.at, by_device = excluded.by_device
                     WHERE excluded.at >= sync_kv.at",
                )
                .bind(uid)
                .bind(key)
                .bind(v["value"].to_string())
                .bind(v["at"].as_i64().unwrap_or(0))
                .bind(v["by"].as_str().unwrap_or(""))
                .execute(&mut *tx)
                .await?;
                rep.sync_values += 1;
            }
        }
    }
    if let Some(text) = state.and_then(|s| std::fs::read_to_string(s.join("podcasts.json")).ok()) {
        let d: serde_json::Value = serde_json::from_str(&text).context("podcasts.json")?;
        let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or("").to_string();
        let n = |v: &serde_json::Value, k: &str| v[k].as_f64().unwrap_or(0.0) as i64;
        for c in d["channels"].as_object().into_iter().flatten().map(|(_, v)| v) {
            sqlx::query("INSERT OR REPLACE INTO podcast (id, url, title, author, description, image, added_at, checked_at, error) VALUES (?,?,?,?,?,?,?,?,?)")
                .bind(s(c, "id")).bind(s(c, "url")).bind(s(c, "title")).bind(s(c, "author")).bind(s(c, "description"))
                .bind(s(c, "image")).bind(n(c, "added")).bind(n(c, "checked")).bind(s(c, "error"))
                .execute(&mut *tx).await?;
            rep.podcasts += 1;
        }
        for e in d["episodes"].as_object().into_iter().flatten().map(|(_, v)| v) {
            sqlx::query(
                "INSERT OR REPLACE INTO episode (id, podcast_id, guid, title, description, published_at, duration, url, size, content_type, image, file, pinned)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            )
            .bind(s(e, "id")).bind(s(e, "channel")).bind(s(e, "guid")).bind(s(e, "title")).bind(s(e, "description"))
            .bind(n(e, "published")).bind(n(e, "duration")).bind(s(e, "url")).bind(n(e, "size"))
            .bind(Some(s(e, "type")).filter(|t| !t.is_empty()).unwrap_or_else(|| "audio/mpeg".into())).bind(s(e, "image"))
            .bind(e["file"].as_str()).bind(e["pinned"].as_bool().unwrap_or(false))
            .execute(&mut *tx).await?;
            rep.podcast_episodes += 1;
        }
        for (eid, p) in d["progress"].as_object().into_iter().flatten() {
            for uid in users.values() {
                sqlx::query(
                    "INSERT OR REPLACE INTO podcast_progress (user_id, episode_id, position_ms, done, at) VALUES (?,?,?,?,?)",
                )
                .bind(uid)
                .bind(eid)
                .bind(n(p, "pos"))
                .bind(p["done"].as_bool().unwrap_or(false))
                .bind(n(p, "at"))
                .execute(&mut *tx)
                .await?;
            }
            rep.podcast_progress += 1;
        }
    }
    tx.commit().await?;
    extras(ours, data, &users, &mut rep).await?;
    if let Some(text) = state.and_then(|s| std::fs::read_to_string(s.join("requests.json")).ok()) {
        let d: serde_json::Value = serde_json::from_str(&text).context("requests.json")?;
        requests(ours, &d, &mut rep).await?;
    }
    Ok(rep)
}

/// music-lookup's requests.json: the songs asked for that it was getting or had got (Deezer "dz-t-…" keys, as
/// Conductor's), so downloads in progress go on, songs on trial keep their clock and "On trial" playlist, and a ⬇
/// song asked for again isn't fetched twice. music-lookup kept them for one user: the owner of its "On trial"
/// playlist, else the first admin. Requests Conductor already has are left as they are.
async fn requests(ours: &Db, d: &serde_json::Value, rep: &mut LookupReport) -> Result<()> {
    let owner_of = async |pid: &str| -> Result<Option<String>> {
        Ok(sqlx::query_scalar("SELECT owner_id FROM playlist WHERE id = ?").bind(pid).fetch_optional(ours).await?)
    };
    let trial_pl = d["meta"]["trial_playlist"].as_str().unwrap_or("");
    let user: Option<String> = match owner_of(trial_pl).await? {
        Some(u) => Some(u),
        None => sqlx::query_scalar("SELECT id FROM user WHERE is_admin = 1 ORDER BY rowid LIMIT 1").fetch_optional(ours).await?,
    };
    let Some(user) = user else { return Ok(()) };
    let mut tx = ours.begin().await?;
    if !trial_pl.is_empty() && owner_of(trial_pl).await?.is_some() {
        // the same "On trial" playlist goes on (it came over from Navidrome), not a second one
        sqlx::query("INSERT OR REPLACE INTO setting (key, value) VALUES (?, ?)")
            .bind(format!("trial_playlist:{user}"))
            .bind(trial_pl)
            .execute(&mut *tx)
            .await?;
    }
    let s = |v: &serde_json::Value, k: &str| v[k].as_str().map(String::from);
    let n = |v: &serde_json::Value, k: &str| v[k].as_f64().map(|x| x as i64);
    for (key, t) in d["tracks"].as_object().into_iter().flatten() {
        if !key.starts_with("dz-t-") {
            continue;
        }
        let e = &d["state"][key];
        let w = &d["want"][key];
        let rel = s(e, "rel");
        // the library song: the file music-lookup filed, else the id it knew (kept by import-navidrome)
        let mut local: Option<String> = None;
        if let Some(r) = &rel {
            local = sqlx::query_scalar("SELECT id FROM track WHERE rel_path = ? AND missing_since IS NULL")
                .bind(r.replace('\\', "/"))
                .fetch_optional(&mut *tx)
                .await?;
        }
        if local.is_none()
            && let Some(id) = d["local"][key].as_str()
        {
            local = sqlx::query_scalar("SELECT id FROM track WHERE id = ? AND missing_since IS NULL")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        }
        let status = match e["status"].as_str() {
            Some("queued") => "queued",
            Some("done") | Some("upgrading") => "done", // an upgrade under way is dropped: the copy you have stays
            Some("notfound") => "notfound",
            _ => "retry",
        };
        let rel = rel.map(|r| r.replace('\\', "/"));
        let folder = s(e, "folder")
            .or_else(|| rel.as_deref().and_then(|r| r.split('/').next()).map(String::from))
            .unwrap_or_else(|| "Songs".into());
        let trial = w["trial"].as_bool().unwrap_or(false);
        let artists: Vec<String> =
            t["artists"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(String::from)).collect();
        let done = sqlx::query(
            "INSERT INTO request (key, title, artists_json, album, duration_ms, isrc, folder, status, slsk_user, slsk_file,
                searches, last_search, tried_json, found, since, rel, local_id, trial, trial_at, arrived_at, asked_at,
                trial_user, keep, kept_at, listed, mix, trial_hours, same_album, upgrade_tried)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(key) DO NOTHING",
        )
        .bind(key)
        .bind(s(t, "name").unwrap_or_default())
        .bind(serde_json::to_string(&artists)?)
        .bind(s(t, "album").unwrap_or_default())
        .bind(n(t, "ms").unwrap_or(0))
        .bind(s(t, "isrc"))
        .bind(&folder)
        .bind(status)
        .bind(s(e, "user"))
        .bind(s(e, "file"))
        .bind(n(e, "searches").unwrap_or(0))
        .bind(n(e, "last").unwrap_or(0))
        .bind(e["tried"].as_array().map(|a| serde_json::Value::from(a.clone()).to_string()).unwrap_or("[]".into()))
        .bind(e["found"].as_bool().unwrap_or(false))
        .bind(n(e, "since").unwrap_or(0))
        .bind(&rel)
        .bind(&local)
        .bind(trial)
        .bind(n(w, "trial_at"))
        .bind(n(w, "arrived"))
        .bind(n(w, "asked").unwrap_or_else(crate::db::now))
        .bind((trial || w["trial_at"].is_number()).then_some(&user))
        .bind(w["keep"].as_bool().unwrap_or(false))
        .bind(n(w, "kept"))
        .bind(w["listed"].as_bool().unwrap_or(false))
        .bind(s(w, "mix"))
        .bind(w["trial_hours"].as_f64())
        .bind(e["same_album"].as_bool())
        .bind(e["upgrade_tried"].as_bool().unwrap_or(false))
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() == 0 {
            rep.requests_already_there += 1;
            continue;
        }
        rep.requests += 1;
        if local.is_none() && status != "notfound" {
            rep.requests_waiting += 1;
        }
        if trial {
            rep.requests_on_trial += 1;
        }
        // what to do once it's there: heart it, add it to playlists (each playlist for its owner)
        let applied = w["applied"].is_number() && local.is_some();
        let mut wants: Vec<(String, String)> = vec![];
        if w["star"].as_bool().unwrap_or(false) {
            wants.push((user.clone(), String::new()));
        }
        for pid in w["playlists"].as_array().into_iter().flatten().filter_map(|p| p.as_str()) {
            if let Some(o) = owner_of(pid).await? {
                wants.push((o, pid.to_string()));
            }
        }
        for (uid, pid) in wants {
            sqlx::query("INSERT OR IGNORE INTO request_want (key, user_id, star, playlist_id, done) VALUES (?, ?, ?, ?, ?)")
                .bind(key)
                .bind(&uid)
                .bind(pid.is_empty())
                .bind(&pid)
                .bind(applied)
                .execute(&mut *tx)
                .await?;
        }
    }
    tx.commit().await?;
    info!(requests = rep.requests, waiting = rep.requests_waiting, on_trial = rep.requests_on_trial, "music-lookup's requests");
    Ok(())
}

/// music-lookup's mixes (so its Discover Weekly, Daily Mix … playlists, brought over from Navidrome, go on being
/// the mixes instead of a second set appearing), its shared-playlist links (so friends' links keep working) and
/// the concerts it found (so shows already seen aren't "new" again). music-lookup made these for one user: the
/// owner of its mix playlists, else the first admin.
async fn extras(ours: &Db, data: &Path, users: &HashMap<String, String>, rep: &mut LookupReport) -> Result<()> {
    let read = |p: &str| -> Option<serde_json::Value> { serde_json::from_str(&std::fs::read_to_string(data.join(p)).ok()?).ok() };
    let first_admin: Option<String> =
        sqlx::query_scalar("SELECT id FROM user WHERE is_admin = 1 ORDER BY rowid LIMIT 1").fetch_optional(ours).await?;
    let mut owner = first_admin.clone();
    if let Some(m) = read("mixes/state.json") {
        let slots: Vec<(String, String)> = m["playlists"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_str().filter(|v| !v.is_empty()).map(|v| (k.clone(), v.to_string())))
            .collect();
        for (_, pid) in &slots {
            if let Some(o) = sqlx::query_scalar::<_, String>("SELECT owner_id FROM playlist WHERE id = ?")
                .bind(pid)
                .fetch_optional(ours)
                .await?
            {
                owner = Some(o);
                break;
            }
        }
        if let Some(uid) = &owner {
            for kind in crate::mixes::KINDS {
                let list = |k: &str| {
                    m[k][*kind].as_array().map(|a| serde_json::Value::from(a.clone()).to_string()).unwrap_or("[]".into())
                };
                sqlx::query(
                    "INSERT OR REPLACE INTO mix_state (user_id, kind, enabled, last_at, current_json, history_json) VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(uid)
                .bind(kind)
                .bind(m["enabled"][*kind].as_bool().unwrap_or(true))
                .bind(m["last"][*kind].as_f64().map(|t| t as i64))
                .bind(list("current"))
                .bind(list("history"))
                .execute(ours)
                .await?;
            }
            for (slot, pid) in slots {
                let exists: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM playlist WHERE id = ?").bind(&pid).fetch_one(ours).await?;
                if exists == 0 {
                    continue;
                }
                let about = &m["about"][&slot];
                sqlx::query("INSERT OR REPLACE INTO mix_playlist (user_id, slot, playlist_id, song_count, made_at) VALUES (?, ?, ?, ?, ?)")
                    .bind(uid)
                    .bind(&slot)
                    .bind(&pid)
                    .bind(about["count"].as_i64().unwrap_or(0))
                    .bind(about["at"].as_f64().unwrap_or(0.0) as i64)
                    .execute(ours)
                    .await?;
                rep.mix_playlists += 1;
            }
        }
    }
    if let Some(sh) = read("encore/playlist-shares.json") {
        for (token, v) in sh.as_object().into_iter().flatten() {
            let (Some(pid), Some(uid)) = (v["playlist"].as_str(), v["owner"].as_str().and_then(|o| users.get(&o.to_lowercase())))
            else {
                continue;
            };
            let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist WHERE id = ?").bind(pid).fetch_one(ours).await?;
            if exists == 0 {
                continue;
            }
            sqlx::query("DELETE FROM playlist_share WHERE token = ? OR playlist_id = ?")
                .bind(token)
                .bind(pid)
                .execute(ours)
                .await?;
            sqlx::query("INSERT INTO playlist_share (token, playlist_id, owner_id, created_at) VALUES (?, ?, ?, ?)")
                .bind(token)
                .bind(pid)
                .bind(uid)
                .bind(v["created"].as_f64().unwrap_or(0.0) as i64)
                .execute(ours)
                .await?;
            for a in v["added"].as_array().into_iter().flatten() {
                sqlx::query("INSERT INTO playlist_share_add (token, track_id, by_name, guest, added_at) VALUES (?, ?, ?, ?, ?)")
                    .bind(token)
                    .bind(a["id"].as_str().unwrap_or(""))
                    .bind(a["by"].as_str().unwrap_or(""))
                    .bind(a["guest"].as_str().unwrap_or(""))
                    .bind(a["at"].as_i64().unwrap_or(0))
                    .execute(ours)
                    .await?;
            }
            rep.playlist_shares += 1;
        }
    }
    if let (Some(c), Some(uid)) = (read("concerts/concerts.json"), owner.as_ref()) {
        let shows = c["show"].as_array().cloned().unwrap_or_default();
        sqlx::query(
            "INSERT OR REPLACE INTO concerts (user_id, shows_json, first_json, city, source, prefs, at) VALUES (?, ?, ?, ?, ?, '', ?)",
        )
        .bind(uid)
        .bind(serde_json::Value::from(shows.clone()).to_string())
        .bind(c["first"].to_string())
        .bind(c["city"].as_str().unwrap_or(""))
        .bind(c["source"].as_str().unwrap_or(""))
        .bind(c["at"].as_f64().unwrap_or(0.0) as i64)
        .execute(ours)
        .await?;
        rep.concerts = shows.len();
    }
    Ok(())
}

/// Navidrome's password key when none is set (its PasswordEncryptionKey default, a constant in its public source).
const ND_DEFAULT_KEY: &str = "just for obfuscation";
/// Passwords Navidrome made up itself (e.g. for reverse-proxy sign-in) start with this: nothing to carry over.
const ND_AUTOGEN: &str = "__NAVIDROME_AUTOGEN__";

/// The AES key Navidrome encrypts passwords with: SHA-256 of its PasswordEncryptionKey (or of the default).
pub fn nd_key(custom: Option<&str>) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(custom.filter(|k| !k.is_empty()).unwrap_or(ND_DEFAULT_KEY).as_bytes()).into()
}

/// A password as Navidrome keeps it (base64 of a 12-byte nonce, then the AES-256-GCM ciphertext and tag), opened
/// with `key`. Only in memory, wiped when dropped; None when it doesn't open (another key) or is one Navidrome made up.
pub fn nd_password(key: &[u8; 32], stored: &str) -> Option<zeroize::Zeroizing<String>> {
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD.decode(stored.trim()).ok()?;
    if raw.len() <= 12 + 16 {
        return None;
    }
    let (nonce, ct) = raw.split_at(12);
    let plain = zeroize::Zeroizing::new(Aes256Gcm::new(key.into()).decrypt(Nonce::from_slice(nonce), ct).ok()?);
    let text = zeroize::Zeroizing::new(String::from_utf8(plain.to_vec()).ok()?);
    (!text.is_empty() && !text.starts_with(ND_AUTOGEN)).then_some(text)
}

#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    /// users whose password couldn't be carried over: set it again (`conductor user passwd NAME`)
    pub users: Vec<String>,
    /// users whose Navidrome password works here too
    pub passwords_carried: usize,
    pub users_imported: usize,
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
    nd_password_key: Option<&str>,
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

    // users: same ids, and the same passwords (Navidrome keeps them encrypted with a key we know, unless it was given
    // its own: then pass it with --nd-key / CONDUCTOR_ND_PASSWORD_KEY). One that won't open gets a random password,
    // to be set again. The passwords are never shown or logged.
    let key = nd_key(nd_password_key);
    let users = sqlx::query("SELECT id, user_name, is_admin, created_at, password FROM user").fetch_all(&nd).await?;
    let mut user_map: HashMap<String, String> = HashMap::new();
    for u in users {
        let (id, name): (String, String) = (u.get("id"), u.get("user_name"));
        let existing: Option<String> =
            sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(&name).fetch_optional(&mut *tx).await?;
        let mine = match existing {
            Some(e) => e,
            None => {
                let stored: Option<String> = u.try_get("password").ok().flatten();
                let enc = match stored.as_deref().and_then(|s| nd_password(&key, s)) {
                    Some(pw) => {
                        rep.passwords_carried += 1;
                        secrets.encrypt(&pw)
                    }
                    None => {
                        rep.users.push(name.clone());
                        secrets.encrypt(&db::random_id())
                    }
                };
                sqlx::query("INSERT INTO user (id, name, password_enc, is_admin, created_at) VALUES (?, ?, ?, ?, ?)")
                    .bind(&id)
                    .bind(&name)
                    .bind(enc)
                    .bind(u.get::<bool, _>("is_admin"))
                    .bind(nd_time(u.get("created_at")).unwrap_or_else(db::now))
                    .execute(&mut *tx)
                    .await?;
                rep.users_imported += 1;
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
    // Albums by what their songs say (Navidrome groups songs by each song's joined album artist + album tag; the
    // album row's own artist can differ, e.g. "Various Artists" for a compilation whose songs name an artist).
    for r in sqlx::query(
        "SELECT a.id,
            COALESCE((SELECT mf.album_artist FROM media_file mf WHERE mf.album_id = a.id
                      GROUP BY mf.album_artist, mf.album ORDER BY COUNT(*) DESC LIMIT 1), a.album_artist) AS album_artist,
            COALESCE((SELECT mf.album FROM media_file mf WHERE mf.album_id = a.id
                      GROUP BY mf.album_artist, mf.album ORDER BY COUNT(*) DESC LIMIT 1), a.name) AS name
         FROM album a WHERE a.missing = 0 ORDER BY a.song_count DESC",
    )
    .fetch_all(&nd)
    .await?
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
    // when each song and album was added, set after the rescan below (which would otherwise use file times)
    let added_songs: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, created_at FROM media_file").fetch_all(&nd).await?;
    let added_albums: Vec<(String, Option<String>)> = sqlx::query_as("SELECT id, created_at FROM album").fetch_all(&nd).await?;
    nd.close().await;

    info!("ids carried over; scanning the music folders with them");
    let status = Arc::new(ScanStatus::default());
    library::scan(ours, libs, &status, true).await?;
    let mut tx = ours.begin().await?;
    for (table, rows) in [("track", &added_songs), ("album", &added_albums)] {
        for (id, at) in rows {
            if let Some(t) = nd_time(at.clone()) {
                sqlx::query(&format!("UPDATE {table} SET created_at = ? WHERE id = ?"))
                    .bind(t)
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
    }
    tx.commit().await?;
    rep.songs_matched_after_scan =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track t JOIN id_override o ON o.kind = 'track' AND o.id = t.id")
            .fetch_one(ours)
            .await? as usize;
    rep.songs_not_found = rep.song_ids.saturating_sub(rep.songs_matched_after_scan);
    Ok(rep)
}

#[cfg(test)]
mod password_tests {
    use super::*;

    // Made by a real Navidrome 0.64 (createAdmin) with test-only passwords: its default key, and its own key.
    const DEFAULT: &str = "51Sxz9molEEEoDcirGPBQkUe5GtQQ43ss2tnvqJ5WEXzYWXvGLcKbRfl1Rwo";
    const CUSTOM: &str = "aKRq7AgwSj43+IBFsufgk+3D0bpQESEx8pqC+qdXjUnBK0WAd7Z+X0QbdbY=";
    const CUSTOM_KEY: &str = "test-only-custom-key";

    #[test]
    fn navidromes_passwords() {
        assert_eq!(nd_password(&nd_key(None), DEFAULT).as_deref().map(String::as_str), Some("test-pw-default-1"));
        assert_eq!(nd_password(&nd_key(Some(CUSTOM_KEY)), CUSTOM).as_deref().map(String::as_str), Some("test-pw-custom-2"));
        assert!(nd_password(&nd_key(None), CUSTOM).is_none(), "the wrong key doesn't open it");
        assert!(nd_password(&nd_key(Some("other")), DEFAULT).is_none());
        assert!(nd_password(&nd_key(None), "not base64!").is_none() && nd_password(&nd_key(None), "").is_none());
        // the check Navidrome keeps for its own key (property PasswordsEncryptedKey) is the SHA-256 of our key
        use sha2::{Digest, Sha256};
        assert_eq!(hex::encode(Sha256::digest(nd_key(Some(CUSTOM_KEY)))), "4542de552716c4147253ceb36e83d900474fa98985ce7fe4ce2eab0f92c5d5dd");
        // one Navidrome made up itself isn't carried over
        use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
        use base64::Engine;
        let nonce = [7u8; 12];
        let mut raw = nonce.to_vec();
        raw.extend(Aes256Gcm::new((&nd_key(None)).into()).encrypt(Nonce::from_slice(&nonce), format!("{ND_AUTOGEN}x").as_bytes()).unwrap());
        assert!(nd_password(&nd_key(None), &base64::engine::general_purpose::STANDARD.encode(raw)).is_none());
    }
}
