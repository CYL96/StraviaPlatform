use std::time::Duration;

use stravia_sql::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

async fn isolated_pool(directory: &std::path::Path) -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(3)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(directory.join("coordination.db"))
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_millis(25)),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn queued_writes_outlive_sqlite_busy_timeout_without_blocking_readers() {
    let directory = tempfile::tempdir().unwrap();
    let pool = isolated_pool(directory.path()).await;
    stravia_sql::query("CREATE TABLE counter (value INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    stravia_sql::query("INSERT INTO counter VALUES (0)")
        .execute(&pool)
        .await
        .unwrap();

    let mut first = pool.acquire_write().await.unwrap();
    let mut transaction = first.begin_with("BEGIN IMMEDIATE").await.unwrap();
    stravia_sql::query("UPDATE counter SET value = value + 1")
        .execute(&mut *transaction)
        .await
        .unwrap();

    let second_pool = pool.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let mut second = tokio::spawn(async move {
        started.send(()).unwrap();
        let mut connection = second_pool.acquire_write().await?;
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
        stravia_sql::query("UPDATE counter SET value = value + 1")
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        connection.finish().await
    });
    ready.await.unwrap();

    let visible: i64 = stravia_sql::query_scalar("SELECT value FROM counter")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        visible, 0,
        "WAL readers must retain their committed snapshot"
    );
    // 故意让首笔写入超过隔离测试库的 busy_timeout；协调不能依赖 SQLite 重试。
    let early = tokio::time::timeout(Duration::from_millis(250), &mut second).await;
    transaction.commit().await.unwrap();
    first.finish().await.unwrap();
    match early {
        Ok(result) => result.unwrap(),
        Err(_) => second.await.unwrap(),
    }
    .expect("both coordinated writes must commit instead of failing SQLITE_BUSY");

    let committed: i64 = stravia_sql::query_scalar("SELECT value FROM counter")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(committed, 2);
    pool.close().await;
}

#[tokio::test]
async fn cancelled_write_holds_admission_until_worker_rollback_finishes() {
    let directory = tempfile::tempdir().unwrap();
    let pool = isolated_pool(directory.path()).await;
    stravia_sql::query("CREATE TABLE counter (value INTEGER NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    stravia_sql::query("INSERT INTO counter VALUES (0)")
        .execute(&pool)
        .await
        .unwrap();

    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let waiting = entered.notified();
    let worker_entered = entered.clone();
    let (release, worker_release) = std::sync::mpsc::channel();
    let first_pool = pool.clone();
    let first = tokio::spawn(async move {
        let mut connection = first_pool.acquire_write().await.unwrap();
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await.unwrap();
        {
            let mut handle = transaction.lock_handle().await.unwrap();
            let mut release = Some(worker_release);
            handle.set_progress_handler(1, move || {
                if let Some(release) = release.take() {
                    worker_entered.notify_one();
                    // 在真实 SQLite worker 内停住语句；发送端释放或销毁都能解除。
                    let _released = release.recv();
                }
                true
            });
        }
        stravia_sql::query("UPDATE counter SET value = value + 100")
            .execute(&mut *transaction)
            .await
            .unwrap();
        transaction.commit().await.unwrap();
        connection.finish().await.unwrap();
    });
    tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("first write must enter the SQLite worker");
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());

    let next_pool = pool.clone();
    let mut next = tokio::spawn(async move {
        let mut connection = next_pool.acquire_write().await?;
        let mut transaction = connection.begin_with("BEGIN IMMEDIATE").await?;
        stravia_sql::query("UPDATE counter SET value = value + 1")
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        connection.finish().await
    });
    let early = tokio::time::timeout(Duration::from_millis(250), &mut next).await;
    release.send(()).unwrap();
    match early {
        Ok(result) => result.unwrap(),
        Err(_) => next.await.unwrap(),
    }
    .expect("the next write must wait for cancelled worker cleanup, not fail SQLITE_BUSY");

    let committed: i64 = stravia_sql::query_scalar("SELECT value FROM counter")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        committed, 1,
        "cancelled transaction must roll back before its successor"
    );
    pool.close().await;
}
