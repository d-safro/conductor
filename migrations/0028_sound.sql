-- "Sounds like" from the audio itself (src/audio/): what each song sounds like, as numbers worked out from a minute
-- of it (tempo, the spectrum's shape, chroma, MFCCs…). A table of its own, which a scan never touches: a rescan keeps
-- every analysis. The file's size and time when it was analysed: a changed file is analysed again, as is every song
-- when the numbers change (`version`). No features: the file couldn't be read, or it's silent or too short (tried
-- again when it changes).
CREATE TABLE sound (
    track_id    TEXT PRIMARY KEY,
    size        INTEGER NOT NULL,
    mtime       INTEGER NOT NULL,
    version     INTEGER NOT NULL,
    features    BLOB,              -- little-endian f32s, src/audio/dsp.rs's Features::to_vec
    analysed_at INTEGER NOT NULL
);
