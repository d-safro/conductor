//! Lyrics translation and romanization by a local Ollama (`ollama_url`; nothing leaves the house).
//!
//!   translateLyrics?id=<song>&lang=<target, e.g. en>&mode=translate|romanize&line=<1st>&line=<2nd>...
//!   → {"lyricsTranslation": {"lang", "source", "same", "lines": [...]}}   one line out per line in (blank stays blank)
//!     "same": the lyrics are already in that language (or there's nothing to romanize); no lines then.
//!     {"error": "ai-off" | "failed"} when Ollama isn't set up / answering, or keeps giving a wrong answer.
//!
//! The app sends the lines it shows, so this works for every lyrics source. Answers are kept (translation_cache).

use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use tokio::sync::Mutex;

use super::{ApiError, Ctx, Reply, ok};
use crate::db::now;

const MAX_LINES: usize = 400;
/// One translation at a time (a small GPU); others wait.
static ONE_AT_A_TIME: Mutex<()> = Mutex::const_new(());

const LANGS: &[(&str, &str)] = &[
    ("en", "English"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("nl", "Dutch"),
    ("ru", "Russian"),
    ("uk", "Ukrainian"),
    ("pl", "Polish"),
    ("tr", "Turkish"),
    ("ar", "Arabic"),
    ("he", "Hebrew"),
    ("hi", "Hindi"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh", "Chinese"),
    ("sv", "Swedish"),
];
const ROMANIZABLE: &[&str] = &["ja", "ko", "zh", "ru", "uk", "ar", "he", "hi", "el"];

fn prompt(lines: &[&str], lang: &str, mode: &str) -> String {
    let target = LANGS.iter().find(|(k, _)| *k == lang).map(|(_, v)| *v).unwrap_or(lang);
    let task = if mode == "romanize" {
        "Write each line in the Latin alphabet (romanization: Hepburn for Japanese, Revised Romanization for Korean, \
         pinyin with tone marks for Chinese, standard transliteration otherwise). Don't translate. Lines already in the \
         Latin alphabet stay as they are."
            .to_string()
    } else {
        format!(
            "Translate each line into {target}, naturally, keeping the meaning and the feel of a song lyric. Lines \
             already in {target}, names and sounds like \"oh\" or \"la la\" stay as they are."
        )
    };
    format!(
        "These are the {n} lines of a song's lyrics, as a JSON array.\n{task}\nGive back exactly {n} lines, one for each \
         line in, in the same order: never merge, split, skip or add lines. Also say the lyrics' main language as an \
         ISO 639-1 code in \"source\".\nLines: {}",
        serde_json::to_string(lines).unwrap_or_default(),
        n = lines.len()
    )
}

enum Asked {
    Off,
    Bad,
    Got(Value),
}

async fn ask(base: &str, model: &str, text: &str) -> Asked {
    let body = json!({
        "model": model, "stream": false, "options": { "temperature": 0.2 },
        "format": { "type": "object", "properties": { "source": { "type": "string" },
            "lines": { "type": "array", "items": { "type": "string" } } }, "required": ["source", "lines"] },
        "messages": [{ "role": "user", "content": text }],
    });
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(300)).build().expect("http client");
    let r = match http.post(format!("{}/api/chat", base.trim_end_matches('/'))).json(&body).send().await {
        Ok(r) => r,
        Err(e) if e.is_connect() || e.is_timeout() => return Asked::Off,
        Err(_) => return Asked::Bad,
    };
    if !r.status().is_success() {
        tracing::warn!(status = %r.status(), "Ollama said no (is the model pulled?)");
        return Asked::Bad;
    }
    let Ok(v) = r.json::<Value>().await else { return Asked::Bad };
    match v["message"]["content"].as_str().and_then(|c| serde_json::from_str::<Value>(c).ok()) {
        Some(x) => Asked::Got(x),
        None => Asked::Bad,
    }
}

pub async fn translate_lyrics(c: &Ctx) -> Result<Reply, ApiError> {
    let lang: String = c.p.get("lang").unwrap_or("en").to_lowercase().chars().take(5).collect();
    let mode = if c.p.get("mode") == Some("romanize") { "romanize" } else { "translate" };
    let lines: Vec<&str> = c.p.all("line").into_iter().take(MAX_LINES).collect();
    let id = c.p.get("id").unwrap_or("");
    let model = &c.st.cfg.translate_model;
    let key = {
        let mut h = Sha1::new();
        h.update(json!([id, lang, mode, model, lines]).to_string());
        format!("{:x}", h.finalize())
    };
    let reply = |v: Value| ok(json!({ "lyricsTranslation": v }));
    if let Some(j) = sqlx::query_scalar::<_, String>("SELECT json FROM translation_cache WHERE key = ?")
        .bind(&key)
        .fetch_optional(&c.st.db)
        .await?
        .and_then(|j| serde_json::from_str::<Value>(&j).ok())
    {
        return reply(j);
    }
    let words: Vec<&str> = lines.iter().copied().filter(|l| !l.trim().is_empty()).collect();
    if words.is_empty() {
        return reply(json!({ "lang": lang, "source": "", "same": true, "lines": [] }));
    }
    let Some(base) = c.st.cfg.ollama_url.as_deref().filter(|u| !u.is_empty()) else {
        return reply(json!({ "error": "ai-off" }));
    };
    let got = {
        let _turn = ONE_AT_A_TIME.lock().await;
        let mut got = None;
        for _ in 0..2 {
            // a second try when the line count comes back wrong
            match ask(base, model, &prompt(&words, &lang, mode)).await {
                Asked::Off => return reply(json!({ "error": "ai-off" })),
                Asked::Bad => continue,
                Asked::Got(v) => {
                    if v["lines"].as_array().is_some_and(|a| a.len() == words.len()) {
                        got = Some(v);
                        break;
                    }
                    tracing::info!(song = id, "translation came back with the wrong number of lines; trying again");
                }
            }
        }
        got
    };
    let Some(got) = got else { return reply(json!({ "error": "failed" })) };
    let source: String = got["source"].as_str().unwrap_or("").to_lowercase().chars().take(2).collect();
    let same = (mode == "translate" && source == lang)
        || (mode == "romanize" && !source.is_empty() && !ROMANIZABLE.contains(&source.as_str()));
    let res = if same {
        json!({ "lang": lang, "source": source, "same": true, "lines": [] })
    } else {
        let mut it = got["lines"].as_array().into_iter().flatten().map(|x| match x {
            Value::String(s) => s.clone(),
            o => o.to_string(),
        });
        let out: Vec<String> =
            lines.iter().map(|l| if l.trim().is_empty() { String::new() } else { it.next().unwrap_or_default() }).collect();
        json!({ "lang": lang, "source": source, "same": false, "lines": out })
    };
    sqlx::query("INSERT OR REPLACE INTO translation_cache (key, json, created_at) VALUES (?, ?, ?)")
        .bind(&key)
        .bind(res.to_string())
        .bind(now())
        .execute(&c.st.db)
        .await?;
    reply(res)
}
