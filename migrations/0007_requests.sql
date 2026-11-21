-- Songs asked for that aren't in the library yet (Deezer ids "dz-t-…"), and how getting each is going.
CREATE TABLE request (
    key           TEXT PRIMARY KEY,             -- dz-t-<deezer track id>
    title         TEXT NOT NULL,
    artists_json  TEXT NOT NULL,                -- ["Main artist", "featured", …]
    album         TEXT NOT NULL DEFAULT '',
    duration_ms   INTEGER NOT NULL DEFAULT 0,
    isrc          TEXT,
    folder        TEXT NOT NULL DEFAULT 'Songs', -- where it's filed in the music folder: Songs, or Trial
    status        TEXT NOT NULL DEFAULT 'retry', -- retry · queued · done (filed, waiting for the scan) · notfound
    slsk_user     TEXT,
    slsk_file     TEXT,
    searches      INTEGER NOT NULL DEFAULT 0,
    last_search   INTEGER NOT NULL DEFAULT 0,
    tried_json    TEXT NOT NULL DEFAULT '[]',   -- Soulseek users that didn't send it
    found         INTEGER NOT NULL DEFAULT 0,   -- someone has it (keep trying longer)
    since         INTEGER NOT NULL DEFAULT 0,   -- when the download was queued
    rel           TEXT,                         -- where it was filed, relative to the music folder
    local_id      TEXT,                         -- the library song, once it's there
    trial         INTEGER NOT NULL DEFAULT 0,
    trial_at      INTEGER,
    arrived_at    INTEGER,
    asked_at      INTEGER NOT NULL
);
CREATE INDEX request_status ON request(status);

-- What to do once a requested song is in the library: heart it for a user, add it to a playlist.
CREATE TABLE request_want (
    key          TEXT NOT NULL REFERENCES request(key) ON DELETE CASCADE,
    user_id      TEXT NOT NULL,
    star         INTEGER NOT NULL DEFAULT 0,
    playlist_id  TEXT NOT NULL DEFAULT '',
    done         INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (key, user_id, playlist_id)
);

-- Deezer matches for library albums and artists (slow to work out; kept for a week / a month).
CREATE TABLE catalog_cache (
    kind   TEXT NOT NULL,
    key    TEXT NOT NULL,
    value  TEXT,
    at     INTEGER NOT NULL,
    PRIMARY KEY (kind, key)
);
