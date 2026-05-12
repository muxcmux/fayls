use anyhow::Result;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};
use tokio_util::sync::CancellationToken;

use sqlx::{
    Decode, Encode, Executor, Sqlite,
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

use crate::{app, config};

#[derive(Clone, serde::Serialize, PartialEq)]
pub enum FaylKind {
    File,
    Symlink,
    Directory,
}

impl FaylKind {
    fn as_str(&self) -> &'static str {
        match self {
            FaylKind::File => "file",
            FaylKind::Symlink => "symlink",
            FaylKind::Directory => "directory",
        }
    }
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
        <&str as Encode<Sqlite>>::encode(self.as_str(), buf)
    }
}

pub struct NewFayl {
    pub name: String,
    pub parent: Option<String>,
    pub kind: FaylKind,
    pub size: i64,
    pub last_modified: Option<i64>,
}

#[derive(Clone, serde::Serialize, sqlx::FromRow)]
pub struct ExistingFayl {
    pub id: i64,
    pub name: String,
    pub parent: Option<String>,
    pub kind: FaylKind,
    pub size: i64,
    pub last_modified: Option<i64>,
    pub processed: i64,
}

pub(crate) trait Entry {
    fn exists_on_disk(&self) -> bool;
    fn new_fayl(&self) -> NewFayl;
}

pub struct EntryFromWalkdir(DirEntry);
#[derive(PartialEq)]
pub struct EntryFromPathBuf(PathBuf);

impl From<PathBuf> for EntryFromPathBuf {
    fn from(value: PathBuf) -> Self {
        Self(value)
    }
}

impl Entry for EntryFromWalkdir {
    fn exists_on_disk(&self) -> bool {
        true
    }

    fn new_fayl(&self) -> NewFayl {
        let entry = &self.0;
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

        NewFayl {
            kind,
            size,
            last_modified,
            name: entry.file_name().to_string_lossy().into_owned(),
            parent: entry
                .path()
                .parent()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

impl Entry for EntryFromPathBuf {
    fn exists_on_disk(&self) -> bool {
        self.0.exists()
    }

    fn new_fayl(&self) -> NewFayl {
        let path = &self.0;
        let metadata = path.metadata().ok();
        let kind = if path.is_dir() {
            FaylKind::Directory
        } else if path.is_symlink() {
            FaylKind::Symlink
        } else {
            FaylKind::File
        };
        let size = (if path.is_dir() {
            dir_size(path.as_path())
        } else {
            metadata.as_ref().map_or(0, std::fs::Metadata::len)
        })
        .cast_signed();
        let last_modified = metadata
            .and_then(|m| m.modified().ok())
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().cast_signed());

        NewFayl {
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

struct DeletedFayl(ExistingFayl);

impl NewFayl {
    pub(crate) fn path_buf(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
    }

    async fn remove<'e, E>(&self, db: E) -> Result<Vec<DeletedFayl>, sqlx::Error>
    where
        E: Executor<'e, Database = Sqlite>,
    {
        let deleted = sqlx::query_as::<_, ExistingFayl>(
            r"
            DELETE FROM fayls WHERE (name = ? AND parent = ?)
            OR parent LIKE ? || '%'
            RETURNING *
            ",
        )
        .bind(&self.name)
        .bind(&self.parent)
        .bind(self.path_buf().to_string_lossy())
        .fetch_all(db)
        .await?;

        Ok(deleted.into_iter().map(DeletedFayl).collect())
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
            INSERT INTO fayls (name, parent, kind, size, last_modified)
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

impl ExistingFayl {
    #[must_use]
    pub fn path_buf(&self) -> PathBuf {
        match &self.parent {
            Some(p) => Path::new(p).join(&self.name),
            None => PathBuf::from(&self.name),
        }
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

    async fn touch<'e, E>(
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
            UPDATE fayls
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
        self.last_modified != new.last_modified
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
    tx: UnboundedSender<(Vec<EntryFromWalkdir>, usize)>,
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

fn scan(tx: &UnboundedSender<(Vec<EntryFromWalkdir>, usize)>, token: &CancellationToken) {
    let batch_size = config::get().indexing.batch_size;

    let paths = config::get().app.canonicalized_sources();
    let mut batch = Vec::with_capacity(batch_size);

    for path in paths {
        for entry in WalkDir::new(path).min_depth(0) {
            if token.is_cancelled() {
                return;
            }

            match entry {
                Ok(entry) => {
                    batch.push(EntryFromWalkdir(entry));
                    if batch.len() >= batch_size {
                        _ = tx.send((std::mem::take(&mut batch), 0));
                    }
                }
                Err(error) => tracing::warn!("failed to read directory entry: {error}"),
            }
        }
    }

    if !batch.is_empty() {
        _ = tx.send((batch, 0));
    }
}

pub(crate) async fn start_indexing_batches<T: Entry + Send + Sync + 'static>(
    mut batch_rx: UnboundedReceiver<(Vec<T>, usize)>,
    batch_tx: UnboundedSender<(Vec<T>, usize)>,
    index_tx: UnboundedSender<(ExistingFayl, usize)>,
    token: CancellationToken,
) {
    let mut queue: JoinSet<()> = JoinSet::new();
    let semaphore = Arc::new(Semaphore::new(
        config::get().indexing.max_concurrent_batches,
    ));

    while let Some((batch, retry)) = batch_rx.recv().await {
        if token.is_cancelled() {
            tracing::info!("breaking indexing recv loop");
            break;
        }

        let index_tx = index_tx.clone();
        let batch_tx = batch_tx.clone();
        let token = token.clone();
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        queue.spawn(async move {
            if let Err(err) = index_batch(&batch, index_tx, token).await {
                if retry > config::get().indexing.max_retries {
                    tracing::error!("batch error: {err}, giving up");
                } else {
                    let retry = retry + 1;
                    tracing::error!("batch error: {err}, retrying ({retry})");
                    _ = batch_tx.send((batch, retry));
                }
            }
            drop(permit);
        });
    }

    tracing::info!("batch indexing stopped");
}

async fn index_batch<T: Entry>(
    entries: &[T],
    tx: UnboundedSender<(ExistingFayl, usize)>,
    token: CancellationToken,
) -> Result<()> {
    let mut to_reindex = Vec::with_capacity(entries.len());
    let mut to_insert = Vec::with_capacity(entries.len());
    let mut to_update = Vec::with_capacity(entries.len());
    let mut to_delete = Vec::with_capacity(entries.len());

    for entry in entries {
        if token.is_cancelled() {
            break;
        }

        if !entry.exists_on_disk() {
            to_delete.push(entry.new_fayl());
            continue;
        }

        let new = entry.new_fayl();

        let existing = new.find_existing(app::db()).await?;

        if let Some(existing) = existing {
            if existing.is_outdated(&new) {
                tracing::info!("reindexing: {}", &existing.path_buf().display());
                to_update.push((existing, new.size, new.last_modified));
            } else if !existing.is_processed() {
                tracing::info!("reindexing: {}", &existing.path_buf().display());
                to_reindex.push(existing);
            }
        } else {
            tracing::info!("indexing: {}", new.path_buf().display());
            to_insert.push(new);
        }
    }

    let mut txn = app::db().begin().await?;

    for new in to_insert {
        let existing = new.insert(&mut *txn).await?;
        to_reindex.push(existing);
    }

    for (mut existing, size, last_modified) in to_update {
        existing.touch(size, last_modified, &mut *txn).await?;
        to_reindex.push(existing);
    }

    for fayl in to_delete {
        let deleted = fayl.remove(&mut *txn).await?;
    }

    txn.commit().await?;

    // refresh the unique parents of all the entries
    // web::broadcast(web::Event::Insert(to_send_insert_events));
    // web::broadcast(web::Event::Update(to_send_update_events));
    // web::broadcast(web::Event::Remove(
    //     to_send_remove_events.into_iter().flatten().collect(),
    // ));

    for item in to_reindex {
        if token.is_cancelled() {
            break;
        }

        tx.send((item, 0))?;
    }

    Ok(())
}
