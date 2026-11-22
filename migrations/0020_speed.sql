-- Speed with big libraries (measured with 100,000 songs: examples/bench_library.rs).
-- Songs in name order: search paging (apps that fetch the whole library 500 at a time) walks this, not a sort.
CREATE INDEX track_sort ON track(sort_title, id) WHERE missing_since IS NULL;
-- What a user starred, without looking at every song.
CREATE INDEX annotation_starred ON annotation(user_id, item_kind, starred_at) WHERE starred_at IS NOT NULL;
CREATE INDEX album_sort ON album(sort_name, id);
-- getGenres: songs and albums per genre, read in order from here.
CREATE INDEX track_genre_album ON track(genre, album_id) WHERE missing_since IS NULL;
