# `encore.dj`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Interlude: a line or two about the next song, written by Ollama and read by Piper. Answers "ai-off" without them.

## `getDjLine`

next, prev, lang: the line, its id and length.

Parameters: `next` (required), `lang`, `prev`.

| Field | Type |
|---|---|
| `djLine` | object |
| `djLine.duration` | number |
| `djLine.error` | string |
| `djLine.id` | string |
| `djLine.text` | string |

## `djAudio`

id: the line read aloud (WAV; a file, so no answer shape).

Parameters: `id` (required).

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
