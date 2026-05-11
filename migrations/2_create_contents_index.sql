CREATE VIRTUAL TABLE "content_index" USING fts5(
  name,
  content,
  content="",
  contentless_delete=1,
  tokenize="trigram",
  detail=full
);

CREATE VIRTUAL TABLE "fts_query_validator" USING fts5(
  name,
  content,
  content="",
  contentless_delete=1,
  tokenize="trigram",
  detail=full
);

CREATE TRIGGER fayls_after_delete AFTER DELETE ON fayls
FOR EACH ROW
BEGIN
    DELETE FROM content_index WHERE (old.id = rowid);
END;

-- this is a test query
-- SELECT f.*, bm25(content_index, 2.0, 1.0) AS score
-- FROM content_index
-- JOIN fayls f ON f.id = content_index.rowid
-- WHERE content_index MATCH ?
-- ORDER BY score
-- LIMIT 50;
