use std::ops::{Deref, DerefMut};

use sqlx_native::Connection as _;
use tokio::{runtime::Handle, sync::OwnedMutexGuard};

use crate::{ConnectionHandle, Database, Error, PoolConnection};

/// 持有连接及其 SQLite 写协调权。事务必须借用此连接，不能另从池中取连接。
#[derive(Debug)]
pub struct WriteConnection<DB: Database> {
    connection: Option<PoolConnection<DB>>,
    guard: Option<OwnedMutexGuard<()>>,
    runtime: Option<Handle>,
    discard_on_drop: bool,
}

impl<DB: Database> WriteConnection<DB> {
    pub(super) fn new(connection: PoolConnection<DB>, guard: Option<OwnedMutexGuard<()>>) -> Self {
        Self {
            connection: Some(connection),
            runtime: guard.as_ref().map(|_| Handle::current()),
            guard,
            discard_on_drop: false,
        }
    }

    /// 等候驱动队列中的语句及回滚完成，再归还连接并释放协调权。
    ///
    /// SQLite 的查询 future 被取消后，worker 仍可能执行语句；仅丢弃 MutexGuard
    /// 不能说明数据库写锁已释放。ping 是同一 worker 队列中的完成屏障。
    /// 屏障失败或事务仍活跃时返回错误，后台关闭任务继续持有协调权直至关闭完成。
    pub async fn finish(mut self) -> Result<(), Error> {
        if self.guard.is_some() {
            let connection = self.connection.as_mut().expect("live write connection");
            if let Err(error) = settle(connection).await {
                self.discard_on_drop = true;
                return Err(error);
            }
        }
        drop(self.connection.take());
        drop(self.guard.take());
        Ok(())
    }
}

impl<DB: Database> Deref for WriteConnection<DB> {
    type Target = ConnectionHandle<'static, DB>;

    fn deref(&self) -> &Self::Target {
        self.connection.as_ref().expect("live write connection")
    }
}

impl<DB: Database> DerefMut for WriteConnection<DB> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection.as_mut().expect("live write connection")
    }
}

impl<DB: Database> Drop for WriteConnection<DB> {
    fn drop(&mut self) {
        let Some(mut connection) = self.connection.take() else {
            return;
        };
        let Some(guard) = self.guard.take() else {
            return;
        };
        let runtime = self.runtime.take().expect("SQLite writer runtime");
        let discard = self.discard_on_drop;
        // 只在取消或错误退出时分离清理任务；正常 finish 不分配额外任务。
        // 事务的 Drop 已先向同一 worker 排入回滚，屏障完成前不能放行后续写入。
        let _cleanup = runtime.spawn(async move {
            let error = if discard {
                None
            } else {
                settle(&mut connection).await.err()
            };
            if let Some(error) = &error {
                tracing::warn!(
                    error_kind = crate::diagnostics::error_kind(error),
                    "SQLite writer cleanup failed"
                );
            }
            if discard || error.is_some() {
                // SQLx 不向已丢弃的事务报告后台 ROLLBACK 错误，必须确认已离开事务。
                // 无法确认时关闭连接，并在 worker 确认关闭后才释放协调权。
                if let Err(error) = connection.handle.close().await {
                    tracing::warn!(
                        error_kind = crate::diagnostics::error_kind(&error),
                        "SQLite writer connection close failed"
                    );
                }
            } else {
                drop(connection);
            }
            drop(guard);
        });
    }
}

async fn settle<DB: Database>(connection: &mut PoolConnection<DB>) -> Result<(), Error> {
    connection.ping().await?;
    if connection.handle.raw().is_in_transaction() {
        return Err(Error::Protocol(
            "SQLite write lease ended with an active transaction".into(),
        ));
    }
    Ok(())
}
