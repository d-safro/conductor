# `encore.mixes`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Repertoire: your mixes, as real playlists, per user: Discover Weekly, Release Radar, Daily Mix 1–6, Daylist.

## `getMixes`

Which kinds are on, and their playlists.

| Field | Type |
|---|---|
| `mixes` | object |
| `mixes.kind` | array |
| `mixes.kind[]` | object |
| `mixes.kind[].enabled` | boolean |
| `mixes.kind[].kind` | string |
| `mixes.kind[].name` | string |
| `mixes.kind[].updated` | number |
| `mixes.mix` | array |
| `mixes.mix[]` | object |
| `mixes.mix[].comment` | string |
| `mixes.mix[].kind` | string |
| `mixes.mix[].name` | string |
| `mixes.mix[].playlistId` | string |
| `mixes.mix[].slot` | string |
| `mixes.mix[].songCount` | number |
| `mixes.mix[].updated` | number |

## `setMix`

kind, enabled: turn one on (made at once) or off (its playlists go).

Parameters: `kind`, `enabled`.

| Field | Type |
|---|---|
| `mixes` | object |
| `mixes.kind` | array |
| `mixes.kind[]` | object |
| `mixes.kind[].enabled` | boolean |
| `mixes.kind[].kind` | string |
| `mixes.kind[].name` | string |
| `mixes.kind[].updated` | number |
| `mixes.mix` | array |

## `refreshMix`

kind: make it again now.

Parameters: `kind`, `enabled`.

| Field | Type |
|---|---|
| `mixes` | object |
| `mixes.kind` | array |
| `mixes.kind[]` | object |
| `mixes.kind[].enabled` | boolean |
| `mixes.kind[].kind` | string |
| `mixes.kind[].name` | string |
| `mixes.kind[].updated` | number |
| `mixes.mix` | array |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
