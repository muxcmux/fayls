use anyhow::Result;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio_util::sync::CancellationToken;

use crc_fast::checksum_file;
use sqlx::{
    Decode, Encode, Sqlite, SqlitePool,
    sqlite::{SqliteArgumentValue, SqliteTypeInfo, SqliteValueRef},
};
use tokio::sync::mpsc::Sender;
use walkdir::{DirEntry, WalkDir};

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

pub struct IndexablePath(pub PathBuf);

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct Fayl {
    pub id: u64,
    pub name: String,
    pub parent: Option<String>,
    pub kind: FaylKind,
    pub size: u64,
    pub last_modified: Option<u64>,
    pub checksum: Option<u64>,
}

impl Fayl {
    #[must_use]
    pub fn path(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }

    pub(crate) fn indexable_path(&self) -> Option<IndexablePath> {
        match &self.kind {
            FaylKind::File => Some(IndexablePath(self.path())),
            _ => None,
        }
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
        let size = if entry.file_type().is_dir() {
            dir_size(entry.path())
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        };
        let last_modified = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs());

        let checksum = checksum_file(
            crc_fast::CrcAlgorithm::Crc32Iscsi,
            &entry.path().to_string_lossy(),
            None,
        )
        .ok();

        Fayl {
            id: 0,
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

const BATCH_SIZE: usize = 10;

pub async fn scan(paths: &[PathBuf], tx: &Sender<Vec<DirEntry>>, token: &CancellationToken) {
    let paths: HashSet<PathBuf> = paths
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

pub(crate) async fn index(
    entries: Vec<DirEntry>,
    db: &SqlitePool,
    tx: &Sender<IndexablePath>,
    token: &CancellationToken,
) -> Result<()> {
    let mut txn = db.begin().await?;
    let mut paths_to_index = Vec::with_capacity(entries.len());

    for entry in entries {
        if token.is_cancelled() {
            break;
        }

        let new = Fayl::from(entry);

        let existing = sqlx::query_as::<_, Fayl>(
            r"
            SELECT id, name, parent, kind, size, checksum, last_modified
            FROM fayls
            WHERE name = ?
            AND parent = ?
            LIMIT 1
        ",
        )
        .bind(&new.name)
        .bind(&new.parent)
        .fetch_optional(&mut *txn)
        .await?;
        if let Some(existing) = existing {
            if new.last_modified != existing.last_modified || new.checksum != existing.checksum {
                sqlx::query(
                    r"
                    UPDATE fayls
                    SET size = ?, checksum = ?, last_modified = ?
                    WHERE id = ?
                ",
                )
                .bind(new.size.cast_signed())
                .bind(new.checksum.map(u64::cast_signed))
                .bind(new.last_modified.map(u64::cast_signed))
                .bind(existing.id.cast_signed())
                .execute(&mut *txn)
                .await?;
                tracing::info!("reindexed: {}", &new.path().display());
                paths_to_index.push(new.indexable_path());
            }
        } else {
            sqlx::query(
                r"
                INSERT INTO fayls (name, parent, kind, size, checksum, last_modified)
                VALUES (?, ?, ?, ?, ?, ?)
            ",
            )
            .bind(&new.name)
            .bind(&new.parent)
            .bind(&new.kind)
            .bind(new.size.cast_signed())
            .bind(new.checksum.map(u64::cast_signed))
            .bind(new.last_modified.map(u64::cast_signed))
            .execute(&mut *txn)
            .await?;
            tracing::info!("indexed: {}", &new.path().display());
            paths_to_index.push(new.indexable_path());
        }
    }

    txn.commit().await?;

    for path in paths_to_index.into_iter().flatten() {
        if token.is_cancelled() {
            break;
        }

        tx.send(path).await?;
    }

    Ok(())
}
