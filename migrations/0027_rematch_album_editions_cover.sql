-- Up to 1.1.1, an edition with two takes of one of your songs counted it twice ("Sticky Fingers (Super Deluxe)" has
-- "Wild Horses (2009 Mix)" and "(Acoustic Version)"), so a deluxe edition could still beat the album itself.
-- Albums are matched again (the next time each is opened).
DELETE FROM catalog_cache WHERE kind = 'album';
