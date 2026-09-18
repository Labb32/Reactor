# CLI 사용법

[English](CLI.md) | 한국어

Reactor는 Codex, Qwen Code, Claude Code 작업을 검토 후 예약 실행합니다. 모든 명령은 비대화형으로 동작하므로 사용자와 Agent가 함께 사용할 수 있습니다.

## 설치

Rust 1.88 이상과 지원 Agent CLI가 하나 이상 필요합니다.

```shell
cargo install --path .
reactor -v
```

Cargo 바이너리 경로를 `PATH`에 추가해야 합니다. 기본 경로는 Windows `%USERPROFILE%\.cargo\bin`, Linux와 macOS `$HOME/.cargo/bin`입니다.

## 기본 형식

```shell
reactor [--output text|json|jsonl] <command>
reactor -h | --help
reactor help <command>
reactor -v | -V | --version
```

- `text`: 기본 출력이며 현재 사용자 시간대를 사용합니다.
- `json`: 버전이 포함된 응답을 반환하며 시각은 UTC입니다.
- `jsonl`: `events --follow` 스트림에 사용합니다.
- `NO_COLOR=1`: 상태 색상을 끕니다.
- `--file -`: stdin을 읽습니다. Plan 초안은 일반 텍스트나 Markdown, 나머지 문서는 JSON이나 YAML을 사용합니다.
- 상태 변경 명령의 `--idempotency-key <KEY>`는 같은 요청의 안전한 재시도에 사용합니다.

## 빠른 시작

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

`init`은 사용자 단위 daemon 자동 시작을 등록하고 즉시 실행합니다. 파일만 초기화하려면 `--no-autostart`를 사용합니다. Plan이 선택한 Agent는 해당 Workspace에서 미리 `prepare`해야 합니다.

## 명령

### 기본

| 명령 | 인자 | 설명 |
|---|---|---|
| `init` | `--file <PATH>`, `--no-autostart` | 설정과 저장소를 생성합니다. 선택적으로 설정 문서를 읽습니다. |
| `info` | 없음 | 버전, Schema, 기능과 동시 실행 한도를 표시합니다. |
| `snapshot` | 없음 | Workspace, 활성 Run, Queue와 Event 요약을 표시합니다. |
| `events` | `--after <CURSOR>`, `--limit <1..10000>`, `--follow` | 감사 Event를 읽습니다. `--follow`는 `--output jsonl`이 필요합니다. |

`--after` 기본값은 `0`, `--limit` 기본값은 `1000`입니다.

### Service와 daemon

| 명령 | 인자 | 설명 |
|---|---|---|
| `service install` | 없음 | 사용자 로그인 시 자동 시작을 등록합니다. |
| `service start` | 없음 | 등록된 daemon을 시작합니다. |
| `service stop` | 없음 | daemon을 중지합니다. |
| `service status` | 없음 | 설치 및 실행 상태를 표시합니다. |
| `service uninstall` | 없음 | Reactor 데이터는 유지하고 자동 시작만 제거합니다. |
| `daemon run` | `[--config-dir <PATH> --data-dir <PATH>]` | daemon을 foreground에서 실행합니다. 두 경로 옵션은 함께 사용해야 합니다. |

### Workspace

| 명령 | 인자 | 설명 |
|---|---|---|
| `workspace add <PATH>` | `--id <ID>`, `--file <POLICY>`, `--idempotency-key <KEY>` | 경로와 선택적 Sandbox 정책을 등록합니다. |
| `workspace prepare <ID>` | `--agent codex|qwen|claude`, `--idempotency-key <KEY>` | Agent 버전, 인증, Trust와 Sandbox 지원을 확인합니다. |
| `workspace status <ID>` | 없음 | 준비 상태를 갱신해 표시합니다. |
| `workspace list` | 없음 | 등록된 Workspace를 표시합니다. |

Workspace ID는 ASCII 문자나 숫자로 시작하며 문자, 숫자, `.`, `-`, `_`를 조합해 최대 64자로 작성합니다. `--file`은 JSON 또는 YAML 정책을 받고 `--agent` 기본값은 `codex`입니다.

### Plan

| 명령 | 주요 인자 | 설명 |
|---|---|---|
| `plan create` | `--name <NAME> --file <PATH>` | 문서에서 Plan을 생성합니다. |
| `plan create` | `--name`, `--workspace`, `--context`, `--title`, `--agent`, `--provider`, `--model`, 실행 한도 | 파일 없이 간단한 Plan을 생성합니다. |
| `plan draft` | `--name`, `--workspace`, `--agent`, `--file <PATH|->`, 모델 및 실행 한도 | Agent에 읽기 전용 초안 생성을 요청합니다. |
| `plan revise <PLAN>` | `--file <PATH|->` 또는 Plan 인자 | 변경 불가능한 새 Version을 추가합니다. 생략한 값은 유지합니다. |
| `plan show <PLAN>` | `--version <N>` | 현재 또는 지정 Version을 표시합니다. |
| `plan list` | 없음 | 현재 Plan Version을 표시합니다. |
| `plan review <PLAN>` | `-y`, `--approve`, `--approved-by <NAME>` | Draft를 검토 상태로 바꾸고 표시합니다. `-y` 또는 `--approve`는 즉시 승인합니다. |
| `plan approve <PLAN>` | `--approved-by <NAME>` | 검토 중인 현재 Version을 승인합니다. |
| `plan unapprove <PLAN>` | 없음 | 참조되지 않은 승인 Version을 검토 상태로 되돌립니다. |
| `plan delete <PLAN>` | 없음 | 연결된 Schedule이 모두 삭제된 Plan을 삭제합니다. |

상태 변경 명령에는 `--idempotency-key <KEY>`를 추가할 수 있습니다.

파일 입력과 간단 입력은 함께 사용할 수 없습니다. 문서 Schema는 [CLI_CONTRACT.md](CLI_CONTRACT.md)를 참고하세요. 존재하지 않는 JSON 또는 YAML 경로를 `--file`로 지정하면 Reactor가 Template만 생성하고 종료합니다.

- `--context <TEXT|->`: 작업 내용이며 `-`는 stdin을 읽습니다.
- `--agent`: `codex`, `qwen`, `claude` 중 하나이며 기본값은 `codex`입니다.
- `--provider`, `--model`: Agent에 전달할 선택적 Routing 정보입니다.
- `--max-wall-time-seconds`, `--max-attempts`, `--max-tokens`: 실행 한도입니다. 기본값은 7,200초, 1회, Token 제한 없음입니다.
- `--draft-timeout-seconds`: 초안 생성 제한이며 기본값은 300초입니다.
- `--clear-provider`, `--clear-model`, `--clear-max-tokens`: 수정 시 선택 값을 제거합니다.

PowerShell에서 여러 줄 Context를 입력하는 예시입니다.

```powershell
@'
Update dependencies.
Keep compatibility with the current runtime.
'@ | reactor plan create --name update-deps --workspace project --context -
```

명령을 여러 줄로 나눌 때 PowerShell은 줄 끝의 backtick, `cmd.exe`는 `^`, POSIX Shell은 `\`를 사용합니다.

### Schedule

| 명령 | 인자 | 설명 |
|---|---|---|
| `schedule add <PLAN>` | `--immediate`, `--manual`, `--at <LOCAL_TIME>` 중 하나와 선택 인자 | 간단한 Schedule을 생성합니다. |
| `schedule add --file <PATH|->` | `--name <ID>`, `--idempotency-key <KEY>` | JSON 또는 YAML에서 Schedule을 생성합니다. |
| `schedule show <SCHEDULE>` | 없음 | Schedule과 최근 Occurrence를 표시합니다. |
| `schedule list` | 없음 | Schedule과 최근 Run 상태를 표시합니다. |
| `schedule stop <SCHEDULE>` | `--idempotency-key <KEY>` | 이후 Occurrence를 중단하고 대기 또는 실행 중인 Run을 취소합니다. |
| `schedule delete <SCHEDULE>` | `--idempotency-key <KEY>` | 중단된 Schedule을 삭제합니다. |
| `schedule trigger <SCHEDULE>` | `--idempotency-key <KEY>` | 수동 Schedule을 Queue에 넣습니다. |

간단 등록의 선택 인자는 `--name`, `--version`, `--start-deadline-seconds`, `--local-endpoint`, `--after-quota-reset`입니다. `--at`은 사용자 시간대의 로컬 시각을 받습니다. 시작 기한 기본값은 3,600초입니다. Quota Reset 정보를 Agent가 제공하지 못하면 `--after-quota-reset` 등록은 실패합니다.

Schedule ID 기본 형식은 `<plan>-<rule>`이며 충돌하면 `-2`, `-3`이 붙습니다. 중지는 되돌릴 수 없습니다. 삭제 순서는 다음과 같습니다.

```shell
reactor schedule stop <schedule-id>
reactor schedule delete <schedule-id>
reactor plan delete <plan-id>
```

반복 문서 규칙은 `daily`, `weekdays`, `weekly`, `interval_after_success`를 지원합니다.

```yaml
plan_id: update-deps
rule:
  type: weekdays
  local_time: 09:00:00
start_deadline_seconds: 3600
```

### Queue와 Run

| 명령 | 인자 | 설명 |
|---|---|---|
| `queue list` | 없음 | Queue의 Run과 실행 가능 여부 또는 차단 사유를 표시합니다. |
| `run next` | 없음 | 다음 실행 가능한 Run을 foreground에서 실행합니다. 보통 daemon이 먼저 가져갑니다. |
| `run show <RUN>` | 없음 | 상태, 시각, 최근 Attempt와 Log 경로를 표시합니다. |
| `run progress <RUN>` | 없음 | `run show`의 별칭입니다. |
| `run result <RUN>` | 없음 | 최종 결과, 사유, 변경 파일 수, Log와 Diff 경로를 표시합니다. |
| `run diff <RUN>` | 없음 | Token 사용량과 보관된 Diff를 출력합니다. |
| `run review <RUN>` | 없음 | `REVIEW_REQUIRED` Run의 사유, Log와 Diff 정보를 표시합니다. |
| `run list` | `--limit`, `--all`, `--status`, `--workspace` | 조건에 맞는 Run을 최신순으로 표시합니다. 기본 제한은 50개입니다. |
| `run cancel <RUN>` | `--idempotency-key <KEY>` | 대기 또는 실행 중인 Run을 취소합니다. |
| `run retry <RUN>` | `--idempotency-key <KEY>` | 승인된 최대 시도 횟수 안에서 대체 Run을 생성합니다. |
| `run clear` | `<RUN>`, `-f`/`--all`, `--before <YYYY-MM-DD>` | 종료 Run과 Log를 삭제합니다. |

Run ID는 `<schedule>-YYYYMMDDTHHMMSSZ` 형식이며 재시도에는 `-retry-N`이 붙습니다. 목록의 8자 `CODE`나 고유한 ID 뒷부분으로도 Run을 지정할 수 있습니다.

인자 없는 `run clear`는 `SUCCEEDED`, `FAILED`, `CANCELLED`, `LIMIT_REACHED`, `MISSED`를 정리합니다. `REVIEW_REQUIRED`는 특정 ID 또는 `--all`을 사용해야 삭제됩니다. 활성 Run은 삭제할 수 없습니다. 감사 Event와 Workspace의 최신 Diff는 유지됩니다.

### Log

| 명령 | 인자 | 설명 |
|---|---|---|
| `log list` | 없음 | 각 Run의 최근 Attempt Log를 표시합니다. |
| `log show <RUN>` | `--all` | 현재 Log 또는 회전된 Log 전체를 출력합니다. |
| `log delete <RUN>` | 없음 | 활성 상태가 아닌 Run의 Log를 삭제하고 감사 Event를 기록합니다. |
| `log prune` | 없음 | 30일 Log 보존 규칙을 즉시 적용합니다. |

## Agent 사용 규칙

Agent는 읽기와 상태 변경에 `--output json`을 사용하고 반환된 ID를 보관해야 합니다. 재시도 가능한 상태 변경에는 고유한 `--idempotency-key`를 전달합니다. 상태 변경 전에 `workspace status`, Plan 상태와 `queue list`를 확인합니다.

Event Polling은 `events --after <cursor> --output json`, Stream은 `--follow --output jsonl`을 사용합니다. `REVIEW_REQUIRED`는 사용자 검토가 필요한 중단 상태이므로 권한 확대나 자동 재시도를 해서는 안 됩니다.

## 라이선스

Reactor는 [MIT License](../LICENSE)로 공개됩니다. 복사하거나 재배포할 때 저작권과 라이선스 고지를 유지하세요.
