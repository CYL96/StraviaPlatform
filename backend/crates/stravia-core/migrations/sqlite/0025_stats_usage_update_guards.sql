-- 仅在统计 source view 的依赖列实际变化时重算 checkpoint。
-- 保留既有增量扣除/记入与删除回收语义，不重建历史汇总。
-- turn: 身份、标签、时间桶及终态耗时；attempt: 用量、性能及归属。
-- attempt 的 run_id/provider_id/provider_name 还影响 run 的 Provider 请求汇总。
-- run: 时间桶和失败判定；rejection: 开始时间回退及失败判定。
-- 比较必须 NULL-safe：未知用量转为已知（包括零）也是真实统计变化。

DROP TRIGGER stats_usage_turn_update;
CREATE TRIGGER stats_usage_turn_update
AFTER UPDATE OF id, route_id, model_display_name, api_key_id, api_key_name, started_at, finished_at ON model_turn_observations
WHEN OLD.id IS NOT NEW.id
    OR OLD.route_id IS NOT NEW.route_id
    OR OLD.model_display_name IS NOT NEW.model_display_name
    OR OLD.api_key_id IS NOT NEW.api_key_id
    OR OLD.api_key_name IS NOT NEW.api_key_name
    OR OLD.started_at IS NOT NEW.started_at
    OR OLD.finished_at IS NOT NEW.finished_at
BEGIN
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
END;

DROP TRIGGER stats_usage_attempt_update;
CREATE TRIGGER stats_usage_attempt_update
AFTER UPDATE OF id, model_turn_id, run_id, provider_id, provider_name, status, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, first_token_ms, duration_ms ON target_attempt_observations
WHEN OLD.id IS NOT NEW.id
    OR OLD.model_turn_id IS NOT NEW.model_turn_id
    OR OLD.run_id IS NOT NEW.run_id
    OR OLD.provider_id IS NOT NEW.provider_id
    OR OLD.provider_name IS NOT NEW.provider_name
    OR OLD.status IS NOT NEW.status
    OR OLD.input_tokens IS NOT NEW.input_tokens
    OR OLD.output_tokens IS NOT NEW.output_tokens
    OR OLD.cache_read_tokens IS NOT NEW.cache_read_tokens
    OR OLD.cache_write_tokens IS NOT NEW.cache_write_tokens
    OR OLD.reasoning_tokens IS NOT NEW.reasoning_tokens
    OR OLD.first_token_ms IS NOT NEW.first_token_ms
    OR OLD.duration_ms IS NOT NEW.duration_ms
BEGIN
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
END;

DROP TRIGGER stats_usage_run_update;
CREATE TRIGGER stats_usage_run_update
AFTER UPDATE OF id, started_at, status, finished_at, failure_json, terminal_reason ON inference_run_observations
WHEN OLD.id IS NOT NEW.id
    OR OLD.started_at IS NOT NEW.started_at
    OR OLD.status IS NOT NEW.status
    OR OLD.finished_at IS NOT NEW.finished_at
    OR OLD.failure_json IS NOT NEW.failure_json
    OR OLD.terminal_reason IS NOT NEW.terminal_reason
BEGIN
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
END;

DROP TRIGGER stats_usage_rejection_update;
CREATE TRIGGER stats_usage_rejection_update
AFTER UPDATE OF id, started_at, occurred_at, status_code, code ON rejected_request_observations
WHEN OLD.id IS NOT NEW.id
    OR OLD.started_at IS NOT NEW.started_at
    OR OLD.occurred_at IS NOT NEW.occurred_at
    OR OLD.status_code IS NOT NEW.status_code
    OR OLD.code IS NOT NEW.code
BEGIN
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
END;

