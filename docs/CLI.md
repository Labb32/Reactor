# CLI guide

English | [한국어](CLI.ko.md)

Reactor schedules reviewed work for Codex, Qwen Code, and Claude Code. Commands are non-interactive and can be used by people or Agents.

## Install

Reactor requires Rust 1.88 or later and at least one supported Agent CLI.

```shell
cargo install --path .
reactor -v
```

Ensure Cargo's binary directory is on `PATH`: `%USERPROFILE%\.cargo\bin` on Windows or `$HOME/.cargo/bin` on Linux and macOS.

## Basics

```shell
reactor [--output text|json|jsonl] <command>
reactor -h | --help
reactor help <command>
reactor -v | -V | --version
```

- `text` is the default. Times use the current local time zone.
- `json` returns a versioned envelope and keeps timestamps in UTC.
- `jsonl` is intended for `events --follow`.
- `NO_COLOR=1` disables status colors.
- `--file -` reads stdin. Plan drafts read plain text or Markdown; other document inputs use JSON or YAML.
- Plan, Schedule, and Run state mutations expose `--idempotency-key <KEY>` for safe retries. Keys contain 1–128 bytes without control characters.

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

The Agent selected by a Plan must be prepared for its Workspace. Preparing one Agent does not prepare the others.

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
| `plan revise <PLAN>` | `--file <PATH|->` or inline fields, `--idempotency-key <KEY>` | Add a new immutable Plan version. Unspecified inline fields keep their current values. |
| `plan show <PLAN>` | `--version <N>` | Show the current or selected version. |
| `plan list` | — | List current Plan versions. |
| `plan review <PLAN>` | `-y`, `--approve`, `--approved-by <NAME>`, `--idempotency-key <KEY>` | Move a Draft to review and display it. `-y` or `--approve` approves it in the same command. |
| `plan approve <PLAN>` | `--approved-by <NAME>`, `--idempotency-key <KEY>` | Approve the current reviewed version. |
| `plan unapprove <PLAN>` | `--idempotency-key <KEY>` | Return an unreferenced approved Version to review. |
| `plan delete <PLAN>` | `--idempotency-key <KEY>` | Delete a Plan after all of its Schedules are deleted. |

`--name` creates a short Plan identifier such as `update-deps`; otherwise Reactor generates an ID. A name follows the same character rules as a Workspace ID.

`--file` cannot be combined with the simple Plan fields. The document schema is defined in [CLI_CONTRACT.md](CLI_CONTRACT.md).

When `plan create --name <NAME> --file <PATH>` points to a missing file, Reactor creates a JSON or YAML template and stops. A path without an extension receives `.yaml`; existing files are never overwritten.

- `--workspace` selects the registered Workspace.
- `--context` supplies the task text; `-` reads plain text from stdin.
- `--title` overrides the title derived from the first Context line.
- `--agent` selects `codex`, `qwen`, or `claude`; the default is `codex`.
- `--provider` and `--model` pass optional routing metadata to the Agent.
- `--max-wall-time-seconds`, `--max-attempts`, and `--max-tokens` set execution limits. Defaults are 7,200 seconds, one Attempt, and no token limit.
- `--draft-timeout-seconds` limits draft generation; the default is 300 seconds.
- `--approved-by` records the approver; the default is the current user when available.

Inline revision accepts `--workspace`, `--context`, `--title`, `--agent`, `--provider`, `--model`, and all limit options. Use `--clear-provider`, `--clear-model`, or `--clear-max-tokens` to remove optional values.

Use stdin for a multiline Context. PowerShell example:

```powershell
@'
Update dependencies.
Keep compatibility with the current runtime.
'@ | reactor plan create --name update-deps --workspace project --context -
```

Shell line continuation belongs to the shell: PowerShell uses a trailing backtick, `cmd.exe` uses `^`, and POSIX shells use `\`.

```batch
reactor plan create ^
  --name update-deps ^
  --workspace project ^
  --context "Update dependencies."
```

### Schedule

| Command | Arguments | Purpose |
|---|---|---|
| `schedule add <PLAN>` | Exactly one of `--immediate`, `--manual`, `--at <LOCAL_TIME>`; optional `--name <ID>`, `--version <N>`, `--start-deadline-seconds <N>`, `--local-endpoint <NAME>`, `--after-quota-reset`, `--idempotency-key <KEY>` | Create a simple Schedule. |
| `schedule add --file <PATH|->` | `--name <ID>`, `--idempotency-key <KEY>` | Create any Schedule rule from JSON or YAML. |
| `schedule show <SCHEDULE>` | — | Show Schedule and latest occurrence. |
| `schedule list` | — | List Schedules and their latest Run state. |
| `schedule stop <SCHEDULE>` | `--idempotency-key <KEY>` | Stop future occurrences and cancel a pending or active Run. |
| `schedule delete <SCHEDULE>` | `--idempotency-key <KEY>` | Delete a stopped Schedule. |
| `schedule trigger <SCHEDULE>` | `--idempotency-key <KEY>` | Queue a manual Schedule. |

Text output distinguishes `schedule status` from `run status`. An immediate Schedule becomes `COMPLETED` after it creates its one Run; this does not mean the Run has completed.

Schedule IDs default to `<plan>-<rule>` and receive `-2`, `-3`, and later suffixes on collision. `--name` assigns a custom ID. IDs remain reserved after deletion so audit records stay unambiguous.

Stopping is terminal. Delete the stopped Schedule before deleting its Plan. An active Run must finish its cancellation before Plan deletion. Deleted Plans and Schedules are hidden from normal commands while past Runs and audit events remain available.

```shell
reactor schedule stop <schedule-id>
reactor schedule delete <schedule-id>
reactor plan delete <plan-id>
```

Manual Schedules have no execution time until triggered. Their output shows the rule and the start deadline measured from trigger time.

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
| `run diff <RUN>` | — | Print token usage and the retained Diff. Only the latest Diff per Workspace is retained. |
| `run review <RUN>` | — | Show reason, log, and Diff information for a `REVIEW_REQUIRED` Run. |
| `run list` | `--limit <1..10000>` (default `50`), `--all`, `--status <STATE>`, `--workspace <ID>` | List matching Runs, newest first. |
| `run cancel <RUN>` | `--idempotency-key <KEY>` | Cancel a queued or active Run. |
| `run retry <RUN>` | `--idempotency-key <KEY>` | Create a replacement Run when the approved `max_attempts` limit remains. |
| `run clear` | `<RUN>`, `-f`/`--all`, `--before <YYYY-MM-DD>` | Delete completed Runs and their logs. `REVIEW_REQUIRED` needs an ID or `--all`. |

`--idempotency-key` identifies one mutation request. Reusing the key with different input is rejected.

New Run IDs use `<schedule>-YYYYMMDDTHHMMSSZ`; retries append `-retry-N`. A numeric suffix resolves timestamp collisions. Existing UUID Run IDs remain valid.

`run list` prints an eight-character `CODE`. Every command accepting `<RUN>` also accepts a unique trailing code; use more trailing characters if a code becomes ambiguous.

`run clear` clears all `SUCCEEDED`, `FAILED`, `CANCELLED`, `LIMIT_REACHED`, and `MISSED` Runs by default. A specific Run ID or `-f`/`--all` also permits `REVIEW_REQUIRED`; active Runs are always protected. `--before` limits bulk cleanup to Runs created before midnight UTC on that date. Clearing removes the database record, Attempts, result, pending checkpoint, and logs while retaining its audit events. A retained Schedule occurrence reports the removed Run as `CLEARED`. Workspace-level latest Diff and manifest files remain under their existing retention rule.

To read a Run log, copy the path from `run show`:

```powershell
Get-Content "<log-path>" -Tail 100
Get-Content "<log-path>" -Wait
```

### Logs

| Command | Arguments | Purpose |
|---|---|---|
| `log list` | — | List the latest Attempt log for each Run. |
| `log show <RUN>` | `--all` | Print the current log or include rotated files. |
| `log delete <RUN>` | — | Delete logs for a non-active Run and write an audit event. |
| `log prune` | — | Apply the 30-day log retention rule immediately. |

## Agent usage

Agents should:

1. Use `--output json` for reads and mutations.
2. Keep returned IDs instead of parsing text tables.
3. Supply a unique `--idempotency-key` when a mutation may be retried.
4. Check `workspace status`, Plan status, and `queue list` before changing state.
5. Use `events --after <cursor> --output json` for polling or `--follow --output jsonl` for a stream.
6. Treat `REVIEW_REQUIRED` as a stop requiring `run review`; do not automatically widen permissions or retry.

JSON schemas, states, limits, and sandbox fields are documented in [CLI_CONTRACT.md](CLI_CONTRACT.md).

## License

Reactor is available under the [MIT License](../LICENSE). Keep the copyright and license notice when copying or redistributing the software.
