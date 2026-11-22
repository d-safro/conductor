# Running Conductor

How to install it, connect your apps, reach it from elsewhere, keep it safe and up to date, and what to do when
something's off. Every setting is in [CONFIG.md](CONFIG.md); everything it can do is in [FEATURES.md](FEATURES.md).

## Install

**Linux** (x86-64 or ARM64, e.g. a Raspberry Pi 4/5): download `conductor-<version>-linux-<arch>.tar.gz` from the
[latest release](../../../releases/latest), unpack it, and run:

```bash
./conductor setup
```

Install `ffmpeg` too (your distribution's package): Conductor plays files as they are without it, but needs it to
convert formats for apps that ask for smaller files.

**Windows:** download `conductor-<version>-windows-x86_64.zip`, unpack it, and in PowerShell as administrator:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1 -Music "D:\Music"
```

It installs Conductor as a service that starts with Windows (add `-OpenFirewall` so phones on your network can reach
it). Put `ffmpeg.exe` in the same folder. More in [deploy/README.md](../deploy/README.md).

**Docker** (Linux, x86-64 or ARM64):

```bash
docker run -d --name conductor -p 4533:4533 \
  -v conductor-data:/data -v /path/to/music:/music \
  ghcr.io/d-safro/conductor
```

Then open `http://<server>:4533/setup` from the server itself (or with the one-time link in `docker logs conductor`)
to make the first admin, or start it with `-e CONDUCTOR_ADMIN_USER=me -e CONDUCTOR_ADMIN_PASSWORD=…`. Any setting can
be given the same way (`CONDUCTOR_<NAME>`, see [CONFIG.md](CONFIG.md)). Add `:ro` to the music folder if Conductor
should never write there (then also `-e CONDUCTOR_READ_ONLY=1`).

## Set it up: Prelude

`conductor setup` asks about each part and checks every answer: your music folders, the address, the first admin,
downloads (optional), a VPN for them, the services around Conductor, how it's reached from outside, bringing your
history in, and running it as a service. Run it again any time to change one part (`conductor setup services`, `conductor setup vpn`, …); the current
answers are the defaults. Keys and passwords go to `secrets.toml`, which only you can read.

The first time, the music step looks at this computer's drives (their label, size, free space, and whether they're
USB) and the music already on them (folders called Music, Musik, Música… near the top of each), and offers them,
the one with the most songs first; or a new music folder on a drive (the one with the most free space, a big data
drive before the system drive), with the downloads beside it on the same drive; or a path you type. On a drive
that can be unplugged it sets up the drive guard (a `.library-id` file in the folder), so while the drive is away
your songs stay in the library. `--music auto` takes what it suggests in a `--yes` run; `--music PATH` is that
folder, as always.

For the services it can set up for you (the downloader, Lidarr, AudioMuse-AI, Pitch, Metronome and, only if you choose
AI, Ollama and Interlude's voice) it writes one Docker folder, `services/` beside your settings. Start or update them
all there with `docker compose up -d --build`.

## Connect your apps

Use the server address (`http://<server>:4533`), your user name and password.

- **Encore** (phone, desktop, watch): everything works, including its extras (Jam, devices, mixes, lyrics…).
- **Symfonium, Tempo, DSub, Substreamer, Amperfy, play:Sub, Feishin, Supersonic** and other Subsonic / OpenSubsonic
  apps: add it as a Subsonic or **OpenSubsonic** server. (In Feishin, choose OpenSubsonic, not Navidrome.)
- **In a browser:** Recital, the built-in player, is at `/`; Podium, the admin page, at `/admin`.
- An app that only knows API keys: make one in Podium › Users, or with `conductor apikey NAME`.

## Reach it from elsewhere

The safest way is a private network such as **Tailscale**: install it on the server and your phone, and use the
server's Tailscale address. Nothing is open to the internet.

`conductor setup domain` sets it up for you, one of three ways:

- **Your own domain, with HTTPS**: Caddy, in Prelude's services folder (`caddy.yml`, `caddy/Caddyfile`), gets the
  certificate from Let's Encrypt and sends `https://<domain>` to Conductor and `/jam/…` to the Jam listener (or a
  second name to it). Prelude checks the name points at this computer's public address; behind a router, forward
  ports 80 and 443 to it. It sets `jam_public_url`, and offers `listen = "127.0.0.1:…"` so only Caddy reaches
  Conductor (the first time; run again, the address you have stays the default). (`--domain NAME [--jam-domain NAME] [--domain-email E]` for `--yes` runs.)
- **A Cloudflare Tunnel** (no ports to open): paste the tunnel's token (kept in `tunnel.env`, readable only by you);
  cloudflared runs in the same folder (`tunnel.yml`). In Cloudflare's dashboard, give the tunnel a public hostname
  to `http://127.0.0.1:4533`, and the same name with the path `^/jam/` to `http://127.0.0.1:4535`.
  (`--tunnel-token TOKEN` or `CONDUCTOR_TUNNEL_TOKEN`, `--tunnel-host NAME`.)
- **Tailscale Funnel**: it shows (and, with a yes, runs) `tailscale funnel --bg` for the Jam listener and
  `tailscale serve --bg --https=8443` for the apps, which stay on your tailnet, and sets `jam_public_url` to this
  computer's `https://<machine>.<tailnet>.ts.net` name. (`--funnel`.)

`--no-domain` takes Caddy or the tunnel out again. Start what it wrote with `docker compose up -d` in the folder.

By hand: put Conductor behind a reverse proxy **on the same computer** (so the proxy's `X-Forwarded-For` is trusted
for rate limits). A proxy on another computer isn't trusted: every app and guest then has the proxy's address, and
they share one limit on wrong passwords. With Caddy:

```
music.example.com {
    reverse_proxy 127.0.0.1:4533
}
```

and set `listen = "127.0.0.1:4533"` so only the proxy reaches it. Behind a proxy, `/setup` (before there's an admin)
needs the one-time link Conductor prints when it starts.

**Jam guests and share links** use a separate listener (`jam_listen`, 127.0.0.1:4535) that serves nothing but those
pages; that is the only thing to put on the internet for them (e.g. Tailscale Funnel), with its public address in
`jam_public_url`.

## Upgrade

1. Make a backup: `conductor backup` (Conductor also makes one every day in `<data>/backups`).
2. Replace the program (or `docker pull ghcr.io/d-safro/conductor` and recreate the container; on Windows, run the new
   zip's `install.ps1`, which keeps your data).
3. Start it. It brings the database up to date by itself; there's nothing else to run.

To go back, restore the backup from before the upgrade with the older version (`conductor restore FILE --force`).
A backup from a newer version is refused by an older one.

## Backups

Every day Conductor saves one zip in `<data>/backups` and keeps the newest 7 (`backup_keep`, `backup_dir`). It holds a
consistent copy of the database, `secret.key` (without it the saved passwords can't be read), your settings and the
playlist pictures; not the music and not the cache. **The zip holds secrets: keep copies somewhere private.**

`conductor backup [FILE]` makes one now. To bring one back, stop Conductor, then `conductor restore FILE` (add
`--force` over an existing library; what was there is moved to `before-restore-<time>/`, never deleted).

## Moving from Navidrome

Prelude offers it (`conductor setup import`), or by hand: stop Navidrome, copy its data folder, then
`conductor import-navidrome /copy/navidrome.db --artwork /copy/artwork`. Songs keep their ids, so apps carry on where
they were; plays, ratings, stars, playlists (with smart rules and pictures), play queues and passwords come along.

## When something's off

| What you see | What to do |
|---|---|
| Songs show but don't play, "drive not connected" | The music folder is missing or empty (a USB drive unplugged, a sentinel file gone). Conductor keeps everything and waits; plug it back in. |
| "Too many wrong passwords" | 30 wrong tries from apps (10 on the admin page) in 15 minutes from one address. Wait a few minutes; a device with an old saved password is usually the cause. |
| A web app (Feishin's web version…) can't connect | Use the server's full address with `http://` or `https://`; Conductor allows web apps on other addresses. |
| Some formats don't play in an app, or no smaller files | Install ffmpeg (or set `ffmpeg` to its path). |
| Downloads stay paused | The VPN check fails and the kill switch is on: Podium › Status shows why. `conductor setup vpn` checks it again. |
| Songs you don't have (⬇) don't show | They need a downloader (slskd or Lidarr); without one they're off unless `catalog = true`. |
| Podcasts from your own network won't add | Feeds come only from the internet unless `podcast_local = true`. |

**Logs:** in the terminal it runs in; `journalctl --user -u conductor` (systemd user service); `docker logs conductor`;
`<data>\conductor.log` for the Windows service. More detail: `RUST_LOG=debug`. Passwords and keys are never logged.
