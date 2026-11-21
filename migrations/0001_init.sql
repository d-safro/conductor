-- Encore Server: first schema (library, users, annotations, playlists, queue, radio).
-- Ids are opaque text. Tracks keep their id across rescans (hash of library + path); migrated
-- Navidrome ids are kept through legacy_id.

CREATE TABLE library (
    id            INTEGER PRIMARY KEY,
    name          TEXT NOT NULL,
    root_path     TEXT NOT NULL UNIQUE,
    online        INTEGER NOT NULL DEFAULT 0,
    last_scan_at  INTEGER
);

CREATE TABLE artist (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    sort_name   TEXT NOT NULL,
    mbid        TEXT,
    album_count INTEGER NOT NULL DEFAULT 0,
    song_count  INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX artist_sort ON artist(sort_name);

CREATE TABLE album (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    sort_name       TEXT NOT NULL,
    artist_id       TEXT NOT NULL,          -- album artist
    artist_name     TEXT NOT NULL,
    year            INTEGER,
    genre           TEXT,
    mbid            TEXT,                   -- MusicBrainz release
    compilation     INTEGER NOT NULL DEFAULT 0,
    song_count      INTEGER NOT NULL DEFAULT 0,
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    cover_track_id  TEXT,                   -- a track whose folder / tags hold the cover
    created_at      INTEGER NOT NULL
);
CREATE INDEX album_artist ON album(artist_id);
CREATE INDEX album_created ON album(created_at);

CREATE TABLE track (
    id              TEXT PRIMARY KEY,
    library_id      INTEGER NOT NULL REFERENCES library(id),
    rel_path        TEXT NOT NULL,
    size            INTEGER NOT NULL,
    mtime           INTEGER NOT NULL,
    suffix          TEXT NOT NULL,
    content_type    TEXT NOT NULL,
    bitrate         INTEGER NOT NULL DEFAULT 0,  -- kbit/s
    sample_rate     INTEGER NOT NULL DEFAULT 0,
    bit_depth       INTEGER,
    channels        INTEGER NOT NULL DEFAULT 0,
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    title           TEXT NOT NULL,
    sort_title      TEXT NOT NULL,
    artist_id       TEXT NOT NULL,
    artist_name     TEXT NOT NULL,
    album_id        TEXT NOT NULL,
    album_name      TEXT NOT NULL,
    disc            INTEGER,
    number          INTEGER,
    year            INTEGER,
    genre           TEXT,
    mbid            TEXT,                        -- MusicBrainz recording
    has_picture     INTEGER NOT NULL DEFAULT 0,
    has_lyrics      INTEGER NOT NULL DEFAULT 0,
    created_at      INTEGER NOT NULL,
    missing_since   INTEGER,                     -- set while the file is gone; never while the library is offline
    UNIQUE(library_id, rel_path)
);
CREATE INDEX track_album ON track(album_id);
CREATE INDEX track_artist ON track(artist_id);
CREATE INDEX track_genre ON track(genre);

CREATE TABLE legacy_id (
    old_id  TEXT PRIMARY KEY,
    kind    TEXT NOT NULL,
    new_id  TEXT NOT NULL
);

CREATE TABLE user (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_enc  TEXT NOT NULL,       -- AES-GCM, key file outside the DB (Subsonic token login needs the password)
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE api_key (
    hash          TEXT PRIMARY KEY,    -- sha256 of the key
    user_id       TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    label         TEXT NOT NULL,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER
);

CREATE TABLE annotation (
    user_id      TEXT NOT NULL,
    item_kind    TEXT NOT NULL,        -- track | album | artist
    item_id      TEXT NOT NULL,
    play_count   INTEGER NOT NULL DEFAULT 0,
    last_played  INTEGER,
    rating       INTEGER NOT NULL DEFAULT 0,
    starred_at   INTEGER,
    PRIMARY KEY (user_id, item_kind, item_id)
);

CREATE TABLE play (
    user_id    TEXT NOT NULL,
    track_id   TEXT NOT NULL,
    played_at  INTEGER NOT NULL,
    client     TEXT
);
CREATE INDEX play_user_time ON play(user_id, played_at);

CREATE TABLE playlist (
    id           TEXT PRIMARY KEY,
    owner_id     TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    name         TEXT NOT NULL,
    comment      TEXT NOT NULL DEFAULT '',
    public       INTEGER NOT NULL DEFAULT 0,
    image_path   TEXT,
    rules_json   TEXT,                  -- smart playlists (later)
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL
);

CREATE TABLE playlist_entry (
    playlist_id  TEXT NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
    position     INTEGER NOT NULL,
    track_id     TEXT NOT NULL,
    PRIMARY KEY (playlist_id, position)
);

CREATE TABLE play_queue (
    user_id      TEXT PRIMARY KEY,
    track_ids    TEXT NOT NULL,         -- JSON array
    current      TEXT,
    position_ms  INTEGER NOT NULL DEFAULT 0,
    changed_by   TEXT NOT NULL DEFAULT '',
    changed_at   INTEGER NOT NULL
);

CREATE TABLE radio (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    stream_url    TEXT NOT NULL,
    homepage_url  TEXT
);

CREATE TABLE setting (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
