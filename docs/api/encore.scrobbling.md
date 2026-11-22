# `encore.scrobbling`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Each user's own ListenBrainz and Last.fm. Tokens and session keys are stored encrypted; send the ListenBrainz token as a form POST, not in the address.

## `getScrobbling`

What's linked.

| Field | Type |
|---|---|
| `scrobbling` | object |
| `scrobbling.lastfm` | object |
| `scrobbling.lastfm.available` | boolean |
| `scrobbling.lastfm.broken` | boolean |
| `scrobbling.lastfm.linked` | boolean |
| `scrobbling.lastfm.name` | string |
| `scrobbling.listenbrainz` | object |
| `scrobbling.listenbrainz.broken` | boolean |
| `scrobbling.listenbrainz.fromSettings` | boolean |
| `scrobbling.listenbrainz.linked` | boolean |
| `scrobbling.listenbrainz.name` | string |

## `setListenBrainz`

token (empty: unlink); checked with ListenBrainz.

Parameters: `token`.

| Field | Type |
|---|---|
| `scrobbling` | object |
| `scrobbling.lastfm` | object |
| `scrobbling.lastfm.available` | boolean |
| `scrobbling.lastfm.broken` | boolean |
| `scrobbling.lastfm.linked` | boolean |
| `scrobbling.lastfm.name` | string |
| `scrobbling.listenbrainz` | object |
| `scrobbling.listenbrainz.broken` | boolean |
| `scrobbling.listenbrainz.fromSettings` | boolean |
| `scrobbling.listenbrainz.linked` | boolean |
| `scrobbling.listenbrainz.name` | string |

## `lastfmLinkStart`

The page to approve on, and the token to finish with (needs `lastfm_api_key` and `lastfm_secret`).

| Field | Type |
|---|---|
| `lastfmLink` | object |
| `lastfmLink.token` | string |
| `lastfmLink.url` | string |

## `lastfmLinkFinish`

token: waiting | linked | expired. Ask every few seconds while waiting; only the user who started can finish.

Parameters: `token` (required).

| Field | Type |
|---|---|
| `lastfmLink` | object |
| `lastfmLink.name` | string |
| `lastfmLink.status` | string |

## `lastfmUnlink`

Unlink Last.fm.

| Field | Type |
|---|---|
| `scrobbling` | object |
| `scrobbling.lastfm` | object |
| `scrobbling.lastfm.available` | boolean |
| `scrobbling.lastfm.broken` | boolean |
| `scrobbling.lastfm.linked` | boolean |
| `scrobbling.lastfm.name` | string |
| `scrobbling.listenbrainz` | object |
| `scrobbling.listenbrainz.broken` | boolean |
| `scrobbling.listenbrainz.fromSettings` | boolean |
| `scrobbling.listenbrainz.linked` | boolean |
| `scrobbling.listenbrainz.name` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
