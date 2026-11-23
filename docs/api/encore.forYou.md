# `encore.forYou`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

"Made for you" without AudioMuse-AI: a mix from the artists you play most and the artists that go with them (Conductor's own "sounds like"), about 60% artists new to you and 40% your favourites' songs you haven't played in two weeks; the same artist never twice in a row. The same all day, a new one each day. Empty until something has been played or hearted.

## `getForYou`

count (default 50, at most 200).

Parameters: `count`.

| Field | Type |
|---|---|
| `forYou` | object |
| `forYou.song` | array |
| `forYou.song[]` | object |
| `forYou.song[].album` | string |
| `forYou.song[].albumArtists` | array |
| `forYou.song[].albumArtists[]` | object |
| `forYou.song[].albumArtists[].id` | string |
| `forYou.song[].albumArtists[].name` | string |
| `forYou.song[].albumId` | string |
| `forYou.song[].artist` | string |
| `forYou.song[].artistId` | string |
| `forYou.song[].artists` | array |
| `forYou.song[].artists[]` | object |
| `forYou.song[].artists[].id` | string |
| `forYou.song[].artists[].name` | string |
| `forYou.song[].bitDepth` | number |
| `forYou.song[].bitRate` | number |
| `forYou.song[].bpm` | number |
| `forYou.song[].channelCount` | number |
| `forYou.song[].comment` | string |
| `forYou.song[].contentType` | string |
| `forYou.song[].contributors` | array |
| `forYou.song[].coverArt` | string |
| `forYou.song[].created` | string |
| `forYou.song[].displayAlbumArtist` | string |
| `forYou.song[].displayArtist` | string |
| `forYou.song[].displayComposer` | string |
| `forYou.song[].duration` | number |
| `forYou.song[].explicitStatus` | string |
| `forYou.song[].genre` | string |
| `forYou.song[].genres` | array |
| `forYou.song[].genres[]` | object |
| `forYou.song[].genres[].name` | string |
| `forYou.song[].id` | string |
| `forYou.song[].isDir` | boolean |
| `forYou.song[].isVideo` | boolean |
| `forYou.song[].isrc` | array |
| `forYou.song[].mediaType` | string |
| `forYou.song[].moods` | array |
| `forYou.song[].musicBrainzId` | string |
| `forYou.song[].parent` | string |
| `forYou.song[].path` | string |
| `forYou.song[].playCount` | number |
| `forYou.song[].replayGain` | object |
| `forYou.song[].replayGain.trackGain` | number |
| `forYou.song[].samplingRate` | number |
| `forYou.song[].size` | number |
| `forYou.song[].sortName` | string |
| `forYou.song[].suffix` | string |
| `forYou.song[].title` | string |
| `forYou.song[].track` | number |
| `forYou.song[].type` | string |
| `forYou.song[].userRating` | number |
| `forYou.song[].year` | number |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
