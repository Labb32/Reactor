# CLI contract

Reactor commands use text output by default. Automation should pass `--output json`; event streams use `--output jsonl`.

## Envelope

```json
{
  "schema_version": "1.0",
  "instance_id": "0199...",
  "timestamp": "2026-09-15T06:00:00Z",
  "data": {}
}
```

- Field names use `snake_case`.
- IDs use UUID v7.
- Timestamps use RFC 3339 UTC.
- A minor schema release may add fields. Consumers must ignore unknown fields.
- A major schema release may change or remove fields.

## Errors

```json
{
  "schema_version": "1.0",
  "instance_id": null,
  "timestamp": "2026-09-15T06:00:00Z",
  "error": {
    "code": "uninitialized_database",
    "message": "database has not been initialized; run `reactor init`",
    "details": {},
    "retryable": false
  }
}
```

| Exit code | Meaning |
|---|---|
| `0` | Success |
| `1` | Runtime, I/O, configuration, or database failure |
| `2` | Invalid command input |
| `3` | Database initialization or schema mismatch |
| `4` | Cursor or idempotency conflict |

JSON responses go to stdout. Human diagnostics go to stderr.

## Input

Commands that accept a document use `--file <path>` or `--file -` for stdin. `.json`, `.yaml`, and `.yml` are recognized; stdin and unknown extensions try JSON followed by YAML.

Mutation commands accept an `idempotency_key`. Reusing a key with the same request returns the stored response. Reusing it with different input returns `idempotency_conflict`.

## Events

```shell
reactor events --after <cursor> --output jsonl
reactor events --after <cursor> --follow --output jsonl
```

Each line is an independent envelope. Cursors increase within one Reactor instance. Events are retained for 90 days or 100,000 entries. An expired cursor returns `cursor_expired`; the client must fetch a new snapshot.
