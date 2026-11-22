//! Robustness: every parser that reads something from outside (exports, feeds, lyrics, tags, rules, dates,
//! WireGuard files, concert listings) gets broken and random input, and must answer (an error is fine) without
//! panicking. Random bytes, random text full of the characters parsers trip on, and real-looking samples with bytes
//! flipped, cut short, repeated or spliced together.
//!
//! The same seed every run, so a failure can be repeated; `FUZZ_SEED=… FUZZ_ITERS=20000 cargo test robustness`
//! looks further.

use std::panic::{AssertUnwindSafe, catch_unwind};

use rand::{Rng, SeedableRng, rngs::StdRng};
use serde_json::{Value, json};

fn iters() -> usize {
    std::env::var("FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(400)
}

fn rng() -> StdRng {
    StdRng::seed_from_u64(std::env::var("FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0xC0DE))
}

/// Characters that break parsers: markup, quotes, separators, multi-byte letters, controls, a BOM.
const NASTY: &[&str] = &[
    "<", ">", "&", "\"", "'", ",", ";", ":", "\n", "\r\n", "\t", "\0", "\\", "/", "{", "}", "[", "]", "#", "%", "=", "-",
    "é", "日本", "🎵", "\u{200b}", "\u{feff}", "\u{1f}", "ß", "İ", "[00:", "]", ".", "0", "9", "2024-", "T", "Z", "+",
    "<![CDATA[", "]]>", "<!--", "-->", "&amp;", "&#x", "<?xml", "</", "/>", "http://", "null", "true", "-1", "1e999",
];

fn junk_text(r: &mut StdRng) -> String {
    let n = r.gen_range(0..80);
    (0..n)
        .map(|_| if r.gen_bool(0.3) { char::from(r.gen_range(32u8..127)).to_string() } else { NASTY[r.gen_range(0..NASTY.len())].to_string() })
        .collect()
}

fn junk_bytes(r: &mut StdRng) -> Vec<u8> {
    let n = r.gen_range(0..600);
    (0..n).map(|_| r.r#gen()).collect()
}

/// A sample with some damage: bytes flipped or dropped, cut short, a piece repeated, nasty text spliced in.
fn mutate(r: &mut StdRng, seed: &[u8]) -> Vec<u8> {
    let mut b = seed.to_vec();
    for _ in 0..r.gen_range(1..6) {
        if b.is_empty() {
            b.extend_from_slice(junk_text(r).as_bytes());
            continue;
        }
        let i = r.gen_range(0..b.len());
        match r.gen_range(0..6) {
            0 => b[i] = r.r#gen(),
            1 => {
                b.remove(i);
            }
            2 => b.truncate(i),
            3 => {
                let j = r.gen_range(i..b.len().min(i + 40) + 1).min(b.len());
                let piece = b[i..j].to_vec();
                for (k, x) in piece.into_iter().enumerate() {
                    b.insert(i + k, x);
                }
            }
            4 => {
                for (k, x) in junk_text(r).bytes().enumerate() {
                    b.insert((i + k).min(b.len()), x);
                }
            }
            _ => b.insert(i, NASTY[r.gen_range(0..NASTY.len())].as_bytes()[0]),
        }
    }
    b
}

fn junk_json(r: &mut StdRng, depth: u32) -> Value {
    match r.gen_range(0..if depth > 3 { 5 } else { 8 }) {
        0 => Value::Null,
        1 => Value::Bool(r.r#gen()),
        2 => json!(r.gen_range(-1_000_000_000i64..1_000_000_000)),
        3 => json!(r.r#gen::<f64>() * 1e12 - 5e11),
        4 => Value::String(junk_text(r)),
        5 => Value::Array((0..r.gen_range(0..4)).map(|_| junk_json(r, depth + 1)).collect()),
        _ => {
            let keys = ["all", "any", "is", "contains", "inTheLast", "inPlaylist", "title", "year", "sort", "limit", "ts", "ms_played",
                "master_metadata_track_name", "trackName", "artistName", "endTime", "id", "name", "_embedded", "events", "dates", "start"];
            Value::Object((0..r.gen_range(0..4)).map(|_| (keys[r.gen_range(0..keys.len())].to_string(), junk_json(r, depth + 1))).collect())
        }
    }
}

/// Run `f` on `n` inputs; on a panic, fail with the input that caused it.
fn survive<I: std::fmt::Debug>(name: &str, mut make: impl FnMut(&mut StdRng) -> I, f: impl Fn(&I)) {
    let mut r = rng();
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failed = None;
    for _ in 0..iters() {
        let input = make(&mut r);
        if catch_unwind(AssertUnwindSafe(|| f(&input))).is_err() {
            failed = Some(format!("{input:?}"));
            break;
        }
    }
    std::panic::set_hook(prev);
    if let Some(i) = failed {
        let short: String = i.chars().take(600).collect();
        panic!("{name} panicked on: {short}");
    }
}

fn text_input(r: &mut StdRng, seeds: &[&str]) -> String {
    match r.gen_range(0..3) {
        0 => junk_text(r),
        1 => String::from_utf8_lossy(&junk_bytes(r)).into_owned(),
        _ => {
            let seed = seeds[r.gen_range(0..seeds.len())];
            String::from_utf8_lossy(&mutate(r, seed.as_bytes())).into_owned()
        }
    }
}

// ── the parsers ──

#[test]
fn dates_and_times() {
    let seeds = ["2024-01-02 13:45", "2024-01-02T13:45:10Z", "2024-01-02T13:45:10.123+02:00", "2024-01-02", "Tue, 10 Nov 2026 08:00:00 +0000"];
    survive("import::parse_time", |r| text_input(r, &seeds), |s| {
        let _ = crate::import::parse_time(s);
    });
    survive("db::parse_iso", |r| text_input(r, &seeds), |s| {
        let _ = crate::db::parse_iso(s);
    });
}

#[test]
fn lyrics() {
    let seeds = ["[00:00.50]hello\n[00:01.75]world\n", "[ar:Someone]\n[01:02.003][01:05]twice\nplain", "[99:59.999]end"];
    survive("media::parse_lyrics", |r| text_input(r, &seeds), |s| {
        let _ = crate::api::media::parse_lyrics(s);
    });
}

#[test]
fn podcast_feeds() {
    let seeds = [r#"<?xml version="1.0"?><rss xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd"><channel><title>T</title>
<itunes:author>A</itunes:author><itunes:image href="http://x/c.png"/><item><title>E</title><guid>g</guid>
<pubDate>Wed, 11 Nov 2026 08:00:00 +0000</pubDate><itunes:duration>1:02:03</itunes:duration>
<enclosure url="http://x/e.mp3" length="99" type="audio/mpeg"/><description><![CDATA[<p>hi</p>]]></description></item></channel></rss>"#];
    survive("podcasts::parse_feed", |r| text_input(r, &seeds), |s| {
        let _ = crate::api::podcasts::parse_feed("http://example.com/feed", s);
    });
}

#[test]
fn smart_playlist_rules() {
    survive("smart::compile", |r| junk_json(r, 0), |v| {
        let _ = crate::api::smart::compile(v);
    });
    let seed = json!({"all": [{"is": {"loved": true}}, {"inTheLast": {"lastplayed": 30}}, {"any": [{"contains": {"title": "a"}}, {"inTheRange": {"year": [1990, 1999]}}]}], "sort": "-year,title", "limit": 50});
    survive("smart::compile (damaged)", |r| serde_json::from_slice::<Value>(&mutate(r, seed.to_string().as_bytes())).unwrap_or(Value::Null), |v| {
        let _ = crate::api::smart::compile(v);
    });
}

#[test]
fn wireguard_files() {
    let seeds = ["[Interface]\nPrivateKey = abc=\nAddress = 10.2.0.2/32, fd00::2/128\n[Peer]\nPublicKey = def=\nEndpoint = 127.0.0.1:51820\nPresharedKey = x\n"];
    survive("gluetun::parse_wg", |r| text_input(r, &seeds), |s| {
        let _ = crate::prelude::gluetun::parse_wg(s);
    });
}

#[test]
fn concert_listings() {
    survive("concerts::parse_ticketmaster / parse_mb_events", |r| junk_json(r, 0), |v| {
        let _ = crate::concerts::parse_ticketmaster(v, "Someone", "2026-01-01");
        let _ = crate::concerts::parse_mb_events(v, "Someone", "2026-01-01");
    });
}

/// Samples of every export format, by the name each is found under.
const EXPORTS: &[(&str, &str)] = &[
    ("StreamingHistory_music_0.json", r#"[{"endTime":"2023-01-02 13:45","artistName":"A","trackName":"T","msPlayed":180000}]"#),
    ("Streaming_History_Audio_2023.json", r#"[{"ts":"2024-03-01T10:00:00Z","ms_played":180000,"master_metadata_track_name":"One","master_metadata_album_artist_name":"Alpha","master_metadata_album_album_name":"X","spotify_track_uri":"spotify:track:1111111111111111111111"}]"#),
    ("YourLibrary.json", r#"{"tracks":[{"artist":"A","album":"B","track":"C","uri":"spotify:track:1111111111111111111111"}],"albums":[{"artist":"A","album":"B"}],"artists":[{"name":"A"}]}"#),
    ("Playlist1.json", r#"{"playlists":[{"name":"P","lastModifiedDate":"2024-01-01","items":[{"track":{"trackName":"T","artistName":"A","albumName":"B","trackUri":"spotify:track:1111111111111111111111"},"addedDate":"2024-01-01"}]}]}"#),
    ("Library.xml", r#"<?xml version="1.0"?><plist version="1.0"><dict><key>Tracks</key><dict><key>1</key><dict><key>Name</key><string>T</string><key>Artist</key><string>A</string><key>Play Count</key><integer>3</integer><key>Loved</key><true/></dict></dict><key>Playlists</key><array><dict><key>Name</key><string>P</string><key>Playlist Items</key><array><dict><key>Track ID</key><integer>1</integer></dict></array></dict></array></dict></plist>"#),
    ("Apple Music Library Tracks.json", r#"[{"Title":"T","Artist":"A","Album":"B","Track Play Count":3,"Favorite Status":"FAVORITED"}]"#),
    ("Apple Music Play Activity.csv", "Song Name,Artist Name,Album Name,Event Start Timestamp,Play Duration Milliseconds,Event Type\nT,A,B,2024-01-01T10:00:00Z,180000,PLAY_END\n"),
    ("watch-history.json", r#"[{"header":"YouTube Music","title":"Watched T","subtitles":[{"name":"A - Topic"}],"time":"2024-01-01T10:00:00Z"}]"#),
    ("music library songs.csv", "Song Title,Album Title,Artist Name 1\nT,B,A\n"),
    ("Liked music.csv", "Video ID,Time Added\nabc,2024-01-01\n"),
    ("listens.jsonl", r#"{"listened_at":1700000000,"track_metadata":{"track_name":"T","artist_name":"A","release_name":"B"}}"#),
    ("feedback.json", r#"[{"score":1,"track_metadata":{"track_name":"T","artist_name":"A"}}]"#),
    ("road trip.m3u8", "#EXTM3U\n#EXTINF:180,A - T\nmusic/A/T.flac\nhttp://x/y.mp3\n"),
    ("exportify.csv", "Track URI,Track Name,Artist Name(s),Album Name,Added At\nspotify:track:1111111111111111111111,T,A,B,2024-01-01T00:00:00Z\n"),
];

#[test]
fn exports() {
    survive(
        "import::parse_files",
        |r| {
            let (name, seed) = EXPORTS[r.gen_range(0..EXPORTS.len())];
            let bytes = match r.gen_range(0..4) {
                0 => junk_bytes(r),
                1 => junk_text(r).into_bytes(),
                _ => mutate(r, seed.as_bytes()),
            };
            (name.to_string(), bytes)
        },
        |(name, bytes)| {
            let files = crate::import::Files { files: vec![(name.clone(), bytes.clone())] };
            let _ = crate::import::parse_files(&files);
        },
    );
}

#[test]
fn zips() {
    // a real zip of a few exports, damaged; and random bytes called .zip
    let real = {
        use std::io::Write;
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (n, b) in &EXPORTS[..4] {
            w.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            w.write_all(b.as_bytes()).unwrap();
        }
        w.finish().unwrap().into_inner()
    };
    survive(
        "import::Files::add (zip)",
        |r| if r.gen_bool(0.5) { mutate(r, &real) } else { junk_bytes(r) },
        |b| {
            let mut f = crate::import::Files::default();
            let mut left = 50 << 20;
            if f.add("export.zip".into(), b.clone(), 0, &mut left).is_ok() {
                let _ = crate::import::parse_files(&f);
            }
        },
    );
}

#[test]
fn audio_tags() {
    // files a download could bring: random bytes and damaged real files, with audio names
    let dir = tempfile::tempdir().unwrap();
    let seeds: Vec<(&str, Vec<u8>)> = ["mp3", "flac", "m4a", "ogg", "opus", "wav"]
        .iter()
        .map(|ext| {
            let p = dir.path().join(format!("seed.{ext}"));
            let ok = std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=0.3", "-metadata", "title=T", "-metadata", "artist=A"])
                .arg(&p)
                .status()
                .is_ok_and(|s| s.success());
            (*ext, if ok { std::fs::read(&p).unwrap_or_default() } else { vec![] })
        })
        .collect();
    let path = dir.path().join("x");
    survive(
        "library::read_meta",
        |r| {
            let (ext, seed) = &seeds[r.gen_range(0..seeds.len())];
            let b = if seed.is_empty() || r.gen_bool(0.25) { junk_bytes(r) } else { mutate(r, seed) };
            (ext.to_string(), b)
        },
        |(ext, b)| {
            let p = path.with_extension(ext);
            std::fs::write(&p, b).unwrap();
            let _ = crate::library::read_meta(&p);
        },
    );
}
