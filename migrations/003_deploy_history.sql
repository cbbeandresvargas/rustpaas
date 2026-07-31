-- Deploy history: keeps the last N binary snapshots for rollback
CREATE TABLE IF NOT EXISTS deploy_history (
    id          TEXT PRIMARY KEY NOT NULL,
    project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    binary_path TEXT NOT NULL,
    is_bundle   INTEGER NOT NULL DEFAULT 0,  -- 1 if the saved file is a tar.gz bundle
    deployed_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_deploy_history_project ON deploy_history(project_id, deployed_at);
