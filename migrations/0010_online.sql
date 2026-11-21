-- Lyrics found online (LRCLIB) for songs that have none of their own; body NULL = looked, nothing there.
CREATE TABLE lyrics_cache (
    track_id    TEXT PRIMARY KEY,
    body        TEXT,
    synced      INTEGER NOT NULL DEFAULT 0,
    source      TEXT NOT NULL DEFAULT 'lrclib',
    fetched_at  INTEGER NOT NULL
);
