CREATE TABLE "fayls" (
  "id" integer PRIMARY KEY,
  "name" varchar NOT NULL,
  "parent" varchar,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL,
  "checksum" integer,
  "last_modified" integer,
  UNIQUE ("parent", "name")
);
