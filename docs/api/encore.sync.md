# `encore.sync`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Settings and small lists kept the same on all your devices: newest change wins, sets merge.

## `syncGet`

What's stored, with when each part changed.

| Field | Type |
|---|---|
| `sync` | object |
| `sync.values` | object |
| `sync.values.pins` | object |
| `sync.values.pins.at` | number |
| `sync.values.pins.by` | string |
| `sync.values.pins.value` | array |
| `sync.values.pins.value[]` | number |

## `syncSet`

Store a part.

Parameters: `key`, `at`, `id`.

| Field | Type |
|---|---|
| `sync` | object |
| `sync.changed` | boolean |
| `sync.value` | object |
| `sync.value.at` | number |
| `sync.value.by` | string |
| `sync.value.value` | array or object |
| `sync.value.value.city` | string |
| `sync.value.value[]` | number or object |
| `sync.value.value[].name` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
