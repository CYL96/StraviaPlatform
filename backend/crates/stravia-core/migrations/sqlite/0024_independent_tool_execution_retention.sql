-- Preserve only results observable under the old contract before dropping it.
DELETE FROM public_tool_executions WHERE id NOT IN (
    SELECT p.id FROM public_tool_execution_lifetimes p
    JOIN history_markers h ON h.reference = p.marker_reference
    WHERE p.retained_until > CAST(strftime('%s', 'now') AS BIGINT) * 1000
      AND (h.expires_at > CAST(strftime('%s', 'now') AS BIGINT) * 1000 OR p.source_kind = 'inference_run')
      AND h.execution_fenced = 0
);
DROP TRIGGER remove_source_tool_executions;
DROP VIEW public_tool_execution_lifetimes;
DROP TABLE tool_execution_sources;
DROP TABLE removed_tool_executions;
ALTER TABLE history_markers DROP COLUMN execution_fenced;
-- A running execution has no result TTL. Its original deadline still applies.
-- Zero is only a representation during upgrade; NULL is used in the new table.
ALTER TABLE public_tool_executions RENAME TO legacy_public_tool_executions;
CREATE TABLE public_tool_executions (
    id TEXT PRIMARY KEY,
    marker_reference TEXT NOT NULL UNIQUE,
    principal TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('mcp', 'inference_run')),
    source_run_id TEXT,
    process_id TEXT NOT NULL,
    handed_off_at BIGINT,
    expires_at BIGINT,
    retention_ms BIGINT NOT NULL,
    terminal_at BIGINT,
    retention_started_at BIGINT,
    CHECK ((source_kind = 'mcp' AND source_run_id IS NULL) OR (source_kind = 'inference_run' AND source_run_id IS NOT NULL))
);
INSERT INTO public_tool_executions
SELECT p.id, p.marker_reference, p.principal, p.source_kind, p.source_run_id,
       p.process_id, p.handed_off_at,
       CASE WHEN h.execution_state IN ('completed','failed','interrupted')
            THEN CAST(strftime('%s', 'now') AS BIGINT) * 1000 + COALESCE((SELECT CAST(value AS BIGINT) * 86400000 FROM settings WHERE name = 'log_retention_days'), 604800000) END,
       COALESCE((SELECT CAST(value AS BIGINT) * 86400000 FROM settings WHERE name = 'log_retention_days'), 604800000),
       NULL,
       CASE WHEN h.execution_state IN ('completed','failed','interrupted')
            THEN CAST(strftime('%s', 'now') AS BIGINT) * 1000 END
FROM legacy_public_tool_executions p JOIN history_markers h ON h.reference = p.marker_reference;
DROP TABLE legacy_public_tool_executions;
CREATE INDEX public_tool_executions_source ON public_tool_executions(source_run_id);
CREATE INDEX public_tool_executions_expiry ON public_tool_executions(expires_at);
-- updated_at is captured by the terminal write, not by subsequent publication.
CREATE TRIGGER retain_terminal_tool_execution AFTER UPDATE OF execution_state ON history_markers
WHEN OLD.execution_state IN ('pending','running') AND NEW.execution_state IN ('completed','failed','interrupted')
BEGIN
    UPDATE public_tool_executions
    SET terminal_at = NEW.updated_at,
        retention_started_at = NEW.updated_at,
        retention_ms = COALESCE((SELECT CAST(value AS BIGINT) * 86400000 FROM settings WHERE name = 'log_retention_days'), retention_ms),
        expires_at = NEW.updated_at + COALESCE((SELECT CAST(value AS BIGINT) * 86400000 FROM settings WHERE name = 'log_retention_days'), retention_ms)
    WHERE marker_reference = NEW.reference AND retention_started_at IS NULL;
END;
