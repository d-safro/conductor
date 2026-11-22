# `encore.stats`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Listening stats from your plays, and the real folders.

## `getListeningStats`

period = week | month | year | all, offset back in time; minutes, plays, top songs, artists, albums, genres, by day and hour (local time).

Parameters: `period`, `offset`.

| Field | Type |
|---|---|
| `listeningStats` | object |
| `listeningStats.album` | array |
| `listeningStats.albumCount` | number |
| `listeningStats.album[]` | object |
| `listeningStats.album[].artist` | string |
| `listeningStats.album[].artistId` | string |
| `listeningStats.album[].coverArt` | string |
| `listeningStats.album[].id` | string |
| `listeningStats.album[].minutes` | number |
| `listeningStats.album[].name` | string |
| `listeningStats.album[].plays` | number |
| `listeningStats.album[].year` | number |
| `listeningStats.artist` | array |
| `listeningStats.artistCount` | number |
| `listeningStats.artist[]` | object |
| `listeningStats.artist[].coverArt` | string |
| `listeningStats.artist[].id` | string |
| `listeningStats.artist[].minutes` | number |
| `listeningStats.artist[].name` | string |
| `listeningStats.artist[].plays` | number |
| `listeningStats.day` | array |
| `listeningStats.day[]` | object |
| `listeningStats.day[].date` | string |
| `listeningStats.day[].minutes` | number |
| `listeningStats.from` | number |
| `listeningStats.hour` | array |
| `listeningStats.hour[]` | number |
| `listeningStats.minutes` | number |
| `listeningStats.newArtists` | number |
| `listeningStats.offset` | number |
| `listeningStats.period` | string |
| `listeningStats.plays` | number |
| `listeningStats.since` | number |
| `listeningStats.song` | array |
| `listeningStats.song[]` | object |
| `listeningStats.song[].album` | string |
| `listeningStats.song[].albumId` | string |
| `listeningStats.song[].artist` | string |
| `listeningStats.song[].artistId` | string |
| `listeningStats.song[].bitRate` | number |
| `listeningStats.song[].coverArt` | string |
| `listeningStats.song[].duration` | number |
| `listeningStats.song[].id` | string |
| `listeningStats.song[].isDir` | boolean |
| `listeningStats.song[].plays` | number |
| `listeningStats.song[].suffix` | string |
| `listeningStats.song[].title` | string |
| `listeningStats.song[].track` | number |
| `listeningStats.song[].type` | string |
| `listeningStats.song[].year` | number |
| `listeningStats.songs` | number |
| `listeningStats.to` | number |

## `getFolder`

A folder of the music as it is on disk.

Parameters: `path`.

| Field | Type |
|---|---|
| `folder` | object |
| `folder.dir` | array |
| `folder.dir[]` | object |
| `folder.dir[].count` | number |
| `folder.dir[].coverArt` | string |
| `folder.dir[].name` | string |
| `folder.dir[].path` | string |
| `folder.path` | string |
| `folder.song` | array |
| `folder.song[]` | object |
| `folder.song[].album` | string |
| `folder.song[].albumId` | string |
| `folder.song[].artist` | string |
| `folder.song[].artistId` | string |
| `folder.song[].bitRate` | number |
| `folder.song[].coverArt` | string |
| `folder.song[].duration` | number |
| `folder.song[].id` | string |
| `folder.song[].isDir` | boolean |
| `folder.song[].plays` | number |
| `folder.song[].suffix` | string |
| `folder.song[].title` | string |
| `folder.song[].track` | number |
| `folder.song[].type` | string |
| `folder.song[].year` | number |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
