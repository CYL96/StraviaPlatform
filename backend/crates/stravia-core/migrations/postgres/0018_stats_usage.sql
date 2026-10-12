-- Durable quarter-hour aggregates; checkpoints exist only while source observations remain.
-- Migration/backfill and each online revision commit atomically. Detail deletion never debits usage.

CREATE TABLE stats_usage_buckets (
    scope TEXT NOT NULL,
    identity TEXT NOT NULL,
    bucket_start BIGINT NOT NULL,
    entity_id TEXT,
    label TEXT NOT NULL,
    request_count BIGINT NOT NULL DEFAULT 0,
    error_count BIGINT NOT NULL DEFAULT 0,
    input_tokens_sum BIGINT NOT NULL DEFAULT 0,
    input_tokens_known BIGINT NOT NULL DEFAULT 0,
    output_tokens_sum BIGINT NOT NULL DEFAULT 0,
    output_tokens_known BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens_sum BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens_known BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens_sum BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens_known BIGINT NOT NULL DEFAULT 0,
    reasoning_tokens_sum BIGINT NOT NULL DEFAULT 0,
    reasoning_tokens_known BIGINT NOT NULL DEFAULT 0,
    duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    duration_ms_count BIGINT NOT NULL DEFAULT 0,
    first_token_ms_sum BIGINT NOT NULL DEFAULT 0,
    first_token_ms_count BIGINT NOT NULL DEFAULT 0,
    successful_attempt_count BIGINT NOT NULL DEFAULT 0,
    successful_output_known_count BIGINT NOT NULL DEFAULT 0,
    successful_duration_known_count BIGINT NOT NULL DEFAULT 0,
    successful_output_sum BIGINT NOT NULL DEFAULT 0,
    successful_duration_sum BIGINT NOT NULL DEFAULT 0,
    last_used_at BIGINT NOT NULL,
    PRIMARY KEY (scope, identity, bucket_start)
);

CREATE TABLE stats_usage_checkpoints (
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    identity TEXT NOT NULL,
    bucket_start BIGINT NOT NULL,
    entity_id TEXT,
    label TEXT NOT NULL,
    request_count BIGINT NOT NULL DEFAULT 0,
    error_count BIGINT NOT NULL DEFAULT 0,
    input_tokens_sum BIGINT NOT NULL DEFAULT 0,
    input_tokens_known BIGINT NOT NULL DEFAULT 0,
    output_tokens_sum BIGINT NOT NULL DEFAULT 0,
    output_tokens_known BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens_sum BIGINT NOT NULL DEFAULT 0,
    cache_read_tokens_known BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens_sum BIGINT NOT NULL DEFAULT 0,
    cache_write_tokens_known BIGINT NOT NULL DEFAULT 0,
    reasoning_tokens_sum BIGINT NOT NULL DEFAULT 0,
    reasoning_tokens_known BIGINT NOT NULL DEFAULT 0,
    duration_ms_sum BIGINT NOT NULL DEFAULT 0,
    duration_ms_count BIGINT NOT NULL DEFAULT 0,
    first_token_ms_sum BIGINT NOT NULL DEFAULT 0,
    first_token_ms_count BIGINT NOT NULL DEFAULT 0,
    successful_attempt_count BIGINT NOT NULL DEFAULT 0,
    successful_output_known_count BIGINT NOT NULL DEFAULT 0,
    successful_duration_known_count BIGINT NOT NULL DEFAULT 0,
    successful_output_sum BIGINT NOT NULL DEFAULT 0,
    successful_duration_sum BIGINT NOT NULL DEFAULT 0,
    last_used_at BIGINT NOT NULL,
    PRIMARY KEY (source_kind, source_id, scope, identity, bucket_start)
);

CREATE INDEX stats_usage_window_idx ON stats_usage_buckets (scope, bucket_start);

CREATE VIEW stats_usage_turn_source AS
SELECT
    'turn' AS source_kind,
    t.id AS source_id,
    'global' AS scope,
    '' AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    NULL AS entity_id,
    '' AS label,
    0 AS request_count,
    0 AS error_count,
    0 AS input_tokens_sum,
    0 AS input_tokens_known,
    0 AS output_tokens_sum,
    0 AS output_tokens_known,
    0 AS cache_read_tokens_sum,
    0 AS cache_read_tokens_known,
    0 AS cache_write_tokens_sum,
    0 AS cache_write_tokens_known,
    0 AS reasoning_tokens_sum,
    0 AS reasoning_tokens_known,
    COALESCE(t.finished_at-t.started_at,0) AS duration_ms_sum,
    CASE WHEN t.finished_at IS NOT NULL THEN 1 ELSE 0 END AS duration_ms_count,
    0 AS first_token_ms_sum,
    0 AS first_token_ms_count,
    0 AS successful_attempt_count,
    0 AS successful_output_known_count,
    0 AS successful_duration_known_count,
    0 AS successful_output_sum,
    0 AS successful_duration_sum,
    t.started_at AS last_used_at
FROM model_turn_observations t
UNION ALL
SELECT
    'turn' AS source_kind,
    t.id AS source_id,
    'model' AS scope,
    t.route_id AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    NULLIF(t.route_id,'') AS entity_id,
    COALESCE(NULLIF(t.model_display_name,''),t.route_id) AS label,
    1 AS request_count,
    0 AS error_count,
    0 AS input_tokens_sum,
    0 AS input_tokens_known,
    0 AS output_tokens_sum,
    0 AS output_tokens_known,
    0 AS cache_read_tokens_sum,
    0 AS cache_read_tokens_known,
    0 AS cache_write_tokens_sum,
    0 AS cache_write_tokens_known,
    0 AS reasoning_tokens_sum,
    0 AS reasoning_tokens_known,
    COALESCE(t.finished_at-t.started_at,0) AS duration_ms_sum,
    CASE WHEN t.finished_at IS NOT NULL THEN 1 ELSE 0 END AS duration_ms_count,
    0 AS first_token_ms_sum,
    0 AS first_token_ms_count,
    0 AS successful_attempt_count,
    0 AS successful_output_known_count,
    0 AS successful_duration_known_count,
    0 AS successful_output_sum,
    0 AS successful_duration_sum,
    t.started_at AS last_used_at
FROM model_turn_observations t
UNION ALL
SELECT
    'turn' AS source_kind,
    t.id AS source_id,
    'api_key' AS scope,
    COALESCE(t.api_key_id,'') AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    t.api_key_id AS entity_id,
    COALESCE(NULLIF(t.api_key_name,''),t.api_key_id,'Unknown API Key') AS label,
    1 AS request_count,
    0 AS error_count,
    0 AS input_tokens_sum,
    0 AS input_tokens_known,
    0 AS output_tokens_sum,
    0 AS output_tokens_known,
    0 AS cache_read_tokens_sum,
    0 AS cache_read_tokens_known,
    0 AS cache_write_tokens_sum,
    0 AS cache_write_tokens_known,
    0 AS reasoning_tokens_sum,
    0 AS reasoning_tokens_known,
    COALESCE(t.finished_at-t.started_at,0) AS duration_ms_sum,
    CASE WHEN t.finished_at IS NOT NULL THEN 1 ELSE 0 END AS duration_ms_count,
    0 AS first_token_ms_sum,
    0 AS first_token_ms_count,
    0 AS successful_attempt_count,
    0 AS successful_output_known_count,
    0 AS successful_duration_known_count,
    0 AS successful_output_sum,
    0 AS successful_duration_sum,
    t.started_at AS last_used_at
FROM model_turn_observations t;

CREATE VIEW stats_usage_attempt_source AS
SELECT
    'attempt' AS source_kind,
    a.id AS source_id,
    'global' AS scope,
    '' AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    NULL AS entity_id,
    '' AS label,
    0 AS request_count,
    0 AS error_count,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN CASE WHEN a.input_tokens>a.cache_read_tokens THEN a.input_tokens-a.cache_read_tokens ELSE 0 END ELSE 0 END AS input_tokens_sum,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS input_tokens_known,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN a.output_tokens ELSE 0 END AS output_tokens_sum,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS output_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN a.cache_read_tokens ELSE 0 END AS cache_read_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_read_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN a.cache_write_tokens ELSE 0 END AS cache_write_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_write_tokens_known,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN a.reasoning_tokens ELSE 0 END AS reasoning_tokens_sum,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN 1 ELSE 0 END AS reasoning_tokens_known,
    0 AS duration_ms_sum,
    0 AS duration_ms_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.first_token_ms,0) ELSE 0 END AS first_token_ms_sum,
    CASE WHEN a.status='completed' AND a.first_token_ms IS NOT NULL THEN 1 ELSE 0 END AS first_token_ms_count,
    CASE WHEN a.status='completed' THEN 1 ELSE 0 END AS successful_attempt_count,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS successful_output_known_count,
    CASE WHEN a.status='completed' AND a.duration_ms IS NOT NULL THEN 1 ELSE 0 END AS successful_duration_known_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.output_tokens,0) ELSE 0 END AS successful_output_sum,
    CASE WHEN a.status='completed' THEN COALESCE(a.duration_ms,0) ELSE 0 END AS successful_duration_sum,
    t.started_at AS last_used_at
FROM target_attempt_observations a JOIN model_turn_observations t ON t.id=a.model_turn_id
UNION ALL
SELECT
    'attempt' AS source_kind,
    a.id AS source_id,
    'model' AS scope,
    t.route_id AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    NULLIF(t.route_id,'') AS entity_id,
    COALESCE(NULLIF(t.model_display_name,''),t.route_id) AS label,
    0 AS request_count,
    0 AS error_count,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN CASE WHEN a.input_tokens>a.cache_read_tokens THEN a.input_tokens-a.cache_read_tokens ELSE 0 END ELSE 0 END AS input_tokens_sum,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS input_tokens_known,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN a.output_tokens ELSE 0 END AS output_tokens_sum,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS output_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN a.cache_read_tokens ELSE 0 END AS cache_read_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_read_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN a.cache_write_tokens ELSE 0 END AS cache_write_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_write_tokens_known,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN a.reasoning_tokens ELSE 0 END AS reasoning_tokens_sum,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN 1 ELSE 0 END AS reasoning_tokens_known,
    0 AS duration_ms_sum,
    0 AS duration_ms_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.first_token_ms,0) ELSE 0 END AS first_token_ms_sum,
    CASE WHEN a.status='completed' AND a.first_token_ms IS NOT NULL THEN 1 ELSE 0 END AS first_token_ms_count,
    CASE WHEN a.status='completed' THEN 1 ELSE 0 END AS successful_attempt_count,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS successful_output_known_count,
    CASE WHEN a.status='completed' AND a.duration_ms IS NOT NULL THEN 1 ELSE 0 END AS successful_duration_known_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.output_tokens,0) ELSE 0 END AS successful_output_sum,
    CASE WHEN a.status='completed' THEN COALESCE(a.duration_ms,0) ELSE 0 END AS successful_duration_sum,
    t.started_at AS last_used_at
FROM target_attempt_observations a JOIN model_turn_observations t ON t.id=a.model_turn_id
UNION ALL
SELECT
    'attempt' AS source_kind,
    a.id AS source_id,
    'api_key' AS scope,
    COALESCE(t.api_key_id,'') AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    t.api_key_id AS entity_id,
    COALESCE(NULLIF(t.api_key_name,''),t.api_key_id,'Unknown API Key') AS label,
    0 AS request_count,
    0 AS error_count,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN CASE WHEN a.input_tokens>a.cache_read_tokens THEN a.input_tokens-a.cache_read_tokens ELSE 0 END ELSE 0 END AS input_tokens_sum,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS input_tokens_known,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN a.output_tokens ELSE 0 END AS output_tokens_sum,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS output_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN a.cache_read_tokens ELSE 0 END AS cache_read_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_read_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN a.cache_write_tokens ELSE 0 END AS cache_write_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_write_tokens_known,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN a.reasoning_tokens ELSE 0 END AS reasoning_tokens_sum,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN 1 ELSE 0 END AS reasoning_tokens_known,
    0 AS duration_ms_sum,
    0 AS duration_ms_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.first_token_ms,0) ELSE 0 END AS first_token_ms_sum,
    CASE WHEN a.status='completed' AND a.first_token_ms IS NOT NULL THEN 1 ELSE 0 END AS first_token_ms_count,
    CASE WHEN a.status='completed' THEN 1 ELSE 0 END AS successful_attempt_count,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS successful_output_known_count,
    CASE WHEN a.status='completed' AND a.duration_ms IS NOT NULL THEN 1 ELSE 0 END AS successful_duration_known_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.output_tokens,0) ELSE 0 END AS successful_output_sum,
    CASE WHEN a.status='completed' THEN COALESCE(a.duration_ms,0) ELSE 0 END AS successful_duration_sum,
    t.started_at AS last_used_at
FROM target_attempt_observations a JOIN model_turn_observations t ON t.id=a.model_turn_id
UNION ALL
SELECT
    'attempt' AS source_kind,
    a.id AS source_id,
    'provider' AS scope,
    a.provider_id AS identity,
    (t.started_at / 900000) * 900000 AS bucket_start,
    NULLIF(a.provider_id,'') AS entity_id,
    COALESCE(NULLIF(a.provider_name,''),a.provider_id) AS label,
    0 AS request_count,
    0 AS error_count,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN CASE WHEN a.input_tokens>a.cache_read_tokens THEN a.input_tokens-a.cache_read_tokens ELSE 0 END ELSE 0 END AS input_tokens_sum,
    CASE WHEN a.status='completed' AND a.input_tokens IS NOT NULL AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS input_tokens_known,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN a.output_tokens ELSE 0 END AS output_tokens_sum,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS output_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN a.cache_read_tokens ELSE 0 END AS cache_read_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_read_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_read_tokens_known,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN a.cache_write_tokens ELSE 0 END AS cache_write_tokens_sum,
    CASE WHEN a.status='completed' AND a.cache_write_tokens IS NOT NULL THEN 1 ELSE 0 END AS cache_write_tokens_known,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN a.reasoning_tokens ELSE 0 END AS reasoning_tokens_sum,
    CASE WHEN a.status='completed' AND a.reasoning_tokens IS NOT NULL THEN 1 ELSE 0 END AS reasoning_tokens_known,
    COALESCE(a.duration_ms,0) AS duration_ms_sum,
    CASE WHEN a.duration_ms IS NOT NULL THEN 1 ELSE 0 END AS duration_ms_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.first_token_ms,0) ELSE 0 END AS first_token_ms_sum,
    CASE WHEN a.status='completed' AND a.first_token_ms IS NOT NULL THEN 1 ELSE 0 END AS first_token_ms_count,
    CASE WHEN a.status='completed' THEN 1 ELSE 0 END AS successful_attempt_count,
    CASE WHEN a.status='completed' AND a.output_tokens IS NOT NULL THEN 1 ELSE 0 END AS successful_output_known_count,
    CASE WHEN a.status='completed' AND a.duration_ms IS NOT NULL THEN 1 ELSE 0 END AS successful_duration_known_count,
    CASE WHEN a.status='completed' THEN COALESCE(a.output_tokens,0) ELSE 0 END AS successful_output_sum,
    CASE WHEN a.status='completed' THEN COALESCE(a.duration_ms,0) ELSE 0 END AS successful_duration_sum,
    t.started_at AS last_used_at
FROM target_attempt_observations a JOIN model_turn_observations t ON t.id=a.model_turn_id;

CREATE VIEW stats_usage_run_source AS
SELECT
    'run' AS source_kind,
    r.id AS source_id,
    'global' AS scope,
    '' AS identity,
    (r.started_at / 900000) * 900000 AS bucket_start,
    NULL AS entity_id,
    '' AS label,
    1 AS request_count,
    CASE WHEN r.status='failed' AND r.finished_at IS NOT NULL AND (r.failure_json IS NOT NULL OR COALESCE(r.terminal_reason,'') NOT IN ('cancelled','client_disconnected','websocket_delivery_dropped','request_aborted')) THEN 1 ELSE 0 END AS error_count,
    0 AS input_tokens_sum,
    0 AS input_tokens_known,
    0 AS output_tokens_sum,
    0 AS output_tokens_known,
    0 AS cache_read_tokens_sum,
    0 AS cache_read_tokens_known,
    0 AS cache_write_tokens_sum,
    0 AS cache_write_tokens_known,
    0 AS reasoning_tokens_sum,
    0 AS reasoning_tokens_known,
    0 AS duration_ms_sum,
    0 AS duration_ms_count,
    0 AS first_token_ms_sum,
    0 AS first_token_ms_count,
    0 AS successful_attempt_count,
    0 AS successful_output_known_count,
    0 AS successful_duration_known_count,
    0 AS successful_output_sum,
    0 AS successful_duration_sum,
    r.started_at AS last_used_at
FROM inference_run_observations r
UNION ALL
SELECT
    'run' AS source_kind,
    r.id AS source_id,
    'provider' AS scope,
    p.provider_id AS identity,
    (r.started_at / 900000) * 900000 AS bucket_start,
    NULLIF(p.provider_id,'') AS entity_id,
    COALESCE(NULLIF(p.provider_name,''),p.provider_id) AS label,
    1 AS request_count,
    CASE WHEN r.status='failed' AND r.finished_at IS NOT NULL AND (r.failure_json IS NOT NULL OR COALESCE(r.terminal_reason,'') NOT IN ('cancelled','client_disconnected','websocket_delivery_dropped','request_aborted')) THEN 1 ELSE 0 END AS error_count,
    0 AS input_tokens_sum,
    0 AS input_tokens_known,
    0 AS output_tokens_sum,
    0 AS output_tokens_known,
    0 AS cache_read_tokens_sum,
    0 AS cache_read_tokens_known,
    0 AS cache_write_tokens_sum,
    0 AS cache_write_tokens_known,
    0 AS reasoning_tokens_sum,
    0 AS reasoning_tokens_known,
    0 AS duration_ms_sum,
    0 AS duration_ms_count,
    0 AS first_token_ms_sum,
    0 AS first_token_ms_count,
    0 AS successful_attempt_count,
    0 AS successful_output_known_count,
    0 AS successful_duration_known_count,
    0 AS successful_output_sum,
    0 AS successful_duration_sum,
    r.started_at AS last_used_at
FROM inference_run_observations r JOIN (SELECT run_id,provider_id,MAX(provider_name) AS provider_name FROM target_attempt_observations GROUP BY run_id,provider_id) p ON p.run_id=r.id;

CREATE VIEW stats_usage_rejection_source AS
SELECT
    'rejection' AS source_kind,
    r.id AS source_id,
    'global' AS scope,
    '' AS identity,
    (COALESCE(r.started_at,r.occurred_at) / 900000) * 900000 AS bucket_start,
    NULL AS entity_id,
    '' AS label,
    1 AS request_count,
    CASE WHEN r.status_code<>499 AND r.code NOT IN ('request_aborted','cancelled','client_disconnected') THEN 1 ELSE 0 END AS error_count,
    0 AS input_tokens_sum,
    0 AS input_tokens_known,
    0 AS output_tokens_sum,
    0 AS output_tokens_known,
    0 AS cache_read_tokens_sum,
    0 AS cache_read_tokens_known,
    0 AS cache_write_tokens_sum,
    0 AS cache_write_tokens_known,
    0 AS reasoning_tokens_sum,
    0 AS reasoning_tokens_known,
    0 AS duration_ms_sum,
    0 AS duration_ms_count,
    0 AS first_token_ms_sum,
    0 AS first_token_ms_count,
    0 AS successful_attempt_count,
    0 AS successful_output_known_count,
    0 AS successful_duration_known_count,
    0 AS successful_output_sum,
    0 AS successful_duration_sum,
    COALESCE(r.started_at,r.occurred_at) AS last_used_at
FROM rejected_request_observations r;

CREATE FUNCTION stats_usage_credit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
INSERT INTO stats_usage_buckets (
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) VALUES (
    NEW.scope,
    NEW.identity,
    NEW.bucket_start,
    NEW.entity_id,
    NEW.label,
    NEW.request_count,
    NEW.error_count,
    NEW.input_tokens_sum,
    NEW.input_tokens_known,
    NEW.output_tokens_sum,
    NEW.output_tokens_known,
    NEW.cache_read_tokens_sum,
    NEW.cache_read_tokens_known,
    NEW.cache_write_tokens_sum,
    NEW.cache_write_tokens_known,
    NEW.reasoning_tokens_sum,
    NEW.reasoning_tokens_known,
    NEW.duration_ms_sum,
    NEW.duration_ms_count,
    NEW.first_token_ms_sum,
    NEW.first_token_ms_count,
    NEW.successful_attempt_count,
    NEW.successful_output_known_count,
    NEW.successful_duration_known_count,
    NEW.successful_output_sum,
    NEW.successful_duration_sum,
    NEW.last_used_at
) ON CONFLICT (scope, identity, bucket_start) DO UPDATE SET
    request_count = stats_usage_buckets.request_count + excluded.request_count,
    error_count = stats_usage_buckets.error_count + excluded.error_count,
    input_tokens_sum = stats_usage_buckets.input_tokens_sum + excluded.input_tokens_sum,
    input_tokens_known = stats_usage_buckets.input_tokens_known + excluded.input_tokens_known,
    output_tokens_sum = stats_usage_buckets.output_tokens_sum + excluded.output_tokens_sum,
    output_tokens_known = stats_usage_buckets.output_tokens_known + excluded.output_tokens_known,
    cache_read_tokens_sum = stats_usage_buckets.cache_read_tokens_sum + excluded.cache_read_tokens_sum,
    cache_read_tokens_known = stats_usage_buckets.cache_read_tokens_known + excluded.cache_read_tokens_known,
    cache_write_tokens_sum = stats_usage_buckets.cache_write_tokens_sum + excluded.cache_write_tokens_sum,
    cache_write_tokens_known = stats_usage_buckets.cache_write_tokens_known + excluded.cache_write_tokens_known,
    reasoning_tokens_sum = stats_usage_buckets.reasoning_tokens_sum + excluded.reasoning_tokens_sum,
    reasoning_tokens_known = stats_usage_buckets.reasoning_tokens_known + excluded.reasoning_tokens_known,
    duration_ms_sum = stats_usage_buckets.duration_ms_sum + excluded.duration_ms_sum,
    duration_ms_count = stats_usage_buckets.duration_ms_count + excluded.duration_ms_count,
    first_token_ms_sum = stats_usage_buckets.first_token_ms_sum + excluded.first_token_ms_sum,
    first_token_ms_count = stats_usage_buckets.first_token_ms_count + excluded.first_token_ms_count,
    successful_attempt_count = stats_usage_buckets.successful_attempt_count + excluded.successful_attempt_count,
    successful_output_known_count = stats_usage_buckets.successful_output_known_count + excluded.successful_output_known_count,
    successful_duration_known_count = stats_usage_buckets.successful_duration_known_count + excluded.successful_duration_known_count,
    successful_output_sum = stats_usage_buckets.successful_output_sum + excluded.successful_output_sum,
    successful_duration_sum = stats_usage_buckets.successful_duration_sum + excluded.successful_duration_sum,
    label = excluded.label,
    last_used_at = GREATEST(stats_usage_buckets.last_used_at, excluded.last_used_at);
    RETURN NEW;
END $$;
CREATE TRIGGER stats_usage_credit AFTER INSERT ON stats_usage_checkpoints
FOR EACH ROW EXECUTE FUNCTION stats_usage_credit();

CREATE FUNCTION stats_usage_turn_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        -- Reclaim work, but preserve permanent historical contribution.
        DELETE FROM stats_usage_checkpoints
        WHERE source_kind = 'turn' AND source_id = OLD.id;
        RETURN OLD;
    END IF;
-- Drive the delta from this source checkpoint, never from the growing aggregate history.
UPDATE stats_usage_buckets SET
    request_count = stats_usage_buckets.request_count - c.request_count,
    error_count = stats_usage_buckets.error_count - c.error_count,
    input_tokens_sum = stats_usage_buckets.input_tokens_sum - c.input_tokens_sum,
    input_tokens_known = stats_usage_buckets.input_tokens_known - c.input_tokens_known,
    output_tokens_sum = stats_usage_buckets.output_tokens_sum - c.output_tokens_sum,
    output_tokens_known = stats_usage_buckets.output_tokens_known - c.output_tokens_known,
    cache_read_tokens_sum = stats_usage_buckets.cache_read_tokens_sum - c.cache_read_tokens_sum,
    cache_read_tokens_known = stats_usage_buckets.cache_read_tokens_known - c.cache_read_tokens_known,
    cache_write_tokens_sum = stats_usage_buckets.cache_write_tokens_sum - c.cache_write_tokens_sum,
    cache_write_tokens_known = stats_usage_buckets.cache_write_tokens_known - c.cache_write_tokens_known,
    reasoning_tokens_sum = stats_usage_buckets.reasoning_tokens_sum - c.reasoning_tokens_sum,
    reasoning_tokens_known = stats_usage_buckets.reasoning_tokens_known - c.reasoning_tokens_known,
    duration_ms_sum = stats_usage_buckets.duration_ms_sum - c.duration_ms_sum,
    duration_ms_count = stats_usage_buckets.duration_ms_count - c.duration_ms_count,
    first_token_ms_sum = stats_usage_buckets.first_token_ms_sum - c.first_token_ms_sum,
    first_token_ms_count = stats_usage_buckets.first_token_ms_count - c.first_token_ms_count,
    successful_attempt_count = stats_usage_buckets.successful_attempt_count - c.successful_attempt_count,
    successful_output_known_count = stats_usage_buckets.successful_output_known_count - c.successful_output_known_count,
    successful_duration_known_count = stats_usage_buckets.successful_duration_known_count - c.successful_duration_known_count,
    successful_output_sum = stats_usage_buckets.successful_output_sum - c.successful_output_sum,
    successful_duration_sum = stats_usage_buckets.successful_duration_sum - c.successful_duration_sum
FROM stats_usage_checkpoints AS c
WHERE c.source_kind = 'turn' AND c.source_id = NEW.id
  AND c.scope = stats_usage_buckets.scope
  AND c.identity = stats_usage_buckets.identity
  AND c.bucket_start = stats_usage_buckets.bucket_start
  AND (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'turn' AND c.source_id = NEW.id
);
DELETE FROM stats_usage_buckets
WHERE (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'turn' AND c.source_id = NEW.id
)
  AND stats_usage_buckets.request_count = 0
  AND stats_usage_buckets.error_count = 0
  AND stats_usage_buckets.input_tokens_sum = 0
  AND stats_usage_buckets.input_tokens_known = 0
  AND stats_usage_buckets.output_tokens_sum = 0
  AND stats_usage_buckets.output_tokens_known = 0
  AND stats_usage_buckets.cache_read_tokens_sum = 0
  AND stats_usage_buckets.cache_read_tokens_known = 0
  AND stats_usage_buckets.cache_write_tokens_sum = 0
  AND stats_usage_buckets.cache_write_tokens_known = 0
  AND stats_usage_buckets.reasoning_tokens_sum = 0
  AND stats_usage_buckets.reasoning_tokens_known = 0
  AND stats_usage_buckets.duration_ms_sum = 0
  AND stats_usage_buckets.duration_ms_count = 0
  AND stats_usage_buckets.first_token_ms_sum = 0
  AND stats_usage_buckets.first_token_ms_count = 0
  AND stats_usage_buckets.successful_attempt_count = 0
  AND stats_usage_buckets.successful_output_known_count = 0
  AND stats_usage_buckets.successful_duration_known_count = 0
  AND stats_usage_buckets.successful_output_sum = 0
  AND stats_usage_buckets.successful_duration_sum = 0;
DELETE FROM stats_usage_checkpoints
WHERE source_kind = 'turn' AND source_id = NEW.id;
INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_turn_source
WHERE source_id = NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER stats_usage_turn_change AFTER INSERT OR UPDATE OR DELETE ON model_turn_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_turn_change();

CREATE FUNCTION stats_usage_attempt_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        -- Reclaim work, but preserve permanent historical contribution.
        DELETE FROM stats_usage_checkpoints
        WHERE source_kind = 'attempt' AND source_id = OLD.id;
        RETURN OLD;
    END IF;
-- Serialize provider request revisions with the owning run.
PERFORM 1 FROM inference_run_observations WHERE id = NEW.run_id FOR UPDATE;
-- Drive the delta from this source checkpoint, never from the growing aggregate history.
UPDATE stats_usage_buckets SET
    request_count = stats_usage_buckets.request_count - c.request_count,
    error_count = stats_usage_buckets.error_count - c.error_count,
    input_tokens_sum = stats_usage_buckets.input_tokens_sum - c.input_tokens_sum,
    input_tokens_known = stats_usage_buckets.input_tokens_known - c.input_tokens_known,
    output_tokens_sum = stats_usage_buckets.output_tokens_sum - c.output_tokens_sum,
    output_tokens_known = stats_usage_buckets.output_tokens_known - c.output_tokens_known,
    cache_read_tokens_sum = stats_usage_buckets.cache_read_tokens_sum - c.cache_read_tokens_sum,
    cache_read_tokens_known = stats_usage_buckets.cache_read_tokens_known - c.cache_read_tokens_known,
    cache_write_tokens_sum = stats_usage_buckets.cache_write_tokens_sum - c.cache_write_tokens_sum,
    cache_write_tokens_known = stats_usage_buckets.cache_write_tokens_known - c.cache_write_tokens_known,
    reasoning_tokens_sum = stats_usage_buckets.reasoning_tokens_sum - c.reasoning_tokens_sum,
    reasoning_tokens_known = stats_usage_buckets.reasoning_tokens_known - c.reasoning_tokens_known,
    duration_ms_sum = stats_usage_buckets.duration_ms_sum - c.duration_ms_sum,
    duration_ms_count = stats_usage_buckets.duration_ms_count - c.duration_ms_count,
    first_token_ms_sum = stats_usage_buckets.first_token_ms_sum - c.first_token_ms_sum,
    first_token_ms_count = stats_usage_buckets.first_token_ms_count - c.first_token_ms_count,
    successful_attempt_count = stats_usage_buckets.successful_attempt_count - c.successful_attempt_count,
    successful_output_known_count = stats_usage_buckets.successful_output_known_count - c.successful_output_known_count,
    successful_duration_known_count = stats_usage_buckets.successful_duration_known_count - c.successful_duration_known_count,
    successful_output_sum = stats_usage_buckets.successful_output_sum - c.successful_output_sum,
    successful_duration_sum = stats_usage_buckets.successful_duration_sum - c.successful_duration_sum
FROM stats_usage_checkpoints AS c
WHERE c.source_kind = 'attempt' AND c.source_id = NEW.id
  AND c.scope = stats_usage_buckets.scope
  AND c.identity = stats_usage_buckets.identity
  AND c.bucket_start = stats_usage_buckets.bucket_start
  AND (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'attempt' AND c.source_id = NEW.id
);
DELETE FROM stats_usage_buckets
WHERE (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'attempt' AND c.source_id = NEW.id
)
  AND stats_usage_buckets.request_count = 0
  AND stats_usage_buckets.error_count = 0
  AND stats_usage_buckets.input_tokens_sum = 0
  AND stats_usage_buckets.input_tokens_known = 0
  AND stats_usage_buckets.output_tokens_sum = 0
  AND stats_usage_buckets.output_tokens_known = 0
  AND stats_usage_buckets.cache_read_tokens_sum = 0
  AND stats_usage_buckets.cache_read_tokens_known = 0
  AND stats_usage_buckets.cache_write_tokens_sum = 0
  AND stats_usage_buckets.cache_write_tokens_known = 0
  AND stats_usage_buckets.reasoning_tokens_sum = 0
  AND stats_usage_buckets.reasoning_tokens_known = 0
  AND stats_usage_buckets.duration_ms_sum = 0
  AND stats_usage_buckets.duration_ms_count = 0
  AND stats_usage_buckets.first_token_ms_sum = 0
  AND stats_usage_buckets.first_token_ms_count = 0
  AND stats_usage_buckets.successful_attempt_count = 0
  AND stats_usage_buckets.successful_output_known_count = 0
  AND stats_usage_buckets.successful_duration_known_count = 0
  AND stats_usage_buckets.successful_output_sum = 0
  AND stats_usage_buckets.successful_duration_sum = 0;
DELETE FROM stats_usage_checkpoints
WHERE source_kind = 'attempt' AND source_id = NEW.id;
INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_attempt_source
WHERE source_id = NEW.id;
-- Drive the delta from this source checkpoint, never from the growing aggregate history.
UPDATE stats_usage_buckets SET
    request_count = stats_usage_buckets.request_count - c.request_count,
    error_count = stats_usage_buckets.error_count - c.error_count,
    input_tokens_sum = stats_usage_buckets.input_tokens_sum - c.input_tokens_sum,
    input_tokens_known = stats_usage_buckets.input_tokens_known - c.input_tokens_known,
    output_tokens_sum = stats_usage_buckets.output_tokens_sum - c.output_tokens_sum,
    output_tokens_known = stats_usage_buckets.output_tokens_known - c.output_tokens_known,
    cache_read_tokens_sum = stats_usage_buckets.cache_read_tokens_sum - c.cache_read_tokens_sum,
    cache_read_tokens_known = stats_usage_buckets.cache_read_tokens_known - c.cache_read_tokens_known,
    cache_write_tokens_sum = stats_usage_buckets.cache_write_tokens_sum - c.cache_write_tokens_sum,
    cache_write_tokens_known = stats_usage_buckets.cache_write_tokens_known - c.cache_write_tokens_known,
    reasoning_tokens_sum = stats_usage_buckets.reasoning_tokens_sum - c.reasoning_tokens_sum,
    reasoning_tokens_known = stats_usage_buckets.reasoning_tokens_known - c.reasoning_tokens_known,
    duration_ms_sum = stats_usage_buckets.duration_ms_sum - c.duration_ms_sum,
    duration_ms_count = stats_usage_buckets.duration_ms_count - c.duration_ms_count,
    first_token_ms_sum = stats_usage_buckets.first_token_ms_sum - c.first_token_ms_sum,
    first_token_ms_count = stats_usage_buckets.first_token_ms_count - c.first_token_ms_count,
    successful_attempt_count = stats_usage_buckets.successful_attempt_count - c.successful_attempt_count,
    successful_output_known_count = stats_usage_buckets.successful_output_known_count - c.successful_output_known_count,
    successful_duration_known_count = stats_usage_buckets.successful_duration_known_count - c.successful_duration_known_count,
    successful_output_sum = stats_usage_buckets.successful_output_sum - c.successful_output_sum,
    successful_duration_sum = stats_usage_buckets.successful_duration_sum - c.successful_duration_sum
FROM stats_usage_checkpoints AS c
WHERE c.source_kind = 'run' AND c.source_id = NEW.run_id
  AND c.scope = stats_usage_buckets.scope
  AND c.identity = stats_usage_buckets.identity
  AND c.bucket_start = stats_usage_buckets.bucket_start
  AND (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'run' AND c.source_id = NEW.run_id
);
DELETE FROM stats_usage_buckets
WHERE (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'run' AND c.source_id = NEW.run_id
)
  AND stats_usage_buckets.request_count = 0
  AND stats_usage_buckets.error_count = 0
  AND stats_usage_buckets.input_tokens_sum = 0
  AND stats_usage_buckets.input_tokens_known = 0
  AND stats_usage_buckets.output_tokens_sum = 0
  AND stats_usage_buckets.output_tokens_known = 0
  AND stats_usage_buckets.cache_read_tokens_sum = 0
  AND stats_usage_buckets.cache_read_tokens_known = 0
  AND stats_usage_buckets.cache_write_tokens_sum = 0
  AND stats_usage_buckets.cache_write_tokens_known = 0
  AND stats_usage_buckets.reasoning_tokens_sum = 0
  AND stats_usage_buckets.reasoning_tokens_known = 0
  AND stats_usage_buckets.duration_ms_sum = 0
  AND stats_usage_buckets.duration_ms_count = 0
  AND stats_usage_buckets.first_token_ms_sum = 0
  AND stats_usage_buckets.first_token_ms_count = 0
  AND stats_usage_buckets.successful_attempt_count = 0
  AND stats_usage_buckets.successful_output_known_count = 0
  AND stats_usage_buckets.successful_duration_known_count = 0
  AND stats_usage_buckets.successful_output_sum = 0
  AND stats_usage_buckets.successful_duration_sum = 0;
DELETE FROM stats_usage_checkpoints
WHERE source_kind = 'run' AND source_id = NEW.run_id;
INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_run_source
WHERE source_id = NEW.run_id;
    RETURN NEW;
END $$;
CREATE TRIGGER stats_usage_attempt_change AFTER INSERT OR UPDATE OR DELETE ON target_attempt_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_attempt_change();

CREATE FUNCTION stats_usage_run_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        -- Reclaim work, but preserve permanent historical contribution.
        DELETE FROM stats_usage_checkpoints
        WHERE source_kind = 'run' AND source_id = OLD.id;
        RETURN OLD;
    END IF;
-- Drive the delta from this source checkpoint, never from the growing aggregate history.
UPDATE stats_usage_buckets SET
    request_count = stats_usage_buckets.request_count - c.request_count,
    error_count = stats_usage_buckets.error_count - c.error_count,
    input_tokens_sum = stats_usage_buckets.input_tokens_sum - c.input_tokens_sum,
    input_tokens_known = stats_usage_buckets.input_tokens_known - c.input_tokens_known,
    output_tokens_sum = stats_usage_buckets.output_tokens_sum - c.output_tokens_sum,
    output_tokens_known = stats_usage_buckets.output_tokens_known - c.output_tokens_known,
    cache_read_tokens_sum = stats_usage_buckets.cache_read_tokens_sum - c.cache_read_tokens_sum,
    cache_read_tokens_known = stats_usage_buckets.cache_read_tokens_known - c.cache_read_tokens_known,
    cache_write_tokens_sum = stats_usage_buckets.cache_write_tokens_sum - c.cache_write_tokens_sum,
    cache_write_tokens_known = stats_usage_buckets.cache_write_tokens_known - c.cache_write_tokens_known,
    reasoning_tokens_sum = stats_usage_buckets.reasoning_tokens_sum - c.reasoning_tokens_sum,
    reasoning_tokens_known = stats_usage_buckets.reasoning_tokens_known - c.reasoning_tokens_known,
    duration_ms_sum = stats_usage_buckets.duration_ms_sum - c.duration_ms_sum,
    duration_ms_count = stats_usage_buckets.duration_ms_count - c.duration_ms_count,
    first_token_ms_sum = stats_usage_buckets.first_token_ms_sum - c.first_token_ms_sum,
    first_token_ms_count = stats_usage_buckets.first_token_ms_count - c.first_token_ms_count,
    successful_attempt_count = stats_usage_buckets.successful_attempt_count - c.successful_attempt_count,
    successful_output_known_count = stats_usage_buckets.successful_output_known_count - c.successful_output_known_count,
    successful_duration_known_count = stats_usage_buckets.successful_duration_known_count - c.successful_duration_known_count,
    successful_output_sum = stats_usage_buckets.successful_output_sum - c.successful_output_sum,
    successful_duration_sum = stats_usage_buckets.successful_duration_sum - c.successful_duration_sum
FROM stats_usage_checkpoints AS c
WHERE c.source_kind = 'run' AND c.source_id = NEW.id
  AND c.scope = stats_usage_buckets.scope
  AND c.identity = stats_usage_buckets.identity
  AND c.bucket_start = stats_usage_buckets.bucket_start
  AND (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'run' AND c.source_id = NEW.id
);
DELETE FROM stats_usage_buckets
WHERE (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'run' AND c.source_id = NEW.id
)
  AND stats_usage_buckets.request_count = 0
  AND stats_usage_buckets.error_count = 0
  AND stats_usage_buckets.input_tokens_sum = 0
  AND stats_usage_buckets.input_tokens_known = 0
  AND stats_usage_buckets.output_tokens_sum = 0
  AND stats_usage_buckets.output_tokens_known = 0
  AND stats_usage_buckets.cache_read_tokens_sum = 0
  AND stats_usage_buckets.cache_read_tokens_known = 0
  AND stats_usage_buckets.cache_write_tokens_sum = 0
  AND stats_usage_buckets.cache_write_tokens_known = 0
  AND stats_usage_buckets.reasoning_tokens_sum = 0
  AND stats_usage_buckets.reasoning_tokens_known = 0
  AND stats_usage_buckets.duration_ms_sum = 0
  AND stats_usage_buckets.duration_ms_count = 0
  AND stats_usage_buckets.first_token_ms_sum = 0
  AND stats_usage_buckets.first_token_ms_count = 0
  AND stats_usage_buckets.successful_attempt_count = 0
  AND stats_usage_buckets.successful_output_known_count = 0
  AND stats_usage_buckets.successful_duration_known_count = 0
  AND stats_usage_buckets.successful_output_sum = 0
  AND stats_usage_buckets.successful_duration_sum = 0;
DELETE FROM stats_usage_checkpoints
WHERE source_kind = 'run' AND source_id = NEW.id;
INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_run_source
WHERE source_id = NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER stats_usage_run_change AFTER INSERT OR UPDATE OR DELETE ON inference_run_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_run_change();

CREATE FUNCTION stats_usage_rejection_change() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        -- Reclaim work, but preserve permanent historical contribution.
        DELETE FROM stats_usage_checkpoints
        WHERE source_kind = 'rejection' AND source_id = OLD.id;
        RETURN OLD;
    END IF;
-- Drive the delta from this source checkpoint, never from the growing aggregate history.
UPDATE stats_usage_buckets SET
    request_count = stats_usage_buckets.request_count - c.request_count,
    error_count = stats_usage_buckets.error_count - c.error_count,
    input_tokens_sum = stats_usage_buckets.input_tokens_sum - c.input_tokens_sum,
    input_tokens_known = stats_usage_buckets.input_tokens_known - c.input_tokens_known,
    output_tokens_sum = stats_usage_buckets.output_tokens_sum - c.output_tokens_sum,
    output_tokens_known = stats_usage_buckets.output_tokens_known - c.output_tokens_known,
    cache_read_tokens_sum = stats_usage_buckets.cache_read_tokens_sum - c.cache_read_tokens_sum,
    cache_read_tokens_known = stats_usage_buckets.cache_read_tokens_known - c.cache_read_tokens_known,
    cache_write_tokens_sum = stats_usage_buckets.cache_write_tokens_sum - c.cache_write_tokens_sum,
    cache_write_tokens_known = stats_usage_buckets.cache_write_tokens_known - c.cache_write_tokens_known,
    reasoning_tokens_sum = stats_usage_buckets.reasoning_tokens_sum - c.reasoning_tokens_sum,
    reasoning_tokens_known = stats_usage_buckets.reasoning_tokens_known - c.reasoning_tokens_known,
    duration_ms_sum = stats_usage_buckets.duration_ms_sum - c.duration_ms_sum,
    duration_ms_count = stats_usage_buckets.duration_ms_count - c.duration_ms_count,
    first_token_ms_sum = stats_usage_buckets.first_token_ms_sum - c.first_token_ms_sum,
    first_token_ms_count = stats_usage_buckets.first_token_ms_count - c.first_token_ms_count,
    successful_attempt_count = stats_usage_buckets.successful_attempt_count - c.successful_attempt_count,
    successful_output_known_count = stats_usage_buckets.successful_output_known_count - c.successful_output_known_count,
    successful_duration_known_count = stats_usage_buckets.successful_duration_known_count - c.successful_duration_known_count,
    successful_output_sum = stats_usage_buckets.successful_output_sum - c.successful_output_sum,
    successful_duration_sum = stats_usage_buckets.successful_duration_sum - c.successful_duration_sum
FROM stats_usage_checkpoints AS c
WHERE c.source_kind = 'rejection' AND c.source_id = NEW.id
  AND c.scope = stats_usage_buckets.scope
  AND c.identity = stats_usage_buckets.identity
  AND c.bucket_start = stats_usage_buckets.bucket_start
  AND (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'rejection' AND c.source_id = NEW.id
);
DELETE FROM stats_usage_buckets
WHERE (stats_usage_buckets.scope, stats_usage_buckets.identity, stats_usage_buckets.bucket_start) IN (
    SELECT c.scope, c.identity, c.bucket_start
    FROM stats_usage_checkpoints AS c
    WHERE c.source_kind = 'rejection' AND c.source_id = NEW.id
)
  AND stats_usage_buckets.request_count = 0
  AND stats_usage_buckets.error_count = 0
  AND stats_usage_buckets.input_tokens_sum = 0
  AND stats_usage_buckets.input_tokens_known = 0
  AND stats_usage_buckets.output_tokens_sum = 0
  AND stats_usage_buckets.output_tokens_known = 0
  AND stats_usage_buckets.cache_read_tokens_sum = 0
  AND stats_usage_buckets.cache_read_tokens_known = 0
  AND stats_usage_buckets.cache_write_tokens_sum = 0
  AND stats_usage_buckets.cache_write_tokens_known = 0
  AND stats_usage_buckets.reasoning_tokens_sum = 0
  AND stats_usage_buckets.reasoning_tokens_known = 0
  AND stats_usage_buckets.duration_ms_sum = 0
  AND stats_usage_buckets.duration_ms_count = 0
  AND stats_usage_buckets.first_token_ms_sum = 0
  AND stats_usage_buckets.first_token_ms_count = 0
  AND stats_usage_buckets.successful_attempt_count = 0
  AND stats_usage_buckets.successful_output_known_count = 0
  AND stats_usage_buckets.successful_duration_known_count = 0
  AND stats_usage_buckets.successful_output_sum = 0
  AND stats_usage_buckets.successful_duration_sum = 0;
DELETE FROM stats_usage_checkpoints
WHERE source_kind = 'rejection' AND source_id = NEW.id;
INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_rejection_source
WHERE source_id = NEW.id;
    RETURN NEW;
END $$;
CREATE TRIGGER stats_usage_rejection_change AFTER INSERT OR UPDATE OR DELETE ON rejected_request_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_rejection_change();

-- Backfill retained facts before migration commits and admin readers can start.
INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_turn_source;

INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_attempt_source;

INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_run_source;

INSERT INTO stats_usage_checkpoints (
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
) SELECT
    source_kind,
    source_id,
    scope,
    identity,
    bucket_start,
    entity_id,
    label,
    request_count,
    error_count,
    input_tokens_sum,
    input_tokens_known,
    output_tokens_sum,
    output_tokens_known,
    cache_read_tokens_sum,
    cache_read_tokens_known,
    cache_write_tokens_sum,
    cache_write_tokens_known,
    reasoning_tokens_sum,
    reasoning_tokens_known,
    duration_ms_sum,
    duration_ms_count,
    first_token_ms_sum,
    first_token_ms_count,
    successful_attempt_count,
    successful_output_known_count,
    successful_duration_known_count,
    successful_output_sum,
    successful_duration_sum,
    last_used_at
FROM stats_usage_rejection_source;
