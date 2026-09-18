# REACTOR 시스템 설계

## 기술

Rust 단일 Package와 Binary로 개발한다.

| 영역 | 선택 |
|---|---|
| Runtime | Tokio |
| CLI | clap |
| 저장소 | SQLite, rusqlite |
| 직렬화 | serde, JSON/YAML |
| 시간 | jiff |
| 로그 | tracing |
| Checkpoint | ignore, blake3, similar |

외부 Broker, PostgreSQL, Plugin Runtime과 분산 Lock은 사용하지 않는다. 버전은 구현 시 lockfile로 고정한다.

## 프로세스

`reactor` Binary가 CLI와 daemon 역할을 함께 제공한다.

```text
CLI ───────→ SQLite ←────── Daemon
                           ├→ Scheduler
                           ├→ Process Supervisor → Agent CLI
                           └→ Checkpoint Files
```

- CLI와 daemon은 각각 SQLite Connection을 연다.
- 모든 쓰기는 짧은 Transaction으로 끝낸다.
- 상태 전이는 `UPDATE ... WHERE state = ?`와 영향받은 Row 수로 경쟁을 막는다.
- `busy_timeout`을 설정하고 Lock 충돌만 짧게 재시도한다.
- Tokio는 Timer와 자식 Process에만 사용한다.

## 코드 구조

```text
src/
├─ main.rs
├─ domain.rs
├─ app.rs
├─ store.rs
├─ scheduler.rs
├─ runner.rs
├─ checkpoint.rs
├─ agents/
└─ platform/
```

실제 분리 필요가 생기기 전에는 Cargo Workspace와 여러 Crate를 만들지 않는다.

## 데이터

| Table | 내용 |
|---|---|
| `workspaces` | ID, Canonical Path, 상태와 Policy Fingerprint |
| `workspace_agents` | Agent별 Trust, Capability와 Readiness |
| `plans` | Plan ID와 현재 Version |
| `plan_versions` | Context, 완료 조건, Agent, Model, 한도와 정책 |
| `plan_approvals` | 승인 주체와 시각 |
| `schedules` | 시각, 반복 규칙, Gate와 상태 |
| `schedule_occurrences` | 각 예약 회차와 `QUEUED`·`MISSED` 결과 |
| `runs` | 실행 상태와 실행 정책 Snapshot |
| `attempts` | Agent Process의 시작·종료·Exit 정보 |
| `run_results` | Workspace의 최신 결과와 Diff 경로 |
| `leases` | Workspace와 공유 자원 Lock |
| `audit_events` | Append-only 상태 변경 이력 |
| `idempotency_keys` | 요청 Hash와 기존 응답 |

승인, 상태 변경과 감사 Event는 같은 Transaction에 저장한다. SQLite는 로컬 Disk의 WAL Mode와 `busy_timeout`을 사용하며 Network Drive에 두지 않는다.

## 불변 규칙

- `READY` Workspace와 Agent만 실행할 수 있다.
- Plan은 Raw Path 대신 `workspace_id`를 참조한다.
- 승인된 Plan Version은 수정하지 않는다.
- Run은 Plan Version과 실행 정책을 Snapshot으로 가진다.
- 같은 Workspace에는 Active Run 하나만 존재한다.
- Terminal Run을 되돌리지 않는다. Retry는 새 Run을 만든다.

## Workspace 준비

```text
workspace add
→ Canonical Path·중첩 검사
→ workspace prepare --agent
→ 설치·인증·Sandbox·Folder Trust 확인
→ Fingerprint 저장
→ READY
```

Trust 변경은 `workspace prepare`에서만 수행하고 감사 이력을 남긴다. Path나 설정이 바뀌면 `DEGRADED`로 전환한다.

## Plan

기본 Plan은 사용자가 검토한 Context와 `requirements: inherit`를 저장하며 Agent를 사전에 실행하지 않는다. 선택한 경우에만 Agent가 읽기 전용 환경에서 Context로부터 Plan 초안과 예상 요구 사항을 만들고, 사용자가 검토한 결과를 새 Plan Version으로 저장한다.

명시된 경로, Network와 Tool 요구 사항은 Workspace 정책과 Agent capability를 넘을 수 없다. Agent가 추출한 요구 사항은 예상치이며 실행 권한을 확대하지 않는다. 승인 직전에 Workspace와 Agent 정책 fingerprint 및 Tool을 다시 확인한다. 반복 Schedule은 승인된 Plan Version을 재사용한다.

## Sandbox

Reactor는 Workspace와 추가 쓰기 경로를 검증하고 이를 Agent 실행 옵션으로 변환한다. 실제 파일·Network·Process 제한은 Agent sandbox가 강제한다. 현재 사용자 환경의 Credential, 도구 설치와 외부 서비스 상태는 Reactor가 관리하지 않는다.

Git과 일반 CLI는 개별 등록 없이 sandbox 안에서 실행한다. Network, MCP와 Hook은 Workspace 정책으로 허용하며 Agent가 해당 기능을 지원할 때만 사용한다. 자세한 책임 범위는 [SANDBOX.md](SANDBOX.md)를 따른다.

## Scheduler

Scheduler는 긴 Sleep 하나에 의존하지 않고 가까운 예약과 짧은 최대 간격을 함께 사용한다.

1. 예약 회차를 계산한다.
2. 실행을 시작하지 못한 채 `start_deadline`이 지난 회차와 Run은 `MISSED`로 기록한다.
3. 조건을 만족한 회차를 Workspace FIFO Queue에 넣는다.
4. 빈 Slot이 생기면 Ready Workspace를 Round-robin으로 선택한다.
5. Workspace 첫 Run이 막히면 다른 Workspace를 확인한다.

기기가 꺼져 있던 동안 지난 회차는 실행하지 않는다. 반복 예약은 다음 미래 회차만 유지한다.

Schedule은 생성 시 OS의 현재 사용자 시간대를 저장한다. 이후 OS 시간대가 바뀌어도 기존 Schedule은 저장된 시간대를 사용하고, 수정 시 사용자가 새 시간대를 적용할 수 있다. DST로 존재하지 않는 시각은 다음 유효 시각에 실행하고 중복되는 시각은 첫 회차만 실행한다. 기본 `start_deadline`은 예정 시각 이후 1시간이다. 내부 시각과 Event 시각은 UTC RFC 3339로 저장한다.

## 동시성

```yaml
global: 2
per_agent: 2
per_provider: 2
per_local_endpoint: 1
```

`global`, `per_agent`, `per_provider`의 `2`는 예약 작업이 토큰과 Provider 사용량을 급격히 소모하지 않게 하는 변경 가능한 기본값이다. `per_local_endpoint: 1`은 로컬 모델의 메모리 경합을 피하기 위한 기본값이다.

Workspace 내부의 `1`은 파일과 Checkpoint의 일관성을 위한 고정 실행 계약이다. Workspace 사이는 위 한도 안에서 병렬 실행한다. 실행 중 한도를 낮춰도 기존 Process는 중단하지 않고 새 Run의 시작만 제한한다.

## Agent Adapter

v1은 Plugin 대신 `AgentKind` Enum과 Codex, Qwen Code, Claude Module을 사용한다.

각 Adapter는 다음을 제공한다.

- Capability와 Preflight
- Non-interactive Command 생성
- Workspace 정책을 Sandbox Option으로 변환
- Process 결과와 사용량 Parsing
- 가능한 경우 Quota Reset 시각 확인

실제 Command와 정책 Fingerprint는 저장하되 Secret 값은 기록하지 않는다.

지원 최소 Version은 비대화형 실행, 권한 Mode와 안정적인 결과 판별 기능을 모두 제공하는 Version으로 Adapter별 고정한다. Quota Reset 정보를 안정적으로 제공하지 않는 Agent나 Provider에는 Quota Gate를 노출하지 않는다.

| Agent | 최소 Version | 기본 Provider Group | 제한 |
|---|---:|---|---|
| Codex | 0.154.0 | `openai` | Git Workspace 필요 |
| Qwen Code | 0.24.0 | `qwen` | Hook 차단 정책 미지원 |
| Claude Code | 2.1.259 | `anthropic` | Native Windows Sandbox 미지원 |

Plan에 Provider가 없으면 Adapter의 기본 Group을 저장한다. 명시한 Provider는 그대로 사용한다. 모든 Adapter는 같은 Plan, Queue, Run 결과 계약을 사용하고 실행 인자와 출력 형식 차이는 Adapter 내부에서 처리한다.

## Process Supervisor

- Unix: Process Group 단위 종료
- Windows: Job Object 단위 종료
- Stdout/Stderr는 File에 계속 기록
- 취소 요청 시 Process Tree에 종료 신호를 보내고 3초 안에 종료되지 않으면 강제 종료
- Daemon 종료 신호도 같은 경로로 처리하고 실행 중 Run은 `REVIEW_REQUIRED`로 종료
- PID만 믿지 않고 Lease와 Process 시작 정보를 함께 확인

Daemon이 중단되면 Process에 다시 붙지 않는다. Run을 `REVIEW_REQUIRED`로 둔다.

취소와 강제 종료 뒤에도 Pipe에 남은 출력을 수집하고 종료 시점의 Manifest와 Diff를 만든다.

## Checkpoint

Run 전후에 Workspace File Manifest를 만든다.

```text
data/
├─ reactor.db
├─ manifests/
├─ diffs/
└─ runs/<run-id>/<attempt-id>.log
```

- Log는 Attempt별 10 MiB 단위로 회전하고 최대 5개를 보관한다. 종료 후 30일이 지나면 정리한다.
- Text File: Unified Diff 저장
- Binary·대용량 File: Hash와 크기만 저장
- `.gitignore`, `.reactorignore`와 정책 제외 규칙 적용
- Symlink는 기본적으로 따라가지 않음
- 원본 File 보관과 Rollback은 제공하지 않음

Manifest와 Diff는 Workspace별 최신 Run 하나만 보관하며 새 결과를 원자적으로 교체한다. 감사 Event에는 이전 Run의 상태와 변경 File 목록만 남긴다. 장기 변경 이력은 외부 VCS가 담당한다.

## 로컬 조정

CLI Mutation은 SQLite Transaction에서 `idempotency_key`를 먼저 확인한다. Daemon은 짧은 간격으로 Queue와 취소 요청을 확인한다. 기본 Poll 간격은 500ms이며 예약이 없을 때는 늘릴 수 있다.

CLI의 JSON 출력은 Stdout, 진단은 Stderr로 분리한다. 다른 로컬 도구는 DB를 직접 수정하지 않고 `reactor ... --output json` 명령을 호출한다.

`reactor init`과 daemon 시작만 Exclusive Lock 아래 Migration을 수행한다. 다른 CLI 명령은 지원하는 Schema Version이 아니면 쓰기를 거절한다. HTTP나 OS별 IPC는 원격 제어, UI 또는 실시간 Streaming이 실제로 필요할 때 추가한다.

## 외부 확인 계약

별도 호스팅 서비스가 Core와 결합되지 않도록 CLI에 안정적인 조회 계약을 둔다.

| 명령 | 용도 |
|---|---|
| `reactor info --output json` | Version, Capability, Instance 확인 |
| `reactor snapshot --output json` | Workspace, Queue와 Run 현재 상태 |
| `reactor events --after <cursor> --output jsonl` | 이후 Event 증분 조회 |
| `reactor events --after <cursor> --follow --output jsonl` | 상태 변경 Streaming |

응답에는 `schema_version`, `instance_id`와 UTC 시각을 넣는다. Event는 단조 증가하는 정수 `cursor`를 가지며 Append-only 감사 Event를 기반으로 만든다. 느린 Consumer를 위해 Event를 Memory Queue에만 두지 않고 SQLite에서 다시 읽을 수 있게 한다.

공개 JSON은 `snake_case`, UUID v7 ID와 RFC 3339 UTC 시각을 사용한다. `schema_version`은 `major.minor` 문자열이며 Minor Version에서는 Field를 추가할 수 있다. 오류는 `code`, `message`, `details`, `retryable`을 가진다. 기계 출력은 Stdout, 진단은 Stderr로 분리하고 종료 Code를 안정적으로 유지한다.

Event는 90일 또는 최신 100,000개 중 먼저 도달한 한도까지 보관한다. 요청한 Cursor가 만료됐으면 `cursor_expired` 오류와 현재 Snapshot 재조회 지침을 반환한다.

Snapshot은 Secret, 전체 작업 Context와 원본 Log를 기본 제외한다. 상세 내용은 별도 CLI 명령과 명시적 권한으로 조회한다.

외부 서비스 연결 방식은 다음 순서로 확장한다.

1. 같은 Host에서 CLI Subprocess 실행
2. Docker 서비스에서 작은 Host Connector가 CLI 실행
3. 호출량이 커질 때 Persistent Stdio Protocol 추가

DB File과 Docker Socket은 외부 서비스에 노출하지 않는다. 인증, 사용자 권한, TLS와 외부 Port는 호스팅 서비스 책임이다.

## Daemon 운영

`reactor daemon run`은 예약 시각과 Queue를 감시하고 Agent Process를 실행하는 장기 실행 명령이다. Service는 별도 Server가 아니라 OS가 이 daemon을 백그라운드에서 시작·재시작하게 하는 등록 항목이다.

`reactor init`은 기본적으로 현재 사용자 계정에 자동 시작을 등록하고 daemon을 즉시 시작한다. 초기화와 등록은 Idempotent하며 `reactor init --no-autostart`로 생략할 수 있다.

| OS | 자동 시작 방식 |
|---|---|
| Windows | 로그인 Trigger의 Task Scheduler 작업 |
| Linux | `systemd --user` Service |
| macOS | `launchd` LaunchAgent |

`reactor service install|start|stop|status|uninstall`로 등록과 상태를 관리한다. 등록 실패 시 데이터 초기화 결과와 실패 원인을 함께 보고하며 성공으로 숨기지 않는다. daemon은 Instance Lock으로 중복 실행을 막는다.

Instance Lock은 Data Directory의 파일에 OS 잠금을 유지한다. PID 문자열은 진단용이며 잠금 소유권 판단에는 사용하지 않는다. daemon은 시작 시 복구와 로그 정리를 한 번 수행한 뒤 `global` 한도만큼 Worker를 실행하며, 실제 선점은 기존 SQLite 동시성 계약을 따른다.

Agent 인증과 Workspace 권한을 그대로 사용하기 위해 관리자 권한과 별도 System Account를 요구하지 않는다. 따라서 자동 시작 기준은 장치 부팅이 아니라 해당 사용자의 로그인이다. 로그인 전 실행이 필요한 System Service는 v1 범위에서 제외한다.

## 구현 순서

1. Binary, SQLite Schema와 Versioned JSON CLI
2. Workspace 등록·Trust와 Codex Adapter
3. Context Plan 승인과 단일 예약
4. Queue, Process 감시와 상태 조회
5. Checkpoint, Diff와 중단 처리
6. 반복 예약, 한도와 Daemon 자동 시작
7. Qwen Code와 Claude Adapter

## 참고

- [Tokio Process](https://docs.rs/tokio/latest/tokio/process/struct.Command.html)
- [rusqlite Transaction](https://docs.rs/rusqlite/latest/rusqlite/struct.Transaction.html)
- [SQLite WAL](https://www.sqlite.org/wal.html)
