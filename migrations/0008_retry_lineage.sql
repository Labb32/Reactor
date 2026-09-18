ALTER TABLE runs ADD COLUMN retry_of TEXT REFERENCES runs(id);

UPDATE runs
SET retry_of = (
    SELECT json_extract(audit_events.payload_json, '$.retry_of')
    FROM audit_events
    WHERE audit_events.event_type = 'RUN_QUEUED'
      AND audit_events.entity_type = 'run'
      AND audit_events.entity_id = runs.id
      AND json_extract(audit_events.payload_json, '$.retry_of') IS NOT NULL
    ORDER BY audit_events.cursor
    LIMIT 1
);

CREATE INDEX runs_by_retry_source
ON runs(retry_of)
WHERE retry_of IS NOT NULL;
