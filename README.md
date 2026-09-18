<p align="center">
  <img src="assets/reactor-logo.svg" width="160" alt="Reactor 로고">
</p>

<h1 align="center">Reactor</h1>

[English](README.en.md) | 한국어

검토한 AI Agent 작업을 원하는 시점에 실행하는 크로스 플랫폼 CLI 스케줄러입니다.

> 현재 상태: v0.2 beta

```text
Workspace 등록 → Plan 검토·승인 → Schedule 등록 → Agent 실행 → 결과·Diff 확인
```

Reactor는 Windows, Linux와 macOS에서 Codex, Qwen Code, Claude Code를 실행합니다. 기기와 daemon이 켜져 있을 때만 동작하며, 꺼져 있는 동안 기한을 넘긴 작업은 `MISSED`로 남기고 몰아서 실행하지 않습니다.

## 설치

Rust 1.88 이상과 사용할 Agent CLI가 필요합니다.

운영체제별 바이너리는 [GitHub Releases](https://github.com/Labb32/Reactor/releases)에서 받을 수 있습니다.

```shell
cargo install --path .
reactor -v
```

Cargo의 binary 경로가 `PATH`에 있어야 합니다. 기본 경로는 Windows의 `%USERPROFILE%\.cargo\bin`, Linux와 macOS의 `$HOME/.cargo/bin`입니다.

## 시작하기

```shell
reactor init
reactor workspace add ./project --id project
reactor workspace prepare project --agent codex
reactor plan create --name update-deps --workspace project --context "Update dependencies and summarize the changes."
reactor plan review update-deps -y
reactor schedule add update-deps --immediate
reactor run list
```

`reactor init`은 현재 사용자 계정에 daemon 자동 시작을 등록하고 즉시 실행합니다. 파일만 초기화하려면 `--no-autostart`를 사용합니다.

## 주요 기능

- 즉시, 수동, 지정 시각과 반복 Schedule
- 검토하고 Version별로 승인하는 Plan
- Workspace별 Trust와 sandbox 정책
- Workspace 내부 순차 실행과 Workspace 사이 제한 병렬 실행
- Run 상태, 회전 로그, 파일 Diff와 감사 이벤트
- 비대화형 JSON/JSONL 출력
- 종료 Run과 로그 정리

같은 Workspace에서는 한 번에 하나의 Run만 실행합니다. 서로 다른 Workspace는 기본 전역 한도 2 안에서 병렬로 실행합니다.

## 작업 관리

```shell
reactor schedule list
reactor queue list
reactor run list --limit 50
reactor run show <run-id>
reactor run diff <run-id>
reactor run result <run-id>
```

Schedule을 제거하려면 먼저 중지합니다. 활성 Run의 취소가 끝난 뒤 Plan을 삭제할 수 있습니다.

```shell
reactor schedule stop <schedule-id>
reactor schedule delete <schedule-id>
reactor plan delete <plan-id>
```

종료 Run은 다음 명령으로 정리합니다. 기본 동작은 `REVIEW_REQUIRED`를 보존하며, 특정 ID 또는 `--all`을 사용한 경우에만 함께 삭제합니다.

```shell
reactor run clear
reactor run clear --before 2026-09-01
reactor run clear <run-id>
reactor run clear --all
```

## 실행 경계

예약 작업은 실행 중 사용자 승인을 기다리지 않습니다. Reactor는 Workspace 경로와 정책을 검증하고 Agent 실행 옵션으로 전달합니다. 실제 파일·Network·Process 격리는 Agent sandbox가 담당합니다. 기존 Agent, Git, SSH와 외부 CLI의 Credential은 사용만 하며 Reactor가 저장하거나 갱신하지 않습니다.

Reactor는 Rust 단일 Binary와 SQLite로 구성하며 내장 HTTP Server를 제공하지 않습니다. Web UI와 원격 제어 서비스는 versioned JSON CLI와 cursor 기반 이벤트를 사용합니다.

```shell
reactor info --output json
reactor snapshot --output json
reactor events --after <cursor> --follow --output jsonl
```

## 문서

- [문서 목록](docs/README.md)
- [CLI 사용법](docs/CLI.ko.md) · [English](docs/CLI.md)
- [CLI JSON 계약](docs/CLI_CONTRACT.md)
- [Sandbox 책임 경계](docs/SANDBOX.md)
- [시스템 설계](docs/SYSTEM_DESIGN.md)
- [Release 안내](docs/RELEASING.md)
- [패치 노트](PATCH_NOTES.md)

## 라이선스

[MIT License](LICENSE)를 적용합니다. 저작권 및 라이선스 고지를 유지하면 상업·비상업 목적의 사용, 수정, 재배포가 가능합니다.
