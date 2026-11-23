-- Albums matched to a Deezer edition by 1.0.0 may have been matched to a deluxe one ("Sticky Fingers" completed
-- with the 33-song Super Deluxe): they're matched again (the next time each album is opened).
DELETE FROM catalog_cache WHERE kind = 'album';
