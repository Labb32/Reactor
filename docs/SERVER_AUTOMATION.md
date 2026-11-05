# 서버 자동화 구현 상태

M3 코드 반영 상태다. 검사·빌드·테스트·Agent 실행·실제 서비스 설치·백업/복원 검증은 수행하지 않았다. 운영 활성화 완료를 의미하지 않는다.

## 실행 모델

기존 단일 Plan 입력은 유지한다. JSON/YAML Plan에 순서 있는 tasks를 지정하면 Codex 우선의 순차 실행을 사용한다. id·context 또는 context_file·pause_on_warnings를 지원한다. context_file은 등록 시 내용으로 고정한다. 같은 회차는 하나의 worktree를 공유하고 미커밋 변경을 후속 Task에 전달한다. commit·push·merge는 Reactor가 관리하지 않으며 Agent 지시로 수행한다.

종료 코드가 실행 성공의 기준이다. 선택적 최종 JSON 보고의 reactor_task_result.warnings는 info/차단 경고를 구분한다. 차단 경고·알 수 없는 보고는 확인 대기하며 reactor run continue <id>로 일반 경고를 확인할 수 있다. 격리·소유권·세션 이상은 이 명령으로 우회하지 않는다. 실패 후 retry는 진행 상태·예약 연결을 유지한다. 확인된 구조화 일시 오류만 제한된 자동 retry를 사용한다.

실행 시간 입력은 execution_timeout_seconds이며 기존 max_wall_time_seconds는 입력 별칭이다. CLI는 --execution-timeout-seconds와 기존 옵션 별칭을 지원한다. JSON 출력은 기존 envelope에 필드를 추가하며 별도 2.0 전환은 하지 않는다. schema 9→10은 기존 데이터와 승인·예약을 보존하고 먼저 SQLite 백업을 남긴다.

## 격리와 세션

경로 루트의 배타 잠금과 상위 경로의 공유 잠금으로 겹치는 쓰기를 막는다. 잠금은 동일 OS 계정의 여러 data directory에도 적용한다. 파일 접근 강제는 Agent sandbox와 서버 계정 권한에 맡긴다. worktree는 보안 sandbox가 아니며 Git 관리 영역을 원본 저장소와 공유한다.

Agent 작업과 CLI probe는 명시한 환경만 전달한다. 공통 환경은 경로·사용자 홈·OS·임시 디렉터리·locale·proxy/CA·SSH agent·Git SSH·사용자 서비스 bus 설정이다. Codex에는 CODEX_HOME/CODEX_API_KEY/OPENAI_API_KEY/OPENAI_BASE_URL/OPENAI_ORG_ID/OPENAI_PROJECT_ID, Claude에는 CLAUDE_CONFIG_DIR/ANTHROPIC_API_KEY/ANTHROPIC_AUTH_TOKEN/ANTHROPIC_BASE_URL, Qwen에는 QWEN_HOME/QWEN_API_KEY/DASHSCOPE_API_KEY/OPENAI_API_KEY/OPENAI_BASE_URL/GEMINI_API_KEY를 전달한다. 다른 daemon 인증 환경은 상속하지 않는다. 공유 CLI 로그인은 유지하며 인증을 저장·갱신하는 broker는 만들지 않는다.

신규 Workspace는 network 허용, MCP/hooks 거부가 기본이다. 기존 저장된 명시 정책은 유지한다. 세션은 다중 Task에서 영속화하고 명시 ID로 재개한다. workspace session/session-reset으로 참조를 관리한다. 초기화는 Agent 세션 파일을 삭제하지 않는다. 단일 legacy 작업은 ephemeral을 유지한다.

## 운영 명령

- reactor status / reactor doctor: readiness·DB·설정 적용 차이·디스크·로그 상태. Agent probe를 실행하지 않는 운영 조회다.
- reactor maintenance enter: 신규 실행 차단과 기본 30분 drain. timeout이면 cancel을 요청하고 잠금을 유지한다.
- reactor maintenance leave: 운영자가 명시적으로 신규 작업을 허용한다.
- reactor run recover: Agent를 다시 실행하지 않고 안전한 상태/산출물 복구만 수행한다.
- reactor service install-system --user <계정>: Linux의 기존 비root 계정을 사용한다. config/data는 해당 계정 소유여야 하며 계정·Agent 로그인·PATH는 운영자가 준비한다. 기존 user service도 유지한다.

디스크는 관련 볼륨별 10 GiB·10% 여유가 모두 필요하다. 기준 미달은 신규 실행을 막고, 실행 중에는 경고한다. 실제 로그 저장 실패는 중단/확인 대상으로 처리한다. 로그 합계 5 GiB 경고를 제공한다. 알려진 환경 인증값에 대한 마스킹은 best-effort이며 미지의 비밀값 탐지를 보장하지 않는다.

## 백업과 복원

유지보수에 진입하고 daemon을 정상 중지한 다음 reactor backup create <새 디렉터리>를 사용한다. 설정·일관된 SQLite snapshot·Run 로그만 포함하며 Workspace·Git·Agent 세션 파일·OS 서비스 등록·인증 파일은 포함하지 않는다. 정기 스케줄·암호화·외부 보존은 운영 측에서 관리한다.

reactor backup restore <디렉터리> --replace는 경로·체크섬을 확인하고 기존 데이터를 pre-restore 보관 디렉터리에 남긴다. 복원 journal이 남으면 같은 백업으로 명령을 재실행한다. 중간 상태에서 일반 App 동작과 초기화를 막는다. 복원 후 유지보수·Workspace 재준비를 요구하며 세션 참조와 백업되지 않은 worktree 회차를 자동 재개하지 않는다. 새 회차 시작은 운영자가 외부 Workspace 복구와 Agent 준비를 확인한 뒤 허용한다.

## M4·M5 전달

M4에서는 CLI/JSON 예시·호환성·배포/업그레이드 문서를 실제 결과에 맞춘다. M5에서 프로세스 중단·Windows Job/ACL·Linux 서비스 계정·경로 alias/추가 쓰기 범위·작업 간 변경 인계·경고 확인·세션 재개·retry·디스크 부족·저장 실패·삭제 재개·백업/복원 중단을 검증한다. 테스트 수행은 별도 사용자 요청이 있을 때 진행한다.

Codex 세션 명령과 JSONL 형식 참고: https://learn.chatgpt.com/docs/non-interactive-mode 및 https://learn.chatgpt.com/docs/cli/reference. 실제 설치 버전과 외부 worktree 호환성은 미검증이다. shell 실행은 후속 범위다.