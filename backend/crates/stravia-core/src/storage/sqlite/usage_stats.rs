use crate::{
    db::models::{StatsQueryError, StatsWindow},
    storage::RouteSchedulingUsage,
};

use super::*;

#[derive(Clone)]
pub(super) struct SqliteUsageStatsStore {
    pub(super) pool: SqlitePool,
    pub(super) last_route_snapshot:
        Arc<parking_lot::RwLock<Vec<crate::router::TargetSchedulingSnapshot>>>,
}

const BASE_BUCKET_MS: i64 = 900_000;

// Display buckets start at the selected boundary and retain the final partial bucket.
fn display_bucket_ms(start: i64, end: i64, requested: i64) -> i64 {
    let span = i128::from(end) - i128::from(start);
    let minimum = ((span + 511) / 512).max(i128::from(requested));
    (((minimum + i128::from(BASE_BUCKET_MS) - 1) / i128::from(BASE_BUCKET_MS))
        * i128::from(BASE_BUCKET_MS)) as i64
}

#[async_trait]
impl UsageStatsStore for SqliteUsageStatsStore {
    async fn route_scheduling_snapshot(&self) -> RouteSchedulingUsage {
        let now = chrono::Utc::now().timestamp_millis();
        let hour_ago = now.saturating_sub(60 * 60 * 1_000);
        let day_ago = now.saturating_sub(24 * 60 * 60 * 1_000);
        let result = sqlx::query_as::<_, crate::router::TargetSchedulingSnapshot>(
            "SELECT provider_id || ':' || upstream_model AS target_key,
        CASE WHEN COUNT(*) > 0 AND COUNT(input_tokens) = COUNT(*) THEN SUM(input_tokens) END AS input_tokens_24h,
        CASE WHEN COUNT(*) > 0 AND COUNT(output_tokens) = COUNT(*) THEN SUM(output_tokens) END AS output_tokens_24h,
        CASE WHEN COUNT(*) > 0 AND COUNT(cache_read_tokens) = COUNT(*) THEN SUM(cache_read_tokens) END AS cache_read_tokens_24h,
        CASE WHEN COUNT(*) > 0 AND COUNT(cache_write_tokens) = COUNT(*) THEN SUM(cache_write_tokens) END AS cache_write_tokens_24h,
        SUM(CASE WHEN started_at >= ? THEN 1 ELSE 0 END) AS attempts_1h,
        SUM(CASE WHEN started_at >= ? AND status = 'completed' THEN 1 ELSE 0 END) AS successes_1h,
        CASE WHEN SUM(CASE WHEN started_at >= ? AND status = 'completed' THEN 1 ELSE 0 END) > 0
                   AND SUM(CASE WHEN started_at >= ? AND status = 'completed' AND output_tokens IS NULL THEN 1 ELSE 0 END) = 0
             THEN SUM(CASE WHEN started_at >= ? AND status = 'completed' THEN output_tokens END) END AS successful_output_tokens_1h,
        CASE WHEN SUM(CASE WHEN started_at >= ? AND status = 'completed' THEN 1 ELSE 0 END) > 0
                   AND SUM(CASE WHEN started_at >= ? AND status = 'completed' AND duration_ms IS NULL THEN 1 ELSE 0 END) = 0
             THEN SUM(CASE WHEN started_at >= ? AND status = 'completed' THEN duration_ms END) END AS successful_upstream_ms_1h,
        NULL AS cost_input, NULL AS cost_output,
        NULL AS cost_cache_read, NULL AS cost_cache_write
             FROM target_attempt_observations
             WHERE started_at >= ?
               AND provider_id <> '' AND upstream_model <> ''
             GROUP BY provider_id, upstream_model",
        )
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(hour_ago)
        .bind(day_ago)
        .fetch_all(&self.pool)
        .await;
        match result {
            Ok(targets) => {
                *self.last_route_snapshot.write() = targets.clone();
                RouteSchedulingUsage {
                    targets,
                    stale: false,
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to refresh confirmed route scheduling usage");
                let targets = self.last_route_snapshot.read().clone();
                RouteSchedulingUsage {
                    targets,
                    stale: true,
                }
            }
        }
    }

    async fn stats_overview(&self, window: StatsWindow) -> anyhow::Result<StatsOverview> {
        window.validate()?;
        Ok(sqlx::query_as::<_, StatsOverview>(
            "SELECT COALESCE(SUM(request_count), 0) AS total_requests,
       COALESCE(SUM(error_count), 0) AS error_count,
       CASE WHEN SUM(input_tokens_known) > 0 THEN SUM(input_tokens_sum) END AS total_input_tokens,
       CASE WHEN SUM(output_tokens_known) > 0 THEN SUM(output_tokens_sum) END AS total_output_tokens,
       CASE WHEN SUM(cache_read_tokens_known) > 0 THEN SUM(cache_read_tokens_sum) END AS total_cache_read_tokens,
       CASE WHEN SUM(cache_write_tokens_known) > 0 THEN SUM(cache_write_tokens_sum) END AS total_cache_write_tokens,
       CASE WHEN SUM(reasoning_tokens_known) > 0 THEN SUM(reasoning_tokens_sum) END AS total_reasoning_tokens,
       SUM(duration_ms_sum) * 1.0 / NULLIF(SUM(duration_ms_count), 0) AS avg_duration_ms,
       SUM(first_token_ms_sum) * 1.0 / NULLIF(SUM(first_token_ms_count), 0) AS avg_first_token_ms,
       CASE WHEN SUM(successful_attempt_count) > 0 AND SUM(successful_output_known_count) = SUM(successful_attempt_count) AND SUM(successful_duration_known_count) = SUM(successful_attempt_count) AND SUM(successful_duration_sum) > 0 THEN SUM(successful_output_sum) * 1000.0 / SUM(successful_duration_sum) END AS avg_output_tps FROM stats_usage_buckets WHERE scope = 'global' AND bucket_start >= COALESCE(?1, -9223372036854775808) AND bucket_start < COALESCE(?2, 9223372036854775807)",
        )
        .bind(window.start_at)
        .bind(window.end_at)
        .fetch_one(&self.pool)
        .await?)
    }

    async fn stats_series(
        &self,
        window: StatsWindow,
        bucket_ms: i64,
    ) -> anyhow::Result<Vec<StatsSeries>> {
        window.validate()?;
        if bucket_ms < BASE_BUCKET_MS || bucket_ms % BASE_BUCKET_MS != 0 {
            return Err(
                StatsQueryError("bucket must be a positive multiple of 900 seconds").into(),
            );
        }
        let (start, end) = match (window.start_at, window.end_at) {
            (Some(start), Some(end)) => (start, end),
            _ => {
                let (start, end): (Option<i64>, Option<i64>) = sqlx::query_as(
                            "SELECT MIN(bucket_start), MAX(bucket_start) FROM stats_usage_buckets WHERE scope = 'global'",
                        ).fetch_one(&self.pool).await?;
                let (Some(start), Some(end)) = (start, end) else {
                    return Ok(Vec::new());
                };
                (start, end.saturating_add(BASE_BUCKET_MS))
            }
        };
        let bucket_ms = display_bucket_ms(start, end, bucket_ms);
        Ok(sqlx::query_as::<_, StatsSeries>(
            "SELECT (bucket_start - ?4) / ?3 * ?3 + ?4 AS bucket_start, ?3 AS bucket_ms, COALESCE(SUM(request_count), 0) AS request_count,
       COALESCE(SUM(error_count), 0) AS error_count,
       CASE WHEN SUM(input_tokens_known) > 0 THEN SUM(input_tokens_sum) END AS total_input_tokens,
       CASE WHEN SUM(output_tokens_known) > 0 THEN SUM(output_tokens_sum) END AS total_output_tokens,
       CASE WHEN SUM(cache_read_tokens_known) > 0 THEN SUM(cache_read_tokens_sum) END AS total_cache_read_tokens,
       CASE WHEN SUM(cache_write_tokens_known) > 0 THEN SUM(cache_write_tokens_sum) END AS total_cache_write_tokens,
       CASE WHEN SUM(reasoning_tokens_known) > 0 THEN SUM(reasoning_tokens_sum) END AS total_reasoning_tokens,
       SUM(duration_ms_sum) * 1.0 / NULLIF(SUM(duration_ms_count), 0) AS avg_duration_ms,
       SUM(first_token_ms_sum) * 1.0 / NULLIF(SUM(first_token_ms_count), 0) AS avg_first_token_ms,
       CASE WHEN SUM(successful_attempt_count) > 0 AND SUM(successful_output_known_count) = SUM(successful_attempt_count) AND SUM(successful_duration_known_count) = SUM(successful_attempt_count) AND SUM(successful_duration_sum) > 0 THEN SUM(successful_output_sum) * 1000.0 / SUM(successful_duration_sum) END AS avg_output_tps FROM stats_usage_buckets WHERE scope = 'global' AND bucket_start >= COALESCE(?1, -9223372036854775808) AND bucket_start < COALESCE(?2, 9223372036854775807) GROUP BY (bucket_start - ?4) / ?3 * ?3 + ?4 ORDER BY bucket_start",
        )
        .bind(start)
        .bind(end)
        .bind(bucket_ms)
        .bind(start)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn stats_by_model(&self, window: StatsWindow) -> anyhow::Result<Vec<ModelStats>> {
        window.validate()?;
        Ok(sqlx::query_as::<_, ModelStats>(
            "SELECT entity_id AS model_id, MAX(label) AS model, NOT EXISTS (SELECT 1 FROM models live WHERE live.id = b.entity_id) AS deleted, COALESCE(SUM(request_count), 0) AS request_count,
       CASE WHEN SUM(input_tokens_known) > 0 THEN SUM(input_tokens_sum) END AS total_input_tokens,
       CASE WHEN SUM(output_tokens_known) > 0 THEN SUM(output_tokens_sum) END AS total_output_tokens,
       CASE WHEN SUM(reasoning_tokens_known) > 0 THEN SUM(reasoning_tokens_sum) END AS total_reasoning_tokens,
       SUM(duration_ms_sum) * 1.0 / NULLIF(SUM(duration_ms_count), 0) AS avg_duration_ms FROM stats_usage_buckets b WHERE scope = 'model' AND bucket_start >= COALESCE(?1, -9223372036854775808) AND bucket_start < COALESCE(?2, 9223372036854775807) GROUP BY identity, entity_id ORDER BY request_count DESC, identity",
        )
        .bind(window.start_at)
        .bind(window.end_at)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn stats_by_provider(&self, window: StatsWindow) -> anyhow::Result<Vec<ProviderStats>> {
        window.validate()?;
        Ok(sqlx::query_as::<_, ProviderStats>(
            "SELECT entity_id AS provider_id, MAX(label) AS provider, NOT EXISTS (SELECT 1 FROM providers live WHERE live.id = b.entity_id) AS deleted, COALESCE(SUM(request_count), 0) AS request_count,
       COALESCE(SUM(error_count), 0) AS error_count,
       SUM(duration_ms_sum) * 1.0 / NULLIF(SUM(duration_ms_count), 0) AS avg_duration_ms,
       CASE WHEN SUM(successful_attempt_count) > 0 AND SUM(successful_output_known_count) = SUM(successful_attempt_count) AND SUM(successful_duration_known_count) = SUM(successful_attempt_count) AND SUM(successful_duration_sum) > 0 THEN SUM(successful_output_sum) * 1000.0 / SUM(successful_duration_sum) END AS avg_output_tps FROM stats_usage_buckets b WHERE scope = 'provider' AND bucket_start >= COALESCE(?1, -9223372036854775808) AND bucket_start < COALESCE(?2, 9223372036854775807) GROUP BY identity, entity_id ORDER BY request_count DESC, identity",
        )
        .bind(window.start_at)
        .bind(window.end_at)
        .fetch_all(&self.pool)
        .await?)
    }

    async fn stats_by_api_key(&self, window: StatsWindow) -> anyhow::Result<Vec<ApiKeyStats>> {
        window.validate()?;
        Ok(sqlx::query_as::<_, ApiKeyStats>(
            "SELECT COALESCE(entity_id, '') AS api_key_id, MAX(label) AS api_key_name, NOT EXISTS (SELECT 1 FROM api_keys live WHERE live.id = b.entity_id) AS deleted, COALESCE(SUM(request_count), 0) AS request_count,
       CASE WHEN SUM(input_tokens_known) > 0 THEN SUM(input_tokens_sum) END AS total_input_tokens,
       CASE WHEN SUM(output_tokens_known) > 0 THEN SUM(output_tokens_sum) END AS total_output_tokens,
       CASE WHEN SUM(cache_read_tokens_known) > 0 THEN SUM(cache_read_tokens_sum) END AS cache_read_tokens,
       CASE WHEN SUM(cache_write_tokens_known) > 0 THEN SUM(cache_write_tokens_sum) END AS cache_write_tokens,
       CASE WHEN SUM(reasoning_tokens_known) > 0 THEN SUM(reasoning_tokens_sum) END AS reasoning_tokens,
       MAX(last_used_at) AS last_used_at FROM stats_usage_buckets b WHERE scope = 'api_key' AND bucket_start >= COALESCE(?1, -9223372036854775808) AND bucket_start < COALESCE(?2, 9223372036854775807) GROUP BY identity, entity_id ORDER BY request_count DESC, identity",
        )
        .bind(window.start_at)
        .bind(window.end_at)
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(hours: i64) -> StatsWindow {
        let end = chrono::Utc::now().timestamp_millis() / BASE_BUCKET_MS * BASE_BUCKET_MS
            + BASE_BUCKET_MS;
        StatsWindow {
            start_at: Some(end - hours * 3_600_000),
            end_at: Some(end),
        }
    }

    async fn insert_turn(
        pool: &SqlitePool,
        id: &str,
        started_at: i64,
        model: &str,
        api_key_id: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO interaction_observations
             (id,principal,api_key_id,api_key_name,root_id,root_run_id,first_route_id,status,started_at,last_active_at,expires_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(id)
        .bind("owner")
        .bind(api_key_id)
        .bind("Test key")
        .bind(id)
        .bind(id)
        .bind("route")
        .bind("completed")
        .bind(started_at)
        .bind(started_at)
        .bind(i64::MAX)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO inference_run_observations
             (id,interaction_id,ingress_protocol,route_id,status,debug_enabled,started_at,last_active_at,finished_at,last_event_sequence,expires_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(id)
        .bind("responses")
        .bind("route")
        .bind("completed")
        .bind(false)
        .bind(started_at)
        .bind(started_at)
        .bind(started_at + 10)
        .bind(0_i64)
        .bind(i64::MAX)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO model_turn_observations
             (id,run_id,interaction_id,route_id,model_display_name,api_key_id,api_key_name,status,started_at,finished_at,last_event_sequence)
             VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .bind(model)
        .bind(model)
        .bind(api_key_id)
        .bind("Test key")
        .bind("completed")
        .bind(started_at)
        .bind(started_at + 10)
        .bind(0_i64)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn insert_attempt(
        pool: &SqlitePool,
        id: &str,
        turn_id: &str,
        started_at: i64,
        input_tokens: i64,
        cache_read_tokens: Option<i64>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO target_attempt_observations
             (id,model_turn_id,run_id,interaction_id,target_id,provider_id,provider_name,upstream_model,protocol,status,started_at,finished_at,duration_ms,input_tokens,output_tokens,cache_read_tokens,reasoning_tokens,usage_recorded,last_event_sequence)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(turn_id)
        .bind(turn_id)
        .bind(turn_id)
        .bind("target")
        .bind("provider")
        .bind("Provider")
        .bind("upstream")
        .bind("responses")
        .bind("completed")
        .bind(started_at)
        .bind(started_at + 10)
        .bind(10_i64)
        .bind(input_tokens)
        .bind(3_i64)
        .bind(cache_read_tokens)
        .bind(1_i64)
        .bind(true)
        .bind(0_i64)
        .execute(pool)
        .await?;
        Ok(())
    }

    async fn insert_failed_attempt(
        pool: &SqlitePool,
        id: &str,
        turn_id: &str,
        started_at: i64,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO target_attempt_observations
             (id,model_turn_id,run_id,interaction_id,target_id,provider_id,provider_name,upstream_model,protocol,status,started_at,finished_at,duration_ms,last_event_sequence)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        )
        .bind(id)
        .bind(turn_id)
        .bind(turn_id)
        .bind(turn_id)
        .bind("target")
        .bind("provider")
        .bind("Provider")
        .bind("upstream")
        .bind("responses")
        .bind("failed")
        .bind(started_at)
        .bind(started_at + 10)
        .bind(10_i64)
        .bind(0_i64)
        .execute(pool)
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn management_errors_count_only_final_failed_requests() -> anyhow::Result<()> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::migrations::migrate_sqlite(&pool, None).await?;
        let now = chrono::Utc::now().timestamp_millis();
        let recent = now - 1_000;
        for (id, run_status, turn_status, reason, finished, expires_at) in [
            ("recovered", "completed", "completed", None, true, i64::MAX),
            ("running", "running", "running", None, false, i64::MAX),
            (
                "cancelled",
                "cancelled",
                "cancelled",
                Some("cancelled"),
                true,
                i64::MAX,
            ),
            (
                "interrupted",
                "interrupted",
                "interrupted",
                Some("process_restarted"),
                true,
                i64::MAX,
            ),
            (
                "disconnected",
                "failed",
                "failed",
                Some("client_disconnected"),
                true,
                i64::MAX,
            ),
            (
                "unfinished",
                "failed",
                "failed",
                Some("upstream_error"),
                false,
                i64::MAX,
            ),
            (
                "expired",
                "failed",
                "failed",
                Some("upstream_error"),
                true,
                now - 1,
            ),
            (
                "upstream-error",
                "failed",
                "failed",
                Some("upstream_error"),
                true,
                i64::MAX,
            ),
            (
                "delivery-error",
                "failed",
                "completed",
                Some("delivery_error"),
                true,
                i64::MAX,
            ),
        ] {
            insert_turn(&pool, id, recent, "model", "key").await?;
            sqlx::query("UPDATE inference_run_observations SET status=?,terminal_reason=?,finished_at=?,expires_at=? WHERE id=?")
                .bind(run_status)
                .bind(reason)
                .bind(finished.then_some(recent + 10))
                .bind(expires_at)
                .bind(id)
                .execute(&pool).await?;
            sqlx::query("UPDATE model_turn_observations SET status=? WHERE id=?")
                .bind(turn_status)
                .bind(id)
                .execute(&pool)
                .await?;
            insert_failed_attempt(&pool, &format!("{id}-failed"), id, recent).await?;
        }
        insert_attempt(&pool, "recovered-success", "recovered", recent, 12, Some(5)).await?;
        insert_attempt(
            &pool,
            "delivery-success",
            "delivery-error",
            recent,
            12,
            Some(5),
        )
        .await?;
        // 同一请求的多次尝试和隐藏轮次不能重复计错。
        insert_failed_attempt(&pool, "upstream-error-retry", "upstream-error", recent).await?;
        sqlx::query(
            "INSERT INTO model_turn_observations
            (id,run_id,interaction_id,route_id,status,started_at,finished_at,last_event_sequence)
            VALUES ('hidden','upstream-error','upstream-error','route','failed',?,?,0)",
        )
        .bind(recent)
        .bind(recent + 10)
        .execute(&pool)
        .await?;
        insert_turn(&pool, "old-error", now - 7_200_000, "model", "key").await?;
        sqlx::query("UPDATE inference_run_observations SET status='failed',terminal_reason='upstream_error' WHERE id='old-error'")
            .execute(&pool).await?;
        insert_failed_attempt(&pool, "old-failed", "old-error", now - 7_200_000).await?;
        // 准入前拒绝也属于失败请求；单纯取消和 499 不属于。
        for (id, code, status_code) in [
            ("rejected", "invalid_api_key", 401),
            ("rejected-cancelled", "cancelled", 400),
            ("rejected-disconnected", "client_disconnected", 499),
        ] {
            sqlx::query("INSERT INTO rejected_request_observations
                (id,occurred_at,method,path,ingress_protocol,stage,code,status_code,debug_enabled,last_event_sequence,expires_at)
                VALUES (?,?,'POST','/v1/responses','responses','authentication',?,?,0,0,?)")
                .bind(id).bind(now - 1_800_000).bind(code).bind(status_code).bind(i64::MAX)
                .execute(&pool).await?;
        }
        let store = SqliteUsageStatsStore {
            pool: pool.clone(),
            last_route_snapshot: Arc::new(parking_lot::RwLock::new(Vec::new())),
        };
        assert_eq!(store.stats_overview(window(1)).await?.error_count, 4);
        assert_eq!(store.stats_overview(window(1)).await?.total_requests, 12);
        assert_eq!(
            store
                .stats_overview(StatsWindow::default())
                .await?
                .error_count,
            5
        );
        assert_eq!(
            store
                .stats_overview(StatsWindow::default())
                .await?
                .total_requests,
            13
        );
        let series = store.stats_series(window(1), 900_000).await?;
        assert_eq!(
            series.iter().map(|bucket| bucket.error_count).sum::<i64>(),
            4
        );
        assert_eq!(
            series
                .iter()
                .map(|bucket| bucket.request_count)
                .sum::<i64>(),
            12
        );
        assert_eq!(
            series
                .iter()
                .find(|bucket| bucket.bucket_start == (now - 1_800_000) / 900_000 * 900_000)
                .unwrap()
                .error_count,
            1
        );
        let providers = store.stats_by_provider(window(1)).await?;
        assert_eq!(providers[0].error_count, 3);
        assert_eq!(providers[0].request_count, 9);
        assert_eq!(
            store.stats_by_provider(StatsWindow::default()).await?[0].error_count,
            4
        );
        pool.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn management_stats_project_net_input_and_preserve_full_attempt_tps() -> anyhow::Result<()>
    {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::migrations::migrate_sqlite(&pool, None).await?;
        let store = SqliteUsageStatsStore {
            pool: pool.clone(),
            last_route_snapshot: Arc::new(parking_lot::RwLock::new(Vec::new())),
        };
        assert_eq!(store.stats_overview(window(3)).await?.avg_output_tps, None);
        assert_eq!(
            store.stats_overview(window(3)).await?.avg_first_token_ms,
            None
        );
        assert!(store.stats_series(window(3), 3_600_000).await?.is_empty());
        let now = chrono::Utc::now().timestamp_millis();
        let recent = now / 3_600_000 * 3_600_000 - 5_400_000;
        let old = now - 4 * 60 * 60 * 1_000;
        insert_turn(&pool, "recent", recent, "model", "key").await?;
        insert_attempt(&pool, "recent-a", "recent", recent, 12, Some(5)).await?;
        insert_attempt(&pool, "recent-b", "recent", recent, 3, Some(9)).await?;
        sqlx::query("UPDATE target_attempt_observations SET output_tokens=CASE id WHEN 'recent-a' THEN 20 ELSE 30 END,duration_ms=CASE id WHEN 'recent-a' THEN 1000 ELSE 9000 END,first_token_ms=CASE id WHEN 'recent-a' THEN 900 ELSE 8900 END WHERE model_turn_id='recent'")
            .execute(&pool).await?;
        let scheduling = store.route_scheduling_snapshot().await;
        assert!(!scheduling.stale);
        assert_eq!(scheduling.targets.len(), 1);
        // 后续发布和客户端交付失败不能改写已成功的上游 attempt。
        sqlx::query("UPDATE model_turn_observations SET status='failed' WHERE id='recent'")
            .execute(&pool)
            .await?;
        sqlx::query("UPDATE inference_run_observations SET status='failed',terminal_reason='delivery_error' WHERE id='recent'")
            .execute(&pool).await?;
        // 失败和未完成 attempt 即使有用量也不计入。
        insert_failed_attempt(&pool, "recent-failed", "recent", recent).await?;
        insert_attempt(&pool, "recent-incomplete", "recent", recent, 999, None).await?;
        sqlx::query("UPDATE target_attempt_observations SET status='running',output_tokens=999,duration_ms=1,first_token_ms=1 WHERE id='recent-incomplete'")
            .execute(&pool).await?;
        sqlx::query("UPDATE target_attempt_observations SET output_tokens=999,first_token_ms=1 WHERE id='recent-failed'")
            .execute(&pool).await?;
        insert_turn(&pool, "old", old, "unknown-cache", "old-key").await?;
        // attempt 自身时间在窗口内，但归属窗口由 Model Turn 开始时间决定。
        insert_attempt(&pool, "old-a", "old", recent, 12528, None).await?;
        sqlx::query("UPDATE target_attempt_observations SET duration_ms=NULL,provider_id='old-provider',provider_name='Old Provider' WHERE id='old-a'")
            .execute(&pool).await?;

        let overview = store.stats_overview(window(3)).await?;
        assert_eq!(overview.total_input_tokens, Some(7));
        assert_eq!(overview.total_output_tokens, Some(50));
        assert_eq!(overview.total_reasoning_tokens, Some(2));
        assert_eq!(overview.avg_output_tps, Some(5.0));
        assert_eq!(overview.avg_first_token_ms, Some(4900.0));
        assert_eq!(overview.avg_duration_ms, Some(10.0));
        // 未知字段只跳过该 attempt，不遮蔽其他已确认用量；全部未报告的字段保持 null。
        let all_time = store.stats_overview(StatsWindow::default()).await?;
        assert_eq!(all_time.total_input_tokens, Some(7));
        assert_eq!(all_time.total_output_tokens, Some(53));
        assert_eq!(all_time.total_cache_write_tokens, None);
        assert_eq!(all_time.avg_output_tps, None);

        let series = store.stats_series(window(3), 3_600_000).await?;
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].total_input_tokens, Some(7));
        assert_eq!(series[0].total_output_tokens, Some(50));
        assert_eq!(series[0].total_reasoning_tokens, Some(2));
        assert_eq!(series[0].avg_output_tps, Some(5.0));
        assert_eq!(series[0].avg_first_token_ms, Some(4900.0));
        assert_eq!(series[0].avg_duration_ms, Some(10.0));
        assert!(series[0].bucket_start >= window(3).start_at.unwrap());

        let quarter = store.stats_series(window(3), 900_000).await?;
        assert_eq!(quarter.len(), 1);
        assert_eq!(quarter[0].bucket_start % 900_000, 0);

        let day = store.stats_series(window(3), 86_400_000).await?;
        assert_eq!(day.len(), 1);
        // Display buckets begin at the absolute selected start, independent of browser timezone.
        assert_eq!(day[0].bucket_start, window(3).start_at.unwrap());

        let models = store.stats_by_model(window(3)).await?;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].total_input_tokens, Some(7));
        assert_eq!(models[0].total_output_tokens, Some(50));
        assert_eq!(models[0].total_reasoning_tokens, Some(2));

        let api_keys = store.stats_by_api_key(window(3)).await?;
        assert_eq!(api_keys.len(), 1);
        assert_eq!(api_keys[0].total_input_tokens, Some(7));
        assert_eq!(api_keys[0].total_output_tokens, Some(50));
        assert_eq!(api_keys[0].reasoning_tokens, Some(2));

        let providers = store.stats_by_provider(window(3)).await?;
        // Two successful attempts belong to one request; the old turn's recent attempt
        // does not move its provider contribution into the recent window.
        assert_eq!(
            providers
                .iter()
                .map(|provider| (
                    provider.provider_id.as_deref(),
                    provider.provider.as_str(),
                    provider.request_count,
                    provider.error_count,
                    provider.avg_output_tps,
                ))
                .collect::<Vec<_>>(),
            vec![(Some("provider"), "Provider", 1, 1, Some(5.0))]
        );
        let old_bucket = old / BASE_BUCKET_MS * BASE_BUCKET_MS;
        let old_providers = store
            .stats_by_provider(StatsWindow {
                start_at: Some(old_bucket),
                end_at: Some(old_bucket + BASE_BUCKET_MS),
            })
            .await?;
        assert_eq!(
            old_providers
                .iter()
                .map(|provider| (
                    provider.provider_id.as_deref(),
                    provider.provider.as_str(),
                    provider.request_count,
                    provider.error_count,
                    provider.avg_output_tps,
                ))
                .collect::<Vec<_>>(),
            vec![(Some("old-provider"), "Old Provider", 1, 0, None)]
        );
        let old_model = store
            .stats_by_model(StatsWindow::default())
            .await?
            .into_iter()
            .find(|model| model.model == "unknown-cache")
            .unwrap();
        assert_eq!(old_model.total_input_tokens, None);

        // 同组只缺一条成功样本也使 TPS 未知，但已知 Token 合计仍保留。
        for (change, expected_tps, expected_output, expected_first, expected_input) in [
            (
                "output_tokens=CASE id WHEN 'recent-a' THEN 20 END,input_tokens=CASE id WHEN 'recent-a' THEN 12 END",
                None,
                Some(20),
                Some(4900.0),
                Some(7),
            ),
            (
                "duration_ms=CASE id WHEN 'recent-a' THEN 1000 END",
                None,
                Some(50),
                Some(4900.0),
                Some(7),
            ),
            ("duration_ms=0", None, Some(50), Some(4900.0), Some(7)),
            ("output_tokens=0", Some(0.0), Some(0), Some(4900.0), Some(7)),
            ("first_token_ms=NULL", Some(5.0), Some(50), None, Some(7)),
            (
                "first_token_ms=CASE id WHEN 'recent-a' THEN 900 END",
                Some(5.0),
                Some(50),
                Some(900.0),
                Some(7),
            ),
            (
                "first_token_ms=CASE id WHEN 'recent-a' THEN 0 END",
                Some(5.0),
                Some(50),
                Some(0.0),
                Some(7),
            ),
            ("first_token_ms=0", Some(5.0), Some(50), Some(0.0), Some(7)),
            ("status='failed'", None, None, None, None),
        ] {
            sqlx::query("UPDATE target_attempt_observations SET status='completed',input_tokens=CASE id WHEN 'recent-a' THEN 12 ELSE 3 END,cache_read_tokens=CASE id WHEN 'recent-a' THEN 5 ELSE 9 END,cache_write_tokens=NULL,output_tokens=CASE id WHEN 'recent-a' THEN 20 ELSE 30 END,duration_ms=CASE id WHEN 'recent-a' THEN 1000 ELSE 9000 END,first_token_ms=CASE id WHEN 'recent-a' THEN 900 ELSE 8900 END WHERE id IN ('recent-a','recent-b')")
                .execute(&pool).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE target_attempt_observations SET {change} WHERE id IN ('recent-a','recent-b')"
            ))).execute(&pool).await?;
            assert_eq!(
                store
                    .stats_by_provider(window(3))
                    .await?
                    .into_iter()
                    .find(|provider| provider.provider == "Provider")
                    .unwrap()
                    .avg_output_tps,
                expected_tps,
                "{change}"
            );
            let overview = store.stats_overview(window(3)).await?;
            assert_eq!(overview.avg_output_tps, expected_tps, "{change}");
            assert_eq!(overview.total_output_tokens, expected_output, "{change}");
            assert_eq!(overview.avg_first_token_ms, expected_first, "{change}");
            assert_eq!(overview.total_input_tokens, expected_input, "{change}");
            let series = store.stats_series(window(3), 3_600_000).await?;
            assert_eq!(series.len(), 1);
            assert_eq!(series[0].avg_output_tps, expected_tps, "{change}");
            assert_eq!(series[0].total_output_tokens, expected_output, "{change}");
            assert_eq!(series[0].avg_first_token_ms, expected_first, "{change}");
            assert_eq!(series[0].total_input_tokens, expected_input, "{change}");
        }

        sqlx::query("UPDATE target_attempt_observations SET status='completed',input_tokens=CASE id WHEN 'recent-a' THEN 12 ELSE 3 END,cache_read_tokens=CASE id WHEN 'recent-a' THEN 5 ELSE 9 END,cache_write_tokens=NULL,output_tokens=CASE id WHEN 'recent-a' THEN 20 ELSE 30 END,duration_ms=CASE id WHEN 'recent-a' THEN 1000 ELSE 9000 END,first_token_ms=CASE id WHEN 'recent-a' THEN 900 ELSE 8900 END WHERE id IN ('recent-a','recent-b')")
            .execute(&pool).await?;
        let raw_input: Option<i64> = sqlx::query_scalar(
            "SELECT SUM(input_tokens) FROM target_attempt_observations WHERE id IN ('recent-a','recent-b')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(raw_input, Some(15));
        // 缓存写入不扣；部分未知照常累加已知 attempt，全未知与已知零保持不同。
        for (assignment, expected) in [
            ("cache_write_tokens=100", Some(7)),
            (
                "cache_read_tokens=CASE id WHEN 'recent-a' THEN 5 END",
                Some(7),
            ),
            ("cache_read_tokens=NULL", None),
            ("cache_read_tokens=100", Some(0)),
            ("input_tokens=NULL,cache_read_tokens=0", None),
            ("input_tokens=0,cache_read_tokens=0", Some(0)),
        ] {
            sqlx::query("UPDATE target_attempt_observations SET status='completed',input_tokens=CASE id WHEN 'recent-a' THEN 12 ELSE 3 END,cache_read_tokens=CASE id WHEN 'recent-a' THEN 5 ELSE 9 END,cache_write_tokens=NULL,output_tokens=CASE id WHEN 'recent-a' THEN 20 ELSE 30 END,duration_ms=CASE id WHEN 'recent-a' THEN 1000 ELSE 9000 END,first_token_ms=CASE id WHEN 'recent-a' THEN 900 ELSE 8900 END WHERE id IN ('recent-a','recent-b')")
                .execute(&pool).await?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "UPDATE target_attempt_observations SET {assignment} WHERE id IN ('recent-a','recent-b')"
            )))
            .execute(&pool)
            .await?;
            assert_eq!(
                store.stats_overview(window(3)).await?.total_input_tokens,
                expected
            );
            assert_eq!(
                store.stats_series(window(3), 3_600_000).await?[0].total_input_tokens,
                expected
            );
            assert_eq!(
                store.stats_by_model(window(3)).await?[0].total_input_tokens,
                expected
            );
            assert_eq!(
                store.stats_by_api_key(window(3)).await?[0].total_input_tokens,
                expected
            );
        }
        // 分属两桶仍按原始窗口合计计算，不平均两个桶的 TPS。
        let next = recent + 3_600_000;
        insert_turn(&pool, "next", next, "model", "key").await?;
        sqlx::query("UPDATE target_attempt_observations SET status='completed',input_tokens=CASE id WHEN 'recent-a' THEN 12 ELSE 3 END,cache_read_tokens=CASE id WHEN 'recent-a' THEN 5 ELSE 9 END,cache_write_tokens=NULL,output_tokens=CASE id WHEN 'recent-a' THEN 20 ELSE 30 END,duration_ms=CASE id WHEN 'recent-a' THEN 1000 ELSE 9000 END,first_token_ms=CASE id WHEN 'recent-a' THEN 900 ELSE 8900 END WHERE id IN ('recent-a','recent-b')")
            .execute(&pool).await?;
        // attempt 时间故意仍在第一桶，验证按所属 Model Turn 分桶。
        sqlx::query("UPDATE target_attempt_observations SET model_turn_id='next',run_id='next',interaction_id='next' WHERE id='recent-b'")
            .execute(&pool).await?;
        assert_eq!(
            store.stats_overview(window(3)).await?.avg_output_tps,
            Some(5.0)
        );
        let series = store.stats_series(window(3), 3_600_000).await?;
        assert_eq!(series.len(), 2);
        assert!(series[0].bucket_start >= window(3).start_at.unwrap());
        assert_eq!(series[1].bucket_start - series[0].bucket_start, 3_600_000);
        assert_eq!(series[0].avg_output_tps, Some(20.0));
        assert!((series[1].avg_output_tps.unwrap() - 10.0 / 3.0).abs() < 1e-12);
        assert_eq!(series[0].avg_first_token_ms, Some(900.0));
        assert_eq!(series[1].avg_first_token_ms, Some(8900.0));
        pool.close().await;
        Ok(())
    }

    async fn verify_fixed_range_survives_detail_cleanup(pool: &SqlitePool) -> anyhow::Result<()> {
        let start = 9_900_000;
        let end = start + 513 * BASE_BUCKET_MS;
        let selected = StatsWindow {
            start_at: Some(start),
            end_at: Some(end),
        };
        for (id, at, route) in [
            ("durable-first", start, "historical-a"),
            ("durable-last", end - BASE_BUCKET_MS, "historical-b"),
            ("durable-excluded", end, "historical-a"),
        ] {
            insert_turn(pool, id, at, "Same name", id).await?;
            sqlx::query("UPDATE model_turn_observations SET route_id=? WHERE id=?")
                .bind(route)
                .bind(id)
                .execute(pool)
                .await?;
            insert_attempt(pool, id, id, at, 12, Some(5)).await?;
        }
        sqlx::query("UPDATE target_attempt_observations SET output_tokens=CASE id WHEN 'durable-first' THEN 20 ELSE 30 END, duration_ms=CASE id WHEN 'durable-first' THEN 1000 ELSE 9000 END, first_token_ms=CASE id WHEN 'durable-first' THEN 0 ELSE 100 END WHERE id IN ('durable-first','durable-last')")
            .execute(pool).await?;
        let store = SqliteUsageStatsStore {
            pool: pool.clone(),
            last_route_snapshot: Arc::new(parking_lot::RwLock::new(Vec::new())),
        };
        let overview = store.stats_overview(selected).await?;
        assert_eq!(overview.total_requests, 2);
        assert_eq!(overview.total_input_tokens, Some(14));
        assert_eq!(overview.total_output_tokens, Some(50));
        assert_eq!(overview.avg_output_tps, Some(5.0));
        assert_eq!(overview.avg_first_token_ms, Some(50.0));
        assert_eq!(overview.avg_duration_ms, Some(10.0));
        assert_eq!(overview.total_cache_write_tokens, None);
        let series = store.stats_series(selected, BASE_BUCKET_MS).await?;
        assert_eq!(series.len(), 2);
        assert!(series.iter().all(|point| point.bucket_ms > BASE_BUCKET_MS));
        assert_eq!(
            series.iter().map(|point| point.request_count).sum::<i64>(),
            2
        );
        assert_eq!(
            series
                .iter()
                .filter_map(|point| point.total_output_tokens)
                .sum::<i64>(),
            50
        );
        assert_eq!(series[0].avg_output_tps, Some(20.0));
        assert!((series[1].avg_output_tps.unwrap() - 10.0 / 3.0).abs() < 1e-12);
        // Quarter-hour start intentionally differs from epoch hour alignment.
        // The 513-quarter range ends with a single-quarter partial hour bucket.
        let hourly = store.stats_series(selected, 3_600_000).await?;
        assert_eq!(hourly.len(), 2);
        assert_eq!(hourly[0].bucket_start, 9_900_000);
        assert_eq!(hourly[1].bucket_start, 470_700_000);
        assert_eq!(hourly[0].bucket_ms, 3_600_000);
        assert_eq!(hourly[1].bucket_ms, 3_600_000);
        assert_eq!(hourly[0].request_count, 1);
        assert_eq!(hourly[1].request_count, 1);
        assert_eq!(hourly[0].total_output_tokens, Some(20));
        assert_eq!(hourly[1].total_output_tokens, Some(30));
        assert_eq!(
            hourly
                .iter()
                .filter_map(|point| point.total_output_tokens)
                .sum::<i64>(),
            50
        );
        let models = store.stats_by_model(selected).await?;
        assert_eq!(models.len(), 2);
        assert!(
            models
                .iter()
                .all(|model| model.model == "Same name" && model.deleted)
        );
        assert_ne!(models[0].model_id, models[1].model_id);
        assert!(store.stats_series(selected, 60_000).await.is_err());
        assert!(store.stats_series(selected, 1_000_000).await.is_err());
        sqlx::query("DELETE FROM interaction_observations WHERE id IN ('durable-first','durable-last','durable-excluded')")
            .execute(pool).await?;
        let retained = store.stats_overview(selected).await?;
        assert_eq!(retained.total_requests, 2);
        assert_eq!(retained.total_input_tokens, Some(14));
        assert_eq!(retained.total_output_tokens, Some(50));
        assert_eq!(retained.avg_output_tps, Some(5.0));
        assert_eq!(store.stats_by_model(selected).await?.len(), 2);
        assert_eq!(store.stats_by_api_key(selected).await?.len(), 2);
        // Legacy/internal turns can legitimately have no API key attribution.
        insert_turn(
            pool,
            "durable-unknown-key",
            start + BASE_BUCKET_MS,
            "Same name",
            "temporary-key",
        )
        .await?;
        sqlx::query("UPDATE model_turn_observations SET api_key_id=NULL,api_key_name='' WHERE id='durable-unknown-key'")
            .execute(pool).await?;
        insert_attempt(
            pool,
            "durable-unknown-key",
            "durable-unknown-key",
            start + BASE_BUCKET_MS,
            12,
            Some(5),
        )
        .await?;
        sqlx::query(
            "UPDATE target_attempt_observations SET output_tokens=0 WHERE id='durable-unknown-key'",
        )
        .execute(pool)
        .await?;
        let keys = store.stats_by_api_key(selected).await?;
        assert_eq!(keys.len(), 3);
        let unknown = keys.iter().find(|key| key.api_key_id.is_empty()).unwrap();
        assert!(unknown.deleted);
        assert_eq!(unknown.api_key_name, "Unknown API Key");
        assert_eq!(unknown.request_count, 1);
        assert_eq!(unknown.total_input_tokens, Some(7));
        assert_eq!(unknown.total_output_tokens, Some(0));
        assert_eq!(unknown.cache_write_tokens, None);
        sqlx::query("DELETE FROM interaction_observations WHERE id='durable-unknown-key'")
            .execute(pool)
            .await?;
        let retained_keys = store.stats_by_api_key(selected).await?;
        assert_eq!(retained_keys.len(), 3);
        let retained_unknown = retained_keys
            .iter()
            .find(|key| key.api_key_id.is_empty())
            .unwrap();
        assert_eq!(retained_unknown.request_count, 1);
        assert_eq!(retained_unknown.total_input_tokens, Some(7));
        assert_eq!(retained_unknown.total_output_tokens, Some(0));
        Ok(())
    }

    #[tokio::test]
    async fn fixed_long_range_keeps_weighted_metrics_and_deleted_identities() -> anyhow::Result<()>
    {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await?;
        crate::migrations::migrate_sqlite(&pool, None).await?;
        verify_fixed_range_survives_detail_cleanup(&pool).await?;
        pool.close().await;
        Ok(())
    }
}
