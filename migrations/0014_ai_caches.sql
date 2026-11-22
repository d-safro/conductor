-- Lyrics translations from the local AI, done once per song, language, mode, model and text.
CREATE TABLE translation_cache (
    key         TEXT PRIMARY KEY,
    json        TEXT NOT NULL,
    created_at  INTEGER NOT NULL
);
