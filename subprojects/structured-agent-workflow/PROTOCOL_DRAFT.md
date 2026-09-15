# Workflow Protocol 초안

## Authoring Plan

```yaml
schema: reactor.workflow/v1alpha1
workspace: example
entry: inspect

context:
  objective: "안전한 의존성 업데이트"
  constraints:
    - "Major Version은 적용하지 않는다"

nodes:
  inspect:
    type: task
    instruction: "업데이트 후보를 분류한다."
    output_schema:
      type: object
      required: [safe]
      properties:
        safe:
          type: array
          items: { type: string }
    next: choose

  choose:
    type: decision
    branches:
      - when:
          path: nodes.inspect.output.safe
          op: not_empty
        next: apply
    default: nothing

  apply:
    type: task
    instruction: "입력받은 Package만 갱신한다."
    inputs:
      packages:
        from: nodes.inspect.output.safe
    next: done

  nothing:
    type: succeed

  done:
    type: succeed
```

## Execution Envelope

Controller가 현재 Node만 일반 Reactor Context로 제출한다.

```json
{
  "protocol": "reactor.execution/v1alpha1",
  "workflow_run_id": "0199...",
  "node_execution_id": "0199...",
  "task": {
    "id": "apply",
    "instruction": "입력받은 Package만 갱신한다.",
    "inputs": { "packages": ["a", "b"] },
    "constraints": ["Major Version은 적용하지 않는다"]
  },
  "result_contract": {
    "status": ["completed", "blocked", "failed"],
    "output_schema": { "type": "object" }
  }
}
```

## Result Envelope

```json
{
  "protocol": "reactor.result/v1alpha1",
  "workflow_run_id": "0199...",
  "node_execution_id": "0199...",
  "status": "completed",
  "summary": "a와 b를 갱신했다.",
  "output": { "updated": ["a", "b"] },
  "evidence": [{ "kind": "file", "path": "package.json" }],
  "issues": []
}
```

분기는 `output`만 사용한다. `summary`는 사용자 표시용이다.

## 조건

```yaml
when:
  all:
    - path: nodes.verify.status
      op: equals
      value: completed
    - path: nodes.verify.output.outcome
      op: equals
      value: passed
```

v1 연산자:

```text
equals, not_equals, exists, not_empty, in,
greater_than, less_than, all, any, not
```

## 상태

```text
PENDING, READY, RUNNING, COMPLETED,
BLOCKED, FAILED, SKIPPED, INVALID_OUTPUT
```

## 전달 규칙

포함:

- 현재 Task
- 공통 목표와 제약
- 선언된 이전 Output
- 완료 조건과 Result Schema

제외:

- 전체 Graph
- 이전 Transcript
- 참조하지 않은 Output
- 다른 Branch의 지시

## Format Repair

Result가 Schema를 만족하지 않으면 작업을 다시 수행시키지 않고 Validation Error, 요구 Schema와 이전 결과만 전달한다. 기본 1회로 제한한다.

## 미결정

- JSON Schema Draft
- Agent별 Structured Output 강제 방식
- 공통 Context 중복 제거
- Group의 Input·Output Scope
