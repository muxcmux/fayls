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

CREATE TRIGGER paths_after_delete AFTER DELETE ON paths
FOR EACH ROW
BEGIN
    DELETE FROM content_index WHERE (old.id = rowid);
END;
