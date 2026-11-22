//! Scrobbling for every user: each one can link their own ListenBrainz (their user token) and Last.fm (approved on
//! Last.fm's own page; the server needs `lastfm_api_key` and `lastfm_secret`). Plays and "playing now" go to each
//! service the user linked; plays a service couldn't take are kept and sent again later, in order.
//!
//! The ListenBrainz token in the settings (`listenbrainz_token`) stays the owner's (`listenbrainz_for`, else the
//! first admin), used for them until they link their own. Tokens and session keys are kept encrypted with
//! secret.key, as passwords are, and never logged.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::{api::AppState, db::now};

pub const LISTENBRAINZ: &str = "listenbrainz";
pub const LASTFM: &str = "lastfm";
/// Where people approve Conductor on Last.fm.
pub const LASTFM_AUTH: &str = "https://www.last.fm/api/auth/";

fn http() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_secs(30)).build().expect("http client")
}

pub struct Account {
    pub secret: String,
    pub name: String,
    pub broken: bool,
}

pub async fn account(st: &AppState, user_id: &str, service: &str) -> Option<Account> {
    let row: Option<(String, String, bool)> =
        sqlx::query_as("SELECT secret, name, broken FROM scrobble_account WHERE user_id = ? AND service = ?")
            .bind(user_id)
            .bind(service)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten();
    let (enc, name, broken) = row?;
    match st.secrets.decrypt(&enc) {
        Ok(secret) => Some(Account { secret, name, broken }),
        Err(_) => {
            warn!(service, "a scrobbling account can't be read with this secret.key; link it again");
            None
        }
    }
}

pub async fn link(st: &AppState, user_id: &str, service: &str, secret: &str, name: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO scrobble_account (user_id, service, secret, name, broken, added_at) VALUES (?, ?, ?, ?, 0, ?)
         ON CONFLICT (user_id, service) DO UPDATE SET secret = excluded.secret, name = excluded.name, broken = 0,
         added_at = excluded.added_at",
    )
    .bind(user_id)
    .bind(service)
    .bind(st.secrets.encrypt(secret))
    .bind(name)
    .bind(now())
    .execute(&st.db)
    .await?;
    Ok(())
}

/// Takes the account away, and the plays still waiting to go to it.
pub async fn unlink(st: &AppState, user_id: &str, service: &str) -> Result<()> {
    sqlx::query("DELETE FROM scrobble_account WHERE user_id = ? AND service = ?").bind(user_id).bind(service).execute(&st.db).await?;
    sqlx::query("DELETE FROM lb_queue WHERE user_id = ? AND service = ?").bind(user_id).bind(service).execute(&st.db).await?;
    Ok(())
}

async fn mark_broken(st: &AppState, user_id: &str, service: &str) {
    let _ = sqlx::query("UPDATE scrobble_account SET broken = 1 WHERE user_id = ? AND service = ?")
        .bind(user_id)
        .bind(service)
        .execute(&st.db)
        .await;
}

// ── ListenBrainz ──

/// The token this user's plays go to ListenBrainz with: their own, else the settings' one if they're its owner.
/// `None` for the user stands for the settings' token itself (plays queued before users had their own).
async fn lb_token(st: &AppState, user_id: Option<&str>) -> Option<String> {
    let Some(uid) = user_id else { return st.cfg.listenbrainz_token.clone() };
    if let Some(a) = account(st, uid, LISTENBRAINZ).await {
        return (!a.broken).then_some(a.secret);
    }
    if st.cfg.listenbrainz_token.is_some() && crate::listenbrainz::owner(st).await.as_deref() == Some(uid) {
        return st.cfg.listenbrainz_token.clone();
    }
    None
}

/// Whether ListenBrainz takes this token, and whose it is.
pub async fn lb_validate(st: &AppState, token: &str) -> Result<Option<String>> {
    let r = http()
        .get(format!("{}/1/validate-token", st.cfg.listenbrainz_url.trim_end_matches('/')))
        .header("Authorization", format!("Token {token}"))
        .send()
        .await?;
    if r.status() == reqwest::StatusCode::UNAUTHORIZED || r.status() == reqwest::StatusCode::BAD_REQUEST {
        return Ok(None);
    }
    if !r.status().is_success() {
        bail!("ListenBrainz answered {}", r.status());
    }
    let v: Value = r.json().await?;
    Ok(v["valid"].as_bool().unwrap_or(false).then(|| v["user_name"].as_str().unwrap_or("").to_string()))
}

enum Sent {
    Ok,
    /// Try again later (the service is down, or busy).
    Later(String),
    /// It will never take it (a token or session that's no longer good, a play it refuses).
    Never(String),
}

/// A play (or "playing now") for ListenBrainz. `user_id` is whose plays they are: a token of their own that
/// ListenBrainz no longer takes is marked as needing a new link, as a Last.fm one is (the settings' token isn't).
async fn lb_submit(st: &AppState, user_id: Option<&str>, token: &str, body: &Value) -> Sent {
    let sent = lb_post(st, token, body).await;
    if let (Sent::Never(m), Some(uid)) = (&sent, user_id)
        && m == LB_REFUSED_TOKEN
        && account(st, uid, LISTENBRAINZ).await.is_some_and(|a| a.secret == token)
    {
        mark_broken(st, uid, LISTENBRAINZ).await;
    }
    sent
}

const LB_REFUSED_TOKEN: &str = "the token isn't accepted";

async fn lb_post(st: &AppState, token: &str, body: &Value) -> Sent {
    let r = http()
        .post(format!("{}/1/submit-listens", st.cfg.listenbrainz_url.trim_end_matches('/')))
        .header("Authorization", format!("Token {token}"))
        .json(body)
        .send()
        .await;
    match r {
        Err(e) => Sent::Later(e.to_string()),
        Ok(r) if r.status().is_success() => Sent::Ok,
        Ok(r) if r.status() == reqwest::StatusCode::UNAUTHORIZED => Sent::Never(LB_REFUSED_TOKEN.into()),
        Ok(r) if r.status() == reqwest::StatusCode::BAD_REQUEST => Sent::Never(format!("ListenBrainz refused it ({})", r.status())),
        Ok(r) => Sent::Later(format!("ListenBrainz answered {}", r.status())),
    }
}

// ── Last.fm ──

/// Last.fm's signature: every parameter (but `format` and `callback`) sorted by name, name and value run together,
/// the shared secret after them, MD5 of that.
pub fn lastfm_sign(params: &[(String, String)], secret: &str) -> String {
    let mut p: Vec<&(String, String)> = params.iter().filter(|(k, _)| k != "format" && k != "callback").collect();
    p.sort_by(|a, b| a.0.cmp(&b.0));
    let mut s = String::new();
    for (k, v) in p {
        s.push_str(k);
        s.push_str(v);
    }
    s.push_str(secret);
    format!("{:x}", md5::compute(s.as_bytes()))
}

/// Whether this server can link Last.fm accounts (its API key and shared secret are set).
pub fn lastfm_available(st: &AppState) -> bool {
    st.cfg.lastfm_api_key.is_some() && st.cfg.lastfm_secret.is_some()
}

/// A signed Last.fm call. Errors carry Last.fm's error number where it gave one.
async fn lastfm_call(st: &AppState, method: &str, mut params: Vec<(String, String)>) -> Result<Value, (Option<i64>, String)> {
    let (Some(key), Some(secret)) = (&st.cfg.lastfm_api_key, &st.cfg.lastfm_secret) else {
        return Err((None, "Last.fm isn't set up on this server".into()));
    };
    params.push(("method".into(), method.into()));
    params.push(("api_key".into(), key.clone()));
    let sig = lastfm_sign(&params, secret);
    params.push(("api_sig".into(), sig));
    params.push(("format".into(), "json".into()));
    let url = format!("{}/", st.cfg.lastfm_url.trim_end_matches('/'));
    let r = http().post(url).form(&params).send().await.map_err(|e| (None, e.to_string()))?;
    let status = r.status();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if let Some(code) = v["error"].as_i64() {
        return Err((Some(code), v["message"].as_str().unwrap_or("Last.fm refused it").to_string()));
    }
    if !status.is_success() {
        return Err((None, format!("Last.fm answered {status}")));
    }
    Ok(v)
}

/// Last.fm's errors that mean "later": its service is down or busy.
fn lastfm_later(code: Option<i64>) -> bool {
    matches!(code, None | Some(8 | 11 | 16 | 29))
}

/// Starting a link: Last.fm hands out a token, which the user approves on its page.
pub async fn lastfm_start(st: &AppState) -> Result<String> {
    let v = lastfm_call(st, "auth.getToken", vec![]).await.map_err(|(_, m)| anyhow!(m))?;
    v["token"].as_str().map(str::to_string).ok_or_else(|| anyhow!("Last.fm gave no token"))
}

pub enum Finish {
    Linked(String),
    Waiting,
    Expired,
}

/// Finishing a link: once the token is approved, it's swapped for a session key that doesn't run out.
pub async fn lastfm_finish(st: &AppState, user_id: &str, token: &str) -> Result<Finish> {
    match lastfm_call(st, "auth.getSession", vec![("token".into(), token.into())]).await {
        Ok(v) => {
            let (Some(key), Some(name)) = (v["session"]["key"].as_str(), v["session"]["name"].as_str()) else {
                bail!("Last.fm gave no session");
            };
            link(st, user_id, LASTFM, key, name).await?;
            info!(service = LASTFM, account = name, "scrobbling linked");
            Ok(Finish::Linked(name.to_string()))
        }
        Err((Some(14), _)) => Ok(Finish::Waiting), // not approved yet
        Err((Some(4 | 15), _)) => Ok(Finish::Expired),
        Err((_, m)) => bail!(m),
    }
}

async fn lastfm_send(st: &AppState, user_id: &str, payload: &Value) -> Sent {
    let Some(acc) = account(st, user_id, LASTFM).await else { return Sent::Never("not linked".into()) };
    if acc.broken {
        return Sent::Never("the link needs renewing".into());
    }
    let method = payload["method"].as_str().unwrap_or("track.scrobble").to_string();
    let mut params: Vec<(String, String)> = payload["params"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
        .collect();
    params.push(("sk".into(), acc.secret));
    match lastfm_call(st, &method, params).await {
        Ok(_) => Sent::Ok,
        Err((Some(9), m)) => {
            mark_broken(st, user_id, LASTFM).await;
            Sent::Never(m)
        }
        Err((code, m)) if lastfm_later(code) => Sent::Later(m),
        Err((_, m)) => Sent::Never(m),
    }
}

// ── plays ──

struct Track {
    title: String,
    artist: String,
    album: String,
    ms: i64,
    mbid: Option<String>,
}

async fn track(st: &AppState, track_id: &str) -> Option<Track> {
    type Row = (String, String, String, i64, Option<String>);
    let (title, artist, album, ms, mbid): Row =
        sqlx::query_as("SELECT title, artist_name, album_name, duration_ms, mbid FROM track WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&st.db)
            .await
            .ok()
            .flatten()?;
    Some(Track { title, artist, album, ms, mbid })
}

fn lb_body(t: &Track, at: i64, playing_now: bool) -> Value {
    let mut info =
        json!({ "duration_ms": t.ms, "submission_client": "Conductor", "submission_client_version": env!("CARGO_PKG_VERSION") });
    if let Some(m) = &t.mbid {
        info["recording_mbid"] = m.clone().into();
    }
    let meta = json!({ "artist_name": t.artist, "track_name": t.title, "release_name": t.album, "additional_info": info });
    if playing_now {
        json!({ "listen_type": "playing_now", "payload": [{ "track_metadata": meta }] })
    } else {
        json!({ "listen_type": "single", "payload": [{ "listened_at": at, "track_metadata": meta }] })
    }
}

fn lastfm_payload(t: &Track, at: i64, playing_now: bool) -> Value {
    let mut p = json!({ "artist": t.artist, "track": t.title });
    if !t.album.is_empty() {
        p["album"] = t.album.clone().into();
    }
    if t.ms > 0 {
        p["duration"] = (t.ms / 1000).to_string().into();
    }
    if let Some(m) = &t.mbid {
        p["mbid"] = m.clone().into();
    }
    if playing_now {
        json!({ "method": "track.updateNowPlaying", "params": p })
    } else {
        p["timestamp"] = at.to_string().into();
        json!({ "method": "track.scrobble", "params": p })
    }
}

async fn keep(st: &AppState, user_id: Option<&str>, service: &str, payload: &Value) {
    let _ = sqlx::query("INSERT INTO lb_queue (payload, added_at, user_id, service) VALUES (?, ?, ?, ?)")
        .bind(payload.to_string())
        .bind(now())
        .bind(user_id)
        .bind(service)
        .execute(&st.db)
        .await;
}

async fn waiting(st: &AppState, user_id: &str, service: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM lb_queue WHERE user_id IS ? AND service = ?")
        .bind(user_id)
        .bind(service)
        .fetch_one(&st.db)
        .await
        .unwrap_or(0)
        > 0
}

/// A play, or "playing now", for every service this user linked. A play a service can't take now is kept and sent
/// later (behind the ones already waiting, so they arrive in order); "playing now" is only worth sending now.
pub async fn scrobble(st: &AppState, user_id: &str, track_id: &str, at: i64, playing_now: bool) {
    let lb = lb_token(st, Some(user_id)).await;
    let lfm = match account(st, user_id, LASTFM).await {
        Some(a) if !a.broken && lastfm_available(st) => Some(()),
        _ => None,
    };
    if lb.is_none() && lfm.is_none() {
        return;
    }
    let Some(t) = track(st, track_id).await else { return };
    if let Some(token) = lb {
        let body = lb_body(&t, at, playing_now);
        if playing_now {
            let _ = lb_submit(st, Some(user_id), &token, &body).await;
        } else if waiting(st, user_id, LISTENBRAINZ).await {
            keep(st, Some(user_id), LISTENBRAINZ, &body).await;
        } else {
            match lb_submit(st, Some(user_id), &token, &body).await {
                Sent::Ok => {}
                Sent::Later(e) => {
                    warn!("ListenBrainz: {e}; the play is kept and sent later");
                    keep(st, Some(user_id), LISTENBRAINZ, &body).await;
                }
                Sent::Never(e) => warn!("ListenBrainz didn't take a play: {e}"),
            }
        }
    }
    if lfm.is_some() {
        let payload = lastfm_payload(&t, at, playing_now);
        if playing_now {
            let _ = lastfm_send(st, user_id, &payload).await;
        } else if waiting(st, user_id, LASTFM).await {
            keep(st, Some(user_id), LASTFM, &payload).await;
        } else {
            match lastfm_send(st, user_id, &payload).await {
                Sent::Ok => {}
                Sent::Later(e) => {
                    warn!("Last.fm: {e}; the play is kept and sent later");
                    keep(st, Some(user_id), LASTFM, &payload).await;
                }
                Sent::Never(e) => warn!("Last.fm didn't take a play: {e}"),
            }
        }
    }
}

/// Send the plays that are waiting, oldest first. A user and service still unreachable is left for next time
/// (and their later plays with it); plays for an account that's gone are dropped.
pub async fn resend(st: &AppState) {
    type Row = (i64, String, Option<String>, String);
    let mut stuck: Vec<(Option<String>, String)> = Vec::new();
    // in pages, all the way through: one user's long wait (their token is being turned away for now) mustn't keep
    // everyone else's plays behind it
    let mut after = 0i64;
    loop {
        let rows: Vec<Row> = sqlx::query_as("SELECT id, payload, user_id, service FROM lb_queue WHERE id > ? ORDER BY id LIMIT 500")
            .bind(after)
            .fetch_all(&st.db)
            .await
            .unwrap_or_default();
        let Some(last) = rows.last() else { break };
        after = last.0;
        for (id, p, uid, service) in rows {
            let who = (uid.clone(), service.clone());
            if stuck.contains(&who) {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(&p) else {
                let _ = sqlx::query("DELETE FROM lb_queue WHERE id = ?").bind(id).execute(&st.db).await;
                continue;
            };
            let sent = match service.as_str() {
                LASTFM => match &uid {
                    Some(u) => lastfm_send(st, u, &v).await,
                    None => Sent::Never("no user".into()),
                },
                _ => match lb_token(st, uid.as_deref()).await {
                    Some(token) => lb_submit(st, uid.as_deref(), &token, &v).await,
                    None => Sent::Never("not linked any more".into()),
                },
            };
            match sent {
                Sent::Ok | Sent::Never(_) => {
                    let _ = sqlx::query("DELETE FROM lb_queue WHERE id = ?").bind(id).execute(&st.db).await;
                }
                Sent::Later(_) => {
                    let _ = sqlx::query("UPDATE lb_queue SET tries = tries + 1 WHERE id = ?").bind(id).execute(&st.db).await;
                    stuck.push(who);
                }
            }
        }
    }
}

/// Every 15 minutes: the plays that are waiting.
pub async fn worker(st: Arc<AppState>) {
    loop {
        tokio::time::sleep(Duration::from_secs(15 * 60)).await;
        resend(&st).await;
    }
}

/// Last.fm tokens handed out to start a link, so only the user who started one can finish it (a token approved
/// by someone else would link their Last.fm). They last an hour, as Last.fm's do.
#[derive(Default)]
pub struct Pending(Mutex<HashMap<String, (String, i64)>>);

impl Pending {
    pub fn add(&self, token: &str, user_id: &str) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let t = now();
        m.retain(|_, (_, at)| t - *at < 3600);
        // a user starting again replaces their earlier one
        m.retain(|_, (u, _)| u != user_id);
        m.insert(token.to_string(), (user_id.to_string(), t));
    }

    pub fn is_theirs(&self, token: &str, user_id: &str) -> bool {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.get(token).is_some_and(|(u, at)| u == user_id && now() - at < 3600)
    }

    pub fn done(&self, token: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lastfm_signature() {
        // as Last.fm's documentation builds it: sorted names, no format, the secret last
        let p = vec![
            ("method".to_string(), "auth.getSession".to_string()),
            ("api_key".to_string(), "k".to_string()),
            ("token".to_string(), "t".to_string()),
            ("format".to_string(), "json".to_string()),
        ];
        assert_eq!(lastfm_sign(&p, "s"), format!("{:x}", md5::compute("api_keykmethodauth.getSessiontokents")));
    }
}
