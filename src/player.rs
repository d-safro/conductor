//! Recital, the web player at `/` (also `/web`): one self-contained page (HTML, CSS and a little JS, no outside files) that
//! signs in with a Conductor user and plays music through the normal Subsonic API with token login.

use axum::{
    Router,
    http::header,
    response::{Html, IntoResponse},
    routing::get,
};

use crate::api::Shared;

const PAGE: &str = include_str!("../web/player/index.html");

/// Only this server: no outside scripts, styles, pictures or connections, and no framing by other sites.
const CSP: &str = "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self' data:; \
                   media-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

async fn page() -> impl IntoResponse {
    static C: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| crate::page::csp(PAGE, CSP));
    ([(header::CONTENT_SECURITY_POLICY, C.as_str()), (header::CACHE_CONTROL, "no-cache")], Html(PAGE))
}

pub fn routes() -> Router<Shared> {
    Router::new().route("/", get(page)).route("/web", get(page)).route("/web/", get(page))
}
