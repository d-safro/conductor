//! Users and login. Subsonic's token login (t = md5(password + salt)) needs the password itself, so passwords
//! are stored encrypted (AES-256-GCM) with a key kept in its own file next to the database, never in it.

use std::path::Path;

use aes_gcm::{
    Aes256Gcm, Key, Nonce,
    aead::{Aead, KeyInit},
};
use anyhow::{Context, Result, anyhow, bail};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::db::{self, Db};

#[derive(Clone)]
pub struct Secrets {
    cipher: Aes256Gcm,
}

impl Secrets {
    /// Load the key from `<data>/secret.key`, creating it (owner-only on Unix) the first time.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let key = if path.exists() {
            let k = hex::decode(std::fs::read_to_string(path)?.trim()).context("secret.key")?;
            if k.len() != 32 {
                bail!("secret.key must hold 32 bytes");
            }
            k
        } else {
            let mut k = vec![0u8; 32];
            rand::thread_rng().fill_bytes(&mut k);
            crate::config::write_private(path, hex::encode(&k).as_bytes()).with_context(|| format!("writing {}", path.display()))?;
            k
        };
        Ok(Self { cipher: Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key)) })
    }

    pub fn encrypt(&self, plain: &str) -> String {
        let mut nonce = [0u8; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let ct = self.cipher.encrypt(Nonce::from_slice(&nonce), plain.as_bytes()).expect("encrypt");
        format!("{}:{}", hex::encode(nonce), hex::encode(ct))
    }

    pub fn decrypt(&self, enc: &str) -> Result<String> {
        let (n, c) = enc.split_once(':').ok_or_else(|| anyhow!("bad password record"))?;
        let pt = self
            .cipher
            .decrypt(Nonce::from_slice(&hex::decode(n)?), hex::decode(c)?.as_ref())
            .map_err(|_| anyhow!("password record can't be decrypted (wrong secret.key?)"))?;
        Ok(String::from_utf8(pt)?)
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: String,
    pub name: String,
    pub password_enc: String,
    pub is_admin: bool,
}

pub async fn create_user(db: &Db, s: &Secrets, name: &str, password: &str, admin: bool) -> Result<String> {
    let id = db::random_id();
    sqlx::query("INSERT INTO user (id, name, password_enc, is_admin, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(name)
        .bind(s.encrypt(password))
        .bind(admin)
        .bind(db::now())
        .execute(db)
        .await?;
    Ok(id)
}

/// The first account (an admin), made only while there's no account at all: of two sent at once, only one is
/// made. None: someone has an account already.
pub async fn create_first_admin(db: &Db, s: &Secrets, name: &str, password: &str) -> Result<Option<String>> {
    let id = db::random_id();
    let r = sqlx::query(
        "INSERT INTO user (id, name, password_enc, is_admin, created_at) SELECT ?, ?, ?, 1, ? WHERE NOT EXISTS (SELECT 1 FROM user WHERE is_admin = 1)",
    )
    .bind(&id)
    .bind(name)
    .bind(s.encrypt(password))
    .bind(db::now())
    .execute(db)
    .await?;
    Ok((r.rows_affected() > 0).then_some(id))
}

pub async fn set_password(db: &Db, s: &Secrets, name: &str, password: &str) -> Result<bool> {
    let r =
        sqlx::query("UPDATE user SET password_enc = ? WHERE name = ?").bind(s.encrypt(password)).bind(name).execute(db).await?;
    Ok(r.rows_affected() > 0)
}

pub async fn user_by_name(db: &Db, name: &str) -> Result<Option<User>> {
    Ok(sqlx::query_as("SELECT id, name, password_enc, is_admin FROM user WHERE name = ?").bind(name).fetch_optional(db).await?)
}

/// The user, if this is their password (for sign-ins that send the password itself).
/// Wrong passwords, counted per user per address. Past the limit, that user is refused from that address until
/// the oldest wrong try is `THROTTLE_MS` old. A right password from there clears the count, so a device with an
/// old saved password can't lock out the rest.
#[derive(Default)]
pub struct Throttle(std::sync::Mutex<std::collections::HashMap<String, Vec<i64>>>);

pub const THROTTLE_MS: i64 = 15 * 60_000;
/// Wrong tries allowed per 15 minutes on the admin page, and from apps (which retry more on their own).
pub const ADMIN_TRIES: usize = 10;
pub const APP_TRIES: usize = 30;

impl Throttle {
    fn key(scope: &str, ip: &str, user: &str) -> String {
        format!("{scope}\n{ip}\n{}", user.to_lowercase())
    }
    pub fn blocked(&self, scope: &str, ip: &str, user: &str, max: usize) -> bool {
        let now = db::now_ms();
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match g.get_mut(&Self::key(scope, ip, user)) {
            Some(v) => {
                v.retain(|t| now - t < THROTTLE_MS);
                v.len() >= max
            }
            None => false,
        }
    }
    pub fn failed(&self, scope: &str, ip: &str, user: &str) {
        let now = db::now_ms();
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.entry(Self::key(scope, ip, user)).or_default().push(now);
        if g.len() > 10_000 {
            g.retain(|_, v| v.last().is_some_and(|t| now - t < THROTTLE_MS));
        }
    }
    pub fn clear(&self, scope: &str, ip: &str, user: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(&Self::key(scope, ip, user));
    }
}

pub async fn verify_password(db: &Db, s: &Secrets, name: &str, password: &str) -> Result<Option<User>> {
    let Some(u) = user_by_name(db, name).await? else { return Ok(None) };
    let real = s.decrypt(&u.password_enc)?;
    Ok(if constant_eq(&real, password) { Some(u) } else { None })
}

pub async fn user_count(db: &Db) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM user").fetch_one(db).await?)
}

/// Admins. With none, the first one can still be made (from the environment, /setup or Prelude), even when a
/// service's own account (AudioMuse's, which Prelude makes) is already there.
pub async fn admin_count(db: &Db) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT COUNT(*) FROM user WHERE is_admin = 1").fetch_one(db).await?)
}

/// A new API key for a user (OpenSubsonic apiKeyAuthentication). Only its hash is stored.
pub async fn create_api_key(db: &Db, user: &str, label: &str) -> Result<String> {
    let u = user_by_name(db, user).await?.ok_or_else(|| anyhow!("no user {user}"))?;
    let mut b = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut b);
    let key = format!("enc_{}", hex::encode(b));
    sqlx::query("INSERT INTO api_key (hash, user_id, label, created_at) VALUES (?, ?, ?, ?)")
        .bind(sha256(&key))
        .bind(&u.id)
        .bind(label)
        .bind(db::now())
        .execute(db)
        .await?;
    Ok(key)
}

pub fn sha256(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Why a login failed, as Subsonic error codes.
#[derive(Debug, PartialEq)]
pub enum LoginError {
    Missing,     // 10
    Wrong,       // 40
    BothMethods, // 43
    InvalidKey,  // 44
}

pub struct Credentials<'a> {
    pub u: Option<&'a str>,
    pub p: Option<&'a str>,
    pub t: Option<&'a str>,
    pub s: Option<&'a str>,
    pub api_key: Option<&'a str>,
}

pub async fn login(db: &Db, secrets: &Secrets, c: Credentials<'_>) -> Result<User, LoginError> {
    if let Some(key) = c.api_key {
        if c.u.is_some() || c.p.is_some() || c.t.is_some() {
            return Err(LoginError::BothMethods);
        }
        let h = sha256(key);
        let u: Option<User> = sqlx::query_as(
            "SELECT u.id, u.name, u.password_enc, u.is_admin FROM api_key k JOIN user u ON u.id = k.user_id WHERE k.hash = ?",
        )
        .bind(&h)
        .fetch_optional(db)
        .await
        .map_err(|_| LoginError::InvalidKey)?;
        let u = u.ok_or(LoginError::InvalidKey)?;
        let _ = sqlx::query("UPDATE api_key SET last_used_at = ? WHERE hash = ?").bind(db::now()).bind(&h).execute(db).await;
        return Ok(u);
    }
    let name = c.u.ok_or(LoginError::Missing)?;
    let user = user_by_name(db, name).await.ok().flatten().ok_or(LoginError::Wrong)?;
    let password = secrets.decrypt(&user.password_enc).map_err(|_| LoginError::Wrong)?;
    let ok = match (c.t, c.s, c.p) {
        (Some(t), Some(s), _) => constant_eq(&format!("{:x}", md5::compute(format!("{password}{s}"))), &t.to_ascii_lowercase()),
        (_, _, Some(p)) => {
            let p = match p.strip_prefix("enc:") {
                Some(h) => hex::decode(h).ok().and_then(|b| String::from_utf8(b).ok()).unwrap_or_default(),
                None => p.to_string(),
            };
            constant_eq(&p, &password)
        }
        _ => return Err(LoginError::Missing),
    };
    if ok { Ok(user) } else { Err(LoginError::Wrong) }
}

pub(crate) fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
