-- When a smart playlist's songs were last worked out from its rules.
ALTER TABLE playlist ADD COLUMN evaluated_at INTEGER;
