//! Conductor's icon, built into the program: the favicon for Podium and the Ensemble (Jam) and Chorus guest pages.
//! The sources and the script that makes them are in assets/brand.

use axum::{Router, http::header, response::IntoResponse, routing::get};

const ICON_SVG: &[u8] = include_bytes!("../assets/brand/conductor-icon.svg");
const ICON_32: &[u8] = include_bytes!("../assets/brand/conductor-icon-32.png");
const ICON_192: &[u8] = include_bytes!("../assets/brand/conductor-icon-192.png");

fn file(ct: &'static str, b: &'static [u8]) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, "public, max-age=604800")], b)
}

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/favicon.svg", get(|| async { file("image/svg+xml", ICON_SVG) }))
        .route("/favicon-32.png", get(|| async { file("image/png", ICON_32) }))
        .route("/favicon.ico", get(|| async { file("image/png", ICON_32) }))
        .route("/icon-192.png", get(|| async { file("image/png", ICON_192) }))
}
