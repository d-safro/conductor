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

/// The music folders an app picked (`musicFolderId`, once or more), or all of them. A folder that isn't there is
/// "not found" (code 70), as in Navidrome, rather than a list of everything that looks like the folder's.
pub(crate) struct Folders(Option<String>);

impl Folders {
    pub(crate) fn of(c: &Ctx) -> Result<Self, ApiError> {
        let asked: Vec<&str> = c.p.all("musicFolderId").into_iter().map(str::trim).filter(|v| !v.is_empty()).collect();
        if asked.is_empty() {
            return Ok(Self(None));
        }
        let mut ids = vec![];
        for a in asked {
            match a.parse::<i64>().ok().filter(|id| c.st.libs.iter().any(|(l, _)| l == id)) {
                Some(id) => ids.push(id.to_string()),
                None => return Err(ApiError::new(70, format!("Music folder {a} not found"))),
            }
        }
        // (numbers checked against the folders: safe in the SQL)
        Ok(Self(Some(ids.join(","))))
    }

    /// A song (`t`) in the folders.
    pub(crate) fn tracks(&self) -> String {
        match &self.0 {
            None => "1".into(),
            Some(ids) => format!("t.library_id IN ({ids})"),
        }
    }

    /// An album (`al`) with a song in the folders.
    pub(crate) fn albums(&self) -> String {
        match &self.0 {
            None => "1".into(),
            Some(ids) => format!(
                "EXISTS (SELECT 1 FROM track f WHERE f.album_id = al.id AND f.missing_since IS NULL AND f.library_id IN ({ids}))"
            ),
        }
    }

    /// An artist (`ar`) with an album or a song in the folders.
    pub(crate) fn artists(&self) -> String {
        match &self.0 {
            None => "1".into(),
            Some(ids) => format!(
                "(EXISTS (SELECT 1 FROM album fa JOIN track f ON f.album_id = fa.id
                   WHERE fa.artist_id = ar.id AND f.missing_since IS NULL AND f.library_id IN ({ids}))
                  OR EXISTS (SELECT 1 FROM track f WHERE f.artist_id = ar.id AND f.missing_since IS NULL AND f.library_id IN ({ids})))"
            ),
        }
    }

    pub(crate) fn all(&self) -> bool {
        self.0.is_none()
    }
}

fn index_letter(sort: &str) -> String {
    match sort.chars().next() {
        Some(ch) if ch.is_alphabetic() => ch.to_uppercase().collect(),
        _ => "#".into(),
    }
}

pub async fn artists(c: &Ctx, old_style: bool) -> Result<Reply, ApiError> {
    let folders = Folders::of(c)?;
    let rows = items::artists_where(c, &format!("ar.album_count > 0 AND {}", folders.artists()), &[], "ORDER BY ar.sort_name").await?;
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
    let songs = items::tracks_where(c, "t.album_id = ?", &[id], "ORDER BY COALESCE(t.disc, 1), t.number, t.sort_title").await?;
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
            let a = items::artists_where(c, "ar.id = ?", &[id], "")
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::not_found("Artist"))?;
            let albums = items::albums_where(c, "al.artist_id = ?", &[id], "ORDER BY al.year, al.sort_name").await?;
            let mut d = json!({ "id": a.id, "name": a.name, "albumCount": a.album_count, "userRating": a.rating.unwrap_or(0),
                "child": albums.iter().map(album_json).collect::<Vec<_>>() });
            if let Some(s) = a.starred_at {
                d["starred"] = iso(s).into();
            }
            ok(json!({ "directory": d }))
        }
        Some("album") => {
            let a = items::albums_where(c, "al.id = ?", &[id], "")
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::not_found("Album"))?;
            let songs = items::tracks_where(c, "t.album_id = ?", &[id], "ORDER BY COALESCE(t.disc, 1), t.number").await?;
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
    let (from, to) = (c.p.int("fromYear").unwrap_or(0), c.p.int("toYear").unwrap_or(9999));
    let years = (from.to_string(), to.to_string());
    let genre_cond;
    let (cond, binds, order): (&str, Vec<&str>, &str) = match ty {
        "random" => ("1", vec![], "RANDOM()"),
        "newest" => ("1", vec![], "al.created_at DESC, al.id"),
        "frequent" => ("an.play_count > 0", vec![], "an.play_count DESC"),
        "recent" => ("an.last_played IS NOT NULL", vec![], "an.last_played DESC"),
        "starred" => ("an.starred_at IS NOT NULL", vec![], "an.starred_at DESC"),
        "highest" => ("an.rating > 0", vec![], "an.rating DESC"),
        "alphabeticalByName" => ("1", vec![], "al.sort_name"),
        "alphabeticalByArtist" => ("1", vec![], "al.artist_name COLLATE NOCASE, al.year"),
        "byYear" => {
            let (lo, hi) = if from > to { (&years.1, &years.0) } else { (&years.0, &years.1) };
            ("al.year BETWEEN ? AND ?", vec![lo.as_str(), hi.as_str()], if from > to { "al.year DESC" } else { "al.year ASC" })
        }
        "byGenre" => {
            genre_cond = format!("al.id IN (SELECT t.album_id FROM track t WHERE {})", crate::library::has_genre_sql("t.genre"));
            (genre_cond.as_str(), vec![c.p.req("genre")?], "al.sort_name")
        }
        other => return Err(ApiError::new(0, format!("Unknown list type: {other}"))),
    };
    let cond = format!("{cond} AND {}", Folders::of(c)?.albums());
    let rows = items::albums_where(c, &cond, &binds, &format!("ORDER BY {order} LIMIT {size} OFFSET {offset}")).await?;
    let list: Vec<Value> = rows.iter().map(album_json).collect();
    if id3 { ok(json!({ "albumList2": { "album": list } })) } else { ok(json!({ "albumList": { "album": list } })) }
}

pub async fn random_songs(c: &Ctx) -> Result<Reply, ApiError> {
    let size = clamp(c.p.int("size"), 10, 500);
    let mut cond = vec![Folders::of(c)?.tracks()];
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
        &format!("{} AND {}", crate::library::has_genre_sql("t.genre"), Folders::of(c)?.tracks()),
        &[g],
        &format!("ORDER BY t.album_name, COALESCE(t.disc, 1), t.number LIMIT {count} OFFSET {offset}"),
    )
    .await?;
    ok(json!({ "songsByGenre": { "song": rows.iter().map(song_json).collect::<Vec<_>>() } }))
}

pub async fn genres(c: &Ctx) -> Result<Reply, ApiError> {
    // each genre of a song counts ("Pop; Rock" is in Pop and in Rock); counted per genre value and album in SQL,
    // then split (a few thousand rows, not one per song)
    // (Navidrome counts in the folders an app picked, too)
    let in_folders = Folders::of(c)?.tracks();
    let rows: Vec<(String, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT genre, album_id, COUNT(*) FROM track t WHERE genre IS NOT NULL AND genre != '' AND missing_since IS NULL
         AND {in_folders} GROUP BY genre, album_id"
    )))
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

/// Every word must appear somewhere in the item's name fields, in any case. An empty query (or `""`) lists everything.
/// SQLite's LIKE ignores case only for A-Z, so a word with other letters (Cyrillic, Greek…) is also looked for in
/// lower case, upper case and capitalised: "кино" finds "Кино" and "КИНО".
pub(crate) fn word_conditions(q: &str, fields: &str) -> (String, Vec<String>) {
    let q = q.trim().trim_matches('"').trim();
    let (mut conds, mut binds) = (vec![], vec![]);
    for w in q.split_whitespace() {
        let mut forms = vec![w.to_string()];
        if !w.is_ascii() {
            let mut c = w.chars();
            let cap = c.next().map(|f| f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect()).unwrap_or_default();
            forms.extend([w.to_lowercase(), w.to_uppercase(), cap]);
            forms.sort();
            forms.dedup();
        }
        conds.push(format!("({})", forms.iter().map(|_| format!("({fields}) LIKE ? ESCAPE '\\'")).collect::<Vec<_>>().join(" OR ")));
        binds.extend(forms.iter().map(|f| format!("%{}%", f.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))));
    }
    if conds.is_empty() {
        return ("1".into(), vec![]);
    }
    (conds.join(" AND "), binds)
}

pub async fn search(c: &Ctx, version: u8) -> Result<Reply, ApiError> {
    let q = c.p.get("query").unwrap_or("");
    let page = |count: &str, offset: &str, def: i64| {
        format!("LIMIT {} OFFSET {}", clamp(c.p.int(count), def, 500), c.p.int(offset).unwrap_or(0).max(0))
    };
    let folders = Folders::of(c)?;
    let (ac, ab) = word_conditions(q, "ar.name");
    let ab: Vec<&str> = ab.iter().map(String::as_str).collect();
    let artists = items::artists_where(
        c,
        &format!("ar.album_count > 0 AND {ac} AND {}", folders.artists()),
        &ab,
        &format!("ORDER BY ar.sort_name, ar.id {}", page("artistCount", "artistOffset", 20)),
    )
    .await?;
    let (lc, lb) = word_conditions(q, "al.name || ' ' || al.artist_name");
    let lb: Vec<&str> = lb.iter().map(String::as_str).collect();
    let albums =
        items::albums_where(c, &format!("{lc} AND {}", folders.albums()), &lb, &format!("ORDER BY al.sort_name, al.id {}", page("albumCount", "albumOffset", 20))).await?;
    let (sc, sb) = word_conditions(q, "t.title || ' ' || t.artist_name || ' ' || t.album_name");
    let sb: Vec<&str> = sb.iter().map(String::as_str).collect();
    // the page's ids first (from the name-order index), then only those songs in full: apps that fetch the whole
    // library 500 at a time would otherwise sort every song for every page (1 s a page at 100,000 songs; now ~10 ms)
    let songs = items::tracks_where(
        c,
        &format!(
            "t.id IN (SELECT t.id FROM track t WHERE t.missing_since IS NULL AND ({sc}) AND {} ORDER BY t.sort_title, t.id {})",
            folders.tracks(),
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
    let f = Folders::of(c)?;
    let artists =
        items::artists_where(c, &format!("{} AND {}", mine("artist", "ar.id"), f.artists()), &me, "ORDER BY an.starred_at DESC").await?;
    let albums =
        items::albums_where(c, &format!("{} AND {}", mine("album", "al.id"), f.albums()), &me, "ORDER BY an.starred_at DESC").await?;
    let songs =
        items::tracks_where(c, &format!("{} AND {}", mine("track", "t.id"), f.tracks()), &me, "ORDER BY an.starred_at DESC").await?;
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
