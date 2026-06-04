use std::{
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::Deserialize;
use sqlx::{
    AssertSqlSafe, Database, Decode, Encode, Executor, FromRow, IntoArguments, Sqlite, Type,
    query::QueryAs,
    sqlite::{SqliteArgumentsBuffer, SqliteTypeInfo, SqliteValueRef},
};

use crate::{
    app, dir_size,
    indexing::IndexEvent,
    web::{Order, Sort},
};

fn bind_vec<'q, DB, O, B>(
    mut q: QueryAs<'q, DB, O, <DB as Database>::Arguments>,
    binds: &'q [B],
) -> QueryAs<'q, DB, O, <DB as Database>::Arguments>
where
    DB: Database,
    B: 'q + Encode<'q, DB> + Type<DB>,
    O: for<'r> FromRow<'r, DB::Row> + Send,
    <DB as Database>::Arguments: IntoArguments<DB>,
{
    for b in binds {
        q = q.bind(b);
    }

    q
}

fn expand_vec_placeholder(q: &str, len: usize) -> String {
    let mut r = String::from("(");
    for _ in 1..len {
        r.push_str("?, ");
    }
    r.push_str("?)");
    q.replace("(?)", &r)
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub(crate) enum PathRecordKind {
    File,
    Symlink,
    Directory,
}

impl PathRecordKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Symlink => "symlink",
            Self::Directory => "directory",
        }
    }
}

impl sqlx::Type<Sqlite> for PathRecordKind {
    fn type_info() -> SqliteTypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }
}

impl<'r> Decode<'r, Sqlite> for PathRecordKind {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as Decode<Sqlite>>::decode(value)?;

        match s.as_str() {
            "file" => Ok(Self::File),
            "symlink" => Ok(Self::Symlink),
            "directory" => Ok(Self::Directory),
            _ => Err(format!("invalid status: {s}").into()),
        }
    }
}

impl Encode<'_, Sqlite> for PathRecordKind {
    fn encode_by_ref(
        &self,
        buf: &mut SqliteArgumentsBuffer,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <&str as Encode<Sqlite>>::encode(self.as_str(), buf)
    }
}

pub(crate) struct NewPathRecord {
    pub(crate) name: String,
    pub(crate) parent: Option<String>,
    pub(crate) kind: PathRecordKind,
    pub(crate) size: i64,
    pub(crate) last_modified: Option<i64>,
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub(crate) struct ExistingPathRecord {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) parent: Option<String>,
    pub(crate) kind: PathRecordKind,
    pub(crate) size: i64,
    pub(crate) last_modified: Option<i64>,
    pub(crate) processed: i64,
}

impl<T: AsRef<Path>> From<T> for NewPathRecord {
    fn from(path: T) -> Self {
        let path = path.as_ref();
        let metadata = path.metadata().ok();
        let kind = if path.is_dir() {
            PathRecordKind::Directory
        } else if path.is_symlink() {
            PathRecordKind::Symlink
        } else {
            PathRecordKind::File
        };
        let size = (if path.is_dir() {
            dir_size(path)
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        })
        .cast_signed();
        let last_modified = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().cast_signed());

        NewPathRecord {
            kind,
            size,
            last_modified,
            name: path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or(path.to_string_lossy().into_owned()),
            parent: path.parent().map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

impl From<&IndexEvent> for NewPathRecord {
    fn from(value: &IndexEvent) -> Self {
        match value {
            IndexEvent::Update(path_buf)
            | IndexEvent::Remove(path_buf)
            | IndexEvent::ForceUpdate(path_buf) => path_buf.into(),
        }
    }
}

impl NewPathRecord {
    pub(crate) async fn find_existing<'e, E>(
        &self,
        db: E,
    ) -> Result<Option<ExistingPathRecord>, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query_as::<_, ExistingPathRecord>(
            "SELECT * FROM paths WHERE name = ? AND parent = ? LIMIT 1",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .fetch_optional(db)
        .await
    }

    pub(crate) async fn insert<'e, E>(self, db: E) -> Result<ExistingPathRecord, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query_as::<_, ExistingPathRecord>(
            r"
            INSERT INTO paths (name, parent, kind, size, last_modified)
            VALUES (?, ?, ?, ?, ?)
            RETURNING *
            ",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .bind(&self.kind)
        .bind(self.size)
        .bind(self.last_modified)
        .fetch_one(db)
        .await
    }
}

impl ExistingPathRecord {
    #[must_use]
    pub(crate) fn path_buf(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }

    pub(crate) async fn remove<'e, E>(&self, db: E) -> Result<Vec<ExistingPathRecord>, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query_as::<_, ExistingPathRecord>(
            r"
            DELETE FROM paths WHERE (name = ? AND parent = ?)
            OR parent LIKE ? || '%'
            RETURNING *
            ",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .bind(self.path_buf().to_string_lossy())
        .fetch_all(db)
        .await
    }

    pub(crate) async fn index_content(&mut self, content: &str) -> Result<(), sqlx::Error> {
        let mut txn = app::db().begin().await?;

        sqlx::query(
            r"
            REPLACE INTO content_index (rowid, name, content)
            VALUES (?, ?, ?)
            ",
        )
        .bind(self.id)
        .bind(&self.name)
        .bind(content)
        .execute(&mut *txn)
        .await?;

        self.mark_as_processed(&mut *txn).await?;

        txn.commit().await?;
        Ok(())
    }

    pub(crate) async fn touch<'e, E>(
        &mut self,
        size: i64,
        last_modified: Option<i64>,
        db: E,
    ) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        *self = sqlx::query_as::<_, Self>(
            r"
            UPDATE paths
            SET size = ?, last_modified = ?
            WHERE id = ?
            RETURNING *
            ",
        )
        .bind(size)
        .bind(last_modified)
        .bind(self.id)
        .fetch_one(db)
        .await?;

        Ok(())
    }

    pub(crate) fn is_processed(&self) -> bool {
        self.processed == 1
    }

    pub(crate) async fn mark_as_processed<'e, E>(&mut self, db: E) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        *self =
            sqlx::query_as::<_, Self>("UPDATE paths SET processed = 1 WHERE id = ? RETURNING *")
                .bind(self.id)
                .fetch_one(db)
                .await?;

        Ok(())
    }

    pub(crate) fn is_outdated(&self, new: &NewPathRecord) -> bool {
        self.last_modified != new.last_modified
    }
}

pub(crate) async fn list_paths(
    paths: &[&str],
    sort: &Sort,
    order: &Order,
) -> Result<Vec<ExistingPathRecord>, sqlx::Error> {
    let sql = expand_vec_placeholder(
        &format!(
            r"
            SELECT * FROM paths
            WHERE parent IN (?)
            ORDER BY
                CASE WHEN kind = 'directory' THEN 0 ELSE 1 END,
                {} {}
            ",
            sort.as_str(),
            order.as_str()
        ),
        paths.len(),
    );
    let query = sqlx::query_as::<_, ExistingPathRecord>(AssertSqlSafe(sql));

    bind_vec(query, paths).fetch_all(app::db()).await
}

const FILENAME_QUERY: &str = "SELECT * FROM paths WHERE name LIKE '%' || ? || '%' LIMIT 20";

const FTS_QUERY: &str = r"
    WITH
    name_hits AS (
        SELECT
            f.*,
            1 AS result_group,
            NULL AS rank
        FROM paths AS f
        WHERE f.name LIKE '%' || ?1 || '%'
    ),

    fts_hits AS (
        SELECT
            f.*,
            2 AS result_group,
            bm25(content_index, 10.0, 5.0) AS rank
        FROM content_index
        JOIN paths AS f
          ON f.id = content_index.rowid
        WHERE content_index MATCH ?1
          AND f.id NOT IN (SELECT id FROM name_hits)
    )

    SELECT *
    FROM (
        SELECT * FROM name_hits
        UNION ALL
        SELECT * FROM fts_hits
    )
    ORDER BY
        result_group ASC,
        rank ASC
    LIMIT 20
";

async fn is_valid_fts(query: &str) -> bool {
    sqlx::query("SELECT content FROM fts_query_validator WHERE content MATCH ?")
        .bind(query)
        .fetch_optional(app::db())
        .await
        .is_ok()
}

pub(crate) async fn search(term: &str) -> Result<Vec<ExistingPathRecord>, sqlx::Error> {
    sqlx::query_as::<_, ExistingPathRecord>(if is_valid_fts(term).await {
        FTS_QUERY
    } else {
        FILENAME_QUERY
    })
    .bind(term)
    .fetch_all(app::db())
    .await
}

#[derive(FromRow)]
pub(crate) struct ExistingShareRecord {
    pub(crate) id: i64,
    pub(crate) path_id: i64,
    pub(crate) url: String,
    pub(crate) password: Option<String>,
    pub(crate) accessed: i64,
}

impl ExistingShareRecord {
    async fn find_by_url(url: &str) -> Result<Option<Self>, sqlx::Error> {
        sqlx::query_as::<_, Self>("SELECT * FROM shares WHERE url = ? LIMIT 1")
            .bind(url)
            .fetch_optional(app::db())
            .await
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct NewShareRecord {
    pub(crate) path_id: i64,
    pub(crate) url: String,
    pub(crate) expires_at: Option<i64>,
    pub(crate) password: Option<String>,
}

impl NewShareRecord {
    pub(crate) async fn new(path_id: i64) -> anyhow::Result<Self> {
        const MAX_RETRIES: usize = 8;
        let mut retry = 0;

        while retry < MAX_RETRIES {
            let url = nanoid::nanoid!(10, &nanoid::alphabet::SAFE);
            if ExistingShareRecord::find_by_url(&url).await?.is_none() {
                return Ok(Self {
                    path_id,
                    url,
                    expires_at: None,
                    password: None,
                });
            }

            retry += 1;
        }

        anyhow::bail!("can't assign a unique id to share record");
    }
}
