//! `maestro import …`:
//!
//!   import <export|folder|spotify|lastfm:USER|deezer:ID> [--dry-run] [--user NAME] [--only liked,albums,…]
//!          [--ask-missing N]                          into this server, as a batch (undo with `import undo ID`)
//!   import <…> --to URL --as USER [--state FILE] [--dry-run]
//!                                                     into any Subsonic server (password: MAESTRO_IMPORT_PASSWORD)
//!   import list                                       the imports here
//!   import undo ID | import undo --to URL --as USER   take one back
//!
//! Kinds for --only: liked, albums, artists, playlists, plays, ratings, counts, podcasts.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{
    apply::{self, Options, Report},
    jobs,
    subsonic::{Server, State},
};
use crate::{api::AppState, db};

fn flag<'a>(args: &'a [&'a str], name: &str) -> Option<&'a str> {
    args.iter().position(|a| *a == name).and_then(|i| args.get(i + 1)).copied()
}

fn input_of(what: &str) -> Result<Value> {
    Ok(match what.split_once(':') {
        _ if what == "spotify" => json!({ "online": "spotify" }),
        Some(("lastfm", u)) => json!({ "online": "lastfm", "user": u }),
        Some(("deezer", u)) => json!({ "online": "deezer", "user": u }),
        _ => {
            let p = std::fs::canonicalize(what).with_context(|| format!("{what} isn't there"))?;
            json!({ "path": p.to_string_lossy() })
        }
    })
}

async fn user_id(st: &AppState, name: Option<&str>) -> Result<String> {
    let id: Option<String> = match name {
        Some(n) => sqlx::query_scalar("SELECT id FROM user WHERE name = ?").bind(n).fetch_optional(&st.db).await?,
        None => sqlx::query_scalar("SELECT id FROM user WHERE is_admin = 1 ORDER BY rowid LIMIT 1").fetch_optional(&st.db).await?,
    };
    id.context("no such user (add one first: maestro user add NAME --admin)")
}

fn print_report(r: &Report) {
    println!("Found in: {}", r.sources.join(", "));
    for n in &r.notes {
        println!("  note: {n}");
    }
    let (f, m) = (&r.found, &r.matched);
    println!("                 found   in your library");
    for (k, a, b) in [
        ("liked songs", f.liked, m.liked),
        ("saved albums", f.albums, m.albums),
        ("artists", f.artists, m.artists),
        ("playlists", f.playlists, m.playlists),
        ("playlist songs", f.playlist_songs, m.playlist_songs),
        ("plays", f.plays, m.plays),
        ("skips (<30 s)", f.skips, m.skips),
        ("ratings", f.ratings, m.ratings),
        ("play counts", f.counts, m.counts),
        ("podcasts", f.podcasts, m.podcasts),
    ] {
        if a > 0 {
            println!("  {k:<15} {a:>7}   {b:>7}");
        }
    }
    let mut by: Vec<String> = r.by.iter().map(|(h, n)| format!("{h:?} {n}").to_lowercase()).collect();
    by.sort();
    println!("Different songs: {}, in your library: {} ({:.1}%; by {})", r.songs, r.songs_matched, r.match_rate * 100.0, by.join(", "));
    if r.missing_total > 0 {
        println!("Songs you don't have: {} (most wanted first):", r.missing_total);
        for m in r.missing.iter().take(15) {
            let why = if m.liked { "liked".to_string() } else if !m.playlists.is_empty() { format!("in {}", m.playlists.join(", ")) } else { format!("played {}×", m.plays) };
            println!("  {} – {}  ({why})", m.song.artist, m.song.title);
        }
    }
    if let Some(c) = &r.changes {
        println!("Changes: {}", serde_json::to_string(c).unwrap_or_default());
    }
}

pub async fn run(st: Arc<AppState>, args: &[&str]) -> Result<()> {
    let dry = args.contains(&"--dry-run");
    let mut opts = flag(args, "--only").map(Options::only).unwrap_or_default();
    opts.ask_missing = flag(args, "--ask-missing").map(|n| n.parse()).transpose().context("--ask-missing takes a number")?.unwrap_or(0);
    let to = flag(args, "--to");
    match args.first().copied() {
        None => bail!("maestro import <export|folder|spotify|lastfm:USER|deezer:ID> [--dry-run] (see src/import/cli.rs)"),
        Some("list") => {
            for b in jobs::list(&st, None).await? {
                println!("{}  {:<8} {}  {}", b["id"].as_str().unwrap_or(""), b["status"].as_str().unwrap_or(""), b["source"].as_str().unwrap_or(""), serde_json::to_string(&b["report"]["changes"]).unwrap_or_default());
            }
            Ok(())
        }
        Some("undo") if to.is_none() => {
            let id = args.get(1).context("import undo ID")?;
            jobs::undo(&st, id).await?;
            println!("import {id} undone");
            Ok(())
        }
        Some(first) => {
            let undo = first == "undo";
            if let Some(url) = to {
                let who = flag(args, "--as").context("--to needs --as USER (password in MAESTRO_IMPORT_PASSWORD)")?;
                let pass = std::env::var("MAESTRO_IMPORT_PASSWORD").context("set MAESTRO_IMPORT_PASSWORD to that user's password")?;
                let server = Server::new(url, who, &pass);
                let host: String = url.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
                let state_path = flag(args, "--state").map(PathBuf::from).unwrap_or_else(|| super::upload_dir(&st.cfg.data_dir).join(format!("sent-{host}-{who}.json")));
                let mut state = State::load(&state_path);
                state.server = url.to_string();
                if undo {
                    println!("{}", server.undo(&mut state, &state_path).await?);
                    return Ok(());
                }
                let p = jobs::read(&st, &input_of(first)?).await?;
                let lib = server.library().await.context("reading the server's songs")?;
                println!("The server has {} songs.", lib.len());
                let plan = apply::plan(&p, &lib, &opts, &Default::default());
                let label = format!("Imported from {} on {}", plan.source, chrono::Local::now().format("%Y-%m-%d"));
                let changes = server.apply(&plan, &label, &mut state, &state_path, dry).await?;
                let mut report = plan.report;
                report.changes = Some(changes);
                print_report(&report);
                if opts.podcasts && !plan.podcasts.is_empty() && !dry {
                    let mut feeds = vec![];
                    for pc in &plan.podcasts {
                        if let Some(f) = pc.feed.clone().or(super::online::podcast_feed(&st.cfg.itunes_url, &pc.title, &pc.publisher).await) {
                            feeds.push(f);
                        }
                    }
                    match server.podcasts(&feeds).await {
                        Ok(n) => println!("Podcasts subscribed: {n}"),
                        Err(e) => println!("Podcasts not sent ({e:#}); this server may not have the podcast API."),
                    }
                }
                println!("{} (state: {})", if dry { "Dry run: nothing was sent." } else { "Sent. Skips aren't sent; plays can't be taken back through the API." }, state_path.display());
                return Ok(());
            }
            let user = user_id(&st, flag(args, "--user")).await?;
            let input = input_of(first)?;
            if dry {
                let p = jobs::read(&st, &input).await?;
                let plan = jobs::plan(&st, &p, &opts).await?;
                let changes = apply::preview(&st.db, &user, &plan).await?;
                let mut report = plan.report;
                report.changes = Some(changes);
                print_report(&report);
                println!("Dry run: nothing was changed.");
                return Ok(());
            }
            let id = db::random_id()[..16].to_string();
            sqlx::query("INSERT INTO import_batch (id, user_id, input_json, status, options_json, created_at) VALUES (?, ?, ?, 'running', ?, ?)")
                .bind(&id)
                .bind(&user)
                .bind(input.to_string())
                .bind(serde_json::to_string(&opts)?)
                .bind(db::now())
                .execute(&st.db)
                .await?;
            let report = jobs::run(&st, &id, &user, &input, &opts).await?;
            println!("Import {id} done: {}", report["changes"]);
            println!("Undo it with: maestro import undo {id}");
            Ok(())
        }
    }
}
