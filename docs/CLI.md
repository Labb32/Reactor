# CLI guide

Reactor schedules reviewed work for Codex, Qwen Code, and Claude Code. Commands are non-interactive and can be used by people or Agents.

## Basics

```shell
reactor [--output text|json|jsonl] <command>
reactor --help
reactor help <command>
reactor --version
```

- `text` is the default. Times use the current local time zone.
- `json` returns a versioned envelope and keeps timestamps in UTC.
- `jsonl` is intended for `events --follow`.
- `NO_COLOR=1` disables status colors.
- `--file -` reads stdin. Plan drafts read plain text or Markdown; other document inputs use JSON or YAML.
- Mutation commands accept `--idempotency-key <KEY>` for safe retries. Keys contain 1–128 bytes without control characters.

## Quick start

```shell
reactor init
reactor workspace add ./project --id project
reactor workspace prepare project --agent codex
reactor plan create --name update-deps --workspace project --context "Update dependencies."
reactor plan review update-deps
reactor plan approve update-deps
reactor schedule add update-deps --immediate
reactor run list
```

`init` installs and starts the per-user daemon. Add `--no-autostart` to initialize files only.

## Commands

### Core

| Command | Arguments | Purpose |
|---|---|---|
| `init` | `--file <PATH>`, `--no-autostart` | Create configuration and storage; optionally read configuration from a document. |
| `info` | — | Show version, schema, capabilities, and concurrency. |
| `snapshot` | — | Show Workspace, active Run, Queue, and event summary. |
| `events` | `--after <CURSOR>` (default `0`), `--limit <1..10000>` (default `1000`), `--follow` | Read audit events. `--follow` requires `--output jsonl`. |

### Service and daemon

| Command | Arguments | Purpose |
|---|---|---|
| `service install` | — | Register per-user login startup. |
| `service start` | — | Start the registered daemon. |
| `service stop` | — | Stop the daemon. |
| `service status` | — | Show installation and running state. |
| `service uninstall` | — | Remove login startup while keeping Reactor data. |
| `daemon run` | `[--config-dir <PATH> --data-dir <PATH>]` | Run in the foreground. Both path options must be used together. |

### Workspace

| Command | Arguments | Purpose |
|---|---|---|
| `workspace add <PATH>` | `--id <ID>`, `--file <POLICY>`, `--idempotency-key <KEY>` | Register a directory and optional sandbox policy. |
| `workspace prepare <ID>` | `--agent codex|qwen|claude`, `--idempotency-key <KEY>` | Check Agent version, authentication, trust, and sandbox support. |
| `workspace status <ID>` | — | Refresh and show readiness. |
| `workspace list` | — | List registered Workspaces. |

Workspace IDs start with an ASCII letter or number and contain at most 64 letters, numbers, `.`, `-`, or `_`.

- `--id` sets the Workspace ID.
- `--file` supplies a JSON or YAML sandbox policy.
- `--agent` selects the Agent to prepare; the default is `codex`.

### Plan

| Command | Arguments | Purpose |
|---|---|---|
| `plan create` | `--name <NAME>`, `--file <PATH>`, `--idempotency-key <KEY>` | Create a Plan from a document. |
| `plan create` | `--name <NAME>`, `--workspace <ID>`, `--context <TEXT|->`, `--title <TEXT>`, `--agent <AGENT>`, `--provider <NAME>`, `--model <NAME>`, `--max-wall-time-seconds <N>`, `--max-attempts <N>`, `--max-tokens <N>`, `--idempotency-key <KEY>` | Create a simple Plan without a file. |
| `plan draft` | `--name <NAME>`, `--workspace <ID>`, `--agent <AGENT>`, `--file <PATH|->`, `--provider <NAME>`, `--model <NAME>`, `--max-wall-time-seconds <N>`, `--max-attempts <N>`, `--max-tokens <N>`, `--draft-timeout-seconds <1..3600>`, `--idempotency-key <KEY>` | Ask an Agent for a read-only draft. |
| `plan revise <PLAN>` | `--file <PATH|->`, `--idempotency-key <KEY>` | Add a new immutable Plan version. |
| `plan show <PLAN>` | `--version <N>` | Show the current or selected version. |
| `plan list` | — | List current Plan versions. |
| `plan review <PLAN>` | `--idempotency-key <KEY>` | Move a Draft to review and display its contents. Approved Plans are displayed without changing state. |
| `plan approve <PLAN>` | `--approved-by <NAME>`, `--idempotency-key <KEY>` | Approve the current reviewed version. |

`--name` creates a short Plan identifier such as `update-deps`; otherwise Reactor generates an ID. A name follows the same character rules as a Workspace ID.

`--file` cannot be combined with the simple Plan fields. The document schema is defined in [CLI_CONTRACT.md](CLI_CONTRACT.md).

- `--workspace` selects the registered Workspace.
- `--context` supplies the task text; `-` reads plain text from stdin.
- `--title` overrides the title derived from the first Context line.
- `--agent` selects `codex`, `qwen`, or `claude`; the default is `codex`.
- `--provider` and `--model` pass optional routing metadata to the Agent.
- `--max-wall-time-seconds`, `--max-attempts`, and `--max-tokens` set execution limits. Defaults are 7,200 seconds, one Attempt, and no token limit.
- `--draft-timeout-seconds` limits draft generation; the default is 300 seconds.
- `--approved-by` records the approver; the default is the current user when available.

### Schedule

| Command | Arguments | Purpose |
|---|---|---|
| `schedule add <PLAN>` | Exactly one of `--immediate`, `--manual`, `--at <LOCAL_TIME>`; optional `--version <N>`, `--start-deadline-seconds <N>`, `--local-endpoint <NAME>`, `--after-quota-reset`, `--idempotency-key <KEY>` | Create a simple Schedule. |
| `schedule add --file <PATH|->` | `--idempotency-key <KEY>` | Create any Schedule rule from JSON or YAML. |
| `schedule show <SCHEDULE>` | — | Show Schedule and latest occurrence. |
| `schedule list` | — | List Schedules and their latest Run state. |
| `schedule pause <SCHEDULE>` | `--idempotency-key <KEY>` | Pause a pending or recurring Schedule. |
| `schedule resume <SCHEDULE>` | `--idempotency-key <KEY>` | Resume a paused Schedule. |
| `schedule trigger <SCHEDULE>` | `--idempotency-key <KEY>` | Queue a manual Schedule. |

Text output distinguishes `schedule status` from `run status`. An immediate Schedule becomes `COMPLETED` after it creates its one Run; this does not mean the Run has completed.

- `--version` pins a Plan Version; the current Version is used by default.
- `--at` accepts a local date and time in the current user's time zone.
- `--start-deadline-seconds` controls how long a Run may wait to start; the default is 3,600 seconds.
- `--local-endpoint` selects a configured local Agent endpoint.
- `--after-quota-reset` waits for provider quota reset data. Scheduling fails when the Agent cannot provide it.

Recurring file rules are `daily`, `weekdays`, `weekly`, and `interval_after_success`:

```yaml
plan_id: update-deps
rule:
  type: weekdays
  local_time: 09:00:00
start_deadline_seconds: 3600
```

### Queue and Run

| Command | Arguments | Purpose |
|---|---|---|
| `queue list` | — | List queued Runs and eligibility or blocking reason. |
| `run next` | — | Execute the next eligible Run in the foreground. The daemon usually consumes it first. |
| `run show <RUN>` | — | Show state, times, latest Attempt, and log path. |
| `run progress <RUN>` | — | Alias of `run show`. |
| `run result <RUN>` | — | Show the final result, reason, changed-file count, log, and Diff path. |
| `run diff <RUN>` | — | Print the retained Diff. Only the latest Diff per Workspace is retained. |
| `run review <RUN>` | — | Show reason, log, and Diff information for a `REVIEW_REQUIRED` Run. |
| `run list` | — | List Runs with start and completion times. |
| `run cancel <RUN>` | `--idempotency-key <KEY>` | Cancel a queued or active Run. |
| `run retry <RUN>` | `--idempotency-key <KEY>` | Create a replacement Run when the approved `max_attempts` limit remains. |

`--idempotency-key` identifies one mutation request. Reusing the key with different input is rejected.

To read a Run log, copy the path from `run show`:

```powershell
Get-Content "<log-path>" -Tail 100
Get-Content "<log-path>" -Wait
```

## Agent usage

Agents should:

1. Use `--output json` for reads and mutations.
2. Keep returned IDs instead of parsing text tables.
3. Supply a unique `--idempotency-key` when a mutation may be retried.
4. Check `workspace status`, Plan status, and `queue list` before changing state.
5. Use `events --after <cursor> --output json` for polling or `--follow --output jsonl` for a stream.
6. Treat `REVIEW_REQUIRED` as a stop requiring `run review`; do not automatically widen permissions or retry.

JSON schemas, states, limits, and sandbox fields are documented in [CLI_CONTRACT.md](CLI_CONTRACT.md).
