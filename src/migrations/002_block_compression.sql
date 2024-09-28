ALTER TABLE block ADD COLUMN compression INTEGER; -- 1 & NULL is LZ4, 0 is None, 2 is Zstd
