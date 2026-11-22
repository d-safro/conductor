# `encore.smartPlaylists`

<!-- Written by tools/api_docs.py from its EXTENSIONS, the handlers in src/ and docs/api/shapes/: edit those, then run it. -->

Smart playlists in Navidrome's rule format (all / any, is, contains, gt, lt, inTheRange, inTheLast, before / after, inPlaylist…; sort, order, limit). Not Subsonic calls: through the Navidrome stand-in Encore uses.

- `POST /auth/login` (JSON: username, password) → a token for the calls below (`X-ND-Authorization: Bearer <token>`).
- `POST /api/playlist` (JSON: name, rules, public) makes one; `PUT /api/playlist/{id}` changes it; `GET /api/playlist/{id}` gives its rules.
- They refill when read (at most once a minute) and can't be edited by hand (`readonly` in getPlaylist). `inPlaylist` only sees your own playlists and public ones.

From 1.0, a field here may gain company but never goes away or changes type (see [STABILITY.md](../STABILITY.md)).
