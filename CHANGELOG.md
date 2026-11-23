# Changes

What changed in each release, newest first. The full notes for each are on the
[releases page](https://github.com/d-safro/conductor/releases); what stays the same from 1.0 on is in
[docs/STABILITY.md](docs/STABILITY.md).

## Unreleased

### Fixed
- **Podium › Settings** turned down a decimal for the settings that take one (`trial_hours`, `lidarr_match_ratio`:
  "a whole number"); they now take decimals, and a phone shows a keypad with a decimal point for them.
- **Search ignored case only in A-Z**: "кино" didn't find "Кино" (SQLite's LIKE folds only ASCII). `search2`/`search3` and
  the Jam and Chorus guest pages now find Cyrillic, Greek and other names typed in any case.
- **Recital** showed a broken-image icon (in Chrome, Edge and Encore's desktop app) for every album without a cover: the
  server tells Recital "not found" so it can draw its own square, but the page never did. It now does.
- **`conductor restore` into another data folder** (a new computer, a moved folder) put the database there but brought
  back `config.toml` still naming the old folder, so the next start used the old one and the restored library seemed
  gone. The restored settings now keep the data folder the database went to.

## 1.0.0 (2026-11-22)

1.0: from here on, the promises in [docs/STABILITY.md](docs/STABILITY.md) hold. It is 1.0.0-rc.1 with these fixes:

### Fixed
- **The Arch package's version** left out the prerelease (1.0.0-rc.1 was 1.0.0-1), so pacman would not have replaced
  it with 1.0.0. A prerelease is now spelled the way pacman sorts it (1.0.0rc.1), and CI checks that every installed
  package's version sorts before its release on Debian, Fedora and Arch.
- **SHA256SUMS.txt** named the .deb and .rpm files with the `~` that GitHub turns into `.`; the files are now named as
  published, so `sha256sum -c` finds them.

### New
- `RUST_LOG=conductor::api=debug` logs every Subsonic error the server answers with, and the app that got it.

## 1.0.0-rc.1 (2026-11-22)

### Fixed
- **Podium on a phone**: the username box no longer capitalises the first letter (so signing in failed); a wrong
  password says so instead of "sign in"; the open tab is marked again after switching (it lost its highlight, and
  screen readers which tab was open); labels sit above their boxes.
- **Readable buttons in dark mode** in Podium and setup (white on light blue was 2.3:1, now dark ink at ~8:1); boxes
  named only by their placeholder have that name for screen readers too; the share page has a main landmark.

### Security
- **Every web page runs only its own script** (Content-Security-Policy with the script's hash, instead of allowing any
  inline script), so markup that got into a page some other way (a tag in a song's title, say) can't run.
- **Podium and setup** had no such policy at all; they now also refuse to be shown inside another site's frame (so
  nobody can lay a page over them and have an admin click through) and aren't cached.
- **Podium's sign-in cookie is Secure** when you sign in over HTTPS through a proxy on the server (Prelude's Caddy, a
  Cloudflare Tunnel, Tailscale), so a browser never sends it over plain HTTP.
- Every answer says `nosniff` and `same-origin` referrers; setup also counts requests that came through Cloudflare or
  Tailscale as from elsewhere by their own headers.

### New
- **Linux packages**: .deb (Debian, Ubuntu), .rpm (Fedora, openSUSE) and Arch, for x86-64 and ARM64, with a system
  service that runs as its own user (not started until it's set up). Each is installed and removed again in its
  system on every change.
- **"Made for you" without AudioMuse-AI** (`encore.forYou`, `getForYou`): a mix from the artists you play most and
  those that go with them, about 60% new to you, the same all day and new each day. Encore uses it when the server has
  no AudioMuse-AI.
- **Settings in Podium**: every setting in the settings reference can be changed from the admin page (keys and
  passwords go to secrets.toml and are never shown; a value Conductor can't use isn't saved; a restart uses them).
  Music folders and the data folder stay with `conductor setup`.
- **Conversions are kept** (`<data>/cache/transcode`, `transcode_cache_mb`, 1000 MB): a song converted for an app
  (another format, a lower bitrate) is written to the cache while the app is already sent it, so ffmpeg is done in a
  second or two however slowly the app listens; the same song again comes from the cache, with seeking.
- **ffmpeg can't swamp the server**: `transcode_slots` (half the cores) run at once as a rule; when they're busy a
  song waits half a second, then starts anyway, up to four times as many; ffmpeg runs at a lower priority than
  everything else. Measured: 64 apps converting at once used every core and 64 ffmpegs (~3 GB) before; now at most
  the limit, the other services and the API stay quick, and nobody waits seconds while it's light.

## 0.5.1 (2026-11-22)

### Fixed
- **Security:** the SQLite built into Conductor had a vulnerability (CVE-2026-119375, RUSTSEC-2026-0459). It's the
  fixed one now (sqlx 0.9, libsqlite3-sys 0.40). Every place that builds SQL as it runs was checked again.
- The API's shapes are recorded and checked (docs/api/), with a spec for each of Conductor's own extensions; the list
  of extensions no longer names calls that don't exist.

## 0.5.0 (2026-11-22)

### New
- **Sounds like, without AI**: song radio, similar songs and similar artists are worked out from your library and what
  you play (played together, genres and their words, the script of names and titles, era), so they stay close
  without AudioMuse-AI or any online service. On a real library, radio after a Russian rock song now stays with
  Russian rock instead of wandering off after two songs.

### Fixed
- Related artists from Deezer were only looked up with downloads on; they're used with online info too.
- Genres written with "|" between them ("Rock | Blues | Hard Rock") are split like the others.

## 0.4.0 (2026-11-22)

### New
- **Scrobbling for every user**: everyone links their own ListenBrainz and Last.fm in Encore; plays go to each, and
  ones a service missed are sent later, in order. (Before, only the owner's plays went to ListenBrainz.)
- **A picture for albums without a cover** for apps other than Encore, as Navidrome sends, instead of "not found".
- **Conductor on your domain** (`conductor setup domain`): HTTPS on your own domain with Caddy, a Cloudflare Tunnel,
  or Tailscale Funnel, each set up for you.
- **Prelude finds your music**: it looks at the computer's drives and the music on them, and offers the folder with
  the most songs, or a new one on the drive with the most room (with the drive guard on a USB drive).
- **Prelude sets up the whole music lab for you** in one Docker folder: the downloader (optionally inside a VPN with
  a kill switch), Lidarr, AudioMuse-AI, Pitch and Metronome, and only with AI on, Ollama and Interlude's voice. Each
  one's address and keys go into Conductor's settings by themselves. Works on SELinux systems (Fedora, RHEL).
- **Downloads are optional**: without a downloader, songs you don't have aren't shown.
- **Linux ARM64** downloads and a **Docker image** (`ghcr.io/d-safro/conductor`, x86-64 and ARM64).
- **Backups**: one a day by itself, `conductor backup` and `conductor restore`.
- **Shares**, **user management** and **avatars** over the Subsonic API; the **index-based play queue**.
- **Every setting from the environment** (`CONDUCTOR_<NAME>`); a settings reference and a user guide.
- **Works with web apps** on other addresses (CORS), such as Feishin's web version.
- OpenSubsonic's song and album fields are always there, as apps expect.

### Faster
- With 100,000 songs: fetching the whole library (Symfonium and others) ~10 s instead of 2½ minutes; stars, genres
  and searches 5-100× quicker; first scan ~10 s, ~75 MB of memory.

### Fixed
- Setting up a new server with an export to bring in (`--transpose`, or typed in Prelude): the songs were matched
  before the music was scanned, so none matched, and with `--yes` nothing came in at all. The music is scanned
  first now, and an export you name is brought in unless you say no.
- Setting up next to another Conductor: the Jam listener's port (4535) was taken, and Jam and share links quietly
  didn't work. Prelude picks a free port near it and says so.
- MP3s whose Cyrillic tags were written as Windows-1251 but marked Latin-1 showed garbled names ("Àêâàðèóì" for
  «Аквариум»), and the same for UTF-8 tags marked Latin-1 ("ÐÐ½Ð´ÑÐµÐ¹" for «Андрей»); they're read right now, songs
  already scanned too (read again once). Western accents stay.
- Plays an app hands in together (made offline) are sent to ListenBrainz and Last.fm in order, one after another.
- A ListenBrainz token that ListenBrainz stops accepting is shown as needing a new link, as Last.fm's are.
- Plays waiting to be sent again: one user's long wait (500 or more, their service turning them away for now) no
  longer keeps everyone else's waiting plays from being sent.
- Podcasts: changes are for admins, and feeds can't reach your own network (`podcast_local` allows it).
- A smart playlist could read someone else's private playlist.
- Phone sign-in codes and wrong passwords are limited per address.
- "What's playing?" failed for every recording from an Android phone (M4A through a pipe).
- AudioMuse was given songs you don't have to analyse.
- A service's account (AudioMuse's) stopped the first admin from being made.
- Interlude's voice failed while it was still downloading.
- An export with a certain kind of date crashed the import.
- `getUser` answered for any name.
- Prelude's domain step: with Conductor on one of the computer's addresses (not 0.0.0.0), "only Caddy reaches it"
  moved Conductor to 127.0.0.1 after the Caddyfile was written, so Caddy pointed at nothing. Run again, it no longer
  moves Conductor back to 127.0.0.1 unasked; an email that isn't one is no longer written into the Caddyfile.
- A better copy swapped in for a song with another file type (FLAC for an MP3) could leave the song twice in the
  library: the old file went only after scans were let run again.
- Prelude's look at the drives: music found on a slow drive just as its time ran out was left out as empty; on
  Windows a music folder on a USB drive wasn't seen as on that drive (no drive guard offered); `--music auto` in a
  terminal, then "Type a path", offered "auto" as the folder.
- Prelude's import step stopped with an error (pointing at a `--replace` it doesn't have) when given a Navidrome
  database on a server already in use; it now asks before replacing, and with `--yes` leaves things as they are.
- Choosing Tailscale Funnel in Prelude's domain step left Caddy or the Cloudflare Tunnel set up before in place,
  still opening Conductor on its old name; they're now offered to be taken out, as switching between those two does.
- A downloaded song filed over an hour before was given up on ("not found", the hearts and playlists it was for
  never applied) while a long scan of the library was running, though its file was there.

## 0.2.1 (2026-11-22)
- Moving from Navidrome keeps your password.

## 0.2.0 (2026-11-22)
The first release as Conductor (it was Maestro): Prelude, the setup wizard, with a VPN for downloads; Recital, the
web player; Transpose, imports from Spotify, Apple Music, YouTube Music, Last.fm, ListenBrainz, Deezer and playlist
files; better copies of downloads; picking files by hand; artist bios; Encore 1.9 and 1.10's features; Windows
service; 17 fixes from a full review.

## 0.1.0-rc1
The first test version, as Maestro: a Subsonic server for Encore, standing in for Navidrome and music-lookup.
