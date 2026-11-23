//! Getting songs that were asked for: search Soulseek through slskd (one search every 12 s at most), pick the best
//! file (the same album first, then FLAC over 320 kbit/s, right title, artist and length, no live/remix/cover
//! versions unless asked), download it, file it as `<folder>/<Artist>/<Artist> - <Title>.<ext>` in the music
//! folder, scan, then heart it / add it to playlists for whoever asked.
//!
//! Upgrades: when there's nothing else to do, a song it filed from another album than the one it was asked from
//! is looked for once more on that album, and a lossy one from the right album as FLAC. A better copy replaces the
//! old file safely (checked first, swapped in while scans wait, old file removed last) and the song keeps its id,
//! hearts, plays and playlists. Only songs in `Songs/` (filed by Conductor), never Lidarr's album folders.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tracing::{info, warn};

use super::text::{VERSION_WORDS, clean_album, clean_title, loose, norm, ratio, translit};
use crate::{api::AppState, db::now, library};

const SEARCH_GAP_SECS: u64 = 12;
const SEARCH_WAIT_MS: i64 = 8000;
const IN_FLIGHT: i64 = 40;
const AUDIO: &[&str] = &["flac", "mp3", "m4a", "ogg", "opus", "aac"];

pub fn is_audio(ext: &str) -> bool {
    AUDIO.contains(&ext)
}

pub struct Slskd {
    base: String,
    key: String,
    http: reqwest::Client,
}

impl Slskd {
    pub fn new(base: &str, key: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("http client");
        Self { base: format!("{}/api/v0", base.trim_end_matches('/')), key: key.to_string(), http }
    }
    pub(crate) async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut r = self.http.request(method, format!("{}{path}", self.base)).header("X-API-Key", &self.key);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let resp = r.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            bail!("slskd {path}: {status}");
        }
        Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) })
    }
    pub(crate) async fn get(&self, path: &str) -> Result<Value> {
        self.call(reqwest::Method::GET, path, None).await
    }
    pub async fn logged_in(&self) -> bool {
        self.get("/application").await.ok().is_some_and(|v| v["server"]["isLoggedIn"] == true)
    }
}

/// A requested song, in the shape the matching needs.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Req {
    pub key: String,
    pub title: String,
    pub artists_json: String,
    pub album: String,
    pub duration_ms: i64,
    pub folder: String,
    pub status: String,
    pub slsk_user: Option<String>,
    pub slsk_file: Option<String>,
    pub searches: i64,
    pub last_search: i64,
    pub tried_json: String,
    pub found: bool,
    pub since: i64,
}

impl Req {
    pub fn artists(&self) -> Vec<String> {
        serde_json::from_str(&self.artists_json).unwrap_or_default()
    }
    pub fn tried(&self) -> Vec<String> {
        serde_json::from_str(&self.tried_json).unwrap_or_default()
    }
}

fn artist_variants(artists: &[String]) -> HashSet<String> {
    let split = regex::Regex::new(r"(?i)\s*[,&/]\s*|\s+and\s+|\s+(?:feat|ft)\.?\s+").unwrap();
    let suffix = regex::Regex::new(r"(?i)^(jr\.?|sr\.?|iii?)$").unwrap();
    let mut out = HashSet::new();
    for a in artists {
        for v in [a.clone(), translit(a)] {
            let n = loose(&norm(&v));
            if n.chars().count() >= 2 {
                out.insert(n);
            }
            out.insert(loose(&norm(&v.replace('&', " and "))));
            for name in split.split(&v) {
                let words: Vec<&str> = name.split_whitespace().filter(|w| !suffix.is_match(w)).collect();
                if let Some(last) = words.last()
                    && norm(last).chars().count() >= 5
                {
                    out.insert(loose(&norm(last))); // one-word name, or a surname ("Dylan", "Dominos")
                }
            }
        }
    }
    out.retain(|x| !x.is_empty());
    out
}

/// Is one of the song's artists named in `text` (a file path), in any alphabet or spelling?
pub(crate) fn artist_in(artists: &[String], text: &str) -> bool {
    let hay = [loose(&norm(text)), loose(&norm(&translit(text)))];
    artist_variants(artists).iter().any(|v| hay.iter().any(|h| !h.is_empty() && h.contains(v.as_str())))
}

/// Is this file from the album the song was asked from? None: the album is named like the song (any copy will do).
pub fn same_album(r: &Req, path: &str) -> Option<bool> {
    let alb = norm(&clean_album(&r.album));
    if alb.chars().count() < 3 || alb == norm(&clean_title(&r.title)) {
        return None;
    }
    let p = path.replace('\\', "/");
    if norm(&p).contains(&alb) || norm(&translit(&p)).contains(&alb) || loose(&norm(&translit(&p))).contains(&loose(&alb)) {
        return Some(true);
    }
    let paren = regex::Regex::new(r"[(\[].*?[)\]]").unwrap();
    let year = regex::Regex::new(r"^\s*\d{4}\s*").unwrap();
    let parts: Vec<&str> = p.split('/').collect();
    for seg in &parts[..parts.len().saturating_sub(1)] {
        let seg = paren.replace_all(seg, "").to_string();
        let mut cands = vec![seg.clone()];
        cands.extend(seg.split(" - ").map(String::from));
        for c in cands {
            let c = norm(&year.replace(&c, ""));
            if c.chars().count() >= 4 && ratio(&alb, &c) >= 0.8 {
                return Some(true);
            }
        }
    }
    Some(false)
}

/// Higher is better; None: not this song.
pub fn score_file(r: &Req, resp: &Value, f: &Value, tried: &[String]) -> Option<f64> {
    let user = resp["username"].as_str()?;
    if tried.iter().any(|t| t == user) {
        return None;
    }
    let name = f["filename"].as_str()?.replace('\\', "/");
    let base = name.rsplit('/').next().unwrap_or(&name).to_string();
    let ext = f["extension"]
        .as_str()
        .filter(|e| !e.is_empty())
        .map(String::from)
        .unwrap_or_else(|| base.rsplit('.').next().unwrap_or("").to_string());
    let ext = ext.trim_start_matches('.').to_lowercase();
    if !AUDIO.contains(&ext.as_str()) {
        return None;
    }
    let br = f["bitRate"].as_i64().unwrap_or(0);
    let mut q = if ext == "flac" {
        30.0
    } else if br >= 310 {
        20.0
    } else if br >= 250 {
        12.0
    } else if ext != "mp3" && br >= 192 {
        8.0
    } else {
        return None;
    };
    let title = norm(&clean_title(&r.title));
    if title.is_empty() || (!norm(&base).contains(&title) && !norm(&translit(&base)).contains(&title)) {
        return None; // the title must be in the file name itself
    }
    if !artist_in(&r.artists(), &name) {
        return None; // ...and an artist somewhere in its path
    }
    let (low, want) = (base.to_lowercase(), r.title.to_lowercase());
    if VERSION_WORDS.iter().any(|w| low.contains(w) && !want.contains(w)) {
        return None;
    }
    if let Some(len) = f["length"].as_f64()
        && r.duration_ms > 0
    {
        let diff = (len - r.duration_ms as f64 / 1000.0).abs();
        if diff > 8.0 {
            return None;
        }
        q += 5.0 - diff / 2.0;
    }
    q += if resp["hasFreeUploadSlot"] == true { 15.0 } else { -(resp["queueLength"].as_f64().unwrap_or(0.0).min(50.0)) / 3.0 };
    q += (resp["uploadSpeed"].as_f64().unwrap_or(0.0) / 500_000.0).min(10.0);
    if same_album(r, &name) == Some(true) {
        q += 40.0; // the same version as asked for beats speed and format
    }
    Some(q)
}

pub(crate) async fn search(st: &AppState, sl: &Slskd, text: &str) -> Result<Vec<Value>> {
    // one search at a time, at most one every 12 s: much faster and the Soulseek server ignores you
    let _turn = st.search_pace.lock().await;
    if crate::vpn::paused(st) {
        bail!("the VPN is down: downloads wait"); // the kill switch, checked before each search (it can go down mid-round)
    }
    let id = crate::db::random_id();
    let id = format!("{}-{}-{}-{}-{}", &id[..8], &id[8..12], &id[12..16], &id[16..20], &id[20..32]);
    let mut started = false;
    for attempt in 0..6 {
        let body = json!({ "id": id, "searchText": text, "searchTimeout": SEARCH_WAIT_MS, "responseLimit": 150, "fileLimit": 20000,
                           "filterResponses": true, "minimumResponseFileCount": 1 });
        match sl.call(reqwest::Method::POST, "/searches", Some(body)).await {
            Ok(_) => {
                started = true;
                break;
            }
            Err(e) if attempt < 5 => {
                warn!("Soulseek search didn't start ({e}); trying again");
                tokio::time::sleep(Duration::from_secs(10 * (attempt + 1))).await;
            }
            Err(e) => return Err(e),
        }
    }
    if !started {
        bail!("Soulseek search didn't start");
    }
    let t0 = std::time::Instant::now();
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(if st.cfg.slskd_fast_test { 20 } else { 1000 })).await;
        if sl.get(&format!("/searches/{id}")).await.ok().is_some_and(|v| v["isComplete"] == true) {
            break;
        }
    }
    let res = sl.get(&format!("/searches/{id}/responses")).await.unwrap_or(Value::Null);
    let _ = sl.call(reqwest::Method::DELETE, &format!("/searches/{id}"), None).await;
    let gap = Duration::from_secs(if st.cfg.slskd_fast_test { 0 } else { SEARCH_GAP_SECS });
    if let Some(rest) = gap.checked_sub(t0.elapsed()) {
        tokio::time::sleep(rest).await;
    }
    Ok(res.as_array().cloned().unwrap_or_default())
}

/// Search Soulseek for one song: the best file of each user who has it (score, user, file), best first. The
/// album search is tried only when the first search had nothing from that album.
async fn gather(st: &AppState, sl: &Slskd, r: &Req, tried: &[String]) -> Result<Vec<(f64, String, Value)>> {
    let artists = r.artists();
    let (title, artist) = (clean_title(&r.title), artists.first().cloned().unwrap_or_default());
    let album = clean_album(&r.album);
    let mut queries = vec![Some(format!("{artist} {title}"))];
    let album_q = if same_album(r, &album).is_some() { Some(format!("{artist} {album} {title}")) } else { None };
    queries.push(album_q.clone());
    queries.push(if norm(&title).chars().count() >= 6 { Some(title.clone()) } else { None });
    let mut cands: Vec<(f64, String, Value)> = vec![];
    let last = queries.len() - 1;
    for (i, text) in queries.into_iter().enumerate() {
        let Some(text) = text else { continue };
        if Some(&text) == album_q.as_ref()
            && cands.iter().any(|c| same_album(r, c.2["filename"].as_str().unwrap_or("")) == Some(true))
        {
            continue;
        }
        if !cands.is_empty() && i == last {
            break;
        }
        for resp in search(st, sl, &text).await? {
            let mut best_here: Option<(f64, String, Value)> = None; // best file per user
            for f in resp["files"].as_array().into_iter().flatten() {
                if let Some(sc) = score_file(r, &resp, f, tried)
                    && best_here.as_ref().is_none_or(|b| sc > b.0)
                {
                    best_here = Some((sc, resp["username"].as_str().unwrap_or("").to_string(), f.clone()));
                }
            }
            cands.extend(best_here);
        }
        if cands.iter().any(|c| same_album(r, c.2["filename"].as_str().unwrap_or("")) == Some(true)) {
            break;
        }
    }
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(cands)
}

/// Search for one song and queue the best file. Returns a short note.
async fn find_and_queue(st: &AppState, sl: &Slskd, r: &Req) -> Result<String> {
    let tried = r.tried();
    let cands = gather(st, sl, r, &tried).await?;
    let searches = r.searches + 1;
    sqlx::query("UPDATE request SET searches = ?, last_search = ? WHERE key = ?")
        .bind(searches)
        .bind(now())
        .bind(&r.key)
        .execute(&st.db)
        .await?;
    let mut tried = tried;
    for (_, user, f) in cands.iter().take(5) {
        if crate::vpn::paused(st) {
            return Ok("the VPN went down: it's fetched later".into());
        }
        let body = json!([{ "filename": f["filename"], "size": f["size"] }]);
        let path = format!("/transfers/downloads/{}", urlencode(user));
        if sl.call(reqwest::Method::POST, &path, Some(body)).await.is_err() {
            tried.push(user.clone()); // unreachable: take the next best
            continue;
        }
        let file = f["filename"].as_str().unwrap_or("").to_string();
        sqlx::query(
            "UPDATE request SET status = 'queued', slsk_user = ?, slsk_file = ?, since = ?, tried_json = ?, same_album = ? WHERE key = ?",
        )
        .bind(user)
        .bind(&file)
        .bind(now())
        .bind(serde_json::to_string(&tried)?)
        .bind(same_album(r, &file))
        .bind(&r.key)
        .execute(&st.db)
        .await?;
        return Ok(format!("{user}: {}", file.replace('\\', "/").rsplit('/').next().unwrap_or("")));
    }
    let found = r.found || !cands.is_empty() || !tried.is_empty(); // it exists; the people sharing it just didn't send it yet
    let limit = if found { 8 } else { 4 };
    let status = if searches >= limit { "notfound" } else { "retry" };
    sqlx::query("UPDATE request SET status = ?, found = ?, tried_json = ? WHERE key = ?")
        .bind(status)
        .bind(found)
        .bind(serde_json::to_string(&tried)?)
        .bind(&r.key)
        .execute(&st.db)
        .await?;
    Ok(if cands.is_empty() { "not found".into() } else { "found, but nobody reachable right now (will retry)".into() })
}

pub(crate) fn urlencode(s: &str) -> String {
    form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

/// Seconds to wait before searching for a song again: 30 min, then longer (people come online at other times).
fn retry_wait(r: &Req) -> i64 {
    if r.status != "retry" || r.searches == 0 { 0 } else { (1800 * (r.searches - 1).max(1)).min(4 * 3600) }
}

fn safe(s: &str) -> String {
    let o: String = s.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) || c.is_control() { '_' } else { c }).collect();
    let o: String = o.trim_matches([' ', '.']).chars().take(120).collect();
    if o.is_empty() { "_".into() } else { o }
}

/// The finished file in slskd's downloads folder. The name is the peer's: a ".." in it never leads outside.
pub(crate) fn locate(downloads: &Path, remote: &str) -> Option<PathBuf> {
    let parts: Vec<&str> = remote.split(['\\', '/']).collect();
    let name = parts.last().filter(|n| !matches!(**n, "" | "." | ".."))?;
    if parts.len() > 1 && !matches!(parts[parts.len() - 2], "" | "." | "..") {
        let p = downloads.join(parts[parts.len() - 2]).join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    walkdir::WalkDir::new(downloads)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| e.file_type().is_file() && e.file_name().to_string_lossy() == *name)
        .map(|e| e.into_path())
}

/// Move finished songs into the music folder; retry failed ones elsewhere.
async fn check_transfers(st: &AppState, sl: &Slskd) -> Result<usize> {
    let transfers = sl.get("/transfers/downloads").await?;
    let queued: Vec<Req> = sqlx::query_as("SELECT * FROM request WHERE status = 'queued'").fetch_all(&st.db).await?;
    let upgrading: Vec<Upgrade> = sqlx::query_as(UPGRADE_SELECT).fetch_all(&st.db).await?;
    let mut known = HashSet::new();
    let mut moved = 0;
    for u in transfers.as_array().into_iter().flatten() {
        let user = u["username"].as_str().unwrap_or("");
        for d in u["directories"].as_array().into_iter().flatten() {
            for f in d["files"].as_array().into_iter().flatten() {
                let file = f["filename"].as_str().unwrap_or("");
                known.insert((user.to_string(), file.to_string()));
                if let Some(u) = upgrading.iter().find(|u| u.up_user.as_deref() == Some(user) && u.up_file.as_deref() == Some(file)) {
                    upgrade_transfer(st, sl, u, f).await?;
                    continue;
                }
                let Some(r) =
                    queued.iter().find(|r| r.slsk_user.as_deref() == Some(user) && r.slsk_file.as_deref() == Some(file))
                else {
                    continue;
                };
                let state = f["state"].as_str().unwrap_or("");
                let tid = f["id"].as_str().unwrap_or("");
                let remove = format!("/transfers/downloads/{}/{}?remove=true", urlencode(user), urlencode(tid));
                if state.contains("Succeeded") {
                    let Some(src) = st.cfg.slskd_downloads.as_deref().and_then(|d| locate(d, file)) else { continue };
                    match file_it(st, r, &src).await {
                        Ok(rel) => {
                            sqlx::query("UPDATE request SET status = 'done', rel = ?, arrived_at = ? WHERE key = ?")
                                .bind(&rel)
                                .bind(now())
                                .bind(&r.key)
                                .execute(&st.db)
                                .await?;
                            moved += 1;
                            info!(song = %format!("{} - {}", r.artists().first().cloned().unwrap_or_default(), r.title), file = %rel, "downloaded");
                            let _ = sl.call(reqwest::Method::DELETE, &remove, None).await;
                        }
                        Err(e) => warn!(song = %r.title, "couldn't file a download: {e:#}"),
                    }
                } else if ["Errored", "Rejected", "TimedOut", "Cancelled", "Aborted"].iter().any(|x| state.contains(x))
                    || (now() - r.since > 6 * 3600 && state.contains("Queued"))
                {
                    let mut tried = r.tried();
                    tried.push(user.to_string());
                    let status = if tried.len() < 10 { "retry" } else { "notfound" };
                    sqlx::query("UPDATE request SET status = ?, found = 1, tried_json = ?, last_search = 0 WHERE key = ?")
                        .bind(status)
                        .bind(serde_json::to_string(&tried)?)
                        .bind(&r.key)
                        .execute(&st.db)
                        .await?;
                    if !state.contains("Completed") {
                        let _ = sl.call(reqwest::Method::DELETE, &remove, None).await;
                    }
                }
            }
        }
    }
    for u in &upgrading {
        if now() - u.up_since.unwrap_or(0) > 600
            && !known.contains(&(u.up_user.clone().unwrap_or_default(), u.up_file.clone().unwrap_or_default()))
        {
            upgrade_over(st, &u.req.key, "slskd lost the transfer").await?; // keep the copy you have
        }
    }
    // slskd lost the transfer (restart, cleared list): look again
    for r in &queued {
        if now() - r.since > 600
            && !known.contains(&(r.slsk_user.clone().unwrap_or_default(), r.slsk_file.clone().unwrap_or_default()))
        {
            let mut tried = r.tried();
            tried.extend(r.slsk_user.clone());
            sqlx::query("UPDATE request SET status = 'retry', last_search = 0, tried_json = ? WHERE key = ?")
                .bind(serde_json::to_string(&tried)?)
                .bind(&r.key)
                .execute(&st.db)
                .await?;
        }
    }
    Ok(moved)
}

/// The library songs are filed into (the first one, unless `import_library` names another).
fn import_root(st: &AppState) -> Option<(i64, PathBuf)> {
    let pick = st.cfg.import_library.as_deref();
    st.libs.iter().find(|(_, l)| pick.is_none_or(|p| l.name.eq_ignore_ascii_case(p))).map(|(id, l)| (*id, l.path.clone()))
}

async fn file_it(st: &AppState, r: &Req, src: &Path) -> Result<String> {
    let (_, root) = import_root(st).ok_or_else(|| anyhow!("no music folder to file songs into"))?;
    let lib = st.libs.iter().find(|(_, l)| l.path == root).map(|(_, l)| l.clone()).unwrap();
    if !library::is_online(&lib) {
        bail!("the music drive isn't connected");
    }
    let artist = safe(r.artists().first().map(String::as_str).unwrap_or("Unknown"));
    let ext = src.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default();
    let folder = safe(&r.folder);
    let stem = format!("{folder}/{artist}/{artist} - {}", safe(&r.title));
    // a file may be there already (another version of the song, the same title from another album, your own
    // file): never replace it
    let mut rel = format!("{stem}{ext}");
    let mut n = 2;
    while tokio::fs::try_exists(root.join(&rel)).await.unwrap_or(true) {
        if n > 99 {
            bail!("{} and 98 others like it are there already", rel);
        }
        rel = format!("{stem} ({n}){ext}");
        n += 1;
    }
    let dst = root.join(&rel);
    if let Some(d) = dst.parent() {
        tokio::fs::create_dir_all(d).await.with_context(|| format!("creating {}", d.display()))?;
    }
    move_into(src, &dst).await?;
    Ok(rel)
}

/// Move a file to `dst` (which isn't there). From another drive it's copied under a hidden name beside `dst` and
/// renamed when complete, so a half-copied file (a full disk, a crash) is never in the library.
pub(crate) async fn move_into(src: &Path, dst: &Path) -> Result<()> {
    if tokio::fs::rename(src, dst).await.is_ok() {
        return Ok(());
    }
    let tmp = dst.with_file_name(format!(".conductor-part-{}", &crate::db::random_id()[..12]));
    let copied = async {
        tokio::fs::copy(src, &tmp).await.with_context(|| format!("copying to {}", dst.display()))?;
        tokio::fs::rename(&tmp, dst).await.with_context(|| format!("moving into {}", dst.display()))
    }
    .await;
    if copied.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        return copied;
    }
    let _ = tokio::fs::remove_file(src).await;
    Ok(())
}

/// Heart it / add it to playlists for whoever asked, now that it's in the library.
pub async fn apply_wants(st: &AppState, key: &str, local: &str) -> Result<()> {
    let wants: Vec<(String, bool, String)> =
        sqlx::query_as("SELECT user_id, star, playlist_id FROM request_want WHERE key = ? AND done = 0")
            .bind(key)
            .fetch_all(&st.db)
            .await?;
    for (user, star, pid) in wants {
        if star {
            sqlx::query("INSERT OR IGNORE INTO annotation (user_id, item_kind, item_id) VALUES (?, 'track', ?)")
                .bind(&user)
                .bind(local)
                .execute(&st.db)
                .await?;
            sqlx::query("UPDATE annotation SET starred_at = COALESCE(starred_at, ?) WHERE user_id = ? AND item_kind = 'track' AND item_id = ?")
                .bind(now()).bind(&user).bind(local).execute(&st.db).await?;
        }
        if !pid.is_empty() {
            // at the end, in one statement: the playlist may be written at the same time (a discovery list being
            // filled, an app's change), and a place worked out first could be taken by then
            let added = sqlx::query(
                "INSERT INTO playlist_entry (playlist_id, position, track_id)
                 SELECT ?1, (SELECT COALESCE(MAX(position) + 1, 0) FROM playlist_entry WHERE playlist_id = ?1), ?2
                 WHERE NOT EXISTS (SELECT 1 FROM playlist_entry WHERE playlist_id = ?1 AND track_id = ?2)
                   AND EXISTS (SELECT 1 FROM playlist WHERE id = ?1)",
            )
            .bind(&pid)
            .bind(local)
            .execute(&st.db)
            .await?;
            if added.rows_affected() > 0 {
                sqlx::query("UPDATE playlist SET updated_at = ? WHERE id = ?").bind(now()).bind(&pid).execute(&st.db).await?;
            }
        }
        sqlx::query("UPDATE request_want SET done = 1 WHERE key = ? AND user_id = ? AND playlist_id = ?")
            .bind(key)
            .bind(&user)
            .bind(&pid)
            .execute(&st.db)
            .await?;
    }
    Ok(())
}

/// Filed songs: scan, find them in the library, apply what was asked. One whose file is gone, or that a scan
/// still doesn't see an hour after it was filed, is given up on (shown as not found; asking again looks again),
/// so it doesn't make a scan every round.
async fn adopt(st: &AppState) -> Result<()> {
    let done: Vec<(String, String, Option<i64>)> =
        sqlx::query_as("SELECT key, rel, arrived_at FROM request WHERE status = 'done' AND local_id IS NULL AND rel IS NOT NULL")
            .fetch_all(&st.db)
            .await?;
    if done.is_empty() {
        return Ok(());
    }
    let (lib_id, root) = import_root(st).ok_or_else(|| anyhow!("no music folder"))?;
    if st.libs.iter().any(|(id, l)| *id == lib_id && !library::is_online(l)) {
        return Ok(()); // the music drive isn't connected: wait for it
    }
    let mut waiting = vec![];
    for (key, rel, at) in done {
        if root.join(&rel).is_file() {
            waiting.push((key, rel, at));
        } else {
            give_up(st, &key, "its file is gone").await?;
        }
    }
    // a scan running now (a full one can take hours) would make this one return at once, seeing nothing: the songs
    // would look unseen and be given up on while their files are there. Next round, then
    if waiting.is_empty() || st.scan.scanning.load(std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    library::scan(&st.db, &st.libs, &st.scan, false).await?;
    for (key, rel, at) in waiting {
        let local: Option<String> =
            sqlx::query_scalar("SELECT id FROM track WHERE library_id = ? AND rel_path = ? AND missing_since IS NULL")
                .bind(lib_id)
                .bind(&rel)
                .fetch_optional(&st.db)
                .await?;
        if let Some(id) = local {
            sqlx::query("UPDATE request SET local_id = ? WHERE key = ?").bind(&id).bind(&key).execute(&st.db).await?;
            apply_wants(st, &key, &id).await?;
        } else if now() - at.unwrap_or(0) > 3600 {
            give_up(st, &key, "the scan doesn't see its file").await?;
        }
    }
    st.catalog.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

async fn give_up(st: &AppState, key: &str, why: &str) -> Result<()> {
    sqlx::query("UPDATE request SET status = 'notfound', rel = NULL WHERE key = ? AND status = 'done' AND local_id IS NULL")
        .bind(key)
        .execute(&st.db)
        .await?;
    warn!(key, "a downloaded song wasn't added: {why}");
    Ok(())
}

/// The download worker: woken when a song is asked for, and every 30 seconds.
pub async fn worker(st: Arc<AppState>) {
    if st.cfg.read_only {
        info!("read-only: songs asked for, trial songs and Lidarr albums wait (nothing is downloaded or moved)");
        return;
    }
    let (Some(url), Some(key)) = (st.cfg.slskd_url.clone(), st.cfg.slskd_api_key.clone()) else {
        info!("downloads: no slskd_url / slskd_api_key set, so songs asked for wait until one is");
        return;
    };
    let sl = Slskd::new(&url, &key);
    let lidarr = super::lidarr::configured(&st);
    if lidarr.is_some() {
        info!("Lidarr's wanted albums are fetched from Soulseek too");
    }
    let mut last_wanted = 0;
    loop {
        if let Some(lid) = &lidarr
            && !crate::vpn::paused(&st)
            && sl.logged_in().await
            && let Err(e) = super::lidarr::tick(&st, &sl, lid, &mut last_wanted).await
        {
            warn!("Lidarr albums: {e:#}");
        }
        if let Err(e) = tick(&st, &sl).await {
            warn!("downloads: {e:#}");
        }
        let _ =
            tokio::time::timeout(Duration::from_secs(if st.cfg.slskd_fast_test { 1 } else { 30 }), st.acquire_wake.notified())
                .await;
    }
}

async fn tick(st: &AppState, sl: &Slskd) -> Result<()> {
    if let Err(e) = super::trial::tick(st).await {
        warn!("songs on trial: {e:#}");
    }
    if !sl.logged_in().await {
        return Ok(()); // slskd drops off Soulseek whenever its VPN tunnel reconnects; wait
    }
    check_transfers(st, sl).await?;
    adopt(st).await?;
    if crate::vpn::paused(st) {
        return Ok(()); // the kill switch: finished downloads are filed, nothing new is searched or fetched
    }
    let in_flight: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE status = 'queued'").fetch_one(&st.db).await?;
    if in_flight >= IN_FLIGHT {
        return Ok(());
    }
    // songs asked for most recently first (that's what someone is waiting for)
    let waiting: Vec<Req> =
        sqlx::query_as("SELECT * FROM request WHERE status = 'retry' ORDER BY asked_at DESC LIMIT 50").fetch_all(&st.db).await?;
    let due: Vec<Req> = waiting.into_iter().filter(|r| now() - r.last_search >= retry_wait(r)).collect();
    let idle = due.is_empty() && in_flight == 0;
    for r in due.into_iter().take((IN_FLIGHT - in_flight) as usize) {
        if crate::vpn::paused(st) {
            return Ok(()); // the VPN went down during this round
        }
        match find_and_queue(st, sl, &r).await {
            Ok(note) => info!(song = %r.title, "Soulseek: {note}"),
            Err(e) => {
                warn!(song = %r.title, "Soulseek search failed: {e:#}");
                break;
            }
        }
    }
    // nothing else to do: look for a better copy of one song
    if idle && st.cfg.upgrades {
        let busy: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE status = 'upgrading'").fetch_one(&st.db).await?;
        if busy == 0
            && let Err(e) = upgrade_one(st, sl).await
        {
            warn!("looking for a better copy: {e:#}");
        }
    }
    Ok(())
}

// ───────────── upgrades ─────────────

/// A song filed by Conductor that may get a better copy, or is getting one.
#[derive(sqlx::FromRow)]
struct Upgrade {
    #[sqlx(flatten)]
    req: Req,
    rel: String,
    local_id: String,
    same_album: Option<bool>,
    up_user: Option<String>,
    up_file: Option<String>,
    up_since: Option<i64>,
}

const UPGRADE_SELECT: &str = "SELECT * FROM request WHERE status = 'upgrading' AND rel IS NOT NULL AND local_id IS NOT NULL";

fn ext_of(path: &str) -> String {
    path.rsplit('.').next().filter(|e| !e.contains(['/', '\\'])).unwrap_or("").to_lowercase()
}

/// Is `file` better than the copy you have? From the album asked for when yours is from another one; else FLAC
/// when yours is lossy (and not from another album than yours).
pub fn better(r: &Req, have_same: Option<bool>, have_ext: &str, file: &str) -> bool {
    let same = same_album(r, file);
    if have_same == Some(false) {
        return same == Some(true);
    }
    have_ext != "flac" && ext_of(file) == "flac" && (same != Some(false) || have_same.is_none())
}

async fn upgrade_one(st: &AppState, sl: &Slskd) -> Result<()> {
    let u: Option<Upgrade> = sqlx::query_as(
        "SELECT * FROM request WHERE status = 'done' AND local_id IS NOT NULL AND rel LIKE 'Songs/%' AND upgrade_tried = 0
            AND trial = 0 AND (same_album = 0 OR lower(rel) NOT LIKE '%.flac')
         ORDER BY arrived_at DESC LIMIT 1",
    )
    .fetch_optional(&st.db)
    .await?;
    let Some(u) = u else { return Ok(()) };
    let have = ext_of(&u.rel);
    let cands: Vec<_> = gather(st, sl, &u.req, &[])
        .await?
        .into_iter()
        .filter(|c| better(&u.req, u.same_album, &have, c.2["filename"].as_str().unwrap_or("")))
        .collect();
    for (_, user, f) in cands.iter().take(5) {
        if crate::vpn::paused(st) {
            return Ok(()); // looked at again later
        }
        let body = json!([{ "filename": f["filename"], "size": f["size"] }]);
        if sl.call(reqwest::Method::POST, &format!("/transfers/downloads/{}", urlencode(user)), Some(body)).await.is_err() {
            continue;
        }
        let file = f["filename"].as_str().unwrap_or("");
        sqlx::query("UPDATE request SET status = 'upgrading', up_user = ?, up_file = ?, up_since = ? WHERE key = ?")
            .bind(user)
            .bind(file)
            .bind(now())
            .bind(&u.req.key)
            .execute(&st.db)
            .await?;
        info!(song = %u.req.title, from = %user, file = %file.replace('\\', "/"), "getting a better copy");
        return Ok(());
    }
    upgrade_over(st, &u.req.key, if cands.is_empty() { "no better copy" } else { "nobody reachable" }).await
}

/// Done looking for a better copy of this song: the one you have stays.
async fn upgrade_over(st: &AppState, key: &str, why: &str) -> Result<()> {
    sqlx::query("UPDATE request SET status = 'done', upgrade_tried = 1, up_user = NULL, up_file = NULL, up_since = NULL WHERE key = ?")
        .bind(key)
        .execute(&st.db)
        .await?;
    info!(key, "keeping the copy you have ({why})");
    Ok(())
}

async fn upgrade_transfer(st: &AppState, sl: &Slskd, u: &Upgrade, f: &Value) -> Result<()> {
    let state = f["state"].as_str().unwrap_or("");
    let user = u.up_user.clone().unwrap_or_default();
    let remove = format!("/transfers/downloads/{}/{}?remove=true", urlencode(&user), urlencode(f["id"].as_str().unwrap_or("")));
    if state.contains("Succeeded") {
        let file = u.up_file.clone().unwrap_or_default();
        let Some(src) = st.cfg.slskd_downloads.as_deref().and_then(|d| locate(d, &file)) else { return Ok(()) };
        match swap_in(st, u, &src).await {
            Ok(rel) => {
                sqlx::query(
                    "UPDATE request SET status = 'done', rel = ?, slsk_user = up_user, slsk_file = up_file, same_album = ?,
                        upgrade_tried = 1, up_user = NULL, up_file = NULL, up_since = NULL WHERE key = ?",
                )
                .bind(&rel)
                .bind(same_album(&u.req, &file))
                .bind(&u.req.key)
                .execute(&st.db)
                .await?;
                info!(song = %u.req.title, file = %rel, "swapped in a better copy (same song id)");
                library::scan(&st.db, &st.libs, &st.scan, false).await?;
                st.catalog.generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Err(e) => {
                warn!(song = %u.req.title, "the better copy wasn't used: {e:#}");
                let _ = tokio::fs::remove_file(&src).await;
                upgrade_over(st, &u.req.key, "the new file didn't check out").await?;
            }
        }
        let _ = sl.call(reqwest::Method::DELETE, &remove, None).await;
    } else if ["Errored", "Rejected", "TimedOut", "Cancelled", "Aborted"].iter().any(|x| state.contains(x))
        || (now() - u.up_since.unwrap_or(0) > 6 * 3600 && state.contains("Queued"))
    {
        upgrade_over(st, &u.req.key, "the download didn't come").await?;
        if !state.contains("Completed") {
            let _ = sl.call(reqwest::Method::DELETE, &remove, None).await;
        }
    }
    Ok(())
}

/// Put the downloaded better copy where the old file is: copied in beside it under a hidden name and checked
/// (it reads, and is as long as the song), then, while scans wait, the song's row moves to the new name (same id)
/// and the file is renamed into place; the old file goes last. Returns the new place.
async fn swap_in(st: &AppState, u: &Upgrade, src: &Path) -> Result<String> {
    let (lib, root) = import_root(st).ok_or_else(|| anyhow!("no music folder"))?;
    if !u.rel.starts_with("Songs/") || u.rel.contains("..") {
        bail!("only songs Conductor filed in Songs/ are swapped");
    }
    let old = root.join(&u.rel);
    if !old.is_file() {
        bail!("the old file isn't there");
    }
    let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let stem = u.rel.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(&u.rel).to_string();
    let mut new_rel = format!("{stem}.{ext}");
    // another file may have the new name (a song of yours, another version): it stays, and this one goes beside it
    let mut n = 2;
    while new_rel != u.rel && taken(st, lib, &root, &new_rel).await? {
        if n > 99 {
            bail!("{new_rel} and 98 others like it are there already");
        }
        new_rel = format!("{stem} ({n}).{ext}");
        n += 1;
    }
    let new = root.join(&new_rel);
    let dir = old.parent().ok_or_else(|| anyhow!("no folder"))?.to_path_buf();
    let tmp = dir.join(format!(".conductor-upgrade-{}.{ext}", u.req.key));
    let done = put_in_place(st, u, src, &tmp, lib, &new_rel, &new, &old).await;
    if done.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await; // never left behind
    }
    done?;
    Ok(new_rel)
}

/// A file or a library song has this name already.
async fn taken(st: &AppState, lib: i64, root: &Path, rel: &str) -> Result<bool> {
    let song: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM track WHERE library_id = ? AND rel_path = ?")
        .bind(lib)
        .bind(rel)
        .fetch_one(&st.db)
        .await?;
    Ok(song > 0 || tokio::fs::try_exists(root.join(rel)).await.unwrap_or(true))
}

/// `swap_in`'s steps from the hidden copy on: check it, move the song's row, rename it into place, and remove the
/// old file (still while scans wait: one in between would add it back as a song of its own).
#[allow(clippy::too_many_arguments)]
async fn put_in_place(st: &AppState, u: &Upgrade, src: &Path, tmp: &Path, lib: i64, new_rel: &str, new: &Path, old: &Path) -> Result<()> {
    if tokio::fs::rename(src, tmp).await.is_err() {
        tokio::fs::copy(src, tmp).await.with_context(|| format!("copying to {}", tmp.display()))?;
        let _ = tokio::fs::remove_file(src).await;
    }
    let t2 = tmp.to_path_buf();
    let m = tokio::task::spawn_blocking(move || library::read_meta(&t2)).await?;
    let want = u.req.duration_ms;
    if m.duration_ms <= 0 || (want > 0 && (m.duration_ms - want).abs() > 8000) {
        bail!("the new file doesn't read as this song ({} ms, wanted {want} ms)", m.duration_ms);
    }
    let Some(_hold) = library::hold_scans(&st.scan).await else {
        bail!("a scan kept running");
    };
    let mut tx = st.db.begin().await?;
    sqlx::query("UPDATE track SET rel_path = ? WHERE id = ?").bind(new_rel).bind(&u.local_id).execute(&mut *tx).await?;
    sqlx::query("INSERT OR REPLACE INTO id_override (kind, key, id) VALUES ('track', ?, ?)")
        .bind(library::track_key(lib, new_rel))
        .bind(&u.local_id)
        .execute(&mut *tx)
        .await?;
    tokio::fs::rename(tmp, new).await.context("moving the new file into place")?; // on error the transaction is dropped: nothing changed
    tx.commit().await?;
    if new != old {
        let _ = tokio::fs::remove_file(old).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn req(title: &str, artist: &str, album: &str, secs: i64) -> Req {
        Req {
            key: "dz-t-1".into(),
            title: title.into(),
            artists_json: serde_json::to_string(&[artist]).unwrap(),
            album: album.into(),
            duration_ms: secs * 1000,
            folder: "Songs".into(),
            status: "retry".into(),
            slsk_user: None,
            slsk_file: None,
            searches: 0,
            last_search: 0,
            tried_json: "[]".into(),
            found: false,
            since: 0,
        }
    }
    #[test]
    fn picks_like_spotify_py() {
        let r = req("Once in a Lifetime", "Talking Heads", "Remain in Light", 259);
        let resp = json!({ "username": "u", "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000 });
        let f = |name: &str, br: i64, len: f64| json!({ "filename": name, "bitRate": br, "length": len, "size": 1 });
        let same =
            score_file(&r, &resp, &f(r"Music\Talking Heads\Remain in Light (1980)\03 Once in a Lifetime.flac", 0, 259.0), &[])
                .unwrap();
        let other = score_file(&r, &resp, &f(r"Music\Talking Heads\Best Of\Once in a Lifetime.flac", 0, 259.0), &[]).unwrap();
        assert!(same > other + 30.0);
        assert!(score_file(&r, &resp, &f(r"Talking Heads\Once in a Lifetime (Live).flac", 0, 259.0), &[]).is_none());
        assert!(score_file(&r, &resp, &f(r"Talking Heads\Once in a Lifetime.mp3", 192, 259.0), &[]).is_none()); // too low
        assert!(score_file(&r, &resp, &f(r"Talking Heads\Once in a Lifetime.flac", 0, 300.0), &[]).is_none()); // other length
        assert!(score_file(&r, &resp, &f(r"Someone Else\Once in a Lifetime.flac", 0, 259.0), &[]).is_none()); // other artist
        assert!(score_file(&r, &resp, &f(r"Talking Heads\Once in a Lifetime.flac", 0, 259.0), &["u".into()]).is_none());
        assert_eq!(same_album(&req("Song", "A", "Song", 1), "x/y.flac"), None);
        // a better copy: the album asked from when yours is from another; else FLAC over lossy, same album
        let r = req("Once in a Lifetime", "Talking Heads", "Remain in Light", 259);
        let (album_mp3, album_flac, other_flac) =
            (r"TH\Remain in Light\03 Once.mp3", r"TH\Remain in Light\03 Once.flac", r"TH\Best Of\Once.flac");
        assert!(better(&r, Some(false), "flac", album_mp3));
        assert!(!better(&r, Some(false), "mp3", other_flac));
        assert!(better(&r, Some(true), "mp3", album_flac));
        assert!(!better(&r, Some(true), "mp3", other_flac), "not a different version");
        assert!(!better(&r, Some(true), "flac", album_flac), "nothing to gain");
        assert!(artist_in(&["Аквариум".into()], "Akvarium - Album/01.flac"));
    }

    #[test]
    fn a_finished_download_is_only_looked_for_in_the_downloads() {
        // the name is a peer's: "..\\Song.flac" made it the file of that name beside the downloads folder (with the
        // downloads in the data folder, as Prelude suggests, that's Conductor's own data), which was then moved
        // into the music folder
        let tmp = tempfile::tempdir().unwrap();
        let downloads = tmp.path().join("downloads");
        std::fs::create_dir_all(downloads.join("Album")).unwrap();
        std::fs::write(tmp.path().join("Song.flac"), b"not a download").unwrap();
        assert_eq!(locate(&downloads, r"Music\..\Song.flac"), None);
        std::fs::write(downloads.join("Album/Song.flac"), b"x").unwrap();
        assert_eq!(locate(&downloads, r"Music\..\Song.flac"), Some(downloads.join("Album/Song.flac")));
        assert_eq!(locate(&downloads, r"Music\Album\Song.flac"), Some(downloads.join("Album/Song.flac")));
    }
}
