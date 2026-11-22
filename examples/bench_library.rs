//! A big music folder for measuring Conductor: N songs (default 100,000) by N/100 artists on N/10 albums, each a
//! one-second MP3 with real tags (copies of one file, tagged with lofty, so it's quick).
//!
//!   cargo run --release --example bench_library -- DIR [N]
//!
//! Needs ffmpeg once, to make the first file. About 4 KB a song.

use std::path::{Path, PathBuf};

use lofty::{
    config::WriteOptions,
    prelude::*,
    tag::{ItemKey, Tag, TagType},
};

const GENRES: &[&str] = &[
    "Rock", "Pop", "Jazz", "Electronic", "Hip-Hop", "Classical", "Folk", "Metal", "Soul", "Reggae", "Blues", "Country",
    "Ambient", "Punk", "Funk", "Indie", "House", "Techno", "R&B", "Latin",
];
const WORDS: &[&str] = &[
    "Night", "Blue", "River", "Fire", "Golden", "Echo", "Paper", "Glass", "Velvet", "Silver", "Wild", "Quiet", "Electric",
    "Summer", "Stone", "Ocean", "Neon", "Ghost", "Honey", "Midnight", "Northern", "Broken", "Little", "Crimson",
];

fn name(i: usize, n: usize) -> String {
    (0..n).map(|k| WORDS[(i / WORDS.len().pow(k as u32)) % WORDS.len()]).collect::<Vec<_>>().join(" ")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(args.first().expect("usage: bench_library DIR [N]"));
    let n: usize = args.get(1).map(|v| v.parse().expect("N")).unwrap_or(100_000);
    std::fs::create_dir_all(&dir).unwrap();
    let base = dir.join(".base.mp3");
    if !base.is_file() {
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=1", "-ac", "1", "-b:a", "32k"])
            .arg(&base)
            .status()
            .expect("ffmpeg")
            .success();
        assert!(ok, "ffmpeg failed");
    }
    let t = std::time::Instant::now();
    for i in 0..n {
        let artist_i = i / 100;
        let album_i = i / 10;
        let artist = format!("{} {artist_i}", name(artist_i, 2));
        let album = format!("{} {album_i}", name(album_i * 7 + 3, 2));
        let title = format!("{} {i}", name(i * 13 + 5, 3));
        let track = (i % 10) + 1;
        let folder = dir.join(safe(&artist)).join(safe(&album));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(format!("{track:02} {}.mp3", safe(&title)));
        if path.is_file() {
            continue;
        }
        std::fs::copy(&base, &path).unwrap();
        tag(&path, &title, &artist, &album, track, 1960 + (album_i % 65) as u32, GENRES[album_i % GENRES.len()]);
        if (i + 1) % 10_000 == 0 {
            eprintln!("{} songs ({:.0?})", i + 1, t.elapsed());
        }
    }
    eprintln!("done: {n} songs in {} ({:.0?})", dir.display(), t.elapsed());
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect()
}

fn tag(path: &Path, title: &str, artist: &str, album: &str, track: usize, year: u32, genre: &str) {
    let mut tf = lofty::read_from_path(path).expect("reading the copy");
    let mut t = Tag::new(TagType::Id3v2);
    t.set_title(title.into());
    t.set_artist(artist.into());
    t.set_album(album.into());
    t.insert_text(ItemKey::AlbumArtist, artist.into());
    t.set_track(track as u32);
    t.set_year(year);
    t.set_genre(genre.into());
    tf.insert_tag(t);
    tf.save_to_path(path, WriteOptions::default()).expect("writing tags");
}
