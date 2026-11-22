//! Artist biographies and album notes from free sources that need no key: MusicBrainz (the artist / release
//! group, found by the MusicBrainz id in the tags or by name) → Wikidata (which Wikipedia page that is) →
//! Wikipedia's page summary. Album pictures from the Cover Art Archive. Answers are kept (catalog_cache: found
//! for 30 days, not found for 7), so each artist or album is looked up once. MusicBrainz is asked at most once
//! a second, by everything in Conductor together (concerts use the same queue).
//!
//! Off with `info_online = false` (CONDUCTOR_INFO_ONLINE=0). Language: `info_lang` (Wikipedia's code, e.g. "en",
//! "ru"), with English when that language has no page.

use std::{
    sync::LazyLock,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

use crate::{api::AppState, db::now};

pub const UA: &str = "Conductor/0.1 (self-hosted music server; https://github.com/d-safro/conductor)";
const FOUND_DAYS: i64 = 30;
const MISSING_DAYS: i64 = 7;

static HTTP: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder().user_agent(UA).timeout(Duration::from_secs(15)).build().expect("http client")
});
/// When the next MusicBrainz request may go (their rule: one a second).
static MB_NEXT: LazyLock<tokio::sync::Mutex<Instant>> = LazyLock::new(|| tokio::sync::Mutex::new(Instant::now()));

async fn get_json(url: &str, q: &[(&str, &str)]) -> Result<Option<Value>, String> {
    let r = HTTP.get(url).query(q).header("accept", "application/json").send().await.map_err(|e| e.to_string())?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !r.status().is_success() {
        return Err(format!("HTTP {} from {}", r.status().as_u16(), r.url().host_str().unwrap_or("")));
    }
    r.json().await.map(Some).map_err(|e| e.to_string())
}

/// A MusicBrainz web service call (`path` like "artist/<id>"), queued so the whole server asks once a second.
pub async fn mb_get(st: &AppState, path: &str, q: &[(&str, &str)]) -> Result<Option<Value>, String> {
    let base = st.cfg.musicbrainz_url.trim_end_matches('/');
    if base.starts_with("https://musicbrainz.org") {
        let mut next = MB_NEXT.lock().await;
        let wait = next.saturating_duration_since(Instant::now());
        tokio::time::sleep(wait).await;
        *next = Instant::now() + Duration::from_millis(1100);
    }
    let mut q = q.to_vec();
    q.push(("fmt", "json"));
    let url = format!("{base}/{path}");
    // "slow down" (503, which MusicBrainz also answers when it's busy): twice more, after longer pauses. A lookup
    // goes on in the background after the app stops waiting, so this costs the app nothing.
    let mut r = get_json(&url, &q).await;
    for pause in [2, 5] {
        match &r {
            Err(e) if e.starts_with("HTTP 503") => {
                tokio::time::sleep(Duration::from_secs(pause)).await;
                r = get_json(&url, &q).await;
            }
            _ => break,
        }
    }
    r
}

/// Lower case, letters and digits only: for comparing names.
fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace(['"', '\\'], ""))
}

async fn cache_get(st: &AppState, kind: &str, key: &str) -> Option<Option<Value>> {
    let (v, at) = sqlx::query_as::<_, (Option<String>, i64)>("SELECT value, at FROM catalog_cache WHERE kind = ? AND key = ?")
        .bind(kind)
        .bind(key)
        .fetch_optional(&st.db)
        .await
        .ok()
        .flatten()?;
    let days = if v.is_some() { FOUND_DAYS } else { MISSING_DAYS };
    (now() - at < days * 86_400).then(|| v.and_then(|s| serde_json::from_str(&s).ok()))
}

async fn cache_put(st: &AppState, kind: &str, key: &str, v: Option<&Value>) {
    let _ = sqlx::query("INSERT OR REPLACE INTO catalog_cache (kind, key, value, at) VALUES (?, ?, ?, ?)")
        .bind(kind)
        .bind(key)
        .bind(v.map(Value::to_string))
        .bind(now())
        .execute(&st.db)
        .await;
}

/// The Wikidata item an entity's url relations point to ("Q123"), or a Wikipedia page named there directly.
fn wikidata_of(mb: &Value) -> (Option<String>, Option<String>) {
    let mut wd = None;
    let mut wp = None;
    for r in mb["relations"].as_array().into_iter().flatten() {
        let url = r["url"]["resource"].as_str().unwrap_or("");
        match r["type"].as_str() {
            Some("wikidata") => wd = url.rsplit('/').next().filter(|q| q.starts_with('Q')).map(String::from),
            Some("wikipedia") => wp = Some(url.to_string()),
            _ => {}
        }
    }
    (wd, wp)
}

/// The Wikipedia page summary for a Wikidata item, in `info_lang` or else English:
/// {"text", "url", "image", "lang"}.
async fn wikipedia(st: &AppState, qid: Option<&str>, direct: Option<&str>) -> Result<Option<Value>, String> {
    let lang = st.cfg.info_lang.trim().to_lowercase();
    let mut pages: Vec<(String, String)> = vec![]; // (lang, title)
    if let Some(q) = qid {
        let url = format!("{}/wiki/Special:EntityData/{q}.json", st.cfg.wikidata_url.trim_end_matches('/'));
        if let Some(j) = get_json(&url, &[]).await? {
            let links = &j["entities"][q]["sitelinks"];
            for l in [lang.as_str(), "en"] {
                if let Some(t) = links[format!("{l}wiki")]["title"].as_str() {
                    pages.push((l.to_string(), t.to_string()));
                }
            }
        }
    }
    if pages.is_empty()
        && let Some(u) = direct
        && let Some((host, title)) = u.split_once(".wikipedia.org/wiki/")
    {
        let l = host.rsplit('/').next().unwrap_or("en");
        pages.push((l.to_string(), urlencoding_decode(title)));
    }
    for (l, title) in pages {
        let base = st.cfg.wikipedia_url.replace("{lang}", &l);
        let t = title.replace(' ', "_");
        let url = format!("{}/api/rest_v1/page/summary/{}", base.trim_end_matches('/'), encode_path(&t));
        let Some(j) = get_json(&url, &[]).await? else { continue };
        let text = j["extract"].as_str().unwrap_or("").trim();
        if text.is_empty() || j["type"] == "disambiguation" {
            continue;
        }
        let page = j["content_urls"]["desktop"]["page"].as_str().map(String::from).unwrap_or(format!("{base}/wiki/{t}"));
        return Ok(Some(json!({
            "text": text,
            "url": page,
            "image": j["originalimage"]["source"].as_str().or(j["thumbnail"]["source"].as_str()),
            "lang": l,
        })));
    }
    Ok(None)
}

fn encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~()!*',".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn urlencoding_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(if b[i] == b'_' { b' ' } else { b[i] });
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ───────────── artists ─────────────

async fn find_artist_mbid(st: &AppState, name: &str) -> Result<Option<String>, String> {
    let Some(j) = mb_get(st, "artist", &[("query", &format!("artist:{}", quoted(name))), ("limit", "5")]).await? else {
        return Ok(None);
    };
    Ok(j["artists"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            a["score"].as_i64().unwrap_or(0) >= 95
                && (norm(a["name"].as_str().unwrap_or("")) == norm(name)
                    || a["aliases"].as_array().into_iter().flatten().any(|x| norm(x["name"].as_str().unwrap_or("")) == norm(name)))
        })
        .and_then(|a| a["id"].as_str())
        .map(String::from))
}

async fn fetch_artist(st: &AppState, name: &str, mbid: Option<&str>) -> Result<Option<Value>, String> {
    let mbid = match mbid.filter(|m| !m.is_empty()) {
        Some(m) => m.to_string(),
        None => match find_artist_mbid(st, name).await? {
            Some(m) => m,
            None => return Ok(None),
        },
    };
    let Some(a) = mb_get(st, &format!("artist/{mbid}"), &[("inc", "url-rels")]).await? else { return Ok(None) };
    let (wd, wp) = wikidata_of(&a);
    let mut out = wikipedia(st, wd.as_deref(), wp.as_deref()).await?.unwrap_or(json!({}));
    out["mbid"] = mbid.into();
    Ok(Some(out))
}

/// What's known about an artist: {"mbid", "text", "url", "image", "lang"} (text and url only when Wikipedia has
/// a page). Waits for the answer at most `wait`; a slower lookup goes on and is kept for next time.
pub async fn artist(st: &std::sync::Arc<AppState>, name: &str, mbid: Option<&str>, wait: Duration) -> Option<Value> {
    if !st.cfg.info_online || name.is_empty() || name.starts_with('[') {
        return None;
    }
    let key = format!("{}|{}", st.cfg.info_lang, mbid.filter(|m| !m.is_empty()).map(String::from).unwrap_or_else(|| norm(name)));
    if let Some(v) = cache_get(st, "wiki_artist", &key).await {
        return v;
    }
    let (st2, name, mbid) = (st.clone(), name.to_string(), mbid.map(String::from));
    let job = tokio::spawn(async move {
        match fetch_artist(&st2, &name, mbid.as_deref()).await {
            Ok(v) => {
                cache_put(&st2, "wiki_artist", &key, v.as_ref()).await;
                v
            }
            Err(e) => {
                tracing::debug!(artist = %name, error = %e, "artist info lookup failed");
                None
            }
        }
    });
    tokio::time::timeout(wait, job).await.ok().and_then(|r| r.ok()).flatten()
}

// ───────────── albums ─────────────

async fn find_release_group(st: &AppState, artist: &str, title: &str) -> Result<Option<String>, String> {
    let q = format!("releasegroup:{} AND artist:{}", quoted(title), quoted(artist));
    let Some(j) = mb_get(st, "release-group", &[("query", &q), ("limit", "5")]).await? else { return Ok(None) };
    Ok(j["release-groups"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|g| g["score"].as_i64().unwrap_or(0) >= 90 && norm(g["title"].as_str().unwrap_or("")) == norm(title))
        .and_then(|g| g["id"].as_str())
        .map(String::from))
}

async fn fetch_album(st: &AppState, artist: &str, title: &str, release: Option<&str>) -> Result<Option<Value>, String> {
    let mut rg = None;
    if let Some(r) = release.filter(|m| !m.is_empty())
        && let Some(j) = mb_get(st, &format!("release/{r}"), &[("inc", "release-groups")]).await?
    {
        rg = j["release-group"]["id"].as_str().map(String::from);
    }
    if rg.is_none() {
        rg = find_release_group(st, artist, title).await?;
    }
    let Some(rg) = rg else { return Ok(None) };
    let Some(g) = mb_get(st, &format!("release-group/{rg}"), &[("inc", "url-rels")]).await? else { return Ok(None) };
    let (wd, wp) = wikidata_of(&g);
    let mut out = wikipedia(st, wd.as_deref(), wp.as_deref()).await?.unwrap_or(json!({}));
    out["mbid"] = release.filter(|m| !m.is_empty()).unwrap_or(&rg).into();
    out["releaseGroup"] = rg.clone().into();
    // pictures: the Cover Art Archive's front cover of the release group, when it has one
    let caa = st.cfg.coverart_url.trim_end_matches('/');
    if let Ok(Some(j)) = get_json(&format!("{caa}/release-group/{rg}"), &[]).await
        && let Some(front) = j["images"].as_array().into_iter().flatten().find(|i| i["front"] == true)
    {
        let th = &front["thumbnails"];
        out["images"] = json!({
            "small": th["250"].as_str().or(th["small"].as_str()),
            "medium": th["500"].as_str().or(th["large"].as_str()),
            "large": th["1200"].as_str().or(front["image"].as_str()),
        });
    }
    Ok(Some(out))
}

/// What's known about an album: {"mbid", "releaseGroup", "text", "url", "images": {small, medium, large}}.
pub async fn album(
    st: &std::sync::Arc<AppState>,
    artist: &str,
    title: &str,
    release: Option<&str>,
    wait: Duration,
) -> Option<Value> {
    if !st.cfg.info_online || title.is_empty() || title.starts_with('[') {
        return None;
    }
    let key = format!(
        "{}|{}",
        st.cfg.info_lang,
        release.filter(|m| !m.is_empty()).map(String::from).unwrap_or_else(|| format!("{}|{}", norm(artist), norm(title)))
    );
    if let Some(v) = cache_get(st, "wiki_album", &key).await {
        return v;
    }
    let (st2, artist, title, release) = (st.clone(), artist.to_string(), title.to_string(), release.map(String::from));
    let job = tokio::spawn(async move {
        match fetch_album(&st2, &artist, &title, release.as_deref()).await {
            Ok(v) => {
                cache_put(&st2, "wiki_album", &key, v.as_ref()).await;
                v
            }
            Err(e) => {
                tracing::debug!(album = %title, error = %e, "album info lookup failed");
                None
            }
        }
    });
    tokio::time::timeout(wait, job).await.ok().and_then(|r| r.ok()).flatten()
}

/// Subsonic's biography / notes: the summary and where it's from, as Navidrome writes Last.fm's.
pub fn as_html(v: &Value) -> Option<String> {
    let text = v["text"].as_str().filter(|t| !t.is_empty())?;
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let url = v["url"].as_str().unwrap_or("");
    Some(format!("{} <a target=\"_blank\" href=\"{}\" rel=\"nofollow\">Read more on Wikipedia</a>", esc(text), esc(url).replace('"', "&quot;")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_and_names() {
        let mb = json!({"relations": [
            {"type": "wikidata", "url": {"resource": "https://www.wikidata.org/wiki/Q1299"}},
            {"type": "official homepage", "url": {"resource": "https://example.com"}}]});
        assert_eq!(wikidata_of(&mb), (Some("Q1299".into()), None));
        assert_eq!(encode_path("AC/DC_(band)"), "AC%2FDC_(band)");
        assert_eq!(urlencoding_decode("Simon_%26_Garfunkel"), "Simon & Garfunkel");
        assert_eq!(norm("The Beatles!"), "thebeatles");
        let h = as_html(&json!({"text": "A <b> & B", "url": "https://en.wikipedia.org/wiki/X"})).unwrap();
        assert!(h.starts_with("A &lt;b&gt; &amp; B <a"), "{h}");
    }
}
