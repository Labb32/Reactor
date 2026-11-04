ALTER TABLE plan_versions ADD COLUMN tasks_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE runs ADD COLUMN task_index INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN execution_path TEXT;
ALTER TABLE attempts ADD COLUMN task_index INTEGER NOT NULL DEFAULT 0;
CREATE TABLE run_tasks (
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    task_index INTEGER NOT NULL,
    task_id TEXT NOT NULL,
    state TEXT NOT NULL,
    warnings_json TEXT NOT NULL DEFAULT '[]',
    response TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(run_id, task_index)
);
