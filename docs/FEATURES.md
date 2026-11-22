# Everything Conductor does

The detailed list. The [README](../README.md) has the short version.

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
- **Works with web apps** served from another address (Feishin's web version, Airsonic-refix…): the Subsonic API
  answers them (CORS, as Navidrome does; the admin page doesn't). In Feishin, add Conductor as "OpenSubsonic".
- **Big libraries**: measured with 100,000 songs (`cargo run --release --example bench_library -- DIR 100000` makes
  one in seconds): first scan ~10 s, a rescan ~1 s, ~75 MB of memory; lists and albums in ~1-25 ms; an app fetching
  the whole library 500 songs at a time ~10 s in all (stable paging, nothing skipped or repeated).
- **Login**: Subsonic token (`t` + `s`), password (`p`, also `enc:`), and OpenSubsonic API keys (`apiKey`).
  Passwords are stored encrypted with a key in `secret.key` beside the database, never in it. Wrong passwords are
  limited per user per address (30 every 15 minutes from apps, 10 on the admin page); a right one resets the count.
- **Users** (`getUser`, `getUsers`, `createUser`, `updateUser`, `deleteUser`, `changePassword`, `getAvatar`): admins
  manage users from any app or Podium; everyone can change their own password; the last admin can't be removed or
  demoted. Two kinds of user, admins and everyone else; `getAvatar` draws a simple picture per user.
- **Shares** (`getShares`, `createShare`, `updateShare`, `deleteShare`): a link to songs, albums or playlists that
  anyone can play without an account, on the Jam listener at `jam_public_url`. The songs are fixed when the share is
  made; visitors get them by their place in the list, never an id, rate-limited per address; expiry and visit counts.
- **Backups**: `conductor backup [FILE]`, `conductor restore FILE [--force]`, and one a day in `<data>/backups` (the
  newest `backup_keep`, 7, kept). One private zip: a consistent copy of the database, `secret.key`, `config.toml`
  and `secrets.toml`, the playlist pictures. Restoring refuses while Conductor runs or over an existing library
  without `--force` (then it's moved aside, never deleted).
- **Smart playlists** in Navidrome's rule format (`all`/`any`, is, contains, gt/lt, inTheRange, inTheLast,
  before/after, inPlaylist…; sort, order, limit). They refill themselves when read (at most once a minute) and
  can't be edited by hand (`readonly`). `inPlaylist` only sees your own playlists and public ones.
- **Navidrome stand-in** for what Encore 1.7.9 uses from Navidrome's own API: `POST /auth/login`,
  `/api/playlist` (smart rules), `/api/playlist/{id}/image` (playlist pictures, served as `pl-<id>_…` covers).
- **Encore's own calls** (advertised as `encore.*` OpenSubsonic extensions, same answers as music-lookup gave):
  devices and remote control (`deviceHello/State/List/Command/Poll/Bye`, long-polls), sync between devices
  (`syncGet/syncSet`, newest wins, kept in the database), signing in from your phone (`pairStart/Poll/Approve`;
  at most 5 codes waiting from one address),
  listening stats (`getListeningStats`: week / month / year / all, local time), the year / month in review
  (`getListeningReview`: top fives, genres, first song, longest streak, peak hour, discoveries) and real folders
  (`getFolder`).
- **Chorus** (collaborative playlists): `playlistShareStart/End`, `playlistShares`; friends open the link (on the Jam
  listener, `/jam/pl/<token>`), search your library and add songs to the real playlist; they can take out only
  their own. Links last 30 days.
- **Repertoire**, your mixes (`getMixes`, `setMix`, `refreshMix`; per user, as real playlists): Discover Weekly (Mondays: 30
  songs you don't have, fetched on trial for a week), Release Radar (Fridays: new releases from followed and top
  artists), Daily Mix 1–6 (your library around your top artists, ~30% new to you), Daylist (what you play at this
  time of day). Discover and Radar need the catalog (Deezer + slskd).
- **Marquee**, concerts near you (`getConcerts`, `refreshConcerts`): weekly, upcoming shows of the artists you play and follow
  near the town set in Encore; Ticketmaster with the key set there, else MusicBrainz events (`concerts`).
- **Libretto**, lyrics translation (`translateLyrics`, optional): translated or romanized line by line by a local **Ollama**
  (`ollama_url`, `translate_model`); kept once done.
- **Interlude**, the DJ (`getDjLine` / `djAudio`, optional): before a song, a line or two about it written by the same Ollama
  (`dj_model`, else `translate_model`) and read aloud by Piper (the dj-voice service, `dj_voice_url`), in 13
  languages. Only facts the library has (title, artist, album, year, genre, your plays and hearts); lines are kept.
- **Pitch**, song ID (`identifySong`, optional): a short recording is matched against your own library by the song-id
  service (`songid_url`). **Automix** (`getMixInfo`, optional): intro / outro / tempo from the mix analyzer's
  `info.jsonl` (`mixinfo_dir`); songs not measured yet are put first in its `wanted.txt`.
- **Ensemble** (Jam): `jamStart/State/Poll/End` for the app; the guest page on its own listener (`jam_listen`, default
  127.0.0.1:4535: nothing else is served there), with rate limits and covers only from the queue or guests' searches.
  Set `jam_public_url` to the address guests open (e.g. through Tailscale Funnel).
- **Podcasts**: the whole Subsonic podcast API; feeds checked every 2 hours; episodes stream through from the
  podcast's server (with seeking) unless saved on the server (`downloadPodcastEpisode`, or `podcast_keep` newest
  per show); played saved episodes go after `podcast_clean_days`; covers fetched once and kept; progress and
  bookmarks per user. Episode and show ids are the same as music-lookup's. Adding, removing, refreshing and saving
  shows is for admins. Feeds, episodes and covers are fetched only from the internet, redirects included, never this
  computer or your network (`podcast_local = true` allows those).
- **Downloads are optional**: without slskd or Lidarr, songs you don't have aren't shown (`catalog` is off unless set),
  and everything else works. Prelude sets the downloader up as one Docker container (slskd; Linux or Windows with
  Docker), on this computer's connection or inside a VPN (gluetun), or uses your own slskd.
- **Prelude finds your music**: with no music folder yet (or `--music auto`), the drives are listed (lsblk or
  /proc/mounts on Linux, the logical drives on Windows; label, size, free space, USB or not) with the music folders
  found near the top of each and their songs counted (a quick look: 5 seconds and 100,000 songs a drive at most).
  It offers the one with the most songs, else a new folder on the drive with the most free space (the downloads
  beside it), and on a USB drive the drive guard's file.
- **Prelude's services folder** (`services/` beside config.toml, one compose project, `docker compose up -d --build`):
  each service can be set up for you there, be one you already run, or be off, and its address and keys go into
  Conductor's settings by themselves. The downloader (`downloads.yml`); Lidarr (its key shared with Conductor, your
  music as its root folder); AudioMuse-AI (its database, web app and worker; it reads the library with its own,
  non-admin Conductor account); Pitch and Metronome (built on your computer from `helpers/`, seeing every music folder); and only with AI on,
  Ollama (or the one already running) and Interlude's voice. Everything listens on 127.0.0.1, on ports that don't
  clash; keys and logins are in files only you can read. `--docker lidarr,audiomuse,pitch,metronome,ai` does the
  same for `--yes` runs. Run again, each part keeps its port and the keys and passwords the service already holds,
  and a downloader inside the VPN stays inside it. Pitch's memory limit follows the number of songs.
- **On a domain** (`conductor setup domain`): your own domain with HTTPS (Caddy in the services folder, the
  certificate from Let's Encrypt, `/jam/` or a second name to the Jam listener; it checks the name points at this
  computer, and offers listening on 127.0.0.1 only), a Cloudflare Tunnel (no ports to open; the token in a file
  only you can read), or Tailscale Funnel (the commands shown, run only with a yes). Each sets `jam_public_url`.
- **Stagehand**, songs you don't have (from Deezer, marked ⬇ / ⏳ / ✕, `lookupState` for Encore): search also lists songs,
  albums and artists you don't have; an album shows its whole tracklist; an artist also lists the albums you don't
  have; `dz-…` ids work everywhere (a 30-second preview plays until the song is here). Heart a ⬇ song or album, or
  add a ⬇ song to a playlist: it's searched on Soulseek through **slskd** (one search every 12 s; the same album
  first, then FLAC over 320, right title/artist/length, no live/remix versions unless asked), downloaded, filed as
  `Songs/<Artist>/<Artist> - <Title>.<ext>`, scanned, then hearted / added. Also `lookupSongs`, `getDiscoveries`,
  `getSongDiscoveries`. Set `slskd_url`, `slskd_api_key` and `slskd_downloads`; filing needs **write access** to the
  music folder. Apps listed in `catalog_skip_clients` (default: Symfonium) never get ⬇ items.
- **Stagehand**: better copies (`upgrades`, on): when there's nothing else to download, a song it filed from another album than
  the one asked for is looked for once on that album, and a lossy one as FLAC. The new file is checked (it reads, the
  right length) before it replaces the old one; the song keeps its id, hearts, plays and playlists. Only songs in
  `Songs/`, never Lidarr's album folders.
- **Audition** (songs on trial): play or queue a ⬇ song (any app's "now playing", or Encore's `lookupSongs?fetch=true`) and it's
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
- **Scrobbling, for everyone** (`encore.scrobbling`): each user links their own **ListenBrainz** (their token,
  checked with ListenBrainz) and **Last.fm** (approved on Last.fm's own page; the server needs `lastfm_api_key` and
  `lastfm_secret`), in Encore's settings. Plays and "playing now" go to every service they linked; plays a service
  couldn't take are kept and sent later, in order. Tokens and session keys are stored encrypted, as passwords are; a
  link Last.fm stops accepting is shown as needing renewal. The settings' `listenbrainz_token` is the owner's.
- **ListenBrainz discovery**: its playlists made
  for you (`listenbrainz_user`; Weekly Exploration, Weekly Jams, Daily Jams — `discovery`) become playlists: songs
  you have go straight in, the rest are downloaded into `Discovery/` and join as they arrive; when the next list
  comes, songs you didn't keep (heart, 4+, another playlist) are deleted and kept ones move to `Songs/`. (Explo's job.)
- **Spotify sync** (spotify-sync + `music-download songs`): every 6 hours your Spotify Liked Songs and chosen playlists
  become playlists here ("Synced from Spotify"), in Spotify's order, Liked Songs hearted; songs you don't have are
  matched on Deezer (ISRC first) and asked for. The login and choices come from spotify.py's folder via
  `import-music-lookup`.
- **Imports waiting**: at most 5 per user (being read, or read and not started), each up to 2 GB; start or discard
  one to make room. Uploads not started are removed after a day.
- **Transpose**, imports from other services (`conductor import`, Podium's Transpose tab, `encore.import`): your liked songs
  (→ hearts), saved albums (→ starred), followed artists (→ follows), playlists (in order), listening history (→ plays
  with their times; under 30 s → skips), ratings, play counts and podcasts, from:
  Spotify (account data, extended streaming history, Exportify CSVs, or the signed-in account), Apple Music / iTunes
  (Library.xml, Apple's privacy export), YouTube Music (Google Takeout; history as JSON), Last.fm (by user name;
  `lastfm_api_key`), ListenBrainz's export, Deezer (a public profile), and M3U / CSV playlists. The format is told
  from the files. Songs are matched by Spotify id, ISRC, MusicBrainz id or path first, then by name; you see what
  was found, the match rate and what would change before anything does; plays already there aren't added twice;
  each import can be undone; songs you don't have can be asked for (off unless you give a number). Only music data is
  read: IP addresses, countries, devices and searches in the exports are never stored, and an upload is deleted
  once imported. `--to URL --as USER` sends an import to any Subsonic server instead (stars, playlists, past plays
  as scrobbles with their time: checked with Navidrome 0.64); note that such a server passes the plays on to
  ListenBrainz / Last.fm if you linked them there.
- **Recital**, the web player at `/` (also `/web`), built in: sign in, browse, search, queue, star, scrobble; phone and desktop, no outside files.
- **Podium**, the admin page at `/admin` (admins only): status, scan now, songs asked for (retry / cancel), picking a Soulseek file by hand for songs the automatic search couldn't get, Lidarr albums,
  users (add, set password, new API key).
- `GET /health` says `ok` or `degraded` (a music folder isn't connected).

See `docs/api/` for the extension list.
