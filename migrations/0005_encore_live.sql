-- Encore: small values kept in step across a user's devices (newest wins), and Jams that survive a restart.
CREATE TABLE sync_kv (
    user_id     TEXT NOT NULL,
    key         TEXT NOT NULL,
    value_json  TEXT NOT NULL,
    at          INTEGER NOT NULL,      -- ms, the device's time of the change (capped to now + 1 min)
    by_device   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (user_id, key)
);

CREATE TABLE jam (
    token       TEXT PRIMARY KEY,
    owner_id    TEXT NOT NULL,
    host_name   TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    seen_at     INTEGER NOT NULL
);

CREATE INDEX play_user_track_time ON play(user_id, track_id, played_at);
