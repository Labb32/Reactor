# 서버 자동화 운영

구현 코드는 미검증 상태다. 검사·빌드·테스트·서버 명령을 이번 작업에서 실행하지 않았다.

## 순차 Task 입력

기존 context 단일 입력을 유지한다. 다중 작업은 Plan JSON/YAML의 tasks 배열에 id와 context 또는 context_file을 순서대로 지정한다. 공통 context는 공통 지시로 사용한다. context_file은 등록 시 snapshot으로 고정한다. pause_on_warnings를 true로 설정하면 안내 경고에도 대기한다. 기본 실행 시간 키는 execution_timeout_seconds이며 기존 max_wall_time_seconds는 입력 별칭이다.

```yaml
workspace_id: example
title: 문서 갱신
agent: codex
limits:
  execution_timeout_seconds: 7200
  max_attempts: 3
tasks:
  - id: inspect
    context: 변경 대상을 조사하고 요약한다.
  - id: update
    context: 조사한 내용을 바탕으로 문서를 갱신한다.
    pause_on_warnings: true
```

정상 종료와 안내 경고는 진행하고 차단·판별 불가 경고는 확인 대기한다. run continue <id>는 일반 경고만 확인한다. 안전 오류는 우회하지 않는다. worktree는 동일 회차 내 변경 인계 수단이며 보안 sandbox가 아니다. Agent sandbox와 서버 계정 권한으로 경계를 강제한다. commit·push는 Agent 작업 지시에 맡긴다.

## 운영

status/doctor는 준비 상태·DB·저장 공간·적용 설정을 보여준다. maintenance enter는 신규 실행을 차단하고 기본 30분 drain을 수행한다. timeout이면 취소를 요청하고 잠금을 유지한다. maintenance leave는 명시적 재개다. run recover는 Agent 재실행 없이 상태·산출물을 복구한다.

workspace session <id>로 명시적 Codex 세션 참조를 확인하고 session-reset으로 유휴 상태에서 초기화한다. 초기화는 Agent 세션 파일 삭제가 아니다. 세션/외부 worktree 호환성은 실제 설치 버전에서 검증해야 한다.

Agent 환경 전달 목록과 격리·디스크·로그 정책은 SERVER_AUTOMATION.md를 따른다. 임의 daemon 환경을 상속하지 않으므로 사용자 정의 인증 설정은 운영 준비 시 확인해야 한다.

## 백업

유지보수에 진입하고 daemon을 중지한 상태에서 backup create <새 디렉터리>를 수행한다. Reactor 설정·DB 기록·로그만 포함한다. Workspace·Git·Agent 세션 파일·OS 서비스 등록·인증 파일은 포함하지 않는다. 정기 실행·암호화·외부 보존은 운영 정책에 맡긴다. restore --replace는 기존 파일을 보관하고 유지보수를 유지한다. 미완료 journal이 있으면 같은 원본 백업으로 재실행한다.
[Linux 서버 M5 테스트 가이드](LINUX_TEST_GUIDE.md)
