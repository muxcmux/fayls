CREATE TABLE "fayls" (
  "path" varchar UNIQUE PRIMARY KEY,
  "parent" varchar,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL,
  "checksum" integer,
  "last_modified" integer
) WITHOUT ROWID;

CREATE INDEX "idx_parent" ON "fayls" ("parent");
