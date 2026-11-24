-- A library album's whole song list as Deezer has it, kept for good once it's looked up (getAlbum), so the album
-- isn't matched and fetched again every time it's opened. Each song points to your copy: one of yours, or the
-- download once it arrives (`local_id`, kept up to date as the album is opened). A missing song asked for from here
-- (or anywhere) is filed into this album: its download is tagged with the album's name, artist, year and genre and
-- its own disc and track number. The album that was opened last wins when a song is on more than one.
-- A miss (no Deezer album) isn't kept here: it's in catalog_cache for a week, then looked for again.
CREATE TABLE album_songs (
    album_id    TEXT PRIMARY KEY,   -- the library album
    dz_album    TEXT NOT NULL,      -- the Deezer album it is
    album_json  TEXT NOT NULL,      -- Deezer's album (title, cover, date), to list the songs you don't have
    at          INTEGER NOT NULL,   -- looked up
    seen_at     INTEGER NOT NULL    -- last opened
);
CREATE TABLE album_song (
    album_id    TEXT NOT NULL,
    pos         INTEGER NOT NULL,   -- order on the album
    key         TEXT NOT NULL,      -- dz-t-<id>
    disc        INTEGER NOT NULL,
    track       INTEGER NOT NULL,
    track_json  TEXT NOT NULL,      -- Deezer's track
    local_id    TEXT,               -- your copy
    PRIMARY KEY (album_id, pos)
);
CREATE INDEX album_song_key ON album_song(key);
