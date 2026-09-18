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

## Initialization and daemon

```shell
reactor init [--file <config.json|yaml>] [--no-autostart]
reactor daemon run [--config-dir <path> --data-dir <path>]
reactor service install|start|stop|status|uninstall
```

By default, `init` registers the current executable for user login and starts the daemon. `--no-autostart` initializes configuration and storage without changing service registration. Service registration is scoped by Reactor instance ID and uses Task Scheduler on Windows, `systemd --user` on Linux, and a LaunchAgent on macOS; it never requires a system account or administrator registration.

The daemon uses an OS file lock to reject duplicate instances and runs up to the configured global concurrency. Concurrency values accept `1..=64`. Restart recovery changes interrupted Runs to `REVIEW_REQUIRED` and preserves their logs and pending diff. `service uninstall` stops and removes login startup without deleting Reactor data.

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
reactor plan create --file <plan.yaml|-> [--idempotency-key <key>]
reactor plan create --workspace <workspace_id> --context <text|-> [--title <title>] [--agent <agent>] [--idempotency-key <key>]
reactor plan draft --workspace <workspace_id> --agent <agent> --file <context> [--idempotency-key <key>]
reactor plan show <plan_id> [--version <number>]
reactor plan list
reactor plan revise <plan_id> --file <plan.yaml> [--idempotency-key <key>]
reactor plan review <plan_id> [--idempotency-key <key>]
reactor plan approve <plan_id> [--approved-by <name>] [--idempotency-key <key>]
```

`plan create` stores reviewed input without running an Agent. Simple input defaults to Codex, inherited requirements, a 7,200 second wall-time limit, and one Attempt. `--context -` reads plain text from stdin. `plan draft` asks an Agent to produce a draft and expected requirements. Draft generation uses a read-only sandbox with Network, MCP, Hooks, and user configuration disabled. Its output still requires user review. An approved Version is immutable and may be reused by recurring Schedules.

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
  max_tokens: 100000
```

Explicit requirements may contain `writable_roots`, `network`, `mcp`, `hooks`, and `tools`. They describe the minimum expected environment and never expand the Workspace policy. Tool availability and current Workspace readiness are checked before approval.

`max_wall_time_seconds` accepts 1 second through 7 days. `max_attempts` accepts 1 through 10 and bounds explicit retries across replacement Runs. Reactor does not move a terminal Run back into execution. `max_tokens` is optional. It uses usage reported by the Agent after exit and ends the Run as `LIMIT_REACHED` with reason `token_limit` when exceeded.

## Schedules and queue

```shell
reactor schedule add --file <schedule.yaml|-> [--idempotency-key <key>]
reactor schedule add <plan_id> (--immediate | --manual | --at <local_time>) [--version <number>] [--start-deadline-seconds <seconds>] [--local-endpoint <name>] [--idempotency-key <key>]
reactor schedule show <schedule_id>
reactor schedule list
reactor schedule pause <schedule_id> [--idempotency-key <key>]
reactor schedule resume <schedule_id> [--idempotency-key <key>]
reactor schedule trigger <schedule_id> [--idempotency-key <key>]
reactor queue list
```

A schedule pins an approved Plan Version. Omitting `plan_version` selects the current approved version. `start_deadline_seconds` defaults to one hour.

```yaml
plan_id: 01994ca0-0000-7000-8000-000000000000
rule:
  type: at
  local_time: 2026-09-20T09:00:00
start_deadline_seconds: 3600
```

Rule types are `immediate`, `manual`, `at`, `daily`, `weekdays`, `weekly`, and `interval_after_success`. Calendar rules use `local_time`; `weekly` also uses a full English `weekday`. `interval_after_success` uses `seconds` and runs once immediately before applying the interval.

Local-time rules use the current user time zone captured when the schedule is created. Reactor stores its IANA name when available and otherwise stores the current UTC offset. A DST gap moves to the next valid local time; a repeated local time selects its first occurrence. Reactor retains one pending future occurrence. A due occurrence past its deadline becomes `MISSED`, then a calendar rule advances directly to its next future occurrence.

`after_quota_reset: true` delays an occurrence until the selected Agent and Provider report a reset time. Schedule creation fails when that information is unavailable; the current Codex adapter does not expose this Gate.

`queue list` refreshes due occurrences and reports each queued Run with `eligible`, `dispatch_order`, and `blocked_by`. A Run that remains queued past its start deadline becomes `MISSED`. Dispatch uses Workspace FIFO, Workspace round-robin, one active Run per Workspace, and the configured global, Agent, Provider, and local endpoint limits.

## Runs

```shell
reactor run next
reactor run show <run_id>
reactor run progress <run_id>
reactor run result <run_id>
reactor run diff <run_id>
reactor run list
reactor run cancel <run_id> [--idempotency-key <key>]
reactor run retry <run_id> [--idempotency-key <key>]
```

`run next` claims and executes one eligible Run. It blocks until the Agent exits, the wall-time limit is reached, or another process requests cancellation. Cancellation stops the Agent process tree, waits up to three seconds, collects the remaining output, and then records `CANCELLED`.

`run retry` accepts `FAILED`, `CANCELLED`, `LIMIT_REACHED`, and `REVIEW_REQUIRED` while Attempt budget remains. It leaves the source Run unchanged and queues one replacement Run with the same approved Plan Version and remaining execution limits. Further retries continue from the latest replacement Run.

Attempt logs are stored under Reactor's data directory, rotate at 10 MiB, retain at most five files, and expire after 30 days. Sandbox permission failures become `REVIEW_REQUIRED`; Reactor does not widen the approved policy automatically.

Each claimed Run records its Workspace and writable roots before and after execution. Text files up to 1 MiB receive a unified diff; binary and larger files retain hashes and sizes. A snapshot accepts 250,000 files and embeds up to 64 MiB of text; remaining text retains hashes and sizes. `.gitignore` and `.reactorignore` apply, and symlinks are not followed. Only the latest diff is retained per Workspace.

At startup, the service changes interrupted `PREPARING` or `RUNNING` Runs with expired execution leases to `REVIEW_REQUIRED` and completes their pending diff. It never reattaches to or restarts an Agent process.

## Events

```shell
reactor events --after <cursor> --output jsonl
reactor events --after <cursor> --follow --output jsonl
```

Each line is an independent envelope. Cursors increase within one Reactor instance. Events are retained for 90 days or 100,000 entries. An expired cursor returns `cursor_expired`; the client must fetch a new snapshot.
