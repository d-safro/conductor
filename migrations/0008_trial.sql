-- Songs on trial: who played it (their "On trial" playlist lists it), kept (hearted / playlisted / rated 4+ before
-- it arrived or since), and whether it has been listed in "On trial" (taking it out there by hand deletes it).
ALTER TABLE request ADD COLUMN trial_user TEXT;
ALTER TABLE request ADD COLUMN keep INTEGER NOT NULL DEFAULT 0;
ALTER TABLE request ADD COLUMN kept_at INTEGER;
ALTER TABLE request ADD COLUMN listed INTEGER NOT NULL DEFAULT 0;
