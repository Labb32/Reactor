# 패치 노트

## 0.1.7

- `REVIEW_REQUIRED` Run의 중단 사유와 로그·Diff 정보를 확인할 명령이 없던 문제 수정.

## 0.1.6

- 텍스트 출력 시간이 읽기 어려운 UTC 원문으로 표시되고 Run 시작·완료 시각이 누락되던 문제 수정.

## 0.1.5

- 승인된 Plan에서 `plan review`로 내용을 다시 확인할 수 없던 문제 수정.

## 0.1.4

- 빈 목록의 정상 여부와 Schedule·Run 상태 구분이 어려웠던 텍스트 출력 문제 수정.

## 0.1.3

- Plan을 긴 자동 생성 ID 대신 짧은 이름으로 참조할 수 없던 문제 수정.

## 0.1.2

- daemon 재시작 시 만료 전 lease를 가진 중단 Run이 `RUNNING` 상태에 남던 오류 수정.

## 0.1.1

- Windows에서 PowerShell 기반 Agent CLI에 stdin을 전달하면 실행이 실패하던 오류 수정.
