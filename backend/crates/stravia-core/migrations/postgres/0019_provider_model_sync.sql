CREATE TABLE provider_model_sync_state (
    provider_id TEXT PRIMARY KEY REFERENCES providers(id) ON DELETE CASCADE,
    automatic_sync BOOLEAN NOT NULL DEFAULT TRUE,
    has_successful_sync BOOLEAN NOT NULL DEFAULT FALSE,
    last_check TEXT
);
INSERT INTO provider_model_sync_state (provider_id, has_successful_sync)
SELECT id, TRUE FROM providers;

-- History intentionally has no Provider foreign key: snapshots survive deletion.
CREATE TABLE model_changes (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    checked_at TEXT NOT NULL,
    source TEXT NOT NULL,
    record_json TEXT NOT NULL
);
CREATE INDEX model_changes_order ON model_changes(checked_at DESC, id DESC);
CREATE INDEX model_changes_provider_order ON model_changes(provider_id, checked_at DESC, id DESC);
CREATE TABLE model_change_items (
    record_id TEXT NOT NULL REFERENCES model_changes(id) ON DELETE CASCADE,
    name_key TEXT NOT NULL,
    model_id_key TEXT NOT NULL,
    change_type TEXT NOT NULL
);
CREATE INDEX model_change_items_record ON model_change_items(record_id, change_type);
