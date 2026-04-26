CREATE VIRTUAL TABLE "content_index" USING fts5(
  name,
  content,
  content="",
  tokenize="unicode61",
  detail=column
);

-- this is a test query
-- SELECT f.*, bm25(content_index, 2.0, 1.0) AS rank
-- FROM content_index
-- JOIN fayls f ON f.id = content_index.rowid
-- WHERE content_index MATCH ?
-- ORDER BY rank
-- LIMIT 50;
