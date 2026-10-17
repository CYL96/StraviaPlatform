-- Preserve pre-type models as text models without overwriting explicit choices.
UPDATE models SET output_modality = 'text' WHERE output_modality IS NULL;
