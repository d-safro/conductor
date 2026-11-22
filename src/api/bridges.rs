//! Two helpers that run beside Maestro (optional, like AudioMuse), both working on the music folder read-only:
//!
//! - **Song ID** (`songid_url`): "What's playing?" against your own library only. `POST identifySong` (login in
//!   the query, 5–15 s of audio as the body) → `{"songIdentification": {"found": true, "song": {...}, "offset",
//!   "score"}}` or `{"found": false, "indexing", "songs"}`. The service answers `POST /identify` with the file's
//!   path (relative to the music folder) and `GET /status` with `{songs, hashes, indexing}`.
//! - **Automix** (`mixinfo_dir`): where each song's sound starts and ends, where its intro ends and outro starts,
//!   its tempo. `getMixInfo?id=&id=…` → `{"mixInfo": {"song": [{id, start, introEnd, outroStart, end, bpm, beat,
//!   dur}]}}` (seconds). The analyzer writes `info.jsonl` (one line per file); songs not measured yet go to the
//!   front of its list (`wanted.txt`), so asking for the songs coming up in a queue gets them done.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

use serde_json::{Map, Value, json};

use super::{ApiError, Ctx, Reply, items, ok};

// ───────────── song ID ─────────────

pub async fn identify_song(c: &Ctx) -> Result<Reply, ApiError> {
    let reply = |v: Value| ok(json!({ "songIdentification": v }));
    let Some(base) = c.st.cfg.songid_url.as_deref().filter(|u| !u.is_empty()).map(|u| u.trim_end_matches('/').to_string()) else {
        return reply(json!({ "found": false, "error": "Song ID isn't set up on the server (songid_url)" }));
    };
    if c.body.is_empty() {
        return reply(json!({ "found": false, "error": "send the recording as the body" }));
    }
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().expect("http client");
    let r = match http
        .post(format!("{base}/identify"))
        .header("content-type", "application/octet-stream")
        .body(c.body.clone())
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return reply(json!({ "found": false, "error": "song-id isn't running" })),
    };
    let status = r.status();
    let v: Value = r.json().await.unwrap_or_default();
    if !status.is_success() {
        let e = v["error"].as_str().map(String::from).unwrap_or_else(|| format!("HTTP {}", status.as_u16()));
        return reply(json!({ "found": false, "error": e }));
    }
    let rel = v["path"].as_str().unwrap_or("");
    let found = if rel.is_empty() { vec![] } else { items::tracks_where(c, "t.rel_path = ?", &[rel], "LIMIT 1").await? };
    let Some(t) = found.first() else {
        let st: Value = match http.get(format!("{base}/status")).timeout(std::time::Duration::from_secs(5)).send().await {
            Ok(r) => r.json().await.unwrap_or_default(),
            Err(_) => Value::Null,
        };
        return reply(json!({ "found": false, "indexing": st["indexing"].as_bool().unwrap_or(false),
            "songs": st["songs"].as_i64().unwrap_or(0) }));
    };
    reply(json!({ "found": true, "song": items::song_json(t), "offset": v["offset"].as_f64().unwrap_or(0.0),
        "score": v["score"].as_f64().unwrap_or(0.0) }))
}

// ───────────── Automix ─────────────

const FIELDS: &[&str] = &["start", "introEnd", "outroStart", "end", "bpm", "beat", "dur"];

/// info.jsonl as read so far: path → its line, and (size, mtime) when last read (it only grows: read the new part).
#[derive(Default)]
struct Info {
    dir: PathBuf,
    by_path: HashMap<String, Map<String, Value>>,
    seen: (u64, Option<std::time::SystemTime>),
}
static INFO: LazyLock<Mutex<Info>> = LazyLock::new(Default::default);

fn reload(g: &mut Info, dir: &Path) {
    use std::io::{BufRead, Seek};
    if g.dir != dir {
        *g = Info { dir: dir.to_path_buf(), ..Default::default() };
    }
    let path = dir.join("info.jsonl");
    let Ok(meta) = std::fs::metadata(&path) else { return };
    let now = (meta.len(), meta.modified().ok());
    if now == g.seen {
        return;
    }
    let from = if meta.len() >= g.seen.0 { g.seen.0 } else { 0 };
    if from == 0 {
        g.by_path.clear();
    }
    let Ok(mut f) = std::fs::File::open(&path) else { return };
    if f.seek(std::io::SeekFrom::Start(from)).is_err() {
        return;
    }
    let mut read = from;
    let mut r = std::io::BufReader::new(f);
    let mut line = String::new();
    loop {
        line.clear();
        match r.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) if !line.ends_with('\n') => break, // half-written: read it again next time
            Ok(n) => read += n as u64,
        }
        if let Ok(Value::Object(j)) = serde_json::from_str::<Value>(line.trim_end())
            && let Some(p) = j.get("path").and_then(Value::as_str)
        {
            g.by_path.insert(p.to_string(), j);
        }
    }
    g.seen = if read == meta.len() { now } else { (read, None) };
}

/// Put these at the front of the analyzer's list (newest last; the file stays short).
fn want(dir: &Path, paths: &[String]) {
    if paths.is_empty() {
        return;
    }
    let p = dir.join("wanted.txt");
    let old = std::fs::read_to_string(&p).unwrap_or_default();
    let mut lines: Vec<&str> = old.lines().map(str::trim).filter(|l| !l.is_empty() && !paths.iter().any(|x| x == l)).collect();
    lines.extend(paths.iter().map(String::as_str));
    let keep = &lines[lines.len().saturating_sub(300)..];
    let tmp = dir.join("wanted.txt.tmp");
    if std::fs::create_dir_all(dir).is_ok() && std::fs::write(&tmp, keep.join("\n") + "\n").is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

pub async fn mix_info(c: &Ctx) -> Result<Reply, ApiError> {
    let Some(dir) = c.st.cfg.mixinfo_dir.clone() else { return ok(json!({ "mixInfo": { "song": [] } })) };
    let ids: Vec<&str> = c.p.all("id").into_iter().take(200).collect();
    let mut paths: Vec<(String, String)> = vec![];
    for chunk in ids.chunks(200) {
        let sql = format!("SELECT id, rel_path FROM track WHERE id IN ({})", vec!["?"; chunk.len()].join(","));
        let mut q = sqlx::query_as::<_, (String, String)>(&sql);
        for id in chunk {
            q = q.bind(*id);
        }
        paths.extend(q.fetch_all(&c.st.db).await?);
    }
    let by_id: HashMap<String, String> = paths.into_iter().collect();
    let (out, missing) = {
        let mut g = INFO.lock().unwrap_or_else(|e| e.into_inner());
        reload(&mut g, &dir);
        let (mut out, mut missing) = (vec![], vec![]);
        for id in &ids {
            let Some(rel) = by_id.get(*id) else { continue };
            match g.by_path.get(rel) {
                None => missing.push(rel.clone()),
                Some(j) if j.contains_key("error") => {}
                Some(j) => {
                    let mut o = Map::new();
                    o.insert("id".into(), (*id).into());
                    for k in FIELDS {
                        if let Some(v) = j.get(*k).filter(|v| !v.is_null()) {
                            o.insert((*k).into(), v.clone());
                        }
                    }
                    out.push(Value::Object(o));
                }
            }
        }
        (out, missing)
    };
    want(&dir, &missing);
    ok(json!({ "mixInfo": { "song": out } }))
}
