# `encore.lyricsTranslation`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Libretto: lyrics translated or romanized line by line by a local Ollama (`ollama_url`). Answers "ai-off" without one.

## `translateLyrics`

id, lang, mode = translate | romanize, line (each line).

Parameters: `lang`, `mode`, `line`, `id`.

| Field | Type |
|---|---|
| `lyricsTranslation` | object |
| `lyricsTranslation.error` | string |
| `lyricsTranslation.lang` | string |
| `lyricsTranslation.lines` | array |
| `lyricsTranslation.lines[]` | string |
| `lyricsTranslation.same` | boolean |
| `lyricsTranslation.source` | string |

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
