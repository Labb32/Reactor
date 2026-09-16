use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

use clap::{Args, Subcommand};

use crate::{
    agents::AgentKind,
    app::App,
    error::{ReactorError, Result, io_error},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    plan::{AgentPlanDraft, PlanInput, PlanLimits, PlanRecord, prepare_input, validate_tools},
    workspace::{WorkspaceRecord, canonicalize, fingerprint, normalize_policy},
};

const DRAFT_SCHEMA: &str = r#"{
  "type": "object",
  "additionalProperties": false,
  "required": ["title", "context", "requirements", "completion_criteria"],
  "properties": {
    "title": { "type": "string" },
    "context": { "type": "string" },
    "requirements": {
      "type": "object",
      "additionalProperties": false,
      "required": ["writable_roots", "network", "mcp", "hooks", "tools"],
      "properties": {
        "writable_roots": { "type": "array", "items": { "type": "string" } },
        "network": { "type": "boolean" },
        "mcp": { "type": "boolean" },
        "hooks": { "type": "boolean" },
        "tools": { "type": "array", "items": { "type": "string" } }
      }
    },
    "completion_criteria": { "type": "array", "items": { "type": "string" } }
  }
}"#;

#[derive(Debug, Args)]
pub(super) struct PlanArgs {
    #[command(subcommand)]
    command: PlanCommand,
}

#[derive(Debug, Subcommand)]
enum PlanCommand {
    /// Create a draft from a reviewed plan document.
    Create(PlanWriteArgs),
    /// Ask an Agent to create a read-only plan draft.
    Draft(PlanDraftArgs),
    /// Create a new draft version.
    Revise(PlanReviseArgs),
    /// Show a plan version.
    Show(PlanShowArgs),
    /// List current plan versions.
    List,
    /// Move the current draft into review.
    Review(PlanMutationArgs),
    /// Approve the current reviewed version.
    Approve(PlanApproveArgs),
}

#[derive(Debug, Args)]
struct PlanWriteArgs {
    /// Read a plan document from JSON or YAML.
    #[arg(long, value_name = "PATH")]
    file: String,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct PlanDraftArgs {
    #[arg(long)]
    workspace: String,

    #[arg(long, value_enum, default_value = "codex")]
    agent: AgentKind,

    /// Read the request Context as plain text or Markdown.
    #[arg(long, value_name = "PATH")]
    file: String,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long, default_value_t = 7_200)]
    max_wall_time_seconds: u64,

    #[arg(long, default_value_t = 1)]
    max_attempts: u32,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct PlanReviseArgs {
    plan_id: String,

    /// Read a replacement plan document from JSON or YAML.
    #[arg(long, value_name = "PATH")]
    file: String,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct PlanShowArgs {
    plan_id: String,

    #[arg(long)]
    version: Option<u32>,
}

#[derive(Debug, Args)]
struct PlanMutationArgs {
    plan_id: String,

    #[arg(long)]
    idempotency_key: Option<String>,
}

#[derive(Debug, Args)]
struct PlanApproveArgs {
    plan_id: String,

    #[arg(long)]
    approved_by: Option<String>,

    #[arg(long)]
    idempotency_key: Option<String>,
}

pub(super) fn execute(format: OutputFormat, arguments: &PlanArgs) -> Result<()> {
    match &arguments.command {
        PlanCommand::Create(arguments) => create(format, arguments),
        PlanCommand::Draft(arguments) => draft(format, arguments),
        PlanCommand::Revise(arguments) => revise(format, arguments),
        PlanCommand::Show(arguments) => show(format, arguments),
        PlanCommand::List => list(format),
        PlanCommand::Review(arguments) => review(format, arguments),
        PlanCommand::Approve(arguments) => approve(format, arguments),
    }
}

fn draft(format: OutputFormat, arguments: &PlanDraftArgs) -> Result<()> {
    let context = input::read_text(&arguments.file)?;
    if context.trim().is_empty() {
        return Err(ReactorError::InvalidPlan(
            "draft context must not be empty".to_owned(),
        ));
    }
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let request = draft_request(arguments, &context)?;
    if let Some(key) = arguments.idempotency_key.as_deref()
        && let Some(plan) = app.store.idempotent_response::<PlanRecord>(key, &request)?
    {
        return write_plan(format, &app.instance_id, &plan);
    }
    let workspace = require_ready_workspace(&mut app, &arguments.workspace, arguments.agent)?;

    let draft = run_agent_draft(&paths, &workspace, arguments, &context)?;
    let input = draft.into_input(
        arguments.workspace.clone(),
        arguments.agent,
        arguments.provider.clone(),
        arguments.model.clone(),
        PlanLimits {
            max_wall_time_seconds: arguments.max_wall_time_seconds,
            max_attempts: arguments.max_attempts,
        },
    );
    let input = prepare_input(input, &workspace)?;
    let plan =
        app.store
            .create_drafted_plan(&input, arguments.idempotency_key.as_deref(), &request)?;
    write_plan(format, &app.instance_id, &plan)
}

fn create(format: OutputFormat, arguments: &PlanWriteArgs) -> Result<()> {
    let input = input::read_document::<PlanInput>(&arguments.file)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let workspace = app.store.get_workspace(&input.workspace_id)?;
    let input = prepare_input(input, &workspace)?;
    let plan = app
        .store
        .create_plan(&input, arguments.idempotency_key.as_deref())?;
    write_plan(format, &app.instance_id, &plan)
}

fn revise(format: OutputFormat, arguments: &PlanReviseArgs) -> Result<()> {
    let input = input::read_document::<PlanInput>(&arguments.file)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let workspace = app.store.get_workspace(&input.workspace_id)?;
    let input = prepare_input(input, &workspace)?;
    let plan = app.store.revise_plan(
        &arguments.plan_id,
        &input,
        arguments.idempotency_key.as_deref(),
    )?;
    write_plan(format, &app.instance_id, &plan)
}

fn show(format: OutputFormat, arguments: &PlanShowArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let plan = app.store.get_plan(&arguments.plan_id, arguments.version)?;
    write_plan(format, &app.instance_id, &plan)
}

fn list(format: OutputFormat) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let app = App::open(&paths)?;
    let plans = app.store.list_plans()?;
    match format {
        OutputFormat::Text => {
            for plan in plans {
                println!(
                    "{} v{} {} {}",
                    plan.id, plan.current_version, plan.version.status, plan.version.title
                );
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, plans))
        }
    }
}

fn review(format: OutputFormat, arguments: &PlanMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let plan = app
        .store
        .review_plan(&arguments.plan_id, arguments.idempotency_key.as_deref())?;
    write_plan(format, &app.instance_id, &plan)
}

fn approve(format: OutputFormat, arguments: &PlanApproveArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let current = app.store.get_plan(&arguments.plan_id, None)?;
    require_ready_workspace(
        &mut app,
        &current.version.workspace_id,
        current.version.agent,
    )?;
    validate_tools(&current.version.requirements)?;
    let approved_by = arguments
        .approved_by
        .clone()
        .unwrap_or_else(default_approver);
    let approved_by = approved_by.trim();
    if approved_by.is_empty() || approved_by.len() > 128 {
        return Err(ReactorError::InvalidArgument(
            "--approved-by must contain 1 to 128 bytes".to_owned(),
        ));
    }
    let plan = app.store.approve_plan(
        &arguments.plan_id,
        approved_by,
        arguments.idempotency_key.as_deref(),
    )?;
    write_plan(format, &app.instance_id, &plan)
}

fn default_approver() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local_user".to_owned())
}

fn require_ready_workspace(
    app: &mut App,
    workspace_id: &str,
    agent: AgentKind,
) -> Result<WorkspaceRecord> {
    let record = app.store.get_workspace(workspace_id)?;
    let workspace = match canonicalize(std::path::Path::new(&record.canonical_path)) {
        Ok(workspace) => workspace,
        Err(_) => {
            app.store
                .mark_workspace_degraded(workspace_id, "path_unavailable")?;
            return Err(ReactorError::AgentNotReady {
                workspace_id: workspace_id.to_owned(),
                agent: agent.as_str().to_owned(),
                reason: "workspace path is unavailable".to_owned(),
            });
        }
    };
    let policy = match normalize_policy(&workspace, &record.policy) {
        Ok(policy) => policy,
        Err(_) => {
            app.store
                .mark_workspace_degraded(workspace_id, "policy_unreadable")?;
            return Err(ReactorError::AgentNotReady {
                workspace_id: workspace_id.to_owned(),
                agent: agent.as_str().to_owned(),
                reason: "workspace policy is unreadable".to_owned(),
            });
        }
    };
    let workspace_fingerprint = fingerprint(&workspace, &policy, &[])?;
    if workspace_fingerprint != record.policy_fingerprint {
        app.store
            .mark_workspace_degraded(workspace_id, "policy_changed")?;
        return Err(ReactorError::AgentNotReady {
            workspace_id: workspace_id.to_owned(),
            agent: agent.as_str().to_owned(),
            reason: "workspace policy changed".to_owned(),
        });
    }

    let adapter = agent.adapter();
    let agent_fingerprint = fingerprint(&workspace, &policy, adapter.policy_files())?;
    let ready = record.agents.iter().any(|prepared| {
        prepared.agent_kind == agent.as_str()
            && prepared.status == "READY"
            && prepared.policy_fingerprint.as_deref() == Some(agent_fingerprint.as_str())
    });
    if record.status != "READY" || !ready {
        app.store.mark_workspace_agent_degraded(
            workspace_id,
            agent.as_str(),
            "agent_policy_changed",
        )?;
        return Err(ReactorError::AgentNotReady {
            workspace_id: workspace_id.to_owned(),
            agent: agent.as_str().to_owned(),
            reason: "run workspace prepare again".to_owned(),
        });
    }
    Ok(record)
}

fn run_agent_draft(
    paths: &ReactorPaths,
    workspace: &crate::workspace::WorkspaceRecord,
    arguments: &PlanDraftArgs,
    context: &str,
) -> Result<AgentPlanDraft> {
    let draft_dir = paths.data_dir.join("drafts");
    fs::create_dir_all(&draft_dir).map_err(|source| io_error(&draft_dir, source))?;
    let token = crate::ids::new_id();
    let schema_path = draft_dir.join(format!("{token}.schema.json"));
    let output_path = draft_dir.join(format!("{token}.output.json"));
    fs::write(&schema_path, DRAFT_SCHEMA).map_err(|source| io_error(&schema_path, source))?;

    let result = (|| {
        let command = arguments.agent.adapter().draft_command(
            std::path::Path::new(&workspace.canonical_path),
            &schema_path,
            &output_path,
            arguments.model.as_deref(),
        )?;
        let mut child = Command::new(&command.program)
            .args(&command.arguments)
            .current_dir(&command.current_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| io_error(&command.current_dir, source))?;
        let prompt = format!(
            "Create an execution plan for the request below. Do not modify files or perform the task. Use `workspace` for the primary writable root and list only additional paths that are required. Return only the requested structured output.\n\n<request>\n{context}\n</request>"
        );
        child
            .stdin
            .take()
            .ok_or_else(|| ReactorError::AgentUnavailable {
                agent: arguments.agent.as_str().to_owned(),
                reason: "stdin_unavailable".to_owned(),
            })?
            .write_all(prompt.as_bytes())
            .map_err(|source| io_error("agent stdin", source))?;
        let output = child
            .wait_with_output()
            .map_err(|source| io_error(&command.current_dir, source))?;
        if !output.status.success() {
            let reason = String::from_utf8_lossy(&output.stderr);
            return Err(ReactorError::AgentUnavailable {
                agent: arguments.agent.as_str().to_owned(),
                reason: reason.chars().take(1_000).collect(),
            });
        }
        let bytes = fs::read(&output_path).map_err(|source| io_error(&output_path, source))?;
        serde_json::from_slice(&bytes)
            .map_err(|error| ReactorError::InvalidInput(format!("invalid Agent draft: {error}")))
    })();

    let _ = fs::remove_file(&schema_path);
    let _ = fs::remove_file(&output_path);
    result
}

fn draft_request(arguments: &PlanDraftArgs, context: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "operation": "plan_draft",
        "workspace_id": arguments.workspace,
        "agent": arguments.agent,
        "provider": arguments.provider,
        "model": arguments.model,
        "max_wall_time_seconds": arguments.max_wall_time_seconds,
        "max_attempts": arguments.max_attempts,
        "context": context
    }))
    .map_err(|error| ReactorError::InvalidInput(error.to_string()))
}

fn write_plan(format: OutputFormat, instance_id: &str, plan: &PlanRecord) -> Result<()> {
    match format {
        OutputFormat::Text => {
            println!("plan: {}", plan.id);
            println!("version: {}", plan.version.version);
            println!("status: {}", plan.version.status);
            println!("title: {}", plan.version.title);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, plan))
        }
    }
}
