# `encore.pairing`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Sign in on a new device with a code shown on it, approved on a device that's signed in. pairStart and pairPoll need no login (the new device has none yet); at most 5 codes wait from one address.

## `pairStart`

The new device gets a code to show, and a token to poll with.

| Field | Type |
|---|---|
| `pair` | object |
| `pair.code` | string |
| `pair.expires` | number |
| `pair.token` | string |

## `pairPoll`

Wait for the approval: waiting, then done with the login.

| Field | Type |
|---|---|
| `pair` | object |
| `pair.login` | object |
| `pair.login.localAddress` | string |
| `pair.login.pass` | string |
| `pair.login.server` | string |
| `pair.login.user` | string |
| `pair.status` | string |

## `pairApprove`

A signed-in device approves the code, handing over a login.

Parameters: `code`.

| Field | Type |
|---|---|
| `pair` | object |
| `pair.name` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
