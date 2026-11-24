//! A library album's whole song list as Deezer has it, kept for good once it's looked up
//! (migrations/0029_album_songs.sql): opening the album again isn't another match and fetch. Each song points to
//! your copy (one of yours, or the download once it arrives), and a song you don't have yet is filed into the
//! album when it's downloaded (`filed_into`).

use std::collections::HashSet;

use serde_json::Value;

use super::{find, match_album, n, s, statuses, text::tk};
use crate::{api::AppState, db::now};

/// A library album as Deezer has it: Deezer's album, its songs in order with your copy of each (None: you don't
/// have it), then your songs that aren't on it.
pub struct AlbumSongs {
    pub album: Value,
    pub songs: Vec<(Value, Option<String>)>,
    pub extra: Vec<String>,
}

/// This library album's songs as Deezer has them (kept once looked up). `artists`: the names to look it up by, in
/// order (the album's artist, then the one all its songs have). `local`: your songs on it, (id, title).
/// None: Deezer has no such album (or didn't answer: it's looked up again next time).
pub async fn album_songs(
    st: &AppState,
    album_id: &str,
    artists: &[String],
    name: &str,
    local: &[(String, String)],
) -> Option<AlbumSongs> {
    let (album, rows) = match stored(st, album_id).await {
        Some(x) => x,
        None => look_up(st, album_id, artists, name, local).await?,
    };
    let keys: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
    let arrived = statuses(st, &keys).await;
    let mut used: HashSet<String> = HashSet::new();
    let mut songs = vec![];
    for (pos, key, t, had) in rows {
        let free = |id: &String| !used.contains(id);
        // yours on this album first (a download filed into it is too); then the copy it pointed to, the download
        // asked for from here, or the same song anywhere in your library
        let mut mine = local.iter().find(|(id, title)| free(id) && tk(title) == tk(s(&t, "title"))).map(|(id, _)| id.clone());
        for id in [had.clone(), arrived.get(&key).and_then(|a| a.local_id.clone())].into_iter().flatten() {
            if mine.is_none() && free(&id) && exists(st, &id).await {
                mine = Some(id);
            }
        }
        if mine.is_none() {
            mine = find(st, s(&t, "title"), s(&t["artist"], "name"), Some(n(&t, "duration"))).await.map(|x| x.id).filter(free);
        }
        if let Some(m) = &mine {
            used.insert(m.clone());
        }
        if mine != had {
            let _ = sqlx::query("UPDATE album_song SET local_id = ? WHERE album_id = ? AND pos = ?")
                .bind(&mine)
                .bind(album_id)
                .bind(pos)
                .execute(&st.db)
                .await;
        }
        songs.push((t, mine));
    }
    if songs.iter().any(|(_, m)| m.is_none()) {
        // the album a missing song was last seen on is the one it's filed into
        let _ = sqlx::query("UPDATE album_songs SET seen_at = ? WHERE album_id = ?").bind(now()).bind(album_id).execute(&st.db).await;
    }
    let extra = local.iter().filter(|(id, _)| !used.contains(id)).map(|(id, _)| id.clone()).collect();
    Some(AlbumSongs { album, songs, extra })
}

type Row = (i64, String, Value, Option<String>);

async fn stored(st: &AppState, album_id: &str) -> Option<(Value, Vec<Row>)> {
    let album: String = sqlx::query_scalar("SELECT album_json FROM album_songs WHERE album_id = ?")
        .bind(album_id)
        .fetch_optional(&st.db)
        .await
        .ok()??;
    let rows: Vec<(i64, String, String, Option<String>)> =
        sqlx::query_as("SELECT pos, key, track_json, local_id FROM album_song WHERE album_id = ? ORDER BY pos")
            .bind(album_id)
            .fetch_all(&st.db)
            .await
            .ok()?;
    let rows = rows.into_iter().filter_map(|(p, k, t, l)| Some((p, k, serde_json::from_str(&t).ok()?, l))).collect();
    Some((serde_json::from_str(&album).ok()?, rows))
}

async fn look_up(
    st: &AppState,
    album_id: &str,
    artists: &[String],
    name: &str,
    local: &[(String, String)],
) -> Option<(Value, Vec<Row>)> {
    let titles: Vec<String> = local.iter().map(|(_, t)| t.clone()).collect();
    let mut did = None;
    for a in artists {
        did = match_album(st, a, name, &titles).await;
        if did.is_some() {
            break;
        }
    }
    let did = did?;
    // (None: Deezer didn't answer; nothing is kept, it's looked up next time)
    let mut album = st.catalog.dz.try_get(&format!("album/{did}"), &[]).await?;
    if let Some(o) = album.as_object_mut() {
        o.remove("tracks"); // kept apart, one row each
    }
    let tracks =
        st.catalog.dz.try_list(&format!("album/{did}/tracks"), &[("limit", "500")]).await.filter(|t| !t.is_empty())?;
    let rows: Vec<Row> =
        tracks.into_iter().enumerate().map(|(i, t)| (i as i64, format!("dz-t-{}", n(&t, "id")), t, None)).collect();
    let mut tx = st.db.begin().await.ok()?;
    sqlx::query("INSERT OR REPLACE INTO album_songs (album_id, dz_album, album_json, at, seen_at) VALUES (?, ?, ?, ?, ?)")
        .bind(album_id)
        .bind(&did)
        .bind(album.to_string())
        .bind(now())
        .bind(now())
        .execute(&mut *tx)
        .await
        .ok()?;
    sqlx::query("DELETE FROM album_song WHERE album_id = ?").bind(album_id).execute(&mut *tx).await.ok()?;
    for (pos, key, t, _) in &rows {
        sqlx::query("INSERT INTO album_song (album_id, pos, key, disc, track, track_json) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(album_id)
            .bind(pos)
            .bind(key)
            .bind(n(t, "disk_number").max(1))
            .bind(n(t, "track_position").max(1))
            .bind(t.to_string())
            .execute(&mut *tx)
            .await
            .ok()?;
    }
    tx.commit().await.ok()?;
    Some((album, rows))
}

async fn exists(st: &AppState, id: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM track WHERE id = ? AND missing_since IS NULL")
        .bind(id)
        .fetch_one(&st.db)
        .await
        .is_ok_and(|c| c > 0)
}

/// The album a downloaded song goes into: the library album it was last seen missing from, as it's tagged now.
#[derive(Debug, Clone, PartialEq)]
pub struct FiledInto {
    pub album: String,
    pub album_artist: String,
    pub year: Option<i64>,
    pub genre: Option<String>,
    pub disc: i64,
    pub track: i64,
}

pub async fn filed_into(st: &AppState, key: &str) -> Option<FiledInto> {
    let row: (String, String, Option<i64>, Option<String>, i64, i64) = sqlx::query_as(
        "SELECT a.name, a.artist_name, a.year, a.genre, s.disc, s.track FROM album_song s
           JOIN album_songs h ON h.album_id = s.album_id JOIN album a ON a.id = s.album_id
          WHERE s.key = ? ORDER BY h.seen_at DESC LIMIT 1",
    )
    .bind(key)
    .fetch_optional(&st.db)
    .await
    .ok()??;
    let (album, album_artist, year, genre, disc, track) = row;
    Some(FiledInto { album, album_artist, year: year.filter(|y| *y > 0), genre: genre.filter(|g| !g.is_empty()), disc, track })
}
