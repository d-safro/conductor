# Maestro

A music server for [Encore](https://github.com/d-safro/encore) and for any app that speaks the
**Subsonic / OpenSubsonic** API. It's written in Rust and runs as a single program on Linux and Windows.

> Early development. Licence: AGPL-3.0-or-later (see `LICENSE`).

## What works now

- **Library**: scans music folders (FLAC, MP3, Opus, Ogg, M4A/AAC, WAV, AIFF, APE, WavPack, …), reads tags
  (title, artists, album artist, track/disc, year, genres — every genre tag, split at `;` `/` `,` as Navidrome
  does — MusicBrainz ids), and gives each song, album and artist an
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
  `getTopSongs`, `getSimilarSongs(2)` (AudioMuse's sound-alikes, then songs by the artist and Deezer's related
  artists that you have, then the genre), `getArtistInfo(2)` (Wikipedia biography through MusicBrainz and
  Wikidata, pictures and similar artists from Deezer), `getAlbumInfo(2)` (Wikipedia notes, Cover Art Archive
  pictures; both looked up once when first asked, then kept; `info_online`, `info_lang`),
  playlists (all), play queue, `getUser`, internet radio (all), `getScanStatus`, `startScan`, `stream`
  (direct with HTTP Range, or transcoded to mp3/opus/ogg/aac with `maxBitRate`, `format`, `timeOffset`),
  `download`, `getCoverArt` (folder images, then embedded; resized and cached), `getLyrics`, `getLyricsBySongId`
  (`.lrc` files next to the song, embedded lyrics, else **LRCLIB** online, remembered; synced when timed).
- **Covers into album folders**: every 6 hours, album folders (`Albums/Artist/Album`, holding just that album) with no
  picture get a `cover.jpg` from iTunes, then Deezer; nothing is ever overwritten (`covers_fetch`).
- **Login**: Subsonic token (`t` + `s`), password (`p`, also `enc:`), and OpenSubsonic API keys (`apiKey`).
  Passwords are stored encrypted with a key in `secret.key` beside the database, never in it.
- **Smart playlists** in Navidrome's rule format (`all`/`any`, is, contains, gt/lt, inTheRange, inTheLast,
  before/after, inPlaylist…; sort, order, limit). They refill themselves when read (at most once a minute) and
  can't be edited by hand (`readonly`).
- **Navidrome stand-in** for what Encore 1.7.9 uses from Navidrome's own API: `POST /auth/login`,
  `/api/playlist` (smart rules), `/api/playlist/{id}/image` (playlist pictures, served as `pl-<id>_…` covers).
- **Encore's own calls** (advertised as `encore.*` OpenSubsonic extensions, same answers as music-lookup gave):
  devices and remote control (`deviceHello/State/List/Command/Poll/Bye`, long-polls), sync between devices
  (`syncGet/syncSet`, newest wins, kept in the database), signing in from your phone (`pairStart/Poll/Approve`),
  listening stats (`getListeningStats`: week / month / year / all, local time), the year / month in review
  (`getListeningReview`: top fives, genres, first song, longest streak, peak hour, discoveries) and real folders
  (`getFolder`).
- **Collaborative playlists**: `playlistShareStart/End`, `playlistShares`; friends open the link (on the Jam
  listener, `/jam/pl/<token>`), search your library and add songs to the real playlist; they can take out only
  their own. Links last 30 days.
- **Your mixes** (`getMixes`, `setMix`, `refreshMix`; per user, as real playlists): Discover Weekly (Mondays: 30
  songs you don't have, fetched on trial for a week), Release Radar (Fridays: new releases from followed and top
  artists), Daily Mix 1–6 (your library around your top artists, ~30% new to you), Daylist (what you play at this
  time of day). Discover and Radar need the catalog (Deezer + slskd).
- **Concerts near you** (`getConcerts`, `refreshConcerts`): weekly, upcoming shows of the artists you play and follow
  near the town set in Encore; Ticketmaster with the key set there, else MusicBrainz events (`concerts`).
- **Lyrics translation** (`translateLyrics`, optional): translated or romanized line by line by a local **Ollama**
  (`ollama_url`, `translate_model`); kept once done.
- **Song ID** (`identifySong`, optional): a short recording is matched against your own library by the song-id
  service (`songid_url`). **Automix** (`getMixInfo`, optional): intro / outro / tempo from the mix analyzer's
  `info.jsonl` (`mixinfo_dir`); songs not measured yet are put first in its `wanted.txt`.
- **Jam**: `jamStart/State/Poll/End` for the app; the guest page on its own listener (`jam_listen`, default
  127.0.0.1:4535: nothing else is served there), with rate limits and covers only from the queue or guests' searches.
  Set `jam_public_url` to the address guests open (e.g. through Tailscale Funnel).
- **Podcasts**: the whole Subsonic podcast API; feeds checked every 2 hours; episodes stream through from the
  podcast's server (with seeking) unless saved on the server (`downloadPodcastEpisode`, or `podcast_keep` newest
  per show); played saved episodes go after `podcast_clean_days`; covers fetched once and kept; progress and
  bookmarks per user. Episode and show ids are the same as music-lookup's.
- **Songs you don't have** (from Deezer, marked ⬇ / ⏳ / ✕, `lookupState` for Encore): search also lists songs,
  albums and artists you don't have; an album shows its whole tracklist; an artist also lists the albums you don't
  have; `dz-…` ids work everywhere (a 30-second preview plays until the song is here). Heart a ⬇ song or album, or
  add a ⬇ song to a playlist: it's searched on Soulseek through **slskd** (one search every 12 s; the same album
  first, then FLAC over 320, right title/artist/length, no live/remix versions unless asked), downloaded, filed as
  `Songs/<Artist>/<Artist> - <Title>.<ext>`, scanned, then hearted / added. Also `lookupSongs`, `getDiscoveries`,
  `getSongDiscoveries`. Set `slskd_url`, `slskd_api_key` and `slskd_downloads`; filing needs **write access** to the
  music folder. Apps listed in `catalog_skip_clients` (default: Symfonium) never get ⬇ items.
- **Songs on trial**: play or queue a ⬇ song (any app's "now playing", or Encore's `lookupSongs?fetch=true`) and it's
  downloaded into `Trial/` and listed in your "On trial" playlist; `stream?hold=true` plays silence until it's here.
  Heart it, add it to another playlist or rate it 4+ to keep it (it moves to `Songs/`, same id); take it out of
  "On trial" to delete it now; the rest go `trial_hours` (24) after they arrive; at most `trial_per_day` (40) a day.
- **Lidarr's wanted albums** (what Soularr did): every `lidarr_interval_min` (5), up to `lidarr_albums_per_run` (20)
  missing albums are searched on Soulseek; one user's folder with the whole album (as many FLAC / MP3 320 files as
  tracks, `lidarr_match_ratio` of the track names, the artist in the path) is downloaded, renamed
  "<Artist> - <Album>" and handed to Lidarr (`DownloadedAlbumsScan`). A user whose copy fails isn't tried again.
  Set `lidarr_url`, `lidarr_api_key`, and `lidarr_downloads` when Lidarr sees the downloads folder at another path.
- **AudioMuse-AI** (optional; set `audiomuse_url` / `audiomuse_token`): `getSonicSimilarTracks` and `findSonicPath`
  from AudioMuse's analysis, and Encore's read-only recommendation calls at `/rest/audiomuse/<endpoint>` (an
  allow-list, logins never passed on). Without it these say "not available" and apps fall back.
- **ListenBrainz**: plays are sent there (`listenbrainz_token`; failed ones resent later), and its playlists made
  for you (`listenbrainz_user`; Weekly Exploration, Weekly Jams, Daily Jams — `discovery`) become playlists: songs
  you have go straight in, the rest are downloaded into `Discovery/` and join as they arrive; when the next list
  comes, songs you didn't keep (heart, 4+, another playlist) are deleted and kept ones move to `Songs/`. (Explo's job.)
- **Spotify sync** (spotify-sync + `music-download songs`): every 6 hours your Spotify Liked Songs and chosen playlists
  become playlists here ("Synced from Spotify"), in Spotify's order, Liked Songs hearted; songs you don't have are
  matched on Deezer (ISRC first) and asked for. The login and choices come from spotify.py's folder via
  `import-music-lookup`.
- **Admin page** at `/admin` (admins only): status, scan now, songs asked for (retry / cancel), picking a Soulseek file by hand for songs the automatic search couldn't get, Lidarr albums,
  users (add, set password, new API key).
- `GET /health` says `ok` or `degraded` (a music folder isn't connected).

Coming next: album info, upgrades to the same-album copy. See `docs/api/` for the extension list.

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
Then `maestro import-music-lookup <music-lookup DATA folder> [<its state folder with podcasts.json>]` adds
music-lookup's listening log, Encore's sync values and the podcasts (plays already brought over from Navidrome
aren't counted twice).

Docker, Windows (`maestro.exe`, cross-built in a container): see `deploy/README.md`.

## Develop

```bash
cargo test          # unit tests + an end-to-end test that makes a tiny music folder with ffmpeg
cargo clippy --all-targets
```

CI (`.github/workflows/ci.yml`) runs both on Linux and Windows.

> Scratch runs against a real music folder: set `MAESTRO_READ_ONLY=1`, or downloads, trial songs and covers write into it.
