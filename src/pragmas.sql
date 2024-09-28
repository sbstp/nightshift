-- https://phiresky.github.io/blog/2020/sqlite-performance-tuning/
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 1073741824; -- 1 GiB
PRAGMA foreign_keys = ON;

-- SQLCipher turns this ON by default but it makes DELETE performance horrible
-- for large files. By turning this off, deleted pages are not zeroed but they
-- are still encrypted. Using the nightshift optimize command will get rid of
-- pages that contain deleted data.
PRAGMA secure_delete = OFF;
