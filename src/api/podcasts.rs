//! Podcasts: the whole Subsonic podcast API, plus streams and covers for podcast ids, and bookmarks.
//!
//!   getPodcasts  getNewestPodcasts  getPodcastEpisode  createPodcastChannel  deletePodcastChannel  refreshPodcasts
//!   downloadPodcastEpisode  deletePodcastEpisode  setPodcastProgress   + stream / download / getCoverArt for
//!   "pe-…" / "pc-…" ids   + createBookmark / getBookmarks / deleteBookmark
//!
//! Feeds are checked every 2 hours. Nothing is downloaded on its own (`podcast_keep = 0`): episodes stream from the
//! podcast's own server, through here; the ones asked for (downloadPodcastEpisode) are kept in the podcast folder
//! (not the music folder). `podcast_keep = 3` also keeps the newest 3 of every show. Played episodes saved on the
//! server go `podcast_clean_days` after they were finished (0: never).

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tokio::io::AsyncWriteExt;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tracing::{info, warn};

use super::{ApiError, AppState, Ctx, Reply, empty, ok, raw};
use crate::db::{self, now};

const UA: &str = concat!("Conductor/", env!("CARGO_PKG_VERSION"), " (podcasts)");
const ITUNES: &str = "http://www.itunes.com/dtds/podcast-1.0.dtd";
const CONTENT: &str = "http://purl.org/rss/1.0/modules/content/";
const REFRESH_SECS: i64 = 2 * 3600;

pub fn is_podcast_id(id: &str) -> bool {
    id.starts_with("pe-") || id.starts_with("pc-")
}

fn hid(prefix: &str, s: &str, n: usize) -> String {
    format!("{prefix}-{}", &hex::encode(Sha1::digest(s.as_bytes()))[..n])
}

/// A name safe for a file or folder on Linux and Windows.
fn safe(s: &str) -> String {
    let mut o = String::new();
    let mut last_bad = false;
    for ch in s.chars() {
        let bad = r#"\/:*?"<>|"#.contains(ch) || ch.is_control();
        if bad {
            if !last_bad {
                o.push('_');
            }
        } else {
            o.push(ch);
        }
        last_bad = bad;
    }
    let o: String = o.trim_matches([' ', '.']).chars().take(100).collect();
    if o.is_empty() { "_".into() } else { o }
}

pub fn strip_html(s: &str) -> String {
    let mut s = s.to_string();
    for t in ["<br>", "<br/>", "<br />", "</p>", "<BR>", "</P>"] {
        s = s.replace(t, "\n");
    }
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    for (a, b) in [("&amp;", "&"), ("&lt;", "<"), ("&gt;", ">"), ("&quot;", "\""), ("&#39;", "'"), ("&nbsp;", " ")] {
        out = out.replace(a, b);
    }
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    out.trim().to_string()
}

fn seconds(v: &str) -> i64 {
    let v = v.trim();
    if v.contains(':') {
        v.split(':').try_fold(0i64, |n, p| p.trim().parse::<f64>().ok().map(|x| n * 60 + x as i64)).unwrap_or(0)
    } else {
        v.parse::<f64>().map(|x| x as i64).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default)]
pub struct FeedEpisode {
    pub id: String,
    pub guid: String,
    pub title: String,
    pub description: String,
    pub published: i64,
    pub duration: i64,
    pub url: String,
    pub size: i64,
    pub content_type: String,
    pub image: String,
}

#[derive(Debug, Clone, Default)]
pub struct Feed {
    pub id: String,
    pub title: String,
    pub author: String,
    pub description: String,
    pub image: String,
    pub episodes: Vec<FeedEpisode>,
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, ns: Option<&str>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name && c.tag_name().namespace() == ns)
}

fn text(n: roxmltree::Node, ns: Option<&str>, name: &str) -> String {
    child(n, ns, name).and_then(|c| c.text()).unwrap_or("").trim().to_string()
}

/// An RSS podcast feed.
pub fn parse_feed(url: &str, xml: &str) -> Result<Feed> {
    let doc = roxmltree::Document::parse(xml).context("That address isn't a podcast feed")?;
    let ch = doc.descendants().find(|n| n.has_tag_name("channel")).ok_or_else(|| anyhow!("That address isn't a podcast feed"))?;
    let image = child(ch, Some(ITUNES), "image")
        .and_then(|i| i.attribute("href"))
        .map(String::from)
        .or_else(|| child(ch, None, "image").map(|i| text(i, None, "url")))
        .unwrap_or_default();
    let id = hid("pc", url, 10);
    let title = Some(text(ch, None, "title")).filter(|t| !t.is_empty()).unwrap_or_else(|| url.to_string());
    let desc = Some(text(ch, None, "description")).filter(|t| !t.is_empty()).unwrap_or_else(|| text(ch, Some(ITUNES), "summary"));
    let mut feed = Feed {
        id: id.clone(),
        title,
        author: text(ch, Some(ITUNES), "author"),
        description: strip_html(&desc),
        image,
        episodes: vec![],
    };
    for it in ch.children().filter(|c| c.has_tag_name("item")) {
        let Some(enc) = child(it, None, "enclosure") else { continue };
        let Some(eurl) = enc.attribute("url").filter(|u| !u.is_empty()) else { continue };
        let guid = Some(text(it, None, "guid")).filter(|g| !g.is_empty()).unwrap_or_else(|| eurl.to_string());
        let published = chrono::DateTime::parse_from_rfc2822(&text(it, None, "pubDate")).map(|d| d.timestamp()).unwrap_or(0);
        let body = [text(it, Some(CONTENT), "encoded"), text(it, None, "description"), text(it, Some(ITUNES), "summary")]
            .into_iter()
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        feed.episodes.push(FeedEpisode {
            id: hid("pe", &format!("{id}{guid}"), 12),
            guid,
            title: text(it, None, "title"),
            description: strip_html(&body).chars().take(4000).collect(),
            published,
            duration: seconds(&text(it, Some(ITUNES), "duration")),
            url: eurl.to_string(),
            size: enc.attribute("length").and_then(|l| l.parse().ok()).unwrap_or(0),
            content_type: enc.attribute("type").filter(|t| !t.is_empty()).unwrap_or("audio/mpeg").to_string(),
            image: child(it, Some(ITUNES), "image").and_then(|i| i.attribute("href")).unwrap_or("").to_string(),
        });
    }
    Ok(feed)
}

/// A request to a podcast's server. Feeds come from anyone, so unless `podcast_local` is on, only internet
/// addresses are reached (redirects too): a feed can't point Conductor at the home network.
fn get(st: &AppState, url: &str) -> Result<reqwest::RequestBuilder> {
    let mut b = reqwest::Client::builder().user_agent(UA).connect_timeout(Duration::from_secs(20));
    if !st.cfg.podcast_local {
        let u: reqwest::Url = url.parse().context("not a web address")?;
        crate::net::check_public_url(&u).map_err(|e| anyhow!(e))?;
        b = crate::net::public_only(b);
    }
    Ok(b.build()?.get(url))
}

async fn fetch_text(st: &AppState, url: &str) -> Result<String> {
    let r = get(st, url)?.timeout(Duration::from_secs(30)).send().await?.error_for_status()?;
    Ok(r.text().await?)
}

/// Save a feed: new episodes added, known ones updated.
async fn store(st: &AppState, url: &str, f: &Feed, new_show: bool) -> Result<usize> {
    let mut tx = st.db.begin().await?;
    if new_show {
        sqlx::query(
            "INSERT OR IGNORE INTO podcast (id, url, title, author, description, image, added_at) VALUES (?,?,?,?,?,?,?)",
        )
        .bind(&f.id)
        .bind(url)
        .bind(&f.title)
        .bind(&f.author)
        .bind(&f.description)
        .bind(&f.image)
        .bind(now())
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("UPDATE podcast SET title = ?, author = ?, description = ?, image = ?, checked_at = ?, error = '' WHERE id = ?")
        .bind(&f.title)
        .bind(&f.author)
        .bind(&f.description)
        .bind(&f.image)
        .bind(now())
        .bind(&f.id)
        .execute(&mut *tx)
        .await?;
    let mut new = 0;
    for e in &f.episodes {
        let r = sqlx::query(
            "INSERT INTO episode (id, podcast_id, guid, title, description, published_at, duration, url, size, content_type, image)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET title = excluded.title, description = excluded.description, duration = excluded.duration,
               url = excluded.url, content_type = excluded.content_type, image = excluded.image,
               size = CASE WHEN excluded.size > 0 THEN excluded.size ELSE episode.size END",
        )
        .bind(&e.id).bind(&f.id).bind(&e.guid).bind(&e.title).bind(&e.description).bind(e.published).bind(e.duration)
        .bind(&e.url).bind(e.size).bind(&e.content_type).bind(&e.image)
        .execute(&mut *tx).await?;
        if r.rows_affected() > 0 && new_show {
            new += 1;
        }
    }
    tx.commit().await?;
    Ok(new)
}

pub async fn refresh(st: &AppState, only: Option<&str>) {
    let shows: Vec<(String, String)> = sqlx::query_as("SELECT id, url FROM podcast").fetch_all(&st.db).await.unwrap_or_default();
    for (id, url) in shows.into_iter().filter(|(id, _)| only.is_none_or(|o| o == id)) {
        let res = async {
            let f = parse_feed(&url, &fetch_text(st, &url).await?)?;
            store(st, &url, &f, false).await
        }
        .await;
        if let Err(e) = res {
            warn!(feed = %url, "couldn't check a podcast: {e:#}");
            let _ = sqlx::query("UPDATE podcast SET checked_at = ?, error = ? WHERE id = ?")
                .bind(now())
                .bind(format!("{e:#}").chars().take(200).collect::<String>())
                .bind(&id)
                .execute(&st.db)
                .await;
        }
    }
    st.podcast_wake.notify_one();
}

pub fn folder(st: &AppState) -> PathBuf {
    st.cfg.podcast_dir.clone().unwrap_or_else(|| st.cfg.data_dir.join("podcasts"))
}

fn ext_of(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or("").to_lowercase();
    for e in ["mp3", "m4a", "aac", "ogg", "opus", "mp4", "wav", "flac"] {
        if path.ends_with(&format!(".{e}")) {
            return match e {
                "mp3" => ".mp3",
                "m4a" => ".m4a",
                "aac" => ".aac",
                "ogg" => ".ogg",
                "opus" => ".opus",
                "mp4" => ".mp4",
                "wav" => ".wav",
                _ => ".flac",
            };
        }
    }
    ".mp3"
}

#[derive(sqlx::FromRow, Clone)]
struct EpRow {
    id: String,
    podcast_id: String,
    title: String,
    description: String,
    published_at: i64,
    duration: i64,
    url: String,
    size: i64,
    content_type: String,
    image: String,
    file: Option<String>,
    pinned: bool,
    show_title: String,
    show_author: String,
    position_ms: Option<i64>,
    done: Option<bool>,
}

const EP_SELECT: &str =
    "SELECT e.id, e.podcast_id, e.title, e.description, e.published_at, e.duration, e.url, e.size, e.content_type,
    e.image, e.file, e.pinned, p.title AS show_title, p.author AS show_author, pr.position_ms, pr.done
  FROM episode e JOIN podcast p ON p.id = e.podcast_id
  LEFT JOIN podcast_progress pr ON pr.user_id = ? AND pr.episode_id = e.id";

async fn download(st: &AppState, e: &EpRow) -> Result<()> {
    let day = chrono::DateTime::from_timestamp(if e.published_at > 0 { e.published_at } else { now() }, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default();
    let mut rel = format!("{}/{day} {}{}", safe(&e.show_title), safe(&e.title), ext_of(&e.url));
    // two episodes on the same day with the same title (or two shows with the same name): each its own file
    let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM episode WHERE file = ? AND id != ?")
        .bind(&rel)
        .bind(&e.id)
        .fetch_one(&st.db)
        .await?;
    if taken > 0 || folder(st).join(&rel).exists() {
        let tail: String = e.id.chars().skip(e.id.chars().count().saturating_sub(6)).collect();
        rel = format!("{}/{day} {} [{tail}]{}", safe(&e.show_title), safe(&e.title), ext_of(&e.url));
    }
    let dst = folder(st).join(&rel);
    if let Some(d) = dst.parent() {
        tokio::fs::create_dir_all(d).await?;
    }
    let part = dst.with_extension("part");
    let r = get(st, &e.url)?.timeout(Duration::from_secs(3600)).send().await?.error_for_status()?;
    let mut f = tokio::fs::File::create(&part).await?;
    let mut body = r.bytes_stream();
    while let Some(chunk) = body.next().await {
        f.write_all(&chunk?).await?;
    }
    f.flush().await?;
    drop(f);
    tokio::fs::rename(&part, &dst).await?;
    let size = tokio::fs::metadata(&dst).await.map(|m| m.len() as i64).unwrap_or(0);
    sqlx::query("UPDATE episode SET file = ?, size = ? WHERE id = ?").bind(&rel).bind(size).bind(&e.id).execute(&st.db).await?;
    info!(show = %e.show_title, episode = %e.title, "podcast episode downloaded");
    Ok(())
}

async fn remove_file(st: &AppState, id: &str) {
    let rel: Option<String> = sqlx::query_scalar("SELECT file FROM episode WHERE id = ?")
        .bind(id)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()
        .flatten();
    if let Some(rel) = rel {
        if !shared(st, &rel, id).await {
            let _ = tokio::fs::remove_file(folder(st).join(&rel)).await;
        }
        let _ = sqlx::query("UPDATE episode SET file = NULL WHERE id = ?").bind(id).execute(&st.db).await;
    }
}

/// Another episode's saved file is this one too (saved before names were kept apart): it stays.
async fn shared(st: &AppState, rel: &str, id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM episode WHERE file = ? AND id != ?")
        .bind(rel)
        .bind(id)
        .fetch_one(&st.db)
        .await
        .map_or(true, |n| n > 0)
}

/// Downloads what's wanted, removes what isn't, refreshes feeds every 2 hours. Woken by `podcast_wake`.
pub async fn worker(st: Arc<AppState>) {
    let mut last_refresh = 0i64;
    loop {
        if now() - last_refresh > REFRESH_SECS {
            last_refresh = now();
            refresh(&st, None).await;
        }
        if let Err(e) = upkeep(&st).await {
            warn!("podcasts: {e:#}");
        }
        let _ = tokio::time::timeout(Duration::from_secs(600), st.podcast_wake.notified()).await;
    }
}

async fn upkeep(st: &AppState) -> Result<()> {
    let keep = st.cfg.podcast_keep;
    let shows: Vec<String> = sqlx::query_scalar("SELECT id FROM podcast").fetch_all(&st.db).await?;
    for show in shows {
        let eps: Vec<EpRow> = sqlx::query_as(&format!("{EP_SELECT} WHERE e.podcast_id = ? ORDER BY e.published_at DESC"))
            .bind("")
            .bind(&show)
            .fetch_all(&st.db)
            .await?;
        for (i, e) in eps.iter().enumerate() {
            let want = e.pinned || (i as i64) < keep;
            if want && e.file.is_none() {
                if let Err(err) = download(st, e).await {
                    warn!(episode = %e.title, "couldn't download: {err:#}");
                }
            } else if !want && e.file.is_some() {
                remove_file(st, &e.id).await; // an older automatic download: make room
            }
        }
    }
    // episodes saved on the server, finished more than N days ago by everyone who started them
    let days = st.cfg.podcast_clean_days;
    if days > 0 {
        let old: Vec<String> = sqlx::query_scalar(
            "SELECT pr.episode_id FROM podcast_progress pr JOIN episode e ON e.id = pr.episode_id WHERE e.file IS NOT NULL
             GROUP BY pr.episode_id HAVING MIN(pr.done) = 1 AND MAX(pr.at) < ?",
        )
        .bind(now() - days * 86_400)
        .fetch_all(&st.db)
        .await?;
        for id in old {
            sqlx::query("UPDATE episode SET pinned = 0 WHERE id = ?").bind(&id).execute(&st.db).await?;
            remove_file(st, &id).await;
        }
    }
    Ok(())
}

// ── the API ──

fn iso_ms(t: i64) -> String {
    db::iso(t).replace('Z', ".000Z")
}

fn ep_json(e: &EpRow) -> Value {
    let ext = e
        .file
        .as_deref()
        .and_then(|f| Path::new(f).extension())
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_else(|| ext_of(&e.url)[1..].to_string());
    json!({
        "id": e.id, "streamId": e.id, "channelId": e.podcast_id, "parent": e.podcast_id, "isDir": false,
        "title": e.title, "album": e.show_title, "artist": if e.show_author.is_empty() { &e.show_title } else { &e.show_author },
        "description": e.description, "publishDate": iso_ms(e.published_at), "status": "completed",
        "coverArt": if e.image.is_empty() { &e.podcast_id } else { &e.id },
        "duration": e.duration, "size": e.size, "contentType": e.content_type, "suffix": ext, "type": "podcast",
        "mediaType": "podcastEpisode", "downloaded": e.file.is_some(), "position": e.position_ms.unwrap_or(0),
        "played": e.done.unwrap_or(false),
    })
}

#[derive(sqlx::FromRow)]
struct ShowRow {
    id: String,
    url: String,
    title: String,
    author: String,
    description: String,
    image: String,
    added_at: i64,
    error: String,
}

async fn show_json(c: &Ctx, s: &ShowRow, with_episodes: bool) -> Result<Value, ApiError> {
    let eps: Vec<EpRow> = sqlx::query_as(&format!("{EP_SELECT} WHERE e.podcast_id = ? ORDER BY e.published_at DESC"))
        .bind(&c.user.id)
        .bind(&s.id)
        .fetch_all(&c.st.db)
        .await?;
    let since = s.added_at - 14 * 86_400; // "new": recent ones, not the whole back catalogue
    let mut v = json!({
        "id": s.id, "url": s.url, "title": s.title, "description": s.description, "coverArt": s.id,
        "originalImageUrl": s.image, "status": if s.error.is_empty() { "completed" } else { "error" }, "author": s.author,
        "unplayed": eps.iter().filter(|e| e.published_at >= since && !e.done.unwrap_or(false)).count(),
        "episodeCount": eps.len(), "latest": eps.first().map(|e| e.published_at).unwrap_or(0),
    });
    if !s.error.is_empty() {
        v["errorMessage"] = s.error.clone().into();
    }
    if with_episodes {
        v["episode"] = eps.iter().map(ep_json).collect::<Vec<_>>().into();
    }
    Ok(v)
}

const SHOW_SELECT: &str = "SELECT id, url, title, author, description, image, added_at, error FROM podcast";

pub async fn get_podcasts(c: &Ctx) -> Result<Reply, ApiError> {
    let with = c.p.get("includeEpisodes") != Some("false");
    let rows: Vec<ShowRow> = match c.p.get("id").filter(|i| !i.is_empty()) {
        Some(id) => sqlx::query_as(&format!("{SHOW_SELECT} WHERE id = ?")).bind(id).fetch_all(&c.st.db).await?,
        None => sqlx::query_as(&format!("{SHOW_SELECT} ORDER BY title COLLATE NOCASE")).fetch_all(&c.st.db).await?,
    };
    let mut out = vec![];
    for s in &rows {
        out.push(show_json(c, s, with).await?);
    }
    ok(json!({ "podcasts": { "channel": out } }))
}

pub async fn newest(c: &Ctx) -> Result<Reply, ApiError> {
    let n = c.p.int("count").unwrap_or(20).clamp(1, 200);
    let eps: Vec<EpRow> = sqlx::query_as(&format!("{EP_SELECT} ORDER BY e.published_at DESC LIMIT {n}"))
        .bind(&c.user.id)
        .fetch_all(&c.st.db)
        .await?;
    ok(json!({ "newestPodcasts": { "episode": eps.iter().map(ep_json).collect::<Vec<_>>() } }))
}

async fn episode(c: &Ctx, id: &str) -> Result<EpRow, ApiError> {
    sqlx::query_as(&format!("{EP_SELECT} WHERE e.id = ?"))
        .bind(&c.user.id)
        .bind(id)
        .fetch_optional(&c.st.db)
        .await?
        .ok_or_else(|| ApiError::not_found("Episode"))
}

/// An episode as an entry of a play queue (apps put episodes in their queues too).
pub async fn queue_entry(c: &Ctx, id: &str) -> Option<Value> {
    episode(c, id).await.ok().map(|e| ep_json(&e))
}

pub async fn get_episode(c: &Ctx) -> Result<Reply, ApiError> {
    let e = episode(c, c.p.req("id")?).await?;
    ok(json!({ "podcastEpisode": ep_json(&e) }))
}

/// Subscribe to a feed (for imports): its id, and whether it's new.
pub async fn subscribe(st: &AppState, url: &str) -> Result<(String, bool)> {
    let f = parse_feed(url, &fetch_text(st, url).await?)?;
    let had: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM podcast WHERE id = ? OR url = ?").bind(&f.id).bind(url).fetch_one(&st.db).await?;
    if had > 0 {
        return Ok((f.id, false));
    }
    store(st, url, &f, true).await?;
    st.podcast_wake.notify_one();
    Ok((f.id, true))
}

/// Remove a show, its downloaded episodes and everyone's progress in it.
pub async fn unsubscribe(st: &AppState, id: &str) -> Result<()> {
    let files: Vec<String> = sqlx::query_scalar(
        "SELECT file FROM episode e WHERE podcast_id = ?1 AND file IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM episode o WHERE o.file = e.file AND o.podcast_id != ?1)",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    for f in files {
        let _ = tokio::fs::remove_file(folder(st).join(f)).await; // (not one another show's episode has too)
    }
    let title: Option<String> = sqlx::query_scalar("SELECT title FROM podcast WHERE id = ?").bind(id).fetch_optional(&st.db).await?;
    sqlx::query("DELETE FROM podcast_progress WHERE episode_id IN (SELECT id FROM episode WHERE podcast_id = ?)").bind(id).execute(&st.db).await?;
    sqlx::query("DELETE FROM podcast WHERE id = ?").bind(id).execute(&st.db).await?;
    if let Some(t) = title {
        let _ = tokio::fs::remove_dir(folder(st).join(safe(&t))).await; // only if it's empty now
    }
    Ok(())
}

/// Adding, removing, refreshing and saving shows changes them for everyone, so it's for admins (as in Navidrome).
fn need_admin(c: &Ctx) -> Result<(), ApiError> {
    if c.user.is_admin { Ok(()) } else { Err(ApiError::new(50, "Only admins can change podcasts")) }
}

pub async fn create(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let url = c.p.get("url").unwrap_or("").trim().to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(ApiError::new(10, "Give the podcast's feed address"));
    }
    let f = match fetch_text(&c.st, &url).await.and_then(|x| parse_feed(&url, &x)) {
        Ok(f) => f,
        Err(e) => return Err(ApiError::new(0, format!("Couldn't read that feed: {e:#}"))),
    };
    let n = store(&c.st, &url, &f, true).await?;
    info!(show = %f.title, episodes = n, "podcast subscribed");
    c.st.podcast_wake.notify_one();
    let s: ShowRow = sqlx::query_as(&format!("{SHOW_SELECT} WHERE id = ?")).bind(&f.id).fetch_one(&c.st.db).await?;
    ok(json!({ "podcastChannel": show_json(c, &s, false).await? }))
}

pub async fn delete_show(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    unsubscribe(&c.st, c.p.req("id")?).await?;
    empty()
}

pub async fn refresh_all(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let st = c.st.clone();
    tokio::spawn(async move { refresh(&st, None).await });
    empty()
}

pub async fn download_episode(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    sqlx::query("UPDATE episode SET pinned = 1 WHERE id = ?").bind(c.p.req("id")?).execute(&c.st.db).await?; // kept until you delete it
    c.st.podcast_wake.notify_one();
    empty()
}

pub async fn delete_episode(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let id = c.p.req("id")?;
    sqlx::query("UPDATE episode SET pinned = 0 WHERE id = ?").bind(id).execute(&c.st.db).await?;
    remove_file(&c.st, id).await;
    empty()
}

async fn set_progress(c: &Ctx, id: &str, pos: Option<i64>, done: Option<bool>) -> Result<(), ApiError> {
    sqlx::query("INSERT OR IGNORE INTO podcast_progress (user_id, episode_id, at) VALUES (?, ?, ?)")
        .bind(&c.user.id)
        .bind(id)
        .bind(now())
        .execute(&c.st.db)
        .await?;
    if let Some(p) = pos {
        sqlx::query("UPDATE podcast_progress SET position_ms = ? WHERE user_id = ? AND episode_id = ?")
            .bind(p)
            .bind(&c.user.id)
            .bind(id)
            .execute(&c.st.db)
            .await?;
    }
    if let Some(d) = done {
        sqlx::query("UPDATE podcast_progress SET done = ?, position_ms = CASE WHEN ? THEN 0 ELSE position_ms END WHERE user_id = ? AND episode_id = ?")
            .bind(d).bind(d).bind(&c.user.id).bind(id).execute(&c.st.db).await?;
    }
    sqlx::query("UPDATE podcast_progress SET at = ? WHERE user_id = ? AND episode_id = ?")
        .bind(now())
        .bind(&c.user.id)
        .bind(id)
        .execute(&c.st.db)
        .await?;
    Ok(())
}

pub async fn set_podcast_progress(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    set_progress(c, id, c.p.int("position"), c.p.get("played").map(|v| v == "true")).await?;
    empty()
}

// ── bookmarks (episodes keep their place in podcast_progress; songs in bookmark) ──

pub async fn create_bookmark(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    let pos = c.p.int("position").ok_or_else(|| ApiError::missing("position"))?;
    if id.starts_with("pe-") {
        let e = episode(c, id).await?;
        let done = e.duration > 0 && pos as f64 >= e.duration as f64 * 1000.0 * 0.95;
        set_progress(c, id, Some(pos), if done { Some(true) } else { None }).await?;
    } else {
        sqlx::query("INSERT OR REPLACE INTO bookmark (user_id, item_id, position_ms, comment) VALUES (?, ?, ?, ?)")
            .bind(&c.user.id)
            .bind(id)
            .bind(pos)
            .bind(c.p.get("comment").unwrap_or(""))
            .execute(&c.st.db)
            .await?;
    }
    empty()
}

pub async fn delete_bookmark(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    if id.starts_with("pe-") {
        sqlx::query("DELETE FROM podcast_progress WHERE user_id = ? AND episode_id = ?")
            .bind(&c.user.id)
            .bind(id)
            .execute(&c.st.db)
            .await?;
    } else {
        sqlx::query("DELETE FROM bookmark WHERE user_id = ? AND item_id = ?").bind(&c.user.id).bind(id).execute(&c.st.db).await?;
    }
    empty()
}

pub async fn get_bookmarks(c: &Ctx) -> Result<Reply, ApiError> {
    let mut out = vec![];
    let songs: Vec<(String, i64, String)> =
        sqlx::query_as("SELECT item_id, position_ms, comment FROM bookmark WHERE user_id = ?")
            .bind(&c.user.id)
            .fetch_all(&c.st.db)
            .await?;
    for (id, pos, comment) in songs {
        if let Ok(t) = super::items::track(c, &id).await {
            out.push(json!({ "position": pos, "username": c.user.name, "comment": comment, "created": db::iso(now()), "changed": db::iso(now()), "entry": super::items::song_json(&t) }));
        }
    }
    let eps: Vec<(EpRow, i64)> = {
        let rows: Vec<EpRow> = sqlx::query_as(&format!("{EP_SELECT} WHERE pr.position_ms > 0 AND pr.done = 0"))
            .bind(&c.user.id)
            .fetch_all(&c.st.db)
            .await?;
        let mut v = vec![];
        for r in rows {
            let at: i64 = sqlx::query_scalar("SELECT at FROM podcast_progress WHERE user_id = ? AND episode_id = ?")
                .bind(&c.user.id)
                .bind(&r.id)
                .fetch_one(&c.st.db)
                .await?;
            v.push((r, at));
        }
        v
    };
    for (e, at) in eps {
        out.push(json!({ "position": e.position_ms.unwrap_or(0), "username": c.user.name, "comment": "", "created": db::iso(at), "changed": db::iso(at), "entry": ep_json(&e) }));
    }
    ok(json!({ "bookmarks": { "bookmark": out } }))
}

// ── streams and covers ──

pub async fn stream(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let e = episode(c, id).await?;
    if let Some(rel) = &e.file {
        let path = folder(&c.st).join(rel);
        if path.is_file() {
            let mut req = Request::builder().uri("/");
            for h in [header::RANGE, header::IF_RANGE] {
                if let Some(v) = c.headers.get(&h) {
                    req = req.header(&h, v);
                }
            }
            let resp = match ServeFile::new(path).oneshot(req.body(Body::empty()).unwrap()).await {
                Ok(r) => r,
                Err(never) => match never {},
            };
            let (mut parts, body) = resp.into_parts();
            if let Ok(v) = e.content_type.parse() {
                parts.headers.insert(header::CONTENT_TYPE, v);
            }
            return Ok(Reply::Raw(axum::response::Response::from_parts(parts, Body::new(body))));
        }
    }
    // not saved here: pass the podcast's own server through (Range included, so seeking works)
    let mut rq = get(&c.st, &e.url).map_err(|err| ApiError::new(0, format!("Can't play this episode: {err:#}")))?;
    if let Some(r) = c.headers.get(header::RANGE) {
        rq = rq.header(header::RANGE, r);
    }
    let r = rq.send().await.map_err(|err| ApiError::new(0, format!("The podcast's server didn't answer: {err}")))?;
    let status = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = vec![];
    for k in [header::CONTENT_TYPE, header::CONTENT_LENGTH, header::CONTENT_RANGE, header::ACCEPT_RANGES] {
        if let Some(v) = r.headers().get(k.as_str()).and_then(|v| v.to_str().ok()) {
            headers.push((k, v.to_string()));
        }
    }
    Ok(raw(status, &headers, Body::from_stream(r.bytes_stream())))
}

/// The picture for a show or an episode (the show's when the episode has none), fetched once and kept.
pub async fn cover_bytes(st: &AppState, id: &str) -> Result<Option<Vec<u8>>> {
    let mut url: Option<String> = if id.starts_with("pe-") {
        sqlx::query_scalar("SELECT CASE WHEN e.image != '' THEN e.image ELSE p.image END FROM episode e JOIN podcast p ON p.id = e.podcast_id WHERE e.id = ?")
            .bind(id)
            .fetch_optional(&st.db)
            .await?
    } else {
        sqlx::query_scalar("SELECT image FROM podcast WHERE id = ?").bind(id).fetch_optional(&st.db).await?
    };
    url = url.filter(|u| u.starts_with("http://") || u.starts_with("https://"));
    let Some(url) = url else { return Ok(None) };
    let cache = st.cfg.cache_dir().join("podcast-covers").join(&hex::encode(Sha1::digest(url.as_bytes()))[..20]);
    if let Ok(b) = tokio::fs::read(&cache).await {
        return Ok(Some(b));
    }
    let r = get(st, &url)?.timeout(Duration::from_secs(20)).send().await?.error_for_status()?;
    let b = r.bytes().await?.to_vec();
    if image::guess_format(&b).is_err() {
        bail!("not a picture");
    }
    if let Some(d) = cache.parent() {
        tokio::fs::create_dir_all(d).await?;
    }
    tokio::fs::write(&cache, &b).await?;
    Ok(Some(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn feed_and_ids() {
        let xml = r#"<?xml version="1.0"?><rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel>
          <title>Show</title><itunes:author>Host</itunes:author><description>&lt;p&gt;About&lt;/p&gt;</description>
          <itunes:image href="https://x/cover.jpg"/>
          <item><title>Ep 1</title><guid>g1</guid><pubDate>Tue, 10 Nov 2026 08:00:00 +0000</pubDate>
            <itunes:duration>1:02:03</itunes:duration><enclosure url="https://x/ep1.m4a?x=1" length="123" type="audio/mp4"/></item>
          <item><title>no audio</title></item></channel></rss>"#;
        let f = parse_feed("https://x/feed", xml).unwrap();
        // same ids as music-lookup: sha1 of the feed address / show id + guid
        assert_eq!(f.id, format!("pc-{}", &hex::encode(Sha1::digest(b"https://x/feed"))[..10]));
        assert_eq!(f.episodes.len(), 1);
        assert_eq!(f.episodes[0].id, format!("pe-{}", &hex::encode(Sha1::digest(format!("{}g1", f.id).as_bytes()))[..12]));
        assert_eq!(f.episodes[0].duration, 3723);
        assert_eq!(f.episodes[0].published, 1794297600);
        assert_eq!(f.description, "About");
        assert_eq!(f.image, "https://x/cover.jpg");
        assert_eq!(ext_of(&f.episodes[0].url), ".m4a");
        assert_eq!(safe("a/b: c?"), "a_b_ c_");
    }
}
