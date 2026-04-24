CREATE TABLE "fayls" (
  "name" varchar NOT NULL,
  "parent" varchar,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL,
  "checksum" integer,
  "last_modified" integer,
  PRIMARY KEY ("parent", "name")
) WITHOUT ROWID;
