CREATE TABLE shares (
  id integer PRIMARY KEY,
  path_id integer NOT NULL,
  url varchar NOT NULL,
  expires_at integer,
  password varchar,
  accessed integer NOT NULL DEFAULT 0,
  UNIQUE (url),
  FOREIGN KEY(path_id) REFERENCES paths(id) ON UPDATE CASCADE ON DELETE CASCADE
);
