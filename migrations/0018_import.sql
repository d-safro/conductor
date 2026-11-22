-- Imports from other services (Spotify, Apple Music, YouTube Music, …): each is a batch that can be undone.
CREATE TABLE import_batch (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    source       TEXT NOT NULL DEFAULT '',          -- what was found, e.g. "Spotify extended streaming history"
    input_json   TEXT NOT NULL DEFAULT '{}',        -- {"file": name in data/imports} or {"online": "spotify", "user": …}
    options_json TEXT NOT NULL DEFAULT '{}',
    status       TEXT NOT NULL DEFAULT 'preview',   -- reading · preview · running · done · failed · undone
    progress     REAL NOT NULL DEFAULT 0,
    report_json  TEXT NOT NULL DEFAULT '{}',
    error        TEXT,
    created_at   INTEGER NOT NULL,
    done_at      INTEGER
);
-- What an import changed, to undo it: star / album_star / follow / playlist / playlist_songs / rating / count /
-- request / podcast (plays and skips are marked by their client "import:<batch>").
CREATE TABLE import_change (
    batch_id  TEXT NOT NULL REFERENCES import_batch(id) ON DELETE CASCADE,
    kind      TEXT NOT NULL,
    item      TEXT NOT NULL,
    before    TEXT
);
CREATE INDEX import_change_batch ON import_change(batch_id);
-- Songs started but not listened to (under 30 s), e.g. from an imported history.
CREATE TABLE skip (
    user_id     TEXT NOT NULL,
    track_id    TEXT NOT NULL,
    skipped_at  INTEGER NOT NULL,
    client      TEXT
);
CREATE INDEX skip_user_time ON skip(user_id, skipped_at);
