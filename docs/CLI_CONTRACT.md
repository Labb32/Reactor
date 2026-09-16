# CLI contract

Reactor commands use text output by default. Automation should pass `--output json`; event streams use `--output jsonl`.

## Envelope

```json
{
  "schema_version": "1.0",
  "instance_id": "01994ca0-0000-7000-8000-000000000000",
  "timestamp": "2026-09-15T06:00:00Z",
  "data": {}
}
```

- Field names use `snake_case`.
- IDs use UUID v7.
- Timestamps use RFC 3339 UTC.
- A minor schema release may add fields. Consumers must ignore unknown fields.
- A major schema release may change or remove fields.

## Errors

```json
{
  "schema_version": "1.0",
  "instance_id": null,
  "timestamp": "2026-09-15T06:00:00Z",
  "error": {
    "code": "uninitialized_database",
    "message": "database has not been initialized; run `reactor init`",
    "details": {},
    "retryable": false
  }
}
```

| Exit code | Meaning |
|---|---|
| `0` | Success |
| `1` | Runtime, I/O, configuration, or database failure |
| `2` | Invalid command input |
| `3` | Database initialization or schema mismatch |
| `4` | State, cursor, or idempotency conflict |

JSON responses go to stdout. Human diagnostics go to stderr.

## Input

Commands that accept a document use `--file <path>` or `--file -` for stdin. `.json`, `.yaml`, and `.yml` are recognized; stdin and unknown extensions try JSON followed by YAML.

Mutation commands accept an `idempotency_key`. Reusing a key with the same request returns the stored response. Reusing it with different input returns `idempotency_conflict`.

## Workspaces

```shell
reactor workspace add <path> --id <workspace_id> [--file <policy>] [--idempotency-key <key>]
reactor workspace prepare <workspace_id> --agent codex [--idempotency-key <key>]
reactor workspace status <workspace_id>
reactor workspace list
```

Only `workspace add` accepts a raw path. The Workspace is always writable. A policy may add `writable_roots` and set `network`, `mcp`, or `hooks` to `allow` or `deny`; access defaults to `allow`. Codex preparation requires version 0.154.0 or later and records only readiness metadata.

Repeating `workspace add` with the same ID and Canonical Path updates its policy, invalidates prior Agent readiness, and requires `workspace prepare` again.

Scheduled Agent commands never request interactive approval. Reactor applies the Workspace policy through the Agent sandbox and rejects unsupported permissions. Existing user credentials remain owned by the Agent and external CLI; Reactor does not store their values.

## Plans

```shell
reactor plan create --file <plan.yaml> [--idempotency-key <key>]
reactor plan draft --workspace <workspace_id> --agent <agent> --file <context> [--idempotency-key <key>]
reactor plan show <plan_id> [--version <number>]
reactor plan list
reactor plan revise <plan_id> --file <plan.yaml> [--idempotency-key <key>]
reactor plan review <plan_id> [--idempotency-key <key>]
reactor plan approve <plan_id> [--approved-by <name>] [--idempotency-key <key>]
```

`plan create` stores a reviewed JSON or YAML document without running an Agent and defaults to `requirements: inherit`. `plan draft` reads plain text or Markdown and optionally asks an Agent to produce a draft and expected requirements. Draft generation uses a read-only sandbox with Network, MCP, Hooks, and user configuration disabled. Its output still requires user review. An approved Version is immutable and may be reused by recurring Schedules.

```yaml
workspace_id: example
title: Update dependencies
agent: codex
context: Update compatible dependencies and report the result.
requirements: inherit
completion_criteria:
  - Report changed files and unresolved issues
limits:
  max_wall_time_seconds: 7200
  max_attempts: 1
```

Explicit requirements may contain `writable_roots`, `network`, `mcp`, `hooks`, and `tools`. They describe the minimum expected environment and never expand the Workspace policy. Tool availability and current Workspace readiness are checked before approval.

## Events

```shell
reactor events --after <cursor> --output jsonl
reactor events --after <cursor> --follow --output jsonl
```

Each line is an independent envelope. Cursors increase within one Reactor instance. Events are retained for 90 days or 100,000 entries. An expired cursor returns `cursor_expired`; the client must fetch a new snapshot.
