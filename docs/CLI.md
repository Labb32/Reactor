# CLI guide

Reactor stores reviewed Agent work, schedules it, and records its result and file diff. The current adapter supports Codex.

## Quick start

Initialize local configuration and storage.

```shell
reactor init
```

Register a project and verify Codex readiness.

```shell
reactor workspace add ./project --id project
reactor workspace prepare project --agent codex
```

Create `plan.yaml`.

```yaml
workspace_id: project
title: Update dependencies
agent: codex
context: Update compatible dependencies and summarize the changes.
requirements: inherit
completion_criteria:
  - Report changed files and unresolved issues
limits:
  max_wall_time_seconds: 7200
  max_attempts: 1
```

Create, review, and approve the Plan. Copy the Plan ID from the first command.

```shell
reactor plan create --file plan.yaml
reactor plan review <plan-id>
reactor plan approve <plan-id>
```

Create `schedule.yaml` and queue the approved Plan immediately.

```yaml
plan_id: <plan-id>
rule:
  type: immediate
```

```shell
reactor schedule add --file schedule.yaml
reactor queue list
reactor run next
```

`run next` blocks until the selected Agent run finishes. Copy its Run ID to inspect the result.

```shell
reactor run progress <run-id>
reactor run diff <run-id>
reactor run result <run-id>
```

## Scheduling

Use the current user's time zone for a scheduled local time.

```yaml
plan_id: <plan-id>
rule:
  type: at
  local_time: 2026-09-20T09:00:00
start_deadline_seconds: 3600
```

For a manual schedule, use `type: manual`, then run:

```shell
reactor schedule trigger <schedule-id>
reactor run next
```

Manage pending schedules with `schedule list`, `schedule show`, `schedule pause`, and `schedule resume`.

## Common commands

```shell
reactor workspace status <workspace-id>
reactor plan list
reactor schedule list
reactor run list
reactor run cancel <run-id>
reactor run retry <run-id>
reactor snapshot
```

Only the latest diff is retained for each Workspace. `run show` reports the latest Attempt log path.

## Automation

Use `--output json` for commands and `--output jsonl` for event streams.

```shell
reactor snapshot --output json
reactor events --after 0 --follow --output jsonl
```

Document inputs accept JSON or YAML with `--file <path>`. Use `--file -` for stdin. Mutation commands accept `--idempotency-key <key>` when safe retries are required.

See [CLI_CONTRACT.md](CLI_CONTRACT.md) for schemas, exit codes, sandbox policy, and retention rules.
