# Reactor

English | [한국어](README.md)

Reactor is a lightweight, cross-platform CLI scheduler for reviewed AI agent work.

> Current status: v0.2 beta

```text
Register workspace -> Review plan -> Add schedule -> Run agent -> Inspect result and diff
```

Reactor runs Codex, Qwen Code, and Claude Code on Windows, Linux, and macOS. It works only while the machine and daemon are running. Work that passes its deadline while offline is marked `MISSED` and is not replayed after restart.

## Install

Reactor requires Rust 1.88 or later and at least one supported agent CLI.

```shell
cargo install --path .
reactor -v
```

Cargo's binary directory must be in `PATH`. The default is `%USERPROFILE%\.cargo\bin` on Windows and `$HOME/.cargo/bin` on Linux and macOS.

## Quick start

```shell
reactor init
reactor workspace add ./project --id project
reactor workspace prepare project --agent codex
reactor plan create --name update-deps --workspace project --context "Update dependencies and summarize the changes."
reactor plan review update-deps -y
reactor schedule add update-deps --immediate
reactor run list
```

`reactor init` registers the daemon for automatic startup in the current user account and starts it immediately. Use `--no-autostart` to initialize files only.

## Features

- Immediate, manual, scheduled, and recurring work
- Reviewed and versioned plans
- Per-workspace trust and sandbox policies
- Sequential runs within a workspace and limited parallel runs across workspaces
- Run state, rotating logs, file diffs, and audit events
- Non-interactive JSON and JSONL output
- Cleanup for finished runs and logs

Only one run executes in a workspace at a time. Different workspaces can run concurrently within the default global limit of two.

## Manage work

```shell
reactor schedule list
reactor queue list
reactor run list --limit 50
reactor run show <run-id>
reactor run diff <run-id>
reactor run result <run-id>
```

Stop a schedule before deleting it. A plan can be deleted after its schedules are deleted and its active runs have ended.

```shell
reactor schedule stop <schedule-id>
reactor schedule delete <schedule-id>
reactor plan delete <plan-id>
```

Clear finished runs with the following commands. The default keeps `REVIEW_REQUIRED` runs. An explicit ID or `--all` can remove them.

```shell
reactor run clear
reactor run clear --before 2026-09-01
reactor run clear <run-id>
reactor run clear --all
```

## Execution boundary

Scheduled work does not wait for interactive approval. Reactor validates workspace paths and policy, then passes execution options to the agent. The agent sandbox enforces file, network, and process isolation. Reactor uses existing agent, Git, SSH, and external CLI credentials without storing or refreshing them.

Reactor consists of one Rust binary and SQLite. It has no embedded HTTP server. A Web UI or remote service can use the versioned JSON CLI and cursor-based events.

```shell
reactor info --output json
reactor snapshot --output json
reactor events --after <cursor> --follow --output jsonl
```

## Documentation

- [Documentation index](docs/README.md)
- [CLI guide](docs/CLI.md) · [한국어](docs/CLI.ko.md)
- [CLI JSON contract](docs/CLI_CONTRACT.md)
- [Sandbox boundary](docs/SANDBOX.md)
- [System design](docs/SYSTEM_DESIGN.md)
- [Patch notes](PATCH_NOTES.md)

## License

Reactor is available under the [MIT License](LICENSE). Commercial and non-commercial use, modification, and redistribution are allowed when the copyright and license notice are preserved.
