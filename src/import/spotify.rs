//! Spotify's exports:
//! - Account data (`my_spotify_data.zip`): YourLibrary.json (liked songs, saved albums, followed artists, shows),
//!   Playlist1.json … (your playlists, in order), StreamingHistory_music_0.json … (the last year's plays).
//! - Extended streaming history: Streaming_History_Audio_*.json (older: endsong_*.json), every play with its time,
//!   how long it played and the Spotify id. Only songs: podcast and audiobook rows are left out.
//! - Exportify's CSVs (one per playlist; Liked_Songs.csv → liked songs).
//!
//! Only titles, artists, albums, ids, times and lengths are read: IP addresses, countries, platforms, searches and
//! the rest are never parsed.

use anyhow::Result;
use serde::Deserialize;

use super::{Album, Files, Parsed, Play, Playlist, Podcast, Song, base, csv::Table, parse_time, spotify_id, stem, text};

#[derive(Deserialize, Default)]
#[serde(default)]
struct Library {
    tracks: Vec<LibTrack>,
    albums: Vec<LibAlbum>,
    artists: Vec<LibArtist>,
    shows: Vec<LibShow>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct LibTrack {
    artist: String,
    album: String,
    track: String,
    uri: String,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct LibAlbum {
    artist: String,
    album: String,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct LibArtist {
    name: String,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct LibShow {
    name: String,
    publisher: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Playlists {
    playlists: Vec<Pl>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct Pl {
    name: String,
    description: Option<String>,
    items: Vec<PlItem>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct PlItem {
    track: Option<PlTrack>,
}
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PlTrack {
    track_name: String,
    artist_name: String,
    album_name: String,
    track_uri: String,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ShortPlay {
    end_time: String,
    artist_name: String,
    track_name: String,
    ms_played: i64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LongPlay {
    ts: String,
    ms_played: i64,
    master_metadata_track_name: Option<String>,
    master_metadata_album_artist_name: Option<String>,
    master_metadata_album_album_name: Option<String>,
    spotify_track_uri: Option<String>,
}

fn lower(p: &str) -> String {
    base(p).to_lowercase()
}

pub fn parse(files: &Files, out: &mut Parsed) -> Result<bool> {
    let mut mine = false;
    for (path, b) in files.named(|n| n.eq_ignore_ascii_case("YourLibrary.json")) {
        let lib: Library = serde_json::from_slice(text(b).as_bytes()).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        out.sources.insert("Spotify account data".into());
        mine = true;
        for t in lib.tracks {
            let mut s = Song::named(&t.track, &t.artist, &t.album);
            s.spotify = spotify_id(&t.uri);
            out.liked.push((s, None));
        }
        out.albums.extend(lib.albums.into_iter().map(|a| Album { title: a.album, artist: a.artist }));
        out.artists.extend(lib.artists.into_iter().map(|a| a.name));
        out.podcasts.extend(lib.shows.into_iter().map(|s| Podcast { title: s.name, publisher: s.publisher, feed: None }));
    }
    for (path, b) in files.named(|n| {
        let l = n.to_lowercase();
        l.starts_with("playlist") && l.ends_with(".json")
    }) {
        let Ok(pls) = serde_json::from_slice::<Playlists>(text(b).as_bytes()) else { continue };
        if pls.playlists.is_empty() {
            continue;
        }
        let _ = path;
        out.sources.insert("Spotify account data".into());
        mine = true;
        for p in pls.playlists {
            let songs = p
                .items
                .into_iter()
                .filter_map(|i| i.track)
                .map(|t| {
                    let mut s = Song::named(&t.track_name, &t.artist_name, &t.album_name);
                    s.spotify = spotify_id(&t.track_uri);
                    s
                })
                .collect();
            out.playlists.push(Playlist { name: p.name, description: p.description.unwrap_or_default(), songs });
        }
    }
    // the last year's plays: when each ended, and for how long
    for (_, b) in files.named(|n| {
        let l = n.to_lowercase();
        (l.starts_with("streaminghistory_music") || (l.starts_with("streaminghistory") && !l.contains("podcast") && !l.contains("video")))
            && !l.starts_with("streaming_history")
            && l.ends_with(".json")
    }) {
        let Ok(rows) = serde_json::from_slice::<Vec<ShortPlay>>(text(b).as_bytes()) else { continue };
        out.sources.insert("Spotify account data (streaming history)".into());
        mine = true;
        for r in rows {
            let Some(end) = parse_time(&r.end_time) else { continue };
            out.plays.push(Play {
                song: Song::named(&r.track_name, &r.artist_name, ""),
                at: end - r.ms_played / 1000,
                ms_played: Some(r.ms_played),
            });
        }
    }
    // the extended history: every play since the account began
    for (_, b) in files.named(|n| {
        let l = n.to_lowercase();
        (l.starts_with("streaming_history_audio") || l.starts_with("endsong")) && l.ends_with(".json")
    }) {
        let Ok(rows) = serde_json::from_slice::<Vec<LongPlay>>(b) else { continue };
        out.sources.insert("Spotify extended streaming history".into());
        mine = true;
        for r in rows {
            let Some(id) = r.spotify_track_uri.as_deref().and_then(spotify_id) else { continue }; // episodes, audiobooks
            let Some(end) = parse_time(&r.ts) else { continue };
            let mut s = Song::named(
                r.master_metadata_track_name.as_deref().unwrap_or(""),
                r.master_metadata_album_artist_name.as_deref().unwrap_or(""),
                r.master_metadata_album_album_name.as_deref().unwrap_or(""),
            );
            s.spotify = Some(id);
            out.plays.push(Play { song: s, at: end - r.ms_played / 1000, ms_played: Some(r.ms_played) });
        }
    }
    // Exportify
    for (path, b) in files.named(|n| n.to_lowercase().ends_with(".csv")) {
        let Some(t) = Table::parse(&text(b), &["Track URI", "Spotify ID"]) else { continue };
        if !t.has(&["Track Name"]) {
            continue;
        }
        out.sources.insert("Exportify (Spotify playlists)".into());
        mine = true;
        let name = stem(path).replace('_', " ");
        let mut songs = vec![];
        let mut liked = vec![];
        let is_liked = lower(path).starts_with("liked");
        for r in &t.rows {
            let artists = t.get(r, &["Artist Name(s)", "Artist Name", "Artists"]);
            // several artists are joined with ", " (or ";" in newer versions)
            let first = artists.split([';']).next().unwrap_or("").split(", ").next().unwrap_or("");
            let mut s = Song::named(t.get(r, &["Track Name"]), first, t.get(r, &["Album Name"]));
            let uri = t.get(r, &["Track URI", "Spotify ID"]);
            s.spotify = spotify_id(uri).or_else(|| (uri.len() == 22).then(|| uri.to_string()));
            s.duration_ms = t.get(r, &["Duration (ms)", "Track Duration (ms)"]).parse().unwrap_or(0);
            s.isrc = Some(t.get(r, &["ISRC"]).to_uppercase()).filter(|i| i.len() == 12);
            if is_liked {
                liked.push((s, parse_time(t.get(r, &["Added At"]))));
            } else {
                songs.push(s);
            }
        }
        out.liked.extend(liked);
        if !is_liked {
            out.playlists.push(Playlist { name, description: String::new(), songs });
        }
    }
    Ok(mine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{Files, parse_files};

    fn files(list: &[(&str, &str)]) -> Files {
        Files { files: list.iter().map(|(n, b)| (n.to_string(), b.as_bytes().to_vec())).collect() }
    }

    #[test]
    fn account_data_and_extended_history() {
        let p = parse_files(&files(&[
            (
                "Spotify Account Data/YourLibrary.json",
                r#"{"tracks":[{"artist":"Talking Heads","album":"Remain in Light","track":"Once in a Lifetime","uri":"spotify:track:6E1Hf1TZhYFbIXuKwUCAEa"}],
                   "albums":[{"artist":"Talking Heads","album":"Remain in Light","uri":"spotify:album:x"}],
                   "artists":[{"name":"Talking Heads","uri":"spotify:artist:x"}],
                   "shows":[{"name":"Song Exploder","publisher":"Hrishikesh Hirway","uri":"spotify:show:x"}],
                   "bannedTracks":[],"other":["x"]}"#,
            ),
            (
                "Spotify Account Data/Playlist1.json",
                r#"{"playlists":[{"name":"Road","lastModifiedDate":"2024-01-01","description":"for driving","numberOfFollowers":0,
                   "items":[{"track":{"trackName":"Psycho Killer","artistName":"Talking Heads","albumName":"77","trackUri":"spotify:track:0qzzHu4Gp1fXJ9wYPfRe2N"},"episode":null,"localTrack":null,"addedDate":"2024-01-01"},
                            {"track":null,"episode":{"episodeName":"x"},"localTrack":null,"addedDate":"2024-01-01"}]}]}"#,
            ),
            (
                "Spotify Account Data/StreamingHistory_music_0.json",
                r#"[{"endTime":"2024-01-02 13:45","artistName":"Talking Heads","trackName":"Psycho Killer","msPlayed":200000},
                    {"endTime":"2024-01-02 13:46","artistName":"Talking Heads","trackName":"Heaven","msPlayed":5000}]"#,
            ),
            ("Spotify Account Data/StreamingHistory_podcast_0.json", r#"[{"endTime":"2024-01-02 13:45","podcastName":"x","episodeName":"y","msPlayed":1}]"#),
            ("Spotify Account Data/SearchQueries.json", r#"[{"platform":"x","searchTime":"t","searchQuery":"secret"}]"#),
            (
                "Spotify Extended Streaming History/Streaming_History_Audio_2024.json",
                r#"[{"ts":"2024-03-01T10:00:00Z","platform":"android","ms_played":240000,"conn_country":"US","ip_addr":"1.2.3.4",
                     "master_metadata_track_name":"Once in a Lifetime","master_metadata_album_artist_name":"Talking Heads",
                     "master_metadata_album_album_name":"Remain in Light","spotify_track_uri":"spotify:track:6E1Hf1TZhYFbIXuKwUCAEa",
                     "episode_name":null,"spotify_episode_uri":null,"reason_start":"trackdone","reason_end":"trackdone","shuffle":false,"skipped":false,"offline":false,"incognito_mode":false},
                    {"ts":"2024-03-01T11:00:00Z","ms_played":100000,"master_metadata_track_name":null,"spotify_track_uri":null,
                     "episode_name":"An episode","spotify_episode_uri":"spotify:episode:x"}]"#,
            ),
        ]))
        .unwrap();
        assert_eq!(p.liked.len(), 1);
        assert_eq!(p.liked[0].0.spotify.as_deref(), Some("6E1Hf1TZhYFbIXuKwUCAEa"));
        assert_eq!(p.albums, [Album { title: "Remain in Light".into(), artist: "Talking Heads".into() }]);
        assert_eq!(p.artists, ["Talking Heads"]);
        assert_eq!(p.podcasts[0].title, "Song Exploder");
        assert_eq!(p.playlists[0].name, "Road");
        assert_eq!(p.playlists[0].songs.len(), 1, "episodes aren't songs");
        assert_eq!(p.plays.len(), 3, "two short-history plays and one extended, no episode");
        let short = p.plays.iter().find(|x| x.song.title == "Psycho Killer").unwrap();
        assert_eq!(short.at, 1704203100 - 200);
        assert!(p.plays.iter().find(|x| x.song.title == "Heaven").unwrap().is_skip());
        let long = p.plays.iter().find(|x| x.song.spotify.is_some()).unwrap();
        assert_eq!((long.at, long.ms_played), (1709287200 - 240, Some(240000)));
        let dump = serde_json::to_string(&p).unwrap();
        assert!(!dump.contains("1.2.3.4") && !dump.contains("android") && !dump.contains("secret"), "nothing private is kept");
    }

    #[test]
    fn exportify() {
        let csv = "\"Track URI\",\"Track Name\",\"Artist Name(s)\",\"Album Name\",\"Duration (ms)\",\"Added At\",\"ISRC\"\n\
                   \"spotify:track:6E1Hf1TZhYFbIXuKwUCAEa\",\"Once in a Lifetime\",\"Talking Heads, Brian Eno\",\"Remain in Light\",\"259000\",\"2020-05-01T10:00:00Z\",\"usew10000001\"\n";
        let p = parse_files(&files(&[("Liked_Songs.csv", csv), ("Road_Trip.csv", csv)])).unwrap();
        assert_eq!(p.liked.len(), 1);
        assert_eq!(p.liked[0].1, Some(1588327200));
        let s = &p.playlists[0].songs[0];
        assert_eq!((p.playlists[0].name.as_str(), s.artist.as_str(), s.duration_ms), ("Road Trip", "Talking Heads", 259000));
        assert_eq!(s.isrc.as_deref(), Some("USEW10000001"));
    }
}
