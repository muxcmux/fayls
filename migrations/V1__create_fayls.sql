CREATE TABLE "fayls" (
  "path" varchar UNIQUE PRIMARY KEY,
  "parent" varchar,
  "name" varchar,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL,
  "checksum" blob,
  "last_modified" integer
) WITHOUT ROWID;

CREATE INDEX "idx_parent" ON "fayls" ("parent");
CREATE INDEX "idx_name" ON "fayls" ("name");
