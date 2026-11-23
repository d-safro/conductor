-- Sound levelling (src/loudness.rs): loudness measured by Conductor for songs whose files have no ReplayGain tags.
-- Kept apart from the rg_* columns, which hold only what the tags say (a scan rewrites those): tags always win, and
-- a rescan never loses a measurement. The file's size and time when it was measured: a changed file is measured
-- again. Measured but no loudness: ffmpeg couldn't read it (tried again when the file changes).
ALTER TABLE track ADD COLUMN lm_loudness REAL;        -- integrated loudness, LUFS (EBU R128 / ITU-R BS.1770)
ALTER TABLE track ADD COLUMN lm_peak REAL;            -- true peak, linear (1.0 = full scale)
ALTER TABLE track ADD COLUMN lm_size INTEGER;
ALTER TABLE track ADD COLUMN lm_mtime INTEGER;
-- the album's, from its measured songs (on each of its songs, so a song's row has all it needs)
ALTER TABLE track ADD COLUMN lm_album_loudness REAL;
ALTER TABLE track ADD COLUMN lm_album_peak REAL;
