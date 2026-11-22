-- Subsonic shares: a link anyone can open to listen to some songs (on the Jam listener, like Jam and Chorus).
CREATE TABLE share (
    id           TEXT PRIMARY KEY,      -- the token in the link
    user_id      TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    description  TEXT NOT NULL DEFAULT '',
    track_ids    TEXT NOT NULL,         -- JSON array, fixed when the share is made
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER,               -- NULL: until it's deleted
    visited_at   INTEGER,
    visits       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX share_user ON share(user_id);
