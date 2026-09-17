CREATE TABLE run_results (
    run_id TEXT PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE,
    completed_at TEXT NOT NULL,
    manifest_path TEXT NOT NULL,
    diff_path TEXT NOT NULL,
    summary_json TEXT NOT NULL
);
