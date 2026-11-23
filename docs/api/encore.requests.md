# `encore.requests`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

The songs asked for (Stagehand: hearted or added ⬇ songs, synced playlists; Audition: songs played on trial; Repertoire's mixes; ListenBrainz discoveries) and the albums Lidarr wants, and how getting each is going, as Podium's admin page shows them. An admin sees all of them and can act on them; anyone else sees only what they asked for (their hearts, playlists, trials and mixes), with only their own name, no Lidarr albums, peers or files, and no actions.

- `id` is a song's `dz-t-…` id (the one `lookupSongs` and the ⬇ songs have) or `lidarr-<n>` for a Lidarr album.
- `state`: `searching` (being looked for), `queued` (a file is chosen; waiting for the peer: `transfer` is slskd's word for it), `downloading` (`progress` 0–100), `done` (filed; `songId` once the scan has it), `notfound` (Soulseek didn't have it), `failed` (it was found, but no one sent it; a Lidarr album that wouldn't import), `setAside` (by an admin). A song that's here and has a better copy on its way (`upgrading`) is `queued` or `downloading` and has its `songId`.
- `reasons`: why it was asked for: `heart`, `playlist`, `trial` (played, queued or picked by autoplay: on trial, deleted unless kept), `mix` (one of Repertoire's mixes: `mix` says which), `discovery` (a ListenBrainz playlist), `lidarr`; `users`: who asked. `trial` / `kept`: on trial now, or for keeps; `folder`: Songs, Trial or Discovery.
- `actions`: what the caller may do with it now (`retry`, `cancel`, `setAside`, `bringBack`, `search`, `pick`); always empty for anyone but an admin. `canPick`: Soulseek can be searched (slskd set up, not read-only). Admins also get `from` (the peer), `file` and `path` (where it was filed).
- Errors: an action by anyone but an admin, or another user's list, is code 50; an unknown `id` or `username` 70; an action that doesn't apply now (it's here, it isn't set aside, it isn't waiting for a file, Soulseek can't be searched) code 0 with why.

## `getRequests`

Newest first (songs by when they were asked for, Lidarr albums by their last search or download). `state`: only these (repeat it, or comma-separate): searching, queued, downloading, done, notfound, failed, setAside. `type`: song or album. `username`: only what this user asked for (an admin; anyone else only their own). `offset`, `count` (default 50, at most 500, 0 for the counts alone). `counts` and `onTrial` are of everything the filters but `state` leave, `total` of what `state` leaves too.

Parameters: `state`, `type`, `username`, `offset`, `count`.

| Field | Type |
|---|---|
| `requests` | object |
| `requests.canPick` | boolean |
| `requests.counts` | object |
| `requests.counts.done` | number |
| `requests.counts.downloading` | number |
| `requests.counts.failed` | number |
| `requests.counts.notfound` | number |
| `requests.counts.queued` | number |
| `requests.counts.searching` | number |
| `requests.counts.setAside` | number |
| `requests.downloadsPaused` | boolean |
| `requests.offset` | number |
| `requests.onTrial` | number |
| `requests.request` | array |
| `requests.request[]` | object |
| `requests.request[].actions` | array |
| `requests.request[].actions[]` | string |
| `requests.request[].album` | string |
| `requests.request[].arrived` | string |
| `requests.request[].artist` | string |
| `requests.request[].artists` | array |
| `requests.request[].artists[]` | string |
| `requests.request[].asked` | string |
| `requests.request[].duration` | number |
| `requests.request[].file` | string |
| `requests.request[].folder` | string |
| `requests.request[].found` | boolean |
| `requests.request[].from` | string |
| `requests.request[].id` | string |
| `requests.request[].kept` | boolean |
| `requests.request[].manual` | boolean |
| `requests.request[].mix` | string |
| `requests.request[].note` | string |
| `requests.request[].path` | string |
| `requests.request[].progress` | number |
| `requests.request[].query` | string |
| `requests.request[].reasons` | array |
| `requests.request[].reasons[]` | string |
| `requests.request[].searches` | number |
| `requests.request[].songId` | string |
| `requests.request[].state` | string |
| `requests.request[].title` | string |
| `requests.request[].transfer` | string |
| `requests.request[].trial` | boolean |
| `requests.request[].type` | string |
| `requests.request[].upgrading` | boolean |
| `requests.request[].users` | array |
| `requests.request[].users[]` | string |
| `requests.total` | number |

## `retryRequest`

Admins. `id`: look again from the start (any file, any peer; a Lidarr album: any folder). The request as listed.

Parameters: `id` (required).

| Field | Type |
|---|---|
| `request` | object |
| `request.actions` | array |
| `request.actions[]` | string |
| `request.album` | string |
| `request.artist` | string |
| `request.artists` | array |
| `request.artists[]` | string |
| `request.asked` | string |
| `request.duration` | number |
| `request.file` | string |
| `request.folder` | string |
| `request.found` | boolean |
| `request.from` | string |
| `request.id` | string |
| `request.kept` | boolean |
| `request.manual` | boolean |
| `request.note` | string |
| `request.path` | string |
| `request.progress` | number |
| `request.query` | string |
| `request.reasons` | array |
| `request.reasons[]` | string |
| `request.searches` | number |
| `request.state` | string |
| `request.title` | string |
| `request.transfer` | string |
| `request.trial` | boolean |
| `request.type` | string |
| `request.upgrading` | boolean |
| `request.users` | array |
| `request.users[]` | string |

## `cancelRequest`

Admins. `id`: stop asking for a song that isn't here yet.

Parameters: `id` (required).

Answers "ok" with no fields.

## `setRequestAside`

Admins. `id`, `aside` (default true): set a song that's being looked for or wasn't found aside (no more automatic searches), or bring it back (false). The request as listed.

Parameters: `id` (required), `aside`.

| Field | Type |
|---|---|
| `request` | object |
| `request.actions` | array |
| `request.actions[]` | string |
| `request.album` | string |
| `request.artist` | string |
| `request.artists` | array |
| `request.artists[]` | string |
| `request.asked` | string |
| `request.duration` | number |
| `request.file` | string |
| `request.folder` | string |
| `request.found` | boolean |
| `request.from` | string |
| `request.id` | string |
| `request.kept` | boolean |
| `request.manual` | boolean |
| `request.note` | string |
| `request.path` | string |
| `request.progress` | number |
| `request.query` | string |
| `request.reasons` | array |
| `request.reasons[]` | string |
| `request.searches` | number |
| `request.state` | string |
| `request.title` | string |
| `request.transfer` | string |
| `request.trial` | boolean |
| `request.type` | string |
| `request.upgrading` | boolean |
| `request.users` | array |
| `request.users[]` | string |

## `searchRequestFiles`

Admins. `id`, `query` (at least 3 letters; by default the request's own `query`): search Soulseek your own way. Every audio file found, the likeliest first (at most 120 of `total`), with what fits: `match` (what the automatic search would take), `titleOk`, `artistOk`, `sameAlbum`. Waits for the search (10–40 s; one at a time). Refused on a read-only server (nothing found could be picked).

Parameters: `id` (required), `query`.

| Field | Type |
|---|---|
| `requestFiles` | object |
| `requestFiles.file` | array |
| `requestFiles.file[]` | object |
| `requestFiles.file[].artistOk` | boolean |
| `requestFiles.file[].bitrate` | number |
| `requestFiles.file[].ext` | string |
| `requestFiles.file[].filename` | string |
| `requestFiles.file[].folder` | string |
| `requestFiles.file[].free` | boolean |
| `requestFiles.file[].length` | number |
| `requestFiles.file[].match` | boolean |
| `requestFiles.file[].name` | string |
| `requestFiles.file[].queue` | number |
| `requestFiles.file[].sameAlbum` | boolean |
| `requestFiles.file[].score` | number |
| `requestFiles.file[].size` | number |
| `requestFiles.file[].speed` | number |
| `requestFiles.file[].titleOk` | boolean |
| `requestFiles.file[].user` | string |
| `requestFiles.query` | string |
| `requestFiles.total` | number |

## `pickRequestFile`

Admins. `id`, `user`, `file`, `size` (a search result's `user`, `filename` and `size`): download that file for a song waiting for one (searching, not found, failed or set aside); it's filed, scanned and hearted / added like any download. The request as listed.

Parameters: `id` (required), `user` (required), `file` (required), `size`.

| Field | Type |
|---|---|
| `request` | object |
| `request.actions` | array |
| `request.actions[]` | string |
| `request.album` | string |
| `request.artist` | string |
| `request.artists` | array |
| `request.artists[]` | string |
| `request.asked` | string |
| `request.duration` | number |
| `request.file` | string |
| `request.folder` | string |
| `request.found` | boolean |
| `request.from` | string |
| `request.id` | string |
| `request.kept` | boolean |
| `request.manual` | boolean |
| `request.note` | string |
| `request.path` | string |
| `request.progress` | number |
| `request.query` | string |
| `request.reasons` | array |
| `request.reasons[]` | string |
| `request.searches` | number |
| `request.state` | string |
| `request.title` | string |
| `request.transfer` | string |
| `request.trial` | boolean |
| `request.type` | string |
| `request.upgrading` | boolean |
| `request.users` | array |
| `request.users[]` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
