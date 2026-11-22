//! SQLite: one file in the data folder, WAL, migrations from `migrations/`.

use std::{path::Path, str::FromStr, time::Duration};

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous};

pub type Db = SqlitePool;

pub async fn open(path: &Path) -> Result<Db> {
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new().max_connections(8).connect_with(opts).await?;
    let migrator = sqlx::migrate!("./migrations");
    accept_crlf_fingerprints(&pool, &migrator).await?;
    migrator.run(&pool).await?;
    Ok(pool)
}

/// A database made by a build whose migrations had Windows line endings (a Windows checkout before .gitattributes
/// fixed them to LF, as the 0.2 Windows releases were) holds those files' fingerprints; this build would call them
/// "modified" and refuse to start. The same migration with CRLF is the same migration: its fingerprint is updated.
async fn accept_crlf_fingerprints(pool: &Db, migrator: &sqlx::migrate::Migrator) -> Result<()> {
    use sha2::{Digest, Sha384};
    let has_table: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'").fetch_one(pool).await?;
    if has_table == 0 {
        return Ok(());
    }
    for m in migrator.iter() {
        let sql: &str = m.sql.as_str();
        let crlf = sql.replace("\r\n", "\n").replace('\n', "\r\n");
        let lf = sql.replace("\r\n", "\n");
        for other in [crlf, lf] {
            let theirs = Sha384::digest(other.as_bytes()).to_vec();
            if theirs.as_slice() != m.checksum.as_ref() {
                sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ? AND checksum = ?")
                    .bind(m.checksum.as_ref())
                    .bind(m.version)
                    .bind(theirs)
                    .execute(pool)
                    .await?;
            }
        }
    }
    Ok(())
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Stable id from a kind and a key: the same file, album or artist always gets the same id.
pub fn stable_id(kind: &str, key: &str) -> String {
    format!("{:x}", md5::compute(format!("{kind}:{key}")))
}

pub fn random_id() -> String {
    use rand::Rng;
    let b: [u8; 16] = rand::thread_rng().r#gen();
    hex::encode(b)
}

/// ISO-8601 UTC time for the API ("2026-11-21T10:00:00Z").
pub fn iso(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Seconds from an ISO-8601 time (only the "YYYY-MM-DDTHH:MM:SS" part; Z assumed).
pub fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?, n(17..19)?);
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se)
}

// Howard Hinnant's date algorithms.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    #[test]
    fn iso_round_trip() {
        for t in [0, 1_763_700_000, 951_782_400] {
            assert_eq!(super::parse_iso(&super::iso(t)), Some(t));
        }
        assert_eq!(super::iso(0), "1970-01-01T00:00:00Z");
        // Navidrome's format
        assert_eq!(super::parse_iso("2026-11-12 19:17:06.918415513+00:00"), super::parse_iso("2026-11-12T19:17:06Z"));
    }
}
