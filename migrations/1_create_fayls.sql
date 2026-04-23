CREATE TABLE "fayls" (
  "path" varchar UNIQUE PRIMARY KEY,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL,
  "checksum" blob,
  "last_modified" integer
) WITHOUT ROWID;
