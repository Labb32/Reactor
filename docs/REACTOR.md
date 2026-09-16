---
type: project
tier: T4
status: In_Progress
created: 2026-09-11
---

# REACTOR

## 목적

Reactor는 검토된 AI 작업을 원하는 시점에 실행하는 크로스 플랫폼 CLI 스케줄러다.

```text
Workspace 등록 → Context 검토·승인 → 예약 → Agent 실행 → 상태·Diff·결과 확인
```

Windows, Linux, macOS를 지원한다. 기기가 켜져 있고 daemon이 실행 중일 때만 동작하며 전원 제어는 하지 않는다.

`reactor init`은 기본적으로 현재 사용자 계정에 daemon 자동 시작을 등록하고 즉시 실행한다. 별도 설정 없이 로그인 후 예약 작업을 처리하며 `--no-autostart`로 등록을 생략할 수 있다.

## 핵심 범위

- Codex, Qwen Code, Claude CLI 실행
- 즉시, 수동, 지정 시각, 시간대, 반복 예약
- AI가 사용할 수 있는 비대화형 JSON CLI
- Workspace별 신뢰·sandbox 정책
- Workspace 내부 FIFO, Workspace 사이 제한 병렬 실행
- Timeout, quota gate, 재시도와 취소
- 실행 상태, 로그, 전후 Diff와 감사 이력
- 기기 종료 중 놓친 예약은 `MISSED` 처리하며 몰아서 실행하지 않음

Reactor는 승인된 Context를 하나의 Agent 작업으로 실행하며 Context 내부 구조는 해석하지 않는다.

## Workspace

모든 작업은 등록된 `workspace_id`를 사용한다. Raw path를 직접 실행 요청에 넣지 않는다.

```shell
reactor workspace add ./example --id example
reactor workspace prepare example --agent codex
reactor workspace status example
```

`workspace prepare`는 경로, 중첩 등록, Agent 설치·인증, Folder Trust, 비대화형 실행, sandbox와 프로젝트 정책을 확인한다. 준비된 Agent만 `READY`가 된다. 경로나 정책 fingerprint가 바뀌면 `DEGRADED`로 전환한다.

## 일반 Context Plan

기본 입력은 사용자가 검토한 Text 또는 Markdown이다. 기존 Agent의 plan mode 결과를 그대로 사용할 수 있다.

```yaml
title: "의존성 업데이트"
workspace_id: example
agent: codex
context: |
  목표: 호환 가능한 의존성을 갱신한다.

  순서:
  1. 현재 상태와 프로젝트 지침을 확인한다.
  2. 안전한 업데이트만 적용한다.
  3. 검증하고 변경 내용을 정리한다.

  조건:
  안전한 업데이트가 없으면 변경하지 않고 이유를 보고한다.
completion_criteria:
  - "변경 파일과 미해결 문제 보고"
limits:
  max_wall_time: 2h
  max_attempts: 2
```

Plan은 `DRAFT → IN_REVIEW → APPROVED` 순서로 승인한다. 승인된 Version은 수정하지 않으며 변경 시 새 Version을 만든다.

## 예약과 Queue

반복 규칙은 cron 대신 `daily`, `weekdays`, `weekly`, `interval_after_success`로 제공한다. 예약 생성 시 OS의 현재 사용자 시간대를 저장한다. 지정 시각 예약은 `start_at`과 `start_deadline`을 가지며 기한을 넘기면 `MISSED`가 된다.

```yaml
concurrency:
  global: 2
  per_agent: 2
  per_provider: 2
  per_local_endpoint: 1
```

- `global`, `per_agent`, `per_provider`의 `2`는 토큰·사용량의 급격한 소모를 막기 위한 변경 가능한 기본값이다.
- `per_local_endpoint: 1`은 로컬 모델의 메모리 경합을 피하기 위한 기본값이다.
- 같은 Workspace에서는 `ready_at`, 등록 순서로 하나씩 실행한다.
- Workspace 사이는 round-robin으로 선택한다.
- 한 Workspace가 막혀도 다른 Workspace는 실행할 수 있다.

## 무인 실행

예약 작업은 실행 중 사용자 승인을 기다리지 않는다.

```yaml
writable_roots:
  - ../shared-library
network: allow
mcp: allow
project_hooks: allow
```

Workspace는 항상 기본 쓰기 경로다. Reactor는 허용 범위를 검증하고 Agent의 sandbox 기능으로 강제한다. Git, CLI와 기존 사용자 인증은 별도로 등록하지 않으며 필요한 외부 기능만 Workspace 정책으로 허용한다. 권한이 부족하면 대기하거나 자동으로 확대하지 않고 사유를 기록한다.

자세한 책임 범위는 [SANDBOX.md](SANDBOX.md)를 따른다.

## 실행과 결과

```text
WAITING → QUEUED → PREPARING → RUNNING
                              ├→ SUCCEEDED
                              ├→ FAILED
                              ├→ CANCELLED
                              ├→ LIMIT_REACHED
                              └→ REVIEW_REQUIRED

WAITING → MISSED
```

기기나 daemon이 실행 중 종료되면 자동 재실행하지 않는다. 사용자가 Diff와 로그를 확인한 뒤 새 Run을 만든다. 취소하면 Agent Process Tree를 중지하고 그 시점까지의 로그와 파일 Diff를 만든다.

Run 시작과 종료에 파일 manifest를 만들어 생성·수정·삭제 목록과 Text Diff를 저장한다. Workspace별로 가장 최근 Run의 Diff만 보관한다. Binary와 큰 파일은 Hash와 크기만 기록한다. 장기 변경 이력은 Git 같은 외부 VCS에 맡긴다.

완료 결과에는 상태, 시각, 요약, 완료 조건, 변경 파일, 검증 결과, 사용량과 로그 경로를 남긴다.

## CLI

```shell
reactor init
reactor service install|start|stop|status|uninstall
reactor workspace add|prepare|status|list
reactor plan create|show|revise|review|approve
reactor schedule add|pause|resume
reactor run start|list|show|progress|diff|result|retry|cancel
reactor queue show
reactor audit list
```

모든 명령은 `--file <yaml|json>`, `--file -`, `--output json`을 지원한다. Mutation은 `idempotency_key`로 중복을 막는다.

## 외부 서비스 연동 경계

Web UI, 원격 제어와 실시간 화면은 Reactor Core에 넣지 않고 별도 호스팅 서비스가 담당한다. 외부 서비스는 DB를 직접 수정하지 않고 다음 CLI 계약을 사용한다.

```shell
reactor info --output json
reactor snapshot --output json
reactor events --after <cursor> --output jsonl
reactor events --after <cursor> --follow --output jsonl
```

- 모든 JSON에 `schema_version`과 `instance_id` 포함
- Event에 단조 증가하는 `cursor` 포함
- 연결이 끊기면 마지막 Cursor 이후부터 재조회
- Mutation은 기존 CLI와 `idempotency_key` 사용
- Secret, 전체 Prompt와 원본 로그는 기본 Snapshot에서 제외
- 외부 서비스의 인증·권한·Network 공개는 해당 서비스가 담당

같은 Host에서는 서비스가 Reactor CLI를 실행한다. Docker 기반 서비스는 Host Connector를 통해 CLI를 호출하며 Reactor DB Volume이나 Docker Socket을 직접 공유하지 않는다.

## 구현 순서

1. Rust 단일 Binary, SQLite Schema와 Versioned JSON CLI
2. Workspace 등록·Trust와 첫 Agent Adapter
3. Context Plan 검토·승인과 단일 예약
4. Queue, 프로세스 감시와 상태 조회
5. Checkpoint, Diff와 중단 복구
6. 반복 예약, 한도와 나머지 Agent

자세한 구현 기준은 [SYSTEM_DESIGN.md](SYSTEM_DESIGN.md)와 [SANDBOX.md](SANDBOX.md)를 따른다.
