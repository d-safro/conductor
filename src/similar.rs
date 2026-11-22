//! Conductor's own "sounds like", for when there's no AudioMuse-AI (or it hasn't heard a song yet): which artists in
//! the library go with an artist, from what's there already. No audio analysis and no outside service is needed;
//! related artists from online info are one more hint when `info_online` allows it.
//!
//! An artist is scored against the seed artist from:
//! - played together: the artists listened to in the same sittings (plays less than 45 minutes apart), everyone's;
//! - related artists from online info (Deezer), the closest first;
//! - their genres: the same genres, and genres sharing a word ("Russian Rock" and "Rock");
//! - the script their names and titles are written in (Cyrillic with Cyrillic, Latin with Latin…), which keeps a
//!   library in several languages from jumping between them;
//! - their era (the years of their songs).
//!
//! What's built from the whole library (each artist's profile, who's played with whom) is kept for ten minutes and
//! built again after a scan.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use crate::{api::AppState, db::now};

/// How long a built profile is kept (it's also built again after each scan).
const KEEP_SECS: i64 = 600;
/// Plays this far apart or less are one sitting.
const SITTING_GAP_SECS: i64 = 45 * 60;
/// A sitting with more artists than this adds only its first ones (a whole-library shuffle says little).
const SITTING_MAX_ARTISTS: usize = 40;

#[derive(Default, Debug, Clone)]
pub struct Profile {
    pub name: String,
    /// lower-cased genres, with how many of the artist's songs have each
    pub genres: HashMap<String, u32>,
    /// the words of those genres ("russian", "rock")
    pub words: HashSet<String>,
    /// the letters of its name and titles: [latin, cyrillic, greek, cjk (with kana and hangul), arabic and hebrew,
    /// other], as fractions
    pub script: [f32; 6],
    pub year: Option<f32>,
    pub songs: u32,
}

#[derive(Default)]
struct Built {
    at: i64,
    scan: i64,
    artists: HashMap<String, Profile>,
    /// artist → (artist played in the same sittings → in how many)
    together: HashMap<String, HashMap<String, u32>>,
}

/// The profiles, kept between calls (one per server).
#[derive(Default)]
pub struct Cache(tokio::sync::Mutex<Option<Arc<Built>>>);

/// Which of the six scripts a letter is in, if it's a letter.
fn script_of(c: char) -> Option<usize> {
    if !c.is_alphabetic() {
        return None;
    }
    Some(match c as u32 {
        0x41..=0x24F | 0x1E00..=0x1EFF => 0,
        0x400..=0x52F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => 1,
        0x370..=0x3FF | 0x1F00..=0x1FFF => 2,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0x1100..=0x11FF => 3,
        0x590..=0x6FF | 0x750..=0x77F | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF => 4,
        _ => 5,
    })
}

fn add_script(counts: &mut [f32; 6], s: &str) {
    for c in s.chars() {
        if let Some(i) = script_of(c) {
            counts[i] += 1.0;
        }
    }
}

fn normalize(counts: [f32; 6]) -> [f32; 6] {
    let sum: f32 = counts.iter().sum();
    if sum == 0.0 { counts } else { counts.map(|x| x / sum) }
}

/// Words that say nothing about how music sounds.
const PLAIN_WORDS: &[&str] = &["music", "and", "the", "of", "&", "-", "n", "'n'"];

fn genre_words(g: &str) -> impl Iterator<Item = String> + '_ {
    g.split(|c: char| !c.is_alphanumeric()).map(str::to_lowercase).filter(|w| w.len() > 1 && !PLAIN_WORDS.contains(&w.as_str()))
}

async fn build(st: &AppState) -> Built {
    let mut artists: HashMap<String, Profile> = HashMap::new();
    let mut years: HashMap<String, (f64, u32)> = HashMap::new();
    let mut scripts: HashMap<String, [f32; 6]> = HashMap::new();
    type Row = (String, String, String, Option<String>, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as("SELECT artist_id, artist_name, title, genre, year FROM track WHERE missing_since IS NULL")
        .fetch_all(&st.db)
        .await
        .unwrap_or_default();
    for (aid, name, title, genre, year) in rows {
        let p = artists.entry(aid.clone()).or_default();
        if p.name.is_empty() {
            p.name = name.clone();
            add_script(scripts.entry(aid.clone()).or_default(), &name);
        }
        p.songs += 1;
        for g in crate::library::split_genres(genre.as_deref().unwrap_or("")) {
            let g = g.to_lowercase();
            p.words.extend(genre_words(&g));
            *p.genres.entry(g).or_default() += 1;
        }
        if let Some(y) = year.filter(|y| (1000..3000).contains(y)) {
            let e = years.entry(aid.clone()).or_default();
            e.0 += y as f64;
            e.1 += 1;
        }
        add_script(scripts.entry(aid).or_default(), &title);
    }
    for (aid, p) in artists.iter_mut() {
        p.year = years.get(aid).map(|(sum, n)| (sum / *n as f64) as f32);
        p.script = normalize(scripts.get(aid).copied().unwrap_or_default());
    }
    // sittings: each user's plays in order, a new sitting after a long gap
    let plays: Vec<(String, i64, String)> = sqlx::query_as(
        "SELECT p.user_id, p.played_at, t.artist_id FROM play p JOIN track t ON t.id = p.track_id ORDER BY p.user_id, p.played_at",
    )
    .fetch_all(&st.db)
    .await
    .unwrap_or_default();
    let mut together: HashMap<String, HashMap<String, u32>> = HashMap::new();
    let mut sitting: Vec<String> = vec![];
    let mut last: Option<(String, i64)> = None;
    let mut close = |sitting: &mut Vec<String>| {
        for a in sitting.iter() {
            for b in sitting.iter() {
                if a != b {
                    *together.entry(a.clone()).or_default().entry(b.clone()).or_default() += 1;
                }
            }
        }
        sitting.clear();
    };
    for (user, at, artist) in plays {
        if last.as_ref().is_some_and(|(u, t)| *u != user || at - t > SITTING_GAP_SECS) {
            close(&mut sitting);
        }
        if sitting.len() < SITTING_MAX_ARTISTS && !sitting.contains(&artist) {
            sitting.push(artist);
        }
        last = Some((user, at));
    }
    close(&mut sitting);
    Built { at: now(), scan: st.scan.last_scan.load(std::sync::atomic::Ordering::Relaxed), artists, together }
}

async fn built(st: &AppState) -> Arc<Built> {
    let mut g = st.similar.0.lock().await;
    let scan = st.scan.last_scan.load(std::sync::atomic::Ordering::Relaxed);
    if let Some(b) = g.as_ref()
        && now() - b.at < KEEP_SECS
        && b.scan == scan
    {
        return b.clone();
    }
    let b = Arc::new(build(st).await);
    *g = Some(b.clone());
    b
}

/// How alike two profiles are, from their genres, script and era (0…about 3.5).
pub fn alike(a: &Profile, b: &Profile) -> f32 {
    // the same genres, weighted by how much of each artist they are
    let total_a: u32 = a.genres.values().sum();
    let total_b: u32 = b.genres.values().sum();
    let mut genre = 0.0;
    if total_a > 0 && total_b > 0 {
        for (g, n) in &a.genres {
            if let Some(m) = b.genres.get(g) {
                genre += (*n as f32 / total_a as f32).min(*m as f32 / total_b as f32);
            }
        }
    }
    let words = if a.words.is_empty() || b.words.is_empty() {
        0.0
    } else {
        a.words.intersection(&b.words).count() as f32 / a.words.union(&b.words).count() as f32
    };
    let script: f32 = a.script.iter().zip(b.script.iter()).map(|(x, y)| x.min(*y)).sum();
    let era = match (a.year, b.year) {
        (Some(x), Some(y)) => (-(x - y).abs() / 15.0).exp(),
        _ => 0.5,
    };
    2.0 * genre + 1.0 * words + 1.0 * script + 0.5 * era
}

/// The artists that go with this one, best first, with their scores. `related` are artist ids from online info,
/// closest first. The artist itself isn't in the list.
pub async fn artists(st: &AppState, seed: &str, related: &[String], limit: usize) -> Vec<(String, f32)> {
    let b = built(st).await;
    let Some(me) = b.artists.get(seed) else { return vec![] };
    let together = b.together.get(seed);
    let most_together = together.and_then(|t| t.values().max().copied()).unwrap_or(0).max(1) as f32;
    let mut out: Vec<(String, f32)> = b
        .artists
        .iter()
        .filter(|(id, _)| id.as_str() != seed)
        .map(|(id, p)| {
            let mut score = alike(me, p);
            if let Some(n) = together.and_then(|t| t.get(id)) {
                // a few sittings together already say a lot; the most-together artist gets the most
                score += 3.0 * (*n as f32 / most_together).sqrt();
            }
            if let Some(rank) = related.iter().position(|r| r == id) {
                score += 2.5 * (1.0 - rank as f32 / (related.len() as f32 + 1.0));
            }
            (id.clone(), score)
        })
        .filter(|(_, s)| *s > 1.2)
        .collect();
    out.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(genres: &[(&str, u32)], script: &str, year: Option<f32>) -> Profile {
        let mut p = Profile { year, ..Default::default() };
        for (g, n) in genres {
            p.genres.insert(g.to_lowercase(), *n);
            p.words.extend(genre_words(&g.to_lowercase()));
        }
        let mut s = [0.0; 6];
        add_script(&mut s, script);
        p.script = normalize(s);
        p
    }

    #[test]
    fn scripts() {
        assert_eq!(script_of('a'), Some(0));
        assert_eq!(script_of('é'), Some(0));
        assert_eq!(script_of('Ж'), Some(1));
        assert_eq!(script_of('λ'), Some(2));
        assert_eq!(script_of('音'), Some(3));
        assert_eq!(script_of('ש'), Some(4));
        assert_eq!(script_of('7'), None);
    }

    #[test]
    fn alike_ranks_genre_words_script_and_era() {
        let kino = profile(&[("Russian Rock", 5)], "Группа крови", Some(1988.0));
        let ddt = profile(&[("Rock", 3), ("Russian Rock", 1)], "Осень", Some(1991.0));
        let oasis = profile(&[("Britpop", 4)], "Wonderwall", Some(1995.0));
        let opera = profile(&[("Classical", 6)], "Scheherazade", Some(1888.0));
        let (a, b, c) = (alike(&kino, &ddt), alike(&kino, &oasis), alike(&kino, &opera));
        assert!(a > b && b > c, "{a} {b} {c}");
        // genre words alone bring "Rock" and "Russian Rock" together
        let rock = profile(&[("Rock", 1)], "Something", None);
        assert!(alike(&kino, &rock) > alike(&kino, &profile(&[("Jazz", 1)], "Something", None)));
    }
}
