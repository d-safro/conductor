# `encore.playlistImages`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

A playlist's own picture. Through the Navidrome stand-in, like smart playlists.

- `POST /api/playlist/{id}/image` (the picture as the body) and `DELETE` the same.
- The picture is served by getCoverArt as `pl-<id>_…`.

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
