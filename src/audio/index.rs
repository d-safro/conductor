//! The analysed songs in memory, for finding the ones that sound alike: each song's numbers (dsp.rs) as z-scores
//! across the library (so a number in Hz and one in dB count alike), weighed by what they say about how music sounds,
//! and compared by distance. Brute force: at 100,000 songs a look is a few milliseconds, and each song's nearest are
//! kept once worked out (until the index is built again, when the analysis changes).

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use super::dsp::{DIMS, N_MFCC};

/// How many of a song's nearest are worked out and kept.
pub const KEPT: usize = 300;
/// At most this many songs' nearest are kept (then they're forgotten and worked out again).
const KEEP_SONGS: usize = 20_000;

/// How much each number counts: groups of numbers, each group as much as its weight however many numbers it has.
pub fn weights() -> [f32; DIMS] {
    let groups: [(usize, f32); 12] = [
        (1, 0.75),       // tempo
        (2, 1.0),        // pulse, onset: the rhythm
        (2, 0.6),        // centroid: bright or dark
        (2, 0.4),        // bandwidth
        (2, 0.4),        // rolloff
        (2, 0.6),        // flatness: noise or tones
        (2, 0.5),        // zero crossings
        (1, 0.35),       // level
        (1, 0.4),        // dynamics
        (12, 1.0),       // chroma: the key, roughly
        (N_MFCC, 2.5),   // MFCC means: the timbre
        (N_MFCC, 1.0),   // MFCC spreads: how the timbre moves
    ];
    let mut w = [0.0; DIMS];
    let mut i = 0;
    for (n, g) in groups {
        for _ in 0..n {
            w[i] = g / n as f32;
            i += 1;
        }
    }
    debug_assert_eq!(i, DIMS);
    w
}

/// One analysed song, as it goes into the index.
pub struct Song {
    pub id: String,
    pub artist: String,
    pub title: String,
    pub features: Vec<f32>,
}

pub struct Index {
    pub ids: Vec<String>,
    pub pos: HashMap<String, usize>,
    pub artist: Vec<String>,
    /// the artist and the title without what's in brackets, in lower case: the same song on another album
    pub same_song: Vec<String>,
    /// each song's numbers, scaled so that a plain distance is the weighed one
    z: Vec<f32>,
    /// the median distance between two songs: what "far" is in this library
    typical: f32,
    near: Mutex<HashMap<usize, Arc<Vec<(usize, f32)>>>>,
}

fn same_song_key(artist: &str, title: &str) -> String {
    let t = title.split(['(', '[']).next().unwrap_or(title).trim().to_lowercase();
    format!("{artist}\u{1}{t}")
}

impl Index {
    /// None with fewer than `least` songs (the z-scores need some songs to mean anything).
    pub fn build(songs: Vec<Song>, least: usize) -> Option<Index> {
        let n = songs.len();
        if n < least.max(2) {
            return None;
        }
        let mut raw: Vec<f32> = Vec::with_capacity(n * DIMS);
        for s in &songs {
            for (d, x) in s.features.iter().enumerate() {
                // the tempo by ratio (100 to 120 is as far as 150 to 180); no beat found: no tempo (the average)
                let x = if d == 0 { if *x > 0.0 { x.log2() } else { f32::NAN } } else { *x };
                raw.push(if x.is_finite() { x } else { f32::NAN });
            }
        }
        let w = weights();
        let mut z = vec![0f32; n * DIMS];
        for d in 0..DIMS {
            let col = || (0..n).map(|i| raw[i * DIMS + d]).filter(|x| !x.is_nan());
            let k = col().count();
            if k == 0 {
                continue;
            }
            let mean = col().map(|x| x as f64).sum::<f64>() / k as f64;
            let sd = (col().map(|x| (x as f64 - mean).powi(2)).sum::<f64>() / k as f64).sqrt();
            if sd < 1e-9 {
                continue; // the same for every song: says nothing
            }
            let weight = w[d].sqrt() as f64;
            for i in 0..n {
                let x = raw[i * DIMS + d];
                // (far outliers are held at 4 spreads, so one odd number can't make a song far from everything)
                z[i * DIMS + d] = if x.is_nan() { 0.0 } else { (((x as f64 - mean) / sd).clamp(-4.0, 4.0) * weight) as f32 };
            }
        }
        let mut ix = Index {
            pos: songs.iter().enumerate().map(|(i, s)| (s.id.clone(), i)).collect(),
            same_song: songs.iter().map(|s| same_song_key(&s.artist, &s.title)).collect(),
            artist: songs.iter().map(|s| s.artist.clone()).collect(),
            ids: songs.into_iter().map(|s| s.id).collect(),
            z,
            typical: 1.0,
            near: Mutex::default(),
        };
        // the median distance of a few thousand pairs (always the same ones)
        let mut d: Vec<f32> = (0..n.min(3000))
            .filter_map(|i| {
                let j = (i * 7919 + 13) % n;
                (i != j).then(|| ix.dist(ix.row(i), ix.row(j)))
            })
            .collect();
        d.sort_by(f32::total_cmp);
        ix.typical = d.get(d.len() / 2).copied().filter(|x| *x > 1e-6).unwrap_or(1.0);
        Some(ix)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn row(&self, i: usize) -> &[f32] {
        &self.z[i * DIMS..(i + 1) * DIMS]
    }

    fn dist(&self, a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
    }

    /// How alike two songs this far apart sound, 0…1: 1 the same, about 0.14 two songs picked at random.
    pub fn similarity(&self, d: f32) -> f32 {
        let r = d / self.typical;
        (-2.0 * r * r).exp()
    }

    /// The songs nearest to a point, nearest first, without those in `skip`.
    pub fn nearest_to(&self, point: &[f32], n: usize, skip: &HashSet<usize>) -> Vec<(usize, f32)> {
        let mut all: Vec<(usize, f32)> =
            (0..self.len()).filter(|i| !skip.contains(i)).map(|i| (i, self.dist(point, self.row(i)))).collect();
        let n = n.min(all.len());
        if n == 0 {
            return vec![];
        }
        all.select_nth_unstable_by(n - 1, |a, b| a.1.total_cmp(&b.1));
        all.truncate(n);
        all.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        all
    }

    /// A song's `KEPT` nearest (itself not among them), nearest first; worked out once.
    pub fn nearest(&self, i: usize) -> Arc<Vec<(usize, f32)>> {
        if let Some(v) = self.near.lock().unwrap_or_else(|e| e.into_inner()).get(&i) {
            return v.clone();
        }
        let v = Arc::new(self.nearest_to(self.row(i), KEPT, &HashSet::from([i])));
        let mut near = self.near.lock().unwrap_or_else(|e| e.into_inner());
        if near.len() >= KEEP_SONGS {
            near.clear();
        }
        near.insert(i, v.clone());
        v
    }

    /// `steps` songs (at most) from `a` to `b`: the nearest to evenly spaced points on the way, each once.
    pub fn path(&self, a: usize, b: usize, steps: usize) -> Vec<usize> {
        let steps = steps.clamp(2, self.len());
        let (pa, pb) = (self.row(a).to_vec(), self.row(b).to_vec());
        let mut used = HashSet::from([a, b]);
        let mut out = vec![a];
        for k in 1..steps - 1 {
            let t = k as f32 / (steps - 1) as f32;
            let p: Vec<f32> = pa.iter().zip(&pb).map(|(x, y)| x + (y - x) * t).collect();
            if let Some(&(i, _)) = self.nearest_to(&p, 1, &used).first() {
                used.insert(i);
                out.push(i);
            }
        }
        out.push(b);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn song(id: &str, artist: &str, title: &str, f: impl Fn(usize) -> f32) -> Song {
        Song { id: id.into(), artist: artist.into(), title: title.into(), features: (0..DIMS).map(f).collect() }
    }

    #[test]
    fn weights_add_up_by_group() {
        let w = weights();
        assert!(w.iter().all(|x| *x > 0.0));
        assert!((w.iter().sum::<f32>() - 9.5).abs() < 1e-3, "{}", w.iter().sum::<f32>());
    }

    #[test]
    fn nearest_are_the_alike_ones() {
        // two kinds of song: numbers near 0 and numbers near 10, a little different each
        let mut songs = vec![];
        for i in 0..6 {
            songs.push(song(&format!("low{i}"), "A", &format!("Low {i}"), |d| (d as f32 * 0.01) + i as f32 * 0.1));
            songs.push(song(&format!("high{i}"), "B", &format!("High {i}"), |d| 10.0 + (d as f32 * 0.02) + i as f32 * 0.1));
        }
        assert!(Index::build(songs.drain(..1).collect(), 4).is_none(), "too few songs");
        let ix = Index::build(songs, 4).unwrap();
        let i = ix.pos["low3"];
        let near = ix.nearest(i);
        assert_eq!(near.len(), ix.len() - 1, "all but itself");
        for (j, _) in near.iter().take(4) {
            assert!(ix.ids[*j].starts_with("low"), "{}", ix.ids[*j]);
        }
        let (j, d) = near[0];
        assert!(ix.ids[j] == "low2" || ix.ids[j] == "low4", "{}", ix.ids[j]);
        assert!(ix.similarity(d) > 0.9 && ix.similarity(near.last().unwrap().1) < 0.2);
        // kept: the same list again
        assert!(Arc::ptr_eq(&near, &ix.nearest(i)));
        // a path from low to high goes through both
        let p = ix.path(ix.pos["low1"], ix.pos["high5"], 4);
        assert_eq!(p.len(), 4);
        assert_eq!((p[0], p[3]), (ix.pos["low1"], ix.pos["high5"]));
        assert_eq!(p.iter().collect::<HashSet<_>>().len(), 4, "each song once");
    }

    #[test]
    fn same_song_is_found_through_brackets() {
        assert_eq!(same_song_key("a", "Song (Remastered 2011)"), same_song_key("a", "song"));
        assert_ne!(same_song_key("a", "Song"), same_song_key("b", "Song"));
    }
}
