# Changes

What changed in each release, newest first. The full notes for each are on the
[releases page](https://github.com/d-safro/conductor/releases); what stays the same from 1.0 on is in
[docs/STABILITY.md](docs/STABILITY.md).

## Unreleased (0.5.0)

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
