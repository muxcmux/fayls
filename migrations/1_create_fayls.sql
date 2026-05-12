CREATE TABLE "fayls" (
  "id" integer PRIMARY KEY,
  "name" varchar NOT NULL,
  "parent" varchar,
  "kind" varchar NOT NULL,
  "size" integer NOT NULL,
  "last_modified" integer,
  "processed" integer NOT NULL DEFAULT 0,
  UNIQUE ("parent", "name")
);
