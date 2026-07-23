use std::future::Future;
use std::sync::Arc;

use tokio::sync::Semaphore;

/// Limits how many SSH operations run at once, so a large fleet doesn't overwhelm the local
/// machine (open file descriptors, memory) or the remote hosts with a connection stampede.
pub struct SshPool {
    max_concurrent: usize,
    semaphore: Arc<Semaphore>,
}

impl SshPool {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            max_concurrent,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
        }
    }

    pub fn max_concurrent(&self) -> usize {
        self.max_concurrent
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    /// Runs every operation, but never more than `max_concurrent` at a time. Results are
    /// returned in the same order as `operations`.
    pub async fn execute_concurrent<T, F, Fut>(&self, operations: Vec<F>) -> Vec<T>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let handles: Vec<_> = operations
            .into_iter()
            .map(|op| {
                let semaphore = Arc::clone(&self.semaphore);
                tokio::spawn(async move {
                    let _permit = semaphore
                        .acquire_owned()
                        .await
                        .expect("SshPool semaphore should never be closed");
                    op().await
                })
            })
            .collect();

        let mut results = Vec::with_capacity(handles.len());
        for handle in handles {
            results.push(handle.await.expect("ssh pool task panicked"));
        }
        results
    }

    /// Runs operations in sequential batches of `batch_size` (defaults to `max_concurrent`).
    /// Every operation in a batch runs concurrently; the next batch doesn't start until the
    /// current one finishes.
    pub async fn execute_batched<T, F, Fut>(
        &self,
        operations: Vec<F>,
        batch_size: Option<usize>,
    ) -> Vec<T>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let size = batch_size.unwrap_or(self.max_concurrent).max(1);
        let mut results = Vec::with_capacity(operations.len());
        let mut iter = operations.into_iter();

        loop {
            let batch: Vec<F> = iter.by_ref().take(size).collect();
            if batch.is_empty() {
                break;
            }

            let handles: Vec<_> = batch.into_iter().map(|op| tokio::spawn(op())).collect();
            for handle in handles {
                results.push(handle.await.expect("ssh pool task panicked"));
            }
        }

        results
    }

    /// Like `execute_concurrent`, but partitions `Ok`/`Err` outcomes instead of stopping at the
    /// first error.
    pub async fn execute_with_error_collection<T, E, F, Fut>(
        &self,
        operations: Vec<F>,
    ) -> (Vec<T>, Vec<E>)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static,
    {
        let outcomes = self.execute_concurrent(operations).await;
        let mut results = Vec::new();
        let mut errors = Vec::new();
        for outcome in outcomes {
            match outcome {
                Ok(value) => results.push(value),
                Err(err) => errors.push(err),
            }
        }
        (results, errors)
    }
}
