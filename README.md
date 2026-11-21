# Maestro

A music server for [Encore](https://github.com/d-safro/encore) and for any app that speaks the
**Subsonic / OpenSubsonic** API. It's written in Rust and runs as a single program on Linux and Windows.

> Early development. Licence: AGPL-3.0-or-later (see `LICENSE`).

## What works now

- **Library**: scans music folders (FLAC, MP3, Opus, Ogg, M4A/AAC, WAV, AIFF, APE, WavPack, …), reads tags
  (title, artists, album artist, track/disc, year, genre, MusicBrainz ids), and gives each song, album and artist an
  id that stays the same across scans. One album per album artist + album name. Unchanged files are skipped, so a
  rescan takes milliseconds.
- **Drive guard**: a music folder that's gone, empty, or missing its sentinel file counts as *not connected*.
  Then nothing is scanned or marked missing, browsing keeps working from the database, and streams answer at once
  with "the music drive isn't connected". A scan that would suddenly lose more than half the songs leaves them alone.
  The server rescans as soon as the folder is back.
- **Subsonic API** (`/rest/<method>`, JSON and XML, GET and form POST): `ping`, `getLicense`,
  `getOpenSubsonicExtensions`, `getMusicFolders`, `getArtists`, `getIndexes`, `getArtist`, `getAlbum`, `getSong`,
  `getMusicDirectory`, `getAlbumList(2)` (all list types), `getRandomSongs`, `getSongsByGenre`, `getGenres`,
  `search2/3`, `getStarred(2)`, `star`, `unstar`, `setRating`, `scrobble`, `reportPlayback`, `getNowPlaying`,
  `getTopSongs`, `getSimilarSongs(2)` (local only for now), `getArtistInfo(2)`/`getAlbumInfo(2)` (empty for now),
  playlists (all), play queue, `getUser`, internet radio (all), `getScanStatus`, `startScan`, `stream`
  (direct with HTTP Range, or transcoded to mp3/opus/ogg/aac with `maxBitRate`, `format`, `timeOffset`),
  `download`, `getCoverArt` (folder images, then embedded; resized and cached), `getLyrics`, `getLyricsBySongId`
  (`.lrc` files next to the song, or embedded lyrics; synced when timed).
- **Login**: Subsonic token (`t` + `s`), password (`p`, also `enc:`), and OpenSubsonic API keys (`apiKey`).
  Passwords are stored encrypted with a key in `secret.key` beside the database, never in it.
- `GET /health` says `ok` or `degraded` (a music folder isn't connected).

Coming next: the Encore extensions (devices, sync, sign-in pairing, Jam, stats, smart playlists, playlist pictures),
a temporary stand-in for the few Navidrome-only calls Encore 1.7.9 makes, podcasts, and
downloading songs you don't have (slskd, Lidarr). See `docs/api/` for the extension list.

## Run it

Needs **ffmpeg** on the PATH for transcoding (playing files as they are works without it).

```bash
cargo build --release
cp config.example.toml config.toml        # set your music folder
MAESTRO_PASSWORD='…' ./target/release/maestro --config config.toml user add me --admin
./target/release/maestro --config config.toml          # serves on 127.0.0.1:4533
```

Or with the environment only: `MAESTRO_MUSIC=/path/to/music MAESTRO_ADMIN_USER=me MAESTRO_ADMIN_PASSWORD=… maestro`.

Other commands: `scan [--full]`, `user passwd NAME`, `apikey NAME [LABEL]` (prints a new API key).

**Moving over from Navidrome:** stop Navidrome, copy its data folder, then
`maestro import-navidrome /copy/navidrome.db --artwork /copy/artwork`. Songs, albums and artists keep
Navidrome's ids (so apps that remember ids keep working), and plays, ratings, stars, play history, playlists (with
their pictures and smart rules), play queues and radio stations come along. Users come over by name; set each
password again with `user passwd NAME`. Running it again replaces what it brought over the last time.

Docker: `docker build -f deploy/Dockerfile -t maestro .` (see `deploy/compose.example.yml`).

## Develop

```bash
cargo test          # unit tests + an end-to-end test that makes a tiny music folder with ffmpeg
cargo clippy --all-targets
```

CI (`.github/workflows/ci.yml`) runs both on Linux and Windows.
