//! `encore.forYou`: "Made for you" without AudioMuse-AI. `getForYou?count=` → `{forYou: {song: […]}}`: from the
//! artists you play most (the last six months, else ever, else what you've hearted) and the artists that go with
//! them (crate::similar), about 60% new-to-you artists' songs and 40% your favourites' that you haven't played in
//! two weeks; the same artist never twice in a row. The same all day, a new one each day.

use std::collections::{HashMap, HashSet};

use rand::{Rng, SeedableRng};
use serde_json::json;

use super::{ApiError, Ctx, Reply, items, ok};
use crate::db::now;

const DAY: i64 = 86_400;

pub async fn get(c: &Ctx) -> Result<Reply, ApiError> {
    let count = c.p.int("count").unwrap_or(50).clamp(1, 200) as usize;
    let uid = c.user.id.as_str();
    // the artists you play most
    let top_since = |since: i64| {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT t.artist_id, COUNT(*) n FROM play p JOIN track t ON t.id = p.track_id
             WHERE p.user_id = ? AND p.played_at >= ? AND t.missing_since IS NULL GROUP BY t.artist_id ORDER BY n DESC LIMIT 10",
        )
        .bind(uid)
        .bind(since)
        .fetch_all(&c.st.db)
    };
    let mut top = top_since(now() - 180 * DAY).await?;
    if top.len() < 3 {
        top = top_since(0).await?;
    }
    if top.is_empty() {
        top = sqlx::query_as(
            "SELECT t.artist_id, COUNT(*) FROM annotation an JOIN track t ON t.id = an.item_id
             WHERE an.user_id = ? AND an.item_kind = 'track' AND an.starred_at IS NOT NULL GROUP BY t.artist_id ORDER BY 2 DESC LIMIT 10",
        )
        .bind(uid)
        .fetch_all(&c.st.db)
        .await?;
    }
    if top.is_empty() {
        return ok(json!({ "forYou": { "song": [] } })); // nothing played or hearted yet
    }
    let total: f32 = top.iter().map(|(_, n)| *n as f32).sum();
    let mine: HashSet<&str> = top.iter().map(|(a, _)| a.as_str()).collect();
    // the artists that go with them, weighted by how much you play the one they go with
    let mut near: HashMap<String, f32> = HashMap::new();
    for (a, n) in &top {
        let share = *n as f32 / total;
        let found = crate::similar::artists(&c.st, a, &[], 12).await;
        let best = found.first().map(|x| x.1).unwrap_or(1.0).max(0.01);
        for (b, score) in found {
            if !mine.contains(b.as_str()) {
                *near.entry(b).or_default() += share * score / best;
            }
        }
    }
    // the day's mix: the same all day
    let day = now() / DAY;
    let mut rng = rand::rngs::StdRng::seed_from_u64(day as u64 ^ uid.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64)));
    let recent: HashSet<String> = sqlx::query_scalar("SELECT DISTINCT track_id FROM play WHERE user_id = ? AND played_at >= ?")
        .bind(uid)
        .bind(now() - 14 * DAY)
        .fetch_all(&c.st.db)
        .await?
        .into_iter()
        .collect();
    let songs_of = async |artist: &str| -> Result<Vec<items::TrackRow>, ApiError> {
        Ok(items::tracks_where(c, "(t.artist_id = ? OR al.artist_id = ?)", &[artist, artist], "ORDER BY t.id").await?
            .into_iter()
            .filter(|t| !recent.contains(&t.id))
            .collect())
    };
    let mut pools: Vec<(String, f32, Vec<items::TrackRow>, bool)> = vec![]; // (artist, weight, songs, one of yours)
    let mut near: Vec<(String, f32)> = near.into_iter().collect();
    near.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for (a, w) in near.into_iter().take(30) {
        pools.push((a.clone(), w, songs_of(&a).await?, false));
    }
    for (a, n) in &top {
        pools.push((a.clone(), *n as f32 / total, songs_of(a).await?, true));
    }
    for p in &mut pools {
        use rand::seq::SliceRandom;
        p.2.shuffle(&mut rng);
    }
    let mut out: Vec<items::TrackRow> = vec![];
    let mut last_artist = String::new();
    let mut tries = 0;
    while out.len() < count && tries < count * 20 {
        tries += 1;
        // 6 in 10 from the artists that go with yours, the rest from yours
        let want_new = rng.gen_bool(0.6);
        let open: Vec<usize> = (0..pools.len()).filter(|&i| !pools[i].2.is_empty() && pools[i].3 != want_new && pools[i].0 != last_artist).collect();
        let open = if open.is_empty() { (0..pools.len()).filter(|&i| !pools[i].2.is_empty() && pools[i].0 != last_artist).collect() } else { open };
        if open.is_empty() {
            break;
        }
        let sum: f32 = open.iter().map(|&i| pools[i].1).sum();
        let mut at = rng.gen_range(0.0..sum.max(f32::MIN_POSITIVE));
        let mut pick = open[open.len() - 1];
        for &i in &open {
            if at < pools[i].1 {
                pick = i;
                break;
            }
            at -= pools[i].1;
        }
        let t = pools[pick].2.pop().unwrap();
        last_artist = pools[pick].0.clone();
        if !out.iter().any(|x| x.id == t.id) {
            out.push(t);
        }
    }
    ok(json!({ "forYou": { "song": out.iter().map(items::song_json).collect::<Vec<_>>() } }))
}
