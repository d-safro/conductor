# `encore.removeSongs`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Take songs out of every library for good (admins only). Ask what would go first, show it and ask the user; then remove exactly those songs.

- Nothing is deleted unless every song given can be (its drive connected; not when the server is read-only).
- A song kept in a Lidarr album folder may be fetched again by Lidarr if it then wants the album.

## `getSongRemoval`

Admins. `id` (repeat it): each song and its other copies (the same title by the same artist, as long within 3 s, in any library), with each one's library, file and size.

Parameters: `id`.

| Field | Type |
|---|---|
| `songRemoval` | object |
| `songRemoval.count` | number |
| `songRemoval.size` | number |
| `songRemoval.song` | array |
| `songRemoval.song[]` | object |
| `songRemoval.song[].album` | string |
| `songRemoval.song[].artist` | string |
| `songRemoval.song[].duration` | number |
| `songRemoval.song[].id` | string |
| `songRemoval.song[].library` | string |
| `songRemoval.song[].path` | string |
| `songRemoval.song[].size` | number |
| `songRemoval.song[].title` | string |

## `removeSongs`

Admins. `id` (repeat it), `confirm=permanently` (required): delete these songs' files (and a `.lrc` beside one), take them out of every playlist and play queue, and set a song Conductor downloaded aside so it isn't fetched again. `removed` and `failed` (ids). Every app hears `songsRemoved`.

Parameters: `confirm`, `id`.

| Field | Type |
|---|---|
| `songsRemoved` | object |
| `songsRemoved.failed` | array |
| `songsRemoved.removed` | array |
| `songsRemoved.removed[]` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
