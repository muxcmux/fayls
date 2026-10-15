//! Background jobs running on the application's Tokio runtime.
//!
//! Request handlers can obtain a [`JobRunner`] from Salvo's depot and call
//! [`JobRunner::spawn`] to start a one-off job.

use std::{future::Future, time::Duration};

use sqlx::SqlitePool;
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// Work that can run in the background. Implementations should return errors
/// so the runner can log them without stopping other jobs.
pub trait Job: Send + Sync + 'static {
    fn run(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
}

#[derive(Clone)]
pub struct JobRunner {
    tracker: TaskTracker,
    shutdown: CancellationToken,
}

impl JobRunner {
    pub fn new(tracker: TaskTracker, shutdown: CancellationToken) -> Self {
        Self { tracker, shutdown }
    }

    /// Start one job without delaying the caller. The returned handle can be
    /// awaited when the caller needs to know that the job has finished.
    pub fn spawn<J: Job>(&self, job: J) -> JoinHandle<()> {
        let shutdown = self.shutdown.clone();
        self.tracker.spawn(async move {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {},
                () = run_job(&job) => {},
            }
        })
    }

    /// Run a job immediately, then at the given interval. Runs are sequential,
    /// so a slow job will never overlap with itself.
    pub fn every<J: Job>(&self, period: Duration, job: J) -> JoinHandle<()> {
        assert!(!period.is_zero(), "job interval must be nonzero");
        let shutdown = self.shutdown.clone();
        self.tracker.spawn(async move {
            let mut ticks = tokio::time::interval(period);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    _ = ticks.tick() => {},
                }

                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => break,
                    () = run_job(&job) => {},
                }
            }
        })
    }
}

async fn run_job<J: Job>(job: &J) {
    if let Err(error) = job.run().await {
        tracing::error!(job = std::any::type_name::<J>(), %error, "background job failed");
    }
}

pub(crate) struct DeleteExpiredShares;

impl Job for DeleteExpiredShares {
    async fn run(&self) -> anyhow::Result<()> {
        let deleted = delete_expired_shares(crate::app::db()).await?;

        if deleted > 0 {
            tracing::info!(deleted, "deleted expired share links");
        }

        Ok(())
    }
}

async fn delete_expired_shares(db: &SqlitePool) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM shares WHERE expires_at < CAST(strftime('%s', 'now') AS INTEGER)")
            .execute(db)
            .await?;

    Ok(result.rows_affected())
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use sqlx::sqlite::SqlitePoolOptions;

    use super::*;

    struct CountRuns(Arc<AtomicUsize>);

    impl Job for CountRuns {
        async fn run(&self) -> anyhow::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn runs_one_off_and_periodic_jobs_until_shutdown() {
        let shutdown = CancellationToken::new();
        let runner = JobRunner::new(TaskTracker::new(), shutdown.clone());
        let count = Arc::new(AtomicUsize::new(0));

        runner.spawn(CountRuns(count.clone())).await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);

        let periodic = runner.every(Duration::from_mins(1), CountRuns(count.clone()));
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);

        tokio::time::advance(Duration::from_mins(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 3);

        shutdown.cancel();
        periodic.await.unwrap();
    }

    #[tokio::test]
    async fn deletes_all_expired_shares_and_keeps_live_shares() -> anyhow::Result<()> {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        sqlx::query("CREATE TABLE shares (id INTEGER PRIMARY KEY, expires_at INTEGER)")
            .execute(&db)
            .await?;
        sqlx::query(
            "INSERT INTO shares (id, expires_at) VALUES \
             (1, CAST(strftime('%s', 'now') AS INTEGER) - 3600), \
             (2, CAST(strftime('%s', 'now') AS INTEGER) - 60), \
             (3, CAST(strftime('%s', 'now') AS INTEGER) + 3600), \
             (4, NULL)",
        )
        .execute(&db)
        .await?;

        assert_eq!(delete_expired_shares(&db).await?, 2);

        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM shares ORDER BY id")
            .fetch_all(&db)
            .await?;
        assert_eq!(ids, vec![3, 4]);
        Ok(())
    }
}
