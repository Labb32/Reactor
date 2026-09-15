# Reactor

**검토한 AI 에이전트 작업을 원하는 시점에 실행하는 크로스 플랫폼 CLI 스케줄러**

> 현재 상태: 초기 구현

```text
Workspace 등록 → 작업 검토·승인 → 예약 → Agent 실행 → 결과·Diff 확인
```

Reactor는 기기와 daemon이 켜져 있을 때만 동작합니다. 꺼져 있는 동안 기한을 넘긴 작업은 `MISSED`로 기록하며 나중에 몰아서 실행하지 않습니다.

`reactor init`은 사용자 계정에 daemon 자동 시작을 등록합니다. 이후 로그인하면 별도 설정 없이 백그라운드에서 예약을 처리합니다.

## 주요 기능

- Windows, Linux와 macOS 지원
- Codex, Qwen Code와 Claude CLI 실행
- 즉시, 지정 시각, 시간대와 반복 예약
- Text 또는 Markdown Plan 검토 및 Version별 승인
- Workspace별 Trust와 sandbox 정책
- 상태, 로그, 파일 Diff와 감사 이력
- AI가 호출할 수 있는 비대화형 JSON CLI

## 사용 흐름

```shell
reactor init
reactor workspace add ./project --id project
reactor workspace prepare project --agent codex
reactor plan create --file plan.yaml
reactor plan approve <plan-id>
reactor schedule add --file schedule.yaml
reactor run progress <run-id>
reactor run diff <run-id>
reactor run result <run-id>
```

모든 작업은 준비된 Workspace에서 실행합니다. 승인된 Plan Version은 수정하지 않으며, 변경하면 새 Version으로 다시 검토합니다.

예약 작업은 실행 중 사용자 승인을 기다리지 않습니다. Workspace 정책 안에서 Agent의 sandbox 또는 Full Access 모드를 사용하고, 필요한 권한이 없으면 실패 사유를 기록합니다.

## 실행 정책

같은 Workspace에서는 파일과 Checkpoint의 일관성을 위해 한 번에 하나만 실행합니다. 서로 다른 Workspace는 설정된 한도 안에서 병렬로 실행합니다.

```yaml
concurrency:
  global: 2
  per_agent: 2
  per_provider: 2
  per_local_endpoint: 1
```

동시성 값은 토큰, Provider 사용량과 장비 부하를 보호하기 위한 변경 가능한 기본값입니다.

중단된 작업은 자동 재실행하지 않습니다. 상태, 로그와 Diff를 검토한 뒤 새 Run으로 이어서 진행합니다.

## 구조

Reactor는 Rust 단일 Binary와 SQLite로 구성합니다. CLI와 daemon이 같은 DB를 사용하며, SQLite Transaction으로 Queue 선점과 상태 전이를 처리합니다.

```text
CLI ───────→ SQLite ←────── Daemon → Agent CLI → Workspace
                                      └───────→ Log / Diff
```

외부 Broker, PostgreSQL, 분산 Lock과 내장 HTTP Server는 사용하지 않습니다.

## 외부 서비스 연동

Web UI, 원격 제어와 실시간 화면은 별도 서비스가 담당합니다. Reactor는 Versioned JSON Snapshot과 Cursor 기반 Event 조회를 제공합니다.

```shell
reactor info --output json
reactor snapshot --output json
reactor events --after <cursor> --output jsonl
```

외부 서비스는 DB를 직접 수정하지 않고 Reactor CLI 또는 Host Connector를 사용합니다. 인증, 사용자 권한, TLS와 외부 Port도 외부 서비스가 담당합니다.

## 개발 순서

개발은 번호가 지정된 Task 순서로 진행합니다. 연속 Task는 같은 Feature Branch로 묶을 수 있으며, Branch 게시 후 사용자가 직접 확인하고 `main`으로 Merge합니다.

- [Task 목록과 Branch 계획](tasks/README.md)

## 문서

- [프로젝트 기획](docs/REACTOR.md)
- [시스템 설계](docs/SYSTEM_DESIGN.md)
- [CLI 계약](docs/CLI_CONTRACT.md)
- [개발 Task](tasks/README.md)
