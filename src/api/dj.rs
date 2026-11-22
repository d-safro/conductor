//! A DJ for Encore: before a song, a line or two about it, written by the local Ollama (`ollama_url`) and read
//! aloud by Piper (the dj-voice service, `dj_voice_url`). Only facts the library knows go into it (title, artist,
//! album, year, genre, how often you played it, whether you liked it). Nothing leaves the house; without both
//! services the answer is "ai-off". Lines are kept in `<data_dir>/dj` (the newest few hundred), so asking twice
//! is free.
//!
//!   getDjLine?next=<song>[&prev=<song>][&lang=xx]  → {"djLine": {"text", "id", "duration"}} or {"djLine": {"error"}}
//!   djAudio?id=<id>                                → the spoken line (audio/wav)

use std::path::PathBuf;

use axum::{body::Body, http::StatusCode, http::header};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use super::{ApiError, Ctx, Reply, items, ok, raw};

const KEEP: usize = 300;
/// Piper voices by language (dj-voice downloads each the first time); other languages: English.
const VOICES: &[(&str, &str, &str)] = &[
    ("en", "English", "en_US-lessac-medium"),
    ("de", "German", "de_DE-thorsten-medium"),
    ("es", "Spanish", "es_ES-davefx-medium"),
    ("fr", "French", "fr_FR-siwis-medium"),
    ("it", "Italian", "it_IT-riccardo-x_low"),
    ("pt", "Portuguese", "pt_BR-faber-medium"),
    ("ru", "Russian", "ru_RU-irina-medium"),
    ("uk", "Ukrainian", "uk_UA-ukrainian_tts-medium"),
    ("pl", "Polish", "pl_PL-gosia-medium"),
    ("nl", "Dutch", "nl_BE-nathalie-medium"),
    ("sv", "Swedish", "sv_SE-nst-medium"),
    ("tr", "Turkish", "tr_TR-dfki-medium"),
    ("zh", "Chinese", "zh_CN-huayan-medium"),
];

fn folder(c: &Ctx) -> PathBuf {
    c.st.cfg.data_dir.join("dj")
}

fn valid_id(id: &str) -> bool {
    id.len() == 20 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What the DJ may say about a song: only what the library knows.
async fn facts(c: &Ctx, id: &str, plays: bool) -> Option<Value> {
    let t = items::track(c, id).await.ok()?;
    let mut f = json!({ "title": t.title, "artist": t.artist_name, "album": t.album_name });
    if let Some(y) = t.year {
        f["year"] = y.into();
    }
    if let Some(g) = t.genre.as_deref().and_then(|g| crate::library::split_genres(g).into_iter().next()) {
        f["genre"] = g.into();
    }
    if plays {
        let n = t.play_count.unwrap_or(0);
        f["you have played it"] = match n {
            0 => "never".into(),
            1 => "once".into(),
            n => format!("{n} times").into(),
        };
        if t.starred_at.is_some() {
            f["you liked it"] = true.into();
        }
    }
    Some(f)
}

pub fn prompt(prev: Option<&Value>, next: &Value, language: &str) -> String {
    format!(
        "You are a warm, brief radio DJ on someone's personal music app. Write what you say between two songs: \
         one or two short sentences, at most 30 words, spoken {language}, no emojis, no hashtags, no quotes around it. \
         Song titles and names stay as they are. \
         Introduce the next song by its title and artist. You may mention the previous song, the album, the year \
         or how often the listener has played it (their plays, not yours: talk to them as \"you\"), but ONLY facts \
         given below: never invent facts about the artists.\n\
         Previous song: {}\nNext song: {next}",
        prev.map(Value::to_string).unwrap_or_else(|| "none (this starts the session)".into())
    )
}

pub fn clean(text: &str) -> String {
    let t = text.replace(['"', '*'], "");
    t.split_whitespace().take(45).collect::<Vec<_>>().join(" ")
}

/// Seconds of audio in a WAV file (0 when it isn't one).
pub fn wav_seconds(b: &[u8]) -> f64 {
    if b.len() < 44 || &b[..4] != b"RIFF" {
        return 0.0;
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as f64;
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]) as f64;
    let (rate, channels, bits) = (u32_at(24), u16_at(22), u16_at(34));
    let frame = (bits / 8.0).floor() * channels;
    let size = b.windows(4).position(|w| w == b"data").filter(|i| i + 8 <= b.len()).map(|i| u32_at(i + 4)).unwrap_or((b.len() - 44) as f64);
    if rate <= 0.0 || frame <= 0.0 { 0.0 } else { (size / (rate * frame) * 100.0).round() / 100.0 }
}

enum Failed {
    Off,
    Bad,
}

async fn write_line(c: &Ctx, base: &str, prev: Option<&Value>, next: &Value, language: &str) -> Result<String, Failed> {
    let model = c.st.cfg.dj_model.clone().unwrap_or_else(|| c.st.cfg.translate_model.clone());
    let body = json!({
        "model": model, "stream": false, "options": { "temperature": 0.8 },
        "format": { "type": "object", "properties": { "line": { "type": "string" } }, "required": ["line"] },
        "messages": [{ "role": "user", "content": prompt(prev, next, language) }],
    });
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build().expect("http client");
    let r = http.post(format!("{}/api/chat", base.trim_end_matches('/'))).json(&body).send().await.map_err(|e| {
        if e.is_connect() || e.is_timeout() { Failed::Off } else { Failed::Bad }
    })?;
    if !r.status().is_success() {
        return Err(Failed::Bad);
    }
    let v: Value = r.json().await.map_err(|_| Failed::Bad)?;
    let line = v["message"]["content"].as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).ok_or(Failed::Bad)?;
    Ok(clean(line["line"].as_str().unwrap_or("")))
}

/// The spoken line; Err(true) when there's no voice for that language.
async fn speak(voice_url: &str, text: &str, voice: &str) -> Result<Vec<u8>, Result<bool, Failed>> {
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().expect("http client");
    let r = http
        .post(format!("{}/speak", voice_url.trim_end_matches('/')))
        .json(&json!({ "text": text, "voice": voice }))
        .send()
        .await
        .map_err(|e| Err(if e.is_connect() || e.is_timeout() { Failed::Off } else { Failed::Bad }))?;
    if !r.status().is_success() {
        return Err(Ok(true));
    }
    Ok(r.bytes().await.map_err(|_| Err(Failed::Bad))?.to_vec())
}

fn prune(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> =
        rd.filter_map(|e| e.ok()).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).collect();
    files.sort();
    let extra = files.len().saturating_sub(KEEP * 2);
    for (_, p) in files.into_iter().take(extra) {
        let _ = std::fs::remove_file(p);
    }
}

pub async fn line(c: &Ctx) -> Result<Reply, ApiError> {
    let next_id = c.p.req("next")?.to_string();
    let lang = c.p.get("lang").unwrap_or("en").chars().take(2).collect::<String>().to_lowercase();
    let mut voice = VOICES.iter().find(|v| v.0 == lang).unwrap_or(&VOICES[0]);
    let reply = |v: Value| ok(json!({ "djLine": v }));
    let Some(next) = facts(c, &next_id, true).await else { return reply(json!({ "error": "unknown song" })) };
    let prev = match c.p.get("prev").filter(|p| !p.is_empty()) {
        Some(p) => facts(c, p, false).await,
        None => None,
    };
    let model = c.st.cfg.dj_model.clone().unwrap_or_else(|| c.st.cfg.translate_model.clone());
    let key: String = {
        let mut h = Sha1::new();
        h.update(json!([prev, next, model, voice.0]).to_string());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()[..20].to_string()
    };
    let dir = folder(c);
    let (meta, audio) = (dir.join(format!("{key}.json")), dir.join(format!("{key}.wav")));
    if audio.is_file()
        && let Some(v) = std::fs::read_to_string(&meta).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        return reply(v);
    }
    let (Some(base), Some(voice_url)) = (
        c.st.cfg.ollama_url.as_deref().filter(|u| !u.is_empty()),
        c.st.cfg.dj_voice_url.as_deref().filter(|u| !u.is_empty()),
    ) else {
        return reply(json!({ "error": "ai-off" }));
    };
    // one AI job at a time (the same queue as lyrics translation: a small GPU)
    let _turn = super::translate::ONE_AT_A_TIME.lock().await;
    let fail = |f: Failed| reply(json!({ "error": match f { Failed::Off => "ai-off", Failed::Bad => "failed" } }));
    let mut text = match write_line(c, base, prev.as_ref(), &next, voice.1).await {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => return fail(Failed::Bad),
        Err(f) => return fail(f),
    };
    let wav = match speak(voice_url, &text, voice.2).await {
        Ok(w) => w,
        Err(Ok(_)) if voice.0 != "en" => {
            // no voice for that language: English then
            voice = &VOICES[0];
            text = match write_line(c, base, prev.as_ref(), &next, voice.1).await {
                Ok(t) if !t.is_empty() => t,
                Ok(_) => return fail(Failed::Bad),
                Err(f) => return fail(f),
            };
            match speak(voice_url, &text, voice.2).await {
                Ok(w) => w,
                Err(Err(f)) => return fail(f),
                Err(Ok(_)) => return fail(Failed::Bad),
            }
        }
        Err(Ok(_)) => return fail(Failed::Bad),
        Err(Err(f)) => return fail(f),
    };
    let out = json!({ "text": text, "id": key, "duration": wav_seconds(&wav) });
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(&audio, &wav).is_ok() {
        let _ = std::fs::write(&meta, out.to_string());
    }
    prune(&dir);
    reply(out)
}

pub async fn audio(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    let p = folder(c).join(format!("{id}.wav"));
    if !valid_id(id) || !p.is_file() {
        return Err(ApiError::not_found("DJ line"));
    }
    let b = tokio::fs::read(&p).await.map_err(|_| ApiError::not_found("DJ line"))?;
    Ok(raw(StatusCode::OK, &[(header::CONTENT_TYPE, "audio/wav".into())], Body::from(b)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_and_text() {
        // 8000 Hz, mono, 16 bit: 16000 bytes of data = 1 s
        let mut w = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(8000u32.to_le_bytes());
        w.extend(16000u32.to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend(16000u32.to_le_bytes());
        w.extend(vec![0u8; 16000]);
        assert_eq!(wav_seconds(&w), 1.0);
        assert_eq!(wav_seconds(b"nope"), 0.0);
        assert_eq!(clean("  \"Up next*\"   is  this "), "Up next is this");
        assert!(valid_id("0123456789abcdef0123") && !valid_id("../../etc/passwd0000"));
        assert!(prompt(None, &json!({"title": "X"}), "Russian").contains("spoken Russian"));
    }
}
