//! Applying an import to any Subsonic server through its API (e.g. Navidrome before switching): the same plan,
//! sent as `star`, `createPlaylist` / `updatePlaylist`, `setRating`, `createPodcastChannel` and `scrobble` with
//! `time` for past plays.
//!
//! What the API can't do: skips aren't sent, and play counts without single plays (iTunes / Apple) only send the
//! last play; followed artists become starred artists; songs you don't have aren't asked for. What was sent is kept
//! in a small state file, so running it again sends only what's new and `undo` can unstar and delete playlists
//! (plays sent to a server can't be taken back through the API).

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    apply::{Changes, Plan},
    matching::{LibSong, Library},
};

pub struct Server {
    base: String,
    user: String,
    pass: String,
    http: reqwest::Client,
}

/// What was sent to a server, so it isn't sent twice and can be undone.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub server: String,
    pub starred: Vec<String>,
    pub starred_albums: Vec<String>,
    pub starred_artists: Vec<String>,
    pub playlists: Vec<String>,
    pub plays: HashSet<String>,
    pub ratings: HashMap<String, i64>,
}

impl State {
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }
}

fn strs(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().filter_map(|x| x.as_str().or(x["name"].as_str()).map(String::from)).collect(),
        Value::String(s) if !s.is_empty() => vec![s.clone()],
        _ => vec![],
    }
}

impl Server {
    pub fn new(base: &str, user: &str, pass: &str) -> Self {
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build().expect("http client");
        Server { base: base.trim_end_matches('/').to_string(), user: user.into(), pass: pass.into(), http }
    }

    /// A Subsonic call, sent as a form POST (long lists of ids don't fit in a URL).
    pub async fn call(&self, method: &str, params: &[(&str, String)]) -> Result<Value> {
        let salt = crate::db::random_id()[..12].to_string();
        let token = format!("{:x}", md5::compute(format!("{}{salt}", self.pass)));
        let mut form: Vec<(&str, String)> =
            vec![("u", self.user.clone()), ("t", token), ("s", salt), ("v", "1.16.1".into()), ("c", "conductor-import".into()), ("f", "json".into())];
        form.extend(params.iter().cloned());
        let r = self.http.post(format!("{}/rest/{method}", self.base)).form(&form).send().await.with_context(|| method.to_string())?;
        let status = r.status();
        let v: Value = r.json().await.with_context(|| format!("{method}: HTTP {status}"))?;
        let resp = &v["subsonic-response"];
        if resp["status"] != "ok" {
            bail!("{method}: {}", resp["error"]["message"].as_str().unwrap_or("failed"));
        }
        Ok(resp.clone())
    }

    /// Every song (search3 with an empty query, a page at a time), and the artists.
    pub async fn library(&self) -> Result<Library> {
        let mut songs = vec![];
        let mut offset = 0;
        loop {
            let r = self
                .call("search3", &[("query", String::new()), ("artistCount", "0".into()), ("albumCount", "0".into()), ("songCount", "500".into()), ("songOffset", offset.to_string())])
                .await?;
            let page = r["searchResult3"]["song"].as_array().cloned().unwrap_or_default();
            for s in &page {
                let album_artist = s["albumArtists"][0]["name"].as_str().or(s["displayAlbumArtist"].as_str()).unwrap_or(s["artist"].as_str().unwrap_or(""));
                songs.push(LibSong {
                    id: s["id"].as_str().unwrap_or("").into(),
                    title: s["title"].as_str().unwrap_or("").into(),
                    artist: s["artist"].as_str().unwrap_or("").into(),
                    album_artist: album_artist.into(),
                    album: s["album"].as_str().unwrap_or("").into(),
                    album_id: s["albumId"].as_str().unwrap_or("").into(),
                    artist_id: s["artistId"].as_str().unwrap_or("").into(),
                    duration_s: s["duration"].as_i64().unwrap_or(0),
                    isrc: strs(&s["isrc"]).into_iter().map(|i| i.to_uppercase()).collect(),
                    mbid: s["musicBrainzId"].as_str().filter(|m| !m.is_empty()).map(String::from),
                    path: s["path"].as_str().unwrap_or("").into(),
                });
            }
            if page.len() < 500 || offset > 2_000_000 {
                break;
            }
            offset += 500;
        }
        let r = self.call("getArtists", &[]).await?;
        let artists = r["artists"]["index"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|i| i["artist"].as_array().cloned().unwrap_or_default())
            .filter_map(|a| Some((a["id"].as_str()?.to_string(), a["name"].as_str()?.to_string())))
            .collect();
        Ok(Library::new(songs, HashMap::new(), artists))
    }

    async fn star_many(&self, key: &str, ids: &[String]) -> Result<()> {
        for chunk in ids.chunks(100) {
            let params: Vec<(&str, String)> = chunk.iter().map(|i| (key, i.clone())).collect();
            self.call("star", &params).await?;
        }
        Ok(())
    }

    /// Send the plan. `dry`: only count what would be sent.
    pub async fn apply(&self, plan: &Plan, label: &str, state: &mut State, state_path: &Path, dry: bool) -> Result<Changes> {
        let mut c = Changes::default();
        let starred = self.call("getStarred2", &[]).await?;
        let ids_of = |k: &str| -> HashSet<String> {
            starred["starred2"][k].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str().map(String::from)).collect()
        };
        let (songs, albums, artists) = (ids_of("song"), ids_of("album"), ids_of("artist"));
        let new_songs: Vec<String> = plan.stars.iter().map(|(id, _)| id.clone()).filter(|id| !songs.contains(id)).collect();
        let new_albums: Vec<String> = plan.album_stars.iter().filter(|id| !albums.contains(*id)).cloned().collect();
        let new_artists: Vec<String> = plan.follows.iter().map(|(id, _)| id.clone()).filter(|id| !artists.contains(id)).collect();
        (c.hearts, c.albums, c.follows) = (new_songs.len(), new_albums.len(), new_artists.len());
        if !dry {
            self.star_many("id", &new_songs).await?;
            self.star_many("albumId", &new_albums).await?;
            self.star_many("artistId", &new_artists).await?;
            state.starred.extend(new_songs);
            state.starred_albums.extend(new_albums);
            state.starred_artists.extend(new_artists);
            state.save(state_path)?;
        }
        // playlists: one made by an earlier import (same name, marked) is filled again
        let lists = self.call("getPlaylists", &[]).await?;
        let mut existing: HashMap<String, String> = HashMap::new();
        for p in lists["playlists"]["playlist"].as_array().into_iter().flatten() {
            if p["comment"].as_str().unwrap_or("").contains("Imported from")
                && let (Some(n), Some(id)) = (p["name"].as_str(), p["id"].as_str())
            {
                existing.entry(n.to_string()).or_insert(id.to_string());
            }
        }
        for l in &plan.playlists {
            let mut params: Vec<(&str, String)> = l.ids.iter().map(|i| ("songId", i.clone())).collect();
            match existing.get(&l.name) {
                Some(id) => {
                    c.playlists_updated += 1;
                    params.push(("playlistId", id.clone()));
                    if !dry {
                        self.call("createPlaylist", &params).await?; // replaces its songs
                    }
                }
                None => {
                    c.playlists_new += 1;
                    if !dry {
                        params.push(("name", l.name.clone()));
                        let r = self.call("createPlaylist", &params).await?;
                        if let Some(id) = r["playlist"]["id"].as_str() {
                            let comment = if l.description.is_empty() { label.to_string() } else { format!("{}\n{label}", l.description) };
                            let _ = self.call("updatePlaylist", &[("playlistId", id.to_string()), ("comment", comment)]).await;
                            state.playlists.push(id.to_string());
                            state.save(state_path)?;
                        }
                    }
                }
            }
        }
        // plays, with their times (and the last play of a play count)
        let mut plays: Vec<(String, i64)> = plan.plays.clone();
        plays.extend(plan.counts.iter().filter_map(|(id, _, last)| last.map(|l| (id.clone(), l))));
        let fresh: Vec<&(String, i64)> = plays.iter().filter(|(id, at)| !state.plays.contains(&format!("{id}@{at}"))).collect();
        c.plays_already_there = plays.len() - fresh.len();
        c.plays = fresh.len();
        if !dry {
            for chunk in fresh.chunks(50) {
                let mut params: Vec<(&str, String)> = vec![("submission", "true".into())];
                for (id, at) in chunk {
                    params.push(("id", id.clone()));
                    params.push(("time", (at * 1000).to_string()));
                }
                self.call("scrobble", &params).await?;
                state.plays.extend(chunk.iter().map(|(id, at)| format!("{id}@{at}")));
                state.save(state_path)?;
            }
        }
        for (id, stars) in &plan.ratings {
            if state.ratings.get(id) == Some(&(*stars as i64)) {
                continue;
            }
            c.ratings += 1;
            if !dry {
                self.call("setRating", &[("id", id.clone()), ("rating", stars.to_string())]).await?;
                state.ratings.insert(id.clone(), *stars as i64);
            }
        }
        if !dry {
            state.save(state_path)?;
        }
        c.counts = plan.counts.len();
        Ok(c)
    }

    /// Podcasts, for servers that have the podcast API (Navidrome doesn't): feeds subscribed, or an error to report.
    pub async fn podcasts(&self, feeds: &[String]) -> Result<usize> {
        let mut n = 0;
        for f in feeds {
            self.call("createPodcastChannel", &[("url", f.clone())]).await?;
            n += 1;
        }
        Ok(n)
    }

    /// Take back what an import sent: unstar, delete the playlists it made. Plays stay (the API can't remove them).
    pub async fn undo(&self, state: &mut State, state_path: &Path) -> Result<Value> {
        let n = json!({ "songs": state.starred.len(), "albums": state.starred_albums.len(), "artists": state.starred_artists.len(), "playlists": state.playlists.len(), "playsKept": state.plays.len() });
        for (key, ids) in [("id", &state.starred), ("albumId", &state.starred_albums), ("artistId", &state.starred_artists)] {
            for chunk in ids.chunks(100) {
                let params: Vec<(&str, String)> = chunk.iter().map(|i| (key, i.clone())).collect();
                self.call("unstar", &params).await?;
            }
        }
        for id in &state.playlists {
            let _ = self.call("deletePlaylist", &[("id", id.clone())]).await;
        }
        state.starred.clear();
        state.starred_albums.clear();
        state.starred_artists.clear();
        state.playlists.clear();
        state.save(state_path)?;
        Ok(n)
    }
}
