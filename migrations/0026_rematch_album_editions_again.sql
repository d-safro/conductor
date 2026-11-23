-- 1.1.0 still matched some albums to a bigger edition when Deezer's exact search found only that one ("Sticky
-- Fingers" with the 33-song Super Deluxe; the 10-song remaster comes up only in its plain search). Matched again
-- (the next time each album is opened).
DELETE FROM catalog_cache WHERE kind = 'album';
