# `encore.sonic`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

AudioMuse-AI's sound-alikes, and Encore's own AudioMuse calls passed through (`/rest/audiomuse/<endpoint>`, an allow-list, read-only; the login is never passed on).

## `getSonicSimilarTracks`

Songs that sound like this one.

Parameters: `id` (required), `count`.

| Field | Type |
|---|---|
| `sonicMatch` | array |
| `sonicMatch[]` | object |
| `sonicMatch[].entry` | object |
| `sonicMatch[].entry.album` | string |
| `sonicMatch[].entry.albumArtists` | array |
| `sonicMatch[].entry.albumArtists[]` | object |
| `sonicMatch[].entry.albumArtists[].id` | string |
| `sonicMatch[].entry.albumArtists[].name` | string |
| `sonicMatch[].entry.albumId` | string |
| `sonicMatch[].entry.artist` | string |
| `sonicMatch[].entry.artistId` | string |
| `sonicMatch[].entry.artists` | array |
| `sonicMatch[].entry.artists[]` | object |
| `sonicMatch[].entry.artists[].id` | string |
| `sonicMatch[].entry.artists[].name` | string |
| `sonicMatch[].entry.bitDepth` | number |
| `sonicMatch[].entry.bitRate` | number |
| `sonicMatch[].entry.bpm` | number |
| `sonicMatch[].entry.channelCount` | number |
| `sonicMatch[].entry.comment` | string |
| `sonicMatch[].entry.contentType` | string |
| `sonicMatch[].entry.contributors` | array |
| `sonicMatch[].entry.coverArt` | string |
| `sonicMatch[].entry.created` | string |
| `sonicMatch[].entry.displayAlbumArtist` | string |
| `sonicMatch[].entry.displayArtist` | string |
| `sonicMatch[].entry.displayComposer` | string |
| `sonicMatch[].entry.duration` | number |
| `sonicMatch[].entry.explicitStatus` | string |
| `sonicMatch[].entry.genre` | string |
| `sonicMatch[].entry.genres` | array |
| `sonicMatch[].entry.genres[]` | object |
| `sonicMatch[].entry.genres[].name` | string |
| `sonicMatch[].entry.id` | string |
| `sonicMatch[].entry.isDir` | boolean |
| `sonicMatch[].entry.isVideo` | boolean |
| `sonicMatch[].entry.isrc` | array |
| `sonicMatch[].entry.isrc[]` | string |
| `sonicMatch[].entry.mediaType` | string |
| `sonicMatch[].entry.moods` | array |
| `sonicMatch[].entry.musicBrainzId` | string |
| `sonicMatch[].entry.parent` | string |
| `sonicMatch[].entry.path` | string |
| `sonicMatch[].entry.playCount` | number |
| `sonicMatch[].entry.replayGain` | object |
| `sonicMatch[].entry.replayGain.trackGain` | number |
| `sonicMatch[].entry.samplingRate` | number |
| `sonicMatch[].entry.size` | number |
| `sonicMatch[].entry.sortName` | string |
| `sonicMatch[].entry.suffix` | string |
| `sonicMatch[].entry.title` | string |
| `sonicMatch[].entry.track` | number |
| `sonicMatch[].entry.type` | string |
| `sonicMatch[].entry.userRating` | number |
| `sonicMatch[].entry.year` | number |
| `sonicMatch[].similarity` | number |

## `findSonicPath`

A path of songs from one to another.

Parameters: `startSongId` (required), `endSongId` (required), `count`.

| Field | Type |
|---|---|
| `sonicPath` | array |
| `sonicPath[]` | object |
| `sonicPath[].entry` | object |
| `sonicPath[].entry.album` | string |
| `sonicPath[].entry.albumArtists` | array |
| `sonicPath[].entry.albumArtists[]` | object |
| `sonicPath[].entry.albumArtists[].id` | string |
| `sonicPath[].entry.albumArtists[].name` | string |
| `sonicPath[].entry.albumId` | string |
| `sonicPath[].entry.artist` | string |
| `sonicPath[].entry.artistId` | string |
| `sonicPath[].entry.artists` | array |
| `sonicPath[].entry.artists[]` | object |
| `sonicPath[].entry.artists[].id` | string |
| `sonicPath[].entry.artists[].name` | string |
| `sonicPath[].entry.bitDepth` | number |
| `sonicPath[].entry.bitRate` | number |
| `sonicPath[].entry.bpm` | number |
| `sonicPath[].entry.channelCount` | number |
| `sonicPath[].entry.comment` | string |
| `sonicPath[].entry.contentType` | string |
| `sonicPath[].entry.contributors` | array |
| `sonicPath[].entry.coverArt` | string |
| `sonicPath[].entry.created` | string |
| `sonicPath[].entry.displayAlbumArtist` | string |
| `sonicPath[].entry.displayArtist` | string |
| `sonicPath[].entry.displayComposer` | string |
| `sonicPath[].entry.duration` | number |
| `sonicPath[].entry.explicitStatus` | string |
| `sonicPath[].entry.genre` | string |
| `sonicPath[].entry.genres` | array |
| `sonicPath[].entry.genres[]` | object |
| `sonicPath[].entry.genres[].name` | string |
| `sonicPath[].entry.id` | string |
| `sonicPath[].entry.isDir` | boolean |
| `sonicPath[].entry.isVideo` | boolean |
| `sonicPath[].entry.isrc` | array |
| `sonicPath[].entry.isrc[]` | string |
| `sonicPath[].entry.mediaType` | string |
| `sonicPath[].entry.moods` | array |
| `sonicPath[].entry.musicBrainzId` | string |
| `sonicPath[].entry.parent` | string |
| `sonicPath[].entry.path` | string |
| `sonicPath[].entry.playCount` | number |
| `sonicPath[].entry.replayGain` | object |
| `sonicPath[].entry.replayGain.trackGain` | number |
| `sonicPath[].entry.samplingRate` | number |
| `sonicPath[].entry.size` | number |
| `sonicPath[].entry.sortName` | string |
| `sonicPath[].entry.suffix` | string |
| `sonicPath[].entry.title` | string |
| `sonicPath[].entry.track` | number |
| `sonicPath[].entry.type` | string |
| `sonicPath[].entry.userRating` | number |
| `sonicPath[].entry.year` | number |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
