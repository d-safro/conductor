//! Backups: everything Conductor needs to come back, in one zip.
//!
//!   conductor backup [FILE]           a backup now (default: conductor-backup-<date>.zip here)
//!   conductor restore FILE [--force]  put one back (with --force, what's there is moved aside first, never deleted)
//!
//! Conductor also makes one by itself once a day in `<data>/backups` (or `backup_dir`), keeping the newest
//! `backup_keep` (7; 0: none).
//!
//! In the zip: `conductor.db` (a consistent copy, made while the server runs), `secret.key` (without it the saved
//! passwords can't be read), `config.toml` and `secrets.toml`, the playlist pictures, and `backup.json` (what's in
//! it, from which version). The zip holds secrets, so it's readable only by you. Music and the cache aren't in it.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::json;
use tracing::{info, warn};

use crate::{config::Config, db::Db};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn stamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
}

pub fn default_name() -> PathBuf {
    PathBuf::from(format!("conductor-backup-{}.zip", stamp()))
}

pub fn dir(cfg: &Config) -> PathBuf {
    cfg.backup_dir.clone().unwrap_or_else(|| cfg.data_dir.join("backups"))
}

fn settings_files(cfg: &Config) -> Vec<(String, PathBuf)> {
    let mut out = vec![];
    if let Some(p) = &cfg.config_path {
        out.push(("config.toml".to_string(), p.clone()));
        out.push(("secrets.toml".to_string(), crate::config::secrets_path(p)));
    }
    out
}

/// A backup to `out`. Returns how big it is.
pub async fn make(cfg: &Config, db: &Db, out: &Path) -> Result<u64> {
    let tmp = cfg.data_dir.join(format!(".backup-{}", crate::db::random_id()));
    std::fs::create_dir_all(&tmp)?;
    let res = async {
        let snap = tmp.join("conductor.db");
        sqlx::query("VACUUM INTO ?").bind(snap.to_string_lossy().to_string()).execute(db).await.context("copying the database")?;
        let mut files: Vec<(String, PathBuf)> = vec![("conductor.db".into(), snap)];
        let key = cfg.data_dir.join("secret.key");
        if key.is_file() {
            files.push(("secret.key".into(), key));
        }
        files.extend(settings_files(cfg).into_iter().filter(|(_, p)| p.is_file()));
        let pics = cfg.data_dir.join("playlist-images");
        if let Ok(rd) = std::fs::read_dir(&pics) {
            for e in rd.flatten() {
                if e.path().is_file() {
                    files.push((format!("playlist-images/{}", e.file_name().to_string_lossy()), e.path()));
                }
            }
        }
        write_zip(out, &files)
    }
    .await;
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

fn write_zip(out: &Path, files: &[(String, PathBuf)]) -> Result<u64> {
    if let Some(d) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d)?;
    }
    let part = out.with_extension("zip.part");
    {
        let f = private_file(&part)?;
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        let manifest = json!({ "conductor": VERSION, "made": chrono::Utc::now().to_rfc3339(), "files": names });
        z.start_file("backup.json", opts)?;
        z.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
        for (name, path) in files {
            z.start_file(name.as_str(), opts)?;
            let mut f = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
            std::io::copy(&mut f, &mut z)?;
        }
        z.finish()?;
    }
    std::fs::rename(&part, out)?;
    Ok(std::fs::metadata(out)?.len())
}

fn private_file(p: &Path) -> Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(p).with_context(|| format!("writing {}", p.display()))
}

/// What a restore did.
#[derive(Debug, Default)]
pub struct Restored {
    pub version: String,
    pub files: Vec<String>,
    /// where what was there before went
    pub moved_aside: Option<PathBuf>,
}

/// Put a backup back. Conductor must not be running. Without `force` it won't replace a database that's there.
pub fn restore(cfg: &Config, zip_path: &Path, force: bool) -> Result<Restored> {
    let f = std::fs::File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
    let mut z = zip::ZipArchive::new(f).context("that isn't a Conductor backup (not a zip)")?;
    let manifest: serde_json::Value = {
        let mut m = z.by_name("backup.json").context("that isn't a Conductor backup (no backup.json)")?;
        let mut s = String::new();
        m.read_to_string(&mut s)?;
        serde_json::from_str(&s)?
    };
    let version = manifest["conductor"].as_str().unwrap_or("?").to_string();
    if z.by_name("conductor.db").is_err() {
        bail!("the backup has no database");
    }
    if newer(&version, VERSION) {
        bail!("this backup is from Conductor {version}, newer than this one ({VERSION}): restore it with {version} or later");
    }
    if std::net::TcpListener::bind(cfg.listen).is_err() {
        bail!("Conductor seems to be running ({} is in use): stop it first", cfg.listen);
    }
    let settings: Vec<(String, PathBuf)> = settings_files(cfg);
    let dest = |name: &str| -> Option<PathBuf> {
        if let Some((_, p)) = settings.iter().find(|(n, _)| n == name) {
            return Some(p.clone());
        }
        match name {
            "conductor.db" | "secret.key" => Some(cfg.data_dir.join(name)),
            n if n.starts_with("playlist-images/") => {
                let file = n.trim_start_matches("playlist-images/");
                // a plain file name only: nothing outside the folder
                (!file.is_empty() && !file.contains(['/', '\\']) && file != ".." && file != ".")
                    .then(|| cfg.data_dir.join("playlist-images").join(file))
            }
            _ => None, // config.toml without a settings file in use, or anything unknown: left out
        }
    };
    let db = cfg.data_dir.join("conductor.db");
    let mut out = Restored { version, ..Default::default() };
    if db.is_file() {
        if !force {
            bail!("there's already a database here ({}); --force moves what's there aside first", db.display());
        }
        let aside = cfg.data_dir.join(format!("before-restore-{}", stamp()));
        std::fs::create_dir_all(&aside)?;
        let mut moving: Vec<PathBuf> = ["conductor.db", "conductor.db-wal", "conductor.db-shm", "secret.key", "playlist-images"]
            .iter()
            .map(|n| cfg.data_dir.join(n))
            .collect();
        moving.extend(settings.iter().map(|(_, p)| p.clone()));
        let mut moved: Vec<(PathBuf, PathBuf)> = vec![];
        for p in moving.into_iter().filter(|p| p.exists()) {
            let to = aside.join(p.file_name().map(|n| n.to_owned()).unwrap_or_default());
            if let Err(e) = rename_patiently(&p, &to) {
                // put back what was moved: all or nothing
                for (from, to) in moved.iter().rev() {
                    let _ = rename_patiently(to, from);
                }
                let _ = std::fs::remove_dir(&aside);
                return Err(anyhow::Error::new(e).context(format!("moving {} aside (is Conductor still running?)", p.display())));
            }
            moved.push((p, to));
        }
        out.moved_aside = Some(aside);
    }
    std::fs::create_dir_all(&cfg.data_dir)?;
    for i in 0..z.len() {
        let mut e = z.by_index(i)?;
        let name = e.name()?.to_string();
        let Some(to) = dest(&name) else { continue };
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = if name == "secret.key" || name == "secrets.toml" { private_file(&to)? } else { std::fs::File::create(&to)? };
        std::io::copy(&mut e, &mut f).with_context(|| format!("writing {}", to.display()))?;
        out.files.push(name);
    }
    Ok(out)
}

/// A rename, tried for a few seconds: on Windows a file another program just let go of (or a virus scanner is
/// reading) can't be moved for a moment.
fn rename_patiently(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(e) if tries < 25 && e.kind() != std::io::ErrorKind::NotFound => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Is version `a` newer than `b` (x.y.z)?
fn newer(a: &str, b: &str) -> bool {
    let p = |v: &str| v.split('.').map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    p(a) > p(b)
}

/// Once a day, a backup in the backups folder; the newest `backup_keep` kept.
pub async fn worker(cfg: Config, db: Db) {
    if cfg.backup_keep == 0 {
        return;
    }
    tokio::time::sleep(Duration::from_secs(10 * 60)).await;
    loop {
        let d = dir(&cfg);
        let out = d.join(format!("conductor-{}.zip", stamp()));
        match make(&cfg, &db, &out).await {
            Ok(n) => info!(file = %out.display(), bytes = n, "backup made"),
            Err(e) => warn!("backup failed: {e:#}"),
        }
        prune(&d, cfg.backup_keep);
        tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
    }
}

/// Keep the newest `keep` of the backups this worker made (conductor-*.zip); anything else there is left alone.
pub fn prune(d: &Path, keep: usize) {
    let mut mine: Vec<PathBuf> = std::fs::read_dir(d)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.starts_with("conductor-") && n.ends_with(".zip") && !n.starts_with("conductor-backup-")
        })
        .collect();
    mine.sort();
    while mine.len() > keep {
        let p = mine.remove(0);
        let _ = std::fs::remove_file(&p);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::newer("0.10.0", "0.9.9"));
        assert!(!super::newer("0.2.1", "0.3.0"));
        assert!(!super::newer("0.3.0", "0.3.0"));
    }
}
