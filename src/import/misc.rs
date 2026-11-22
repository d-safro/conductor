//! ListenBrainz's export, and plain playlists:
//! - ListenBrainz (listenbrainz.org → settings → export): listens as JSON (one array) or JSON Lines (one file per
//!   month in newer zips), each with its time, names and, often, MusicBrainz ids / ISRC; feedback.jsonl's loved
//!   recordings (score 1) → liked.
//! - M3U / M3U8 playlists (#EXTINF names, or the file paths, matched to your files first).
//! - CSV playlists with title and artist columns (any app's export; the file name is the playlist's).

use anyhow::Result;
use serde_json::Value;

use super::{Files, Parsed, Play, Playlist, Song, csv::Table, stem, text};

fn listens_of(v: &Value, out: &mut Vec<Play>) {
    let Some(at) = v["listened_at"].as_i64() else { return };
    let m = &v["track_metadata"];
    let info = &m["additional_info"];
    let map = &m["mbid_mapping"];
    let mut s = Song::named(m["track_name"].as_str().unwrap_or(""), m["artist_name"].as_str().unwrap_or(""), m["release_name"].as_str().unwrap_or(""));
    s.mbid = map["recording_mbid"].as_str().or(info["recording_mbid"].as_str()).filter(|x| !x.is_empty()).map(String::from);
    s.isrc = info["isrc"].as_str().filter(|i| i.len() == 12).map(str::to_uppercase);
    s.duration_ms = info["duration_ms"].as_i64().or(info["duration"].as_i64().map(|d| d * 1000)).unwrap_or(0);
    s.spotify = info["spotify_id"].as_str().and_then(super::spotify_id);
    out.push(Play { song: s, at, ms_played: None });
}

pub fn listenbrainz(files: &Files, out: &mut Parsed) -> Result<bool> {
    let mut mine = false;
    for (path, b) in files.named(|n| {
        let l = n.to_lowercase();
        l.ends_with(".json") || l.ends_with(".jsonl")
    }) {
        let t = text(b);
        let head = t.trim_start();
        if !head.contains("listened_at") && !path.to_lowercase().contains("feedback") {
            continue;
        }
        let rows: Vec<Value> = if head.starts_with('[') {
            serde_json::from_str(head).unwrap_or_default()
        } else {
            head.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
        };
        let mut plays = vec![];
        for r in &rows {
            if r.get("listened_at").is_some() {
                listens_of(r, &mut plays);
            } else if r["score"].as_i64() == Some(1)
                && let Some(mbid) = r["recording_mbid"].as_str().filter(|m| !m.is_empty())
            {
                let s = Song { mbid: Some(mbid.to_string()), ..Default::default() };
                out.liked.push((s, r["created"].as_i64()));
                mine = true;
            }
        }
        if !plays.is_empty() {
            out.sources.insert("ListenBrainz export".into());
            out.plays.extend(plays);
            mine = true;
        }
    }
    Ok(mine)
}

/// "03 - Artist - Title", "Artist - Title" → (artist, title).
fn from_name(name: &str) -> (String, String) {
    let n = name.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start_matches(['.', ' ', '-', '_']).trim();
    match n.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), n.to_string()),
    }
}

fn m3u(name: &str, body: &str) -> Playlist {
    let mut p = Playlist { name: name.to_string(), description: String::new(), songs: vec![] };
    let mut info: Option<(i64, String)> = None;
    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(r) = line.strip_prefix("#PLAYLIST:") {
            p.name = r.trim().to_string();
        } else if let Some(r) = line.strip_prefix("#EXTINF:") {
            let (secs, label) = r.split_once(',').unwrap_or((r, ""));
            info = Some((secs.trim().parse::<f64>().map(|s| s as i64).unwrap_or(-1), label.trim().to_string()));
        } else if !line.starts_with('#') {
            let path = line.replace('\\', "/");
            let file = path.rsplit('/').next().unwrap_or(&path);
            let file_stem = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file);
            let parts: Vec<&str> = path.split('/').collect();
            let (mut artist, mut title) = from_name(file_stem);
            let mut secs = -1;
            if let Some((s, label)) = info.take() {
                secs = s;
                if !label.is_empty() {
                    (artist, title) = from_name(&label);
                }
            }
            let album = if parts.len() >= 2 { parts[parts.len() - 2].to_string() } else { String::new() };
            if artist.is_empty() && parts.len() >= 3 {
                artist = parts[parts.len() - 3].to_string(); // …/Artist/Album/01 Title.flac
            }
            let mut s = Song::named(&title, &artist, &album);
            s.duration_ms = if secs > 0 { secs * 1000 } else { 0 };
            if !line.starts_with("http://") && !line.starts_with("https://") {
                s.path = Some(path.clone());
            }
            p.songs.push(s);
        }
    }
    p
}

/// Columns other formats own: those files are theirs.
const OTHERS: &[&str] = &["Track URI", "Spotify ID", "Video ID", "Video Id", "Event Type", "Event Start Timestamp", "Playlist ID"];

pub fn playlists(files: &Files, out: &mut Parsed) -> Result<bool> {
    let mut mine = false;
    for (path, b) in files.named(|n| {
        let l = n.to_lowercase();
        l.ends_with(".m3u") || l.ends_with(".m3u8")
    }) {
        let p = m3u(stem(path), &text(b));
        if !p.songs.is_empty() {
            out.sources.insert("M3U playlists".into());
            out.playlists.push(p);
            mine = true;
        }
    }
    for (path, b) in files.named(|n| n.to_lowercase().ends_with(".csv")) {
        let txt = text(b);
        let Some(t) = Table::parse(&txt, &["title", "track", "track name", "song", "song title", "name"]) else { continue };
        if t.has(OTHERS) || !t.has(&["artist", "artist name", "artists", "artist name(s)", "performer"]) {
            continue;
        }
        let songs: Vec<Song> = t
            .rows
            .iter()
            .map(|r| {
                let mut s = Song::named(
                    t.get(r, &["title", "track", "track name", "song", "song title", "name"]),
                    t.get(r, &["artist", "artist name", "artists", "artist name(s)", "performer"]).split([',', ';']).next().unwrap_or(""),
                    t.get(r, &["album", "album name", "album title", "release"]),
                );
                s.isrc = Some(t.get(r, &["isrc"]).to_uppercase()).filter(|i| i.len() == 12);
                let d = t.get(r, &["duration (ms)", "duration_ms", "length (ms)"]).parse::<i64>().ok()
                    .or_else(|| t.get(r, &["duration", "length", "time"]).split(':').try_fold(0i64, |acc, x| x.trim().parse::<i64>().ok().map(|v| acc * 60 + v)).map(|s| s * 1000));
                s.duration_ms = d.unwrap_or(0);
                s
            })
            .collect();
        if !songs.is_empty() {
            out.sources.insert("CSV playlists".into());
            out.playlists.push(Playlist { name: stem(path).replace('_', " "), description: String::new(), songs });
            mine = true;
        }
    }
    Ok(mine)
}

#[cfg(test)]
mod tests {
    use crate::import::{Files, parse_files};

    fn files(list: &[(&str, &str)]) -> Files {
        Files { files: list.iter().map(|(n, b)| (n.to_string(), b.as_bytes().to_vec())).collect() }
    }

    #[test]
    fn listenbrainz_both_shapes() {
        let listen = r#"{"listened_at":1700000000,"track_metadata":{"artist_name":"Talking Heads","track_name":"Once in a Lifetime","release_name":"Remain in Light",
            "additional_info":{"recording_mbid":"rec-1","duration_ms":259000,"isrc":"usew10000001"},"mbid_mapping":{"recording_mbid":"rec-2"}},"user_name":"x"}"#;
        let p = parse_files(&files(&[
            ("listens/2023/11.jsonl", &format!("{0}\n{0}", listen.replace('\n', " "))),
            ("old/listens.json", &format!("[{listen}]")),
            ("feedback.jsonl", "{\"score\":1,\"recording_mbid\":\"rec-9\",\"created\":1600000000}\n{\"score\":-1,\"recording_mbid\":\"rec-8\"}"),
        ]))
        .unwrap();
        assert_eq!(p.plays.len(), 3);
        let s = &p.plays[0].song;
        assert_eq!((s.mbid.as_deref(), s.isrc.as_deref(), s.duration_ms), (Some("rec-2"), Some("USEW10000001"), 259000));
        assert_eq!(p.liked.len(), 1);
        assert_eq!(p.liked[0].0.mbid.as_deref(), Some("rec-9"));
    }

    #[test]
    fn m3u_and_csv() {
        let m3u = "#EXTM3U\n#PLAYLIST:Evening\n#EXTINF:259,Talking Heads - Once in a Lifetime\nAlbums/Talking Heads/Remain in Light/03 Once in a Lifetime.flac\n\
                   C:\\Music\\Talking Heads\\Fear of Music\\05 Heaven.mp3\nhttps://radio.example/stream\n";
        let csv = "Title,Artist,Album,Duration\nOnce in a Lifetime,\"Talking Heads, Eno\",Remain in Light,4:19\n";
        let p = parse_files(&files(&[("evening.m3u8", m3u), ("Morning_Mix.csv", csv), ("Liked_Songs.csv", "\"Track URI\",\"Track Name\",\"Artist Name(s)\"\n")])).unwrap();
        let ev = p.playlists.iter().find(|x| x.name == "Evening").unwrap();
        assert_eq!(ev.songs.len(), 3);
        assert_eq!((ev.songs[0].artist.as_str(), ev.songs[0].duration_ms), ("Talking Heads", 259000));
        assert_eq!((ev.songs[1].title.as_str(), ev.songs[1].artist.as_str(), ev.songs[1].album.as_str()), ("Heaven", "Talking Heads", "Fear of Music"));
        assert_eq!(ev.songs[1].path.as_deref(), Some("C:/Music/Talking Heads/Fear of Music/05 Heaven.mp3"));
        let mm = p.playlists.iter().find(|x| x.name == "Morning Mix").unwrap();
        assert_eq!((mm.songs[0].artist.as_str(), mm.songs[0].duration_ms), ("Talking Heads", 259000));
        assert_eq!(p.playlists.len(), 2, "the Exportify file isn't a plain CSV playlist");
    }
}
