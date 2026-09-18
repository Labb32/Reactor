# System design

Reactor is a local Rust CLI and background daemon. It schedules reviewed work for external AI agent CLIs while the host is running.

## Components

- **CLI:** validates input and reads or updates local state.
- **Daemon:** materializes due schedules, dispatches eligible runs, and records results.
- **SQLite store:** keeps workspaces, plans, schedules, runs, leases, and audit events.
- **Agent adapters:** translate a plan and workspace policy into a provider-specific process invocation.
- **Process supervisor:** starts agents, handles cancellation, and records termination.
- **Result capture:** stores the final response, logs, and the latest workspace diff.

There is no internal HTTP server. External services can use versioned JSON output and the cursor-based event stream.

## Execution model

Runs are sequential within a workspace. Different workspaces may run concurrently up to the global and provider limits, which default to two.

The daemon only processes work while the machine is running. An occurrence that passes its deadline while Reactor is unavailable becomes `MISSED`; it is not replayed in a burst after restart.

## Data model

```text
Workspace
  |-- Plan -> PlanVersion -> Approval
  `-- Schedule -> Occurrence -> Run -> Attempt -> Result
```

Plans and schedules use soft deletion so existing run and audit history remains readable. Completed runs can be cleared independently. Audit events are retained for up to 90 days or 100,000 entries.

The current beta uses one consolidated schema file. Databases already at schema version 9 remain compatible. Databases from intermediate development schema versions must be recreated.

## Process and recovery

The supervisor sends a graceful interrupt first and escalates if the process does not exit. Logs and the partial diff are preserved after cancellation.

Expired run leases are recovered when the daemon starts. Reactor does not retry failed work automatically unless the user explicitly creates a retry.

## Public interface

Text output is intended for people. Integrations should request JSON or JSONL:

```shell
reactor info --output json
reactor snapshot --output json
reactor events --after <cursor> --follow --output jsonl
```

Public fields use `snake_case` and RFC 3339 UTC timestamps. Consumers must treat identifiers as opaque strings even when schedule and run IDs contain readable parts.
