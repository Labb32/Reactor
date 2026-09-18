use clap::{Args, Subcommand};

use crate::{
    agents::AgentKind,
    app::App,
    error::{ReactorError, Result},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    plan::{PlanInput, PlanLimits, PlanRecord, prepare_input, validate_tools},
    plan_draft,
};

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
    file: Option<String>,

    #[arg(long)]
    workspace: Option<String>,

    /// Use plain Context text or '-' for stdin.
    #[arg(long, value_name = "TEXT")]
    context: Option<String>,

    #[arg(long)]
    title: Option<String>,

    #[arg(long, value_enum)]
    agent: Option<AgentKind>,

    #[arg(long)]
    provider: Option<String>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    max_wall_time_seconds: Option<u64>,

    #[arg(long)]
    max_attempts: Option<u32>,

    #[arg(long)]
    max_tokens: Option<u64>,

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
    max_tokens: Option<u64>,

    #[arg(long, default_value_t = plan_draft::DEFAULT_TIMEOUT_SECONDS)]
    draft_timeout_seconds: u64,

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
    if context.trim().is_empty() || context.len() > 1024 * 1024 {
        return Err(ReactorError::InvalidPlan(
            "draft context must contain 1 byte to 1 MiB".to_owned(),
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
    if !(1..=plan_draft::MAX_TIMEOUT_SECONDS).contains(&arguments.draft_timeout_seconds) {
        return Err(ReactorError::InvalidArgument(
            "--draft-timeout-seconds must be between 1 and 3600".to_owned(),
        ));
    }
    let workspace =
        super::workspaces::require_ready(&mut app, &arguments.workspace, arguments.agent)?;

    let draft = plan_draft::generate(
        &paths,
        &workspace,
        arguments.agent,
        arguments.model.as_deref(),
        &context,
        arguments.draft_timeout_seconds,
    )?;
    let input = draft.into_input(
        arguments.workspace.clone(),
        arguments.agent,
        arguments.provider.clone(),
        arguments.model.clone(),
        PlanLimits {
            max_wall_time_seconds: arguments.max_wall_time_seconds,
            max_attempts: arguments.max_attempts,
            max_tokens: arguments.max_tokens,
        },
    );
    let input = prepare_input(input, &workspace)?;
    let plan =
        app.store
            .create_drafted_plan(&input, arguments.idempotency_key.as_deref(), &request)?;
    write_plan(format, &app.instance_id, &plan)
}

fn create(format: OutputFormat, arguments: &PlanWriteArgs) -> Result<()> {
    let input = plan_input(arguments)?;
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let workspace = app.store.get_workspace(&input.workspace_id)?;
    let input = prepare_input(input, &workspace)?;
    let plan = app
        .store
        .create_plan(&input, arguments.idempotency_key.as_deref())?;
    write_plan(format, &app.instance_id, &plan)
}

fn plan_input(arguments: &PlanWriteArgs) -> Result<PlanInput> {
    if let Some(file) = &arguments.file {
        if arguments.workspace.is_some()
            || arguments.context.is_some()
            || arguments.title.is_some()
            || arguments.agent.is_some()
            || arguments.provider.is_some()
            || arguments.model.is_some()
            || arguments.max_wall_time_seconds.is_some()
            || arguments.max_attempts.is_some()
            || arguments.max_tokens.is_some()
        {
            return Err(ReactorError::InvalidArgument(
                "--file cannot be combined with simple Plan arguments".to_owned(),
            ));
        }
        return input::read_document::<PlanInput>(file);
    }
    let workspace_id = arguments.workspace.clone().ok_or_else(|| {
        ReactorError::InvalidArgument("--workspace is required without --file".to_owned())
    })?;
    let context_value = arguments.context.as_deref().ok_or_else(|| {
        ReactorError::InvalidArgument("--context is required without --file".to_owned())
    })?;
    let context = if context_value == "-" {
        input::read_text("-")?
    } else {
        context_value.to_owned()
    };
    let defaults = PlanLimits::default();
    Ok(PlanInput {
        workspace_id,
        title: arguments
            .title
            .clone()
            .unwrap_or_else(|| default_title(&context)),
        context,
        agent: arguments.agent.unwrap_or(AgentKind::Codex),
        provider: arguments.provider.clone(),
        model: arguments.model.clone(),
        requirements: Default::default(),
        completion_criteria: Vec::new(),
        limits: PlanLimits {
            max_wall_time_seconds: arguments
                .max_wall_time_seconds
                .unwrap_or(defaults.max_wall_time_seconds),
            max_attempts: arguments.max_attempts.unwrap_or(defaults.max_attempts),
            max_tokens: arguments.max_tokens,
        },
    })
}

fn default_title(context: &str) -> String {
    let line = context
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Quick task");
    let mut end = line.len().min(200);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].trim().to_owned()
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
    super::workspaces::require_ready(
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
    if approved_by.is_empty()
        || approved_by.len() > 128
        || approved_by.chars().any(char::is_control)
    {
        return Err(ReactorError::InvalidArgument(
            "--approved-by must contain 1 to 128 bytes without control characters".to_owned(),
        ));
    }
    let plan = app.store.approve_plan(
        &arguments.plan_id,
        current.current_version,
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

fn draft_request(arguments: &PlanDraftArgs, context: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "operation": "plan_draft",
        "workspace_id": arguments.workspace,
        "agent": arguments.agent,
        "provider": arguments.provider,
        "model": arguments.model,
        "max_wall_time_seconds": arguments.max_wall_time_seconds,
        "max_attempts": arguments.max_attempts,
        "max_tokens": arguments.max_tokens,
        "draft_timeout_seconds": arguments.draft_timeout_seconds,
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
