use anyhow::Result;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio_util::sync::CancellationToken;

use crc_fast::checksum_file;
use sqlx::{
    Decode, Encode, Executor, Sqlite, SqlitePool,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};
use tokio::sync::mpsc::{Receiver, Sender};
use walkdir::{DirEntry, WalkDir};

use crate::config;

#[derive(serde::Serialize, PartialEq)]
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

pub struct Indexable(pub Fayl);

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct Fayl {
    pub id: i64,
    pub name: String,
    pub parent: Option<String>,
    pub kind: FaylKind,
    pub size: i64,
    pub last_modified: Option<i64>,
    pub checksum: Option<i64>,
    pub content_indexed: i64,
}

impl Fayl {
    #[must_use]
    pub fn path(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }

    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn to_indexable(self) -> Option<Indexable> {
        match self.kind {
            FaylKind::File => Some(Indexable(self)),
            _ => None,
        }
    }

    async fn existing<'e, E>(&self, db: E) -> Result<Option<Self>, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query_as::<_, Self>("SELECT * FROM fayls WHERE name = ? AND parent = ? LIMIT 1")
            .bind(&self.name)
            .bind(&self.parent)
            .fetch_optional(db)
            .await
    }

    async fn touch<'e, E>(&mut self, id: i64, db: E) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        *self = sqlx::query_as::<_, Self>(
            r"
            UPDATE fayls
            SET size = ?, checksum = ?, last_modified = ?
            WHERE id = ?
            ",
        )
        .bind(self.size)
        .bind(self.checksum)
        .bind(self.last_modified)
        .bind(id)
        .fetch_one(db)
        .await?;

        Ok(())
    }

    async fn insert<'e, E>(&mut self, db: E) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        *self = sqlx::query_as::<_, Self>(
            r"
            INSERT INTO fayls (name, parent, kind, size, checksum, last_modified)
            VALUES (?, ?, ?, ?, ?, ?)
            RETURNING *
            ",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .bind(&self.kind)
        .bind(self.size)
        .bind(self.checksum)
        .bind(self.last_modified)
        .fetch_one(db)
        .await?;

        Ok(())
    }

    async fn is_indexed<'e, E>(&self, db: E) -> bool
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query("SELECT id FROM content_index WHERE rowid = ?")
            .bind(self.id)
            .fetch_optional(db)
            .await
            .is_ok_and(|r| r.is_some())
    }

    pub(crate) async fn index<'e, E>(&self, db: E, content: &str) -> Result<(), sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        sqlx::query(
            r"
                INSERT INTO content_index (rowid, name, content)
                VALUES (?, ?, ?)
            ",
        )
        .bind(self.id)
        .bind(&self.name)
        .bind(content)
        .execute(db)
        .await?;

        Ok(())
    }

    fn is_outdated(&self, other: &Self) -> bool {
        self.last_modified != other.last_modified || self.checksum != other.checksum
    }
}

impl From<DirEntry> for Fayl {
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

        Fayl {
            id: 0,
            kind,
            size,
            last_modified,
            checksum,
            content_indexed: 0,
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

const BATCH_SIZE: usize = 1000;

pub async fn start_scanning(
    mut rx: Receiver<()>,
    tx: Sender<Vec<DirEntry>>,
    token: CancellationToken,
) {
    while rx.recv().await.is_some() {
        if token.is_cancelled() {
            break;
        }

        scan(&tx, &token).await;
    }

    tracing::info!("scanning stopped");
}

async fn scan(tx: &Sender<Vec<DirEntry>>, token: &CancellationToken) {
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
    let mut batch = Vec::with_capacity(BATCH_SIZE);

    for path in paths {
        for entry in WalkDir::new(path).min_depth(1) {
            if token.is_cancelled() {
                return;
            }

            match entry {
                Ok(entry) => {
                    batch.push(entry);
                    if batch.len() >= BATCH_SIZE {
                        _ = tx.send(std::mem::take(&mut batch)).await;
                    }
                }
                Err(error) => tracing::warn!("failed to read directory entry: {error}"),
            }
        }
    }

    if !batch.is_empty() {
        _ = tx.send(batch).await;
    }
}

pub(crate) async fn start_indexing(
    db: SqlitePool,
    mut rx: Receiver<Vec<DirEntry>>,
    tx: Sender<Indexable>,
    token: CancellationToken,
) {
    while let Some(batch) = rx.recv().await {
        if token.is_cancelled() {
            break;
        }

        if let Err(err) = index(batch, &db, &tx, &token).await {
            tracing::error!("{err}");
        }
    }

    tracing::info!("batch indexing stopped");
}

async fn index(
    entries: Vec<DirEntry>,
    db: &SqlitePool,
    tx: &Sender<Indexable>,
    token: &CancellationToken,
) -> Result<()> {
    let mut txn = db.begin().await?;
    let mut indexables = Vec::with_capacity(entries.len());

    for entry in entries {
        if token.is_cancelled() {
            break;
        }

        let mut new = Fayl::from(entry);

        let existing = new.existing(&mut *txn).await?;

        if let Some(existing) = existing {
            if new.is_outdated(&existing) {
                new.touch(existing.id, &mut *txn).await?;
                tracing::info!("reindexed: {}", &new.path().display());
                indexables.push(new.to_indexable());
            } else if !existing.is_indexed(&mut *txn).await {
                indexables.push(new.to_indexable());
            }
        } else {
            new.insert(&mut *txn).await?;
            tracing::info!("indexed: {}", &new.path().display());
            indexables.push(new.to_indexable());
        }
    }

    txn.commit().await?;

    for path in indexables.into_iter().flatten() {
        if token.is_cancelled() {
            break;
        }

        tx.send(path).await?;
    }

    Ok(())
}
