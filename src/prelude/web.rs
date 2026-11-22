//! The web side of Prelude: /setup makes the first admin from a browser. It's open only while there's no admin,
//! and then only from this computer or with the one-time token Conductor prints when it starts (like Jupyter's), so
//! nobody else on the network can claim the server first. Once an admin exists it's closed for good. Everything
//! else is set up with `conductor setup` on the server.

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use crate::api::Shared;

const PAGE: &str = include_str!("../../web/setup/index.html");

/// Open: there's no admin yet, and the request is from this computer or has the token.
pub fn allowed(no_users: bool, local: bool, given: Option<&str>, token: &str) -> bool {
    no_users && (local || given.is_some_and(|g| !g.is_empty() && crate::auth::constant_eq(g, token)))
}

/// A name for this computer itself ("localhost", 127.0.0.1, [::1]), with or without a port.
pub fn loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => match host.rsplit_once(':') {
            Some((h, port)) if !h.contains(':') && port.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => host,
        },
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Really from this computer: the connection is, it wasn't passed on by a proxy (a reverse proxy on this computer
/// connects from 127.0.0.1 for everyone it serves), and it was addressed to this computer by name (a web page whose
/// own name was pointed at 127.0.0.1, DNS rebinding, is reached from the browser here but keeps its own Host).
fn local(req: &Request) -> bool {
    let peer = req.extensions().get::<ConnectInfo<std::net::SocketAddr>>().is_some_and(|c| c.0.ip().is_loopback());
    let h = req.headers();
    let proxied = ["forwarded", "x-forwarded-for", "x-forwarded-host", "x-real-ip"].iter().any(|k| h.contains_key(*k));
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).map(String::from).or_else(|| req.uri().authority().map(|a| a.to_string()));
    peer && !proxied && host.as_deref().is_some_and(loopback_host)
}

fn token_of(req: &Request) -> Option<String> {
    req.uri().query().and_then(|q| form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "token").map(|(_, v)| v.into_owned()))
}

async fn no_users(st: &Shared) -> bool {
    crate::auth::admin_count(&st.db).await.map(|n| n == 0).unwrap_or(false)
}

fn closed() -> Response {
    (StatusCode::NOT_FOUND, "Conductor is set up: sign in at /admin. (Or run `conductor setup` on the server.)").into_response()
}

async fn page(State(st): State<Shared>, req: Request) -> Response {
    if !allowed(no_users(&st).await, local(&req), token_of(&req).as_deref(), &st.setup_token) {
        return closed();
    }
    Html(PAGE).into_response()
}

async fn status(State(st): State<Shared>, req: Request) -> Response {
    if !allowed(no_users(&st).await, local(&req), token_of(&req).as_deref(), &st.setup_token) {
        return closed();
    }
    let libs: Vec<Value> = st
        .libs
        .iter()
        .map(|(_, l)| json!({ "name": l.name, "path": l.path, "connected": crate::library::is_online(l) }))
        .collect();
    Json(json!({ "libraries": libs, "config": st.cfg.config_path })).into_response()
}

async fn admin(State(st): State<Shared>, req: Request) -> Response {
    let ok = allowed(no_users(&st).await, local(&req), token_of(&req).as_deref(), &st.setup_token);
    let header = req.headers().get("x-conductor").and_then(|v| v.to_str().ok()) == Some("1");
    if !ok {
        return closed();
    }
    if !header {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "missing X-Conductor header" }))).into_response();
    }
    let body: Value = match to_bytes(req.into_body(), 16 << 10).await.ok().and_then(|b| serde_json::from_slice(&b).ok()) {
        Some(v) => v,
        None => return (StatusCode::BAD_REQUEST, Json(json!({ "error": "send {name, password}" }))).into_response(),
    };
    let (name, pw) = (body["name"].as_str().unwrap_or("").trim(), body["password"].as_str().unwrap_or(""));
    if name.is_empty() || pw.chars().count() < 6 {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "a name, and a password of 6 or more characters" }))).into_response();
    }
    // only while there's still nobody: of two sent at once, one is made
    match crate::auth::create_first_admin(&st.db, &st.secrets, name, pw).await {
        Ok(Some(_)) => {
            tracing::info!(user = name, "first admin made at /setup; /setup is closed now");
            Json(json!({ "ok": true })).into_response()
        }
        Ok(None) => closed(),
        Err(e) => (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("{e:#}") }))).into_response(),
    }
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/setup", get(page)).route("/setup/api/status", get(status)).route("/setup/api/admin", post(admin))
}

#[cfg(test)]
mod tests {
    #[test]
    fn who_may_set_up() {
        use super::allowed;
        assert!(allowed(true, true, None, "t"));
        assert!(!allowed(true, false, None, "t"), "from elsewhere: only with the token");
        assert!(allowed(true, false, Some("t"), "t"));
        assert!(!allowed(true, false, Some("x"), "t"));
        assert!(!allowed(true, false, Some(""), ""));
        assert!(!allowed(false, true, Some("t"), "t"), "closed once someone has an account");
    }

    #[test]
    fn this_computers_names() {
        use super::loopback_host;
        for h in ["localhost", "LOCALHOST:4533", "127.0.0.1", "127.0.0.1:4533", "127.9.9.9:80", "[::1]", "[::1]:4533", "::1"] {
            assert!(loopback_host(h), "{h}");
        }
        for h in ["", "evil.example", "evil.example:4533", "localhost.evil.example", "192.168.1.5:4533", "[::2]:4533", "127.0.0.1.evil.example"] {
            assert!(!loopback_host(h), "{h}");
        }
    }
}
