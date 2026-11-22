//! Rows from the database and how they look in the API (Subsonic "child", AlbumID3, ArtistID3).

use serde_json::{Value, json};

use super::{ApiError, Ctx};
use crate::db::iso;

/// Songs with their album artist and this user's plays, rating and star. Bind the user id first.
pub const TRACK_SELECT: &str = "SELECT t.*, al.artist_id AS album_artist_id, al.artist_name AS album_artist_name,
       an.play_count, an.last_played, an.rating, an.starred_at
  FROM track t JOIN album al ON al.id = t.album_id
  LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'track' AND an.item_id = t.id";

pub const ALBUM_SELECT: &str = "SELECT al.*, an.play_count, an.last_played, an.rating, an.starred_at
  FROM album al
  LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'album' AND an.item_id = al.id";

pub const ARTIST_SELECT: &str = "SELECT ar.*, an.rating, an.starred_at,
       (SELECT id FROM album a WHERE a.artist_id = ar.id AND a.song_count > 0 ORDER BY a.year DESC LIMIT 1) AS cover_album
  FROM artist ar
  LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'artist' AND an.item_id = ar.id";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrackRow {
    pub id: String,
    pub library_id: i64,
    pub rel_path: String,
    pub size: i64,
    pub suffix: String,
    pub content_type: String,
    pub bitrate: i64,
    pub sample_rate: i64,
    pub bit_depth: Option<i64>,
    pub channels: i64,
    pub duration_ms: i64,
    pub title: String,
    pub sort_title: String,
    pub artist_id: String,
    pub artist_name: String,
    pub album_id: String,
    pub album_name: String,
    pub disc: Option<i64>,
    pub number: Option<i64>,
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub mbid: Option<String>,
    pub created_at: i64,
    pub isrc: Option<String>,
    pub rg_track_gain: Option<f64>,
    pub rg_track_peak: Option<f64>,
    pub rg_album_gain: Option<f64>,
    pub rg_album_peak: Option<f64>,
    pub album_artist_id: String,
    pub album_artist_name: String,
    pub play_count: Option<i64>,
    pub last_played: Option<i64>,
    pub rating: Option<i64>,
    pub starred_at: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AlbumRow {
    pub id: String,
    pub name: String,
    pub sort_name: String,
    pub artist_id: String,
    pub artist_name: String,
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub mbid: Option<String>,
    pub compilation: bool,
    pub song_count: i64,
    pub duration_ms: i64,
    pub created_at: i64,
    pub play_count: Option<i64>,
    pub last_played: Option<i64>,
    pub rating: Option<i64>,
    pub starred_at: Option<i64>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ArtistRow {
    pub id: String,
    pub name: String,
    pub sort_name: String,
    pub mbid: Option<String>,
    pub album_count: i64,
    pub rating: Option<i64>,
    pub starred_at: Option<i64>,
    pub cover_album: Option<String>,
}

fn put(v: &mut Value, k: &str, x: impl Into<Value>) {
    v.as_object_mut().unwrap().insert(k.into(), x.into());
}

pub fn song_json(t: &TrackRow) -> Value {
    let mut v = json!({
        "id": t.id,
        "parent": t.album_id,
        "isDir": false,
        "title": t.title,
        "album": t.album_name,
        "artist": t.artist_name,
        "coverArt": t.album_id,
        "size": t.size,
        "contentType": t.content_type,
        "suffix": t.suffix,
        "duration": t.duration_ms / 1000,
        "bitRate": t.bitrate,
        "samplingRate": t.sample_rate,
        "channelCount": t.channels,
        "path": t.rel_path,
        "created": iso(t.created_at),
        "albumId": t.album_id,
        "artistId": t.artist_id,
        "type": "music",
        "mediaType": "song",
        "isVideo": false,
        "playCount": t.play_count.unwrap_or(0),
        "userRating": t.rating.unwrap_or(0),
        "sortName": t.sort_title,
        "musicBrainzId": t.mbid.clone().unwrap_or_default(),
        "artists": [{ "id": t.artist_id, "name": t.artist_name }],
        "displayArtist": t.artist_name,
        "albumArtists": [{ "id": t.album_artist_id, "name": t.album_artist_name }],
        "displayAlbumArtist": t.album_artist_name,
        "genres": t.genre.iter().flat_map(|g| crate::library::split_genres(g)).map(|g| json!({ "name": g })).collect::<Vec<_>>(),
    });
    if let Some(n) = t.number {
        put(&mut v, "track", n);
    }
    if let Some(d) = t.disc {
        put(&mut v, "discNumber", d);
    }
    if let Some(y) = t.year {
        put(&mut v, "year", y);
    }
    if let Some(g) = t.genre.as_deref().and_then(|g| crate::library::split_genres(g).into_iter().next()) {
        put(&mut v, "genre", g); // the first; all of them are in "genres"
    }
    let mut rg = serde_json::Map::new();
    for (k, x) in [
        ("trackGain", t.rg_track_gain),
        ("trackPeak", t.rg_track_peak),
        ("albumGain", t.rg_album_gain),
        ("albumPeak", t.rg_album_peak),
    ] {
        if let Some(x) = x {
            rg.insert(k.into(), x.into());
        }
    }
    // OpenSubsonic's fields, always there (empty or 0 when not known), as Navidrome sends them: some apps expect them
    put(&mut v, "replayGain", Value::Object(rg));
    put(&mut v, "isrc", t.isrc.iter().cloned().collect::<Vec<_>>());
    put(&mut v, "bitDepth", t.bit_depth.unwrap_or(0));
    put(&mut v, "bpm", 0);
    put(&mut v, "comment", "");
    put(&mut v, "moods", Vec::<Value>::new());
    put(&mut v, "contributors", Vec::<Value>::new());
    put(&mut v, "displayComposer", "");
    put(&mut v, "explicitStatus", "");
    if let Some(p) = t.last_played {
        put(&mut v, "played", iso(p));
    }
    if let Some(s) = t.starred_at {
        put(&mut v, "starred", iso(s));
    }
    v
}

pub fn album_json(a: &AlbumRow) -> Value {
    let mut v = json!({
        "id": a.id,
        "name": a.name,
        "title": a.name,
        "album": a.name,
        "isDir": true,
        "parent": a.artist_id,
        "artist": a.artist_name,
        "artistId": a.artist_id,
        "coverArt": a.id,
        "songCount": a.song_count,
        "duration": a.duration_ms / 1000,
        "created": iso(a.created_at),
        "playCount": a.play_count.unwrap_or(0),
        "userRating": a.rating.unwrap_or(0),
        "musicBrainzId": a.mbid.clone().unwrap_or_default(),
        "isCompilation": a.compilation,
        "sortName": a.sort_name,
        "artists": [{ "id": a.artist_id, "name": a.artist_name }],
        "displayArtist": a.artist_name,
        "genres": a.genre.iter().flat_map(|g| crate::library::split_genres(g)).map(|g| json!({ "name": g })).collect::<Vec<_>>(),
        "mediaType": "album",
        "releaseTypes": [],
        "recordLabels": [],
        "moods": [],
        "discTitles": [],
        "explicitStatus": "",
        "version": "",
        "releaseDate": {},
        "originalReleaseDate": {},
    });
    if let Some(y) = a.year {
        put(&mut v, "year", y);
        put(&mut v, "releaseDate", json!({ "year": y }));
        put(&mut v, "originalReleaseDate", json!({ "year": y }));
    }
    if let Some(g) = a.genre.as_deref().and_then(|g| crate::library::split_genres(g).into_iter().next()) {
        put(&mut v, "genre", g);
    }
    if let Some(p) = a.last_played {
        put(&mut v, "played", iso(p));
    }
    if let Some(s) = a.starred_at {
        put(&mut v, "starred", iso(s));
    }
    v
}

pub fn artist_json(a: &ArtistRow) -> Value {
    let mut v = json!({
        "id": a.id,
        "name": a.name,
        "albumCount": a.album_count,
        "userRating": a.rating.unwrap_or(0),
        "musicBrainzId": a.mbid.clone().unwrap_or_default(),
        "sortName": a.sort_name,
        "roles": if a.album_count > 0 { json!(["albumartist", "artist"]) } else { json!(["artist"]) },
    });
    if let Some(c) = &a.cover_album {
        put(&mut v, "coverArt", c.clone());
    }
    if let Some(s) = a.starred_at {
        put(&mut v, "starred", iso(s));
    }
    v
}

pub async fn tracks_where(c: &Ctx, cond: &str, binds: &[&str], tail: &str) -> Result<Vec<TrackRow>, ApiError> {
    let sql = format!("{TRACK_SELECT} WHERE t.missing_since IS NULL AND ({cond}) {tail}");
    let mut q = sqlx::query_as::<_, TrackRow>(&sql).bind(&c.user.id);
    for b in binds {
        q = q.bind(*b);
    }
    Ok(q.fetch_all(&c.st.db).await?)
}

pub async fn track(c: &Ctx, id: &str) -> Result<TrackRow, ApiError> {
    let sql = format!("{TRACK_SELECT} WHERE t.id = ?");
    sqlx::query_as::<_, TrackRow>(&sql)
        .bind(&c.user.id)
        .bind(id)
        .fetch_optional(&c.st.db)
        .await?
        .ok_or_else(|| ApiError::not_found("Song"))
}

pub async fn albums_where(c: &Ctx, cond: &str, binds: &[&str], tail: &str) -> Result<Vec<AlbumRow>, ApiError> {
    let sql = format!("{ALBUM_SELECT} WHERE al.song_count > 0 AND ({cond}) {tail}");
    let mut q = sqlx::query_as::<_, AlbumRow>(&sql).bind(&c.user.id);
    for b in binds {
        q = q.bind(*b);
    }
    Ok(q.fetch_all(&c.st.db).await?)
}

pub async fn artists_where(c: &Ctx, cond: &str, binds: &[&str], tail: &str) -> Result<Vec<ArtistRow>, ApiError> {
    let sql = format!("{ARTIST_SELECT} WHERE ({cond}) {tail}");
    let mut q = sqlx::query_as::<_, ArtistRow>(&sql).bind(&c.user.id);
    for b in binds {
        q = q.bind(*b);
    }
    Ok(q.fetch_all(&c.st.db).await?)
}

/// Which kind of item an id is.
pub async fn kind_of(c: &Ctx, id: &str) -> Result<Option<&'static str>, ApiError> {
    for (kind, table) in [("track", "track"), ("album", "album"), ("artist", "artist")] {
        let n: i64 =
            sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table} WHERE id = ?")).bind(id).fetch_one(&c.st.db).await?;
        if n > 0 {
            return Ok(Some(kind));
        }
    }
    Ok(None)
}
