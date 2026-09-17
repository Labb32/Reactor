use serde::{Deserialize, Serialize};

use crate::error::{ReactorError, Result};

const DEFAULT_DEADLINE_SECONDS: u64 = 3_600;
const MAX_DEADLINE_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleRule {
    Immediate,
    Manual,
    At { local_time: String },
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInput {
    pub plan_id: String,
    #[serde(default)]
    pub plan_version: Option<u32>,
    pub rule: ScheduleRule,
    #[serde(default = "default_deadline_seconds")]
    pub start_deadline_seconds: u64,
    #[serde(default)]
    pub local_endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ScheduleRecord {
    pub id: String,
    pub plan_id: String,
    pub plan_version: u32,
    pub status: String,
    pub rule: ScheduleRule,
    pub timezone: String,
    pub start_at: Option<String>,
    pub start_deadline: Option<String>,
    pub start_deadline_seconds: u64,
    pub local_endpoint: Option<String>,
    pub occurrence: Option<OccurrenceRecord>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OccurrenceRecord {
    pub id: String,
    pub scheduled_at: String,
    pub deadline_at: Option<String>,
    pub state: String,
    pub run_id: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct QueueRun {
    pub id: String,
    pub workspace_id: String,
    pub state: String,
    pub agent_kind: String,
    pub provider: Option<String>,
    pub local_endpoint: Option<String>,
    pub ready_at: String,
    pub created_at: String,
    pub eligible: bool,
    pub dispatch_order: Option<u32>,
    pub blocked_by: Option<String>,
}

pub struct PreparedSchedule {
    pub timezone: String,
    pub scheduled_at: Option<String>,
    pub deadline_at: Option<String>,
}

pub fn prepare(input: &ScheduleInput) -> Result<PreparedSchedule> {
    validate(input)?;
    let timezone = jiff::tz::TimeZone::system();
    let Some(local_time) = local_time(&input.rule) else {
        return Ok(PreparedSchedule {
            timezone: timezone_name(&timezone, jiff::Zoned::now().offset()),
            scheduled_at: None,
            deadline_at: None,
        });
    };
    let civil = local_time
        .parse::<jiff::civil::DateTime>()
        .map_err(|error| ReactorError::InvalidSchedule(format!("invalid local time: {error}")))?;
    let zoned = timezone
        .to_ambiguous_zoned(civil)
        .compatible()
        .map_err(|error| ReactorError::InvalidSchedule(error.to_string()))?;
    let scheduled = zoned.timestamp();
    let deadline = deadline(scheduled, input.start_deadline_seconds)?;
    Ok(PreparedSchedule {
        timezone: timezone_name(&timezone, zoned.offset()),
        scheduled_at: Some(scheduled.to_string()),
        deadline_at: Some(deadline.to_string()),
    })
}

fn timezone_name(timezone: &jiff::tz::TimeZone, offset: jiff::tz::Offset) -> String {
    timezone
        .iana_name()
        .map(str::to_owned)
        .unwrap_or_else(|| format!("UTC{offset}"))
}

pub fn deadline(timestamp: jiff::Timestamp, seconds: u64) -> Result<jiff::Timestamp> {
    let seconds = i64::try_from(seconds)
        .map_err(|_| ReactorError::InvalidSchedule("deadline is too large".to_owned()))?;
    timestamp
        .checked_add(jiff::SignedDuration::from_secs(seconds))
        .map_err(|error| ReactorError::InvalidSchedule(error.to_string()))
}

fn validate(input: &ScheduleInput) -> Result<()> {
    if !(1..=MAX_DEADLINE_SECONDS).contains(&input.start_deadline_seconds) {
        return Err(ReactorError::InvalidSchedule(
            "start deadline must be between 1 second and 7 days".to_owned(),
        ));
    }
    if input.local_endpoint.as_deref().is_some_and(|value| {
        value.trim().is_empty()
            || value != value.trim()
            || value.len() > 128
            || value.chars().any(char::is_control)
    }) {
        return Err(ReactorError::InvalidSchedule(
            "local endpoint must contain 1 to 128 bytes without surrounding whitespace or control characters"
                .to_owned(),
        ));
    }
    if let ScheduleRule::At { local_time } = &input.rule
        && (local_time.trim().is_empty() || local_time.len() > 64)
    {
        return Err(ReactorError::InvalidSchedule(
            "local time must contain 1 to 64 bytes".to_owned(),
        ));
    }
    Ok(())
}

fn local_time(rule: &ScheduleRule) -> Option<&str> {
    match rule {
        ScheduleRule::At { local_time } => Some(local_time),
        ScheduleRule::Immediate | ScheduleRule::Manual => None,
    }
}

const fn default_deadline_seconds() -> u64 {
    DEFAULT_DEADLINE_SECONDS
}
