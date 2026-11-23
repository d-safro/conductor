//! Conductor: a music server for Encore and any Subsonic / OpenSubsonic app.
//!
//!   conductor [--config FILE] [serve]          run the server (default; the first time in a terminal: Prelude first)
//!   conductor [--config FILE] setup [PART] [--yes …]  Prelude, the setup wizard (also `prelude`; see src/prelude)
//!   conductor [--config FILE] scan [--full]     scan the libraries once and exit
//!   conductor [--config FILE] health            is the server answering? (exit 0 / 1; Docker's health check)
//!   conductor [--config FILE] user add NAME [--admin] [--if-missing]   password from CONDUCTOR_PASSWORD or stdin
//!                                                         (--if-missing: a user that's there is left as it is)
//!   conductor [--config FILE] user passwd NAME
//!   conductor [--config FILE] apikey NAME [LABEL]          prints a new API key
//!   conductor [--config FILE] backup [FILE]                 everything needed to come back, in one zip
//!   conductor [--config FILE] restore FILE [--force]        put a backup back (Conductor stopped; see src/backup.rs)
//!   conductor [--config FILE] import-navidrome NAVIDROME.DB [--artwork DIR] [--nd-key KEY] [--replace]   (a copy of the database)
//!   conductor [--config FILE] service install|uninstall|start|stop   (Windows: run as a service)
//!   conductor [--config FILE] import <export|folder|spotify|lastfm:USER|deezer:ID> [--dry-run] [--to URL --as USER]
//!                                                         (your music from another service; see src/import/cli.rs)
//!   conductor [--config FILE] import-music-lookup DATA [STATE]  (play log, Encore's sync values, mixes, shares,
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

use conductor::{
    api::{self, AppState},
    auth::{self, Secrets},
    config::{Config, LibraryConfig},
    db, library,
};

#[cfg(windows)]
mod winsvc;

const USAGE: &str = "\
conductor [--config FILE] [serve]           run the server (the first time in a terminal, Prelude first)
conductor setup [PART] [--yes ...]          Prelude, the setup wizard (conductor setup --help)
conductor scan [--full]                     scan the music folders once
conductor health                            is the server answering? (exit 0 / 1, for health checks)
conductor user add NAME [--admin] [--if-missing]   (password from CONDUCTOR_PASSWORD or typed)
conductor user passwd NAME
conductor apikey NAME [LABEL]               print a new API key
conductor backup [FILE]                     everything needed to come back, in one zip
conductor restore FILE [--force]            put a backup back (Conductor stopped)
conductor import-navidrome NAVIDROME.DB [--artwork DIR] [--nd-key KEY] [--replace]
conductor import-music-lookup DATA [STATE]
conductor import <export|folder|spotify|lastfm:USER|deezer:ID> [--dry-run] [--to URL --as USER]
conductor service install|uninstall|start|stop   (Windows)
";

/// Stops the server when it completes (Ctrl+C, SIGTERM, or the Windows service being stopped).
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
        bail!("`conductor service` is for Windows; on Linux use Docker or a systemd unit (see deploy/README.md)");
    }
    init_logs(None);
    let rt = tokio::runtime::Runtime::new()?;
    let res = rt.block_on(run(args, config_path, Box::pin(stop_asked())));
    // what's still running (a request past its time, a scan's file reads) isn't waited for long
    rt.shutdown_timeout(Duration::from_secs(1));
    res
}

/// Ctrl+C, or SIGTERM: what systemd, `docker stop` and `kill` send. Both stop the server the same way (before, SIGTERM
/// killed it at once, in the middle of whatever it was doing).
async fn stop_asked() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

/// How long requests under way (a song playing, a download) get to finish once Conductor is asked to stop. `docker
/// stop` waits 10 s before it kills; this, then closing the database, fits in that.
const STOP_GRACE: Duration = Duration::from_secs(4);

pub async fn run(args: Vec<String>, config_path: Option<PathBuf>, shutdown: Shutdown) -> Result<()> {
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    // Docker's health check, every minute: only the settings are read (nothing opened, made or moved)
    if a.as_slice() == ["health"] {
        return health(&Config::load(config_path)?).await;
    }
    // an older install's default folder (and its config.toml) gets the new name before anything looks for them
    if config_path.is_none() && conductor::config::var_os("CONFIG").is_none() {
        conductor::config::move_old_data_dir();
    }
    // Prelude: `conductor setup`, and the first start in a terminal with nothing set up yet
    if let ["setup" | "prelude", rest @ ..] = a.as_slice() {
        let path = conductor::prelude::cli(config_path, rest).await?;
        if let Ok(cfg) = Config::load(Some(path)) {
            conductor::config::give_to_data_owner(&cfg);
        }
        return Ok(());
    }
    // a mistyped command says so before anything is opened or made (no database in the default folder)
    if matches!(a.as_slice(), ["help" | "--help" | "-h", ..]) {
        print!("{USAGE}");
        return Ok(());
    }
    if !matches!(
        a.as_slice(),
        [] | ["serve"]
            | ["scan", ..]
            | ["user", "add", _, ..]
            | ["user", "passwd", _]
            | ["backup", ..]
            | ["restore", _, ..]
            | ["apikey", _, ..]
            | ["import-navidrome", _, ..]
            | ["import-music-lookup", _, ..]
            | ["import", ..]
    ) {
        bail!("unknown command: {}\n\n{USAGE}", a.join(" "));
    }
    let first_start = matches!(a.as_slice(), [] | ["serve"])
        && config_path.is_none()
        && conductor::config::var_os("CONFIG").is_none()
        && conductor::config::var_os("MUSIC").is_none()
        && !conductor::config::default_config_path().is_file()
        && std::io::IsTerminal::is_terminal(&std::io::stdin());
    let config_path = if first_start { Some(conductor::prelude::cli(None, &[]).await?) } else { config_path };
    let cfg = Config::load(config_path)?;
    conductor::config::move_old_names(&cfg);
    // before the database is opened: it's what gets replaced
    if let ["restore", file, rest @ ..] = a.as_slice() {
        let r = conductor::backup::restore(&cfg, std::path::Path::new(file), rest.contains(&"--force"))?;
        conductor::config::give_to_data_owner(&cfg);
        if let Some(d) = &r.moved_aside {
            println!("What was here is in {}", d.display());
        }
        println!("Restored a backup from Conductor {}: {} files.", r.version, r.files.len());
        println!("Start Conductor as usual; it brings the database up to date if the backup is older.");
        return Ok(());
    }
    std::fs::create_dir_all(&cfg.data_dir).with_context(|| format!("creating {}", cfg.data_dir.display()))?;
    let pool = db::open(&cfg.data_dir.join("conductor.db")).await?;
    let secrets = Secrets::load_or_create(&cfg.data_dir.join("secret.key"))?;
    let libs = library::ensure_libraries(&pool, &cfg.libraries).await?;
    if !matches!(a.as_slice(), [] | ["serve"]) {
        conductor::config::give_to_data_owner(&cfg); // a command run with sudo: the service's files stay its own
    }

    match a.as_slice() {
        [] | ["serve"] => serve(cfg, pool, secrets, libs, shutdown).await,
        ["scan", rest @ ..] => {
            let status = Arc::new(library::ScanStatus::default());
            let rep = library::scan(&pool, &libs, &status, rest.contains(&"--full")).await?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            Ok(())
        }
        ["user", "add", name, rest @ ..] => {
            if rest.contains(&"--if-missing") {
                let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user WHERE name = ?").bind(name).fetch_one(&pool).await?;
                if n > 0 {
                    println!("user {name} is there already");
                    return Ok(());
                }
            }
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
        ["backup", rest @ ..] => {
            let out = rest.first().map(PathBuf::from).unwrap_or_else(conductor::backup::default_name);
            let n = conductor::backup::make(&cfg, &pool, &out).await?;
            println!("Backup: {} ({} KB). It holds your passwords' key and secrets: keep it somewhere safe.", out.display(), n / 1024);
            Ok(())
        }
        ["apikey", name, rest @ ..] => {
            let key = auth::create_api_key(&pool, name, rest.first().copied().unwrap_or("cli")).await?;
            println!("{key}");
            Ok(())
        }
        ["import-navidrome", db_path, rest @ ..] => {
            let flag = |f: &str| rest.iter().position(|a| *a == f).and_then(|i| rest.get(i + 1)).copied();
            let artwork = flag("--artwork").map(PathBuf::from);
            // Navidrome's own password key, if it was given one (else its default is used)
            let nd_key = flag("--nd-key").map(String::from).or_else(|| conductor::config::var("ND_PASSWORD_KEY").ok());
            // a second run replaces everything since (plays, hearts, playlists): only when asked
            let replace = rest.contains(&"--replace");
            let rep = conductor::import_nd::run(&cfg, &pool, &secrets, &libs, std::path::Path::new(db_path), artwork.as_deref(), nd_key.as_deref(), replace)
                .await?;
            println!("{}", serde_json::to_string_pretty(&rep)?);
            println!("Passwords carried over for {} of {} users.", rep.passwords_carried, rep.users_imported);
            if !rep.users.is_empty() {
                println!(
                    "Set a password for: {} (conductor user passwd NAME). If Navidrome had its own PasswordEncryptionKey, \
                     run the import again with it (--nd-key, or CONDUCTOR_ND_PASSWORD_KEY).",
                    rep.users.join(", ")
                );
            }
            Ok(())
        }
        ["import-music-lookup", data, rest @ ..] => {
            let state = rest.first().map(std::path::Path::new);
            let rep = conductor::import_nd::music_lookup(&pool, std::path::Path::new(data), state).await?;
            if let Some(dir) = state {
                let (login, mapped) = conductor::spotify::import_from(&pool, dir, libs.first().map(|l| l.0)).await?;
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
        ["import", rest @ ..] => {
            let st = Arc::new(conductor::api::AppState::new(pool, secrets, cfg, libs));
            conductor::import::cli::run(st, rest).await
        }
        _ => bail!("unknown command\n\n{USAGE}"),
    }
}

/// Asks the server at the address its settings give (the same settings and environment as `serve`: config.toml,
/// CONDUCTOR_CONFIG, CONDUCTOR_LISTEN). Before, Docker's health check assumed 4533, or CONDUCTOR_LISTEN's port, and a
/// server whose address was only in config.toml was "unhealthy" for good.
async fn health(cfg: &Config) -> Result<()> {
    let mut addr = cfg.listen;
    if addr.ip().is_unspecified() {
        addr.set_ip(if addr.is_ipv4() { std::net::Ipv4Addr::LOCALHOST.into() } else { std::net::Ipv6Addr::LOCALHOST.into() });
    }
    let url = format!("http://{addr}/health");
    let http = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(4)).build()?;
    let r = http.get(&url).send().await.with_context(|| format!("nothing answers at {url}"))?;
    if !r.status().is_success() {
        bail!("{url}: {}", r.status());
    }
    println!("{}", r.text().await.unwrap_or_default());
    Ok(())
}

fn read_password() -> Result<String> {
    if let Ok(p) = conductor::config::var("PASSWORD") {
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
    if auth::admin_count(&pool).await? == 0 {
        match (conductor::config::var("ADMIN_USER"), conductor::config::var("ADMIN_PASSWORD")) {
            (Ok(u), Ok(p)) if !u.is_empty() && !p.is_empty() => {
                auth::create_user(&pool, &secrets, &u, &p, true).await?;
                info!(user = %u, "first admin created from CONDUCTOR_ADMIN_USER");
            }
            _ => warn!(
                "no admin yet: add one with `conductor user add NAME --admin` or set CONDUCTOR_ADMIN_USER / CONDUCTOR_ADMIN_PASSWORD"
            ),
        }
    }
    if libs.is_empty() {
        warn!("no music folders configured (config.toml [[libraries]] or CONDUCTOR_MUSIC)");
    }
    if cfg.read_only {
        warn!("read-only: nothing is written to the music folders");
    }
    let addr = cfg.listen;
    let state = Arc::new(AppState::new(pool, secrets, cfg, libs));
    conductor::import::jobs::recover(&state).await;
    // the background workers, stopped when Conductor is
    let mut workers = vec![tokio::spawn(scan_loop(state.clone()))];
    conductor::jam::load(&state).await;
    workers.push(tokio::spawn(conductor::api::podcasts::worker(state.clone())));
    workers.push(tokio::spawn(conductor::catalog::acquire::worker(state.clone())));
    workers.push(tokio::spawn(conductor::online::worker(state.clone())));
    workers.push(tokio::spawn(conductor::vpn::worker(state.clone())));
    workers.push(tokio::spawn(conductor::listenbrainz::worker(state.clone())));
    workers.push(tokio::spawn(conductor::scrobble::worker(state.clone())));
    conductor::transcode::clean_parts(&state.cfg);
    workers.push(tokio::spawn(conductor::spotify::worker(state.clone())));
    workers.push(tokio::spawn(conductor::concerts::worker(state.clone())));
    workers.push(tokio::spawn(conductor::mixes::worker(state.clone())));
    workers.push(tokio::spawn(conductor::loudness::worker(state.clone())));
    workers.push(tokio::spawn(conductor::backup::worker(state.cfg.clone(), state.db.clone())));
    if !state.cfg.jam_listen.is_empty() {
        let jam_addr: std::net::SocketAddr = state.cfg.jam_listen.parse().context("jam_listen")?;
        let st = state.clone();
        workers.push(tokio::spawn(async move {
            if let Err(e) = conductor::jam::serve(st, jam_addr).await {
                warn!("Jam guest page: {e:#}");
            }
        }));
    }

    let admins: i64 = auth::admin_count(&state.db).await.unwrap_or(1);
    if admins == 0 {
        let host = if addr.ip().is_unspecified() { "THIS-COMPUTER".to_string() } else { addr.ip().to_string() };
        let link = format!("http://{host}:{}/setup?token={}", addr.port(), state.setup_token);
        info!("No admin yet. Make the first admin at {link} (or run `conductor setup` here)");
        println!("No admin yet. Make the first admin at {link}\n(from this computer: http://127.0.0.1:{}/setup; or run `conductor setup`)", addr.port());
    }
    let db = state.db.clone();
    let app = api::router(state).layer(api::trace_layer());
    let listener = tokio::net::TcpListener::bind(addr).await.with_context(|| format!("listening on {addr}"))?;
    info!(%addr, "Conductor listening");
    // asked to stop: no new connections; the requests under way get STOP_GRACE to finish
    let (stopping_tx, mut stopping) = tokio::sync::watch::channel(false);
    let asked = async move {
        shutdown.await;
        info!("stopping");
        let _ = stopping_tx.send(true);
    };
    let server = axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(asked);
    let res = tokio::select! {
        r = std::future::IntoFuture::into_future(server) => r.map_err(anyhow::Error::from),
        _ = async {
            if stopping.wait_for(|s| *s).await.is_err() {
                std::future::pending::<()>().await; // (the server ended by itself: the branch above)
            }
            tokio::time::sleep(STOP_GRACE).await;
        } => {
            warn!("requests still under way after {} s were cut off", STOP_GRACE.as_secs());
            Ok(())
        }
    };
    for w in &workers {
        w.abort();
    }
    // everything written goes into conductor.db itself, and the database is closed
    let close = async {
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)").execute(&db).await;
        db.close().await;
    };
    if tokio::time::timeout(Duration::from_secs(3), close).await.is_err() {
        warn!("the database took too long to close (it's checked when Conductor starts again)");
    }
    info!("Conductor stopped");
    res
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
