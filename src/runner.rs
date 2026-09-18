use std::{
    io::Write,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    agents::{AgentCommand, AgentKind},
    app::App,
    checkpoint,
    domain::{PlanStatus, RunState},
    error::{ReactorError, Result, io_error},
    paths::ReactorPaths,
    plan::{PlanVersionRecord, validate_requirements, validate_tools},
    process_supervisor::ProcessSupervisor,
    run::{BeginAttempt, ExecutionRun, RunRecord},
    run_log::{self, RunLog},
    workspace::{WorkspacePolicy, canonicalize, fingerprint},
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(3);

pub fn execute_next(paths: &ReactorPaths, app: &mut App) -> Result<Option<RunRecord>> {
    prepare_execution(paths, app)?;
    execute_available(paths, app)
}

pub(crate) fn prepare_execution(paths: &ReactorPaths, app: &mut App) -> Result<()> {
    recover(paths, app)?;
    let runs_dir = paths.data_dir.join("runs");
    run_log::prune(&runs_dir).map_err(|source| io_error(&runs_dir, source))?;
    Ok(())
}

pub(crate) fn execute_available(paths: &ReactorPaths, app: &mut App) -> Result<Option<RunRecord>> {
    let owner_id = crate::ids::new_id();
    let Some(claimed) = app
        .store
        .claim_next_run(&app.config.concurrency, &owner_id)?
    else {
        return Ok(None);
    };
    let _heartbeat = Heartbeat::start(paths, &claimed.id, &owner_id);
    execute_claimed(paths, app, &claimed.id, &owner_id).map(Some)
}

fn execute_claimed(
    paths: &ReactorPaths,
    app: &mut App,
    run_id: &str,
    owner_id: &str,
) -> Result<RunRecord> {
    let execution = app.store.execution_run(run_id)?;
    let workspace = app.store.get_workspace(&execution.record.workspace_id)?;
    let roots = execution
        .policy
        .write_paths(Path::new(&workspace.canonical_path));
    if checkpoint::begin(paths, run_id, &execution.record.workspace_id, &roots).is_err() {
        let run = app.store.finish_run(
            run_id,
            None,
            RunState::Preparing,
            RunState::ReviewRequired,
            None,
            "checkpoint_unavailable",
        )?;
        app.store.release_run(run_id, owner_id)?;
        return Ok(run);
    }
    let run = execute_prepared(paths, app, execution, workspace)?;
    record_checkpoint_result(paths, app, &run)
}

fn execute_prepared(
    paths: &ReactorPaths,
    app: &mut App,
    execution: ExecutionRun,
    workspace: crate::workspace::WorkspaceRecord,
) -> Result<RunRecord> {
    let run_id = &execution.record.id;
    if execution.record.cancel_requested_at.is_some() {
        return app.store.finish_run(
            run_id,
            None,
            RunState::Preparing,
            RunState::Cancelled,
            None,
            "cancel_requested",
        );
    }
    let plan = app.store.get_plan(
        &execution.record.plan_id,
        Some(execution.record.plan_version),
    )?;
    let prepared = match prepare(
        &execution.record,
        &execution.policy,
        &plan.version,
        &workspace,
    )? {
        Preparation::Ready(prepared) => prepared,
        Preparation::ReviewRequired(reason) => {
            return app.store.finish_run(
                run_id,
                None,
                RunState::Preparing,
                RunState::ReviewRequired,
                None,
                &reason,
            );
        }
    };

    let attempt_id = crate::ids::new_id();
    let log_path = paths
        .data_dir
        .join("runs")
        .join(run_id)
        .join(format!("{attempt_id}.log"));
    let log_path_text = log_path.to_string_lossy().into_owned();
    let attempt = match app
        .store
        .begin_attempt(run_id, &attempt_id, &log_path_text)?
    {
        BeginAttempt::Started(attempt) => attempt,
        BeginAttempt::CancelRequested => {
            return app.store.finish_run(
                run_id,
                None,
                RunState::Preparing,
                RunState::Cancelled,
                None,
                "cancel_requested",
            );
        }
        BeginAttempt::LimitReached => {
            return app.store.finish_run(
                run_id,
                None,
                RunState::Preparing,
                RunState::LimitReached,
                None,
                "attempt_limit",
            );
        }
    };
    let log = match RunLog::open(&log_path) {
        Ok(log) => log,
        Err(_) => {
            return app.store.finish_run(
                run_id,
                Some(&attempt.id),
                RunState::Preparing,
                RunState::Failed,
                None,
                "log_unavailable",
            );
        }
    };

    let command = process_command(&prepared.command);
    let mut supervisor = match ProcessSupervisor::spawn(command) {
        Ok(supervisor) => supervisor,
        Err(_) => {
            return app.store.finish_run(
                run_id,
                Some(&attempt.id),
                RunState::Preparing,
                RunState::Failed,
                None,
                "spawn_failed",
            );
        }
    };
    let Some(stdout) = supervisor.take_stdout() else {
        let status = supervisor
            .stop(STOP_GRACE_PERIOD)
            .map_err(|source| io_error(&log_path, source))?;
        return app.store.finish_run(
            run_id,
            Some(&attempt.id),
            RunState::Preparing,
            RunState::Failed,
            status.code(),
            "output_unavailable",
        );
    };
    let Some(stderr) = supervisor.take_stderr() else {
        let status = supervisor
            .stop(STOP_GRACE_PERIOD)
            .map_err(|source| io_error(&log_path, source))?;
        return app.store.finish_run(
            run_id,
            Some(&attempt.id),
            RunState::Preparing,
            RunState::Failed,
            status.code(),
            "output_unavailable",
        );
    };
    let stdout_log = log.clone();
    let stderr_log = log.clone();
    let stdout_thread = thread::spawn(move || run_log::capture(stdout, stdout_log));
    let stderr_thread = thread::spawn(move || run_log::capture(stderr, stderr_log));

    if !app
        .store
        .mark_run_running(run_id, &attempt.id, supervisor.id())?
    {
        let status = supervisor
            .stop(STOP_GRACE_PERIOD)
            .map_err(|source| io_error(&log_path, source))?;
        let _ = finish_capture(stdout_thread, stderr_thread);
        let _ = RunLog::flush(&log);
        return app.store.finish_run(
            run_id,
            Some(&attempt.id),
            RunState::Preparing,
            RunState::Cancelled,
            status.code(),
            "cancel_requested",
        );
    }

    let Some(mut stdin) = supervisor.take_stdin() else {
        let status = supervisor
            .stop(STOP_GRACE_PERIOD)
            .map_err(|source| io_error(&log_path, source))?;
        let _ = finish_capture(stdout_thread, stderr_thread);
        return app.store.finish_run(
            run_id,
            Some(&attempt.id),
            RunState::Running,
            RunState::Failed,
            status.code(),
            "input_unavailable",
        );
    };
    let prompt = prepared.prompt;
    let stdin_thread = thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    let started = Instant::now();
    let outcome = match monitor(
        app,
        run_id,
        &mut supervisor,
        prepared.timeout,
        started,
        &log_path,
    ) {
        Ok(outcome) => outcome,
        Err(_) => {
            let status = supervisor
                .stop(STOP_GRACE_PERIOD)
                .map_err(|source| io_error(&log_path, source))?;
            let _ = join_input(stdin_thread);
            let _ = finish_capture(stdout_thread, stderr_thread);
            let _ = RunLog::flush(&log);
            return app.store.finish_run(
                run_id,
                Some(&attempt.id),
                RunState::Running,
                RunState::Failed,
                status.code(),
                "supervisor_error",
            );
        }
    };
    let stdin_result = join_input(stdin_thread);
    let capture_result = finish_capture(stdout_thread, stderr_thread);
    let flush_result = RunLog::flush(&log);
    let output = RunLog::tail(&log);
    let log_failed = capture_result.is_err() || flush_result.is_err() || output.is_err();
    let output = output.unwrap_or_default();
    let token_usage = prepared.agent.adapter().token_usage(&output);
    if let Some(tokens) = token_usage {
        app.store
            .record_attempt_usage(run_id, &attempt.id, tokens)?;
    }

    let (next, reason) = match outcome.reason {
        StopReason::Cancelled => (RunState::Cancelled, "cancel_requested"),
        StopReason::TimedOut => (RunState::LimitReached, "wall_time_limit"),
        StopReason::Exited if log_failed => (RunState::Failed, "log_io_error"),
        StopReason::Exited if stdin_result.is_err() => (RunState::Failed, "input_failed"),
        StopReason::Exited
            if !outcome.status.success()
                && prepared.agent.adapter().failure_requires_review(&output) =>
        {
            (RunState::ReviewRequired, "sandbox_denied")
        }
        StopReason::Exited if !outcome.status.success() => (RunState::Failed, "agent_exit"),
        StopReason::Exited
            if execution
                .record
                .limits
                .max_tokens
                .is_some_and(|limit| token_usage.is_some_and(|usage| usage > limit)) =>
        {
            (RunState::LimitReached, "token_limit")
        }
        StopReason::Exited
            if execution.record.limits.max_tokens.is_some() && token_usage.is_none() =>
        {
            (RunState::ReviewRequired, "token_usage_unavailable")
        }
        StopReason::Exited if outcome.status.success() => (RunState::Succeeded, "completed"),
        StopReason::Exited => (RunState::Failed, "agent_exit"),
    };
    app.store.finish_run(
        run_id,
        Some(&attempt.id),
        RunState::Running,
        next,
        outcome.status.code(),
        reason,
    )
}

pub fn recover(paths: &ReactorPaths, app: &mut App) -> Result<Vec<RunRecord>> {
    let owner_id = crate::ids::new_id();
    let mut recovered = app.store.recover_interrupted_runs(&owner_id)?;
    for run in &recovered {
        if checkpoint::pending_exists(paths, &run.id) {
            continue;
        }
        if !app.store.claim_run_lease(&run.id, &owner_id)? {
            continue;
        }
        let execution = app.store.execution_run(&run.id)?;
        let workspace = app.store.get_workspace(&run.workspace_id)?;
        let roots = execution
            .policy
            .write_paths(Path::new(&workspace.canonical_path));
        let heartbeat = Heartbeat::start(paths, &run.id, &owner_id);
        let checkpoint = checkpoint::begin(paths, &run.id, &run.workspace_id, &roots);
        drop(heartbeat);
        if checkpoint.is_err() {
            app.store.release_run(&run.id, &owner_id)?;
        }
    }
    for run_id in checkpoint::pending_run_ids(paths)? {
        let run = match app.store.get_run(&run_id) {
            Ok(run) => run,
            Err(ReactorError::RunNotFound(_)) => {
                checkpoint::complete(paths, &run_id)?;
                continue;
            }
            Err(error) => return Err(error),
        };
        if run.result.is_some() {
            checkpoint::complete(paths, &run_id)?;
            continue;
        }
        let Some(state) = RunState::parse(&run.state) else {
            continue;
        };
        if !state.is_terminal() {
            continue;
        }
        if !app.store.claim_run_lease(&run_id, &owner_id)? {
            continue;
        }
        let heartbeat = Heartbeat::start(paths, &run_id, &owner_id);
        let completed = record_checkpoint_result(paths, app, &run);
        drop(heartbeat);
        let completed = completed?;
        if let Some(existing) = recovered.iter_mut().find(|existing| existing.id == run.id) {
            *existing = completed;
        } else {
            recovered.push(completed);
        }
    }
    Ok(recovered)
}

fn record_checkpoint_result(
    paths: &ReactorPaths,
    app: &mut App,
    run: &RunRecord,
) -> Result<RunRecord> {
    let mut result = checkpoint::finish(paths, &run.id, &run.state)?;
    if let Some(attempt) = &run.attempt {
        result.exit_code = attempt.exit_code;
        result.termination_reason = attempt.termination_reason.clone();
        result.log_path = Some(attempt.log_path.clone());
    }
    let recorded = app.store.record_run_result(&run.id, &result)?;
    checkpoint::complete(paths, &run.id)?;
    Ok(recorded)
}

struct Heartbeat {
    stop: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Heartbeat {
    fn start(paths: &ReactorPaths, run_id: &str, owner_id: &str) -> Self {
        let database = paths.database_file();
        let run_id = run_id.to_owned();
        let owner_id = owner_id.to_owned();
        let (stop, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            let Ok(store) = crate::store::Store::open_initialized(&database) else {
                return;
            };
            loop {
                match receiver.recv_timeout(HEARTBEAT_INTERVAL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                match store.heartbeat_run(&run_id, &owner_id) {
                    Ok(true) => {}
                    Ok(false) => break,
                    Err(_) => continue,
                }
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct PreparedRun {
    agent: AgentKind,
    command: AgentCommand,
    prompt: String,
    timeout: Duration,
}

enum Preparation {
    Ready(PreparedRun),
    ReviewRequired(String),
}

fn prepare(
    run: &RunRecord,
    policy: &WorkspacePolicy,
    plan: &PlanVersionRecord,
    workspace: &crate::workspace::WorkspaceRecord,
) -> Result<Preparation> {
    if plan.status != PlanStatus::Approved.as_str()
        || plan.workspace_id != run.workspace_id
        || plan.agent.as_str() != run.agent_kind
        || plan.limits.max_wall_time_seconds != run.limits.max_wall_time_seconds
        || plan.limits.max_attempts < run.limits.max_attempts
        || plan.limits.max_tokens != run.limits.max_tokens
    {
        return Ok(Preparation::ReviewRequired("run_plan_mismatch".to_owned()));
    }
    let Some(agent) = AgentKind::parse(&run.agent_kind) else {
        return Ok(Preparation::ReviewRequired("agent_unsupported".to_owned()));
    };
    let workspace_policy = serde_json::to_vec(&workspace.policy)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    let run_policy = serde_json::to_vec(policy)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    if workspace.status != "READY"
        || workspace.policy_fingerprint != plan.workspace_policy_fingerprint
        || workspace_policy != run_policy
    {
        return Ok(Preparation::ReviewRequired(
            "workspace_policy_changed".to_owned(),
        ));
    }
    let ready = workspace.agents.iter().any(|record| {
        record.agent_kind == run.agent_kind
            && record.status == "READY"
            && record.policy_fingerprint.as_deref() == Some(&workspace.policy_fingerprint)
    });
    if !ready {
        return Ok(Preparation::ReviewRequired(
            "workspace_not_ready".to_owned(),
        ));
    }
    let canonical = match canonicalize(Path::new(&workspace.canonical_path)) {
        Ok(canonical) => canonical,
        Err(_) => {
            return Ok(Preparation::ReviewRequired(
                "workspace_unavailable".to_owned(),
            ));
        }
    };
    let current_fingerprint = match fingerprint(&canonical, policy, agent.adapter().policy_files())
    {
        Ok(fingerprint) => fingerprint,
        Err(_) => {
            return Ok(Preparation::ReviewRequired(
                "workspace_policy_unreadable".to_owned(),
            ));
        }
    };
    if current_fingerprint != workspace.policy_fingerprint {
        return Ok(Preparation::ReviewRequired(
            "workspace_policy_changed".to_owned(),
        ));
    }
    if validate_requirements(&plan.requirements, workspace).is_err()
        || validate_tools(&plan.requirements).is_err()
    {
        return Ok(Preparation::ReviewRequired(
            "plan_requirement_unavailable".to_owned(),
        ));
    }
    let preflight = match agent.adapter().preflight(&canonical.path, policy) {
        Ok(preflight) => preflight,
        Err(_) => {
            return Ok(Preparation::ReviewRequired(
                "agent_preflight_failed".to_owned(),
            ));
        }
    };
    if !preflight.ready {
        return Ok(Preparation::ReviewRequired(
            preflight
                .reason
                .unwrap_or_else(|| "agent_not_ready".to_owned()),
        ));
    }
    let command = match agent
        .adapter()
        .command(&canonical.path, policy, plan.model.as_deref())
    {
        Ok(command) => command,
        Err(_) => {
            return Ok(Preparation::ReviewRequired("agent_unavailable".to_owned()));
        }
    };
    Ok(Preparation::Ready(PreparedRun {
        agent,
        command,
        prompt: execution_prompt(plan),
        timeout: Duration::from_secs(run.limits.max_wall_time_seconds),
    }))
}

fn process_command(agent: &AgentCommand) -> Command {
    let mut command = Command::new(&agent.program);
    command
        .args(&agent.arguments)
        .current_dir(&agent.current_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

struct ProcessOutcome {
    status: ExitStatus,
    reason: StopReason,
}

enum StopReason {
    Exited,
    Cancelled,
    TimedOut,
}

fn monitor(
    app: &App,
    run_id: &str,
    supervisor: &mut ProcessSupervisor,
    timeout: Duration,
    started: Instant,
    log_path: &Path,
) -> Result<ProcessOutcome> {
    loop {
        if app.store.cancellation_requested(run_id)? {
            let status = supervisor
                .stop(STOP_GRACE_PERIOD)
                .map_err(|source| io_error(log_path, source))?;
            return Ok(ProcessOutcome {
                status,
                reason: StopReason::Cancelled,
            });
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let status = supervisor
                .stop(STOP_GRACE_PERIOD)
                .map_err(|source| io_error(log_path, source))?;
            return Ok(ProcessOutcome {
                status,
                reason: StopReason::TimedOut,
            });
        }
        let wait = POLL_INTERVAL.min(timeout - elapsed);
        if let Some(status) = supervisor
            .wait_for(wait)
            .map_err(|source| io_error(log_path, source))?
        {
            return Ok(ProcessOutcome {
                status,
                reason: StopReason::Exited,
            });
        }
    }
}

fn finish_capture(
    stdout: thread::JoinHandle<std::io::Result<()>>,
    stderr: thread::JoinHandle<std::io::Result<()>>,
) -> std::io::Result<()> {
    let stdout = join_capture(stdout);
    let stderr = join_capture(stderr);
    stdout.and(stderr)
}

fn join_capture(handle: thread::JoinHandle<std::io::Result<()>>) -> std::io::Result<()> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("log capture thread panicked"))?
}

fn join_input(handle: thread::JoinHandle<std::io::Result<()>>) -> std::io::Result<()> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("agent input thread panicked"))?
}

fn execution_prompt(plan: &PlanVersionRecord) -> String {
    let mut prompt = format!(
        "Execute the reviewed task below. Work autonomously within the configured sandbox. Do not request interactive approval.\n\n<task>\n{}\n</task>",
        plan.context
    );
    if !plan.completion_criteria.is_empty() {
        prompt.push_str("\n\n<completion_criteria>\n");
        for (index, criterion) in plan.completion_criteria.iter().enumerate() {
            prompt.push_str(&format!("{}. {}\n", index + 1, criterion));
        }
        prompt.push_str("</completion_criteria>");
    }
    prompt
}
