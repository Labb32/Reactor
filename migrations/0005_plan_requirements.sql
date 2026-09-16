ALTER TABLE plan_versions ADD COLUMN provider TEXT;
ALTER TABLE plan_versions ADD COLUMN requirements_json TEXT NOT NULL DEFAULT '"inherit"';
ALTER TABLE plan_versions ADD COLUMN workspace_policy_fingerprint TEXT NOT NULL DEFAULT '';
