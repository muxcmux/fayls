use anyhow::Result;
use sqlx::{Executor, Sqlite};

use crate::fayls::{Fayl, FaylKind};

pub(crate) async fn index<'e, E>(fayl: &Fayl, db: E) -> Result<()>
where
    E: Executor<'e, Database = Sqlite>,
{
    if fayl.kind != FaylKind::File {
        return Ok(());
    }

    match tokio::fs::read_to_string(fayl.path()).await {
        Ok(content) => {
            tracing::info!("indexing content for {}", fayl.name);
            sqlx::query(
                r"
                INSERT INTO content_index (rowid, name, content)
                VALUES (?, ?, ?)
            ",
            )
            .bind(fayl.id.cast_signed())
            .bind(&fayl.name)
            .bind(&content)
            .execute(db)
            .await?;
        }
        Err(err) => tracing::error!("can't read contents for {}:\n{err}", fayl.path().display()),
    }

    Ok(())
}
