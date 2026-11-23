# `encore.events`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Live updates: one stream of server-sent events (`text/event-stream`) instead of long-polls and timers. Each event is a small hint (what changed, not the thing itself): the app fetches it with the calls it already has. Only what the signed-in account may see: its own devices, commands, sync, playlists, Jam and requests; now playing and the scan, which everyone sees.

- The answer is `text/event-stream`: `retry: 5000`, then `id:`, `event:` and one `data:` line of JSON per event. First always `hello`; then `resync` when a resume can't be honoured (an id older than the 512 events kept, from before the server restarted, or 32+ events at once): fetch everything anew.
- `devices` (`device`: which one said hello, changed or left: deviceList / devicePoll), `command` (`device`, `seq`: a command waits; take it with devicePoll), `jam` (a guest did something or came: jamPoll), `sync` (`key`, `by`, `at`: syncGet), `nowPlaying` (getNowPlaying), `scan` (`state` started / progress / finished, `scanning`, `count`), `playlist` (`id`, `deleted`: one of yours), `requests` (what you asked for changed: a song found, its download's progress by 10%, arrived, kept; an admin hears of any change, Lidarr's albums too: getRequests; looked at every 3 s, so at most that often).
- A keep-alive comment (`: keep-alive`, with the newest `id:`) every 20 s, so proxies keep the stream open. Behind Caddy nothing is needed (it doesn't hold back event streams); nginx is told by `X-Accel-Buffering: no`.
- At most 20 streams per account (another answers HTTP 429); a new stream from the same `id` replaces the older one. A stream that doesn't read what it's sent is closed, not buffered. A changed password or a deleted API key ends it within a keep-alive.
- The event fields are below, under each event's name.

## `eventStream`

GET (or a form POST, to keep the login out of the address). `id`: this device's id (then `command` events are only its own, and the stream keeps it listed as online, as devicePoll does). `lastEventId` or the `Last-Event-ID` header: resume after it.

Parameters: `id`, `lastEventId`.

| Field | Type |
|---|---|
| `command` | object |
| `command.device` | string |
| `command.seq` | number |
| `devices` | object |
| `devices.device` | string |
| `hello` | object |
| `hello.events` | array |
| `hello.events[]` | string |
| `hello.keepalive` | number |
| `hello.resumed` | boolean |
| `jam` | object |
| `nowPlaying` | object |
| `playlist` | object |
| `playlist.deleted` | boolean |
| `playlist.id` | string |
| `requests` | object |
| `resync` | object |
| `scan` | object |
| `scan.count` | number |
| `scan.scanning` | boolean |
| `scan.state` | string |
| `sync` | object |
| `sync.at` | number |
| `sync.by` | string |
| `sync.key` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
