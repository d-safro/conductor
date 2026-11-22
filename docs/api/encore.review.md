# `encore.review`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Curtain Call: the year or month in review.

## `getListeningReview`

period = year | month, offset back in time: top fives, genres, first song, longest streak, peak hour, discoveries.

Parameters: `period`, `offset`.

| Field | Type |
|---|---|
| `listeningReview` | object |
| `listeningReview.album` | array |
| `listeningReview.albumCount` | number |
| `listeningReview.album[]` | object |
| `listeningReview.album[].artist` | string |
| `listeningReview.album[].artistId` | string |
| `listeningReview.album[].coverArt` | string |
| `listeningReview.album[].id` | string |
| `listeningReview.album[].minutes` | number |
| `listeningReview.album[].name` | string |
| `listeningReview.album[].plays` | number |
| `listeningReview.album[].year` | number |
| `listeningReview.artist` | array |
| `listeningReview.artistCount` | number |
| `listeningReview.artist[]` | object |
| `listeningReview.artist[].coverArt` | string |
| `listeningReview.artist[].id` | string |
| `listeningReview.artist[].minutes` | number |
| `listeningReview.artist[].name` | string |
| `listeningReview.artist[].plays` | number |
| `listeningReview.day` | array |
| `listeningReview.day[]` | object |
| `listeningReview.day[].date` | string |
| `listeningReview.day[].minutes` | number |
| `listeningReview.discoveries` | array |
| `listeningReview.discoveries[]` | object |
| `listeningReview.discoveries[].album` | string |
| `listeningReview.discoveries[].albumId` | string |
| `listeningReview.discoveries[].artist` | string |
| `listeningReview.discoveries[].artistId` | string |
| `listeningReview.discoveries[].bitRate` | number |
| `listeningReview.discoveries[].coverArt` | string |
| `listeningReview.discoveries[].duration` | number |
| `listeningReview.discoveries[].id` | string |
| `listeningReview.discoveries[].isDir` | boolean |
| `listeningReview.discoveries[].plays` | number |
| `listeningReview.discoveries[].suffix` | string |
| `listeningReview.discoveries[].title` | string |
| `listeningReview.discoveries[].track` | number |
| `listeningReview.discoveries[].type` | string |
| `listeningReview.discoveries[].year` | number |
| `listeningReview.discoveryCount` | number |
| `listeningReview.firstSong` | null or object |
| `listeningReview.firstSong.album` | string |
| `listeningReview.firstSong.albumId` | string |
| `listeningReview.firstSong.artist` | string |
| `listeningReview.firstSong.artistId` | string |
| `listeningReview.firstSong.bitRate` | number |
| `listeningReview.firstSong.coverArt` | string |
| `listeningReview.firstSong.duration` | number |
| `listeningReview.firstSong.id` | string |
| `listeningReview.firstSong.isDir` | boolean |
| `listeningReview.firstSong.plays` | number |
| `listeningReview.firstSong.suffix` | string |
| `listeningReview.firstSong.title` | string |
| `listeningReview.firstSong.track` | number |
| `listeningReview.firstSong.type` | string |
| `listeningReview.firstSong.year` | number |
| `listeningReview.from` | number |
| `listeningReview.genre` | array |
| `listeningReview.genre[]` | object |
| `listeningReview.genre[].minutes` | number |
| `listeningReview.genre[].name` | string |
| `listeningReview.hour` | array |
| `listeningReview.hour[]` | number |
| `listeningReview.minutes` | number |
| `listeningReview.newArtists` | number |
| `listeningReview.offset` | number |
| `listeningReview.peakHour` | null or number |
| `listeningReview.period` | string |
| `listeningReview.plays` | number |
| `listeningReview.since` | number |
| `listeningReview.song` | array |
| `listeningReview.song[]` | object |
| `listeningReview.song[].album` | string |
| `listeningReview.song[].albumId` | string |
| `listeningReview.song[].artist` | string |
| `listeningReview.song[].artistId` | string |
| `listeningReview.song[].bitRate` | number |
| `listeningReview.song[].coverArt` | string |
| `listeningReview.song[].duration` | number |
| `listeningReview.song[].id` | string |
| `listeningReview.song[].isDir` | boolean |
| `listeningReview.song[].plays` | number |
| `listeningReview.song[].suffix` | string |
| `listeningReview.song[].title` | string |
| `listeningReview.song[].track` | number |
| `listeningReview.song[].type` | string |
| `listeningReview.song[].year` | number |
| `listeningReview.songs` | number |
| `listeningReview.streak` | object |
| `listeningReview.streak.days` | number |
| `listeningReview.streak.from` | null or string |
| `listeningReview.to` | number |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
