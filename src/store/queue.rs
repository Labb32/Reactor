use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::Store;
use crate::{config::ConcurrencyConfig, error::Result, schedule::QueueRun};

impl Store {
    pub fn queue(&self, config: &ConcurrencyConfig) -> Result<Vec<QueueRun>> {
        self.ensure_supported_schema()?;
        evaluate(&self.connection, config)
    }

    pub(crate) fn claim_next_run(
        &mut self,
        config: &ConcurrencyConfig,
        owner_id: &str,
    ) -> Result<Option<QueueRun>> {
        self.write_transaction(|transaction| {
            super::schedules::refresh_due(transaction)?;
            let selected = evaluate(transaction, config)?
                .into_iter()
                .filter(|run| run.eligible)
                .min_by_key(|run| run.dispatch_order);
            let Some(mut run) = selected else {
                return Ok(None);
            };
            let now = crate::time::now_utc();
            let changed = transaction.execute(
                "UPDATE runs SET state = 'PREPARING', updated_at = ?1
                 WHERE id = ?2 AND state = 'QUEUED'",
                params![now, run.id],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            transaction.execute(
                "INSERT INTO reactor_meta (key, value) VALUES ('queue_last_workspace', ?1)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [&run.workspace_id],
            )?;
            super::write_event(
                transaction,
                "RUN_STATE_CHANGED",
                "run",
                &run.id,
                serde_json::json!({ "from": "QUEUED", "to": "PREPARING" }),
                &now,
            )?;
            transaction.execute(
                "INSERT INTO leases (resource_type, resource_id, owner_id, acquired_at, expires_at)
                 VALUES ('run', ?1, ?2, ?3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+5 seconds'))",
                params![run.id, owner_id, now],
            )?;
            run.state = "PREPARING".to_owned();
            run.eligible = false;
            run.dispatch_order = None;
            run.blocked_by = None;
            Ok(Some(run))
        })
    }
}

fn evaluate(connection: &Connection, config: &ConcurrencyConfig) -> Result<Vec<QueueRun>> {
    let mut runs = query_queued_runs(connection)?;
    let active = query_active_runs(connection)?;
    let active_workspaces: HashSet<&str> =
        active.iter().map(|run| run.workspace_id.as_str()).collect();
    let mut agent_counts = counts(active.iter().map(|run| run.agent_kind.clone()));
    let mut provider_counts = counts(active.iter().filter_map(|run| run.provider.clone()));
    let mut endpoint_counts = counts(active.iter().filter_map(|run| run.local_endpoint.clone()));
    let mut active_count = active.len();
    let mut heads = Vec::new();
    let mut seen = HashSet::new();

    for (index, run) in runs.iter_mut().enumerate() {
        if !seen.insert(run.workspace_id.clone()) {
            run.blocked_by = Some("workspace_fifo".to_owned());
        } else if active_workspaces.contains(run.workspace_id.as_str()) {
            run.blocked_by = Some("workspace_active".to_owned());
        } else {
            heads.push(index);
        }
    }
    rotate_after_last_workspace(connection, &runs, &mut heads)?;

    let mut dispatch_order = 0_u32;
    for index in heads {
        let run = &mut runs[index];
        let blocked = blocked_by(
            run,
            config,
            active_count,
            &agent_counts,
            &provider_counts,
            &endpoint_counts,
        );
        if let Some(blocked) = blocked {
            run.blocked_by = Some(blocked.to_owned());
            continue;
        }
        run.eligible = true;
        run.dispatch_order = Some(dispatch_order);
        dispatch_order += 1;
        active_count += 1;
        increment(&mut agent_counts, run.agent_kind.clone());
        if let Some(provider) = &run.provider {
            increment(&mut provider_counts, provider.clone());
        }
        if let Some(endpoint) = &run.local_endpoint {
            increment(&mut endpoint_counts, endpoint.clone());
        }
    }
    Ok(runs)
}

fn blocked_by(
    run: &QueueRun,
    config: &ConcurrencyConfig,
    active_count: usize,
    agent_counts: &HashMap<String, usize>,
    provider_counts: &HashMap<String, usize>,
    endpoint_counts: &HashMap<String, usize>,
) -> Option<&'static str> {
    if active_count >= usize::from(config.global) {
        Some("global_limit")
    } else if count_of(agent_counts, &run.agent_kind) >= usize::from(config.per_agent) {
        Some("agent_limit")
    } else if run.provider.as_ref().is_some_and(|provider| {
        count_of(provider_counts, provider) >= usize::from(config.per_provider)
    }) {
        Some("provider_limit")
    } else if run.local_endpoint.as_ref().is_some_and(|endpoint| {
        count_of(endpoint_counts, endpoint) >= usize::from(config.per_local_endpoint)
    }) {
        Some("local_endpoint_limit")
    } else {
        None
    }
}

fn query_queued_runs(connection: &Connection) -> Result<Vec<QueueRun>> {
    let mut statement = connection.prepare(
        "SELECT id, workspace_id, state, agent_kind, provider, local_endpoint, ready_at, created_at
         FROM runs WHERE state = 'QUEUED'
         ORDER BY julianday(ready_at), julianday(created_at), rowid",
    )?;
    let rows = statement.query_map([], map_run)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn query_active_runs(connection: &Connection) -> Result<Vec<QueueRun>> {
    let mut statement = connection.prepare(
        "SELECT id, workspace_id, state, agent_kind, provider, local_endpoint, ready_at, created_at
         FROM runs WHERE state IN ('PREPARING', 'RUNNING')
         ORDER BY julianday(created_at), rowid",
    )?;
    let rows = statement.query_map([], map_run)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn map_run(row: &Row<'_>) -> rusqlite::Result<QueueRun> {
    Ok(QueueRun {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        state: row.get(2)?,
        agent_kind: row.get(3)?,
        provider: row.get(4)?,
        local_endpoint: row.get(5)?,
        ready_at: row.get(6)?,
        created_at: row.get(7)?,
        eligible: false,
        dispatch_order: None,
        blocked_by: None,
    })
}

fn rotate_after_last_workspace(
    connection: &Connection,
    runs: &[QueueRun],
    heads: &mut [usize],
) -> Result<()> {
    let last: Option<String> = connection
        .query_row(
            "SELECT value FROM reactor_meta WHERE key = 'queue_last_workspace'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(last) = last else {
        return Ok(());
    };
    if let Some(position) = heads
        .iter()
        .position(|index| runs[*index].workspace_id == last)
    {
        heads.rotate_left(position + 1);
    }
    Ok(())
}

fn counts(values: impl Iterator<Item = String>) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for value in values {
        increment(&mut counts, value);
    }
    counts
}

fn increment(counts: &mut HashMap<String, usize>, key: String) {
    *counts.entry(key).or_default() += 1;
}

fn count_of(counts: &HashMap<String, usize>, key: &str) -> usize {
    counts.get(key).copied().unwrap_or_default()
}
