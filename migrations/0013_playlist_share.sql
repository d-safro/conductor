-- Collaborative playlists: a link that lets friends add songs to one of your playlists.
CREATE TABLE playlist_share (
    token        TEXT PRIMARY KEY,
    playlist_id  TEXT NOT NULL REFERENCES playlist(id) ON DELETE CASCADE,
    owner_id     TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL
);
CREATE UNIQUE INDEX playlist_share_playlist ON playlist_share(playlist_id);

-- Who added what through the link (so a friend can take out only their own songs).
CREATE TABLE playlist_share_add (
    token        TEXT NOT NULL REFERENCES playlist_share(token) ON DELETE CASCADE,
    track_id     TEXT NOT NULL,
    by_name      TEXT NOT NULL,
    guest        TEXT NOT NULL,
    added_at     INTEGER NOT NULL
);
CREATE INDEX playlist_share_add_token ON playlist_share_add(token);
