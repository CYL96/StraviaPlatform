DELETE FROM public_tool_executions WHERE id NOT IN (
    SELECT p.id FROM public_tool_execution_lifetimes p
    JOIN history_markers h ON h.reference = p.marker_reference
    WHERE p.retained_until > (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) * 1000)::BIGINT
      AND (h.expires_at > (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) * 1000)::BIGINT OR p.source_kind = 'inference_run')
      AND h.execution_fenced = 0
);
DROP TRIGGER remove_source_tool_executions ON inference_run_observations;
DROP FUNCTION remove_source_tool_executions();
DROP VIEW public_tool_execution_lifetimes;
DROP TABLE tool_execution_sources;
DROP TABLE removed_tool_executions;
ALTER TABLE history_markers DROP COLUMN execution_fenced;
ALTER TABLE public_tool_executions ALTER COLUMN expires_at DROP NOT NULL;
ALTER TABLE public_tool_executions ADD COLUMN retention_ms BIGINT NOT NULL DEFAULT 604800000;
ALTER TABLE public_tool_executions ADD COLUMN terminal_at BIGINT;
ALTER TABLE public_tool_executions ADD COLUMN retention_started_at BIGINT;
UPDATE public_tool_executions p SET
    retention_ms = COALESCE((SELECT value::BIGINT * 86400000 FROM settings WHERE name = 'log_retention_days'), 604800000),
    retention_started_at = CASE WHEN h.execution_state IN ('completed','failed','interrupted')
        THEN (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) * 1000)::BIGINT END,
    expires_at = CASE WHEN h.execution_state IN ('completed','failed','interrupted')
        THEN (EXTRACT(EPOCH FROM CURRENT_TIMESTAMP) * 1000)::BIGINT + COALESCE((SELECT value::BIGINT * 86400000 FROM settings WHERE name = 'log_retention_days'), 604800000) END
FROM history_markers h WHERE h.reference = p.marker_reference;
ALTER TABLE public_tool_executions ALTER COLUMN retention_ms DROP DEFAULT;
CREATE INDEX public_tool_executions_expiry ON public_tool_executions(expires_at);
CREATE FUNCTION retain_terminal_tool_execution() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.execution_state IN ('pending','running') AND NEW.execution_state IN ('completed','failed','interrupted') THEN
        UPDATE public_tool_executions
        SET terminal_at = NEW.updated_at,
            retention_started_at = NEW.updated_at,
            retention_ms = COALESCE((SELECT value::BIGINT * 86400000 FROM settings WHERE name = 'log_retention_days'), retention_ms),
            expires_at = NEW.updated_at + COALESCE((SELECT value::BIGINT * 86400000 FROM settings WHERE name = 'log_retention_days'), retention_ms)
        WHERE marker_reference = NEW.reference AND retention_started_at IS NULL;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER retain_terminal_tool_execution AFTER UPDATE OF execution_state ON history_markers
    FOR EACH ROW EXECUTE FUNCTION retain_terminal_tool_execution();
