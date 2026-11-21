//! Smart playlists: rules in Navidrome's format (what Encore sends), turned into SQL over the owner's library.
//!
//! ```json
//! { "all": [ {"is": {"loved": true}}, {"notInTheLast": {"lastplayed": 180}} ], "sort": "random", "limit": 100 }
//! ```
//! Groups: `all`, `any` (nested). Operators: is, isNot, gt, lt, contains, notContains, startsWith, endsWith,
//! inTheRange, before, after, inTheLast, notInTheLast, inPlaylist, notInPlaylist.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;

use super::AppState;
use crate::db::{self, now};

#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    Text(String),
    Int(i64),
    Real(f64),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Text,
    Number,
    Bool,
    Date,
}

/// Field name → (SQL over `t` track, `al` album, `an` the owner's annotation, kind).
fn field(name: &str) -> Option<(&'static str, Kind)> {
    Some(match name.to_ascii_lowercase().as_str() {
        "title" => ("t.title", Kind::Text),
        "artist" => ("t.artist_name", Kind::Text),
        "albumartist" => ("al.artist_name", Kind::Text),
        "album" => ("t.album_name", Kind::Text),
        "genre" => ("COALESCE(t.genre, '')", Kind::Text),
        "filetype" => ("t.suffix", Kind::Text),
        "year" => ("COALESCE(t.year, 0)", Kind::Number),
        "rating" => ("COALESCE(an.rating, 0)", Kind::Number),
        "playcount" => ("COALESCE(an.play_count, 0)", Kind::Number),
        "duration" => ("(t.duration_ms / 1000)", Kind::Number),
        "bitrate" => ("t.bitrate", Kind::Number),
        "tracknumber" => ("COALESCE(t.number, 0)", Kind::Number),
        "discnumber" => ("COALESCE(t.disc, 0)", Kind::Number),
        "loved" => ("(an.starred_at IS NOT NULL)", Kind::Bool),
        "compilation" => ("al.compilation", Kind::Bool),
        "lastplayed" => ("an.last_played", Kind::Date),
        "dateadded" => ("t.created_at", Kind::Date),
        "dateloved" => ("an.starred_at", Kind::Date),
        _ => return None,
    })
}

/// The columns to sort by for a sort name (several for "album": disc and track order inside it).
fn sort_expr(name: &str) -> Option<&'static [&'static str]> {
    Some(match name.to_ascii_lowercase().as_str() {
        "random" => &["RANDOM()"],
        "playcount" => &["COALESCE(an.play_count, 0)"],
        "lastplayed" => &["COALESCE(an.last_played, 0)"],
        "dateadded" => &["t.created_at"],
        "dateloved" => &["COALESCE(an.starred_at, 0)"],
        "rating" => &["COALESCE(an.rating, 0)"],
        "title" => &["t.sort_title"],
        "artist" => &["t.artist_name COLLATE NOCASE"],
        "albumartist" => &["al.artist_name COLLATE NOCASE"],
        "album" => &["t.album_name COLLATE NOCASE", "t.disc", "t.number"],
        "year" => &["COALESCE(t.year, 0)"],
        "duration" => &["t.duration_ms"],
        "bitrate" => &["t.bitrate"],
        "genre" => &["t.genre COLLATE NOCASE"],
        _ => return None,
    })
}

fn date_value(v: &Value) -> Result<i64> {
    match v {
        Value::Number(n) => n.as_i64().ok_or_else(|| anyhow!("bad date")),
        Value::String(s) => {
            let s = s.trim();
            let full = if s.len() == 10 { format!("{s}T00:00:00Z") } else { s.to_string() };
            db::parse_iso(&full).ok_or_else(|| anyhow!("bad date: {s}"))
        }
        _ => bail!("bad date"),
    }
}

fn num(v: &Value) -> Result<Bind> {
    match v {
        Value::Number(n) => Ok(n.as_i64().map(Bind::Int).unwrap_or_else(|| Bind::Real(n.as_f64().unwrap_or(0.0)))),
        Value::String(s) => s
            .trim()
            .parse::<i64>()
            .map(Bind::Int)
            .or_else(|_| s.trim().parse::<f64>().map(Bind::Real))
            .map_err(|_| anyhow!("not a number: {s}")),
        Value::Bool(b) => Ok(Bind::Int(*b as i64)),
        _ => bail!("not a number"),
    }
}

fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn like_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// One condition like `{"contains": {"title": "love"}}`, or a nested group.
fn condition(c: &Value, out: &mut Vec<Bind>) -> Result<String> {
    let o = c.as_object().ok_or_else(|| anyhow!("a rule must be an object"))?;
    if o.len() != 1 {
        bail!("a rule must have one operator");
    }
    let (op, body) = o.iter().next().unwrap();
    if op == "all" || op == "any" {
        return group(op, body, out);
    }
    if op == "inPlaylist" || op == "notInPlaylist" {
        let id = body.get("id").map(text).ok_or_else(|| anyhow!("{op} needs an id"))?;
        out.push(Bind::Text(id));
        let not = if op == "notInPlaylist" { "NOT " } else { "" };
        return Ok(format!("t.id {not}IN (SELECT track_id FROM playlist_entry WHERE playlist_id = ?)"));
    }
    let b = body.as_object().filter(|b| b.len() == 1).ok_or_else(|| anyhow!("{op} needs one field"))?;
    let (fname, v) = b.iter().next().unwrap();
    let (col, kind) = field(fname).ok_or_else(|| anyhow!("unknown field: {fname}"))?;
    let text_col = format!("{col} COLLATE NOCASE");
    let sql = match (op.as_str(), kind) {
        ("is", Kind::Text) => {
            out.push(Bind::Text(text(v)));
            format!("{text_col} = ?")
        }
        ("isNot", Kind::Text) => {
            out.push(Bind::Text(text(v)));
            format!("{text_col} != ?")
        }
        ("contains" | "notContains" | "startsWith" | "endsWith", Kind::Text) => {
            let s = like_escape(&text(v));
            let pat = match op.as_str() {
                "startsWith" => format!("{s}%"),
                "endsWith" => format!("%{s}"),
                _ => format!("%{s}%"),
            };
            out.push(Bind::Text(pat));
            let not = if op == "notContains" { "NOT " } else { "" };
            format!("{col} {not}LIKE ? ESCAPE '\\'")
        }
        ("is" | "isNot", Kind::Bool) => {
            let want = matches!(v, Value::Bool(true)) || matches!(v, Value::String(s) if s == "true") || v.as_i64() == Some(1);
            let want = if op == "isNot" { !want } else { want };
            format!("({col}) = {}", if want { 1 } else { 0 })
        }
        ("is" | "isNot" | "gt" | "lt", Kind::Number) => {
            out.push(num(v)?);
            let sym = match op.as_str() {
                "is" => "=",
                "isNot" => "!=",
                "gt" => ">",
                _ => "<",
            };
            format!("{col} {sym} ?")
        }
        ("inTheRange", Kind::Number) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| anyhow!("inTheRange needs two values"))?;
            out.push(num(&a[0])?);
            out.push(num(&a[1])?);
            format!("{col} BETWEEN ? AND ?")
        }
        ("inTheRange", Kind::Date) => {
            let a = v.as_array().filter(|a| a.len() == 2).ok_or_else(|| anyhow!("inTheRange needs two dates"))?;
            out.push(Bind::Int(date_value(&a[0])?));
            out.push(Bind::Int(date_value(&a[1])? + 86_399));
            format!("{col} BETWEEN ? AND ?")
        }
        ("inTheLast", Kind::Date) => {
            out.push(Bind::Int(now() - num_days(v)? * 86_400));
            format!("{col} >= ?")
        }
        ("notInTheLast", Kind::Date) => {
            out.push(Bind::Int(now() - num_days(v)? * 86_400));
            format!("({col} IS NULL OR {col} < ?)")
        }
        ("before", Kind::Date) => {
            out.push(Bind::Int(date_value(v)?));
            format!("{col} < ?")
        }
        ("after", Kind::Date) => {
            out.push(Bind::Int(date_value(v)? + 86_399));
            format!("{col} > ?")
        }
        _ => bail!("{op} doesn't work with {fname}"),
    };
    Ok(sql)
}

fn num_days(v: &Value) -> Result<i64> {
    match num(v)? {
        Bind::Int(i) => Ok(i),
        Bind::Real(f) => Ok(f as i64),
        Bind::Text(_) => bail!("not a number of days"),
    }
}

fn group(op: &str, body: &Value, out: &mut Vec<Bind>) -> Result<String> {
    let items = body.as_array().ok_or_else(|| anyhow!("{op} needs a list"))?;
    if items.is_empty() {
        return Ok("1".into());
    }
    let parts: Vec<String> = items.iter().map(|c| condition(c, out)).collect::<Result<_>>()?;
    Ok(format!("({})", parts.join(if op == "all" { " AND " } else { " OR " })))
}

/// The WHERE, ORDER BY and LIMIT for a set of rules, with the values to bind (in order).
pub fn compile(rules: &Value) -> Result<(String, String, Vec<Bind>)> {
    let mut binds = vec![];
    let r = rules.as_object().ok_or_else(|| anyhow!("rules must be an object"))?;
    let mut conds = vec![];
    for g in ["all", "any"] {
        if let Some(b) = r.get(g) {
            conds.push(group(g, b, &mut binds)?);
        }
    }
    let wh = if conds.is_empty() { "1".to_string() } else { conds.join(" AND ") };

    let order_word = if r.get("order").and_then(Value::as_str) == Some("desc") { "DESC" } else { "ASC" };
    let mut order = vec![];
    for part in r.get("sort").and_then(Value::as_str).unwrap_or("random").split(',') {
        let p = part.trim();
        let (name, word) = match p.strip_prefix('-') {
            Some(n) => (n, "DESC"),
            None => (p.trim_start_matches('+'), order_word),
        };
        if let Some(cols) = sort_expr(name) {
            order.extend(cols.iter().map(|c| if *c == "RANDOM()" { c.to_string() } else { format!("{c} {word}") }));
        }
    }
    if order.is_empty() {
        order.push("RANDOM()".into());
    }
    let limit = r.get("limit").and_then(Value::as_i64).filter(|l| *l > 0).unwrap_or(10_000).min(10_000);
    let offset = r.get("offset").and_then(Value::as_i64).unwrap_or(0).max(0);
    Ok((wh, format!("ORDER BY {} LIMIT {limit} OFFSET {offset}", order.join(", ")), binds))
}

/// The songs a smart playlist holds right now, for its owner.
pub async fn evaluate(st: &AppState, owner: &str, rules: &Value) -> Result<Vec<String>> {
    let (wh, tail, binds) = compile(rules)?;
    let sql = format!(
        "SELECT t.id FROM track t JOIN album al ON al.id = t.album_id
         LEFT JOIN annotation an ON an.user_id = ? AND an.item_kind = 'track' AND an.item_id = t.id
         WHERE t.missing_since IS NULL AND {wh} {tail}"
    );
    let mut q = sqlx::query_scalar::<_, String>(&sql).bind(owner);
    for b in binds {
        q = match b {
            Bind::Text(s) => q.bind(s),
            Bind::Int(i) => q.bind(i),
            Bind::Real(f) => q.bind(f),
        };
    }
    Ok(q.fetch_all(&st.db).await?)
}

/// Re-fill a smart playlist's songs when they're older than `max_age` seconds (0: always).
pub async fn refresh(st: &AppState, pid: &str, max_age: i64) -> Result<()> {
    let row: Option<(String, Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT owner_id, rules_json, evaluated_at FROM playlist WHERE id = ?")
            .bind(pid)
            .fetch_optional(&st.db)
            .await?;
    let Some((owner, Some(rules), at)) = row else { return Ok(()) };
    if max_age > 0 && at.is_some_and(|a| now() - a < max_age) {
        return Ok(());
    }
    let rules: Value = serde_json::from_str(&rules)?;
    let ids = evaluate(st, &owner, &rules).await?;
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM playlist_entry WHERE playlist_id = ?").bind(pid).execute(&mut *tx).await?;
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("INSERT INTO playlist_entry (playlist_id, position, track_id) VALUES (?, ?, ?)")
            .bind(pid)
            .bind(i as i64)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("UPDATE playlist SET evaluated_at = ? WHERE id = ?").bind(now()).bind(pid).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Smart playlists are re-filled when they're read and older than this.
pub const MAX_AGE: i64 = 60;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compiles_encore_presets() {
        let (w, t, b) = compile(&json!({
            "all": [{"is": {"loved": true}}, {"notInTheLast": {"lastplayed": 180}}, {"any": [{"contains": {"title": "50%"}}, {"gt": {"playcount": "3"}}]}],
            "sort": "playcount", "order": "desc", "limit": 100
        }))
        .unwrap();
        assert!(
            w.starts_with(
                "(((an.starred_at IS NOT NULL)) = 1 AND (an.last_played IS NULL OR an.last_played < ?) AND (t.title LIKE ? ESCAPE"
            ),
            "{w}"
        );
        assert_eq!(t, "ORDER BY COALESCE(an.play_count, 0) DESC LIMIT 100 OFFSET 0");
        assert_eq!(b[1], Bind::Text("%50\\%%".into()));
        assert_eq!(b[2], Bind::Int(3));
        assert!(compile(&json!({"all": [{"is": {"nope": 1}}]})).is_err());
        let (_, t, _) = compile(&json!({"sort": "-year,title"})).unwrap();
        assert_eq!(t, "ORDER BY COALESCE(t.year, 0) DESC, t.sort_title ASC LIMIT 10000 OFFSET 0");
    }
}
