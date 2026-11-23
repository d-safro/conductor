# Roadmap

Conductor 1.0 is out (2026-11-22): from here on the promises in [STABILITY.md](STABILITY.md) hold, and your data,
settings, the API and Conductor's own extensions keep working across every 1.x release. Below: what 1.0 needed, how
each ended, and what comes next.

## Needed for 1.0
1. ~~**Specs for Conductor's own API extensions.**~~ Done: a spec per `encore.*` extension in [api/](api/) (calls,
   parameters, every field of the answer), and the recorded shape of every call's answer (117 calls), checked by the
   tests and by CI, so a change can add fields but never drop or change one.
2. **Other apps, for real.** Used against a real server, one by one, and fixed for what they trip over. Done so far:
   Tempus and Ultrasonic (Android: browse, search, stream with seeking, star, rate, scrobble, lyrics, podcasts,
   radio, shares, bookmarks; no errors), Encore, Feishin's web version. Still to try: Symfonium, Substreamer;
   Feishin and Supersonic (desktop); Airsonic-refix (web); Amperfy and play:Sub (iPhone).
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
   unbounded (every core, ~50 MB each), fixed by the conversion cache and slots. Still to do on a small machine (a
   Raspberry Pi), with the numbers in the notes.

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
- "Sounds like" from the audio itself, without AudioMuse-AI.
- ~~Loudness levelling on the server.~~ Done: songs without ReplayGain tags are measured in the background and the
  apps get the measured gain as their ReplayGain; Recital and share links level songs too. Still to do: levelling
  baked into conversions, for apps that can't level themselves.
- Jukebox mode (the server plays on its own speakers).

## Done on the way (0.4.0, 0.5.0)
Prelude sets up the whole music lab, a domain, and finds the music on the drives; backups; upgrades tested from every
earlier release; fast at 100,000 songs; works with web apps; shares, users, avatars; scrobbling for every user; a
picture for albums without one; "sounds like" without AI; two full reviews (about 100 fixes); a from-scratch install
tested.
