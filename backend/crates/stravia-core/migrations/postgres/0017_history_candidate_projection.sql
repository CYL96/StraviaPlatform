ALTER TABLE turn_chain_nodes ADD COLUMN prefix_projection_version integer;
ALTER TABLE turn_chain_nodes ADD COLUMN prefix_controls_fingerprint text;
ALTER TABLE turn_chain_nodes ADD COLUMN prefix_context_fingerprint text;
