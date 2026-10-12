// Supplemental storage seams: deterministic timestamps and database-trigger faults cannot
// reliably be driven through the wall-clock HTTP inference boundary. Production queries,
// Observation retention/clear transactions, and both migrated databases remain real.
use std::sync::Arc;

use super::{manifest_index::DebugTraceIndex, store::ObservationStore};
use crate::{
    db::models::StatsWindow,
    storage::{Storage, UsageStatsStore, postgres::PostgresStorage, sqlite::SqliteStorage},
};

const QUARTER: i64 = 900_000;
const START: i64 = 1_800_000;

// All interpolated SQL values below are test-owned literals, never external input.
enum Database {
    Sqlite(sqlx::SqlitePool),
    Postgres(sqlx::PgPool),
}
impl Database {
    async fn execute(&self, sql: impl Into<String>) -> anyhow::Result<()> {
        let sql = sql.into();
        match self {
            Self::Sqlite(pool) => {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(pool)
                    .await?;
            }
            Self::Postgres(pool) => {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(pool)
                    .await?;
            }
        }
        Ok(())
    }
    async fn scalar(&self, sql: &'static str) -> anyhow::Result<i64> {
        Ok(match self {
            Self::Sqlite(pool) => sqlx::query_scalar(sql).fetch_one(pool).await?,
            Self::Postgres(pool) => sqlx::query_scalar(sql).fetch_one(pool).await?,
        })
    }
    fn observation(&self, directory: &std::path::Path) -> anyhow::Result<ObservationStore> {
        let index = Arc::new(DebugTraceIndex::load(directory)?);
        match self {
            Self::Sqlite(pool) => ObservationStore::new(
                Some(pool.clone()),
                None,
                index,
                Some(Arc::new(tokio::sync::Mutex::new(()))),
            ),
            Self::Postgres(pool) => ObservationStore::new(None, Some(pool.clone()), index, None),
        }
    }
    async fn seed(
        &self,
        id: &str,
        run_at: i64,
        turn_at: i64,
        duration: i64,
        output: i64,
        first: i64,
    ) -> anyhow::Result<()> {
        let finished = turn_at + duration;
        self.execute(format!("INSERT INTO interaction_observations (id,principal,api_key_id,api_key_name,root_id,root_run_id,first_route_id,status,started_at,last_active_at,expires_at) VALUES ('{id}','owner','key','Test key','{id}','{id}','route','completed',{run_at},{finished},9223372036854775807);
INSERT INTO inference_run_observations (id,interaction_id,ingress_protocol,route_id,status,debug_enabled,started_at,last_active_at,finished_at,last_event_sequence,expires_at) VALUES ('{id}','{id}','responses','route','completed',false,{run_at},{finished},{finished},0,9223372036854775807);
INSERT INTO model_turn_observations (id,run_id,interaction_id,route_id,model_display_name,api_key_id,api_key_name,status,started_at,finished_at,last_event_sequence) VALUES ('{id}','{id}','{id}','route','Test model','key','Test key','completed',{turn_at},{finished},0);
INSERT INTO target_attempt_observations (id,model_turn_id,run_id,interaction_id,target_id,provider_id,provider_name,upstream_model,protocol,status,started_at,finished_at,duration_ms,first_token_ms,input_tokens,output_tokens,cache_read_tokens,usage_recorded,last_event_sequence) VALUES ('{id}','{id}','{id}','{id}','target','provider','Provider','upstream','responses','completed',{turn_at},{finished},{duration},{first},12,{output},5,true,0);" )).await
    }
    async fn install_fault(&self, table: &str, event: &str) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(_) => self.execute(format!("CREATE TRIGGER hard_case_fault BEFORE {event} ON {table} BEGIN SELECT RAISE(ABORT,'supplemental summary fault'); END;")).await,
            Self::Postgres(_) => self.execute(format!("CREATE FUNCTION hard_case_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'supplemental summary fault' USING ERRCODE='P0001'; END $$; CREATE TRIGGER hard_case_fault BEFORE {event} ON {table} FOR EACH ROW EXECUTE FUNCTION hard_case_fault();")).await,
        }
    }
    async fn remove_fault(&self, table: &str) -> anyhow::Result<()> {
        match self {
            Self::Sqlite(_) => self.execute("DROP TRIGGER hard_case_fault").await,
            Self::Postgres(_) => {
                self.execute(format!(
                    "DROP TRIGGER hard_case_fault ON {table}; DROP FUNCTION hard_case_fault();"
                ))
                .await
            }
        }
    }
}
fn window(start: i64, end: i64) -> StatsWindow {
    StatsWindow {
        start_at: Some(start),
        end_at: Some(end),
    }
}

async fn cross_quarter_and_weighted_metrics(
    db: &Database,
    stats: &dyn UsageStatsStore,
) -> anyhow::Result<()> {
    // Request, turn and attempt each start in a different quarter. The model
    // turn includes preparation time; the successful attempt lasts one second.
    db.seed(
        "cross",
        START + QUARTER - 1,
        START + 2 * QUARTER - 1000,
        10000,
        20,
        0,
    )
    .await?;
    db.execute(format!("UPDATE target_attempt_observations SET started_at={},finished_at={},duration_ms=1000 WHERE id='cross'", START + 2 * QUARTER, START + 2 * QUARTER + 1000)).await?;
    let provider_request = stats
        .stats_by_provider(window(START, START + QUARTER))
        .await?;
    assert_eq!(provider_request.len(), 1);
    assert_eq!(provider_request[0].request_count, 1);
    assert_eq!(provider_request[0].avg_output_tps, None);
    let provider_turn = stats
        .stats_by_provider(window(START + QUARTER, START + 2 * QUARTER))
        .await?;
    assert_eq!(provider_turn.len(), 1);
    assert_eq!(provider_turn[0].request_count, 0);
    assert_eq!(provider_turn[0].avg_output_tps, Some(20.0));
    assert!(
        stats
            .stats_by_provider(window(START + 2 * QUARTER, START + 3 * QUARTER))
            .await?
            .is_empty()
    );
    // Two samples in the next bucket ensure averages cannot be averages of bucket means.
    db.seed(
        "long",
        START + 2 * QUARTER,
        START + 2 * QUARTER,
        9000,
        30,
        900,
    )
    .await?;
    db.seed(
        "short",
        START + 2 * QUARTER + 1,
        START + 2 * QUARTER + 1,
        2000,
        10,
        300,
    )
    .await?;
    let before = stats.stats_overview(window(START, START + QUARTER)).await?;
    assert_eq!(before.total_requests, 1);
    assert_eq!(before.total_output_tokens, None);
    assert_eq!(before.avg_duration_ms, None);
    let turns = stats
        .stats_overview(window(START + QUARTER, START + 3 * QUARTER))
        .await?;
    assert_eq!(turns.total_requests, 2);
    assert_eq!(turns.total_input_tokens, Some(21));
    assert_eq!(turns.total_output_tokens, Some(60));
    assert_eq!(turns.avg_duration_ms, Some(7000.0));
    assert_eq!(turns.avg_first_token_ms, Some(400.0));
    assert_eq!(turns.avg_output_tps, Some(5.0));
    let all = window(START, START + 4 * QUARTER);
    let series = stats.stats_series(all, QUARTER).await?;
    assert_eq!(series.len(), 3);
    assert_eq!(
        series
            .iter()
            .map(|point| point.bucket_start)
            .collect::<Vec<_>>(),
        vec![START, START + QUARTER, START + 2 * QUARTER]
    );
    assert_eq!(
        series
            .iter()
            .map(|point| point.request_count)
            .collect::<Vec<_>>(),
        vec![1, 0, 2]
    );
    assert_eq!(
        series
            .iter()
            .map(|point| point.total_output_tokens)
            .collect::<Vec<_>>(),
        vec![None, Some(20), Some(40)]
    );
    assert_eq!(series[1].avg_output_tps, Some(20.0));
    assert!((series[2].avg_output_tps.unwrap() - 40.0 / 11.0).abs() < 1e-12);
    let coarse = stats.stats_series(all, 8 * QUARTER).await?;
    assert_eq!(coarse.len(), 1);
    assert_eq!(coarse[0].request_count, 3);
    assert_eq!(coarse[0].total_output_tokens, Some(60));
    assert_eq!(coarse[0].avg_duration_ms, Some(7000.0));
    assert_eq!(coarse[0].avg_first_token_ms, Some(400.0));
    assert_eq!(coarse[0].avg_output_tps, Some(5.0));
    assert_eq!(
        stats.stats_by_model(all).await?[0].avg_duration_ms,
        Some(7000.0)
    );
    assert_eq!(
        stats.stats_by_api_key(all).await?[0].total_output_tokens,
        Some(60)
    );
    Ok(())
}

async fn retention_and_failed_clear(
    db: &Database,
    stats: &dyn UsageStatsStore,
    observation: &ObservationStore,
) -> anyhow::Result<()> {
    let selected = window(START, START + 4 * QUARTER);
    // The real retention setting recalculates expiry. No TTL shortening or wall-clock sleeps.
    db.execute("UPDATE interaction_observations SET status='running' WHERE id='long'; UPDATE inference_run_observations SET status='running',finished_at=NULL WHERE id='long'").await?;
    observation.update_retention(7).await?;
    let cutoff = START + 3 * QUARTER + 7 * 86_400_000;
    assert!(
        observation
            .purge_expired_rows(cutoff - 2 * QUARTER)
            .await?
            .is_empty()
    );
    let mut removed = observation.purge_expired_rows(cutoff).await?;
    removed.sort();
    assert_eq!(removed, vec!["cross", "short"]);
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM interaction_observations WHERE id='long'")
            .await?,
        1
    );
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM target_attempt_observations WHERE id IN ('cross','short')")
            .await?,
        0
    );
    assert_eq!(stats.stats_overview(selected).await?.total_requests, 3);
    assert_eq!(
        stats.stats_overview(selected).await?.total_output_tokens,
        Some(60)
    );
    assert_eq!(
        stats.stats_overview(selected).await?.avg_output_tps,
        Some(5.0)
    );
    assert_eq!(stats.stats_by_model(selected).await?[0].request_count, 3);
    assert_eq!(
        stats.stats_by_model(selected).await?[0].avg_duration_ms,
        Some(7000.0)
    );
    assert_eq!(
        stats.stats_by_api_key(selected).await?[0].total_output_tokens,
        Some(60)
    );
    assert_eq!(stats.stats_by_provider(selected).await?[0].request_count, 3);
    assert!(observation.purge_expired_rows(cutoff).await?.is_empty());
    observation.update_retention(30).await?;
    assert_eq!(
        stats.stats_overview(selected).await?.total_output_tokens,
        Some(60)
    );

    db.seed("protected", START, START, 1000, 7, 0).await?;
    // A summary write error must roll back the source revision, not commit uncounted usage.
    db.install_fault("stats_usage_buckets", "UPDATE").await?;
    assert!(
        db.execute("UPDATE target_attempt_observations SET output_tokens=99 WHERE id='protected'")
            .await
            .is_err()
    );
    assert_eq!(
        db.scalar("SELECT output_tokens FROM target_attempt_observations WHERE id='protected'")
            .await?,
        7
    );
    assert_eq!(
        stats.stats_overview(selected).await?.total_output_tokens,
        Some(67)
    );
    db.remove_fault("stats_usage_buckets").await?;
    // Checkpoint reclamation is part of the same real clear transaction. Its failure
    // must preserve source facts and expose failure instead of reporting a successful clear.
    db.install_fault("stats_usage_checkpoints", "DELETE")
        .await?;
    assert!(observation.purge_clear_rows().await.is_err());
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM interaction_observations WHERE id='protected'")
            .await?,
        1
    );
    assert_eq!(
        db.scalar("SELECT output_tokens FROM target_attempt_observations WHERE id='protected'")
            .await?,
        7
    );
    assert_eq!(
        stats.stats_overview(selected).await?.total_output_tokens,
        Some(67)
    );
    db.remove_fault("stats_usage_checkpoints").await?;
    assert_eq!(observation.purge_clear_rows().await?, vec!["protected"]);
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM interaction_observations WHERE id='long'")
            .await?,
        1
    );
    assert_eq!(
        stats.stats_overview(selected).await?.total_output_tokens,
        Some(67)
    );
    assert_eq!(
        stats
            .stats_overview(StatsWindow::default())
            .await?
            .total_output_tokens,
        Some(67)
    );
    Ok(())
}

#[tokio::test]
async fn sqlite_durable_usage_hard_cases() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let pool = crate::test_support::migrated_sqlite_pool().await?;
    let storage = SqliteStorage::from_pool(pool.clone());
    let db = Database::Sqlite(pool.clone());
    let observation = db.observation(directory.path())?;
    let result = async {
        cross_quarter_and_weighted_metrics(&db, storage.usage_stats()).await?;
        retention_and_failed_clear(&db, storage.usage_stats(), &observation).await
    }
    .await;
    pool.close().await;
    result
}

#[tokio::test]
async fn postgres_durable_usage_hard_cases() -> anyhow::Result<()> {
    let Ok(url) = std::env::var("DB_URL") else {
        eprintln!("skip PostgreSQL durable usage hard cases: DB_URL is not set");
        return Ok(());
    };
    let admin = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let schema = format!("stravia_usage_hard_cases_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA {schema}")))
        .execute(&admin)
        .await?;
    let result = async {
        // SQLx 0.9's PgConnectOptions::to_url_lossy omits startup options.
        // Preserve the original URL and encode search_path for the storage URL parser.
        let mut isolated_url = url::Url::parse(&url)?;
        isolated_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let storage = PostgresStorage::connect(crate::storage::sql::config::SqlBackendConfig {
            url: isolated_url.to_string(),
            max_connections: 1,
            min_connections: 0,
            ..Default::default()
        })
        .await?;
        let pool = storage.pool().clone();
        let result = async {
            // Fail before migrations or data changes if isolation is ever lost again.
            let current_schema: Option<String> = sqlx::query_scalar("SELECT current_schema()")
                .fetch_one(&pool)
                .await?;
            anyhow::ensure!(
                current_schema.as_deref() == Some(schema.as_str()),
                "PostgreSQL hard-case fixture did not select its isolated schema"
            );
            crate::migrations::migrate_postgres(&pool, None).await?;
            let directory = tempfile::tempdir()?;
            let db = Database::Postgres(pool.clone());
            let observation = db.observation(directory.path())?;
            cross_quarter_and_weighted_metrics(&db, storage.usage_stats()).await?;
            retention_and_failed_clear(&db, storage.usage_stats(), &observation).await
        }
        .await;
        pool.close().await;
        result
    }
    .await;
    let cleanup = sqlx::query(sqlx::AssertSqlSafe(format!("DROP SCHEMA {schema} CASCADE")))
        .execute(&admin)
        .await;
    admin.close().await;
    result?;
    cleanup?;
    Ok(())
}
