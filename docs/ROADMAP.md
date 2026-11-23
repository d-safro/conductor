# To 1.0

What's left before Conductor 1.0, and what comes after. 1.0 means the promises in [STABILITY.md](STABILITY.md)
start: your data, settings, the API and Conductor's own extensions keep working across every 1.x release.

## Needed for 1.0
1. ~~**Specs for Conductor's own API extensions.**~~ Done: a spec per `encore.*` extension in [api/](api/) (calls,
   parameters, every field of the answer), and the recorded shape of every call's answer (117 calls), checked by the
   tests and by CI, so a change can add fields but never drop or change one.
2. **Other apps, for real.** Used against a real server, one by one, and fixed for what they trip over: Symfonium,
   Tempo, Ultrasonic / DSub, Substreamer (Android); Feishin and Supersonic (desktop); Airsonic-refix (web); Amperfy
   and play:Sub (iPhone). Until now only Encore, Feishin's web version and scripted calls have been.
3. **Windows on a real PC.** Installed with `install.ps1` as a service, reached from a phone through the firewall,
   with the music on a USB drive (the drive guard). CI covers the tests and the service install, not a real machine.
4. **A release candidate run.** 1.0-rc on a real home server for a couple of weeks: memory, errors, scans and the
   daily backups looked at along the way.
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
10. **Accessibility and phone layout** of Recital and Podium, checked.

## After 1.0
- Live updates (an event stream) instead of long-polls; Discord presence from it.
- "Sounds like" from the audio itself, without AudioMuse-AI.
- Loudness levelling on the server (ReplayGain applied when converting).
- Jukebox mode (the server plays on its own speakers).

## Done on the way (0.4.0, 0.5.0)
Prelude sets up the whole music lab, a domain, and finds the music on the drives; backups; upgrades tested from every
earlier release; fast at 100,000 songs; works with web apps; shares, users, avatars; scrobbling for every user; a
picture for albums without one; "sounds like" without AI; two full reviews (about 100 fixes); a from-scratch install
tested.
