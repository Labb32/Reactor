use std::{io::Write, path::Path};

use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    agents::AgentKind,
    app::App,
    error::{ReactorError, Result},
    input, output,
    output::{Envelope, OutputFormat},
    paths::ReactorPaths,
    plan::{PlanInput, PlanLimits, PlanRecord, prepare_input, validate_id, validate_tools},
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
    Review(PlanReviewArgs),
    /// Approve the current reviewed version.
    Approve(PlanApproveArgs),
    /// Return an approved plan to review.
    Unapprove(PlanMutationArgs),
    /// Delete an unused plan and all of its versions.
    Delete(PlanMutationArgs),
}

#[derive(Debug, Args)]
struct PlanWriteArgs {
    /// Read a plan document from JSON or YAML.
    #[arg(long, value_name = "PATH")]
    file: Option<String>,

    /// Use a short Plan identifier instead of a generated ID.
    #[arg(long)]
    name: Option<String>,

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

    #[arg(long = "execution-timeout-seconds", alias = "max-wall-time-seconds")]
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
    /// Use a short Plan identifier instead of a generated ID.
    #[arg(long)]
    name: Option<String>,

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
    file: Option<String>,

    #[arg(long)]
    workspace: Option<String>,

    /// Replace Context text or read it from stdin with '-'.
    #[arg(long, value_name = "TEXT")]
    context: Option<String>,

    #[arg(long)]
    title: Option<String>,

    #[arg(long, value_enum)]
    agent: Option<AgentKind>,

    #[arg(long, conflicts_with = "clear_provider")]
    provider: Option<String>,

    #[arg(long)]
    clear_provider: bool,

    #[arg(long, conflicts_with = "clear_model")]
    model: Option<String>,

    #[arg(long)]
    clear_model: bool,

    #[arg(long = "execution-timeout-seconds", alias = "max-wall-time-seconds")]
    max_wall_time_seconds: Option<u64>,

    #[arg(long)]
    max_attempts: Option<u32>,

    #[arg(long, conflicts_with = "clear_max_tokens")]
    max_tokens: Option<u64>,

    #[arg(long)]
    clear_max_tokens: bool,

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
struct PlanReviewArgs {
    plan_id: String,

    /// Approve immediately after displaying the reviewed Plan.
    #[arg(short = 'y', long)]
    approve: bool,

    #[arg(long, requires = "approve")]
    approved_by: Option<String>,

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

#[derive(Serialize)]
struct PlanTemplateCreated {
    path: String,
    created: bool,
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
        PlanCommand::Unapprove(arguments) => unapprove(format, arguments),
        PlanCommand::Delete(arguments) => delete(format, arguments),
    }
}

fn draft(format: OutputFormat, arguments: &PlanDraftArgs) -> Result<()> {
    if let Some(name) = arguments.name.as_deref() {
        validate_id(name)?;
    }
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
        super::workspaces::require_ready(&paths, &mut app, &arguments.workspace, arguments.agent)?;

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
    let plan = app.store.create_drafted_plan(
        arguments.name.as_deref(),
        &input,
        arguments.idempotency_key.as_deref(),
        &request,
    )?;
    write_plan(format, &app.instance_id, &plan)
}

fn create(format: OutputFormat, arguments: &PlanWriteArgs) -> Result<()> {
    if let Some(name) = arguments.name.as_deref() {
        validate_id(name)?;
    }
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    if let (Some(name), Some(file)) = (arguments.name.as_deref(), arguments.file.as_deref())
        && file != "-"
        && !Path::new(file).exists()
    {
        if has_simple_plan_fields(arguments) {
            return Err(ReactorError::InvalidArgument(
                "--file cannot be combined with simple Plan arguments".to_owned(),
            ));
        }
        return create_template(format, &app.instance_id, name, file);
    }
    let input = plan_input(arguments)?;
    let workspace = app.store.get_workspace(&input.workspace_id)?;
    let input = prepare_input(input, &workspace)?;
    let plan = app.store.create_plan(
        arguments.name.as_deref(),
        &input,
        arguments.idempotency_key.as_deref(),
    )?;
    write_plan(format, &app.instance_id, &plan)
}

fn create_template(
    format: OutputFormat,
    instance_id: &str,
    name: &str,
    source: &str,
) -> Result<()> {
    let requested = Path::new(source);
    let mut path = requested.to_owned();
    let extension = requested.extension().and_then(|value| value.to_str());
    match extension {
        None => {
            path.set_extension("yaml");
        }
        Some(value)
            if value.eq_ignore_ascii_case("yaml")
                || value.eq_ignore_ascii_case("yml")
                || value.eq_ignore_ascii_case("json") => {}
        Some(_) => {
            return Err(ReactorError::InvalidArgument(
                "Plan template path must use .json, .yaml, or .yml".to_owned(),
            ));
        }
    }
    let parent = path
        .parent()
        .filter(|value| !value.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent =
        std::fs::canonicalize(parent).map_err(|source| crate::error::io_error(parent, source))?;
    let file_name = path.file_name().ok_or_else(|| {
        ReactorError::InvalidArgument("Plan template path must include a file name".to_owned())
    })?;
    let path = parent.join(file_name);
    let template = PlanInput {
        tasks: Vec::new(),
        workspace_id: "workspace-id".to_owned(),
        title: name.to_owned(),
        context: "Describe the task.".to_owned(),
        agent: AgentKind::Codex,
        provider: None,
        model: None,
        requirements: Default::default(),
        completion_criteria: Vec::new(),
        limits: PlanLimits::default(),
    };
    let bytes = if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("json"))
    {
        serde_json::to_vec_pretty(&template)
            .map_err(|error| ReactorError::OutputSerialization(error.to_string()))?
    } else {
        serde_yaml_ng::to_string(&template)
            .map(String::into_bytes)
            .map_err(|error| ReactorError::OutputSerialization(error.to_string()))?
    };
    let mut file = crate::permissions::create_private_new_file(&path)
        .map_err(|source| crate::error::io_error(&path, source))?;
    file.write_all(&bytes)
        .map_err(|source| crate::error::io_error(&path, source))?;
    let created = PlanTemplateCreated {
        path: path.to_string_lossy().into_owned(),
        created: true,
    };
    match format {
        OutputFormat::Text => {
            println!("plan template created: {}", created.path);
            println!("edit the template, then pass the printed path to --file");
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, created))
        }
    }
}

fn plan_input(arguments: &PlanWriteArgs) -> Result<PlanInput> {
    if let Some(file) = &arguments.file {
        if has_simple_plan_fields(arguments) {
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
        tasks: Vec::new(),
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

fn has_simple_plan_fields(arguments: &PlanWriteArgs) -> bool {
    arguments.workspace.is_some()
        || arguments.context.is_some()
        || arguments.title.is_some()
        || arguments.agent.is_some()
        || arguments.provider.is_some()
        || arguments.model.is_some()
        || arguments.max_wall_time_seconds.is_some()
        || arguments.max_attempts.is_some()
        || arguments.max_tokens.is_some()
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
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let current = app.store.get_plan(&arguments.plan_id, None)?;
    let input = revised_input(arguments, &current)?;
    let workspace = app.store.get_workspace(&input.workspace_id)?;
    let input = prepare_input(input, &workspace)?;
    let plan = app.store.revise_plan(
        &arguments.plan_id,
        &input,
        arguments.idempotency_key.as_deref(),
    )?;
    write_plan(format, &app.instance_id, &plan)
}

fn revised_input(arguments: &PlanReviseArgs, current: &PlanRecord) -> Result<PlanInput> {
    let has_inline = arguments.workspace.is_some()
        || arguments.context.is_some()
        || arguments.title.is_some()
        || arguments.agent.is_some()
        || arguments.provider.is_some()
        || arguments.clear_provider
        || arguments.model.is_some()
        || arguments.clear_model
        || arguments.max_wall_time_seconds.is_some()
        || arguments.max_attempts.is_some()
        || arguments.max_tokens.is_some()
        || arguments.clear_max_tokens;
    if let Some(file) = &arguments.file {
        if has_inline {
            return Err(ReactorError::InvalidArgument(
                "--file cannot be combined with inline Plan revisions".to_owned(),
            ));
        }
        return input::read_document::<PlanInput>(file);
    }
    if !has_inline {
        return Err(ReactorError::InvalidArgument(
            "provide --file or at least one Plan field to revise".to_owned(),
        ));
    }

    let version = &current.version;
    let context = match arguments.context.as_deref() {
        Some("-") => input::read_text("-")?,
        Some(value) => value.to_owned(),
        None => version.context.clone(),
    };
    let provider = if arguments.clear_provider {
        None
    } else {
        arguments
            .provider
            .clone()
            .or_else(|| version.provider.clone())
    };
    let model = if arguments.clear_model {
        None
    } else {
        arguments.model.clone().or_else(|| version.model.clone())
    };
    let max_tokens = if arguments.clear_max_tokens {
        None
    } else {
        arguments.max_tokens.or(version.limits.max_tokens)
    };
    Ok(PlanInput {
        tasks: version.tasks.clone(),
        workspace_id: arguments
            .workspace
            .clone()
            .unwrap_or_else(|| version.workspace_id.clone()),
        title: arguments
            .title
            .clone()
            .unwrap_or_else(|| version.title.clone()),
        context,
        agent: arguments.agent.unwrap_or(version.agent),
        provider,
        model,
        requirements: version.requirements.clone(),
        completion_criteria: version.completion_criteria.clone(),
        limits: PlanLimits {
            max_wall_time_seconds: arguments
                .max_wall_time_seconds
                .unwrap_or(version.limits.max_wall_time_seconds),
            max_attempts: arguments
                .max_attempts
                .unwrap_or(version.limits.max_attempts),
            max_tokens,
        },
    })
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
            let rows = plans
                .into_iter()
                .map(|plan| {
                    vec![
                        plan.id,
                        format!("v{}", plan.current_version),
                        output::display_status(&plan.version.status),
                        plan.version.title,
                    ]
                })
                .collect::<Vec<_>>();
            output::write_table(&["ID", "VERSION", "STATUS", "TITLE"], &rows);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, plans))
        }
    }
}

fn review(format: OutputFormat, arguments: &PlanReviewArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let current = app.store.get_plan(&arguments.plan_id, None)?;
    let plan = if current.version.status == "DRAFT" {
        let key = (!arguments.approve)
            .then_some(arguments.idempotency_key.as_deref())
            .flatten();
        app.store.review_plan(&arguments.plan_id, key)?
    } else {
        current
    };
    if arguments.approve {
        if plan.version.status == "APPROVED" {
            return write_plan_review(format, &app.instance_id, &plan);
        }
        super::workspaces::require_ready(
            &paths,
            &mut app,
            &plan.version.workspace_id,
            plan.version.agent,
        )?;
        validate_tools(&plan.version.requirements)?;
        let approved_by = valid_approver(arguments.approved_by.clone())?;
        let plan = app.store.approve_plan(
            &arguments.plan_id,
            plan.current_version,
            &approved_by,
            arguments.idempotency_key.as_deref(),
        )?;
        return write_plan_review(format, &app.instance_id, &plan);
    }
    write_plan_review(format, &app.instance_id, &plan)
}

fn approve(format: OutputFormat, arguments: &PlanApproveArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let current = app.store.get_plan(&arguments.plan_id, None)?;
    super::workspaces::require_ready(
        &paths,
        &mut app,
        &current.version.workspace_id,
        current.version.agent,
    )?;
    validate_tools(&current.version.requirements)?;
    let approved_by = valid_approver(arguments.approved_by.clone())?;
    let plan = app.store.approve_plan(
        &arguments.plan_id,
        current.current_version,
        &approved_by,
        arguments.idempotency_key.as_deref(),
    )?;
    write_plan(format, &app.instance_id, &plan)
}

fn valid_approver(value: Option<String>) -> Result<String> {
    let approved_by = value.unwrap_or_else(default_approver);
    let approved_by = approved_by.trim();
    if approved_by.is_empty()
        || approved_by.len() > 128
        || approved_by.chars().any(char::is_control)
    {
        return Err(ReactorError::InvalidArgument(
            "--approved-by must contain 1 to 128 bytes without control characters".to_owned(),
        ));
    }
    Ok(approved_by.to_owned())
}

fn unapprove(format: OutputFormat, arguments: &PlanMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let plan = app
        .store
        .unapprove_plan(&arguments.plan_id, arguments.idempotency_key.as_deref())?;
    write_plan(format, &app.instance_id, &plan)
}

fn delete(format: OutputFormat, arguments: &PlanMutationArgs) -> Result<()> {
    let paths = ReactorPaths::discover()?;
    let mut app = App::open(&paths)?;
    let plan = app
        .store
        .delete_plan(&arguments.plan_id, arguments.idempotency_key.as_deref())?;
    match format {
        OutputFormat::Text => {
            println!("deleted plan: {}", plan.id);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(app.instance_id, plan))
        }
    }
}

fn default_approver() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "local_user".to_owned())
}

fn draft_request(arguments: &PlanDraftArgs, context: &str) -> Result<Vec<u8>> {
    serde_json::to_vec(&serde_json::json!({
        "operation": "plan_draft",
        "name": arguments.name,
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
            println!("status: {}", output::display_status(&plan.version.status));
            println!("title: {}", plan.version.title);
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, plan))
        }
    }
}

fn write_plan_review(format: OutputFormat, instance_id: &str, plan: &PlanRecord) -> Result<()> {
    match format {
        OutputFormat::Text => {
            write_plan(OutputFormat::Text, instance_id, plan)?;
            println!("workspace: {}", plan.version.workspace_id);
            println!("agent: {}", plan.version.agent.as_str());
            println!(
                "provider: {}",
                plan.version.provider.as_deref().unwrap_or("-")
            );
            println!("model: {}", plan.version.model.as_deref().unwrap_or("-"));
            println!(
                "requirements: {}",
                compact_json(&plan.version.requirements)?
            );
            println!(
                "sandbox policy: {}",
                compact_json(&plan.version.policy_snapshot)?
            );
            println!(
                "limits: wall={}s, attempts={}, tokens={}",
                plan.version.limits.max_wall_time_seconds,
                plan.version.limits.max_attempts,
                plan.version
                    .limits
                    .max_tokens
                    .map_or_else(|| "-".to_owned(), |value| value.to_string())
            );
            println!("context:");
            println!("{}", plan.version.context);
            if !plan.version.completion_criteria.is_empty() {
                println!("completion criteria:");
                for criterion in &plan.version.completion_criteria {
                    println!("- {criterion}");
                }
            }
            if let Some(approval) = &plan.version.approval {
                println!("approved by: {}", approval.approved_by);
                println!("approved: {}", output::display_time(&approval.approved_at));
            }
            Ok(())
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            output::write_json(&Envelope::new(instance_id, plan))
        }
    }
}

fn compact_json(value: &impl serde::Serialize) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| ReactorError::OutputSerialization(error.to_string()))
}
