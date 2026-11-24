# `encore.catalog`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Stagehand: songs you don't have, from Deezer, marked ⬇ / ⏳ / ✕. They appear in search3, getAlbum and getArtist answers with `dz-…` ids (accepted everywhere an id is); hearting or adding one asks for it. Only with a downloader (or `catalog = true`). A library album's list from Deezer is looked up once and kept; a ⬇ song from it is filed into that album when it's downloaded (tagged with the album's name, artist, year, genre, disc and track).

## `lookupSongs`

Where songs asked for are (status, and the song id once here); fetch=true asks for them.

Parameters: `id`, `fetch`.

| Field | Type |
|---|---|
| `lookupSongs` | object |
| `lookupSongs.perDay` | number |
| `lookupSongs.playlist` | string |
| `lookupSongs.playlistId` | string |
| `lookupSongs.song` | array |
| `lookupSongs.song[]` | object |
| `lookupSongs.song[].id` | string |
| `lookupSongs.song[].songId` | string |
| `lookupSongs.song[].status` | string |
| `lookupSongs.song[].temporary` | boolean |
| `lookupSongs.trialHours` | number |
| `lookupSongs.usedToday` | number |

## `getDiscoveries`

Songs you might like that you don't have.

| Field | Type |
|---|---|
| `discoveries` | object |
| `discoveries.section` | array |

## `getSongDiscoveries`

The same, around one song.

Parameters: `count`, `id` (required).

| Field | Type |
|---|---|
| `songDiscoveries` | object |
| `songDiscoveries.song` | array |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
