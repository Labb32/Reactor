UPDATE workspaces
SET policy_json = '{"writable_roots":[],"network":"allow","mcp":"allow","hooks":"allow"}',
    policy_fingerprint = '',
    status = 'DEGRADED';

UPDATE workspace_agents
SET status = 'DEGRADED',
    policy_fingerprint = NULL;
