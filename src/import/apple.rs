//! Apple Music / iTunes:
//! - iTunes "Library.xml" (File → Library → Export Library; Music on a Mac the same): loved songs, star ratings
//!   (not ones inherited from the album), play counts with the last play, your playlists (not the built-in or smart
//!   ones), and podcasts you have.
//! - Apple's privacy export (privacy.apple.com → "Apple Media Services information", zips inside a zip):
//!   "Apple Music Play Activity.csv" (each play, its start and how long it played), "Apple Music Library
//!   Tracks.json" (play counts, loved songs, ratings) and "Apple Music Library Playlists.json".
//!
//! The privacy export's columns have changed over the years, so they're looked up by several names. Only titles,
//! artists, albums, times, lengths, counts, ratings and loves are read: IP addresses, devices, store front, etc. never.

use std::collections::HashMap;

use anyhow::Result;
use serde_json::{Map, Value};

use super::{Files, Parsed, Play, Playlist, Podcast, Song, csv::Table, parse_time, text};

// ───────────── plist (XML) ─────────────

fn plist_value(n: roxmltree::Node) -> Value {
    match n.tag_name().name() {
        "dict" => {
            let mut m = Map::new();
            let kids: Vec<_> = n.children().filter(|c| c.is_element()).collect();
            for pair in kids.chunks(2) {
                if let [k, v] = pair
                    && k.tag_name().name() == "key"
                {
                    m.insert(k.text().unwrap_or("").to_string(), plist_value(*v));
                }
            }
            Value::Object(m)
        }
        "array" => Value::Array(n.children().filter(|c| c.is_element()).map(plist_value).collect()),
        "integer" => n.text().and_then(|t| t.trim().parse::<i64>().ok()).map(Value::from).unwrap_or(Value::Null),
        "real" => n.text().and_then(|t| t.trim().parse::<f64>().ok()).map(Value::from).unwrap_or(Value::Null),
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        _ => Value::String(n.text().unwrap_or("").to_string()), // string, date (ISO), data
    }
}

pub fn plist(xml: &str) -> Option<Value> {
    let doc = roxmltree::Document::parse_with_options(xml, roxmltree::ParsingOptions { allow_dtd: true, ..Default::default() }).ok()?;
    let root = doc.root_element();
    let top = if root.tag_name().name() == "plist" { root.children().find(|c| c.is_element())? } else { root };
    Some(plist_value(top))
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

fn itunes(lib: &Value, out: &mut Parsed) {
    out.sources.insert("iTunes / Music library (Library.xml)".into());
    let mut by_id: HashMap<i64, Song> = HashMap::new();
    for (_, t) in lib["Tracks"].as_object().into_iter().flatten() {
        let is_podcast = t["Podcast"] == true || s(t, "Genre") == "Podcast";
        let video = t["Has Video"] == true || t["Movie"] == true || t["TV Show"] == true;
        if is_podcast {
            let title = if s(t, "Album").is_empty() { s(t, "Artist") } else { s(t, "Album") };
            out.podcasts.push(Podcast { title: title.to_string(), publisher: s(t, "Artist").to_string(), feed: None });
            continue;
        }
        if video || t["Audiobook"] == true || s(t, "Kind").to_lowercase().contains("audiobook") {
            continue;
        }
        let artist = if s(t, "Artist").is_empty() { s(t, "Album Artist") } else { s(t, "Artist") };
        let mut song = Song::named(s(t, "Name"), artist, s(t, "Album"));
        song.duration_ms = t["Total Time"].as_i64().unwrap_or(0);
        if t["Loved"] == true || t["Favorited"] == true {
            out.liked.push((song.clone(), parse_time(s(t, "Date Added"))));
        }
        if let Some(r) = t["Rating"].as_i64()
            && r >= 20
            && t["Rating Computed"] != true
        {
            out.ratings.push((song.clone(), (r / 20).clamp(1, 5) as u8));
        }
        let n = t["Play Count"].as_i64().unwrap_or(0);
        if n > 0 {
            out.counts.push((song.clone(), n, parse_time(s(t, "Play Date UTC"))));
        }
        if let Some(id) = t["Track ID"].as_i64() {
            by_id.insert(id, song);
        }
    }
    for p in lib["Playlists"].as_array().into_iter().flatten() {
        let built_in = p["Master"] == true || p.get("Distinguished Kind").is_some() || p["Folder"] == true || p["Visible"] == false;
        if built_in {
            continue;
        }
        if p.get("Smart Info").is_some() || p.get("Smart Criteria").is_some() {
            continue; // smart playlists fill themselves; Maestro has its own
        }
        let songs: Vec<Song> = p["Playlist Items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| i["Track ID"].as_i64().and_then(|id| by_id.get(&id)).cloned())
            .collect();
        out.playlists.push(Playlist { name: s(p, "Name").to_string(), description: s(p, "Description").to_string(), songs });
    }
}

// ───────────── Apple's privacy export ─────────────

fn pick<'a>(v: &'a Value, keys: &[&str]) -> &'a Value {
    keys.iter().map(|k| &v[*k]).find(|x| !x.is_null()).unwrap_or(&Value::Null)
}

fn when(v: &Value) -> Option<i64> {
    match v {
        Value::String(t) => parse_time(t).or_else(|| t.parse::<i64>().ok().map(|n| if n > 1e11 as i64 { n / 1000 } else { n })),
        Value::Number(n) => n.as_i64().map(|n| if n > 1e11 as i64 { n / 1000 } else { n }),
        _ => None,
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::String(t) => ["true", "yes", "love", "loved", "like", "liked", "1"].contains(&t.to_lowercase().as_str()),
        Value::Number(n) => n.as_i64() == Some(1),
        _ => false,
    }
}

fn privacy_tracks(v: &Value, out: &mut Parsed) -> HashMap<String, Song> {
    let mut by_id = HashMap::new();
    for t in v.as_array().into_iter().flatten() {
        let title = pick(t, &["Title", "Song Name", "Name"]).as_str().unwrap_or("");
        if title.is_empty() || truthy(pick(t, &["Is Podcast", "Podcast"])) {
            continue;
        }
        let artist = pick(t, &["Artist", "Artist Name", "Album Artist"]).as_str().unwrap_or("");
        let mut song = Song::named(title, artist, pick(t, &["Album", "Album Name"]).as_str().unwrap_or(""));
        song.duration_ms = pick(t, &["Track Duration", "Duration", "Media Duration In Milliseconds"]).as_i64().unwrap_or(0);
        song.isrc = pick(t, &["ISRC", "Isrc"]).as_str().filter(|i| i.len() == 12).map(str::to_uppercase);
        let loved = truthy(pick(t, &["Loved", "Is Loved", "Favorite", "Favorited", "Track Like Rating", "Like Rating", "Preference"]));
        if loved {
            out.liked.push((song.clone(), when(pick(t, &["Date Added To Library", "Date Added"]))));
        }
        if let Some(r) = pick(t, &["Rating", "Star Rating"]).as_i64().filter(|r| *r > 0) {
            out.ratings.push((song.clone(), if r > 5 { (r / 20).clamp(1, 5) } else { r } as u8));
        }
        let n = pick(t, &["Play Count", "Track Play Count"]).as_i64().unwrap_or(0);
        if n > 0 {
            out.counts.push((song.clone(), n, when(pick(t, &["Last Played Date", "Play Date UTC", "Last Played"]))));
        }
        for k in ["Track Identifier", "Apple Music Track Identifier", "Catalog Identifier", "Track ID"] {
            if let Some(id) = t[k].as_i64().map(|i| i.to_string()).or_else(|| t[k].as_str().map(String::from)) {
                by_id.insert(id, song.clone());
            }
        }
    }
    by_id
}

fn privacy_playlists(v: &Value, tracks: &HashMap<String, Song>, out: &mut Parsed) {
    for p in v.as_array().into_iter().flatten() {
        let name = pick(p, &["Title", "Name", "Playlist Name"]).as_str().unwrap_or("");
        if name.is_empty() {
            continue;
        }
        let ids = pick(p, &["Playlist Item Identifiers", "Track Identifiers", "Items", "Playlist Items"]);
        let songs: Vec<Song> = ids
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|i| {
                let id = i.as_i64().map(|n| n.to_string()).or_else(|| i.as_str().map(String::from))
                    .or_else(|| i["Track Identifier"].as_i64().map(|n| n.to_string()))?;
                tracks.get(&id).cloned()
            })
            .collect();
        out.playlists.push(Playlist {
            name: name.to_string(),
            description: pick(p, &["Description"]).as_str().unwrap_or("").to_string(),
            songs,
        });
    }
}

fn play_activity(csv: &str, out: &mut Parsed) {
    let Some(t) = Table::parse(csv, &["Song Name", "Content Name"]) else { return };
    out.sources.insert("Apple Music play activity".into());
    let typed = t.has(&["Event Type"]);
    for r in &t.rows {
        if typed && !t.get(r, &["Event Type"]).to_uppercase().contains("PLAY_END") {
            continue;
        }
        let ms = t.get(r, &["Play Duration Milliseconds", "Play Duration"]).parse::<i64>().ok();
        if !typed && ms.unwrap_or(0) <= 0 {
            continue;
        }
        let Some(at) = parse_time(t.get(r, &["Event Start Timestamp", "Event Timestamp", "Event Received Timestamp", "Play Start Time"]))
        else {
            continue;
        };
        let mut song = Song::named(
            t.get(r, &["Song Name", "Content Name"]),
            t.get(r, &["Artist Name", "Container Artist Name"]),
            t.get(r, &["Album Name", "Container Album Name"]),
        );
        song.duration_ms = t.get(r, &["Media Duration In Milliseconds"]).parse().unwrap_or(0);
        out.plays.push(Play { song, at, ms_played: ms });
    }
}

pub fn parse(files: &Files, out: &mut Parsed) -> Result<bool> {
    let mut mine = false;
    for (_, b) in files.named(|n| n.to_lowercase().ends_with(".xml")) {
        let x = text(b);
        if !x.contains("<plist") {
            continue;
        }
        if let Some(lib) = plist(&x).filter(|l| l["Tracks"].is_object()) {
            itunes(&lib, out);
            mine = true;
        }
    }
    let mut tracks = HashMap::new();
    for (_, b) in files.named(|n| {
        let l = n.to_lowercase();
        l.contains("library tracks") && l.ends_with(".json")
    }) {
        if let Ok(v) = serde_json::from_str::<Value>(&text(b)) {
            out.sources.insert("Apple Music library (privacy export)".into());
            tracks.extend(privacy_tracks(&v, out));
            mine = true;
        }
    }
    for (_, b) in files.named(|n| {
        let l = n.to_lowercase();
        l.contains("library playlists") && l.ends_with(".json")
    }) {
        if let Ok(v) = serde_json::from_str::<Value>(&text(b)) {
            privacy_playlists(&v, &tracks, out);
            mine = true;
        }
    }
    for (_, b) in files.named(|n| {
        let l = n.to_lowercase();
        l.contains("play activity") && l.ends_with(".csv")
    }) {
        play_activity(&text(b), out);
        mine = true;
    }
    Ok(mine)
}

#[cfg(test)]
mod tests {
    use crate::import::{Files, parse_files};

    fn files(list: &[(&str, &str)]) -> Files {
        Files { files: list.iter().map(|(n, b)| (n.to_string(), b.as_bytes().to_vec())).collect() }
    }

    const LIB: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Major Version</key><integer>1</integer>
  <key>Tracks</key><dict>
    <key>101</key><dict><key>Track ID</key><integer>101</integer><key>Name</key><string>Once in a Lifetime</string>
      <key>Artist</key><string>Talking Heads</string><key>Album</key><string>Remain in Light</string>
      <key>Total Time</key><integer>259000</integer><key>Play Count</key><integer>12</integer>
      <key>Play Date UTC</key><date>2020-05-01T10:00:00Z</date><key>Rating</key><integer>80</integer>
      <key>Loved</key><true/><key>Date Added</key><date>2019-01-01T00:00:00Z</date></dict>
    <key>102</key><dict><key>Track ID</key><integer>102</integer><key>Name</key><string>Heaven</string>
      <key>Artist</key><string>Talking Heads</string><key>Album</key><string>Fear of Music</string>
      <key>Rating</key><integer>60</integer><key>Rating Computed</key><true/></dict>
    <key>103</key><dict><key>Track ID</key><integer>103</integer><key>Name</key><string>Episode 1</string>
      <key>Artist</key><string>Hrishikesh Hirway</string><key>Album</key><string>Song Exploder</string><key>Podcast</key><true/></dict>
  </dict>
  <key>Playlists</key><array>
    <dict><key>Name</key><string>Library</string><key>Master</key><true/><key>Playlist Items</key><array><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
    <dict><key>Name</key><string>Music</string><key>Distinguished Kind</key><integer>4</integer></dict>
    <dict><key>Name</key><string>Top 25</string><key>Smart Info</key><data>AA==</data></dict>
    <dict><key>Name</key><string>Mine</string><key>Playlist Items</key><array>
      <dict><key>Track ID</key><integer>102</integer></dict><dict><key>Track ID</key><integer>101</integer></dict></array></dict>
  </array>
</dict></plist>"#;

    #[test]
    fn itunes_library() {
        let p = parse_files(&files(&[("Library.xml", LIB)])).unwrap();
        assert_eq!(p.liked.len(), 1);
        assert_eq!(p.liked[0].1, Some(1546300800));
        assert_eq!(p.ratings.len(), 1, "not the computed one");
        assert_eq!(p.ratings[0].1, 4);
        assert_eq!((p.counts[0].1, p.counts[0].2), (12, Some(1588327200)));
        assert_eq!(p.playlists.len(), 1);
        let titles: Vec<&str> = p.playlists[0].songs.iter().map(|s| s.title.as_str()).collect();
        assert_eq!((p.playlists[0].name.as_str(), titles), ("Mine", vec!["Heaven", "Once in a Lifetime"]));
        assert_eq!(p.podcasts[0].title, "Song Exploder");
    }

    #[test]
    fn privacy_export() {
        let activity = "Album Name,Song Name,Artist Name,Client IP Address,Event Start Timestamp,Event Type,Play Duration Milliseconds,Media Duration In Milliseconds\n\
            Remain in Light,Once in a Lifetime,Talking Heads,10.0.0.9,2023-05-01T12:00:00.000Z,PLAY_END,250000,259000\n\
            Remain in Light,Once in a Lifetime,Talking Heads,10.0.0.9,2023-05-01T11:59:00.000Z,PLAY_START,0,259000\n\
            Fear of Music,Heaven,Talking Heads,10.0.0.9,2023-05-01T12:05:00.000Z,PLAY_END,4000,240000\n";
        let tracks = r#"[{"Title":"Once in a Lifetime","Artist":"Talking Heads","Album":"Remain in Light","Track Identifier":55,
                          "Play Count":3,"Last Played Date":"2023-05-01T12:00:00Z","Track Like Rating":"LOVE","Date Added To Library":"2022-01-01T00:00:00Z"},
                         {"Title":"Heaven","Artist":"Talking Heads","Album":"Fear of Music","Track Identifier":56}]"#;
        let lists = r#"[{"Title":"Weekend","Playlist Item Identifiers":[56,55,999]}]"#;
        let p = parse_files(&files(&[
            ("Apple_Media_Services/Apple Music Activity/Apple Music Play Activity.csv", activity),
            ("Apple_Media_Services/Apple Music Activity/Apple Music Library Tracks.json", tracks),
            ("Apple_Media_Services/Apple Music Activity/Apple Music Library Playlists.json", lists),
        ]))
        .unwrap();
        assert_eq!(p.plays.len(), 2, "only the ends of plays");
        assert!(p.plays.iter().any(|x| x.song.title == "Heaven" && x.is_skip()));
        assert_eq!(p.liked.len(), 1);
        assert_eq!(p.counts[0].1, 3);
        assert_eq!(p.playlists[0].songs.len(), 2);
        assert!(!serde_json::to_string(&p).unwrap().contains("10.0.0.9"));
    }
}
