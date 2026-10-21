-- 仅在统计 source view 的依赖列实际变化时重算 checkpoint。
-- 保留既有增量扣除/记入与删除回收语义，不重建历史汇总。
-- turn: 身份、标签、时间桶及终态耗时；attempt: 用量、性能及归属。
-- attempt 的 run_id/provider_id/provider_name 还影响 run 的 Provider 请求汇总。
-- run: 时间桶和失败判定；rejection: 开始时间回退及失败判定。
-- 比较必须 NULL-safe：未知用量转为已知（包括零）也是真实统计变化。

DROP TRIGGER stats_usage_turn_change ON model_turn_observations;
CREATE TRIGGER stats_usage_turn_change AFTER INSERT OR DELETE ON model_turn_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_turn_change();
CREATE TRIGGER stats_usage_turn_update
AFTER UPDATE OF id, route_id, model_display_name, api_key_id, api_key_name, started_at, finished_at ON model_turn_observations
FOR EACH ROW WHEN (
    OLD.id IS DISTINCT FROM NEW.id
    OR OLD.route_id IS DISTINCT FROM NEW.route_id
    OR OLD.model_display_name IS DISTINCT FROM NEW.model_display_name
    OR OLD.api_key_id IS DISTINCT FROM NEW.api_key_id
    OR OLD.api_key_name IS DISTINCT FROM NEW.api_key_name
    OR OLD.started_at IS DISTINCT FROM NEW.started_at
    OR OLD.finished_at IS DISTINCT FROM NEW.finished_at
) EXECUTE FUNCTION stats_usage_turn_change();

DROP TRIGGER stats_usage_attempt_change ON target_attempt_observations;
CREATE TRIGGER stats_usage_attempt_change AFTER INSERT OR DELETE ON target_attempt_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_attempt_change();
CREATE TRIGGER stats_usage_attempt_update
AFTER UPDATE OF id, model_turn_id, run_id, provider_id, provider_name, status, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, first_token_ms, duration_ms ON target_attempt_observations
FOR EACH ROW WHEN (
    OLD.id IS DISTINCT FROM NEW.id
    OR OLD.model_turn_id IS DISTINCT FROM NEW.model_turn_id
    OR OLD.run_id IS DISTINCT FROM NEW.run_id
    OR OLD.provider_id IS DISTINCT FROM NEW.provider_id
    OR OLD.provider_name IS DISTINCT FROM NEW.provider_name
    OR OLD.status IS DISTINCT FROM NEW.status
    OR OLD.input_tokens IS DISTINCT FROM NEW.input_tokens
    OR OLD.output_tokens IS DISTINCT FROM NEW.output_tokens
    OR OLD.cache_read_tokens IS DISTINCT FROM NEW.cache_read_tokens
    OR OLD.cache_write_tokens IS DISTINCT FROM NEW.cache_write_tokens
    OR OLD.reasoning_tokens IS DISTINCT FROM NEW.reasoning_tokens
    OR OLD.first_token_ms IS DISTINCT FROM NEW.first_token_ms
    OR OLD.duration_ms IS DISTINCT FROM NEW.duration_ms
) EXECUTE FUNCTION stats_usage_attempt_change();

DROP TRIGGER stats_usage_run_change ON inference_run_observations;
CREATE TRIGGER stats_usage_run_change AFTER INSERT OR DELETE ON inference_run_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_run_change();
CREATE TRIGGER stats_usage_run_update
AFTER UPDATE OF id, started_at, status, finished_at, failure_json, terminal_reason ON inference_run_observations
FOR EACH ROW WHEN (
    OLD.id IS DISTINCT FROM NEW.id
    OR OLD.started_at IS DISTINCT FROM NEW.started_at
    OR OLD.status IS DISTINCT FROM NEW.status
    OR OLD.finished_at IS DISTINCT FROM NEW.finished_at
    OR OLD.failure_json IS DISTINCT FROM NEW.failure_json
    OR OLD.terminal_reason IS DISTINCT FROM NEW.terminal_reason
) EXECUTE FUNCTION stats_usage_run_change();

DROP TRIGGER stats_usage_rejection_change ON rejected_request_observations;
CREATE TRIGGER stats_usage_rejection_change AFTER INSERT OR DELETE ON rejected_request_observations
FOR EACH ROW EXECUTE FUNCTION stats_usage_rejection_change();
CREATE TRIGGER stats_usage_rejection_update
AFTER UPDATE OF id, started_at, occurred_at, status_code, code ON rejected_request_observations
FOR EACH ROW WHEN (
    OLD.id IS DISTINCT FROM NEW.id
    OR OLD.started_at IS DISTINCT FROM NEW.started_at
    OR OLD.occurred_at IS DISTINCT FROM NEW.occurred_at
    OR OLD.status_code IS DISTINCT FROM NEW.status_code
    OR OLD.code IS DISTINCT FROM NEW.code
) EXECUTE FUNCTION stats_usage_rejection_change();

