# Linux 서버 M5 테스트 가이드

이 문서는 실행 절차다. 서버 테스트를 실행했다는 기록이 아니다. Windows에서 빌드·Clippy와 기본 smoke 3개만 통과했으며 Linux·실제 Codex·격리·장애 복구는 미검증이다. PASS/FAIL/BLOCKED/미실행을 구분한다. 모든 필수 시나리오와 차단 결함을 확인한 후에만 M6 적용을 판단한다.

## 1. 대상 고정과 기본 검사

검증할 커밋을 별도로 checkout한 소스 디렉터리에서 실행한다. Rust 1.88 이상, Python 3, Git, Codex CLI가 필요하다. 실제 사용한 커밋과 버전을 반드시 기록한다. 작업용 저장소에 push하거나 릴리스 태그를 만들지 않는다.

```bash
git rev-parse HEAD
rustc --version
cargo --version
git --version
codex --version
uname -a
cargo check --locked
cargo build --locked
cargo clippy --locked -- -D warnings
cargo test --locked
```

기대 결과: 각 명령 종료 코드 0. 테스트 0개는 기능 검증 통과가 아니다. `tests/runtime_smoke.py`는 Windows의 reactor.exe 경로에 고정되어 있으므로 Linux에서 그대로 실행하지 않는다. 아래 CLI 시나리오로 검증하고 필요한 Linux 회귀 테스트를 별도로 작성한다. 오류가 나면 해당 단계에서 수정·관련 재검증 후 진행한다.

## 2. 전용 환경 준비

기존 Reactor 서비스와 다른 OS 계정 또는 별도 검증 서버를 권장한다. 실제 작업 Workspace와 인증 파일을 실험 대상으로 사용하지 않는다. 관련 볼륨마다 최소 10 GiB 및 10% 여유가 필요하다. 디스크 기준을 낮춰 실행시키지 않는다.

```bash
export M5_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/reactor-m5.XXXXXXXX")"
export REACTOR_CONFIG_DIR="$M5_ROOT/config"
export REACTOR_DATA_DIR="$M5_ROOT/data"
export M5_EVIDENCE="$M5_ROOT/evidence"
export REACTOR_BIN="$(pwd)/target/debug/reactor"
mkdir -p "$M5_EVIDENCE" "$M5_ROOT/workspace" "$M5_ROOT/outside"
umask 077
"$REACTOR_BIN" init --no-autostart
"$REACTOR_BIN" --output json status | tee "$M5_EVIDENCE/status-initial.json"
"$REACTOR_BIN" --output json doctor | tee "$M5_EVIDENCE/doctor.json"
git -C "$M5_ROOT/workspace" init
git -C "$M5_ROOT/workspace" config user.name 'Reactor M5'
git -C "$M5_ROOT/workspace" config user.email 'reactor-m5@example.invalid'
printf 'M5 baseline\n' > "$M5_ROOT/workspace/README.md"
git -C "$M5_ROOT/workspace" add README.md
git -C "$M5_ROOT/workspace" commit -m '초기 테스트 자료'
printf 'unchanged\n' > "$M5_ROOT/outside/canary.txt"
sha256sum "$M5_ROOT/outside/canary.txt" > "$M5_EVIDENCE/canary-before.sha256"
"$REACTOR_BIN" workspace add "$M5_ROOT/workspace" --id m5
codex login status
"$REACTOR_BIN" --output json workspace prepare m5 --agent codex | tee "$M5_EVIDENCE/prepare.json"
```

기대 결과: schema 10으로 초기화되고 서비스가 자동 등록되지 않는다. 아직 daemon이 없으므로 ready=false는 정상이다. Workspace는 prepare 성공 후 READY여야 한다. 인증이 없으면 실제 Agent 항목은 BLOCKED로 남기고 운영자가 해당 계정의 인증을 준비한다. 토큰을 문서·명령 기록에 붙여 넣지 않는다.

## 3. 기본 순차 실행

```bash
cat > "$M5_ROOT/plan.yaml" <<'YAML'
workspace_id: m5
title: M5 순차 실행
agent: codex
limits:
  execution_timeout_seconds: 120
  max_attempts: 3
tasks:
  - id: create
    context: Create handoff.txt containing the exact text REACTOR_M5_OK. Do not commit or push. Reply briefly.
  - id: consume
    context: Read handoff.txt and create confirmed.txt with its exact content. Do not commit or push. Reply briefly.
YAML
"$REACTOR_BIN" plan create --name m5-sequence --file "$M5_ROOT/plan.yaml"
"$REACTOR_BIN" plan review m5-sequence -y
"$REACTOR_BIN" schedule add m5-sequence --name m5-sequence --immediate
"$REACTOR_BIN" --output json run next | tee "$M5_EVIDENCE/sequence.json"
"$REACTOR_BIN" --output json run list --all | tee "$M5_EVIDENCE/runs.json"
"$REACTOR_BIN" workspace session m5
```

JSON 결과에서 Run ID를 확인해 다음 명령의 변수에 직접 입력한다. 실행 경로는 결과의 execution_path를 사용하며 원본 Workspace에 결과가 생길 것으로 가정하지 않는다.

```bash
export M5_RUN_ID='<Run ID>'
"$REACTOR_BIN" --output json run show "$M5_RUN_ID"
"$REACTOR_BIN" run result "$M5_RUN_ID"
"$REACTOR_BIN" run diff "$M5_RUN_ID"
"$REACTOR_BIN" log show "$M5_RUN_ID" --all
```

기대 결과: 두 Task가 순서대로 성공하고 동일 execution_path의 confirmed.txt가 REACTOR_M5_OK를 포함한다. Task 2가 미커밋 파일을 읽어야 한다. 원본 작업 파일은 바뀌지 않아야 한다. 각 시도의 session ID가 동일하고 실제 Codex JSONL의 thread.started ID와 일치해야 한다. session ID 누락·불일치나 resume 실패를 새 세션으로 조용히 전환하면 FAIL이다. 응답·사용량·로그·Diff를 구분해 확인한다.

## 4. 경고·실패·retry

위 YAML을 복사해 매번 다른 Plan·Schedule 이름으로 등록한다. 각 시험 후 다음 시험을 진행한다.

| 시나리오 | 입력과 진행 | 기대 결과 |
|---|---|---|
| 안내 경고 | Task 1 최종 응답을 아래 info JSON으로 지시 | 경고 기록 후 Task 2 실행 |
| 차단 경고 | level을 blocking으로 변경 | Task 2 미실행, WAITING_REVIEW 및 REVIEW_REQUIRED |
| 엄격 경고 | Task 1에 pause_on_warnings: true, info 경고 | 확인 대기 |
| 경고 확인 | `run continue <id>` 후 `run next` | 같은 회차·파일·세션을 유지해 후속 Task 실행 |
| 알 수 없는 경고 | level을 unknown으로 변경 | 확인 대기 |
| 실패 | 결정적 가짜 Agent가 비정상 종료 | 후속 Task 차단, 성공으로 표시하지 않음 |
| 명시 retry | `run retry <id>` 후 `run next` | 완료 Task를 반복하지 않고 실패 Task부터 재개, 시도 예산 유지 |
| 자동 retry | 가짜 Agent가 구조화 rate_limit_exceeded/server_error/service_unavailable 오류 후 비정상 종료 | 예산 내 1분/5분 지연, 문자열 메시지만으로 재시도하지 않음 |
| 안전 오류 | 세션 ID 불일치·소유권 상실 | 일반 경고 continue로 우회할 수 없음 |

```json
{"reactor_task_result":{"warnings":[{"level":"info","message":"M5 informational warning"}]}}
```

모델이 지시한 JSON을 생성하지 않으면 경고 시험은 미판정이다. 일반 메시지나 작업 내용 실패를 프로세스 실패로 추정하지 않는다. 실패·retry 시험은 운영 Codex 대신 별도 시험 계정/PATH의 가짜 codex 실행기를 사용해 exit code와 JSONL을 결정적으로 생성한다. 가짜 실행기는 version/help/login probe와 thread.started/item.completed/turn.completed 형식을 충족해야 한다. 준비되지 않았다면 BLOCKED로 남기며 실제 Codex 시험과 혼동하지 않는다.

## 5. 예약·동시성

- 같은 Workspace에 두 예약을 넣고 daemon으로 실행한다. 동시에 Agent가 두 개 실행되면 FAIL이다.
- 서로 겹치지 않는 두 Workspace와 전역 한도 2에서 병렬 실행을 확인한다.
- Workspace 하위 경로를 별도 Workspace로 등록하거나 추가 쓰기 경로를 겹치게 해 등록/실행 차단을 확인한다. 다른 data directory 시험은 별도 config/data를 사용하되 같은 OS 계정에서 수행한다.
- `schedule add <plan> --manual` 후 `schedule trigger <schedule>`로 수동 실행을 확인한다.
- `schedule add <plan> --at '<서버 현지 시각>' --start-deadline-seconds 1`로 실행 기한을 넘긴 뒤 MISSED와 Agent 미실행을 확인한다. 현지 시각 형식은 해당 바이너리의 `schedule add --help`와 일치시킨다.
- 반복 예약 문서는 `rule: {type: interval_after_success, seconds: 10}` 및 plan_id를 지정해 `schedule add --file <파일>`로 등록한다. 성공 후 다음 회차, 실패 후 대기·성공 retry 후 재개를 확인한다.
- 이전 회차 실행/경고 대기 중 신규 회차는 실행하지 않아야 한다. 중첩 건너뛰기는 감사 이벤트 SKIPPED_OVERLAP과 함께 확인한다.
- retry 또는 경고 확인 후 후속 Task에 첫 시작 기한이 다시 적용되면 FAIL이다. `schedule stop <id>`가 연결된 retry·대기 작업도 중단하는지 확인한다.

## 6. 취소·timeout·장애 복구

먼저 service를 설치하지 않고 foreground daemon을 별도 세션에서 실행한다. Bash로 백그라운드 실행할 경우 PID를 즉시 보관한다.

```bash
"$REACTOR_BIN" daemon run > "$M5_EVIDENCE/daemon.log" 2>&1 &
M5_DAEMON_PID=$!
printf '%s\n' "$M5_DAEMON_PID" > "$M5_EVIDENCE/daemon.pid"
ps -p "$M5_DAEMON_PID" -o pid,ppid,args
"$REACTOR_BIN" --output json status
```

- 취소: 긴 테스트 작업 실행 중 `run cancel <id>`. 해당 프로세스 그룹이 종료되고 후속 Task는 실행되지 않아야 한다.
- timeout: 짧은 execution_timeout_seconds와 긴 시험 작업. LIMIT_REACHED와 프로세스 정리를 확인한다.
- 정상 종료: 위 PID가 해당 검증 daemon인지 확인한 뒤 `kill -TERM "$M5_DAEMON_PID"` 및 `wait "$M5_DAEMON_PID"`. 실서비스 PID를 사용하지 않는다.
- 비정상 종료: 별도 새 시험 회차에서 같은 방법으로 PID를 재확인한 뒤 SIGKILL을 보내고 daemon을 재시작한다. lease 만료 전에 중복 Agent가 시작되지 않아야 한다. 잔존 프로세스/그룹의 종료를 확인할 수 없으면 Workspace 점유를 유지해야 한다.
- 소유권 상실: 가짜 Agent 실행과 별도 DB fixture에서만 lease를 변조한다. 실제 DB에는 SQL을 실행하지 않는다. Agent 중단과 이전 owner의 완료 기록 거절을 확인한다.
- `run recover`는 Agent를 재실행하지 않고 상태·산출물만 복구해야 한다. 기준선이 없으면 빈 Diff를 만들어 성공처럼 보이지 않아야 한다.

시험마다 ps의 PID·PPID·PGID, Run JSON, daemon 로그, 감사 이벤트를 보존한다. 가짜 Agent 결과만으로 실제 Codex sandbox나 Linux 프로세스 격리가 검증됐다고 표시하지 않는다.

## 7. 경로 격리·기록·저장 실패

- 테스트 Agent에게 `$M5_ROOT/outside/canary.txt`를 바꾸도록 지시한다. 기본 권한에서는 쓰기가 거절돼야 하며 해시가 그대로여야 한다. 경로는 Plan context에 실제 문자열로 넣는다. 원본 사용자 파일을 대상으로 하지 않는다.
- 별도 정책 파일의 writable_roots에 전용 outside 경로를 명시하고 새 Workspace 정책 준비·새 Plan 승인 후에는 그 경로 쓰기가 가능해야 한다.
- Workspace 내부 symlink로 canary를 가리키는 시험도 수행한다. sandbox가 경계를 강제하지 못하면 운영 차단 결함이다.
- 새 실행 후 이전 Run의 result/Diff가 유지되는지 확인한다. 실행·복구·경고 대기 기록의 clear/log prune이 보호되는지 확인한다.
- 실제 비밀값 대신 시험용 환경 credential을 사용해 stdout·stderr·최종 응답·Diff의 마스킹을 확인한다. 마스킹은 알려진 환경값에 대한 best-effort이며 일반 secret scanner가 아니다.
- 디스크/저장 실패는 별도 소형 loopback 파일시스템이나 disposable VM에서만 주입한다. 신규 실행은 최소 10 GiB·10% 미달에서 차단돼야 하고 시도 수를 소비하지 않아야 한다. 실행 중 기준 미달은 경고, 실제 저장 실패는 중단/확인 대상이어야 한다.
- 로그 합계 5 GiB 경고와 삭제 중단 재개는 전용 파일시스템·DB fixture에서 시험한다. 실제 서버 디스크를 대량 파일로 채우지 않는다. 주입 환경이 없으면 해당 항목을 BLOCKED로 기록한다.

## 8. 유지보수와 백업·복원

진행 중 작업이 없는 상태부터 시작한다. foreground daemon은 TERM으로 종료하고 wait로 종료를 확인한다. 서비스 실행 중이면 `service stop`을 사용한다.

```bash
"$REACTOR_BIN" maintenance enter --timeout-seconds 30
"$REACTOR_BIN" maintenance status
# Stop the verified test daemon before backup.
"$REACTOR_BIN" backup create "$M5_ROOT/backup-good"
"$REACTOR_BIN" backup restore "$M5_ROOT/backup-good" --replace
"$REACTOR_BIN" --output json status
```

기대 결과: 유지보수 중 신규 작업 차단, backup에 config.json·reactor.db·Run 로그만 포함, 복원 후 maintenance=true와 Workspace 재준비 요구. 서비스·세션·기존 worktree 회차가 자동 재개되면 FAIL이다. Workspace/Git/Agent 세션 파일은 별도 복원 전략의 대상이다.

검증용 백업 사본을 만든 뒤 config.json을 변조해 restore가 checksum 오류로 거절하고 현재 데이터를 교체하지 않는지 확인한다. 원본 backup-good는 보존한다. restore --replace 중단 시험은 disposable 데이터에서만 수행하며 restore.json이 남은 상태에서 일반 명령이 차단되고 같은 원본 백업으로 재실행하면 완료돼야 한다. pre-restore 보관 디렉터리는 원본 파일 보관이며 자동 rollback 명령은 아니다.

schema 9 migration은 구버전으로 만든 폐기 가능한 별도 config/data와 DB 사본에서 수행한다. 승인·예약·이력 개수와 내용을 전후 비교하고 pre-v10 SQLite 백업을 확인한다. 새 schema 파일을 구버전 바이너리로 열지 않는다. 운영 DB를 직접 이전 시험에 사용하지 않는다.

## 9. 선택: systemd·전용 계정·재부팅

전용 비root 계정을 미리 준비하고 그 계정으로 Agent 인증과 init을 수행한다. config/data directory는 해당 계정 소유여야 한다. mktemp 경로는 재부팅 후 삭제될 수 있으므로 이 단계에는 관리자가 정한 영속 검증 경로를 사용한다. 위 환경 변수와 REACTOR_BIN을 영속 경로로 다시 설정한다.

```bash
# Run only after confirming the dedicated account and persistent test paths.
export M5_USER='<전용 계정>'
sudo env REACTOR_CONFIG_DIR="$REACTOR_CONFIG_DIR" REACTOR_DATA_DIR="$REACTOR_DATA_DIR" \
  "$REACTOR_BIN" service install-system --user "$M5_USER"
sudo env REACTOR_CONFIG_DIR="$REACTOR_CONFIG_DIR" REACTOR_DATA_DIR="$REACTOR_DATA_DIR" \
  "$REACTOR_BIN" service start
```

등록 결과의 instance UUID로 `reactor-<UUID>.service`를 식별한다. `systemctl status`와 `journalctl -u`로 전용 계정·실행 바이너리·경로·PATH·인증과 readiness를 확인한다. 유지보수는 자동 해제되지 않아야 한다. 재개는 서비스 계정의 해당 환경에서 maintenance leave로 명시한다. 정상 stop의 drain, DB 손상 fixture에서도 서비스 status/stop이 가능한지, 재시작 후 로그·예약·소유권 복구를 확인한다. 재부팅은 관리자와 점검 시간을 합의한 별도 검증 서버에서만 수행한다. 실제 운영 서버를 가이드 때문에 자동 재부팅하지 않는다.

## 10. 증거와 정리

```bash
"$REACTOR_BIN" --output json run list --all > "$M5_EVIDENCE/runs-final.json"
"$REACTOR_BIN" --output json schedule list > "$M5_EVIDENCE/schedules-final.json"
"$REACTOR_BIN" --output json events --after 0 --limit 10000 > "$M5_EVIDENCE/events.json"
"$REACTOR_BIN" --output json status > "$M5_EVIDENCE/status-final.json"
sha256sum "$M5_ROOT/outside/canary.txt" > "$M5_EVIDENCE/canary-after.sha256"
```

| 항목 | 결과 | 기대/실제 차이 | 증거 파일 | 수정 커밋·재검증 |
|---|---|---|---|---|
| 빌드·Clippy | 미실행 | | | |
| 기본·순차 Task | 미실행 | | | |
| 경고·실패·retry | 미실행 | | | |
| 세션·예약·동시성 | 미실행 | | | |
| 격리·취소·timeout·복구 | 미실행 | | | |
| 기록·마스킹·저장 실패 | 미실행 | | | |
| schema 9 이전·백업·복원 | 미실행 | | | |
| Linux 서비스·再起動 | 미실행 | | | |

검증 후 신규 작업을 막고 실행 중 프로세스를 정상 종료한다. 설치한 테스트 서비스만 UUID를 확인해 uninstall한다. 테스트 worktree는 `git -C <검증 원본> worktree list`로 위치를 확인한 뒤 필요 자료를 보존하고 `git worktree remove <해당 경로>`로 정리한다. 로그·결과·백업을 먼저 보관하며 인증값이 들어간 자료는 공유하지 않는다. M5_ROOT를 출력·확인한 뒤 관리자가 해당 테스트 디렉터리만 정리한다. 광범위한 rm/pkill 명령은 사용하지 않는다.

M6 진입 조건은 필수 시나리오 PASS와 운영 차단 결함 해소다. BLOCKED·미실행을 PASS로 바꾸지 않는다. Agent 업무 결과의 정확성은 이 가이드의 판정 범위가 아니다. Git 게시 추가 시험이 필요하면 임시 로컬 bare 저장소만 사용하고 외부 원격 저장소에 push하지 않는다.