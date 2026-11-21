# Encore extensions (planned)

Encore Server speaks standard Subsonic 1.16.1 and OpenSubsonic. Extra features for Encore are advertised in
`getOpenSubsonicExtensions`, so any app can find and use them, and Encore can tell which servers have them.
Each one gets its own spec file here before it's built.

| Extension | Calls | Status |
|---|---|---|
| `encore.devices` | `deviceHello`, `deviceState`, `deviceList`, `deviceCommand`, `devicePoll`, `deviceBye` | planned |
| `encore.sync` | `syncGet`, `syncSet` | planned |
| `encore.pairing` | `pairStart`, `pairPoll`, `pairApprove` | planned |
| `encore.jam` | `jamStart`, `jamState`, `jamPoll`, `jamEnd` (guests on a separate listener) | planned |
| `encore.stats` | `getListeningStats`, `getFolder` | planned |
| `encore.smartPlaylists` | `createSmartPlaylist`, `updateSmartPlaylist`, `getSmartPlaylistRules` | planned |
| `encore.playlistImages` | `setPlaylistImage`, `deletePlaylistImage` | planned |
| `encore.catalog` | songs you don't have in `search3` / `getAlbum` / `getArtist`, `getDiscoveries`, `getSongDiscoveries`, `requestItem`, `getRequests` | planned |
| `encore.sonic` | `getSonicSimilarTracks`, `findSonicPath`, AudioMuse-AI bridge | planned |
