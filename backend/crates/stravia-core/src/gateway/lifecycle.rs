use super::*;

pub(crate) struct GatewayLifecycle {
    pub(crate) cancellation: stravia_runtime_contract::CancellationToken,
    owners: AtomicUsize,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl GatewayLifecycle {
    pub(super) fn new() -> Self {
        Self {
            cancellation: stravia_runtime_contract::CancellationToken::new(),
            owners: AtomicUsize::new(1),
            tasks: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn add_owner(&self) {
        self.owners.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn release_owner(&self) -> bool {
        self.owners.fetch_sub(1, Ordering::AcqRel) == 1
    }

    pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        let handle = tokio::spawn(task);
        let mut tasks = self.tasks.lock();
        // 周期维护不断启动有限任务；完成的句柄不应随实例运行时间累积。
        tasks.retain_mut(|task| {
            if !task.is_finished() {
                return true;
            }
            match futures::FutureExt::now_or_never(task) {
                Some(Ok(())) => false,
                Some(Err(error)) => {
                    tracing::warn!(%error, "gateway background task failed");
                    false
                }
                None => true,
            }
        });
        tasks.push(handle);
    }

    pub(super) fn abort_tasks(&self) {
        self.cancellation.cancel();
        let tasks = self.tasks.lock();
        for task in tasks.iter() {
            task.abort();
        }
    }

    /// Request cooperative shutdown and join every tracked task. Started finite
    /// operations drain normally.
    pub(super) async fn shutdown(&self) {
        self.cancellation.cancel();
        loop {
            let tasks = {
                let mut tasks = self.tasks.lock();
                std::mem::take(&mut *tasks)
            };
            if tasks.is_empty() {
                break;
            }
            for task in tasks {
                if let Err(error) = task.await {
                    tracing::debug!(%error, "gateway task failed during shutdown");
                }
            }
        }
    }
}

impl Drop for GatewayLifecycle {
    fn drop(&mut self) {
        self.cancellation.cancel();
        let tasks = self.tasks.get_mut();
        for task in tasks.drain(..) {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ordinary_shutdown_finishes_started_task_before_returning() {
        let lifecycle = GatewayLifecycle::new();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (completed_tx, completed_rx) = tokio::sync::oneshot::channel();
        lifecycle.spawn(async move {
            entered_tx.send(()).expect("announce started operation");
            release_rx.await.expect("release started operation");
            completed_tx.send(()).expect("record completed operation");
        });
        entered_rx.await.expect("operation has started");
        let mut shutdown = Box::pin(lifecycle.shutdown());
        // Poll shutdown through cancellation and into joining the blocked task.
        // This is a controlled barrier, not a scheduler delay or timing assumption.
        std::future::poll_fn(|cx| {
            assert!(shutdown.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert!(lifecycle.cancellation.is_cancelled());
        release_tx
            .send(())
            .expect("ordinary shutdown preserves operation");
        shutdown.await;
        completed_rx
            .await
            .expect("operation completes before shutdown returns");
    }
}
