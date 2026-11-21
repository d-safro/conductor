-- Podcasts: shows and episodes are shared; where each user is in an episode is their own.
-- Ids match music-lookup's ("pc-" + sha1(feed url)[:10], "pe-" + sha1(show id + guid)[:12]).
CREATE TABLE podcast (
    id           TEXT PRIMARY KEY,
    url          TEXT NOT NULL UNIQUE,
    title        TEXT NOT NULL,
    author       TEXT NOT NULL DEFAULT '',
    description  TEXT NOT NULL DEFAULT '',
    image        TEXT NOT NULL DEFAULT '',
    added_at     INTEGER NOT NULL,
    checked_at   INTEGER,
    error        TEXT NOT NULL DEFAULT ''
);

CREATE TABLE episode (
    id            TEXT PRIMARY KEY,
    podcast_id    TEXT NOT NULL REFERENCES podcast(id) ON DELETE CASCADE,
    guid          TEXT NOT NULL,
    title         TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    published_at  INTEGER NOT NULL DEFAULT 0,
    duration      INTEGER NOT NULL DEFAULT 0,     -- seconds
    url           TEXT NOT NULL,
    size          INTEGER NOT NULL DEFAULT 0,
    content_type  TEXT NOT NULL DEFAULT 'audio/mpeg',
    image         TEXT NOT NULL DEFAULT '',
    file          TEXT,                           -- relative to the podcast folder, when saved on the server
    pinned        INTEGER NOT NULL DEFAULT 0      -- asked for (downloadPodcastEpisode): kept until deleted
);
CREATE INDEX episode_podcast ON episode(podcast_id, published_at);
CREATE INDEX episode_published ON episode(published_at);

CREATE TABLE podcast_progress (
    user_id      TEXT NOT NULL,
    episode_id   TEXT NOT NULL,
    position_ms  INTEGER NOT NULL DEFAULT 0,
    done         INTEGER NOT NULL DEFAULT 0,
    at           INTEGER NOT NULL,
    PRIMARY KEY (user_id, episode_id)
);

-- Where you stopped in a song (Subsonic bookmarks; podcast episodes use podcast_progress).
CREATE TABLE bookmark (
    user_id      TEXT NOT NULL,
    item_id      TEXT NOT NULL,
    position_ms  INTEGER NOT NULL,
    comment      TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_id, item_id)
);
