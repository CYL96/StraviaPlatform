-- Historical Routes deliberately remain unconfigured until explicitly repaired.
ALTER TABLE models ADD COLUMN output_modality TEXT
    CHECK (output_modality IS NULL OR output_modality IN ('text', 'image'));
