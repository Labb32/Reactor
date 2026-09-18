# Sandbox 책임 경계

Reactor는 Workspace의 허용 범위를 관리하고 Agent의 sandbox 기능으로 이를 강제한다. OS별 sandbox를 직접 구현하지 않는다.

## Reactor가 책임지는 부분

- Workspace와 추가 쓰기 경로를 Canonical Path로 저장한다.
- 경로 중첩과 다른 Workspace의 쓰기 경로 충돌을 막는다.
- 실행 직전에 쓰기 경로를 다시 정규화하고 Reactor 저장 경로와의 중첩을 거절한다.
- Plan이 Workspace 정책보다 넓은 권한을 요청하지 못하게 한다.
- Agent가 필요한 정책을 지원하는지 실행 전에 확인한다.
- Workspace 정책을 Agent별 실행 옵션으로 변환한다.
- 사용자 승인을 기다리지 않는 비대화형 모드로 실행한다.
- 쓰기 경로 전체를 실행 Lock, Checkpoint와 Diff 범위로 사용한다.
- 정책 변경을 fingerprint로 감지하고 Workspace를 `DEGRADED`로 전환한다.
- 권한 부족 시 권한을 자동으로 넓히지 않고 Run을 `REVIEW_REQUIRED`로 끝낸다.
- 실행 정책과 상태 변경을 감사 이력에 남기되 Secret 값은 기록하지 않는다.

Reactor는 개별 CLI 명령이나 Git 하위 명령을 등록하지 않는다. 일반 CLI와 로컬 Git 작업은 Agent sandbox 안에서 실행된다.

## Agent 설정에 맡기는 부분

- 등록된 경로 밖의 파일 쓰기 차단
- Network 허용 여부의 실제 강제
- MCP와 Hook 실행은 Agent 설정과 sandbox 동작에 맡김
- CLI와 자식 Process에 같은 sandbox 적용
- 승인 요청을 만들지 않고 허용되지 않은 작업을 거절
- 인증 상태, sandbox capability와 실행 결과 제공

Workspace 정책에서 Network, MCP 또는 Hook을 허용해도 Agent가 해당 기능을 지원하지 않으면 그 Agent는 `READY`가 될 수 없다. MCP와 Hook의 세부 권한은 Agent 설정의 책임이며, Reactor의 파일 경로 보장은 Agent가 직접 실행하는 작업에 적용된다.

## Reactor가 별도로 관리하지 않는 부분

- Codex, Git, GitHub, Cloud CLI와 API의 Credential 생성·저장·갱신
- CLI와 개발 도구의 설치 및 업데이트
- MCP 서버와 Hook의 정의 자체
- Git commit 이후의 버전 이력과 원격 저장소 상태
- API, 원격 저장소와 외부 서비스에서 발생한 변경의 Rollback

Agent Process는 현재 사용자 계정으로 실행한다. 기존 Agent 로그인, Git Credential Manager, SSH Agent와 CLI 인증을 그대로 사용할 수 있지만 Reactor는 Credential 값을 읽거나 DB와 로그에 복사하지 않는다. 인증이 없거나 만료되면 자동 로그인하지 않고 Run 결과에 사유를 남긴다.

## Workspace 정책

Workspace는 항상 기본 쓰기 경로다. 추가 경로와 외부 기능만 정책에 선언한다.

```yaml
writable_roots:
  - ../shared-library
network: allow
mcp: allow
hooks: allow
```

- `writable_roots`는 등록 시 Canonical Path로 변환한다.
- `network`, `mcp`, `hooks`는 개별 요청이나 명령 목록이 아닌 Workspace 단위 권한이다.
- 세 항목은 기본적으로 `allow`이며 필요하면 Workspace별로 `deny`할 수 있다.
- 이전 Schema에서 이전된 Workspace는 기존 실행 범위를 유지하기 위해 세 항목을 `deny`로 시작한다.
- 지원하지 않는 항목은 허용된 것처럼 처리하지 않고 준비 또는 실행을 거절한다.
- 실행 도중 정책을 바꾸지 않는다. 변경된 정책은 다시 준비한 다음 새 Run부터 적용한다.
- 같은 ID와 경로로 Workspace를 다시 등록하면 정책을 갱신하고 기존 Agent 준비 상태를 무효화한다.

v1은 Network 목적지, CLI 명령과 Credential별 allowlist를 제공하지 않는다. 세밀한 제한이 필요하면 Network proxy나 격리된 실행 계층을 별도 기능으로 추가한다.

## 보장 범위

기본 보장은 등록된 경로 밖의 변경 차단이다. 파일 읽기와 Network 목적지까지 격리할 수 있는지는 Agent와 OS의 sandbox capability에 달려 있다. Agent가 보장하지 못하는 제한을 Reactor의 보안 기능으로 표시하지 않는다.
