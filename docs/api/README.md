# Encore extensions

Maestro speaks standard Subsonic 1.16.1 and OpenSubsonic. Extra features for Encore are advertised in
`getOpenSubsonicExtensions`, so any app can find and use them, and Encore can tell which servers have them.
Each one gets its own spec file here before it's built.

| Extension | Calls | Status |
|---|---|---|
| `encore.devices` | `deviceHello`, `deviceState`, `deviceList`, `deviceCommand`, `devicePoll`, `deviceBye` | done (same answers as music-lookup / Navidrome) |
| `encore.sync` | `syncGet`, `syncSet` | done (same answers as music-lookup / Navidrome) |
| `encore.pairing` | `pairStart`, `pairPoll`, `pairApprove` | done (same answers as music-lookup / Navidrome) |
| `encore.jam` | `jamStart`, `jamState`, `jamPoll`, `jamEnd` (guests on a separate listener) | done (same answers as music-lookup / Navidrome) |
| `encore.stats` | `getListeningStats`, `getFolder` | done (same answers as music-lookup / Navidrome) |
| `encore.smartPlaylists` | `createSmartPlaylist`, `updateSmartPlaylist`, `getSmartPlaylistRules` | done (same answers as music-lookup / Navidrome) |
| `encore.playlistImages` | `setPlaylistImage`, `deletePlaylistImage` | done (same answers as music-lookup / Navidrome) |
| `encore.catalog` | ⬇ items in `search3` / `getAlbum` / `getArtist`, `dz-…` ids everywhere, `lookupSongs`, `getDiscoveries`, `getSongDiscoveries` | done (same answers as music-lookup, trial songs included) |
| `encore.sonic` | `getSonicSimilarTracks`, `findSonicPath`, `/rest/audiomuse/<endpoint>` (allow-list) | done (AudioMuse's parameters and answers as documented for its Jellyfin plugin; Encore's own AudioMuse calls pass through unchanged) |
| `encore.review` | `getListeningReview?period=year\|month&offset=` | done (music-lookup's answer; peak hour from unrounded time) |
| `encore.collabPlaylists` | `playlistShareStart`, `playlistShareEnd`, `playlistShares`; friends at `/jam/pl/<token>` on the Jam listener | done (same answers as music-lookup) |
| `encore.mixes` | `getMixes`, `setMix?kind=&enabled=`, `refreshMix?kind=` (discover · radar · daily · daylist) | done (per user; same answers as music-lookup) |
| `encore.concerts` | `getConcerts`, `refreshConcerts` | done (per user; same answers as music-lookup) |
| `encore.lyricsTranslation` | `translateLyrics?id=&lang=&mode=translate\|romanize&line=…` | done (needs `ollama_url`; "ai-off" otherwise) |
| `encore.dj` | `getDjLine?next=&prev=&lang=` → `{djLine: {text, id, duration}}`, `djAudio?id=` (WAV) | done (needs `ollama_url` and `dj_voice_url`; "ai-off" otherwise) |
| `encore.songId` | `POST identifySong` (recording as the body) | done (needs `songid_url`) |
| `encore.automix` | `getMixInfo?id=&id=…` | done (needs `mixinfo_dir`) |
