-- Ids carried over from another server (Navidrome): kind (track|album|artist) + the key the scanner uses
-- (track: "<library>:<path>", album: album artist + name, artist: name, all lower-case) -> the old id.
DROP TABLE legacy_id;
CREATE TABLE id_override (
    kind  TEXT NOT NULL,
    key   TEXT NOT NULL,
    id    TEXT NOT NULL,
    PRIMARY KEY (kind, key)
);
