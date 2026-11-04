use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    agents::AgentKind,
    error::{ReactorError, Result},
    workspace::{PolicyAccess, WorkspaceRecord, canonicalize},
};

const MAX_TITLE_BYTES: usize = 200;
const MAX_CONTEXT_BYTES: usize = 1024 * 1024;
const MAX_COMPLETION_CRITERIA: usize = 32;
const MAX_TOOLS: usize = 32;
const MAX_WALL_TIME_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_ATTEMPTS: u32 = 10;
const MAX_PLAN_ID_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequirementMode {
    Inherit,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PlanRequirements {
    Mode(RequirementMode),
    Explicit(ExplicitRequirements),
}

impl Default for PlanRequirements {
    fn default() -> Self {
        Self::Mode(RequirementMode::Inherit)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExplicitRequirements {
    pub writable_roots: Vec<String>,
    pub network: bool,
    pub mcp: bool,
    pub hooks: bool,
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlanLimits {
    #[serde(rename = "execution_timeout_seconds", alias = "max_wall_time_seconds")]
    pub max_wall_time_seconds: u64,
    pub max_attempts: u32,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanTask {
    pub id: String,
    #[serde(default)]
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_file: Option<String>,
    #[serde(default)]
    pub pause_on_warnings: bool,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_wall_time_seconds: 7_200,
            max_attempts: 1,
            max_tokens: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlanInput {
    pub workspace_id: String,
    pub title: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub tasks: Vec<PlanTask>,
    pub agent: AgentKind,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub requirements: PlanRequirements,
    #[serde(default)]
    pub completion_criteria: Vec<String>,
    #[serde(default)]
    pub limits: PlanLimits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentPlanDraft {
    pub title: String,
    pub context: String,
    pub requirements: ExplicitRequirements,
    pub completion_criteria: Vec<String>,
}

impl AgentPlanDraft {
    pub fn into_input(
        self,
        workspace_id: String,
        agent: AgentKind,
        provider: Option<String>,
        model: Option<String>,
        limits: PlanLimits,
    ) -> PlanInput {
        PlanInput {
            tasks: Vec::new(),
            workspace_id,
            title: self.title,
            context: self.context,
            agent,
            provider,
            model,
            requirements: PlanRequirements::Explicit(self.requirements),
            completion_criteria: self.completion_criteria,
            limits,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanRecord {
    pub id: String,
    pub current_version: u32,
    pub version: PlanVersionRecord,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanVersionRecord {
    pub version: u32,
    pub workspace_id: String,
    pub status: String,
    pub title: String,
    pub context: String,
    #[serde(default)]
    pub tasks: Vec<PlanTask>,
    pub completion_criteria: Vec<String>,
    pub agent: AgentKind,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub requirements: PlanRequirements,
    pub limits: PlanLimits,
    pub policy_snapshot: crate::workspace::WorkspacePolicy,
    pub workspace_policy_fingerprint: String,
    pub approval: Option<PlanApproval>,
    pub created_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlanApproval {
    pub approved_by: String,
    pub approved_at: String,
}

pub fn prepare_input(mut input: PlanInput, workspace: &WorkspaceRecord) -> Result<PlanInput> {
    use std::io::Read;
    for task in &mut input.tasks {
        if let Some(path) = task.context_file.take() {
            if !task.context.trim().is_empty() {
                return Err(ReactorError::InvalidPlan("task context and context_file are mutually exclusive".to_owned()));
            }
            let file = std::fs::File::open(&path).map_err(|error| crate::error::io_error(&path, error))?;
            let mut text = String::new();
            file.take(MAX_CONTEXT_BYTES as u64 + 1).read_to_string(&mut text)
                .map_err(|error| crate::error::io_error(&path, error))?;
            task.context = text;
        }
    }
    validate_input(&input)?;
    input.provider = Some(
        input
            .agent
            .adapter()
            .provider_group(input.provider.as_deref()),
    );
    input.requirements = normalize_requirements(&input.requirements, workspace)?;
    Ok(input)
}

pub fn validate_id(id: &str) -> Result<()> {
    let mut characters = id.chars();
    let valid_first = characters
        .next()
        .is_some_and(|value| value.is_ascii_alphanumeric());
    let valid_rest = characters.all(|value| value.is_ascii_alphanumeric() || ".-_".contains(value));
    if !valid_first || !valid_rest || id.len() > MAX_PLAN_ID_BYTES {
        return Err(ReactorError::InvalidPlan(format!(
            "plan name must start with an ASCII letter or number and contain at most {MAX_PLAN_ID_BYTES} letters, numbers, '.', '-', or '_'"
        )));
    }
    Ok(())
}

pub fn validate_requirements(
    requirements: &PlanRequirements,
    workspace: &WorkspaceRecord,
) -> Result<()> {
    let PlanRequirements::Explicit(requirements) = requirements else {
        return Ok(());
    };
    require_access("network", requirements.network, workspace.policy.network)?;
    require_access("mcp", requirements.mcp, workspace.policy.mcp)?;
    require_access("hooks", requirements.hooks, workspace.policy.hooks)?;

    let allowed = workspace.write_paths();
    for value in &requirements.writable_roots {
        let requested = Path::new(value);
        if !allowed.iter().any(|root| requested.starts_with(root)) {
            return Err(ReactorError::PlanRequirementUnavailable {
                requirement: format!("writable_root:{value}"),
            });
        }
    }
    Ok(())
}

pub fn validate_tools(requirements: &PlanRequirements) -> Result<()> {
    let PlanRequirements::Explicit(requirements) = requirements else {
        return Ok(());
    };
    for tool in &requirements.tools {
        if find_tool(tool).is_none() {
            return Err(ReactorError::PlanRequirementUnavailable {
                requirement: format!("tool:{tool}"),
            });
        }
    }
    Ok(())
}

fn validate_input(input: &PlanInput) -> Result<()> {
    let title = input.title.trim();
    if title.is_empty() || title.len() > MAX_TITLE_BYTES || title.chars().any(char::is_control) {
        return Err(ReactorError::InvalidPlan(
            "title must contain 1 to 200 bytes without control characters".to_owned(),
        ));
    }
    if (input.context.trim().is_empty() && input.tasks.is_empty()) || input.context.len() > MAX_CONTEXT_BYTES {
        return Err(ReactorError::InvalidPlan(
            "context must contain 1 byte to 1 MiB".to_owned(),
        ));
    }
    if input.tasks.len() > 100 {
        return Err(ReactorError::InvalidPlan("at most 100 sequential tasks are supported".to_owned()));
    }
    if !input.tasks.is_empty() && !matches!(input.agent, AgentKind::Codex) {
        return Err(ReactorError::InvalidPlan("sequential task execution currently requires Codex".to_owned()));
    }
    let mut ids = std::collections::HashSet::new();
    let mut total_context = input.context.len();
    for task in &input.tasks {
        validate_id(&task.id)?;
        total_context = total_context.saturating_add(task.context.len());
        if !ids.insert(&task.id) || task.context.trim().is_empty() || task.context.len() > MAX_CONTEXT_BYTES {
            return Err(ReactorError::InvalidPlan("task IDs must be unique and task context must contain 1 byte to 1 MiB".to_owned()));
        }
    }
    if total_context > 8 * MAX_CONTEXT_BYTES {
        return Err(ReactorError::InvalidPlan("combined task context exceeds 8 MiB".to_owned()));
    }
    if input.completion_criteria.len() > MAX_COMPLETION_CRITERIA
        || input
            .completion_criteria
            .iter()
            .any(|criterion| criterion.trim().is_empty() || criterion.len() > 1_000)
    {
        return Err(ReactorError::InvalidPlan(
            "completion criteria must contain at most 32 non-empty values of 1,000 bytes"
                .to_owned(),
        ));
    }
    if !(1..=MAX_WALL_TIME_SECONDS).contains(&input.limits.max_wall_time_seconds)
        || !(1..=MAX_ATTEMPTS).contains(&input.limits.max_attempts)
    {
        return Err(ReactorError::InvalidPlan(
            "wall time must be between 1 second and 7 days, and attempts between 1 and 10"
                .to_owned(),
        ));
    }
    if input
        .limits
        .max_tokens
        .is_some_and(|value| value == 0 || value > i64::MAX as u64)
    {
        return Err(ReactorError::InvalidPlan(
            "token limit must be between 1 and 9223372036854775807".to_owned(),
        ));
    }
    validate_optional_name("provider", input.provider.as_deref())?;
    validate_optional_name("model", input.model.as_deref())?;
    if let PlanRequirements::Explicit(requirements) = &input.requirements
        && (requirements.tools.len() > MAX_TOOLS
            || requirements.tools.iter().any(|tool| !valid_tool(tool)))
    {
        return Err(ReactorError::InvalidPlan(
            "tools must contain at most 32 executable names".to_owned(),
        ));
    }
    Ok(())
}

fn validate_optional_name(field: &str, value: Option<&str>) -> Result<()> {
    if value.is_some_and(|value| {
        value.trim().is_empty()
            || value != value.trim()
            || value.len() > 128
            || value.chars().any(char::is_control)
    }) {
        return Err(ReactorError::InvalidPlan(format!(
            "{field} must contain 1 to 128 bytes without surrounding whitespace or control characters"
        )));
    }
    Ok(())
}

fn normalize_requirements(
    requirements: &PlanRequirements,
    workspace: &WorkspaceRecord,
) -> Result<PlanRequirements> {
    let PlanRequirements::Explicit(requirements) = requirements else {
        return Ok(requirements.clone());
    };
    let workspace_path = Path::new(&workspace.canonical_path);
    let mut roots = Vec::new();
    for value in &requirements.writable_roots {
        let input = if value == "workspace" {
            workspace_path.to_owned()
        } else {
            let path = Path::new(value);
            if path.is_absolute() {
                path.to_owned()
            } else {
                workspace_path.join(path)
            }
        };
        let root = canonicalize(&input)?;
        if !roots.iter().any(|stored: &String| stored == &root.display) {
            roots.push(root.display);
        }
    }
    let normalized = PlanRequirements::Explicit(ExplicitRequirements {
        writable_roots: roots,
        network: requirements.network,
        mcp: requirements.mcp,
        hooks: requirements.hooks,
        tools: requirements.tools.clone(),
    });
    validate_requirements(&normalized, workspace)?;
    Ok(normalized)
}

fn require_access(name: &str, required: bool, policy: PolicyAccess) -> Result<()> {
    if required && !policy.is_allowed() {
        return Err(ReactorError::PlanRequirementUnavailable {
            requirement: name.to_owned(),
        });
    }
    Ok(())
}

fn valid_tool(tool: &str) -> bool {
    !tool.is_empty()
        && tool.len() <= 64
        && tool
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || ".-_+".contains(value))
}

fn find_tool(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let extensions = executable_extensions();
    std::env::split_paths(&path).find_map(|directory| {
        extensions
            .iter()
            .map(|extension| directory.join(format!("{name}{extension}")))
            .find(|candidate| is_executable(candidate))
    })
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.metadata()
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(windows)]
fn executable_extensions() -> Vec<String> {
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned())
        .split(';')
        .map(str::to_ascii_lowercase)
        .chain(std::iter::once(String::new()))
        .collect()
}

#[cfg(not(windows))]
fn executable_extensions() -> Vec<String> {
    vec![String::new()]
}
