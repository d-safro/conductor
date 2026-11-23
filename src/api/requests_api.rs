//! `encore.requests`: the songs asked for (and Lidarr's albums) and how getting each is going, as Podium shows them,
//! and what an admin does with them: retry, cancel, set aside / bring back, search Soulseek and pick a file. The
//! work is `crate::catalog::requests`, shared with Podium. An admin sees everything; anyone else what they asked for.

use serde_json::json;

use super::{ApiError, Ctx, Reply, empty, ok};
use crate::catalog::requests::{self, Filter, Refusal, STATES};

fn admins_only(c: &Ctx) -> Result<(), ApiError> {
    if c.user.is_admin { Ok(()) } else { Err(ApiError::new(50, "Only admins can change what's asked for")) }
}

fn refused(r: Refusal) -> ApiError {
    match r {
        Refusal::NotFound(_) => ApiError::not_found("Request"),
        other => ApiError::new(0, other.message()),
    }
}

/// The states asked for: `state` once per state, or several in one, comma-separated.
fn states(given: Vec<&str>) -> Result<Vec<&'static str>, ApiError> {
    let mut out = vec![];
    for s in given.into_iter().flat_map(|v| v.split(',')).map(str::trim).filter(|s| !s.is_empty()) {
        match STATES.iter().find(|x| x.eq_ignore_ascii_case(s)) {
            Some(x) => out.push(*x),
            None => return Err(ApiError::new(0, format!("Unknown state: {s} (one of {})", STATES.join(", ")))),
        }
    }
    Ok(out)
}

/// The list, a page at a time.
pub async fn get_requests(c: &Ctx) -> Result<Reply, ApiError> {
    let states = states(c.p.all("state"))?;
    let kind = match c.p.get("type").filter(|t| !t.is_empty()) {
        None => None,
        Some(t @ ("song" | "album")) => Some(t),
        Some(t) => return Err(ApiError::new(0, format!("Unknown type: {t} (song or album)"))),
    };
    let user = match c.p.get("username").filter(|u| !u.is_empty()) {
        None => None,
        Some(u) if u == c.user.name => Some(c.user.id.clone()),
        Some(_) if !c.user.is_admin => return Err(ApiError::new(50, "Only admins can see what others asked for")),
        Some(u) => Some(
            sqlx::query_scalar::<_, String>("SELECT id FROM user WHERE name = ?")
                .bind(u)
                .fetch_optional(&c.st.db)
                .await?
                .ok_or_else(|| ApiError::not_found("User"))?,
        ),
    };
    let offset = c.p.int("offset").unwrap_or(0).max(0) as usize;
    let count = c.p.int("count").unwrap_or(50).clamp(0, 500) as usize;
    let f = Filter { states, kind, user: user.as_deref(), id: None };
    ok(json!({ "requests": requests::list(&c.st, &c.user, &f, offset, count).await? }))
}

/// One request as the list has it (after an action).
async fn one(c: &Ctx, id: &str) -> Result<Reply, ApiError> {
    let f = Filter { id: Some(id), ..Default::default() };
    let v = requests::list(&c.st, &c.user, &f, 0, 1).await?;
    match v["request"].get(0) {
        Some(r) => ok(json!({ "request": r })),
        None => Err(ApiError::not_found("Request")),
    }
}

async fn known(c: &Ctx, id: &str) -> Result<(), ApiError> {
    let n: i64 = if let Some(a) = id.strip_prefix("lidarr-") {
        sqlx::query_scalar("SELECT COUNT(*) FROM lidarr_grab WHERE album_id = ?").bind(a.parse::<i64>().unwrap_or(-1)).fetch_one(&c.st.db).await?
    } else {
        sqlx::query_scalar("SELECT COUNT(*) FROM request WHERE key = ?").bind(id).fetch_one(&c.st.db).await?
    };
    if n > 0 { Ok(()) } else { Err(ApiError::not_found("Request")) }
}

/// Look again from the start (a song: any file, any peer; a Lidarr album: any folder).
pub async fn retry_request(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    let id = c.p.req("id")?;
    known(c, id).await?;
    let done = match id.strip_prefix("lidarr-") {
        Some(a) => requests::retry_album(&c.st, a.parse().unwrap_or(-1)).await,
        None => requests::retry(&c.st, id).await,
    };
    if !done {
        return Err(ApiError::new(0, "It's already here"));
    }
    one(c, id).await
}

/// Stop asking for a song that isn't here yet.
pub async fn cancel_request(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    let id = c.p.req("id")?;
    if id.starts_with("lidarr-") {
        return Err(ApiError::new(0, "Lidarr's albums are cancelled in Lidarr"));
    }
    known(c, id).await?;
    if !requests::cancel(&c.st, id).await {
        return Err(ApiError::new(0, "It's already here"));
    }
    empty()
}

/// Set a song aside (no more automatic searches; `aside` = true, the default) or bring it back (false).
pub async fn set_request_aside(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    let id = c.p.req("id")?;
    let on = c.p.bool("aside").unwrap_or(true);
    known(c, id).await?;
    if !requests::set_aside(&c.st, id, on).await {
        let why = if on { "Only a song being looked for or not found can be set aside" } else { "It isn't set aside" };
        return Err(ApiError::new(0, why));
    }
    one(c, id).await
}

/// Search Soulseek for a song in your own words (`query`, at least 3 letters; by default the song's own `query`).
pub async fn search_request_files(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    let id = c.p.req("id")?;
    let q = match c.p.get("query").filter(|q| !q.trim().is_empty()) {
        Some(q) => q.to_string(),
        None => {
            let v = requests::list(&c.st, &c.user, &Filter { id: Some(id), ..Default::default() }, 0, 1).await?;
            v["request"][0]["query"].as_str().ok_or_else(|| ApiError::not_found("Request"))?.to_string()
        }
    };
    let v = requests::search_files(&c.st, id, &q).await.map_err(refused)?;
    ok(json!({ "requestFiles": { "total": v["total"], "query": q, "file": v["results"] } }))
}

/// Download one of the files a search found (`user`, `file` and `size` as it gave them) for a song waiting for one.
pub async fn pick_request_file(c: &Ctx) -> Result<Reply, ApiError> {
    admins_only(c)?;
    let id = c.p.req("id")?;
    let (user, file) = (c.p.req("user")?, c.p.req("file")?);
    let size = c.p.int("size").unwrap_or(0);
    requests::pick_file(&c.st, id, user, file, size).await.map_err(refused)?;
    one(c, id).await
}
