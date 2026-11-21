# Encore extensions (planned)

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
| `encore.catalog` | ⬇ items in `search3` / `getAlbum` / `getArtist`, `dz-…` ids everywhere, `lookupSongs`, `getDiscoveries`, `getSongDiscoveries` | done (same answers as music-lookup; trial songs not yet) |
| `encore.sonic` | `getSonicSimilarTracks`, `findSonicPath`, AudioMuse-AI bridge | planned |
