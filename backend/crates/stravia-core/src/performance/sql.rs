//! SQL 详情只接收执行适配器的来源和结果白名单，不读取 SQL 或错误正文。

use serde::Serialize;
use serde_json::Value;
use tracing::field::Field;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SpanKind {
    #[default]
    Operation,
    Query,
    Acquire,
}

impl SpanKind {
    pub(super) fn from_target(target: &str, name: &str) -> Option<Self> {
        match (target, name) {
            ("stravia::perf", _) => Some(Self::Operation),
            ("stravia::sql", "sql.query") => Some(Self::Query),
            ("stravia::sql", "sql.pool.acquire") => Some(Self::Acquire),
            _ => None,
        }
    }
}

#[derive(Clone, Default, Serialize)]
pub(super) struct SqlDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    query_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_identity: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_scope: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backend: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_scope: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_kind: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows_returned: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows_affected: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_idle: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_waiting: Option<u64>,
    #[serde(
        rename = "pool_acquire_failed",
        skip_serializing_if = "Option::is_none"
    )]
    pub pool_failed: Option<u64>,
    #[serde(
        rename = "pool_acquire_cancelled",
        skip_serializing_if = "Option::is_none"
    )]
    pub pool_cancelled: Option<u64>,
}

impl SqlDetails {
    pub(super) fn record_u64(&mut self, field: &Field, value: u64) {
        let destination = match field.name() {
            "rows_returned" => &mut self.rows_returned,
            "rows_affected" => &mut self.rows_affected,
            "pool_size" => &mut self.pool_size,
            "pool_idle" => &mut self.pool_idle,
            "pool_waiting" => &mut self.pool_waiting,
            "pool_acquire_failed" => &mut self.pool_failed,
            "pool_acquire_cancelled" => &mut self.pool_cancelled,
            _ => return,
        };
        *destination = Some(value);
    }

    pub(super) fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "query_id"
                if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
            {
                self.query_id = Some(value.into())
            }
            "query_identity" if value == "sha256_template" => {
                self.query_identity = Some("sha256_template")
            }
            "source_scope" if value == "unavailable" => self.source_scope = Some("unavailable"),
            "backend" => {
                self.backend = match value {
                    "SQLite" | "sqlite" => Some("sqlite"),
                    "PostgreSQL" | "postgres" => Some("postgres"),
                    _ => None,
                }
            }
            "pool" => {
                self.pool = match value {
                    "main" => Some("main"),
                    "artifact_lock" => Some("artifact_lock"),
                    "connection" | "unknown" | "unregistered" => Some("unregistered"),
                    _ => None,
                }
            }
            "execution_scope" if value == "driver" => self.execution_scope = Some("driver"),
            "error_kind" => {
                self.error_kind = Some(match value {
                    "database" => "database",
                    "pool_timeout" => "pool_timeout",
                    "pool_closed" => "pool_closed",
                    "io" => "io",
                    "tls" => "tls",
                    "protocol" => "protocol",
                    "decode" => "decode",
                    "encode" => "encode",
                    "row_not_found" => "row_not_found",
                    "column_not_found" => "column_not_found",
                    "type_not_found" => "type_not_found",
                    "configuration" => "configuration",
                    "cancelled" => "cancelled",
                    "worker_crashed" => "worker_crashed",
                    _ => "other",
                })
            }
            "error_code"
                if (!value.is_empty()
                    && value.len() <= 10
                    && value.bytes().all(|byte| byte.is_ascii_digit())
                    && value.parse::<u32>().is_ok())
                    || (value.len() == 5
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())) =>
            {
                self.error_code = Some(value.into());
            }
            _ => {}
        }
    }

    pub(super) fn merge(&mut self, other: Self) {
        macro_rules! merge { ($($field:ident),* $(,)?) => { $(if other.$field.is_some() { self.$field = other.$field; })* }; }
        merge!(
            query_id,
            query_identity,
            source_scope,
            backend,
            pool,
            execution_scope,
            error_kind,
            error_code,
            rows_returned,
            rows_affected,
            pool_size,
            pool_idle,
            pool_waiting,
            pool_failed,
            pool_cancelled
        );
    }

    pub(super) fn export(&self) -> Value {
        serde_json::to_value(self).expect("SQL metadata contains serializable scalars")
    }
}

#[derive(Clone, Serialize)]
pub(super) struct PoolPoint {
    pub backend: &'static str,
    pub pool: &'static str,
    pub size: Option<u64>,
    pub idle: Option<u64>,
    pub waiting: Option<u64>,
    pub failed: Option<u64>,
    pub cancelled: Option<u64>,
    pub observed_at_us: u64,
    pub scope: &'static str,
}

impl SqlDetails {
    pub(super) fn pool_point(&self, observed_at_us: u64) -> Option<PoolPoint> {
        if self.pool_size.is_none()
            && self.pool_idle.is_none()
            && self.pool_waiting.is_none()
            && self.pool_failed.is_none()
            && self.pool_cancelled.is_none()
        {
            return None;
        }
        Some(PoolPoint {
            backend: self.backend?,
            pool: self.pool?,
            size: self.pool_size,
            idle: self.pool_idle,
            waiting: self.pool_waiting,
            failed: self.pool_failed,
            cancelled: self.pool_cancelled,
            observed_at_us,
            scope: "size/idle: sampled pool; waiting/failures/cancellations: instrumented backend-role totals; driver-internal waits excluded",
        })
    }
}
