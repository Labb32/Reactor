use std::{
    io::Write,
    path::Path,
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use crate::{
    agents::{AgentCommand, AgentKind},
    app::App,
    checkpoint,
    domain::{PlanStatus, RunState},
    error::{ReactorError, Result, io_error},
    interrupt,
    paths::ReactorPaths,
    plan::{PlanVersionRecord, validate_requirements, validate_tools},
    process_supervisor::ProcessSupervisor,
    run::{BeginAttempt, ExecutionRun, RunRecord},
    run_log::{self, RunLog},
    workspace::{
        CanonicalWorkspace, WorkspacePolicy, canonicalize, fingerprint, normalize_policy,
        paths_equal, validate_storage_isolation,
    },
};

const POLL_INTERVAL: Duration = Duration::from_millis(500);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const STOP_GRACE_PERIOD: Duration = Duration::from_secs(3);

pub fn execute_next(paths: &ReactorPaths, app: &mut App) -> Result<Option<RunRecord>> {
    let shutdown = interrupt::install("run interrupt handler")?;
    prepare_execution(paths, app)?;
    execute_available(paths, app, Some(&shutdown))
}

pub(crate) fn prepare_execution(paths: &ReactorPaths, app: &mut App) -> Result<()> {
    recover(paths, app)?;
    let runs_dir = paths.data_dir.join("runs");
    run_log::prune(&runs_dir).map_err(|source| io_error(&runs_dir, source))?;
    Ok(())
}

pub(crate) fn execute_available(
    paths: &ReactorPaths,
    app: &mut App,
    shutdown: Option<&AtomicBool>,
) -> Result<Option<RunRecord>> {
    if shutdown.is_some_and(|shutdown| shutdown.load(Ordering::Acquire)) {
        return Ok(None);
    }
    let owner_id = crate::ids::new_id();
    let Some(claimed) = app
        .store
        .claim_next_run(&app.config.concurrency, &owner_id)?
    else {
        return Ok(None);
    };
    let heartbeat = Heartbeat::start(paths, &claimed.id, &owner_id);
    let budget = crate::execution_control::Guard::enter(
        Duration::from_secs(600), Arc::clone(&heartbeat.lost), Arc::clone(&heartbeat.cancelled),
    );
    app.store.fence_execution(Some((&claimed.id, &owner_id)));
    let result = execute_claimed(paths, app, &claimed.id, &owner_id, shutdown, &heartbeat.lost);
    app.store.fence_execution(None);
    drop(budget);
    result.map(Some)
}

fn execute_claimed(
    paths: &ReactorPaths,
    app: &mut App,
    run_id: &str,
    owner_id: &str,
    shutdown: Option<&AtomicBool>,
    ownership_lost: &AtomicBool,
) -> Result<RunRecord> {
    let execution = app.store.execution_run(run_id)?;
    let workspace = app.store.get_workspace(&execution.record.workspace_id)?;
    let runtime = match runtime_workspace(paths, &workspace, &execution.policy) {
        Ok(runtime) => runtime,
        Err(_) => {
            let run = app.store.finish_run(
                run_id,
                None,
                RunState::Preparing,
                RunState::ReviewRequired,
                None,
                "workspace_policy_changed",
            )?;
            app.store.release_run(run_id, owner_id)?;
            return Ok(run);
        }
    };
    let roots = runtime.policy.write_paths(&runtime.canonical.path);
    let _locks = crate::workspace_execution::WorkspaceLocks::acquire(&roots)?;
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
    let run = execute_prepared(paths, app, execution, workspace, shutdown, ownership_lost)?;
    if ownership_lost.load(Ordering::Acquire) {
        return Ok(run);
    }
    crate::execution_control::set_timeout(Some(Duration::from_secs(60)));
    record_checkpoint_result(paths, app, &run)
}

fn execute_prepared(
    paths: &ReactorPaths,
    app: &mut App,
    execution: ExecutionRun,
    workspace: crate::workspace::WorkspaceRecord,
    shutdown: Option<&AtomicBool>,
    ownership_lost: &AtomicBool,
) -> Result<RunRecord> {
    let run_id = &execution.record.id;
    if ownership_lost.load(Ordering::Acquire)
        || shutdown.is_some_and(|shutdown| shutdown.load(Ordering::Acquire)) {
        return app.store.finish_run(
            run_id,
            None,
            RunState::Preparing,
            RunState::ReviewRequired,
            None,
            "runner_interrupted",
        );
    }
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
        paths,
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
    if let Err(error) = crate::execution_control::check() {
        return app.store.finish_run(run_id, None, RunState::Preparing,
            RunState::ReviewRequired, None, &format!("preparation_stopped: {error}"));
    }
    crate::execution_control::set_timeout(None);

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

    if ownership_lost.load(Ordering::Acquire) {
        return Err(ReactorError::InvalidStoredData("run ownership lost before spawn".to_owned()));
    }
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
        shutdown,
        ownership_lost,
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
    let captured = finish_capture(stdout_thread, stderr_thread);
    let flush_result = RunLog::flush(&log);
    let log_failed = captured.is_err() || flush_result.is_err();
    let (stdout, stderr) = captured.unwrap_or_default();
    if ownership_lost.load(Ordering::Acquire) {
        // Leave the record and lease for recovery; a former owner must not finalize it.
        return app.store.get_run(run_id);
    }
    let stdout = String::from_utf8_lossy(&stdout);
    let stderr = String::from_utf8_lossy(&stderr);
    let token_usage = prepared.agent.adapter().token_usage(&stdout);
    if let Some(tokens) = token_usage {
        app.store
            .record_attempt_usage(run_id, &attempt.id, tokens)?;
    }

    let (next, reason) = match outcome.reason {
        StopReason::Cancelled => (RunState::Cancelled, "cancel_requested"),
        StopReason::TimedOut => (RunState::LimitReached, "wall_time_limit"),
        StopReason::Shutdown => (RunState::ReviewRequired, "runner_interrupted"),
        StopReason::Exited if log_failed => (RunState::Failed, "log_io_error"),
        StopReason::Exited if stdin_result.is_err() => (RunState::Failed, "input_failed"),
        StopReason::Exited
            if !outcome.status.success()
                && (prepared.agent.adapter().failure_requires_review(&stdout)
                    || prepared.agent.adapter().failure_requires_review(&stderr)) =>
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
    // A missing before-manifest cannot be recreated after execution: keep the
    // result unavailable rather than inventing a misleading empty diff.
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
    lost: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    stop: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Heartbeat {
    fn start(paths: &ReactorPaths, run_id: &str, owner_id: &str) -> Self {
        let database = paths.database_file();
        let run_id = run_id.to_owned();
        let owner_id = owner_id.to_owned();
        let (stop, receiver) = mpsc::channel();
        let lost = Arc::new(AtomicBool::new(false));
        let signal = Arc::clone(&lost);
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = Arc::clone(&cancelled);
        let handle = thread::spawn(move || {
            let Ok(store) = crate::store::Store::open_initialized(&database) else {
                signal.store(true, Ordering::Release);
                return;
            };
            let mut renewed = Instant::now();
            loop {
                match receiver.recv_timeout(HEARTBEAT_INTERVAL) {
                    Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
                match store.heartbeat_run(&run_id, &owner_id) {
                    Ok(true) => renewed = Instant::now(),
                    Ok(false) => {
                        signal.store(true, Ordering::Release);
                        break;
                    }
                    Err(_) if renewed.elapsed() < Duration::from_secs(45) => continue,
                    Err(_) => {
                        signal.store(true, Ordering::Release);
                        break;
                    }
                }
                match store.cancellation_requested(&run_id) {
                    Ok(true) => cancellation.store(true, Ordering::Release),
                    Ok(false) => {},
                    Err(_) => { signal.store(true, Ordering::Release); break; }
                }
            }
        });
        Self {
            lost,
            cancelled,
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
    paths: &ReactorPaths,
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
    let prepared_agent = workspace.agents.iter().find(|record| {
        record.agent_kind == run.agent_kind && record.status == "READY"
    });
    let Some(prepared_agent) = prepared_agent else {
        return Ok(Preparation::ReviewRequired(
            "workspace_not_ready".to_owned(),
        ));
    };
    let runtime = match runtime_workspace(paths, workspace, policy) {
        Ok(runtime) => runtime,
        Err(_) => {
            return Ok(Preparation::ReviewRequired(
                "workspace_policy_changed".to_owned(),
            ));
        }
    };
    let canonical = runtime.canonical;
    let policy = &runtime.policy;
    let current_fingerprint = match fingerprint(&canonical, policy, &[])
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
    let agent_fingerprint = match fingerprint(&canonical, policy, agent.adapter().policy_files()) {
        Ok(value) => value,
        Err(_) => return Ok(Preparation::ReviewRequired("agent_policy_unreadable".to_owned())),
    };
    if prepared_agent.policy_fingerprint.as_deref() != Some(agent_fingerprint.as_str()) {
        return Ok(Preparation::ReviewRequired("agent_policy_changed".to_owned()));
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

struct RuntimeWorkspace {
    canonical: CanonicalWorkspace,
    policy: WorkspacePolicy,
}

fn runtime_workspace(
    paths: &ReactorPaths,
    workspace: &crate::workspace::WorkspaceRecord,
    policy: &WorkspacePolicy,
) -> Result<RuntimeWorkspace> {
    let canonical = canonicalize(Path::new(&workspace.canonical_path))?;
    if !paths_equal(&canonical.display, &workspace.canonical_path) {
        return Err(ReactorError::InvalidSandboxPolicy(
            "workspace path changed".to_owned(),
        ));
    }
    let normalized = normalize_policy(&canonical, policy)?;
    let stored = serde_json::to_vec(policy)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    let current = serde_json::to_vec(&normalized)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))?;
    if current != stored {
        return Err(ReactorError::InvalidSandboxPolicy(
            "workspace write scope changed".to_owned(),
        ));
    }
    validate_storage_isolation(&canonical, &normalized, paths)?;
    Ok(RuntimeWorkspace {
        canonical,
        policy: normalized,
    })
}

fn process_command(agent: &AgentCommand) -> Command {
    let mut command = Command::new(&agent.program);
    command
        .args(&agent.arguments)
        .envs(agent.environment.iter().map(|(key, value)| (key, value)))
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
    Shutdown,
}

fn monitor(
    app: &App,
    run_id: &str,
    supervisor: &mut ProcessSupervisor,
    timeout: Duration,
    started: Instant,
    log_path: &Path,
    shutdown: Option<&AtomicBool>,
    ownership_lost: &AtomicBool,
) -> Result<ProcessOutcome> {
    loop {
        if ownership_lost.load(Ordering::Acquire)
            || shutdown.is_some_and(|shutdown| shutdown.load(Ordering::Acquire)) {
            let status = supervisor
                .stop(STOP_GRACE_PERIOD)
                .map_err(|source| io_error(log_path, source))?;
            return Ok(ProcessOutcome {
                status,
                reason: StopReason::Shutdown,
            });
        }
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
    stdout: thread::JoinHandle<std::io::Result<Vec<u8>>>,
    stderr: thread::JoinHandle<std::io::Result<Vec<u8>>>,
) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let output = join_capture(stdout);
        let error = join_capture(stderr);
        let _ = sender.send(output.and_then(|output| error.map(|error| (output, error))));
    });
    receiver.recv_timeout(Duration::from_secs(60))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "output collection timed out"))?
}

fn join_capture(handle: thread::JoinHandle<std::io::Result<Vec<u8>>>) -> std::io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("log capture thread panicked"))?
}

fn join_input(handle: thread::JoinHandle<std::io::Result<()>>) -> std::io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(handle.join()
            .map_err(|_| std::io::Error::other("agent input thread panicked"))
            .and_then(|result| result));
    });
    receiver.recv_timeout(Duration::from_secs(2))
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "agent input close timed out"))?
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
