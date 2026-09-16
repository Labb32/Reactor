UPDATE workspaces
SET policy_json = '{"writable_roots":[],"network":"deny","mcp":"deny","hooks":"deny"}',
    policy_fingerprint = '',
    status = 'DEGRADED';

UPDATE workspace_agents
SET status = 'DEGRADED',
    policy_fingerprint = NULL;
