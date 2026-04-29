use anyhow::{Result, anyhow};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};
use tokio_util::sync::CancellationToken;

use crc_fast::checksum_file;
use sqlx::{
    Decode, Encode, Executor, Sqlite, SqlitePool,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};
use tokio::{
    sync::{
        Semaphore,
        mpsc::{UnboundedReceiver, UnboundedSender},
    },
    task::JoinSet,
};
use walkdir::{DirEntry, WalkDir};

use crate::config;

#[derive(Clone, serde::Serialize, PartialEq)]
pub enum FaylKind {
    File,
    Symlink,
    Directory,
}

impl sqlx::Type<Sqlite> for FaylKind {
    fn type_info() -> SqliteTypeInfo {
        <String as sqlx::Type<Sqlite>>::type_info()
    }
}

impl<'r> Decode<'r, Sqlite> for FaylKind {
    fn decode(value: SqliteValueRef<'r>) -> Result<Self, sqlx::error::BoxDynError> {
        let s = <String as Decode<Sqlite>>::decode(value)?;

        match s.as_str() {
            "file" => Ok(FaylKind::File),
            "symlink" => Ok(FaylKind::Symlink),
            "directory" => Ok(FaylKind::Directory),
            _ => Err(format!("invalid status: {s}").into()),
        }
    }
}

impl<'q> Encode<'q, Sqlite> for FaylKind {
    fn encode_by_ref(
        &self,
        buf: &mut Vec<SqliteArgumentValue<'q>>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        let s = match self {
            FaylKind::File => "file",
            FaylKind::Symlink => "symlink",
            FaylKind::Directory => "directory",
        };

        <&str as Encode<Sqlite>>::encode(s, buf)
    }
}

struct NewFayl {
    name: String,
    parent: Option<String>,
    kind: FaylKind,
    size: i64,
    last_modified: Option<i64>,
    checksum: Option<i64>,
}

#[derive(Clone, serde::Serialize, sqlx::FromRow)]
pub struct ExistingFayl {
    pub id: i64,
    pub name: String,
    pub parent: Option<String>,
    pub kind: FaylKind,
    pub size: i64,
    pub last_modified: Option<i64>,
    pub checksum: Option<i64>,
    pub processed: i64,
}

pub struct ContentIndexable(ExistingFayl);

impl ContentIndexable {
    #[must_use]
    pub fn fayl(&self) -> &ExistingFayl {
        &self.0
    }

    pub(crate) async fn index_content(
        &mut self,
        db: &SqlitePool,
        content: &str,
    ) -> Result<(), sqlx::Error> {
        let mut txn = db.begin().await?;

        sqlx::query(
            r"
            INSERT INTO content_index (rowid, name, content)
            VALUES (?, ?, ?)
            ",
        )
        .bind(self.0.id)
        .bind(&self.0.name)
        .bind(content)
        .execute(&mut *txn)
        .await?;

        self.0.mark_as_processed(&mut *txn).await?;

        txn.commit().await?;
        Ok(())
    }

    pub(crate) async fn mark_as_processed(&mut self, db: &SqlitePool) -> Result<(), sqlx::Error> {
        self.0.mark_as_processed(db).await
    }
}

impl NewFayl {
    pub fn path(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }

    async fn find_existing<'e, E>(&self, db: E) -> Result<Option<ExistingFayl>, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query_as::<_, ExistingFayl>(
            "SELECT * FROM fayls WHERE name = ? AND parent = ? LIMIT 1",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .fetch_optional(db)
        .await
    }

    async fn insert<'e, E>(self, db: E) -> Result<ExistingFayl, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query_as::<_, ExistingFayl>(
            r"
            INSERT INTO fayls (name, parent, kind, size, checksum, last_modified, processed)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            RETURNING *
            ",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .bind(&self.kind)
        .bind(self.size)
        .bind(self.checksum)
        .bind(self.last_modified)
        .bind(i32::from(FaylKind::File != self.kind))
        .fetch_one(db)
        .await
    }
}

impl ExistingFayl {
    #[must_use]
    pub fn path(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }

    fn into_content_indexable(self) -> Option<ContentIndexable> {
        match self.kind {
            FaylKind::File => Some(ContentIndexable(self)),
            _ => None,
        }
    }

    async fn touch<'e, E>(
        &mut self,
        size: i64,
        checksum: Option<i64>,
        last_modified: Option<i64>,
        db: E,
    ) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        *self = sqlx::query_as::<_, Self>(
            r"
            UPDATE fayls
            SET size = ?, checksum = ?, last_modified = ?
            WHERE id = ?
            RETURNING *
            ",
        )
        .bind(size)
        .bind(checksum)
        .bind(last_modified)
        .bind(self.id)
        .fetch_one(db)
        .await?;

        Ok(())
    }

    fn is_processed(&self) -> bool {
        self.processed == 1
    }

    async fn mark_as_processed<'e, E>(&mut self, db: E) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        *self =
            sqlx::query_as::<_, Self>("UPDATE fayls SET processed = 1 WHERE id = ? RETURNING *")
                .bind(self.id)
                .fetch_one(db)
                .await?;

        Ok(())
    }

    fn is_outdated(&self, new: &NewFayl) -> bool {
        self.last_modified != new.last_modified || self.checksum != new.checksum
    }
}

impl From<DirEntry> for NewFayl {
    fn from(entry: DirEntry) -> Self {
        let metadata = entry.metadata().ok();
        let kind = if entry.file_type().is_dir() {
            FaylKind::Directory
        } else if entry.file_type().is_symlink() {
            FaylKind::Symlink
        } else {
            FaylKind::File
        };
        let size = (if entry.file_type().is_dir() {
            dir_size(entry.path())
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        })
        .cast_signed();
        let last_modified = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().cast_signed());

        let checksum = checksum_file(
            crc_fast::CrcAlgorithm::Crc32Iscsi,
            &entry.path().to_string_lossy(),
            None,
        )
        .ok()
        .map(u64::cast_signed);

        Self {
            kind,
            size,
            last_modified,
            checksum,
            name: entry.file_name().to_string_lossy().into_owned(),
            parent: entry
                .path()
                .parent()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

fn dir_size(path: &Path) -> u64 {
    WalkDir::new(path)
        .min_depth(1)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(entry),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(
                    "failed to read directory tree entry under {}: {error}",
                    path.display()
                );
                None
            }
        })
        .fold(0, |acc, entry| match entry.metadata() {
            Ok(metadata) => acc.saturating_add(metadata.len()),
            Err(error) => {
                tracing::warn!(
                    "failed to read metadata for {}: {error}",
                    entry.path().display()
                );
                acc
            }
        })
}

pub async fn start_scanning(
    mut rx: UnboundedReceiver<()>,
    tx: UnboundedSender<Vec<DirEntry>>,
    token: CancellationToken,
) {
    while rx.recv().await.is_some() {
        if token.is_cancelled() {
            break;
        }

        scan(&tx, &token);
    }

    tracing::info!("scanning stopped");
}

fn scan(tx: &UnboundedSender<Vec<DirEntry>>, token: &CancellationToken) {
    let batch_size = config::get().app.batch_size;

    let paths: HashSet<PathBuf> = config::get()
        .app
        .sources
        .iter()
        .filter_map(|p| {
            p.canonicalize()
                .map_err(|err| {
                    tracing::warn!(
                        "failed to canonicalize path for source {} ({})",
                        p.display(),
                        err
                    );
                })
                .ok()
        })
        .collect();
    let mut batch = Vec::with_capacity(batch_size);

    for path in paths {
        for entry in WalkDir::new(path).min_depth(1) {
            if token.is_cancelled() {
                return;
            }

            match entry {
                Ok(entry) => {
                    batch.push(entry);
                    if batch.len() >= batch_size {
                        _ = tx.send(std::mem::take(&mut batch));
                    }
                }
                Err(error) => tracing::warn!("failed to read directory entry: {error}"),
            }
        }
    }

    if !batch.is_empty() {
        _ = tx.send(batch);
    }
}

pub(crate) async fn start_indexing(
    db: SqlitePool,
    mut rx: UnboundedReceiver<Vec<DirEntry>>,
    tx: UnboundedSender<ContentIndexable>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(config::get().app.max_concurrent_batches));

    while let Some(batch) = rx.recv().await {
        if token.is_cancelled() {
            tracing::info!("breaking indexing recv loop");
            break;
        }

        let db = db.clone();
        let tx = tx.clone();
        let token = token.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        queue.spawn(async move {
            if let Err(err) = index(batch, db, tx, token).await {
                tracing::error!("batch error: {err}");
            }
            drop(permit);
        });
    }

    tracing::info!("batch indexing stopped");
}

async fn index(
    entries: Vec<DirEntry>,
    db: SqlitePool,
    tx: UnboundedSender<ContentIndexable>,
    token: CancellationToken,
) -> Result<()> {
    let mut to_reindex = Vec::with_capacity(entries.len());
    let mut to_insert = Vec::with_capacity(entries.len());
    let mut to_update = Vec::with_capacity(entries.len());

    for entry in entries {
        if token.is_cancelled() {
            break;
        }

        let new = NewFayl::from(entry);

        let existing = new
            .find_existing(&db)
            .await
            .map_err(|_| anyhow!("find existing failed"))?;

        if let Some(existing) = existing {
            if existing.is_outdated(&new) {
                tracing::info!("reindexing: {}", &existing.path().display());
                to_update.push((existing, new.size, new.checksum, new.last_modified));
            } else if !existing.is_processed() {
                tracing::info!("reindexing: {}", &existing.path().display());
                to_reindex.push(existing.into_content_indexable());
            }
        } else {
            tracing::info!("indexing: {}", new.path().display());
            to_insert.push(new);
        }
    }

    let mut txn = db.begin().await.map_err(|_| anyhow!("txn failed"))?;
    for new in to_insert {
        let existing = new
            .insert(&mut *txn)
            .await
            .map_err(|_| anyhow!("insert failed"))?;
        if !existing.is_processed() {
            to_reindex.push(existing.into_content_indexable());
        }
    }
    for (mut existing, size, checksum, last_modified) in to_update {
        existing
            .touch(size, checksum, last_modified, &mut *txn)
            .await
            .map_err(|_| anyhow!("existing touch failed"))?;
        to_reindex.push(existing.into_content_indexable());
    }
    txn.commit()
        .await
        .map_err(|_| anyhow!("txn commit failed"))?;

    for item in to_reindex.into_iter().flatten() {
        if token.is_cancelled() {
            break;
        }

        tx.send(item)?;
    }

    Ok(())
}
