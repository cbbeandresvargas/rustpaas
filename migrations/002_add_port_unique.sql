-- Add UNIQUE constraint on projects.port to prevent duplicate port allocation
-- under concurrent deploy requests
CREATE UNIQUE INDEX IF NOT EXISTS idx_projects_port ON projects(port);
