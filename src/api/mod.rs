//! The Subsonic / OpenSubsonic API at `/rest/<method>[.view]`, JSON or XML, GET or POST (formPost).

mod bridges;
mod browse;
mod catalog_api;
mod dj;
mod encore;
mod imports;
mod info;
mod items;
pub(crate) mod media;
mod native;
pub mod podcasts;
pub mod shares;
pub mod smart;
pub mod sonic;
mod translate;
mod userdata;
mod users;

use std::{collections::HashMap, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Path, Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::{
    auth::{self, Credentials, LoginError, Secrets, User},
    config::{Config, LibraryConfig},
    db::Db,
    library::ScanStatus,
};

pub const API_VERSION: &str = "1.16.1";
pub const SERVER_TYPE: &str = "conductor";

pub struct AppState {
    pub db: Db,
    pub secrets: Secrets,
    pub cfg: Config,
    pub libs: Vec<(i64, LibraryConfig)>,
    pub scan: Arc<ScanStatus>,
    /// user id → what each player reported last (scrobble submission=false, reportPlayback)
    pub now_playing: Mutex<HashMap<(String, String), NowPlaying>>,
    /// sign-in tokens for the Navidrome stand-in: token → (user id, expires)
    pub native_tokens: Mutex<HashMap<String, (String, i64)>>,
    /// devices, sign-in codes, Jams
    pub live: crate::live::Live,
    /// wakes the podcast worker (something to download, a new show)
    pub podcast_wake: tokio::sync::Notify,
    /// songs you don't have: Deezer, the library index for matching
    pub catalog: crate::catalog::Catalog,
    /// wakes the download worker (a song was asked for)
    pub acquire_wake: tokio::sync::Notify,
    /// one Soulseek search at a time
    pub search_pace: tokio::sync::Mutex<()>,
    /// the VPN downloads go through, as last checked (vpn.rs)
    pub vpn: std::sync::RwLock<crate::vpn::Status>,
    /// opens /setup from another computer while nobody has an account (printed at start)
    pub setup_token: String,
    /// wrong passwords, per user per address
    pub login_fails: crate::auth::Throttle,
}

impl AppState {
    pub fn new(db: Db, secrets: Secrets, cfg: Config, libs: Vec<(i64, LibraryConfig)>) -> Self {
        let catalog = crate::catalog::Catalog::new(&cfg.deezer_url, crate::net::downloads_proxy(&cfg).as_deref());
        let cfg_vpn = cfg.vpn.clone();
        Self {
            catalog,
            acquire_wake: Default::default(),
            search_pace: Default::default(),
            db,
            secrets,
            cfg,
            libs,
            scan: Arc::new(ScanStatus::default()),
            now_playing: Mutex::default(),
            native_tokens: Mutex::default(),
            live: Default::default(),
            podcast_wake: Default::default(),
            vpn: std::sync::RwLock::new(crate::vpn::Status { mode: cfg_vpn, ..Default::default() }),
            setup_token: crate::db::random_id(),
            login_fails: Default::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct NowPlaying {
    pub track_id: String,
    pub username: String,
    pub client: String,
    pub at: i64,
}

pub type Shared = Arc<AppState>;

pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/rest/audiomuse/{*endpoint}", any(sonic::passthrough))
        .route("/rest/{method}", any(handle))
        .route("/health", get(health))
        .merge(native::routes())
        .merge(crate::admin::routes())
        .merge(crate::brand::routes())
        .merge(crate::prelude::web::routes())
        .merge(crate::player::routes())
        .with_state(state)
}

/// A request's address as it may be logged: passwords, tokens and keys in the query (`p`, `t`, `s`, `apiKey`,
/// the /setup `token`) are left out.
pub fn loggable_uri(uri: &axum::http::Uri) -> String {
    const SECRET: &[&str] = &["p", "t", "s", "apikey", "token", "password"];
    let Some(q) = uri.query() else { return uri.path().to_string() };
    let kept: Vec<String> = form_urlencoded::parse(q.as_bytes())
        .map(|(k, v)| if SECRET.contains(&k.to_ascii_lowercase().as_str()) { format!("{k}=…") } else { format!("{k}={v}") })
        .collect();
    format!("{}?{}", uri.path(), kept.join("&"))
}

type MakeSpan = fn(&axum::http::Request<Body>) -> tracing::Span;
type Trace = tower_http::trace::TraceLayer<tower_http::classify::SharedClassifier<tower_http::classify::ServerErrorsAsFailures>, MakeSpan>;

/// Request logging (at debug level) without the secrets Subsonic apps put in the address.
pub fn trace_layer() -> Trace {
    fn span(r: &axum::http::Request<Body>) -> tracing::Span {
        tracing::debug_span!(target: "tower_http::trace::make_span", "request", method = %r.method(), uri = %loggable_uri(r.uri()), version = ?r.version())
    }
    tower_http::trace::TraceLayer::new_for_http().make_span_with(span as MakeSpan)
}

async fn health(State(st): State<Shared>) -> Response {
    let offline: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM library WHERE online = 0").fetch_one(&st.db).await.unwrap_or(1);
    let status = if offline == 0 { "ok" } else { "degraded" };
    let mut out = json!({ "status": status });
    if st.cfg.vpn != "none" || st.cfg.downloads_require_vpn {
        let v = crate::vpn::status(&st);
        out["vpn"] = json!({ "mode": v.mode, "up": v.up, "country": v.country, "downloadsPaused": crate::vpn::paused(&st) });
    }
    axum::Json(out).into_response()
}

/// Request parameters from the query string and a form body, keeping repeated keys (id=1&id=2).
#[derive(Debug, Default, Clone)]
pub struct Params(Vec<(String, String)>);

impl Params {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.0.iter().filter(|(a, _)| a == k).map(|(_, v)| v.as_str()).collect()
    }
    pub fn int(&self, k: &str) -> Option<i64> {
        self.get(k).and_then(|v| v.trim().parse().ok())
    }
    pub fn bool(&self, k: &str) -> Option<bool> {
        self.get(k).map(|v| v == "true" || v == "1")
    }
    pub fn req(&self, k: &str) -> Result<&str, ApiError> {
        self.get(k).filter(|v| !v.is_empty()).ok_or_else(|| ApiError::missing(k))
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Fmt {
    Json,
    Xml,
}

pub struct Ctx {
    pub st: Shared,
    pub user: User,
    pub p: Params,
    pub headers: HeaderMap,
    pub client: String,
    /// The raw request body, for the calls that take one that isn't a form (identifySong).
    pub body: axum::body::Bytes,
}

#[derive(Debug)]
pub struct ApiError {
    pub code: u32,
    pub message: String,
    pub http: StatusCode,
}

impl ApiError {
    pub fn new(code: u32, m: impl Into<String>) -> Self {
        Self { code, message: m.into(), http: StatusCode::OK }
    }
    pub fn missing(p: &str) -> Self {
        Self::new(10, format!("Required parameter is missing: {p}"))
    }
    pub fn not_found(what: &str) -> Self {
        Self::new(70, format!("{what} not found"))
    }
    pub fn offline() -> Self {
        Self { code: 0, message: "The music drive isn't connected right now".into(), http: StatusCode::SERVICE_UNAVAILABLE }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("database: {e}");
        Self::new(0, "Server error (database)")
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!("{e:#}");
        Self::new(0, "Server error")
    }
}

pub enum Reply {
    /// fields that go into "subsonic-response" next to status and version
    Ok(Value),
    /// a file, an image or a stream
    Raw(Response),
}

pub fn ok(v: Value) -> Result<Reply, ApiError> {
    Ok(Reply::Ok(v))
}
pub fn empty() -> Result<Reply, ApiError> {
    Ok(Reply::Ok(json!({})))
}

async fn handle(State(st): State<Shared>, Path(method): Path<String>, req: Request) -> Response {
    let method = method.strip_suffix(".view").unwrap_or(&method).to_string();
    let headers = req.headers().clone();
    let ip = req
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|ci| crate::jam::client_ip(&headers, ci.0))
        .unwrap_or_default();
    let mut pairs: Vec<(String, String)> =
        req.uri().query().map(|q| form_urlencoded::parse(q.as_bytes()).into_owned().collect()).unwrap_or_default();
    let is_form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/x-www-form-urlencoded"));
    let mut body = axum::body::Bytes::new();
    let is_post = req.method() == Method::POST;
    let mut upload: Option<Body> = None;
    if is_post && method == "importUpload" {
        // the body is the export, whatever it's labelled as; read only once the login is checked (it can be large)
        upload = Some(req.into_body());
    } else if is_post && is_form {
        if let Ok(b) = to_bytes(req.into_body(), 1 << 20).await {
            pairs.extend(form_urlencoded::parse(&b).into_owned());
        }
    } else if is_post && method == "identifySong" {
        // the body is a short recording, not a form (the login is in the query)
        body = to_bytes(req.into_body(), 16 << 20).await.unwrap_or_default();
    }
    let p = Params(pairs);
    let fmt = match p.get("f") {
        Some("json") => Fmt::Json,
        _ => Fmt::Xml,
    };
    if method == "pairStart" || method == "pairPoll" {
        let r = if method == "pairStart" {
            encore::pair_start(&st, p.get("name"), &ip).await
        } else {
            encore::pair_poll(&st, p.get("token").unwrap_or(""), p.int("wait").unwrap_or(20).clamp(0, 25) as u64).await
        };
        return render(fmt, r.map(|v| Reply::Ok(json!({ "pair": v }))));
    }
    let who = p.get("u").unwrap_or("").to_string();
    if st.login_fails.blocked("app", &ip, &who, auth::APP_TRIES) {
        return render(fmt, Err(ApiError::new(40, "Too many wrong passwords. Try again in a few minutes.")));
    }
    let creds = Credentials { u: p.get("u"), p: p.get("p"), t: p.get("t"), s: p.get("s"), api_key: p.get("apiKey") };
    let user = match auth::login(&st.db, &st.secrets, creds).await {
        Ok(u) => {
            st.login_fails.clear("app", &ip, &who);
            u
        }
        Err(e) => {
            if matches!(e, LoginError::Wrong | LoginError::InvalidKey) {
                st.login_fails.failed("app", &ip, &who);
            }
            let (code, msg) = match e {
                LoginError::Missing => (10, "Required parameter is missing: u, or apiKey"),
                LoginError::Wrong => (40, "Wrong username or password"),
                LoginError::BothMethods => (43, "Multiple conflicting authentication mechanisms provided"),
                LoginError::InvalidKey => (44, "Invalid API key"),
            };
            return render(fmt, Err(ApiError::new(code, msg)));
        }
    };
    let client = p.get("c").unwrap_or("").to_string();
    if let Some(b) = upload {
        let r = imports::upload(&st, &user, p.get("name").unwrap_or("export.zip"), b).await;
        return render(fmt, r);
    }
    let ctx = Ctx { st, user, p, headers, client, body };
    let r = dispatch(&ctx, &method).await;
    render(fmt, r)
}

async fn dispatch(c: &Ctx, method: &str) -> Result<Reply, ApiError> {
    if c.st.cfg.catalog
        && let Some(r) = catalog_api::intercept(c, method).await
    {
        return r;
    }
    let r = dispatch_plain(c, method).await;
    catalog_api::augment(c, method, r).await
}

async fn dispatch_plain(c: &Ctx, method: &str) -> Result<Reply, ApiError> {
    match method {
        "ping" => empty(),
        "getLicense" => ok(json!({ "license": { "valid": true, "email": "", "licenseExpires": "2100-01-01T00:00:00Z" } })),
        "getOpenSubsonicExtensions" => ok(json!({ "openSubsonicExtensions": [
            { "name": "formPost", "versions": [1] },
            { "name": "apiKeyAuthentication", "versions": [1] },
            { "name": "songLyrics", "versions": [1] },
            { "name": "transcodeOffset", "versions": [1] },
            { "name": "encore.devices", "versions": [1] },
            { "name": "encore.sync", "versions": [1] },
            { "name": "encore.pairing", "versions": [1] },
            { "name": "encore.stats", "versions": [1] },
            { "name": "encore.jam", "versions": [1] },
            { "name": "encore.smartPlaylists", "versions": [1] },
            { "name": "encore.playlistImages", "versions": [1] },
            { "name": "encore.sonic", "versions": [1] },
            { "name": "encore.review", "versions": [1] },
            { "name": "encore.collabPlaylists", "versions": [1] },
            { "name": "encore.lyricsTranslation", "versions": [1] },
            { "name": "encore.dj", "versions": [1] },
            { "name": "encore.import", "versions": [1] },
            { "name": "encore.songId", "versions": [1] },
            { "name": "encore.automix", "versions": [1] },
            { "name": "encore.concerts", "versions": [1] },
            { "name": "encore.mixes", "versions": [1] },
        ] })),
        "getMusicFolders" => browse::music_folders(c).await,
        "getArtists" => browse::artists(c, false).await,
        "getIndexes" => browse::artists(c, true).await,
        "getArtist" => browse::artist(c).await,
        "getAlbum" => browse::album(c).await,
        "getSong" => browse::song(c).await,
        "getMusicDirectory" => browse::directory(c).await,
        "getAlbumList2" => browse::album_list(c, true).await,
        "getAlbumList" => browse::album_list(c, false).await,
        "getRandomSongs" => browse::random_songs(c).await,
        "getSongsByGenre" => browse::songs_by_genre(c).await,
        "getGenres" => browse::genres(c).await,
        "search3" => browse::search(c, 3).await,
        "search2" => browse::search(c, 2).await,
        "getStarred2" => browse::starred(c, true).await,
        "getStarred" => browse::starred(c, false).await,
        "getTopSongs" => browse::top_songs(c).await,
        "getSimilarSongs2" => info::similar_songs(c, true).await,
        "getSimilarSongs" => info::similar_songs(c, false).await,
        "getArtistInfo2" => info::artist_info(c, true).await,
        "getArtistInfo" => info::artist_info(c, false).await,
        "getAlbumInfo2" | "getAlbumInfo" => info::album_info(c).await,
        "getDjLine" => dj::line(c).await,
        "importOnline" => imports::online(c).await,
        "getImports" => imports::list(c).await,
        "getImport" => imports::get(c).await,
        "startImport" => imports::start(c).await,
        "undoImport" => imports::undo(c).await,
        "deleteImport" => imports::delete(c).await,
        "djAudio" => dj::audio(c).await,
        "getScanStatus" => browse::scan_status(c).await,
        "startScan" => browse::start_scan(c).await,
        "stream" => media::stream(c, false).await,
        "download" => media::stream(c, true).await,
        "getCoverArt" => media::cover_art(c).await,
        "getLyrics" => media::lyrics(c).await,
        "getLyricsBySongId" => media::lyrics_by_song(c).await,
        "star" => userdata::star(c, true).await,
        "unstar" => userdata::star(c, false).await,
        "setRating" => userdata::set_rating(c).await,
        "scrobble" => userdata::scrobble(c).await,
        "reportPlayback" => userdata::report_playback(c).await,
        "getNowPlaying" => userdata::now_playing(c).await,
        "getPlaylists" => userdata::playlists(c).await,
        "getPlaylist" => userdata::playlist(c).await,
        "createPlaylist" => userdata::create_playlist(c).await,
        "updatePlaylist" => userdata::update_playlist(c).await,
        "deletePlaylist" => userdata::delete_playlist(c).await,
        "getPlayQueue" => userdata::get_play_queue(c).await,
        "savePlayQueue" => userdata::save_play_queue(c).await,
        "getUser" => users::get_user(c).await,
        "getUsers" => users::get_users(c).await,
        "createUser" => users::create_user(c).await,
        "updateUser" => users::update_user(c).await,
        "deleteUser" => users::delete_user(c).await,
        "changePassword" => users::change_password(c).await,
        "getAvatar" => users::get_avatar(c).await,
        "getShares" => shares::get_shares(c).await,
        "createShare" => shares::create(c).await,
        "updateShare" => shares::update(c).await,
        "deleteShare" => shares::delete(c).await,
        "getSonicSimilarTracks" => sonic::similar(c).await,
        "findSonicPath" => sonic::path(c).await,
        "deviceHello" => encore::device_hello(c).await,
        "deviceState" => encore::device_state(c).await,
        "deviceList" => encore::device_list(c).await,
        "deviceCommand" => encore::device_command(c).await,
        "devicePoll" => encore::device_poll(c).await,
        "deviceBye" => encore::device_bye(c).await,
        "syncGet" => encore::sync_get(c).await,
        "syncSet" => encore::sync_set(c).await,
        "pairApprove" => encore::pair_approve(c).await,
        "getListeningStats" => encore::listening_stats(c).await,
        "getListeningReview" => encore::listening_review(c).await,
        "playlistShareStart" => crate::collab::share_start(c).await,
        "playlistShareEnd" => crate::collab::share_end(c).await,
        "playlistShares" => crate::collab::shares(c).await,
        "translateLyrics" => translate::translate_lyrics(c).await,
        "identifySong" => bridges::identify_song(c).await,
        "getMixInfo" => bridges::mix_info(c).await,
        "getMixes" => crate::mixes::get_mixes(c).await,
        "setMix" => crate::mixes::set_mix(c, false).await,
        "refreshMix" => crate::mixes::set_mix(c, true).await,
        "getConcerts" => crate::concerts::get_concerts(c, false).await,
        "refreshConcerts" => crate::concerts::get_concerts(c, true).await,
        "getFolder" => encore::folder(c).await,
        "jamStart" => encore::jam_start(c).await,
        "jamState" => encore::jam_state(c).await,
        "jamPoll" => encore::jam_poll(c).await,
        "jamEnd" => encore::jam_end(c).await,
        "getPodcasts" => podcasts::get_podcasts(c).await,
        "getNewestPodcasts" => podcasts::newest(c).await,
        "getPodcastEpisode" => podcasts::get_episode(c).await,
        "createPodcastChannel" => podcasts::create(c).await,
        "deletePodcastChannel" => podcasts::delete_show(c).await,
        "refreshPodcasts" => podcasts::refresh_all(c).await,
        "downloadPodcastEpisode" => podcasts::download_episode(c).await,
        "deletePodcastEpisode" => podcasts::delete_episode(c).await,
        "setPodcastProgress" => podcasts::set_podcast_progress(c).await,
        "createBookmark" => podcasts::create_bookmark(c).await,
        "deleteBookmark" => podcasts::delete_bookmark(c).await,
        "getBookmarks" => podcasts::get_bookmarks(c).await,
        "getInternetRadioStations" => userdata::radio_list(c).await,
        "createInternetRadioStation" => userdata::radio_create(c).await,
        "updateInternetRadioStation" => userdata::radio_update(c).await,
        "deleteInternetRadioStation" => userdata::radio_delete(c).await,
        _ => Err(ApiError { code: 0, message: format!("Unknown method: {method}"), http: StatusCode::NOT_FOUND }),
    }
}

fn envelope(status: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("status".into(), status.into());
    m.insert("version".into(), API_VERSION.into());
    m.insert("type".into(), SERVER_TYPE.into());
    m.insert("serverVersion".into(), env!("CARGO_PKG_VERSION").into());
    m.insert("openSubsonic".into(), true.into());
    m
}

fn render(fmt: Fmt, r: Result<Reply, ApiError>) -> Response {
    let (http, body) = match r {
        Ok(Reply::Raw(resp)) => return resp,
        Ok(Reply::Ok(v)) => {
            let mut m = envelope("ok");
            if let Value::Object(o) = v {
                m.extend(o);
            }
            (StatusCode::OK, m)
        }
        Err(e) => {
            let mut m = envelope("failed");
            m.insert("error".into(), json!({ "code": e.code, "message": e.message }));
            (e.http, m)
        }
    };
    match fmt {
        Fmt::Json => {
            let v = json!({ "subsonic-response": Value::Object(body) });
            (http, [(header::CONTENT_TYPE, "application/json")], v.to_string()).into_response()
        }
        Fmt::Xml => {
            let mut s = String::from(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
            xml_element(&mut s, "subsonic-response", &body, true);
            (http, [(header::CONTENT_TYPE, "application/xml; charset=utf-8")], s).into_response()
        }
    }
}

/// Subsonic XML from the JSON shape: scalars become attributes, objects and arrays child elements,
/// and a "value" field becomes the element's text.
fn xml_element(out: &mut String, name: &str, obj: &Map<String, Value>, root: bool) {
    out.push('<');
    out.push_str(name);
    if root {
        out.push_str(r#" xmlns="http://subsonic.org/restapi""#);
    }
    let mut text = None;
    for (k, v) in obj {
        match v {
            Value::String(_) | Value::Number(_) | Value::Bool(_) => {
                let s = match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                if k == "value" {
                    text = Some(s);
                } else {
                    out.push_str(&format!(" {k}=\"{}\"", xml_escape(&s)));
                }
            }
            _ => {}
        }
    }
    out.push('>');
    if let Some(t) = text {
        out.push_str(&xml_escape(&t));
    }
    for (k, v) in obj {
        match v {
            Value::Object(o) => xml_element(out, k, o, false),
            Value::Array(a) => {
                for item in a {
                    match item {
                        Value::Object(o) => xml_element(out, k, o, false),
                        Value::Null => {}
                        scalar => {
                            let s = match scalar {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            out.push_str(&format!("<{k}>{}</{k}>", xml_escape(&s)));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => o.push(c),
        }
    }
    o
}

pub fn raw(status: StatusCode, headers: &[(header::HeaderName, String)], body: Body) -> Reply {
    let mut r = Response::builder().status(status);
    for (k, v) in headers {
        r = r.header(k, v);
    }
    Reply::Raw(r.body(body).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_secrets_in_logged_addresses() {
        let u: axum::http::Uri = "/rest/stream.view?u=me&p=enc%3A736563726574&t=abc&s=salt&apiKey=k1&id=42&c=app".parse().unwrap();
        let l = loggable_uri(&u);
        assert_eq!(l, "/rest/stream.view?u=me&p=…&t=…&s=…&apiKey=…&id=42&c=app");
        assert_eq!(loggable_uri(&"/setup?token=abc".parse().unwrap()), "/setup?token=…");
        assert_eq!(loggable_uri(&"/health".parse().unwrap()), "/health");
    }

    #[test]
    fn xml_shape() {
        let v = json!({ "lyrics": { "artist": "A & B", "value": "la <la>" }, "list": { "x": [{ "id": "1" }, { "id": "2" }] } });
        let mut s = String::new();
        xml_element(&mut s, "r", v.as_object().unwrap(), false);
        assert_eq!(s, r#"<r><list><x id="1"></x><x id="2"></x></list><lyrics artist="A &amp; B">la &lt;la&gt;</lyrics></r>"#);
    }
}
