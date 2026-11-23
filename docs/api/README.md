# Encore extensions

Conductor speaks standard Subsonic 1.16.1 and OpenSubsonic. Extra features for Encore are advertised in
`getOpenSubsonicExtensions`, so any app can find and use them, and Encore can tell which servers have them.

Each extension has a spec here (linked below): what it's for, each call's parameters, and every field of its
answer with its types. The fields come from `shapes/`, the recorded shape of every call's answer (the standard
Subsonic calls too). The tests check every answer against them; CI records them again and fails if they change, so
from 1.0 a field can be added but never dropped or changed ([STABILITY.md](../STABILITY.md)). The specs are written by
`tools/api_docs.py`: edit its descriptions, then run it.

| Extension | Calls | Status |
|---|---|---|
| [`encore.devices`](encore.devices.md) | `deviceHello`, `deviceState`, `deviceList`, `deviceCommand`, `devicePoll`, `deviceBye` | done (same answers as music-lookup / Navidrome) |
| [`encore.sync`](encore.sync.md) | `syncGet`, `syncSet` | done (same answers as music-lookup / Navidrome) |
| [`encore.pairing`](encore.pairing.md) | `pairStart`, `pairPoll`, `pairApprove` | done (same answers as music-lookup / Navidrome) |
| [`encore.jam`](encore.jam.md) | `jamStart`, `jamState`, `jamPoll`, `jamEnd` (guests on a separate listener) | done (same answers as music-lookup / Navidrome) |
| [`encore.stats`](encore.stats.md) | `getListeningStats`, `getFolder` | done (same answers as music-lookup / Navidrome) |
| [`encore.smartPlaylists`](encore.smartPlaylists.md) | through the Navidrome stand-in: `POST /api/playlist` (rules), `PUT` / `GET /api/playlist/{id}` | done (Navidrome's rule format) |
| [`encore.playlistImages`](encore.playlistImages.md) | through the Navidrome stand-in: `POST` / `DELETE /api/playlist/{id}/image`; served as `pl-<id>` covers | done |
| [`encore.catalog`](encore.catalog.md) | ⬇ items in `search3` / `getAlbum` / `getArtist`, `dz-…` ids everywhere, `lookupSongs`, `getDiscoveries`, `getSongDiscoveries` | done (same answers as music-lookup, trial songs included) |
| [`encore.sonic`](encore.sonic.md) | `getSonicSimilarTracks`, `findSonicPath`, `/rest/audiomuse/<endpoint>` (allow-list) | done (AudioMuse's parameters and answers as documented for its Jellyfin plugin; Encore's own AudioMuse calls pass through unchanged) |
| [`encore.review`](encore.review.md) | `getListeningReview?period=year\|month&offset=` | done (music-lookup's answer; peak hour from unrounded time) |
| [`encore.collabPlaylists`](encore.collabPlaylists.md) | `playlistShareStart`, `playlistShareEnd`, `playlistShares`; friends at `/jam/pl/<token>` on the Jam listener | done (same answers as music-lookup) |
| [`encore.mixes`](encore.mixes.md) | `getMixes`, `setMix?kind=&enabled=`, `refreshMix?kind=` (discover · radar · daily · daylist) | done (per user; same answers as music-lookup) |
| [`encore.concerts`](encore.concerts.md) | `getConcerts`, `refreshConcerts` | done (per user; same answers as music-lookup) |
| [`encore.lyricsTranslation`](encore.lyricsTranslation.md) | `translateLyrics?id=&lang=&mode=translate\|romanize&line=…` | done (needs `ollama_url`; "ai-off" otherwise) |
| [`encore.import`](encore.import.md) | `POST importUpload?name=` (body: the export), `importOnline?source=spotify\|lastfm\|deezer&account=`, `getImports`, `getImport?id=`, `startImport?id=&kinds=&askMissing=`, `undoImport?id=`, `deleteImport?id=` | done: see `src/api/imports.rs` for the shapes |
| [`encore.dj`](encore.dj.md) | `getDjLine?next=&prev=&lang=` → `{djLine: {text, id, duration}}`, `djAudio?id=` (WAV) | done (needs `ollama_url` and `dj_voice_url`; "ai-off" otherwise) |
| [`encore.songId`](encore.songId.md) | `POST identifySong` (recording as the body) | done (needs `songid_url`) |
| [`encore.automix`](encore.automix.md) | `getMixInfo?id=&id=…` | done (needs `mixinfo_dir`) |
| [`encore.forYou`](encore.forYou.md) | `getForYou?count=` | done (Conductor's own "sounds like"; no AudioMuse needed) |
| [`encore.scrobbling`](encore.scrobbling.md) | `getScrobbling`, `setListenBrainz?token=` (empty unlinks), `lastfmLinkStart` → `{url, token}`, `lastfmLinkFinish?token=` → `waiting \| linked \| expired`, `lastfmUnlink` | done (per user; Last.fm needs `lastfm_api_key` and `lastfm_secret`; see `src/api/scrobbling.rs`) |
| [`encore.events`](encore.events.md) | `eventStream?id=&lastEventId=` → `text/event-stream`: devices, commands, Jam, sync, now playing, scan, playlists | done (Conductor's own; replaces Encore's long-polls and timers) |
