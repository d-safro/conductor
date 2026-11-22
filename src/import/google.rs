//! YouTube Music from Google Takeout ("YouTube and YouTube Music"):
//! - music (library and uploads)/music library songs.csv: the songs in your library (→ liked), with titles.
//! - playlists/<name>-videos.csv: each playlist's video ids in order ("Liked music" → liked songs).
//! - history/watch-history.json: what you played, only the entries marked "YouTube Music" (not other YouTube).
//!
//! Takeout's folder names are in your language, so files are recognised by their columns. Playlist files only have
//! video ids: the titles come from the library file and the history. The HTML history can't be read: choose JSON
//! for "history" in Takeout.

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;

use super::{Files, Parsed, Play, Playlist, Song, csv::Table, parse_time, stem, text};

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Watched {
    header: String,
    title: String,
    title_url: String,
    subtitles: Vec<Sub>,
    time: String,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Sub {
    name: String,
}

fn video_id(url: &str) -> Option<String> {
    let (_, q) = url.split_once("v=")?;
    let id: String = q.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (id.len() == 11).then_some(id)
}

fn artist_of(name: &str) -> String {
    name.trim().trim_end_matches(" - Topic").trim().to_string()
}

pub fn parse(files: &Files, out: &mut Parsed) -> Result<bool> {
    let mut mine = false;
    let mut known: HashMap<String, Song> = HashMap::new();
    // the library: titles for the ids
    for (_, b) in files.named(|n| n.to_lowercase().ends_with(".csv")) {
        let Some(t) = Table::parse(&text(b), &["Video ID", "Video Id"]) else { continue };
        if !t.has(&["Song Title"]) {
            continue;
        }
        out.sources.insert("YouTube Music library (Takeout)".into());
        mine = true;
        for r in &t.rows {
            let id = t.get(r, &["Video ID", "Video Id"]).to_string();
            let mut s = Song::named(
                t.get(r, &["Song Title"]),
                &artist_of(t.get(r, &["Artist Name 1", "Artist Names", "Artist Name"]).split(',').next().unwrap_or("")),
                t.get(r, &["Album Title"]),
            );
            s.youtube = Some(id.clone()).filter(|i| !i.is_empty());
            out.liked.push((s.clone(), None));
            known.insert(id, s);
        }
    }
    // what you played on YouTube Music
    let mut history = vec![];
    for (path, b) in files.named(|n| n.to_lowercase().starts_with("watch-history") || n.to_lowercase().starts_with("watch history")) {
        if path.to_lowercase().ends_with(".html") {
            out.notes.push("The YouTube history is in HTML: export it again choosing JSON for \"history\" in Takeout.".into());
            continue;
        }
        let Ok(rows) = serde_json::from_slice::<Vec<Watched>>(text(b).as_bytes()) else { continue };
        out.sources.insert("YouTube Music history (Takeout)".into());
        mine = true;
        for w in rows.into_iter().filter(|w| w.header == "YouTube Music") {
            let Some(at) = parse_time(&w.time) else { continue };
            let id = video_id(&w.title_url);
            let song = match id.as_ref().and_then(|i| known.get(i)) {
                Some(s) => s.clone(),
                None => {
                    let title = w.title.strip_prefix("Watched ").unwrap_or(&w.title);
                    let mut s = Song::named(title, &w.subtitles.first().map(|x| artist_of(&x.name)).unwrap_or_default(), "");
                    s.youtube = id.clone();
                    s
                }
            };
            if let Some(i) = &id {
                known.entry(i.clone()).or_insert_with(|| song.clone());
            }
            history.push(Play { song, at, ms_played: None });
        }
    }
    out.plays.extend(history);
    // playlists: ids in order
    let mut untitled = 0;
    for (path, b) in files.named(|n| n.to_lowercase().ends_with(".csv")) {
        let Some(t) = Table::parse(&text(b), &["Video ID", "Video Id"]) else { continue };
        if t.has(&["Song Title"]) || t.has(&["Playlist ID", "Playlist Id"]) {
            continue;
        }
        mine = true;
        out.sources.insert("YouTube Music playlists (Takeout)".into());
        let name = stem(path).trim_end_matches("-videos").trim_end_matches(" videos").trim().to_string();
        let lower = name.to_lowercase();
        if lower.starts_with("liked videos") || lower.starts_with("watch later") {
            continue; // YouTube, not music
        }
        let mut songs = vec![];
        for r in &t.rows {
            let id = t.get(r, &["Video ID", "Video Id"]);
            match known.get(id) {
                Some(s) => songs.push(s.clone()),
                None => untitled += 1,
            }
        }
        if lower.starts_with("liked music") {
            out.liked.extend(songs.into_iter().map(|s| (s, None)));
        } else {
            out.playlists.push(Playlist { name, description: String::new(), songs });
        }
    }
    if untitled > 0 {
        out.notes.push(format!(
            "{untitled} playlist songs only have a YouTube video id in Takeout (not in your library or history), so they can't be matched."
        ));
    }
    Ok(mine)
}

#[cfg(test)]
mod tests {
    use crate::import::{Files, parse_files};

    #[test]
    fn takeout() {
        let f = |n: &str, b: &str| (n.to_string(), b.as_bytes().to_vec());
        let p = parse_files(&Files {
            files: vec![
                f(
                    "Takeout/YouTube and YouTube Music/music (library and uploads)/music library songs.csv",
                    "Video ID,Song Title,Album Title,Artist Name 1\nAAAAAAAAAA1,Once in a Lifetime,Remain in Light,Talking Heads\n",
                ),
                f(
                    "Takeout/YouTube and YouTube Music/playlists/Road-videos.csv",
                    "Video ID,Playlist Video Creation Timestamp\nBBBBBBBBBB2,2024-01-01T00:00:00+00:00\nAAAAAAAAAA1,2024-01-01T00:00:00+00:00\nCCCCCCCCCC3,2024-01-01T00:00:00+00:00\n",
                ),
                f("Takeout/YouTube and YouTube Music/playlists/playlists.csv", "Playlist ID,Playlist Title (Original)\nPL1,Road\n"),
                f("Takeout/YouTube and YouTube Music/playlists/Liked music-videos.csv", "Video ID,Playlist Video Creation Timestamp\nBBBBBBBBBB2,2024-01-01T00:00:00+00:00\n"),
                f(
                    "Takeout/YouTube and YouTube Music/history/watch-history.json",
                    r#"[{"header":"YouTube Music","title":"Watched Psycho Killer","titleUrl":"https://music.youtube.com/watch?v=BBBBBBBBBB2",
                         "subtitles":[{"name":"Talking Heads - Topic","url":"x"}],"time":"2024-02-01T10:00:00.000Z","products":["YouTube"],"activityControls":["YouTube watch history"]},
                        {"header":"YouTube","title":"Watched a cat video","titleUrl":"https://www.youtube.com/watch?v=DDDDDDDDDD4","time":"2024-02-01T11:00:00Z"}]"#,
                ),
            ],
        })
        .unwrap();
        assert_eq!(p.plays.len(), 1, "only YouTube Music");
        assert_eq!((p.plays[0].song.title.as_str(), p.plays[0].song.artist.as_str()), ("Psycho Killer", "Talking Heads"));
        assert_eq!(p.playlists.len(), 1);
        let t: Vec<&str> = p.playlists[0].songs.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(t, ["Psycho Killer", "Once in a Lifetime"], "titles from history and library, in order");
        assert_eq!(p.liked.len(), 2, "library songs and Liked music");
        assert!(p.notes.iter().any(|n| n.starts_with("1 playlist songs")));
    }
}
