# `encore.concerts`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Marquee: upcoming shows of the artists you play and follow, near the town set in Encore.

## `getConcerts`

The shows found.

| Field | Type |
|---|---|
| `concerts` | object |
| `concerts.city` | string |
| `concerts.show` | array |
| `concerts.show[]` | object |
| `concerts.show[].artist` | string |
| `concerts.show[].city` | string |
| `concerts.show[].country` | string |
| `concerts.show[].date` | string |
| `concerts.show[].first` | number |
| `concerts.show[].name` | string |
| `concerts.show[].region` | string |
| `concerts.show[].source` | string |
| `concerts.show[].time` | string |
| `concerts.show[].url` | string |
| `concerts.show[].venue` | string |
| `concerts.source` | string |
| `concerts.updated` | number |

## `refreshConcerts`

Look again now.

| Field | Type |
|---|---|
| `concerts` | object |
| `concerts.city` | string |
| `concerts.show` | array |
| `concerts.source` | string |
| `concerts.updated` | number |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
