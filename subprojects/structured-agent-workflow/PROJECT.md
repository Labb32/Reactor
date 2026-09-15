---
type: internal-project
parent: REACTOR
status: Planning
---

# Structured Agent Workflow

## 목적

긴 자연어 요청을 검증 가능한 작업 Graph와 작은 Agent 요청으로 나눈다.

목표는 다음과 같다.

- 작업 순서와 조건을 명시적으로 전달
- 각 Agent 호출에 필요한 Context만 전달
- 자연어 Summary와 분기용 Output 분리
- 작은·로컬 모델의 작업 부담 감소
- 부분 실패와 실행 경로 추적

Reactor Core의 필수 의존성이 아니다. 사용하지 않으면 검토된 일반 Text·Markdown Context를 Reactor에 직접 예약한다.

## 구조

```text
자연어 목표
→ Planning Agent가 Workflow 초안 작성
→ 사용자 검토·승인
→ Controller가 현재 Node Context 생성
→ Reactor에 일반 Run 요청
→ Result 검증
→ 다음 Node 선택
```

형식은 세 가지로 나눈다.

| 형식 | 역할 |
|---|---|
| Authoring Plan | 사람이 검토하는 전체 Graph |
| Execution Envelope | 현재 Node에 필요한 최소 입력 |
| Result Envelope | Summary, 구조화 Output과 근거 |

## 책임

### Workflow Controller

- Graph 검증과 분기
- 필요한 이전 Output만 선택
- 각 Node를 일반 Reactor Context로 제출
- Result Schema 검증과 Format Repair
- 전체 Workflow 상태 저장

### Reactor Core

- Workspace Queue와 Agent Process 실행
- Sandbox, Timeout, 로그와 Run Diff
- 일반 Run 결과 반환

Reactor는 Workflow Schema를 해석하지 않는다. Controller는 `source_ref`, `correlation_id`, `approval_ref`로 Run을 연결한다.

## Context 최소화

Worker Agent에는 다음만 전달한다.

- 현재 Task
- 공통 목표와 제약
- 현재 Task가 참조한 이전 Output
- 완료 조건과 Result Schema
- 실행 한도

전체 Graph, 이전 Transcript, 사용하지 않는 Output과 다른 Branch의 지시는 전달하지 않는다.

## 원칙

- Control Flow는 Prompt 밖에서 처리한다.
- 가능한 판단은 Enum과 구조화 Output으로 받는다.
- 자연어 결과를 다시 해석해 분기하지 않는다.
- Schema 오류 시 파일 작업을 반복하지 않고 결과 형식만 수정한다.
- 너무 작은 작업으로 나눠 호출 고정 비용을 키우지 않는다.
- v1은 Node를 순차 실행하고 Retry 외 Cycle을 금지한다.

## 가설과 지표

구조화가 항상 효율적이라고 가정하지 않는다. 같은 작업의 단일 Prompt 방식과 비교한다.

| 지표 | 확인할 내용 |
|---|---|
| 완료율 | 전체 목표 달성 여부 |
| 제약 위반 | 범위·순서·금지 작업 위반 |
| 전체 Token | 모든 호출의 Input·Output 합계 |
| 호출·재시도 | 성공까지 필요한 횟수 |
| 분기 정확도 | 기대 경로 선택 여부 |
| 경량 모델 완료율 | 작은 모델에서의 효과 |
| 검토 시간 | 사용자가 Plan과 결과를 확인하는 시간 |

## 범위

v1:

- `task`, `decision`, `succeed`, `fail`
- `next`, `branches`, `default`
- JSON Schema Output
- 제한된 조건 연산자
- Graph 정적 검사
- Context Projection과 Format Repair

제외:

- Node 병렬 실행
- 자유로운 Cycle
- 임의 Expression 실행
- 범용 Workflow 표준 호환
- Reactor Core 내부 구현

## 산출물

- Authoring Plan Schema
- Execution·Result Envelope Schema
- Validator와 Condition Evaluator
- Context Projection 규칙
- CLI `validate`, `render`, `run`
- 단일 Prompt 비교 기준

## 관련 문서

- [Reactor](../../docs/REACTOR.md)
- [프로토콜](PROTOCOL_DRAFT.md)
- [형식 조사](TASK_TREE_FORMAT_RESEARCH.md)
