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
5. **A last security pass on what faces the internet**, now that Prelude puts Conductor on a domain: headers, rate
   limits, `/setup`, Jam and share pages behind Caddy and tunnels. (The dependency vulnerability check is in CI now;
   its first run found the bundled SQLite's CVE, fixed by moving to sqlx 0.9.)
6. **A load test.** Many streams and conversions at once, a full rescan while playing, and a cap on how many ffmpeg
   conversions run at the same time.

## Should be in 1.0
7. **Settings in Podium**, so nothing needs the terminal after setup (the Last.fm key and secret, ListenBrainz,
   downloads on or off, online lookups…).
8. **Encore's "Made for you" without AudioMuse-AI**, from Conductor's own "sounds like" (0.5.0). Moods need audio
   analysis and stay AudioMuse's.
9. **Linux packages** for Conductor (.deb, .rpm, Arch), as Encore has.
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
