//! Users over the Subsonic API: `getUser` `getUsers` `createUser` `updateUser` `deleteUser` `changePassword`
//! `getAvatar`. Conductor has two kinds of user, admins and everyone else, so the roles follow from that: everyone
//! may stream, download, make playlists and share; admins also manage users and podcasts. Changing other people is
//! for admins; there's always at least one admin left.

use axum::http::{StatusCode, header};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};

use super::{ApiError, Ctx, Reply, empty, ok, raw};
use crate::auth;

fn user_json(c: &Ctx, name: &str, admin: bool) -> Value {
    let folders: Vec<i64> = c.st.libs.iter().map(|(id, _)| *id).collect();
    json!({
        "username": name, "email": "", "scrobblingEnabled": true, "adminRole": admin, "settingsRole": true,
        "downloadRole": true, "uploadRole": false, "playlistRole": true, "coverArtRole": true, "commentRole": false,
        "podcastRole": admin, "streamRole": true, "jukeboxRole": false, "shareRole": c.st.cfg.jam_public_url.is_some(),
        "videoConversionRole": false, "folder": folders,
    })
}

fn need_admin(c: &Ctx) -> Result<(), ApiError> {
    if c.user.is_admin { Ok(()) } else { Err(ApiError::new(50, "Only admins can manage users")) }
}

/// `password`, plain or `enc:` + hex (Subsonic).
fn password(c: &Ctx) -> Result<String, ApiError> {
    let p = c.p.req("password")?;
    let p = match p.strip_prefix("enc:") {
        Some(h) => hex::decode(h).ok().and_then(|b| String::from_utf8(b).ok()).ok_or_else(|| ApiError::new(10, "password: bad enc: value"))?,
        None => p.to_string(),
    };
    if p.chars().count() < 6 {
        return Err(ApiError::new(10, "The password needs at least 6 characters"));
    }
    Ok(p)
}

async fn find(c: &Ctx, name: &str) -> Result<(String, String, bool), ApiError> {
    sqlx::query_as("SELECT id, name, is_admin FROM user WHERE name = ?")
        .bind(name)
        .fetch_optional(&c.st.db)
        .await?
        .ok_or_else(|| ApiError::not_found("User"))
}

async fn admins(c: &Ctx) -> Result<i64, ApiError> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM user WHERE is_admin = 1").fetch_one(&c.st.db).await?)
}

pub async fn get_user(c: &Ctx) -> Result<Reply, ApiError> {
    let name = c.p.get("username").unwrap_or(&c.user.name);
    if !name.eq_ignore_ascii_case(&c.user.name) && !c.user.is_admin {
        return Err(ApiError::new(50, "Only admins can see other users"));
    }
    let (_, name, admin) = find(c, name).await?;
    ok(json!({ "user": user_json(c, &name, admin) }))
}

pub async fn get_users(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let rows: Vec<(String, bool)> =
        sqlx::query_as("SELECT name, is_admin FROM user ORDER BY name COLLATE NOCASE").fetch_all(&c.st.db).await?;
    let list: Vec<Value> = rows.iter().map(|(n, a)| user_json(c, n, *a)).collect();
    ok(json!({ "users": { "user": list } }))
}

pub async fn create_user(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let name = c.p.req("username")?.trim().to_string();
    if name.is_empty() || name.chars().count() > 64 || name.chars().any(char::is_control) {
        return Err(ApiError::new(10, "username: 1 to 64 characters"));
    }
    let pass = password(c)?;
    if find(c, &name).await.is_ok() {
        return Err(ApiError::new(0, "There's already a user with that name"));
    }
    let admin = c.p.bool("adminRole").unwrap_or(false);
    auth::create_user(&c.st.db, &c.st.secrets, &name, &pass, admin).await?;
    tracing::info!(user = %name, admin, by = %c.user.name, "user created");
    empty()
}

pub async fn update_user(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let (id, name, was_admin) = find(c, c.p.req("username")?).await?;
    if c.p.get("password").is_some() {
        let pass = password(c)?;
        auth::set_password(&c.st.db, &c.st.secrets, &name, &pass).await?;
    }
    if let Some(admin) = c.p.bool("adminRole")
        && admin != was_admin
    {
        if !admin && admins(c).await? <= 1 {
            return Err(ApiError::new(50, "That's the only admin: make someone else an admin first"));
        }
        sqlx::query("UPDATE user SET is_admin = ? WHERE id = ?").bind(admin).bind(&id).execute(&c.st.db).await?;
    }
    empty()
}

pub async fn delete_user(c: &Ctx) -> Result<Reply, ApiError> {
    need_admin(c)?;
    let (id, name, admin) = find(c, c.p.req("username")?).await?;
    if id == c.user.id {
        return Err(ApiError::new(50, "You can't delete yourself"));
    }
    if admin && admins(c).await? <= 1 {
        return Err(ApiError::new(50, "That's the only admin"));
    }
    sqlx::query("DELETE FROM user WHERE id = ?").bind(&id).execute(&c.st.db).await?;
    tracing::info!(user = %name, by = %c.user.name, "user deleted");
    empty()
}

pub async fn change_password(c: &Ctx) -> Result<Reply, ApiError> {
    let name = c.p.req("username")?;
    if !name.eq_ignore_ascii_case(&c.user.name) && !c.user.is_admin {
        return Err(ApiError::new(50, "Only admins can change someone else's password"));
    }
    let (_, name, _) = find(c, name).await?;
    let pass = password(c)?;
    auth::set_password(&c.st.db, &c.st.secrets, &name, &pass).await?;
    empty()
}

/// Conductor keeps no pictures of people: a plain one, its colour from the name, so each user looks different.
pub async fn get_avatar(c: &Ctx) -> Result<Reply, ApiError> {
    let name = c.p.get("username").unwrap_or(&c.user.name);
    let (_, name, _) = find(c, name).await?;
    let png = avatar(&name.to_lowercase());
    Ok(raw(StatusCode::OK, &[(header::CONTENT_TYPE, "image/png".into()), (header::CACHE_CONTROL, "private, max-age=86400".into())], png.into()))
}

pub fn avatar(seed: &str) -> Vec<u8> {
    let h = Sha1::digest(seed.as_bytes());
    let hue = (u16::from(h[0]) << 8 | u16::from(h[1])) as f32 / 65535.0 * 360.0;
    let rgb = |l: f32| -> image::Rgb<u8> {
        // HSL with saturation .45
        let s = 0.45;
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((hue / 60.0) % 2.0 - 1.0).abs());
        let (r, g, b) = match (hue / 60.0) as u32 {
            0 => (c, x, 0.0),
            1 => (x, c, 0.0),
            2 => (0.0, c, x),
            3 => (0.0, x, c),
            4 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };
        let m = l - c / 2.0;
        image::Rgb([((r + m) * 255.0) as u8, ((g + m) * 255.0) as u8, ((b + m) * 255.0) as u8])
    };
    let (bg, fg) = (rgb(0.30), rgb(0.72));
    let n = 128u32;
    let img = image::RgbImage::from_fn(n, n, |x, y| {
        let (x, y) = (x as f32, y as f32);
        let head = (x - 64.0).powi(2) + (y - 50.0).powi(2) <= 22.0f32.powi(2);
        let body = y >= 82.0 && ((x - 64.0) / 42.0).powi(2) + ((y - 128.0) / 44.0).powi(2) <= 1.0;
        if head || body { fg } else { bg }
    });
    let mut out = std::io::Cursor::new(Vec::new());
    let _ = img.write_to(&mut out, image::ImageFormat::Png);
    out.into_inner()
}
