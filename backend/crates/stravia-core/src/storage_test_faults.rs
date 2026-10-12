//! Isolated host-test faults at real persistence boundaries.
use std::sync::LazyLock;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FaultKind {
    IoFull,
    OrdinaryIo,
    PostgresFull,
    SqliteFull,
}
struct BoundaryFault {
    boundary: String,
    kind: FaultKind,
    arm_file: std::path::PathBuf,
}
impl BoundaryFault {
    fn from_environment() -> Option<Self> {
        let boundary = std::env::var("STRAVIA_TEST_STORAGE_FAULT").ok()?;
        if !matches!(boundary.as_str(), "observation" | "debug" | "artifact") {
            return None;
        }
        let kind = match std::env::var("STRAVIA_TEST_STORAGE_FAULT_KIND")
            .ok()?
            .as_str()
        {
            "io-full" => FaultKind::IoFull,
            "ordinary-io" => FaultKind::OrdinaryIo,
            "postgres-full" if boundary == "observation" => FaultKind::PostgresFull,
            "sqlite-full" if boundary == "observation" => FaultKind::SqliteFull,
            _ => return None,
        };
        Some(Self {
            boundary,
            kind,
            arm_file: std::env::var_os("STRAVIA_TEST_STORAGE_FAULT_ARM_FILE")?.into(),
        })
    }
    fn consume(&self, boundary: &str, kind: FaultKind) -> std::io::Result<bool> {
        if self.kind != kind || self.boundary != boundary {
            return Ok(false);
        }
        match std::fs::remove_file(&self.arm_file) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}
static FAULT: LazyLock<Option<BoundaryFault>> = LazyLock::new(BoundaryFault::from_environment);
fn configured_fault() -> Option<&'static BoundaryFault> {
    FAULT.as_ref()
}

/// Consumed once after the isolated host harness arms the selected boundary.
pub(crate) fn inject_io(boundary: &str) -> std::io::Result<()> {
    let Some(fault) = configured_fault() else {
        return Ok(());
    };
    if !matches!(fault.kind, FaultKind::IoFull | FaultKind::OrdinaryIo)
        || !fault.consume(boundary, fault.kind)?
    {
        return Ok(());
    }
    let error = if fault.kind == FaultKind::OrdinaryIo {
        std::io::Error::from(std::io::ErrorKind::PermissionDenied)
    } else {
        #[cfg(windows)]
        let code = 112;
        #[cfg(not(windows))]
        let code = 28;
        std::io::Error::from_raw_os_error(code)
    };
    Err(error)
}

/// Obtain the PostgreSQL driver's real typed 53100 error, without exhausting disk.
pub(crate) async fn inject_postgres(
    pool: &sqlx::PgPool,
    boundary: &str,
) -> Result<(), sqlx::Error> {
    let Some(fault) = configured_fault() else {
        return Ok(());
    };
    if !fault
        .consume(boundary, FaultKind::PostgresFull)
        .map_err(sqlx::Error::Io)?
    {
        return Ok(());
    }
    sqlx::query("DO $$ BEGIN RAISE EXCEPTION USING ERRCODE = '53100', MESSAGE = 'isolated typed capacity fault'; END $$")
        .execute(pool).await.map(|_| ())
}

/// Produce real SQLITE_FULL in a bounded in-memory database, not the runtime pool.
pub(crate) async fn inject_sqlite(boundary: &str) -> Result<(), sqlx::Error> {
    use sqlx::Connection;
    let Some(fault) = configured_fault() else {
        return Ok(());
    };
    if !fault
        .consume(boundary, FaultKind::SqliteFull)
        .map_err(sqlx::Error::Io)?
    {
        return Ok(());
    }
    let mut connection = sqlx::SqliteConnection::connect("sqlite::memory:").await?;
    let result = async {
        sqlx::query("CREATE TABLE capacity_probe (bytes BLOB)")
            .execute(&mut connection)
            .await?;
        let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
            .fetch_one(&mut connection)
            .await?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "PRAGMA max_page_count = {pages}"
        )))
        .execute(&mut connection)
        .await?;
        sqlx::query("INSERT INTO capacity_probe VALUES (zeroblob(1048576))")
            .execute(&mut connection)
            .await
            .map(|_| ())
    }
    .await;
    let closed = connection.close().await;
    result?;
    closed
}
