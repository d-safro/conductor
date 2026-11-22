//! Browsing and searching: artists, albums, songs, lists, genres, starred, scan status.

use std::{collections::BTreeMap, sync::atomic::Ordering};

use serde_json::{Value, json};

use super::{
    ApiError, Ctx, Reply,
    items::{self, album_json, artist_json, song_json},
    ok,
};
use crate::{db::iso, library};

fn clamp(v: Option<i64>, default: i64, max: i64) -> i64 {
    v.unwrap_or(default).clamp(0, max)
}

pub async fn music_folders(c: &Ctx) -> Result<Reply, ApiError> {
    let f: Vec<Value> = c.st.libs.iter().map(|(id, l)| json!({ "id": id, "name": l.name })).collect();
    ok(json!({ "musicFolders": { "musicFolder": f } }))
}

fn index_letter(sort: &str) -> String {
    match sort.chars().next() {
        Some(ch) if ch.is_alphabetic() => ch.to_uppercase().collect(),
        _ => "#".into(),
    }
}

pub async fn artists(c: &Ctx, old_style: bool) -> Result<Reply, ApiError> {
    let rows = items::artists_where(c, "ar.album_count > 0", &[], "ORDER BY ar.sort_name").await?;
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for a in &rows {
        groups.entry(index_letter(&a.sort_name)).or_default().push(artist_json(a));
    }
    let index: Vec<Value> = groups.into_iter().map(|(name, artist)| json!({ "name": name, "artist": artist })).collect();
    let last = c.st.scan.last_scan.load(Ordering::Relaxed) * 1000;
    if old_style {
        ok(json!({ "indexes": { "lastModified": last, "ignoredArticles": "The An A", "index": index } }))
    } else {
        ok(json!({ "artists": { "lastModified": last, "ignoredArticles": "The An A", "index": index } }))
    }
}

pub async fn artist(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    let a =
        items::artists_where(c, "ar.id = ?", &[id], "").await?.into_iter().next().ok_or_else(|| ApiError::not_found("Artist"))?;
    let albums = items::albums_where(c, "al.artist_id = ?", &[id], "ORDER BY al.year, al.sort_name").await?;
    let mut v = artist_json(&a);
    v["album"] = albums.iter().map(album_json).collect();
    ok(json!({ "artist": v }))
}

pub async fn album(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    let a =
        items::albums_where(c, "al.id = ?", &[id], "").await?.into_iter().next().ok_or_else(|| ApiError::not_found("Album"))?;
    let songs = items::tracks_where(c, "t.album_id = ?", &[id], "ORDER BY t.disc, t.number, t.sort_title").await?;
    let mut v = album_json(&a);
    v["song"] = songs.iter().map(song_json).collect();
    ok(json!({ "album": v }))
}

pub async fn song(c: &Ctx) -> Result<Reply, ApiError> {
    let t = items::track(c, c.p.req("id")?).await?;
    ok(json!({ "song": song_json(&t) }))
}

/// Folder-style browsing on top of artists and albums (for older apps).
pub async fn directory(c: &Ctx) -> Result<Reply, ApiError> {
    let id = c.p.req("id")?;
    match items::kind_of(c, id).await? {
        Some("artist") => {
            let a = items::artists_where(c, "ar.id = ?", &[id], "").await?.remove(0);
            let albums = items::albums_where(c, "al.artist_id = ?", &[id], "ORDER BY al.year, al.sort_name").await?;
            let mut d = json!({ "id": a.id, "name": a.name, "albumCount": a.album_count, "userRating": a.rating.unwrap_or(0),
                "child": albums.iter().map(album_json).collect::<Vec<_>>() });
            if let Some(s) = a.starred_at {
                d["starred"] = iso(s).into();
            }
            ok(json!({ "directory": d }))
        }
        Some("album") => {
            let a = items::albums_where(c, "al.id = ?", &[id], "").await?.remove(0);
            let songs = items::tracks_where(c, "t.album_id = ?", &[id], "ORDER BY t.disc, t.number").await?;
            let mut d = json!({ "id": a.id, "name": a.name, "parent": a.artist_id, "songCount": a.song_count,
                "playCount": a.play_count.unwrap_or(0), "userRating": a.rating.unwrap_or(0),
                "child": songs.iter().map(song_json).collect::<Vec<_>>() });
            if let Some(p) = a.last_played {
                d["played"] = iso(p).into();
            }
            if let Some(s) = a.starred_at {
                d["starred"] = iso(s).into();
            }
            ok(json!({ "directory": d }))
        }
        _ => Err(ApiError::not_found("Directory")),
    }
}

pub async fn album_list(c: &Ctx, id3: bool) -> Result<Reply, ApiError> {
    let ty = c.p.req("type")?;
    let size = clamp(c.p.int("size"), 10, 500);
    let offset = c.p.int("offset").unwrap_or(0).max(0);
    let page = format!("LIMIT {size} OFFSET {offset}");
    let (from, to) = (c.p.int("fromYear").unwrap_or(0), c.p.int("toYear").unwrap_or(9999));
    let years = (from.to_string(), to.to_string());
    let rows = match ty {
        "random" => items::albums_where(c, "1", &[], &format!("ORDER BY RANDOM() {page}")).await?,
        "newest" => items::albums_where(c, "1", &[], &format!("ORDER BY al.created_at DESC, al.id {page}")).await?,
        "frequent" => items::albums_where(c, "an.play_count > 0", &[], &format!("ORDER BY an.play_count DESC {page}")).await?,
        "recent" => {
            items::albums_where(c, "an.last_played IS NOT NULL", &[], &format!("ORDER BY an.last_played DESC {page}")).await?
        }
        "starred" => {
            items::albums_where(c, "an.starred_at IS NOT NULL", &[], &format!("ORDER BY an.starred_at DESC {page}")).await?
        }
        "highest" => items::albums_where(c, "an.rating > 0", &[], &format!("ORDER BY an.rating DESC {page}")).await?,
        "alphabeticalByName" => items::albums_where(c, "1", &[], &format!("ORDER BY al.sort_name {page}")).await?,
        "alphabeticalByArtist" => {
            items::albums_where(c, "1", &[], &format!("ORDER BY al.artist_name COLLATE NOCASE, al.year {page}")).await?
        }
        "byYear" => {
            let order = if from > to { "DESC" } else { "ASC" };
            let (lo, hi) = if from > to { (&years.1, &years.0) } else { (&years.0, &years.1) };
            items::albums_where(c, "al.year BETWEEN ? AND ?", &[lo, hi], &format!("ORDER BY al.year {order} {page}")).await?
        }
        "byGenre" => {
            let g = c.p.req("genre")?;
            let cond = format!("al.id IN (SELECT t.album_id FROM track t WHERE {})", crate::library::has_genre_sql("t.genre"));
            items::albums_where(c, &cond, &[g], &format!("ORDER BY al.sort_name {page}")).await?
        }
        other => return Err(ApiError::new(0, format!("Unknown list type: {other}"))),
    };
    let list: Vec<Value> = rows.iter().map(album_json).collect();
    if id3 { ok(json!({ "albumList2": { "album": list } })) } else { ok(json!({ "albumList": { "album": list } })) }
}

pub async fn random_songs(c: &Ctx) -> Result<Reply, ApiError> {
    let size = clamp(c.p.int("size"), 10, 500);
    let mut cond = vec!["1".to_string()];
    let mut binds: Vec<String> = vec![];
    if let Some(g) = c.p.get("genre").filter(|g| !g.is_empty()) {
        cond.push(crate::library::has_genre_sql("t.genre"));
        binds.push(g.into());
    }
    if let Some(y) = c.p.int("fromYear") {
        cond.push("t.year >= ?".into());
        binds.push(y.to_string());
    }
    if let Some(y) = c.p.int("toYear") {
        cond.push("t.year <= ?".into());
        binds.push(y.to_string());
    }
    let b: Vec<&str> = binds.iter().map(String::as_str).collect();
    let rows = items::tracks_where(c, &cond.join(" AND "), &b, &format!("ORDER BY RANDOM() LIMIT {size}")).await?;
    ok(json!({ "randomSongs": { "song": rows.iter().map(song_json).collect::<Vec<_>>() } }))
}

pub async fn songs_by_genre(c: &Ctx) -> Result<Reply, ApiError> {
    let g = c.p.req("genre")?;
    let count = clamp(c.p.int("count"), 10, 500);
    let offset = c.p.int("offset").unwrap_or(0).max(0);
    let rows = items::tracks_where(
        c,
        &crate::library::has_genre_sql("t.genre"),
        &[g],
        &format!("ORDER BY t.album_name, t.disc, t.number LIMIT {count} OFFSET {offset}"),
    )
    .await?;
    ok(json!({ "songsByGenre": { "song": rows.iter().map(song_json).collect::<Vec<_>>() } }))
}

pub async fn genres(c: &Ctx) -> Result<Reply, ApiError> {
    // each genre of a song counts ("Pop; Rock" is in Pop and in Rock); counted per genre value and album in SQL,
    // then split (a few thousand rows, not one per song)
    let rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT genre, album_id, COUNT(*) FROM track WHERE genre IS NOT NULL AND genre != '' AND missing_since IS NULL
         GROUP BY genre, album_id",
    )
    .fetch_all(&c.st.db)
    .await?;
    let mut by: std::collections::BTreeMap<String, (String, i64, std::collections::HashSet<String>)> = Default::default();
    for (g, album, n) in rows {
        for name in crate::library::split_genres(&g) {
            let e = by.entry(name.to_lowercase()).or_insert_with(|| (name.clone(), 0, Default::default()));
            e.1 += n;
            e.2.insert(album.clone());
        }
    }
    let g: Vec<Value> = by.into_values().map(|(n, s, a)| json!({ "value": n, "songCount": s, "albumCount": a.len() })).collect();
    ok(json!({ "genres": { "genre": g } }))
}

/// Every word must appear somewhere in the item's name fields. An empty query (or `""`) lists everything.
fn word_conditions(q: &str, fields: &str) -> (String, Vec<String>) {
    let q = q.trim().trim_matches('"').trim();
    let words: Vec<String> = q.split_whitespace().map(|w| format!("%{}%", w.replace('%', "\\%").replace('_', "\\_"))).collect();
    if words.is_empty() {
        return ("1".into(), vec![]);
    }
    let cond = words.iter().map(|_| format!("({fields}) LIKE ? ESCAPE '\\'")).collect::<Vec<_>>().join(" AND ");
    (cond, words)
}

pub async fn search(c: &Ctx, version: u8) -> Result<Reply, ApiError> {
    let q = c.p.get("query").unwrap_or("");
    let page = |count: &str, offset: &str, def: i64| {
        format!("LIMIT {} OFFSET {}", clamp(c.p.int(count), def, 500), c.p.int(offset).unwrap_or(0).max(0))
    };
    let (ac, ab) = word_conditions(q, "ar.name");
    let ab: Vec<&str> = ab.iter().map(String::as_str).collect();
    let artists = items::artists_where(
        c,
        &format!("ar.album_count > 0 AND {ac}"),
        &ab,
        &format!("ORDER BY ar.sort_name, ar.id {}", page("artistCount", "artistOffset", 20)),
    )
    .await?;
    let (lc, lb) = word_conditions(q, "al.name || ' ' || al.artist_name");
    let lb: Vec<&str> = lb.iter().map(String::as_str).collect();
    let albums =
        items::albums_where(c, &lc, &lb, &format!("ORDER BY al.sort_name, al.id {}", page("albumCount", "albumOffset", 20))).await?;
    let (sc, sb) = word_conditions(q, "t.title || ' ' || t.artist_name || ' ' || t.album_name");
    let sb: Vec<&str> = sb.iter().map(String::as_str).collect();
    // the page's ids first (from the name-order index), then only those songs in full: apps that fetch the whole
    // library 500 at a time would otherwise sort every song for every page (1 s a page at 100,000 songs; now ~10 ms)
    let songs = items::tracks_where(
        c,
        &format!(
            "t.id IN (SELECT t.id FROM track t WHERE t.missing_since IS NULL AND ({sc}) ORDER BY t.sort_title, t.id {})",
            page("songCount", "songOffset", 20)
        ),
        &sb,
        "ORDER BY t.sort_title, t.id",
    )
    .await?;
    let body = json!({
        "artist": artists.iter().map(artist_json).collect::<Vec<_>>(),
        "album": albums.iter().map(album_json).collect::<Vec<_>>(),
        "song": songs.iter().map(song_json).collect::<Vec<_>>(),
    });
    if version == 3 { ok(json!({ "searchResult3": body })) } else { ok(json!({ "searchResult2": body })) }
}

pub async fn starred(c: &Ctx, id3: bool) -> Result<Reply, ApiError> {
    // from the user's stars (an index), not by looking at every song
    let mine = |kind: &str, col: &str| {
        format!("{col} IN (SELECT item_id FROM annotation WHERE user_id = ? AND item_kind = '{kind}' AND starred_at IS NOT NULL)")
    };
    let me = [c.user.id.as_str()];
    let artists = items::artists_where(c, &mine("artist", "ar.id"), &me, "ORDER BY an.starred_at DESC").await?;
    let albums = items::albums_where(c, &mine("album", "al.id"), &me, "ORDER BY an.starred_at DESC").await?;
    let songs = items::tracks_where(c, &mine("track", "t.id"), &me, "ORDER BY an.starred_at DESC").await?;
    let body = json!({
        "artist": artists.iter().map(artist_json).collect::<Vec<_>>(),
        "album": albums.iter().map(album_json).collect::<Vec<_>>(),
        "song": songs.iter().map(song_json).collect::<Vec<_>>(),
    });
    if id3 { ok(json!({ "starred2": body })) } else { ok(json!({ "starred": body })) }
}

pub async fn top_songs(c: &Ctx) -> Result<Reply, ApiError> {
    let artist = c.p.req("artist")?;
    let count = clamp(c.p.int("count"), 50, 500);
    let rows = items::tracks_where(
        c,
        "t.artist_name = ? COLLATE NOCASE OR al.artist_name = ? COLLATE NOCASE",
        &[artist, artist],
        &format!("ORDER BY COALESCE(an.play_count, 0) DESC, an.rating DESC, t.year LIMIT {count}"),
    )
    .await?;
    ok(json!({ "topSongs": { "song": rows.iter().map(song_json).collect::<Vec<_>>() } }))
}

/// Without outside sources yet: songs by the same artist and in the same genre, shuffled.
pub async fn scan_status(c: &Ctx) -> Result<Reply, ApiError> {
    let s = &c.st.scan;
    let last = s.last_scan.load(Ordering::Relaxed);
    let folders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM album WHERE song_count > 0").fetch_one(&c.st.db).await.unwrap_or(0);
    let mut v = json!({ "scanning": s.scanning.load(Ordering::Relaxed), "count": s.count.load(Ordering::Relaxed),
        "folderCount": folders, "scanType": if s.last_full.load(Ordering::Relaxed) { "full" } else { "quick" },
        "elapsedTime": s.last_elapsed_ms.load(Ordering::Relaxed) / 1000 });
    if last > 0 {
        v["lastScan"] = iso(last).into();
    }
    ok(json!({ "scanStatus": v }))
}

pub async fn start_scan(c: &Ctx) -> Result<Reply, ApiError> {
    if !c.user.is_admin {
        return Err(ApiError::new(50, "Only admins can start a scan"));
    }
    let st = c.st.clone();
    let full = c.p.bool("fullScan").unwrap_or(false);
    tokio::spawn(async move {
        if let Err(e) = library::scan(&st.db, &st.libs, &st.scan, full).await {
            tracing::error!("scan failed: {e:#}");
        }
    });
    scan_status(c).await
}
