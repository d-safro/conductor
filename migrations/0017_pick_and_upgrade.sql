-- Hand-picked downloads and upgrades to a better copy of a song you already have.
ALTER TABLE request ADD COLUMN same_album INTEGER;               -- the file is from the album asked from: 1, 0, NULL = can't tell
ALTER TABLE request ADD COLUMN manual INTEGER NOT NULL DEFAULT 0; -- picked by hand on the admin page
ALTER TABLE request ADD COLUMN skipped_from TEXT;                -- status before it was set aside ("skipped")
ALTER TABLE request ADD COLUMN upgrade_tried INTEGER NOT NULL DEFAULT 0; -- looked for a better copy once
ALTER TABLE request ADD COLUMN up_user TEXT;                     -- the better copy being downloaded (status "upgrading")
ALTER TABLE request ADD COLUMN up_file TEXT;
ALTER TABLE request ADD COLUMN up_since INTEGER;
