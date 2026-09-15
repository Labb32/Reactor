ALTER TABLE workspaces ADD COLUMN canonical_key TEXT;
ALTER TABLE workspaces ADD COLUMN policy_json TEXT NOT NULL DEFAULT '{"access":"sandbox"}';
UPDATE workspaces SET canonical_key = canonical_path WHERE canonical_key IS NULL;
CREATE UNIQUE INDEX workspaces_canonical_key ON workspaces(canonical_key);
