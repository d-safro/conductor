# `encore.collabPlaylists`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Chorus: friends add songs to your playlist through a link on the Jam listener (`/jam/pl/<token>`); they can take out only their own. Links last 30 days.

## `playlistShareStart`

A link for a playlist.

Parameters: `id` (required).

| Field | Type |
|---|---|
| `playlistShare` | object |
| `playlistShare.added` | array |
| `playlistShare.error` | string |
| `playlistShare.playlist` | string |
| `playlistShare.token` | string |
| `playlistShare.url` | string |

## `playlistShareEnd`

Stop it.

Parameters: `id`.

| Field | Type |
|---|---|
| `playlistShare` | object |
| `playlistShare.ended` | boolean |

## `playlistShares`

Your playlists' links.

| Field | Type |
|---|---|
| `playlistShares` | object |
| `playlistShares.share` | array |
| `playlistShares.share[]` | object |
| `playlistShares.share[].added` | array |
| `playlistShares.share[].added[]` | object |
| `playlistShares.share[].added[].at` | number |
| `playlistShares.share[].added[].by` | string |
| `playlistShares.share[].added[].guest` | string |
| `playlistShares.share[].added[].id` | string |
| `playlistShares.share[].playlist` | string |
| `playlistShares.share[].url` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
