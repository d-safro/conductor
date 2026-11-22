//! Maestro: a music server for Encore and any Subsonic / OpenSubsonic app.
//!
//!   maestro [--config FILE] [serve]          run the server (default)
//!   maestro [--config FILE] scan [--full]     scan the libraries once and exit
//!   maestro [--config FILE] user add NAME [--admin]     password from MAESTRO_PASSWORD or stdin
//!   maestro [--config FILE] user passwd NAME
//!   maestro [--config FILE] apikey NAME [LABEL]          prints a new API key
//!   maestro [--config FILE] import-navidrome NAVIDROME.DB [--artwork DIR]   (use a copy of the database)
//!   maestro [--config FILE] service install|uninstall|start|stop   (Windows: run as a service)
//!   maestro [--config FILE] import-music-lookup DATA [STATE]  (play log, Encore's sync values, mixes, shares,
//!                                                         concerts; podcasts and songs asked for from STATE's
//!                                                         podcasts.json and requests.json; after import-navidrome)

use std::{
    io::{BufRead, Write},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tracing::{info, warn};

use maestro::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
};

#[cfg(windows)]
mod winsvc;

/// Stops the server when it completes (Ctrl+C, or the Windows service being stopped).
pub type Shutdown = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

pub fn init_logs(file: Option<std::fs::File>) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,sqlx=warn,lofty=error".into());
    match file {
        Some(f) => {
            tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(f)).init()
        }
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut config_path = None;
    if let Some(i) = args.iter().position(|a| a == "--config") {
        config_path = Some(PathBuf::from(args.get(i + 1).context("--config needs a file")?));
        args.drain(i..=i + 1);
    }
    if args.first().map(String::as_str) == Some("service") {
        #[cfg(windows)]
        return winsvc::command(&args[1..], config_path);
        #[cfg(not(windows))]
        bail!("`maestro service` is for Windows; on Linux use Docker or a systemd unit (see deploy/README.md)");
    }
    init_logs(None);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run(
        args,
        config_path,
        Box::pin(async {
            let _ = tokio::signal::ctrl_c().await;
        }),
    ))
}

pub async fn run(args: Vec<String>, config_path: Option<PathBuf>, shutdown: Shutdown) -> Result<()> {
    let cfg = Config::load(config_path)?;
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let pool = db::open(&cfg.data_dir.join("maestro.db")).await?;
    let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key"))?;
    let libs = library::ensure_libraries(&pool, &cfg.libraries).await?;
    let a: Vec<&str> = args.iter().map(String::as_str).collect();

    match a.as_slice() {
        [] | ["serve"] => serve(cfg, pool, secrets, libs, shutdown).await,
        ["scan", rest @ ..] => {
            let status = Arc::new(library::ScanStatus::default());
            let rep = library::scan(&pool, &libs, &status, rest.contains(&"--full")).await?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            Ok(())
        }
        ["user", "add", name, rest @ ..] => {
            let pw = read_password()?;
            auth::create_user(&pool, &secrets, name, &pw, rest.contains(&"--admin")).await?;
            println!("user {name} added");
            Ok(())
        }
        ["user", "passwd", name] => {
            let pw = read_password()?;
            if !auth::set_password(&pool, &secrets, name, &pw).await? {
                bail!("no user {name}");
            }
            println!("password changed");
            Ok(())
        }
        ["apikey", name, rest @ ..] => {
            let key = auth::create_api_key(&pool, name, rest.first().copied().unwrap_or("cli")).await?;
            println!("{key}");
            Ok(())
        }
        ["import-navidrome", db_path, rest @ ..] => {
            let artwork = rest.iter().position(|a| *a == "--artwork").and_then(|i| rest.get(i + 1)).map(PathBuf::from);
            let rep =
                maestro::import_nd::run(&cfg, &pool, &secrets, &libs, std::path::Path::new(db_path), artwork.as_deref()).await?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            if !rep.users.is_empty() {
                println!("Set a password for each imported user: maestro user passwd NAME");
            }
            Ok(())
        }
        ["import-music-lookup", data, rest @ ..] => {
            let state = rest.first().map(std::path::Path::new);
            let rep = maestro::import_nd::music_lookup(&pool, std::path::Path::new(data), state).await?;
            if let Some(dir) = state {
                let (login, mapped) = maestro::spotify::import_from(&pool, dir, libs.first().map(|l| l.0)).await?;
                println!(
                    "Spotify: login {}, {mapped} songs matched to their files",
                    if login { "brought over" } else { "not found" }
                );
            }
            if rep.podcasts > 0 {
                println!("Podcast episodes saved by music-lookup are in {data}/podcasts: set podcast_dir to that folder.");
            }
            println!("{}", serde_json::to_string_pretty(&rep)?);
            Ok(())
        }
        _ => bail!("unknown command; see the top of src/main.rs"),
    }
}

fn read_password() -> Result<String> {
    if let Ok(p) = std::env::var("MAESTRO_PASSWORD") {
        return Ok(p);
    }
    eprint!("password: ");
    std::io::stderr().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    let s = s.trim_end_matches(['\r', '\n']).to_string();
    if s.is_empty() {
        bail!("empty password");
    }
    Ok(s)
}

async fn serve(cfg: Config, pool: db::Db, secrets: Secrets, libs: Vec<(i64, LibraryConfig)>, shutdown: Shutdown) -> Result<()> {
    if auth::user_count(&pool).await? == 0 {
        match (std::env::var("MAESTRO_ADMIN_USER"), std::env::var("MAESTRO_ADMIN_PASSWORD")) {
            (Ok(u), Ok(p)) if !u.is_empty() && !p.is_empty() => {
                auth::create_user(&pool, &secrets, &u, &p, true).await?;
                info!(user = %u, "first admin created from MAESTRO_ADMIN_USER");
            }
            _ => warn!(
                "no users yet: add one with `maestro user add NAME --admin` or set MAESTRO_ADMIN_USER / MAESTRO_ADMIN_PASSWORD"
            ),
        }
    }
    if libs.is_empty() {
        warn!("no music folders configured (config.toml [[libraries]] or MAESTRO_MUSIC)");
    }
    if cfg.read_only {
        warn!("read-only: nothing is written to the music folders");
    }
    let addr = cfg.listen;
    let state = Arc::new(AppState::new(pool, secrets, cfg, libs));
    tokio::spawn(scan_loop(state.clone()));
    maestro::jam::load(&state).await;
    tokio::spawn(maestro::api::podcasts::worker(state.clone()));
    tokio::spawn(maestro::catalog::acquire::worker(state.clone()));
    tokio::spawn(maestro::online::worker(state.clone()));
    tokio::spawn(maestro::listenbrainz::worker(state.clone()));
    tokio::spawn(maestro::spotify::worker(state.clone()));
    tokio::spawn(maestro::concerts::worker(state.clone()));
    tokio::spawn(maestro::mixes::worker(state.clone()));
    if !state.cfg.jam_listen.is_empty() {
        let jam_addr: std::net::SocketAddr = state.cfg.jam_listen.parse().context("jam_listen")?;
        let st = state.clone();
        tokio::spawn(async move {
            if let Err(e) = maestro::jam::serve(st, jam_addr).await {
                warn!("Jam guest page: {e:#}");
            }
        });
    }

    let app = api::router(state).layer(tower_http::trace::TraceLayer::new_for_http());
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("listening on {addr}"))?;
    info!(%addr, "Maestro listening");
    axum::serve(listener, app).with_graceful_shutdown(shutdown).await?;
    Ok(())
}

/// Scan at start, then on a timer. A music folder that comes back (drive reconnected) is scanned at once.
async fn scan_loop(st: Arc<AppState>) {
    let interval = Duration::from_secs(st.cfg.scan_interval_min.max(1) * 60);
    let mut last = std::time::Instant::now();
    let mut was_online: Vec<bool> = st.libs.iter().map(|_| false).collect();
    let mut first = true;
    loop {
        let online: Vec<bool> = st.libs.iter().map(|(_, l)| library::is_online(l)).collect();
        let came_back = online.iter().zip(&was_online).any(|(n, o)| *n && !*o);
        let due = st.cfg.scan_interval_min > 0 && last.elapsed() >= interval;
        if first || came_back || due {
            if came_back && !first {
                info!("a music folder is connected again: scanning");
            }
            if let Err(e) = library::scan(&st.db, &st.libs, &st.scan, false).await {
                warn!("scan failed: {e:#}");
            }
            last = std::time::Instant::now();
            first = false;
        } else if online != was_online {
            for ((id, l), on) in st.libs.iter().zip(&online) {
                let _ = sqlx::query("UPDATE library SET online = ? WHERE id = ?").bind(on).bind(id).execute(&st.db).await;
                if !on {
                    warn!(library = %l.name, "music folder disconnected");
                }
            }
        }
        was_online = online;
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
