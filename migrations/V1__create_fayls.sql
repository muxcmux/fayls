CREATE TABLE "fayls" (
  "path" varchar UNIQUE PRIMARY KEY,
  "parent" varchar,
  "name" varchar NOT NULL,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL DEFAULT 0,
  "hash" varchar NOT NULL,
  "last_modified" integer NOT NULL DEFAULT 0,
) WITHOUT ROWID;

CREATE INDEX "idx_parent" ON "fayls" ("parent");
