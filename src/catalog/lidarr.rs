//! Albums Lidarr wants, from Soulseek (what Soularr did): every few minutes, take Lidarr's missing albums, search
//! slskd for "<artist> <album>", pick one user's folder that holds the whole album (as many audio files as the album
//! has tracks, FLAC or MP3 320, most track names in the file names, the artist in the path), download the folder,
//! rename it "<Artist> - <Album>" in the downloads folder and ask Lidarr to import it (DownloadedAlbumsScan). Lidarr
//! files it into its own music folder; Conductor's scan picks it up. A user whose copy fails isn't tried again.

use std::{collections::HashMap, time::Duration};

use anyhow::{Result, bail};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::{
    acquire::{Slskd, artist_in, locate, search, urlencode},
    text::{clean_album, clean_title, norm, ratio, translit},
};
use crate::{api::AppState, db::now};

const AUDIO: &[&str] = &["flac", "mp3"];

pub struct Lidarr {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl Lidarr {
    pub fn new(base: &str, key: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client");
        Self { base: format!("{}/api/v1", base.trim_end_matches('/')), key: key.into(), http }
    }
    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self.http.request(method, format!("{}{path}", self.base)).header("X-Api-Key", &self.key);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await?;
        if !resp.status().is_success() {
            bail!("Lidarr {path}: {}", resp.status());
        }
        Ok(resp.json().await.unwrap_or(Value::Null))
    }
}

#[derive(sqlx::FromRow, Debug)]
struct Grab {
    album_id: i64,
    title: String,
    artist: String,
    status: String,
    slsk_user: Option<String>,
    files_json: String,
    local_dir: Option<String>,
    command_id: Option<i64>,
    searches: i64,
    last_search: i64,
    tried_json: String,
    since: i64,
}

impl Grab {
    fn tried(&self) -> Vec<String> {
        serde_json::from_str(&self.tried_json).unwrap_or_default()
    }
    fn files(&self) -> Vec<String> {
        serde_json::from_str(&self.files_json).unwrap_or_default()
    }
}

fn safe(s: &str) -> String {
    let o: String = s.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let o = o.trim_matches([' ', '.']).to_string();
    if o.is_empty() { "_".into() } else { o }
}

/// A file name without its folder, track number and extension: "03 - Once in a Lifetime.flac" → "once in a lifetime".
fn bare(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = base.rsplit_once('.').map(|(s, _)| s).unwrap_or(base);
    let stem = stem.trim_start_matches(|c: char| c.is_ascii_digit() || " .-_".contains(c));
    norm(&clean_title(stem))
}

fn file_ok(f: &Value) -> bool {
    let name = f["filename"].as_str().unwrap_or("");
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    AUDIO.contains(&ext.as_str()) && (ext == "flac" || f["bitRate"].as_i64().unwrap_or(0) >= 310)
}

/// The best folder for an album across the search answers: (score, user, folder, files).
fn pick_folder(
    responses: &[Value],
    artist: &str,
    album: &str,
    titles: &[String],
    tried: &[String],
    need: f64,
) -> Option<(f64, String, String, Vec<Value>)> {
    let mut best: Option<(f64, String, String, Vec<Value>)> = None;
    for r in responses {
        let user = r["username"].as_str().unwrap_or("").to_string();
        if user.is_empty() || tried.contains(&user) || r["queueLength"].as_i64().unwrap_or(0) > 50 {
            continue;
        }
        let mut dirs: HashMap<String, Vec<Value>> = HashMap::new();
        for f in r["files"].as_array().into_iter().flatten() {
            let name = f["filename"].as_str().unwrap_or("").replace('\\', "/");
            let dir = name.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
            dirs.entry(dir).or_default().push(f.clone());
        }
        for (dir, files) in dirs {
            let audio: Vec<Value> = files.into_iter().filter(file_ok).collect();
            if audio.len() != titles.len() || audio.is_empty() || !artist_in(&[artist.to_string()], &dir) {
                continue;
            }
            // every track's name should be in one of the file names
            let names: Vec<String> = audio.iter().map(|f| bare(f["filename"].as_str().unwrap_or(""))).collect();
            let matched = titles
                .iter()
                .filter(|t| {
                    let want = norm(&clean_title(t));
                    names.iter().any(|n| n.contains(&want) || norm(&translit(n)).contains(&want) || ratio(&want, n) >= 0.8)
                })
                .count();
            let share = matched as f64 / titles.len() as f64;
            if share < need {
                continue;
            }
            let flac = audio.iter().all(|f| f["filename"].as_str().unwrap_or("").to_lowercase().ends_with(".flac"));
            let alb = norm(&clean_album(album));
            let mut sc = share * 50.0 + if flac { 30.0 } else { 15.0 };
            sc += if norm(&dir).contains(&alb) { 20.0 } else { 0.0 };
            sc += if r["hasFreeUploadSlot"] == true { 15.0 } else { -(r["queueLength"].as_f64().unwrap_or(0.0)) / 3.0 };
            sc += (r["uploadSpeed"].as_f64().unwrap_or(0.0) / 500_000.0).min(10.0);
            if best.as_ref().is_none_or(|b| sc > b.0) {
                best = Some((sc, user.clone(), dir, audio));
            }
        }
    }
    best
}

/// Lidarr's missing albums → new grabs to look for.
async fn take_wanted(st: &AppState, lid: &Lidarr) -> Result<()> {
    let n = st.cfg.lidarr_albums_per_run.max(1);
    let page = lid
        .call(reqwest::Method::GET, &format!("/wanted/missing?page=1&pageSize={n}&includeArtist=true&monitored=true"), None)
        .await?;
    for a in page["records"].as_array().into_iter().flatten() {
        let (Some(id), Some(title)) = (a["id"].as_i64(), a["title"].as_str()) else { continue };
        let artist = a["artist"]["artistName"].as_str().unwrap_or("");
        sqlx::query("INSERT OR IGNORE INTO lidarr_grab (album_id, title, artist) VALUES (?, ?, ?)")
            .bind(id)
            .bind(title)
            .bind(artist)
            .execute(&st.db)
            .await?;
    }
    Ok(())
}

fn retry_wait(g: &Grab) -> i64 {
    if g.searches == 0 { 0 } else { (3600 * g.searches).min(24 * 3600) }
}

async fn look_for(st: &AppState, sl: &Slskd, lid: &Lidarr, g: &Grab) -> Result<()> {
    let tracks = lid.call(reqwest::Method::GET, &format!("/track?albumId={}", g.album_id), None).await?;
    let titles: Vec<String> =
        tracks.as_array().into_iter().flatten().filter_map(|t| t["title"].as_str().map(String::from)).collect();
    sqlx::query("UPDATE lidarr_grab SET searches = searches + 1, last_search = ? WHERE album_id = ?")
        .bind(now())
        .bind(g.album_id)
        .execute(&st.db)
        .await?;
    if titles.is_empty() {
        return Ok(());
    }
    let responses = search(st, sl, &format!("{} {}", g.artist, clean_album(&g.title))).await?;
    let Some((_, user, folder, files)) =
        pick_folder(&responses, &g.artist, &g.title, &titles, &g.tried(), st.cfg.lidarr_match_ratio)
    else {
        if g.searches + 1 >= 6 {
            sqlx::query("UPDATE lidarr_grab SET status = 'notfound' WHERE album_id = ?").bind(g.album_id).execute(&st.db).await?;
        }
        info!(album = %format!("{} - {}", g.artist, g.title), "Lidarr album: no complete copy on Soulseek yet");
        return Ok(());
    };
    let body: Vec<Value> = files.iter().map(|f| json!({ "filename": f["filename"], "size": f["size"] })).collect();
    sl.call(reqwest::Method::POST, &format!("/transfers/downloads/{}", urlencode(&user)), Some(Value::Array(body))).await?;
    let names: Vec<&str> = files.iter().filter_map(|f| f["filename"].as_str()).collect();
    sqlx::query(
        "UPDATE lidarr_grab SET status = 'downloading', slsk_user = ?, folder = ?, files_json = ?, since = ? WHERE album_id = ?",
    )
    .bind(&user)
    .bind(&folder)
    .bind(serde_json::to_string(&names)?)
    .bind(now())
    .bind(g.album_id)
    .execute(&st.db)
    .await?;
    info!(album = %format!("{} - {}", g.artist, g.title), %user, "Lidarr album: downloading {} files", names.len());
    Ok(())
}

async fn fail(st: &AppState, g: &Grab, why: &str) -> Result<()> {
    let mut tried = g.tried();
    tried.extend(g.slsk_user.clone());
    sqlx::query("UPDATE lidarr_grab SET status = 'retry', tried_json = ?, last_search = 0, error = ?, command_id = NULL WHERE album_id = ?")
        .bind(serde_json::to_string(&tried)?)
        .bind(why)
        .bind(g.album_id)
        .execute(&st.db)
        .await?;
    warn!(album = %format!("{} - {}", g.artist, g.title), "Lidarr album: {why}; trying someone else");
    Ok(())
}

/// Downloads that finished → one folder, handed to Lidarr; imports that finished → done (or try someone else).
async fn follow(st: &AppState, sl: &Slskd, lid: &Lidarr) -> Result<()> {
    let Some(downloads) = st.cfg.slskd_downloads.clone() else { return Ok(()) };
    let transfers = sl.get("/transfers/downloads").await?;
    let mut state: HashMap<(String, String), (String, String)> = HashMap::new(); // (user, file) → (state, id)
    for u in transfers.as_array().into_iter().flatten() {
        let user = u["username"].as_str().unwrap_or("").to_string();
        for d in u["directories"].as_array().into_iter().flatten() {
            for f in d["files"].as_array().into_iter().flatten() {
                state.insert(
                    (user.clone(), f["filename"].as_str().unwrap_or("").to_string()),
                    (f["state"].as_str().unwrap_or("").to_string(), f["id"].as_str().unwrap_or("").to_string()),
                );
            }
        }
    }
    let grabs: Vec<Grab> =
        sqlx::query_as("SELECT * FROM lidarr_grab WHERE status IN ('downloading', 'importing')").fetch_all(&st.db).await?;
    for g in grabs {
        let user = g.slsk_user.clone().unwrap_or_default();
        if g.status == "downloading" {
            let files = g.files();
            let states: Vec<Option<&(String, String)>> = files.iter().map(|f| state.get(&(user.clone(), f.clone()))).collect();
            let bad = |s: &str| ["Errored", "Rejected", "TimedOut", "Cancelled", "Aborted"].iter().any(|x| s.contains(x));
            if states.iter().any(|s| s.is_some_and(|(st, _)| bad(st)))
                || (now() - g.since > 6 * 3600)
                || (now() - g.since > 600 && states.iter().all(|s| s.is_none()))
            {
                for (_, id) in states.iter().flatten() {
                    let _ = sl
                        .call(
                            reqwest::Method::DELETE,
                            &format!("/transfers/downloads/{}/{}?remove=true", urlencode(&user), urlencode(id)),
                            None,
                        )
                        .await;
                }
                fail(st, &g, "the download didn't finish").await?;
                continue;
            }
            if !states.iter().all(|s| s.is_some_and(|(st, _)| st.contains("Succeeded"))) {
                continue;
            }
            // all here: one folder named "<Artist> - <Album>"
            let dir_name = safe(&format!("{} - {}", g.artist, g.title));
            let dst = downloads.join(&dir_name);
            tokio::fs::create_dir_all(&dst).await?;
            for f in &files {
                if let Some(src) = locate(&downloads, f).filter(|p| !p.starts_with(&dst)) {
                    let name = src.file_name().map(|n| n.to_os_string()).unwrap_or_default();
                    if tokio::fs::rename(&src, dst.join(&name)).await.is_err() {
                        tokio::fs::copy(&src, dst.join(&name)).await?;
                        let _ = tokio::fs::remove_file(&src).await;
                    }
                }
            }
            for s in states.iter().flatten() {
                let _ = sl
                    .call(
                        reqwest::Method::DELETE,
                        &format!("/transfers/downloads/{}/{}?remove=true", urlencode(&user), urlencode(&s.1)),
                        None,
                    )
                    .await;
            }
            let lidarr_dir = match &st.cfg.lidarr_downloads {
                Some(base) => format!("{}/{dir_name}", base.trim_end_matches(['/', '\\'])),
                None => dst.to_string_lossy().to_string(),
            };
            let cmd = lid
                .call(reqwest::Method::POST, "/command", Some(json!({ "name": "DownloadedAlbumsScan", "path": lidarr_dir })))
                .await?;
            sqlx::query("UPDATE lidarr_grab SET status = 'importing', local_dir = ?, command_id = ? WHERE album_id = ?")
                .bind(&dir_name)
                .bind(cmd["id"].as_i64())
                .bind(g.album_id)
                .execute(&st.db)
                .await?;
            info!(album = %format!("{} - {}", g.artist, g.title), "Lidarr album: downloaded, Lidarr is importing it");
        } else {
            if let Some(cid) = g.command_id {
                let c = lid.call(reqwest::Method::GET, &format!("/command/{cid}"), None).await?;
                if !matches!(c["status"].as_str(), Some("completed" | "failed" | "aborted")) {
                    continue; // still importing
                }
            }
            let album = lid.call(reqwest::Method::GET, &format!("/album/{}", g.album_id), None).await?;
            if album["statistics"]["trackFileCount"].as_i64().unwrap_or(0) > 0 {
                sqlx::query("UPDATE lidarr_grab SET status = 'done', error = '' WHERE album_id = ?")
                    .bind(g.album_id)
                    .execute(&st.db)
                    .await?;
                info!(album = %format!("{} - {}", g.artist, g.title), "Lidarr album: imported");
                if let Some(d) = &g.local_dir {
                    let _ = tokio::fs::remove_dir(downloads.join(d)).await; // only if Lidarr moved everything out
                }
            } else {
                fail(st, &g, "Lidarr didn't import it").await?;
            }
        }
    }
    Ok(())
}

pub fn configured(st: &AppState) -> Option<Lidarr> {
    match (&st.cfg.lidarr_url, &st.cfg.lidarr_api_key) {
        (Some(u), Some(k)) => Some(Lidarr::new(u, k)),
        _ => None,
    }
}

/// One round: follow downloads and imports; every `lidarr_interval_min`, take new wanted albums and search for some.
pub async fn tick(st: &AppState, sl: &Slskd, lid: &Lidarr, last_wanted: &mut i64) -> Result<()> {
    follow(st, sl, lid).await?;
    let gap = (st.cfg.lidarr_interval_min.max(1) * 60) as i64;
    if !st.cfg.slskd_fast_test && now() - *last_wanted < gap {
        return Ok(());
    }
    *last_wanted = now();
    take_wanted(st, lid).await?;
    let waiting: Vec<Grab> = sqlx::query_as("SELECT * FROM lidarr_grab WHERE status = 'retry' ORDER BY last_search LIMIT ?")
        .bind(st.cfg.lidarr_albums_per_run.max(1))
        .fetch_all(&st.db)
        .await?;
    for g in waiting.into_iter().filter(|g| now() - g.last_search >= retry_wait(g)) {
        if let Err(e) = look_for(st, sl, lid, &g).await {
            warn!(album = %g.title, "Lidarr album search failed: {e:#}");
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn picks_a_whole_album_folder() {
        let titles = vec!["Born Under Punches".to_string(), "Crosseyed and Painless".to_string()];
        let f = |n: &str| json!({ "filename": n, "size": 1, "bitRate": 0 });
        let responses = vec![
            json!({ "username": "half", "hasFreeUploadSlot": true, "files": [f(r"Talking Heads\Remain in Light\01 Born Under Punches.flac")] }),
            json!({ "username": "good", "hasFreeUploadSlot": true, "files": [
                f(r"Music\Talking Heads\Remain in Light (1980)\01 - Born Under Punches.flac"),
                f(r"Music\Talking Heads\Remain in Light (1980)\02 - Crosseyed and Painless.flac"),
                f(r"Music\Talking Heads\Remain in Light (1980)\cover.jpg")] }),
            json!({ "username": "lowmp3", "files": [
                json!({ "filename": r"Talking Heads\Remain in Light\01 Born Under Punches.mp3", "bitRate": 128 }),
                json!({ "filename": r"Talking Heads\Remain in Light\02 Crosseyed and Painless.mp3", "bitRate": 128 })] }),
        ];
        let (_, user, dir, files) = pick_folder(&responses, "Talking Heads", "Remain in Light", &titles, &[], 0.8).unwrap();
        assert_eq!(user, "good");
        assert_eq!(dir, "Music/Talking Heads/Remain in Light (1980)");
        assert_eq!(files.len(), 2);
        assert!(pick_folder(&responses, "Talking Heads", "Remain in Light", &titles, &["good".into()], 0.8).is_none());
        assert_eq!(bare("03 - Once in a Lifetime.flac"), "onceinalifetime");
    }
}
