# `encore.import`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Transpose: bring in liked songs, playlists, plays and more from other services' exports or accounts. You see what would change first; each import can be undone.

## `importUpload`

POST, the export as the body, name = its file name (no answer shape recorded: see getImport).

## `importOnline`

source = spotify | lastfm | deezer, account: read from the service.

Parameters: `source` (required), `account`.

| Field | Type |
|---|---|
| `import` | object |
| `import.created` | number |
| `import.done` | null or number |
| `import.error` | null or string |
| `import.from` | string |
| `import.id` | string |
| `import.options` | object |
| `import.progress` | number |
| `import.report` | object |
| `import.source` | string |
| `import.status` | string |
| `import.userId` | string |

## `getImports`

Your imports.

| Field | Type |
|---|---|
| `imports` | object |
| `imports.import` | array |
| `imports.import[]` | object |
| `imports.import[].created` | number |
| `imports.import[].done` | null or number |
| `imports.import[].error` | null or string |
| `imports.import[].from` | string |
| `imports.import[].id` | string |
| `imports.import[].options` | object |
| `imports.import[].options.albums` | boolean |
| `imports.import[].options.artists` | boolean |
| `imports.import[].options.askMissing` | number |
| `imports.import[].options.counts` | boolean |
| `imports.import[].options.liked` | boolean |
| `imports.import[].options.playlists` | boolean |
| `imports.import[].options.plays` | boolean |
| `imports.import[].options.podcasts` | boolean |
| `imports.import[].options.ratings` | boolean |
| `imports.import[].progress` | number |
| `imports.import[].report` | object |
| `imports.import[].report.by` | object |
| `imports.import[].report.by.name` | number |
| `imports.import[].report.changes` | object |
| `imports.import[].report.changes.albums` | number |
| `imports.import[].report.changes.counts` | number |
| `imports.import[].report.changes.follows` | number |
| `imports.import[].report.changes.hearts` | number |
| `imports.import[].report.changes.playlistsNew` | number |
| `imports.import[].report.changes.playlistsUpdated` | number |
| `imports.import[].report.changes.plays` | number |
| `imports.import[].report.changes.playsAlreadyThere` | number |
| `imports.import[].report.changes.podcasts` | number |
| `imports.import[].report.changes.ratings` | number |
| `imports.import[].report.changes.requests` | number |
| `imports.import[].report.changes.skips` | number |
| `imports.import[].report.found` | object |
| `imports.import[].report.found.albums` | number |
| `imports.import[].report.found.artists` | number |
| `imports.import[].report.found.counts` | number |
| `imports.import[].report.found.liked` | number |
| `imports.import[].report.found.playlistSongs` | number |
| `imports.import[].report.found.playlists` | number |
| `imports.import[].report.found.plays` | number |
| `imports.import[].report.found.podcasts` | number |
| `imports.import[].report.found.ratings` | number |
| `imports.import[].report.found.skips` | number |
| `imports.import[].report.matchRate` | number |
| `imports.import[].report.matched` | object |
| `imports.import[].report.matched.albums` | number |
| `imports.import[].report.matched.artists` | number |
| `imports.import[].report.matched.counts` | number |
| `imports.import[].report.matched.liked` | number |
| `imports.import[].report.matched.playlistSongs` | number |
| `imports.import[].report.matched.playlists` | number |
| `imports.import[].report.matched.plays` | number |
| `imports.import[].report.matched.podcasts` | number |
| `imports.import[].report.matched.ratings` | number |
| `imports.import[].report.matched.skips` | number |
| `imports.import[].report.missingTotal` | number |
| `imports.import[].report.notes` | array |
| `imports.import[].report.songs` | number |
| `imports.import[].report.songsMatched` | number |
| `imports.import[].report.sources` | array |
| `imports.import[].report.sources[]` | string |
| `imports.import[].source` | string |
| `imports.import[].status` | string |
| `imports.import[].userId` | string |

## `getImport`

id: one, with its report.

| Field | Type |
|---|---|
| `import` | object |
| `import.created` | number |
| `import.done` | null or number |
| `import.error` | null or string |
| `import.from` | string |
| `import.id` | string |
| `import.options` | object |
| `import.options.albums` | boolean |
| `import.options.artists` | boolean |
| `import.options.askMissing` | number |
| `import.options.counts` | boolean |
| `import.options.liked` | boolean |
| `import.options.playlists` | boolean |
| `import.options.plays` | boolean |
| `import.options.podcasts` | boolean |
| `import.options.ratings` | boolean |
| `import.progress` | number |
| `import.report` | object |
| `import.report.albumsMissing` | array |
| `import.report.artistsMissing` | array |
| `import.report.artistsMissing[]` | string |
| `import.report.by` | object |
| `import.report.by.name` | number |
| `import.report.changes` | object |
| `import.report.changes.albums` | number |
| `import.report.changes.counts` | number |
| `import.report.changes.follows` | number |
| `import.report.changes.hearts` | number |
| `import.report.changes.playlistsNew` | number |
| `import.report.changes.playlistsUpdated` | number |
| `import.report.changes.plays` | number |
| `import.report.changes.playsAlreadyThere` | number |
| `import.report.changes.podcasts` | number |
| `import.report.changes.ratings` | number |
| `import.report.changes.requests` | number |
| `import.report.changes.skips` | number |
| `import.report.found` | object |
| `import.report.found.albums` | number |
| `import.report.found.artists` | number |
| `import.report.found.counts` | number |
| `import.report.found.liked` | number |
| `import.report.found.playlistSongs` | number |
| `import.report.found.playlists` | number |
| `import.report.found.plays` | number |
| `import.report.found.podcasts` | number |
| `import.report.found.ratings` | number |
| `import.report.found.skips` | number |
| `import.report.matchRate` | number |
| `import.report.matched` | object |
| `import.report.matched.albums` | number |
| `import.report.matched.artists` | number |
| `import.report.matched.counts` | number |
| `import.report.matched.liked` | number |
| `import.report.matched.playlistSongs` | number |
| `import.report.matched.playlists` | number |
| `import.report.matched.plays` | number |
| `import.report.matched.podcasts` | number |
| `import.report.matched.ratings` | number |
| `import.report.matched.skips` | number |
| `import.report.missing` | array |
| `import.report.missingTotal` | number |
| `import.report.missing[]` | object |
| `import.report.missing[].album` | string |
| `import.report.missing[].artist` | string |
| `import.report.missing[].deezer` | number |
| `import.report.missing[].duration_ms` | number |
| `import.report.missing[].liked` | boolean |
| `import.report.missing[].playlists` | array |
| `import.report.missing[].playlists[]` | string |
| `import.report.missing[].plays` | number |
| `import.report.missing[].spotify` | string |
| `import.report.missing[].title` | string |
| `import.report.notes` | array |
| `import.report.notes[]` | string |
| `import.report.songs` | number |
| `import.report.songsMatched` | number |
| `import.report.sources` | array |
| `import.report.sources[]` | string |
| `import.source` | string |
| `import.status` | string |
| `import.userId` | string |

## `startImport`

id, kinds, askMissing: bring it in.

Parameters: `kinds`, `askMissing`.

| Field | Type |
|---|---|
| `import` | object |
| `import.created` | number |
| `import.done` | null or number |
| `import.error` | null or string |
| `import.from` | string |
| `import.id` | string |
| `import.options` | object |
| `import.options.albums` | boolean |
| `import.options.artists` | boolean |
| `import.options.askMissing` | number |
| `import.options.counts` | boolean |
| `import.options.liked` | boolean |
| `import.options.playlists` | boolean |
| `import.options.plays` | boolean |
| `import.options.podcasts` | boolean |
| `import.options.ratings` | boolean |
| `import.progress` | number |
| `import.report` | object |
| `import.report.albumsMissing` | array |
| `import.report.artistsMissing` | array |
| `import.report.artistsMissing[]` | string |
| `import.report.by` | object |
| `import.report.by.name` | number |
| `import.report.changes` | object |
| `import.report.changes.albums` | number |
| `import.report.changes.counts` | number |
| `import.report.changes.follows` | number |
| `import.report.changes.hearts` | number |
| `import.report.changes.playlistsNew` | number |
| `import.report.changes.playlistsUpdated` | number |
| `import.report.changes.plays` | number |
| `import.report.changes.playsAlreadyThere` | number |
| `import.report.changes.podcasts` | number |
| `import.report.changes.ratings` | number |
| `import.report.changes.requests` | number |
| `import.report.changes.skips` | number |
| `import.report.found` | object |
| `import.report.found.albums` | number |
| `import.report.found.artists` | number |
| `import.report.found.counts` | number |
| `import.report.found.liked` | number |
| `import.report.found.playlistSongs` | number |
| `import.report.found.playlists` | number |
| `import.report.found.plays` | number |
| `import.report.found.podcasts` | number |
| `import.report.found.ratings` | number |
| `import.report.found.skips` | number |
| `import.report.matchRate` | number |
| `import.report.matched` | object |
| `import.report.matched.albums` | number |
| `import.report.matched.artists` | number |
| `import.report.matched.counts` | number |
| `import.report.matched.liked` | number |
| `import.report.matched.playlistSongs` | number |
| `import.report.matched.playlists` | number |
| `import.report.matched.plays` | number |
| `import.report.matched.podcasts` | number |
| `import.report.matched.ratings` | number |
| `import.report.matched.skips` | number |
| `import.report.missing` | array |
| `import.report.missingTotal` | number |
| `import.report.missing[]` | object |
| `import.report.missing[].album` | string |
| `import.report.missing[].artist` | string |
| `import.report.missing[].liked` | boolean |
| `import.report.missing[].playlists` | array |
| `import.report.missing[].playlists[]` | string |
| `import.report.missing[].plays` | number |
| `import.report.missing[].spotify` | string |
| `import.report.missing[].title` | string |
| `import.report.notes` | array |
| `import.report.songs` | number |
| `import.report.songsMatched` | number |
| `import.report.sources` | array |
| `import.report.sources[]` | string |
| `import.source` | string |
| `import.status` | string |
| `import.userId` | string |

## `undoImport`

id: take it back.

| Field | Type |
|---|---|
| `import` | object |
| `import.created` | number |
| `import.done` | number |
| `import.error` | null or string |
| `import.from` | string |
| `import.id` | string |
| `import.options` | object |
| `import.options.albums` | boolean |
| `import.options.artists` | boolean |
| `import.options.askMissing` | number |
| `import.options.counts` | boolean |
| `import.options.liked` | boolean |
| `import.options.playlists` | boolean |
| `import.options.plays` | boolean |
| `import.options.podcasts` | boolean |
| `import.options.ratings` | boolean |
| `import.progress` | number |
| `import.report` | object |
| `import.report.albumsMissing` | array |
| `import.report.artistsMissing` | array |
| `import.report.artistsMissing[]` | string |
| `import.report.by` | object |
| `import.report.by.name` | number |
| `import.report.changes` | object |
| `import.report.changes.albums` | number |
| `import.report.changes.counts` | number |
| `import.report.changes.follows` | number |
| `import.report.changes.hearts` | number |
| `import.report.changes.playlistsNew` | number |
| `import.report.changes.playlistsUpdated` | number |
| `import.report.changes.plays` | number |
| `import.report.changes.playsAlreadyThere` | number |
| `import.report.changes.podcasts` | number |
| `import.report.changes.ratings` | number |
| `import.report.changes.requests` | number |
| `import.report.changes.skips` | number |
| `import.report.found` | object |
| `import.report.found.albums` | number |
| `import.report.found.artists` | number |
| `import.report.found.counts` | number |
| `import.report.found.liked` | number |
| `import.report.found.playlistSongs` | number |
| `import.report.found.playlists` | number |
| `import.report.found.plays` | number |
| `import.report.found.podcasts` | number |
| `import.report.found.ratings` | number |
| `import.report.found.skips` | number |
| `import.report.matchRate` | number |
| `import.report.matched` | object |
| `import.report.matched.albums` | number |
| `import.report.matched.artists` | number |
| `import.report.matched.counts` | number |
| `import.report.matched.liked` | number |
| `import.report.matched.playlistSongs` | number |
| `import.report.matched.playlists` | number |
| `import.report.matched.plays` | number |
| `import.report.matched.podcasts` | number |
| `import.report.matched.ratings` | number |
| `import.report.matched.skips` | number |
| `import.report.missing` | array |
| `import.report.missingTotal` | number |
| `import.report.missing[]` | object |
| `import.report.missing[].album` | string |
| `import.report.missing[].artist` | string |
| `import.report.missing[].liked` | boolean |
| `import.report.missing[].playlists` | array |
| `import.report.missing[].playlists[]` | string |
| `import.report.missing[].plays` | number |
| `import.report.missing[].spotify` | string |
| `import.report.missing[].title` | string |
| `import.report.notes` | array |
| `import.report.songs` | number |
| `import.report.songsMatched` | number |
| `import.report.sources` | array |
| `import.report.sources[]` | string |
| `import.source` | string |
| `import.status` | string |
| `import.userId` | string |

## `deleteImport`

id: forget it.

Answers "ok" with no fields.

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
