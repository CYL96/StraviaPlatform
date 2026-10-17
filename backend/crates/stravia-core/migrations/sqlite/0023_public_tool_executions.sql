-- Source fences are deliberately independent of diagnostic row availability.
ALTER TABLE history_markers ADD COLUMN execution_fenced INTEGER NOT NULL DEFAULT 0;
CREATE TABLE tool_execution_sources (source_run_id TEXT PRIMARY KEY, removed INTEGER NOT NULL DEFAULT 0, expires_at BIGINT NOT NULL DEFAULT 0, fence_until BIGINT NOT NULL DEFAULT 0);
CREATE TABLE public_tool_executions (
    id TEXT PRIMARY KEY,
    marker_reference TEXT NOT NULL UNIQUE,
    principal TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK (source_kind IN ('mcp', 'inference_run')),
    source_run_id TEXT,
    process_id TEXT NOT NULL,
    handed_off_at BIGINT,
    expires_at BIGINT NOT NULL,
    CHECK ((source_kind = 'mcp' AND source_run_id IS NULL) OR (source_kind = 'inference_run' AND source_run_id IS NOT NULL))
);
CREATE INDEX public_tool_executions_source ON public_tool_executions(source_run_id);
-- A retained real source (including an active one) owns its lifetime until
-- observation cleanup deletes it. Missing diagnostics use the first source
-- association's retention, never a fresh lifetime per tool or query.
CREATE VIEW public_tool_execution_lifetimes AS
SELECT p.*,
    CASE WHEN p.source_kind = 'mcp' THEN p.expires_at
         WHEN EXISTS (SELECT 1 FROM inference_run_observations r WHERE r.id = p.source_run_id) THEN 9223372036854775807
         ELSE s.expires_at END AS retained_until
FROM public_tool_executions p LEFT JOIN tool_execution_sources s ON s.source_run_id = p.source_run_id;
CREATE TABLE removed_tool_executions (id TEXT PRIMARY KEY);
CREATE TRIGGER remove_source_tool_executions BEFORE DELETE ON inference_run_observations BEGIN
    -- Both public tools have a 900s absolute acceptance deadline. Keep even an
    -- as-yet unassociated source fenced until every pre-delete admission must
    -- have expired. Admission checks that original deadline, not a fresh TTL.
    -- Changing public execution_limit requires updating this safety boundary.
    -- 901s rounds the second-resolution SQLite clock conservatively upward.
    INSERT INTO tool_execution_sources(source_run_id, removed, fence_until)
        VALUES (OLD.id, 1, MAX(CAST(strftime('%s', 'now') AS BIGINT) * 1000 + 901000,
            COALESCE((SELECT MAX(MAX(h.execution_deadline, COALESCE(h.lease_expires_at, 0)))
                FROM history_markers h JOIN public_tool_executions p ON p.marker_reference = h.reference
                WHERE p.source_run_id = OLD.id), 0)))
        ON CONFLICT(source_run_id) DO UPDATE SET removed = 1, fence_until = excluded.fence_until;
    INSERT INTO removed_tool_executions(id) SELECT id FROM public_tool_executions WHERE source_run_id = OLD.id ON CONFLICT DO NOTHING;
    UPDATE history_markers SET execution_fenced = 1, execution_owner = NULL, lease_expires_at = 0,
        execution_state = CASE WHEN execution_state = 'pending' THEN 'running' ELSE execution_state END
        WHERE reference IN (SELECT marker_reference FROM public_tool_executions WHERE source_run_id = OLD.id);
    DELETE FROM public_tool_executions WHERE source_run_id = OLD.id;
END;
