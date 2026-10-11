ALTER TABLE turn_chain_nodes ADD COLUMN prefix_projection_version INTEGER;
ALTER TABLE turn_chain_nodes ADD COLUMN prefix_controls_fingerprint TEXT;
ALTER TABLE turn_chain_nodes ADD COLUMN prefix_context_fingerprint TEXT;
