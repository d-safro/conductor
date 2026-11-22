<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/conductor-lockup-dark.svg">
    <img alt="Conductor" src="assets/brand/conductor-lockup-light.svg" width="440">
  </picture>
</p>

<p align="center">
  <a href="../../releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/d-safro/conductor?label=release"></a>
  <a href="../../actions/workflows/ci.yml"><img alt="CI" src="https://github.com/d-safro/conductor/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Linux, Windows and Docker" src="https://img.shields.io/badge/runs%20on-Linux%20%7C%20Windows%20%7C%20Docker-555">
  <img alt="AGPL-3.0" src="https://img.shields.io/badge/licence-AGPL--3.0-blue">
</p>

<p align="center">
  <b>A self-hosted music server</b> for <a href="https://github.com/d-safro/encore">Encore</a> and any app that speaks
  <b>Subsonic / OpenSubsonic</b>.<br>One Rust program for Linux (x86-64 and ARM64) and Windows: your library, mixes, lyrics, a web player, and downloads if you want them.
</p>

> Early development, used daily by its author. Conductor was called Maestro until November 2026: `MAESTRO_*`
> settings still work, and an older install's data is moved to the new names when it starts.

## Highlights

- **Your library, safely:** fast scans that skip unchanged files, ids that survive rescans, and a guard that won't wipe anything when a USB drive drops.
- **Plays everywhere:**
  - the Subsonic API for any app;
  - **Recital**, a small built-in web player;
  - Encore's extras for phone, desktop and watch.
- **Gets music you don't have, if you want it** (everything else works without it):
  - Prelude sets the downloader up as one Docker container, optionally inside a VPN;
  - search shows what you don't have, and hearting fetches it from Soulseek through slskd (**Stagehand**);
  - songs you only try out come and go as **Auditions**;
  - Lidarr's wanted albums too.
- **Made for you:** Discover Weekly, Release Radar, Daily Mixes and the Daylist (**Repertoire**); concerts near you (**Marquee**); your year in review (**Curtain Call**).
- **Brings your history with you** (**Transpose**): Spotify, Apple Music / iTunes, YouTube Music, Last.fm, ListenBrainz and playlist files, matched to your library and undoable.
- **Optional AI, all local:** lyrics translation (**Libretto**), a DJ between songs (**Interlude**), song ID (**Pitch**).
- **Easy to set up:** **Prelude** asks a few questions and tests each answer. It can set up everything around Conductor for
  you, in one Docker folder started together: the downloader (inside a VPN with a kill switch, if you like), Lidarr,
  AudioMuse-AI, Pitch, Metronome, and, only if you choose AI, Ollama and Interlude's voice.
- **Safe to run for years:** a backup every day (database, its key and your settings, in one zip) and `conductor restore` to bring one back.
- **Share and manage:** share links anyone can play without an account; users managed from any Subsonic app or Podium.

Everything, in detail: [docs/FEATURES.md](docs/FEATURES.md).

## Get it

Download the program for your system from the [latest release](../../releases/latest) (Linux x86-64 and ARM64,
Windows), use the Docker image, or build it (below). Then run **`conductor setup`**; Prelude walks you through the
rest.

```bash
docker run -d --name conductor -p 4533:4533 -v conductor-data:/data -v /path/to/music:/music:ro ghcr.io/d-safro/conductor
```

(With Docker, the first admin is made at `http://<server>:4533/setup`.) Coming from Navidrome? Prelude offers to
bring everything over, keeping your songs' ids so your apps carry on where they were.

## The parts, by name

| Name | What it is |
|---|---|
| Prelude | the setup wizard (`conductor setup`) |
| Podium | the admin page (`/admin`) |
| Recital | the web player (`/`) |
| Transpose | bringing your music in from other services |
| Stagehand | downloads, hand-picked files and better copies |
| Audition | songs on trial |
| Pitch | song ID ("What's playing?") |
| Metronome | Automix analysis (where songs start, end and fade) |
| Interlude | the DJ between songs |
| Libretto | lyrics translation |
| Ensemble | Jam (friends add to your queue) |
| Chorus | collaborative playlists |
| Repertoire | your mixes (Discover Weekly, Release Radar, Daily Mixes, Daylist) |
| Marquee | concerts near you |
| Curtain Call | year (and month) in review |

API method names, settings and `encore.*` extension names stay as they were, for the apps that use them.

## Run it

Needs **ffmpeg** on the PATH for transcoding (playing files as they are works without it).

```bash
cargo build --release
./target/release/conductor setup      # Prelude: asks for everything, checks it, writes the settings
./target/release/conductor            # serves (the first time in a terminal, Prelude runs first by itself)
```

**Prelude** (`conductor setup`, or `conductor prelude`) walks through the music folders, the data folder and port,
the first admin, downloads (optional: the downloader set up for you as one Docker container, or your own slskd),
**a VPN for downloads**, the other services (each one set up for you in Docker, one you already run, or none), bringing your history in
(Navidrome, music-lookup, Transpose) and running it as a service (systemd, the Windows service, or Docker). Run it
again to change one part (`conductor setup vpn`); `--yes` with flags answers everything for scripts
(`conductor setup --help`). Keys and passwords go to `secrets.toml` (mode 600) beside `config.toml`.
Before anyone has an account, `/setup` in a browser makes the first admin: only from the server itself, or with the
one-time link Conductor prints when it starts.

**Downloads through a VPN** (Prelude's VPN step): slskd already behind one (checked: the address downloads use
differs from this computer's); or set one up (Linux with Docker: gluetun with slskd inside, from a WireGuard .conf
of any provider or a provider's key, with the forwarded port kept on slskd's listen port); or a SOCKS5/HTTP proxy for
Conductor's own download traffic (on Windows, a VPN app with split tunnelling works too); or none. With
`downloads_require_vpn` (the kill switch) downloads wait whenever the VPN check fails, never using the open
connection; Podium and `/health` show the VPN's state and exit country (never the address).

By hand instead: `cp config.example.toml config.toml`, set your music folder, then
`CONDUCTOR_PASSWORD='…' conductor --config config.toml user add me --admin` and `conductor --config config.toml`.

Or with the environment only: `CONDUCTOR_MUSIC=/path/to/music CONDUCTOR_ADMIN_USER=me CONDUCTOR_ADMIN_PASSWORD=… conductor`.

Other commands: `scan [--full]`, `user passwd NAME`, `apikey NAME [LABEL]` (prints a new API key),
`backup [FILE]` and `restore FILE [--force]`.

**Backups:** every day Conductor saves one zip in `<data>/backups` (the newest 7 kept; `backup_keep`, `backup_dir`)
with a consistent copy of the database, `secret.key` (the saved passwords need it), your settings and the playlist
pictures. `conductor backup` makes one now. To bring one back, stop Conductor and run `conductor restore FILE`
(with `--force` over an existing library, which is moved aside, never deleted). The zip holds secrets: keep it
private.

**Moving over from Navidrome:** stop Navidrome, copy its data folder, then
`conductor import-navidrome /copy/navidrome.db --artwork /copy/artwork`. Songs, albums and artists keep
Navidrome's ids (so apps that remember ids keep working), and plays, ratings, stars, play history, playlists (with
their pictures and smart rules), play queues and radio stations come along. Users come over by name; set each
password is carried over from Navidrome's encrypted copy (`--nd-key` if Navidrome had its own key). Running it again
replaces what it brought over the last time.
Then `conductor import-music-lookup <music-lookup DATA folder> [<its state folder with podcasts.json>]` adds
music-lookup's listening log, Encore's sync values and the podcasts (plays already brought over from Navidrome
aren't counted twice).

Docker and Windows details: see `deploy/README.md`.

## Develop

```bash
cargo test          # unit tests + an end-to-end test that makes a tiny music folder with ffmpeg
cargo clippy --all-targets
```

CI (`.github/workflows/ci.yml`) runs both on Linux and Windows.

> Scratch runs against a real music folder: set `CONDUCTOR_READ_ONLY=1`, or downloads, trial songs and covers write into it.
