-- Custom domain per project (e.g., "myapp.com" pointing to the project)
ALTER TABLE projects ADD COLUMN custom_domain TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_custom_domain
    ON projects(custom_domain) WHERE custom_domain IS NOT NULL;

-- RAM limit in MB (NULL = no limit)
ALTER TABLE projects ADD COLUMN ram_limit_mb INTEGER;
