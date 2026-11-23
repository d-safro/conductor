# Roadmap

Conductor 1.0 is out (2026-11-22): from here on the promises in [STABILITY.md](STABILITY.md) hold, and your data,
settings, the API and Conductor's own extensions keep working across every 1.x release. Below: what 1.0 needed, how
each ended, and what comes next.

## Needed for 1.0
1. ~~**Specs for Conductor's own API extensions.**~~ Done: a spec per `encore.*` extension in [api/](api/) (calls,
   parameters, every field of the answer), and the recorded shape of every call's answer (117 calls), checked by the
   tests and by CI, so a change can add fields but never drop or change one.
2. **Other apps, for real.** Used against a real server, one by one, and fixed for what they trip over. Done so far:
   Tempus, Ultrasonic, Symfonium and Substreamer (Android: a full library sync, browse, search, stream with
   seeking, star, rate, scrobble, lyrics, podcasts, radio, shares, bookmarks; no errors), Encore, Feishin's web
   version. Still to try: Feishin and Supersonic (desktop); Airsonic-refix (web); Amperfy and play:Sub (iPhone).
3. **Windows on a real PC.** Not done for 1.0: CI runs the tests and installs, restarts and removes the service on
   Windows on every change, but no real PC has run it yet (`install.ps1` as a service, a phone through the firewall,
   music on a USB drive). Still wanted; reports welcome.
4. **A release candidate run.** Cut short: 1.0.0-rc.1 ran on the home server (the Docker image, the full library, a
   phone) only briefly before 1.0 was released. The weeks-long watch (memory, errors, scans, daily backups) carries on
   with 1.0.x.
5. ~~**A last security pass on what faces the internet.**~~ Done: every page runs only its own script (hashed CSP);
   Podium and setup can't be framed or cached; Podium's cookie is Secure over HTTPS; proxies (Caddy, Cloudflare,
   Tailscale) never count as "this computer" for setup; dependencies are checked in CI (which found and fixed the
   SQLite CVE in 0.5.1).
6. ~~**A load test.**~~ Done: up to 64 apps streaming and converting at once, with a full rescan; it found ffmpeg
   unbounded (every core, ~50 MB each), fixed by the conversion cache and slots. Then on a small machine: no Pi to
   hand, so a Raspberry Pi 4 simulated by holding the release build and its ffmpegs to 4 cores at a Pi 4 core's speed
   (CPU quotas: a 4-minute FLAC to mp3 in ~16 s, about what a Pi 4 core takes; a close stand-in, not a Pi), with 2 GB
   and then 700 MB (a 1 GB Pi), the default settings (2 slots), 3,060 songs (306 albums; 60 songs 4 minutes long, the
   rest 20 s) and the levelling pass measuring them throughout. Apps each pulled a converted song (mp3, 128k) the
   moment the last one was in, much harder than people listening, while a light call (`getAlbumList2`) was timed:

   | apps converting at once | first byte (median / 95%) | light call (median / max) |
   |---|---|---|
   | 1 | 0.31 / 0.39 s | 5 / 29 ms |
   | 2 | 0.46 / 0.68 s | 10 / 40 ms |
   | 4 | 0.98 / 1.6 s (4-minute songs: 1.2 / 1.8 s) | 20 / 79 ms |
   | 8 | 2.2 / 2.6 s (4-minute: 1.6 / 2.3 s; levelled: 1.7 / 2.6 s) | 40 / 210 ms |
   | 8, with a full rescan | 2.0 / 2.9 s; the rescan of 3,060 songs 12.5 s | 51 / 260 ms |
   | 16 | 9.5 / 14 s | 59 / 210 ms |
   | 32 | 19 / 23 s | 58 / 370 ms |

   A Pi 5 (the same, its cores about 2.5 times as fast): 4 apps 0.45 s, 8 apps 0.9 s, 16 apps 3.1 s to the first
   byte, light calls at most 140 ms. A song from the cache: 2-50 ms. Nothing failed: no errors, never more than 9 ffmpegs (8 conversions and the
   levelling pass), Conductor itself 31-44 MB, everything together (Conductor and its ffmpegs) at most 165 MB besides
   the page cache, which gave way at 700 MB: nothing was killed or slowed (8 apps on 4-minute songs with a rescan:
   first byte 2.3 s). The levelling pass took 1-1.8 s a short song under that load (a 4-minute song: 6.6 s on a Pi 4 core). It found
   two things, both fixed: ffmpeg wrote nothing until about 15 s of the song was converted (before: 2 apps 1.1 s, 8
   apps 5.3 s to the first byte), and a song skipped went on being converted to its end (eight skipped a second
   apart: the song stayed on started in 4.1 s, now 0.5 s). Recommended settings for small machines are in
   [GUIDE.md](GUIDE.md#small-machines-a-raspberry-pi).

## Should be in 1.0
7. ~~**Settings in Podium.**~~ Done: every setting in the reference, changed from Podium › Settings (secrets never
   shown); music folders and the data folder stay with `conductor setup`.
8. ~~**Encore's "Made for you" without AudioMuse-AI.**~~ Done: `encore.forYou` (Conductor) and Encore's Discover and
   Home use it when there's no AudioMuse-AI. Moods need audio analysis and stay AudioMuse's.
9. ~~**Linux packages.**~~ Done: .deb, .rpm and Arch for x86-64 and ARM64, each installed and removed in its system's
   container by CI.
10. ~~**Accessibility and phone layout.**~~ Done: checked on a phone (Firefox, dark mode); fixed contrast, Podium's
    sign-in on phones and its tab marking, names for screen readers, a landmark on the share page.

## After 1.0
- ~~Live updates (an event stream) instead of long-polls.~~ Done: `encore.events`, and Encore uses it (its long-polls
  and timers stay for other servers). Still to do: Discord presence from it.
- ~~"Sounds like" from the audio itself, without AudioMuse-AI.~~ Done: every song is analysed once in the background
  (tempo, beat, brightness, timbre, key, dynamics, from a minute of it), and similar songs, Instant Mix, sonic paths and
  the mixes go by how songs sound, blended with which artists go together (`audio_analysis`).
- ~~Loudness levelling on the server.~~ Done: songs without ReplayGain tags are measured in the background and the
  apps get the measured gain as their ReplayGain; Recital and share links level songs too; and conversions are
  levelled for apps that can't level themselves (`level_conversions`).
- Jukebox mode (the server plays on its own speakers).

## Done on the way (0.4.0, 0.5.0)
Prelude sets up the whole music lab, a domain, and finds the music on the drives; backups; upgrades tested from every
earlier release; fast at 100,000 songs; works with web apps; shares, users, avatars; scrobbling for every user; a
picture for albums without one; "sounds like" without AI; two full reviews (about 100 fixes); a from-scratch install
tested.
