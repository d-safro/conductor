# `encore.jam`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Ensemble: listen together. The host starts a Jam; guests open a link on the separate Jam listener (`jam_listen`, `jam_public_url`), search, add and reorder songs.

## `jamStart`

Start one; its link and token.

Parameters: `name`.

| Field | Type |
|---|---|
| `jam` | object |
| `jam.token` | string |
| `jam.url` | string |

## `jamState`

The host's queue and position, sent for the guests.

Parameters: `token`.

| Field | Type |
|---|---|
| `jam` | object |

## `jamPoll`

Wait for what guests did.

Parameters: `token`, `ack`, `wait`.

| Field | Type |
|---|---|
| `jam` | object |
| `jam.commands` | array |
| `jam.commands[]` | object |
| `jam.commands[].by` | string |
| `jam.commands[].id` | string |
| `jam.commands[].seq` | number |
| `jam.commands[].type` | string |
| `jam.guests` | number |

## `jamEnd`

End it: the link stops working.

Parameters: `token`.

| Field | Type |
|---|---|
| `jam` | object |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
