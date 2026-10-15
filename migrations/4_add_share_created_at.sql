CREATE TABLE shares_with_created_at (
  id integer PRIMARY KEY,
  path_id integer NOT NULL,
  url varchar NOT NULL,
  expires_at integer,
  password varchar,
  accessed integer NOT NULL DEFAULT 0,
  created_at integer NOT NULL DEFAULT (CAST(strftime('%s', 'now') AS INTEGER)),
  UNIQUE (url),
  FOREIGN KEY(path_id) REFERENCES paths(id) ON UPDATE CASCADE ON DELETE CASCADE
);

INSERT INTO shares_with_created_at (id, path_id, url, expires_at, password, accessed)
SELECT id, path_id, url, expires_at, password, accessed
FROM shares;

DROP TABLE shares;
ALTER TABLE shares_with_created_at RENAME TO shares;
