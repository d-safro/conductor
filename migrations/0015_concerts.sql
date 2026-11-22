-- Concerts near you, per user: the shows found last time (JSON), when each was first seen, and what it was
-- looked for with (town + whether a Ticketmaster key was used), so a change of town looks again at once.
CREATE TABLE concerts (
    user_id     TEXT PRIMARY KEY REFERENCES user(id) ON DELETE CASCADE,
    shows_json  TEXT NOT NULL DEFAULT '[]',
    first_json  TEXT NOT NULL DEFAULT '{}',
    city        TEXT NOT NULL DEFAULT '',
    source      TEXT NOT NULL DEFAULT '',
    prefs       TEXT NOT NULL DEFAULT '',
    at          INTEGER NOT NULL DEFAULT 0     -- seconds; 0 = never looked
);
