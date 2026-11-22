//! Files: stream / download (direct with Range, or transcoded through ffmpeg), cover art, lyrics.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use lofty::prelude::*;
use serde_json::{Value, json};
use tokio_util::io::ReaderStream;
use tower::ServiceExt;
use tower_http::services::ServeFile;

use super::{ApiError, AppState, Ctx, Reply, items, ok, raw};
use crate::{config::LibraryConfig, library};

fn library_of(c: &Ctx, id: i64) -> Option<&LibraryConfig> {
    c.st.libs.iter().find(|(l, _)| *l == id).map(|(_, l)| l)
}

/// The file behind a song, or a clear error when it (or its drive) isn't there.
fn file_of(c: &Ctx, t: &items::TrackRow) -> Result<PathBuf, ApiError> {
    let path = library::track_path(&c.st.libs, t.library_id, &t.rel_path).ok_or_else(|| ApiError::not_found("File"))?;
    if path.is_file() {
        return Ok(path);
    }
    match library_of(c, t.library_id) {
        Some(l) if !library::is_online(l) => Err(ApiError::offline()),
        _ => Err(ApiError::not_found("File")),
    }
}

struct Transcode {
    codec: &'static str,
    container: &'static str,
    content_type: &'static str,
    suffix: &'static str,
}

fn target(format: &str) -> Option<Transcode> {
    Some(match format {
        "mp3" => Transcode { codec: "libmp3lame", container: "mp3", content_type: "audio/mpeg", suffix: "mp3" },
        "opus" => Transcode { codec: "libopus", container: "ogg", content_type: "audio/ogg", suffix: "opus" },
        "ogg" | "vorbis" => Transcode { codec: "libvorbis", container: "ogg", content_type: "audio/ogg", suffix: "ogg" },
        "aac" | "m4a" => Transcode { codec: "aac", container: "adts", content_type: "audio/aac", suffix: "aac" },
        _ => return None,
    })
}

pub async fn stream(c: &Ctx, download: bool) -> Result<Reply, ApiError> {
    if let Some(id) = c.p.get("id").filter(|i| i.starts_with("pe-")) {
        return super::podcasts::stream(c, id).await;
    }
    let t = items::track(c, c.p.req("id")?).await?;
    let path = file_of(c, &t)?;
    let max = c.p.int("maxBitRate").unwrap_or(0).max(0);
    let format = c.p.get("format").unwrap_or("").to_ascii_lowercase();
    let offset = c.p.int("timeOffset").unwrap_or(0).max(0);

    let mut transcode = if download || format == "raw" {
        None
    } else if !format.is_empty() && format != t.suffix {
        target(&format).map(|x| (x, if max > 0 { max } else { 192 }))
    } else if max > 0 && t.bitrate > max {
        target("mp3").map(|x| (x, max))
    } else if offset > 0 {
        target(if format.is_empty() { "mp3" } else { &format }).map(|x| (x, if max > 0 { max } else { 320 }))
    } else {
        None
    };

    if transcode.is_none() && offset > 0 && !download && format != "raw" {
        // a format it can't make (format=flac, wav…): mp3 rather than the whole file from the start, which the app
        // would play as if it began at the offset
        transcode = target("mp3").map(|x| (x, if max > 0 { max } else { 320 }));
    }
    match transcode {
        None => serve_file(c, &path, &t, download).await,
        Some((x, br)) => transcoded(c, &path, x, br.clamp(32, 512), offset).await,
    }
}

async fn serve_file(c: &Ctx, path: &Path, t: &items::TrackRow, download: bool) -> Result<Reply, ApiError> {
    let mut req = Request::builder().uri("/");
    for h in [header::RANGE, header::IF_RANGE, header::IF_MODIFIED_SINCE, header::IF_NONE_MATCH] {
        if let Some(v) = c.headers.get(&h) {
            req = req.header(&h, v);
        }
    }
    let req = req.body(Body::empty()).map_err(|e| ApiError::from(anyhow::anyhow!(e)))?;
    let resp = match ServeFile::new(path).oneshot(req).await {
        Ok(r) => r,
        Err(never) => match never {},
    };
    let (mut parts, body) = resp.into_parts();
    if parts.status.is_success()
        && let Ok(v) = t.content_type.parse()
    {
        parts.headers.insert(header::CONTENT_TYPE, v);
    }
    if download && let Ok(v) = attachment(&format!("{} - {}.{}", t.artist_name, t.title, t.suffix)).parse() {
        parts.headers.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(Reply::Raw(axum::response::Response::from_parts(parts, Body::new(body))))
}

/// Content-Disposition for saving as `name`: a plain ASCII name for old clients and the real one (RFC 6266
/// filename*), so names in any language keep their letters. (A header can only hold ASCII: before, a name with
/// "ö" or "日" made the header fail and the file was saved as "download.view".)
pub fn attachment(name: &str) -> String {
    let name: String = name.chars().map(|c| if c.is_control() || "/\\\"".contains(c) { '_' } else { c }).collect();
    let plain: String = name.chars().map(|c| if c.is_ascii() { c } else { '_' }).collect();
    let utf8: String = name
        .bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect();
    format!("attachment; filename=\"{plain}\"; filename*=UTF-8''{utf8}")
}

async fn transcoded(c: &Ctx, path: &Path, x: Transcode, kbps: i64, offset: i64) -> Result<Reply, ApiError> {
    let mut cmd = tokio::process::Command::new(c.st.cfg.ffmpeg());
    cmd.args(["-v", "error", "-nostdin"]);
    if offset > 0 {
        cmd.args(["-ss", &offset.to_string()]);
    }
    cmd.arg("-i").arg(path);
    cmd.args(["-map", "0:a:0", "-vn", "-c:a", x.codec, "-b:a", &format!("{kbps}k"), "-f", x.container, "pipe:1"]);
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| ApiError::from(anyhow::anyhow!("ffmpeg can't start ({e}); is it installed?")))?;
    let mut stdout = child.stdout.take().ok_or_else(|| ApiError::new(0, "ffmpeg gave no output"))?;
    // the first bytes before answering: when ffmpeg can't read the file (damaged, not audio, an offset past the
    // end) the app gets an error, not an empty song that "plays" as silence and counts as played
    let mut first = vec![0u8; 64 * 1024];
    let n = {
        use tokio::io::AsyncReadExt;
        stdout.read(&mut first).await.unwrap_or(0)
    };
    if n == 0 {
        let _ = child.wait().await;
        return Err(ApiError { http: StatusCode::INTERNAL_SERVER_ERROR, ..ApiError::new(0, "This song can't be transcoded (ffmpeg couldn't read it)") });
    }
    first.truncate(n);
    tokio::spawn(async move {
        let _ = child.wait().await; // ends when the app stops listening (ffmpeg gets a broken pipe)
    });
    let rest = ReaderStream::with_capacity(stdout, 64 * 1024);
    let body = futures_util::StreamExt::chain(futures_util::stream::once(async move { Ok(axum::body::Bytes::from(first)) }), rest);
    Ok(raw(
        StatusCode::OK,
        &[
            (header::CONTENT_TYPE, x.content_type.to_string()),
            (header::HeaderName::from_static("x-transcoded"), format!("{}@{kbps}k", x.suffix)),
        ],
        Body::from_stream(body),
    ))
}

// ── cover art ──

const COVER_NAMES: &[&str] = &["cover", "folder", "front", "album", "albumart", "albumartsmall"];
const IMAGE_EXT: &[&str] = &["jpg", "jpeg", "png", "webp"];

fn folder_image(dir: &Path) -> Option<PathBuf> {
    let mut best: Option<(usize, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        let (Some(stem), Some(ext)) = (p.file_stem(), p.extension()) else { continue };
        let (stem, ext) = (stem.to_string_lossy().to_lowercase(), ext.to_string_lossy().to_lowercase());
        if !IMAGE_EXT.contains(&ext.as_str()) {
            continue;
        }
        if let Some(rank) = COVER_NAMES.iter().position(|n| *n == stem)
            && best.as_ref().is_none_or(|(r, _)| rank < *r)
        {
            best = Some((rank, p));
        }
    }
    best.map(|(_, p)| p)
}

fn embedded_image(file: &Path) -> Option<Vec<u8>> {
    let tf = lofty::read_from_path(file).ok()?;
    let mut pics: Vec<&lofty::picture::Picture> = tf.tags().iter().flat_map(|t| t.pictures()).collect();
    pics.sort_by_key(|p| if p.pic_type() == lofty::picture::PictureType::CoverFront { 0 } else { 1 });
    pics.first().map(|p| p.data().to_vec())
}

fn sniff(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.len() > 12 && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

pub async fn cover_art(c: &Ctx) -> Result<Reply, ApiError> {
    let size = c.p.int("size").filter(|s| *s > 0).map(|s| s.min(2000) as u32);
    match cover_for(&c.st, c.p.req("id")?, size).await? {
        Some(b) => Ok(image_reply(b)),
        None => Err(ApiError { http: StatusCode::NOT_FOUND, ..ApiError::not_found("Cover art") }),
    }
}

/// A cover picture for any id (song, album, artist, `pl-<playlist>`), at most `size` pixels on a side.
/// Navidrome-style ids (`al-<id>_<hash>`, `mf-…`, `ar-…`) work too, for apps that remember them.
pub async fn cover_for(st: &AppState, id: &str, size: Option<u32>) -> Result<Option<Vec<u8>>, ApiError> {
    let cache = size.map(|s| st.cfg.cache_dir().join("covers").join(cache_name(id, s)));
    if let Some(p) = &cache
        && let Ok(b) = tokio::fs::read(p).await
    {
        return Ok(Some(b));
    }
    let bytes = match id.strip_prefix("pl-") {
        _ if super::podcasts::is_podcast_id(id) => super::podcasts::cover_bytes(st, id).await.unwrap_or(None),
        Some(rest) => playlist_image(st, rest.split('_').next().unwrap_or(rest)).await?,
        None => {
            let bare = ["mf-", "al-", "ar-"]
                .iter()
                .find_map(|p| id.strip_prefix(p))
                .map(|r| r.split('_').next().unwrap_or(r))
                .unwrap_or(id);
            song_image(st, bare).await?
        }
    };
    let Some(bytes) = bytes else { return Ok(None) };
    let Some(size) = size else { return Ok(Some(bytes)) };
    let resized = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let img = image::load_from_memory(&bytes)?;
        let img = if img.width() > size || img.height() > size { img.thumbnail(size, size) } else { img };
        let mut out = std::io::Cursor::new(Vec::new());
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&img.to_rgb8())?;
        Ok(out.into_inner())
    })
    .await
    .map_err(|e| ApiError::from(anyhow::anyhow!(e)))??;
    if let Some(p) = cache {
        if let Some(d) = p.parent() {
            let _ = tokio::fs::create_dir_all(d).await;
        }
        let _ = tokio::fs::write(&p, &resized).await;
    }
    Ok(Some(resized))
}

/// The resized cover's file name for an id (any text an app sends): only letters, digits, "-" and "_", so it stays
/// a name inside the cache folder everywhere. (Only "/", "\\" and "." were replaced: on Windows "C:x" made a path
/// on drive C outside it, and "x:y" an alternate data stream.)
fn cache_name(id: &str, size: u32) -> String {
    let safe: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    format!("{safe}_{size}.jpg")
}

/// A playlist's own picture (uploaded, or brought over from Navidrome).
async fn playlist_image(st: &AppState, pid: &str) -> Result<Option<Vec<u8>>, ApiError> {
    let path: Option<String> =
        sqlx::query_scalar("SELECT image_path FROM playlist WHERE id = ?").bind(pid).fetch_optional(&st.db).await?.flatten();
    Ok(match path {
        Some(p) => tokio::fs::read(st.cfg.playlist_image(&p)).await.ok(),
        None => None,
    })
}

/// The picture for a song, album or artist: an image file in the song's folder, else one inside the file.
async fn song_image(st: &AppState, id: &str) -> Result<Option<Vec<u8>>, ApiError> {
    const WHICH: [&str; 3] = [
        "SELECT library_id, rel_path FROM track WHERE id = ?",
        "SELECT t.library_id, t.rel_path FROM album al JOIN track t ON t.id = al.cover_track_id WHERE al.id = ?",
        "SELECT t.library_id, t.rel_path FROM album al JOIN track t ON t.id = al.cover_track_id
          WHERE al.artist_id = ? AND al.song_count > 0 ORDER BY al.year DESC LIMIT 1",
    ];
    let mut found = None;
    for q in WHICH {
        if let Some(r) = sqlx::query_as::<_, (i64, String)>(q).bind(id).fetch_optional(&st.db).await? {
            found = Some(r);
            break;
        }
    }
    let Some((lib, rel)) = found else { return Ok(None) };
    let file = library::track_path(&st.libs, lib, &rel).ok_or_else(|| ApiError::not_found("File"))?;
    if !file.is_file() {
        return match st.libs.iter().find(|(l, _)| *l == lib) {
            Some((_, l)) if !library::is_online(l) => Err(ApiError::offline()),
            _ => Ok(None),
        };
    }
    tokio::task::spawn_blocking(move || {
        let from_folder = file.parent().and_then(folder_image).and_then(|p| std::fs::read(p).ok());
        from_folder.or_else(|| embedded_image(&file))
    })
    .await
    .map_err(|e| ApiError::from(anyhow::anyhow!(e)))
}

fn image_reply(b: Vec<u8>) -> Reply {
    raw(
        StatusCode::OK,
        &[(header::CONTENT_TYPE, sniff(&b).to_string()), (header::CACHE_CONTROL, "public, max-age=86400".into())],
        Body::from(b),
    )
}

// ── lyrics ──

#[derive(Debug, PartialEq)]
pub struct Line {
    pub start_ms: Option<i64>,
    pub text: String,
}

/// LRC ("[01:02.50]text") into lines; plain text into unsynced lines.
pub fn parse_lyrics(s: &str) -> (bool, Vec<Line>) {
    let mut out = vec![];
    let mut synced = false;
    for raw in s.lines() {
        let mut rest = raw.trim();
        let mut stamps = vec![];
        while let Some(r) = rest.strip_prefix('[') {
            let Some(end) = r.find(']') else { break };
            let tag = &r[..end];
            if let Some(ms) = parse_stamp(tag) {
                stamps.push(ms);
            } else if tag.contains(':') && stamps.is_empty() {
                // metadata like [ar:Artist]
                rest = "";
                break;
            } else {
                break;
            }
            rest = r[end + 1..].trim_start();
        }
        if stamps.is_empty() {
            if !rest.is_empty() || !out.is_empty() {
                out.push(Line { start_ms: None, text: rest.to_string() });
            }
        } else {
            synced = true;
            for ms in stamps {
                out.push(Line { start_ms: Some(ms), text: rest.to_string() });
            }
        }
    }
    if synced {
        out.retain(|l| l.start_ms.is_some());
        out.sort_by_key(|l| l.start_ms);
    }
    (synced, out)
}

fn parse_stamp(tag: &str) -> Option<i64> {
    let (m, s) = tag.split_once(':')?;
    let m: i64 = m.trim().parse().ok()?;
    let s: f64 = s.trim().replace(',', ".").parse().ok()?;
    Some(m * 60_000 + (s * 1000.0).round() as i64)
}

async fn lyrics_text(c: &Ctx, t: &items::TrackRow) -> Result<Option<String>, ApiError> {
    let file = file_of(c, t)?;
    let own = tokio::task::spawn_blocking(move || {
        if let Ok(s) = std::fs::read_to_string(file.with_extension("lrc"))
            && !s.trim().is_empty()
        {
            return Some(s);
        }
        let tf = lofty::read_from_path(&file).ok()?;
        tf.tags().iter().find_map(|t| t.get_string(&ItemKey::Lyrics).map(str::to_string)).filter(|s| !s.trim().is_empty())
    })
    .await
    .map_err(|e| ApiError::from(anyhow::anyhow!(e)))?;
    if own.is_some() {
        return Ok(own);
    }
    // none of its own: LRCLIB (remembered)
    Ok(crate::online::lyrics(&c.st, &t.id, &t.title, &t.artist_name, &t.album_name, t.duration_ms / 1000).await)
}

pub async fn lyrics_by_song(c: &Ctx) -> Result<Reply, ApiError> {
    let t = items::track(c, c.p.req("id")?).await?;
    let list: Vec<Value> = match lyrics_text(c, &t).await? {
        None => vec![],
        Some(s) => {
            let (synced, lines) = parse_lyrics(&s);
            vec![json!({
                "lang": "und",
                "synced": synced,
                "displayArtist": t.artist_name,
                "displayTitle": t.title,
                "offset": 0,
                "line": lines.iter().map(|l| match l.start_ms {
                    Some(ms) => json!({ "start": ms, "value": l.text }),
                    None => json!({ "value": l.text }),
                }).collect::<Vec<_>>(),
            })]
        }
    };
    ok(json!({ "lyricsList": { "structuredLyrics": list } }))
}

pub async fn lyrics(c: &Ctx) -> Result<Reply, ApiError> {
    let artist = c.p.get("artist").unwrap_or("");
    let title = c.p.get("title").unwrap_or("");
    let found = items::tracks_where(
        c,
        "t.title = ? COLLATE NOCASE AND (? = '' OR t.artist_name = ? COLLATE NOCASE)",
        &[title, artist, artist],
        "ORDER BY t.has_lyrics DESC LIMIT 1",
    )
    .await?;
    let Some(t) = found.into_iter().next() else {
        return ok(json!({ "lyrics": {} }));
    };
    let text = lyrics_text(c, &t).await?.map(|s| {
        let (_, lines) = parse_lyrics(&s);
        lines.into_iter().map(|l| l.text).collect::<Vec<_>>().join("\n")
    });
    ok(json!({ "lyrics": { "artist": t.artist_name, "title": t.title, "value": text.unwrap_or_default() } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cover_cache_names_stay_in_the_folder() {
        assert_eq!(cache_name("al-1f2e_3a", 300), "al-1f2e_3a_300.jpg");
        for id in ["C:secret", "x:stream", "..\\..\\x", "../../x", "\\\\?\\C:\\x"] {
            let n = cache_name(id, 300);
            assert!(n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) && !n.contains(".."), "{id} → {n}");
            assert_eq!(std::path::Path::new("covers").join(&n).parent(), Some(std::path::Path::new("covers")));
        }
    }

    #[test]
    fn download_names() {
        assert_eq!(attachment("A - B.flac"), "attachment; filename=\"A - B.flac\"; filename*=UTF-8''A%20-%20B.flac");
        assert_eq!(attachment("Björk - Jóga/\"x\".flac"), "attachment; filename=\"Bj_rk - J_ga__x_.flac\"; filename*=UTF-8''Bj%C3%B6rk%20-%20J%C3%B3ga__x_.flac");
        assert!(attachment("日本.mp3").parse::<axum::http::HeaderValue>().is_ok());
    }

    #[test]
    fn lrc() {
        let (s, l) = parse_lyrics("[ar:Someone]\n[00:01.50]one\n[00:03.00][00:05.25]two\n");
        assert!(s);
        assert_eq!(l.len(), 3);
        assert_eq!(l[0], Line { start_ms: Some(1500), text: "one".into() });
        assert_eq!(l[2].start_ms, Some(5250));
        let (s, l) = parse_lyrics("plain\nwords");
        assert!(!s);
        assert_eq!(l.len(), 2);
    }
}
