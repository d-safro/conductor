-- Songs whose tags may be Windows-1251 (Cyrillic) text read as Latin-1 (letters in U+00C0–U+00FF, Ё/ё as
-- U+00A8/U+00B8): the next scan reads their tags again, since their files haven't changed. Others with Western
-- accents are read again too, once, and stay as they are.
UPDATE track SET mtime = -1
 WHERE title GLOB '*[À-ÿ¨¸]*' OR artist_name GLOB '*[À-ÿ¨¸]*' OR album_name GLOB '*[À-ÿ¨¸]*';
