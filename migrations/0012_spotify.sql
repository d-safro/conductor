-- Spotify sync: which library song each Spotify song is (learned from music-download's files and from downloads).
CREATE TABLE spotify_map (
    spotify_id  TEXT PRIMARY KEY,
    track_id    TEXT NOT NULL
);
