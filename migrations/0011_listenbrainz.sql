-- Plays waiting to be sent to ListenBrainz (it was unreachable).
CREATE TABLE lb_queue (
    id         INTEGER PRIMARY KEY,
    payload    TEXT NOT NULL,
    tries      INTEGER NOT NULL DEFAULT 0,
    added_at   INTEGER NOT NULL
);
-- Discovery playlists (ListenBrainz Weekly Exploration / Weekly Jams / Daily Jams): which ListenBrainz playlist
-- each one was last filled from.
CREATE TABLE discovery (
    kind         TEXT PRIMARY KEY,           -- weekly-exploration · weekly-jams · daily-jams
    playlist_id  TEXT NOT NULL,              -- ours
    source       TEXT NOT NULL DEFAULT '',   -- ListenBrainz playlist id it was filled from
    filled_at    INTEGER NOT NULL DEFAULT 0
);
