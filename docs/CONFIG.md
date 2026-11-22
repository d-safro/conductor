# Settings

Every setting Conductor reads. They go in `config.toml` (Prelude, `conductor setup`, writes it for you; see
`config.example.toml`). Secrets (marked 🔑) go in `secrets.toml` beside it, which only you can read; Prelude puts
them there. Every setting can also come from the environment as `CONDUCTOR_` + its name in capitals (handy for Docker), which
wins over the file: `CONDUCTOR_TRIAL_PER_DAY=5`, `CONDUCTOR_SPOTIFY_SYNC=true`, lists comma-separated
(`CONDUCTOR_DISCOVERY=weekly-jams,daily-jams`). Two have their own names: the music folder is `CONDUCTOR_MUSIC`, and
`covers_fetch` is `CONDUCTOR_COVERS`. The old `MAESTRO_` names still work.

Paths may use `~`. A setting that isn't there takes the default shown.

## Basics

| Setting | Default | What it does |
|---|---|---|
| `data_dir` | see below | Where the database, the password key and caches live. Keep it on the internal disk, not the music drive. |
| `listen` | "0.0.0.0:4533" | Address for the apps (Subsonic API). |
| `libraries` | [] | Music folders. Each one is a library. Environment: `CONDUCTOR_MUSIC`. |
| `read_only` | false | Never write to the music folders: no downloads filed, no trial songs moved or deleted, no covers saved. For running beside another server (a trial run) on the same music. |
| `scan_interval_min` | 15 | Minutes between full scans (0: only at start and on request). |
| `ffmpeg` | (not set) | ffmpeg for transcoding; found on PATH when not set. |
| `backup_keep` | 7 | Daily backups kept (0: none) and where (default: <data_dir>/backups). See src/backup.rs. |
| `backup_dir` | (not set) | Daily backups kept (0: none) and where (default: <data_dir>/backups). See src/backup.rs. |

## Ensemble (Jam), Chorus and share links

| Setting | Default | What it does |
|---|---|---|
| `jam_listen` | "127.0.0.1:4535" | Address of the Jam guest page (the only thing to put on the internet). Empty string: no Jam listener. |
| `jam_public_url` | (not set) | The guest page's public address (e.g. https://music.example.com:8443). Without it, Jam can't start. |

## Podcasts

| Setting | Default | What it does |
|---|---|---|
| `podcast_dir` | (not set) | Where podcast episodes saved on the server go (default: <data_dir>/podcasts). Not the music folder. |
| `podcast_keep` | 0 | Also keep the newest N episodes of every show saved on the server (0: only the ones asked for). |
| `podcast_clean_days` | 7 | Played episodes saved on the server go after this many days (0: never). |
| `podcast_local` | false | Let podcast feeds, episodes and pictures come from this computer or your own network (off: internet only). |

## Stagehand: songs you don't have, and downloads

| Setting | Default | What it does |
|---|---|---|
| `catalog` | true | Songs you don't have (⬇) in searches, albums and artists (from Deezer). When it isn't set, it's on only if something can get those songs (slskd or Lidarr): without downloads, nothing shows that can't be played. |
| `catalog_skip_clients` | ["symfonium", "audiomuse"] | Apps that never get ⬇ items (part of the client name, any case). |
| `deezer_url` | "https://api.deezer.com" | Apps that never get ⬇ items (part of the client name, any case). |
| `slskd_url` | (not set) | slskd (Soulseek) for downloading songs asked for: its address and API key. |
| `slskd_api_key` | (not set) | slskd (Soulseek) for downloading songs asked for: its address and API key. |
| `slskd_downloads` | (not set) | slskd's finished-downloads folder, as Conductor sees it (path mapping when they run on different systems). |
| `import_library` | (not set) | The music folder downloads are filed into (a library name; default: the first). |
| `trial_hours` | 24.0 | Songs you play but don't have go on trial: deleted this long after they arrive unless you keep them. |
| `trial_per_day` | 40 | At most this many new trial downloads a day. |
| `upgrades` | true | Look for a better copy of songs it downloaded (the album asked from, FLAC over lossy) when there's nothing else to do, and swap it in (same song id). |

## Downloads through a VPN or proxy

| Setting | Default | What it does |
|---|---|---|
| `vpn` | "none" | Downloads through a VPN (Prelude sets these): "none", "existing" (slskd already runs behind one), "gluetun" (slskd inside a gluetun container Prelude set up) or "proxy" (Conductor's own download traffic through `proxy`). |
| `vpn_check_url` | (not set) | Asked for the public address the downloads use: JSON with "public_ip" or "ip" (and "country"), e.g. gluetun's control server at /v1/publicip/ip. The address itself is never logged or shown. |
| `vpn_check_key` | (not set) | Sent as X-API-Key to vpn_check_url (gluetun's control server). A secret: keep it in secrets.toml. |
| `vpn_ip_url` | "https://ipinfo.io/json" | Where this computer's own public address is asked for, to compare (any JSON answer with "ip"). |
| `downloads_require_vpn` | false | The kill switch: while the VPN check fails, downloads wait (never over the open connection). |
| `vpn_check_secs` | 180 | Seconds between VPN checks. |
| `vpn_port_sync` | false | Keep slskd's Soulseek listen port on the VPN's forwarded port (from gluetun's control server). |
| `proxy` | (not set) | A SOCKS5 or HTTP proxy for Conductor's own download traffic (the ⬇ catalog and its previews), e.g. socks5h://user:pass@127.0.0.1:1080. A secret when it has a password: keep it in secrets.toml. |
| `proxy_metadata` | false | Also send cover, lyrics and artist-info fetches through the proxy. |

## Lidarr

| Setting | Default | What it does |
|---|---|---|
| `lidarr_url` | (not set) | Lidarr: albums it wants are fetched from Soulseek (one folder from one user) and handed to it to import. |
| `lidarr_api_key` | (not set) | Lidarr: albums it wants are fetched from Soulseek (one folder from one user) and handed to it to import. |
| `lidarr_downloads` | (not set) | The downloads folder as Lidarr sees it (default: the same as slskd_downloads). |
| `lidarr_interval_min` | 5 | Minutes between looks at Lidarr's wanted list, and how many albums to take each time. |
| `lidarr_albums_per_run` | 20 | Minutes between looks at Lidarr's wanted list, and how many albums to take each time. |
| `lidarr_match_ratio` | 0.8 | Smallest share of an album's track names that must match a folder's file names (0..1). |

## Lyrics, covers and artist info

| Setting | Default | What it does |
|---|---|---|
| `lyrics_online` | true | Lyrics from LRCLIB for songs that have none (no .lrc, nothing in the tags). |
| `lrclib_url` | "https://lrclib.net" | Lyrics from LRCLIB for songs that have none (no .lrc, nothing in the tags). |
| `covers_fetch` | true | Save a cover.jpg (from iTunes, then Deezer) into album folders that have no picture. Environment: `CONDUCTOR_COVERS`. |
| `itunes_url` | "https://itunes.apple.com" | Save a cover.jpg (from iTunes, then Deezer) into album folders that have no picture. |
| `info_online` | true | Artist biographies and album notes (MusicBrainz → Wikidata → Wikipedia, pictures from the Cover Art Archive): looked up when an app first asks, then kept. `info_lang` is Wikipedia's language code. |
| `info_lang` | "en" | Look for a better copy of songs it downloaded (the album asked from, FLAC over lossy) when there's nothing else to do, and swap it in (same song id). |
| `wikidata_url` | "https://www.wikidata.org" | Look for a better copy of songs it downloaded (the album asked from, FLAC over lossy) when there's nothing else to do, and swap it in (same song id). |
| `wikipedia_url` | "https://{lang}.wikipedia.org" | "{lang}" is replaced with the language |
| `musicbrainz_url` | "https://musicbrainz.org/ws/2" | Concerts near you: Ticketmaster's Discovery API (with the key set in Encore), else MusicBrainz events. |
| `coverart_url` | "https://coverartarchive.org" | "{lang}" is replaced with the language |

## Concerts (Marquee)

| Setting | Default | What it does |
|---|---|---|
| `concerts` | true | Concerts near you: Ticketmaster's Discovery API (with the key set in Encore), else MusicBrainz events. |
| `ticketmaster_url` | "https://app.ticketmaster.com/discovery/v2" | Concerts near you: Ticketmaster's Discovery API (with the key set in Encore), else MusicBrainz events. |

## ListenBrainz, Last.fm and Spotify

| Setting | Default | What it does |
|---|---|---|
| `listenbrainz_user` | (not set) | ListenBrainz: plays are sent there (as Navidrome did), and its weekly playlists become discovery playlists. |
| `listenbrainz_token` | (not set) | ListenBrainz: plays are sent there (as Navidrome did), and its weekly playlists become discovery playlists. |
| `listenbrainz_for` | (not set) | Whose plays go to ListenBrainz and who owns the discovery playlists (default: the first admin). |
| `listenbrainz_url` | "https://api.listenbrainz.org" | Whose plays go to ListenBrainz and who owns the discovery playlists (default: the first admin). |
| `discovery` | ["weekly-exploration", "weekly-jams", "daily-jams"] | Which ListenBrainz playlists to turn into discovery playlists ([] = none). |
| `lastfm_url` | "https://ws.audioscrobbler.com/2.0" | Imports from Last.fm (scrobbles and loved songs by username) need an API key (last.fm/api/account/create). |
| `lastfm_api_key` | (not set) | Imports from Last.fm (scrobbles and loved songs by username) need an API key (last.fm/api/account/create). |
| `spotify_sync` | true | Spotify sync (spotify.py's "songs" mode): your Liked Songs and chosen playlists become playlists here; songs you don't have are asked for. The login and choices come from `import-music-lookup` (spotify/ folder). |
| `spotify_interval_hours` | 6 | Spotify sync (spotify.py's "songs" mode): your Liked Songs and chosen playlists become playlists here; songs you don't have are asked for. The login and choices come from `import-music-lookup` (spotify/ folder). |
| `spotify_api_url` | "https://api.spotify.com/v1" | Spotify sync (spotify.py's "songs" mode): your Liked Songs and chosen playlists become playlists here; songs you don't have are asked for. The login and choices come from `import-music-lookup` (spotify/ folder). |
| `spotify_accounts_url` | "https://accounts.spotify.com" | Spotify sync (spotify.py's "songs" mode): your Liked Songs and chosen playlists become playlists here; songs you don't have are asked for. The login and choices come from `import-music-lookup` (spotify/ folder). |

## AudioMuse-AI, Pitch and Metronome

| Setting | Default | What it does |
|---|---|---|
| `audiomuse_url` | (not set) | AudioMuse-AI (optional): sound-alike songs, paths between sounds, Encore's recommendation calls. |
| `audiomuse_token` | (not set) | AudioMuse-AI (optional): sound-alike songs, paths between sounds, Encore's recommendation calls. |
| `songid_url` | (not set) | The song-id service (optional): "What's playing?" against your own library. POST /identify, GET /status. |
| `mixinfo_dir` | (not set) | Automix: the mix analyzer's folder (info.jsonl in, wanted.txt out; paths relative to the music folder). |

## AI (off unless set): Libretto and Interlude

| Setting | Default | What it does |
|---|---|---|
| `ollama_url` | (not set) | Ollama (optional, local AI): lyrics translation and romanization. Off when not set. |
| `translate_model` | "gemma4:e2b-it-qat" | Ollama (optional, local AI): lyrics translation and romanization. Off when not set. |
| `dj_model` | (not set) | Encore's DJ: lines written by the Ollama above (`dj_model`, else `translate_model`) and read by Piper (the dj-voice service: POST /speak). Off unless both are set. |
| `dj_voice_url` | (not set) | Encore's DJ: lines written by the Ollama above (`dj_model`, else `translate_model`) and read by Piper (the dj-voice service: POST /speak). Off unless both are set. |

## Music folders

Each `[[libraries]]` entry is one music folder:

```toml
[[libraries]]
name = "Music"
path = "/srv/music"
sentinel = ".conductor-drive"   # optional: a file that must be there, or the drive counts as not connected
scan_threads = 4                  # optional
```

With the environment alone: `CONDUCTOR_MUSIC=/path/to/music` (one folder; `CONDUCTOR_SENTINEL`,
`CONDUCTOR_SCAN_THREADS`).

## Where data goes

`data_dir` defaults to `~/.local/share/conductor` on Linux and `%ProgramData%\conductor` on Windows (the service),
and `/data` in the Docker image. The settings file is `config.toml` in it unless `--config` or `CONDUCTOR_CONFIG`
says otherwise.
