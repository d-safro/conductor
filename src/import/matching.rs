//! Finding an imported song in your library: exact ids first (the Spotify id → file map Spotify sync made, ISRC,
//! MusicBrainz id, the file path of an M3U entry), then title + artist (the same keys the ⬇ catalog uses: any
//! alphabet, "- Remastered" and "(feat. …)" left out), preferring the same album and a close length.
//!
//! The library is a plain list, so the same matching works on Maestro's database and on any Subsonic server's
//! songs (`apply::Subsonic`).

use std::collections::HashMap;

use serde::Serialize;

use super::{Album, Song};
use crate::catalog::text::{ak, alk, norm, same_artist, tk, translit};

#[derive(Debug, Clone, Default)]
pub struct LibSong {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album_artist: String,
    pub album: String,
    pub album_id: String,
    pub artist_id: String,
    pub duration_s: i64,
    pub isrc: Vec<String>,
    pub mbid: Option<String>,
    /// relative to the music folder (Maestro), or as the server gives it
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum How {
    Spotify,
    Isrc,
    Mbid,
    Path,
    Name,
}

#[derive(Default)]
pub struct Library {
    pub songs: Vec<LibSong>,
    by_id: HashMap<String, usize>,
    by_title: HashMap<String, Vec<usize>>,
    by_isrc: HashMap<String, usize>,
    by_mbid: HashMap<String, usize>,
    by_path: HashMap<String, usize>,
    /// Spotify id → song id (Maestro's Spotify sync remembers these)
    spotify: HashMap<String, String>,
    albums: HashMap<(String, String), String>,
    album_titles: HashMap<String, Vec<String>>,
    artists: HashMap<String, (String, String)>,
}

fn title_keys(t: &str) -> Vec<String> {
    let a = tk(t);
    let b = norm(&translit(&crate::catalog::text::clean_title(t)));
    if a == b { vec![a] } else { vec![a, b] }
}

impl Library {
    pub fn new(songs: Vec<LibSong>, spotify: HashMap<String, String>, artists: Vec<(String, String)>) -> Self {
        let mut l = Library { spotify, ..Default::default() };
        for (i, s) in songs.iter().enumerate() {
            l.by_id.insert(s.id.clone(), i);
            for k in title_keys(&s.title) {
                if !k.is_empty() {
                    l.by_title.entry(k).or_default().push(i);
                }
            }
            for isrc in &s.isrc {
                l.by_isrc.entry(isrc.to_uppercase()).or_insert(i);
            }
            if let Some(m) = &s.mbid {
                l.by_mbid.entry(m.clone()).or_insert(i);
            }
            if !s.path.is_empty() {
                l.by_path.entry(s.path.to_lowercase().replace('\\', "/")).or_insert(i);
            }
            for name in [&s.album_artist, &s.artist] {
                l.albums.entry((ak(name), alk(&s.album))).or_insert_with(|| s.album_id.clone());
            }
            let ids = l.album_titles.entry(alk(&s.album)).or_default();
            if !ids.contains(&s.album_id) {
                ids.push(s.album_id.clone());
            }
        }
        for (id, name) in artists {
            l.artists.entry(ak(&name)).or_insert((id, name));
        }
        l.songs = songs;
        l
    }

    pub fn len(&self) -> usize {
        self.songs.len()
    }
    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }

    /// The library song for an imported one, and how it was found.
    pub fn find(&self, s: &Song) -> Option<(usize, How)> {
        if let Some(i) = s.spotify.as_ref().and_then(|x| self.spotify.get(x)).and_then(|id| self.by_id.get(id)) {
            return Some((*i, How::Spotify));
        }
        if let Some(i) = s.isrc.as_ref().and_then(|x| self.by_isrc.get(&x.to_uppercase())) {
            return Some((*i, How::Isrc));
        }
        if let Some(i) = s.mbid.as_ref().and_then(|x| self.by_mbid.get(x)) {
            return Some((*i, How::Mbid));
        }
        if let Some(p) = &s.path {
            // the longest tail of the path that's a file in the library ("/mnt/x/music/Albums/A/B.flac" → "Albums/A/B.flac")
            let p = p.to_lowercase();
            let mut rest = p.as_str();
            loop {
                if let Some(i) = self.by_path.get(rest) {
                    return Some((*i, How::Path));
                }
                match rest.split_once('/') {
                    Some((_, r)) if !r.is_empty() => rest = r,
                    _ => break,
                }
            }
        }
        self.by_name(s).map(|i| (i, How::Name))
    }

    fn by_name(&self, s: &Song) -> Option<usize> {
        if s.title.trim().is_empty() {
            return None;
        }
        let want_album = alk(&s.album);
        let mut best: Option<(i32, usize)> = None;
        let mut seen = std::collections::HashSet::new();
        for k in title_keys(&s.title) {
            for &i in self.by_title.get(&k).into_iter().flatten() {
                if !seen.insert(i) {
                    continue;
                }
                let c = &self.songs[i];
                let artist_ok = if s.artist.trim().is_empty() {
                    !want_album.is_empty() && alk(&c.album) == want_album // Apple's newer exports leave the artist out
                } else {
                    same_artist(&s.artist, &c.artist) || same_artist(&s.artist, &c.album_artist)
                };
                if !artist_ok {
                    continue;
                }
                let mut score = 10;
                if !want_album.is_empty() && alk(&c.album) == want_album {
                    score += 5;
                }
                if s.duration_ms > 0 && c.duration_s > 0 {
                    let d = (c.duration_s - s.duration_ms / 1000).abs();
                    score += if d <= 3 {
                        3
                    } else if d <= 25 {
                        1
                    } else {
                        -8 // another length: a live version or another edit
                    };
                }
                if best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, i));
                }
            }
        }
        best.map(|(_, i)| i)
    }

    pub fn album(&self, a: &Album) -> Option<String> {
        if let Some(id) = self.albums.get(&(ak(&a.artist), alk(&a.title))) {
            return Some(id.clone());
        }
        // the artist spelled another way, but only one album of that name, by someone like them
        let ids = self.album_titles.get(&alk(&a.title))?;
        if ids.len() != 1 {
            return None;
        }
        let ok = a.artist.is_empty()
            || self.songs.iter().any(|s| s.album_id == ids[0] && (same_artist(&a.artist, &s.album_artist) || same_artist(&a.artist, &s.artist)));
        ok.then(|| ids[0].clone())
    }

    pub fn artist(&self, name: &str) -> Option<(String, String)> {
        self.artists.get(&ak(name)).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib() -> Library {
        let s = |id: &str, t: &str, a: &str, al: &str, d: i64| LibSong {
            id: id.into(),
            title: t.into(),
            artist: a.into(),
            album_artist: a.into(),
            album: al.into(),
            album_id: format!("al-{al}"),
            artist_id: format!("ar-{a}"),
            duration_s: d,
            path: format!("Albums/{a}/{al}/{t}.flac"),
            ..Default::default()
        };
        let mut v = vec![
            s("1", "Once in a Lifetime", "Talking Heads", "Remain in Light", 259),
            s("2", "Once in a Lifetime", "Talking Heads", "Stop Making Sense", 325),
            s("3", "Группа крови", "Кино", "Группа крови", 286),
            s("4", "Heaven", "Talking Heads", "Fear of Music", 240),
        ];
        v[3].isrc = vec!["USWB10000001".into()];
        Library::new(v, [("sp1".to_string(), "4".to_string())].into(), vec![("ar-1".into(), "Talking Heads".into())])
    }

    #[test]
    fn ids_then_names() {
        let l = lib();
        let find = |s: Song| l.find(&s).map(|(i, h)| (l.songs[i].id.clone(), h));
        let mut s = Song::named("Something else", "Nobody", "");
        s.spotify = Some("sp1".into());
        assert_eq!(find(s), Some(("4".into(), How::Spotify)));
        let mut s = Song::named("", "", "");
        s.isrc = Some("uswb10000001".into());
        assert_eq!(find(s), Some(("4".into(), How::Isrc)));
        let mut s = Song::named("x", "y", "");
        s.path = Some("/mnt/music/Albums/Talking Heads/Fear of Music/Heaven.flac".into());
        assert_eq!(find(s).unwrap().1, How::Path);
        // the version from the album asked for, and by length when the album isn't said
        assert_eq!(find(Song::named("Once in a Lifetime - 2005 Remaster", "Talking Heads", "Stop Making Sense")).unwrap().0, "2");
        let mut s = Song::named("Once in a Lifetime", "talking heads", "");
        s.duration_ms = 258_000;
        assert_eq!(find(s).unwrap().0, "1");
        // another alphabet, and no artist but the album (Apple)
        assert_eq!(find(Song::named("Gruppa krovi", "Kino", "")).unwrap().0, "3");
        assert_eq!(find(Song::named("Heaven", "", "Fear of Music")).unwrap().0, "4");
        assert_eq!(find(Song::named("Heaven", "Someone Else", "")), None);
        let ril = Album { title: "Remain in Light (Deluxe Version)".into(), artist: "Talking Heads".into() };
        assert_eq!(l.album(&ril).as_deref(), Some("al-Remain in Light"));
        assert_eq!(l.artist("talking heads").unwrap().0, "ar-1");
    }
}
