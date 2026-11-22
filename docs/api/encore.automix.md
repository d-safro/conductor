# `encore.automix`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Automix: each song's intro, outro and tempo, from the mix analyzer (`mixinfo_dir`).

## `getMixInfo`

id (again for more).

Parameters: `id`.

| Field | Type |
|---|---|
| `mixInfo` | object |
| `mixInfo.song` | array |
| `mixInfo.song[]` | object |
| `mixInfo.song[].beat` | number |
| `mixInfo.song[].bpm` | number |
| `mixInfo.song[].dur` | number |
| `mixInfo.song[].end` | number |
| `mixInfo.song[].id` | string |
| `mixInfo.song[].introEnd` | number |
| `mixInfo.song[].outroStart` | number |
| `mixInfo.song[].start` | number |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
