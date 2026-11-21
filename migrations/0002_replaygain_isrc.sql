-- ReplayGain and ISRC for songs (Encore reads both). Old rows get re-read on the next scan.
ALTER TABLE track ADD COLUMN isrc TEXT;
ALTER TABLE track ADD COLUMN rg_track_gain REAL;
ALTER TABLE track ADD COLUMN rg_track_peak REAL;
ALTER TABLE track ADD COLUMN rg_album_gain REAL;
ALTER TABLE track ADD COLUMN rg_album_peak REAL;
UPDATE track SET mtime = 0;
