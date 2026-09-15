# 작업 트리 형식 조사

## 결론

AI 작업 Plan을 교환하는 범용 표준은 없다. Structured Agent Workflow에는 기존 형식을 그대로 구현하기보다 필요한 개념만 가져온 작은 YAML 형식이 적합하다.

Reactor Core는 이 형식을 해석하지 않는다.

## 비교

| 형식 | 참고할 점 | 제외 이유 |
|---|---|---|
| Amazon States Language | `Task`, `Choice`, `Next`, `Retry`, `Fail` | AWS 개념과 JSON이 장황함 |
| BPMN 2.0 | 명시적 분기와 기본 경로 | XML과 전체 표준이 무거움 |
| Argo Workflows | 의존 상태와 구조화 Output | Kubernetes 전제 |
| GitHub Actions | `needs`, `if`, named output | CI 전용 기능이 많음 |
| LangGraph | State, Node, Conditional Edge | Python·JS 코드 중심 |
| Behavior Tree | Sequence, Fallback과 계층화 | 반복 Tick 실행 모델 |
| HTN/HDDL | 큰 목표의 계층 분해 | 자동 Planner용 형식 |

참고 자료:

- [Amazon States Language](https://docs.aws.amazon.com/step-functions/latest/dg/concepts-amazon-states-language.html)
- [BPMN 2.0](https://www.omg.org/spec/BPMN/2.0/PDF)
- [Argo DAG](https://argoproj.github.io/argo-workflows/walk-through/dag/)
- [GitHub Actions 문법](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax)
- [LangGraph Graph API](https://langchain-ai.github.io/langgraph/concepts/multi_agent/)
- [BehaviorTree.CPP](https://behaviortree.dev/docs/nodes-library/FallbackNode/)
- [HDDL](https://openreview.net/pdf?id=HJeT8HBbt4)

## 권장 형식

작성용 YAML을 승인할 때 Canonical JSON으로 고정한다.

```yaml
schema: reactor.workflow/v1
workspace: example
entry: inspect

nodes:
  inspect:
    type: task
    instruction: "업데이트 후보를 조사한다."
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
    instruction: "안전한 업데이트만 적용한다."
    next: done

  nothing:
    type: succeed

  done:
    type: succeed
```

## 규칙

- Node Type은 `task`, `decision`, `succeed`, `fail`로 시작한다.
- 순서는 `next`, 분기는 `decision`만 사용한다.
- Task의 자연어 Summary와 분기용 Output을 분리한다.
- Output은 JSON Schema로 검증한다.
- 모든 Decision은 `default` 경로를 가진다.
- Retry 외 Cycle과 Node 병렬 실행은 v1에서 금지한다.
- Shell, JavaScript와 Python `eval` 조건은 허용하지 않는다.
- 초기 조건은 `equals`, `exists`, `not_empty`, `in`, `all`, `any`, `not`으로 제한한다.

표현력이 부족해지면 안전한 내장 표현식 언어인 [CEL](https://cel.dev/overview/cel-overview)을 검토한다.

## Task 결과

```yaml
status: completed
summary: "안전한 후보 2개를 찾았다."
output:
  safe: [a, b]
evidence:
  - path: package.json
```

분기는 `output`만 사용한다. Schema를 만족하지 못하면 `INVALID_OUTPUT`으로 처리하고 결과 형식 수정만 한 번 요청한다.

## 승인 전 검사

- 존재하지 않는 `next`
- 도달할 수 없는 Node
- Retry 외 Cycle
- 종료 경로 누락
- Decision의 `default` 누락
- Output 참조와 Schema Type 불일치
- Node 수와 Graph 깊이 제한 초과
