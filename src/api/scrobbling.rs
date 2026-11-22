//! `encore.scrobbling`: each user links their own ListenBrainz and Last.fm (see `crate::scrobble`).
//!
//! - `getScrobbling` → `{scrobbling: {listenbrainz: {linked, name, fromSettings, broken}, lastfm: {available, linked,
//!   name, broken}}}`
//! - `setListenBrainz?token=` checks the token with ListenBrainz and links it; an empty token unlinks.
//! - `lastfmLinkStart` → `{lastfmLink: {url, token}}`: open `url`, approve Conductor there, then
//! - `lastfmLinkFinish?token=` → `{lastfmLink: {status: waiting | linked | expired, name}}` (ask again while waiting)
//! - `lastfmUnlink`

use serde_json::json;

use super::{ApiError, Ctx, Reply, ok};
use crate::scrobble::{self, Finish, LASTFM, LISTENBRAINZ};

async fn state(c: &Ctx) -> serde_json::Value {
    let st = &c.st;
    let uid = c.user.id.as_str();
    let lb = scrobble::account(st, uid, LISTENBRAINZ).await;
    let settings = lb.is_none()
        && st.cfg.listenbrainz_token.is_some()
        && crate::listenbrainz::owner(st).await.as_deref() == Some(uid);
    let lfm = scrobble::account(st, uid, LASTFM).await;
    json!({
        "listenbrainz": {
            "linked": lb.is_some() || settings,
            "name": lb.as_ref().map(|a| a.name.clone()).or_else(|| settings.then(|| st.cfg.listenbrainz_user.clone().unwrap_or_default())).unwrap_or_default(),
            "fromSettings": settings,
            "broken": lb.as_ref().is_some_and(|a| a.broken),
        },
        "lastfm": {
            "available": scrobble::lastfm_available(st),
            "linked": lfm.is_some(),
            "name": lfm.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            "broken": lfm.as_ref().is_some_and(|a| a.broken),
        },
    })
}

pub async fn get(c: &Ctx) -> Result<Reply, ApiError> {
    ok(json!({ "scrobbling": state(c).await }))
}

pub async fn set_listenbrainz(c: &Ctx) -> Result<Reply, ApiError> {
    let token = c.p.get("token").unwrap_or("").trim();
    if token.is_empty() {
        scrobble::unlink(&c.st, &c.user.id, LISTENBRAINZ).await?;
        tracing::info!(user = %c.user.name, service = LISTENBRAINZ, "scrobbling unlinked");
        return get(c).await;
    }
    if token.len() > 200 {
        return Err(ApiError::new(10, "That isn't a ListenBrainz token"));
    }
    let name = scrobble::lb_validate(&c.st, token).await.map_err(|_| ApiError::new(0, "ListenBrainz can't be reached; try again later"))?;
    let Some(name) = name else { return Err(ApiError::new(0, "ListenBrainz doesn't accept this token")) };
    scrobble::link(&c.st, &c.user.id, LISTENBRAINZ, token, &name).await?;
    tracing::info!(user = %c.user.name, service = LISTENBRAINZ, account = %name, "scrobbling linked");
    get(c).await
}

pub async fn lastfm_start(c: &Ctx) -> Result<Reply, ApiError> {
    if !scrobble::lastfm_available(&c.st) {
        return Err(ApiError::new(0, "Last.fm isn't set up on this server (lastfm_api_key and lastfm_secret)"));
    }
    let token = scrobble::lastfm_start(&c.st).await.map_err(|e| ApiError::new(0, format!("Last.fm: {e}")))?;
    c.st.lastfm_pending.add(&token, &c.user.id);
    let key = c.st.cfg.lastfm_api_key.as_deref().unwrap_or("");
    let url = format!(
        "{}?{}",
        scrobble::LASTFM_AUTH,
        form_urlencoded::Serializer::new(String::new()).append_pair("api_key", key).append_pair("token", &token).finish()
    );
    ok(json!({ "lastfmLink": { "url": url, "token": token } }))
}

pub async fn lastfm_finish(c: &Ctx) -> Result<Reply, ApiError> {
    let token = c.p.req("token")?;
    if !c.st.lastfm_pending.is_theirs(token, &c.user.id) {
        return ok(json!({ "lastfmLink": { "status": "expired" } }));
    }
    let r = scrobble::lastfm_finish(&c.st, &c.user.id, token).await.map_err(|e| ApiError::new(0, format!("Last.fm: {e}")))?;
    let (status, name) = match r {
        Finish::Linked(name) => {
            c.st.lastfm_pending.done(token);
            ("linked", name)
        }
        Finish::Waiting => ("waiting", String::new()),
        Finish::Expired => {
            c.st.lastfm_pending.done(token);
            ("expired", String::new())
        }
    };
    ok(json!({ "lastfmLink": { "status": status, "name": name } }))
}

pub async fn lastfm_unlink(c: &Ctx) -> Result<Reply, ApiError> {
    scrobble::unlink(&c.st, &c.user.id, LASTFM).await?;
    tracing::info!(user = %c.user.name, service = LASTFM, "scrobbling unlinked");
    get(c).await
}
