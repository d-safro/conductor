-- Your mixes (Discover Weekly, Release Radar, Daily Mix 1–6, Daylist), per user, as real playlists.
-- Songs a mix fetched are on trial for `trial_hours` (a week), not listed in "On trial".
ALTER TABLE request ADD COLUMN mix TEXT;
ALTER TABLE request ADD COLUMN trial_hours REAL;

CREATE TABLE mix_state (
    user_id       TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    kind          TEXT NOT NULL,                -- discover · radar · daily · daylist
    enabled       INTEGER NOT NULL DEFAULT 1,
    last_at       INTEGER,                      -- when it was last made (seconds)
    current_json  TEXT NOT NULL DEFAULT '[]',   -- the songs of this one: "dz-t-…" or "lib:<id>"
    history_json  TEXT NOT NULL DEFAULT '[]',   -- songs it has had (not picked again)
    PRIMARY KEY (user_id, kind)
);

CREATE TABLE mix_playlist (
    user_id      TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    slot         TEXT NOT NULL,                 -- discover, radar, daily-1 … daily-6, daylist
    playlist_id  TEXT NOT NULL,
    song_count   INTEGER NOT NULL DEFAULT 0,
    made_at      INTEGER NOT NULL,
    PRIMARY KEY (user_id, slot)
);
