-- Each user's own scrobbling accounts: a ListenBrainz user token, or a Last.fm session key. The secret is encrypted
-- with secret.key, as passwords are.
CREATE TABLE scrobble_account (
    user_id   TEXT NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    service   TEXT NOT NULL,                 -- listenbrainz · lastfm
    secret    TEXT NOT NULL,
    name      TEXT NOT NULL DEFAULT '',      -- the account's name there
    broken    INTEGER NOT NULL DEFAULT 0,    -- the service no longer accepts it: link it again
    added_at  INTEGER NOT NULL,
    PRIMARY KEY (user_id, service)
);
-- Plays waiting to be sent now say whose they are and where to: no user is the settings' listenbrainz_token.
ALTER TABLE lb_queue ADD COLUMN user_id TEXT REFERENCES user(id) ON DELETE CASCADE;
ALTER TABLE lb_queue ADD COLUMN service TEXT NOT NULL DEFAULT 'listenbrainz';
