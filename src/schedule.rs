use serde::{Deserialize, Serialize};

use crate::error::{ReactorError, Result};

pub const DEFAULT_DEADLINE_SECONDS: u64 = 3_600;
const MAX_DEADLINE_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_INTERVAL_SECONDS: u64 = 365 * 24 * 60 * 60;
const MAX_SCHEDULE_ID_BYTES: usize = 96;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleRule {
    Immediate,
    Manual,
    At {
        local_time: String,
    },
    Daily {
        local_time: String,
    },
    Weekdays {
        local_time: String,
    },
    Weekly {
        weekday: ScheduleWeekday,
        local_time: String,
    },
    IntervalAfterSuccess {
        seconds: u64,
    },
}

pub fn rule_name(rule: &ScheduleRule) -> &'static str {
    match rule {
        ScheduleRule::Immediate => "immediate",
        ScheduleRule::Manual => "manual",
        ScheduleRule::At { .. } => "at",
        ScheduleRule::Daily { .. } => "daily",
        ScheduleRule::Weekdays { .. } => "weekdays",
        ScheduleRule::Weekly { .. } => "weekly",
        ScheduleRule::IntervalAfterSuccess { .. } => "interval_after_success",
    }
}

pub fn validate_id(id: &str) -> Result<()> {
    let mut characters = id.chars();
    let valid_first = characters
        .next()
        .is_some_and(|value| value.is_ascii_alphanumeric());
    let valid_rest = characters.all(|value| value.is_ascii_alphanumeric() || ".-_".contains(value));
    if !valid_first || !valid_rest || id.len() > MAX_SCHEDULE_ID_BYTES {
        return Err(ReactorError::InvalidSchedule(format!(
            "schedule name must start with an ASCII letter or number and contain at most {MAX_SCHEDULE_ID_BYTES} letters, numbers, '.', '-', or '_'"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleWeekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl ScheduleWeekday {
    const fn to_jiff(self) -> jiff::civil::Weekday {
        use jiff::civil::Weekday;
        match self {
            Self::Monday => Weekday::Monday,
            Self::Tuesday => Weekday::Tuesday,
            Self::Wednesday => Weekday::Wednesday,
            Self::Thursday => Weekday::Thursday,
            Self::Friday => Weekday::Friday,
            Self::Saturday => Weekday::Saturday,
            Self::Sunday => Weekday::Sunday,
        }
    }
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
    #[serde(default)]
    pub after_quota_reset: bool,
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
    #[serde(default)]
    pub after_quota_reset: bool,
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
    pub scheduled_at: Option<jiff::Timestamp>,
}

pub fn prepare(input: &ScheduleInput) -> Result<PreparedSchedule> {
    validate(input)?;
    let timezone = jiff::tz::TimeZone::system();
    let now = jiff::Timestamp::now();
    let scheduled = match &input.rule {
        ScheduleRule::At { local_time } => Some(parse_at(&timezone, local_time)?),
        ScheduleRule::Daily { .. }
        | ScheduleRule::Weekdays { .. }
        | ScheduleRule::Weekly { .. } => next_calendar_occurrence(&input.rule, &timezone, now)?,
        ScheduleRule::Immediate
        | ScheduleRule::Manual
        | ScheduleRule::IntervalAfterSuccess { .. } => None,
    };
    let offset = timezone.to_offset(scheduled.unwrap_or(now));
    Ok(PreparedSchedule {
        timezone: timezone_name(&timezone, offset),
        scheduled_at: scheduled,
    })
}

pub(crate) fn next_occurrence(
    rule: &ScheduleRule,
    timezone_name: &str,
    after: jiff::Timestamp,
) -> Result<Option<jiff::Timestamp>> {
    match rule {
        ScheduleRule::Daily { .. }
        | ScheduleRule::Weekdays { .. }
        | ScheduleRule::Weekly { .. } => {
            let timezone = load_timezone(timezone_name)?;
            next_calendar_occurrence(rule, &timezone, after)
        }
        _ => Ok(None),
    }
}

pub(crate) fn interval_after_success(
    rule: &ScheduleRule,
    completed_at: jiff::Timestamp,
) -> Result<Option<jiff::Timestamp>> {
    let ScheduleRule::IntervalAfterSuccess { seconds } = rule else {
        return Ok(None);
    };
    deadline(completed_at, *seconds).map(Some)
}

fn next_calendar_occurrence(
    rule: &ScheduleRule,
    timezone: &jiff::tz::TimeZone,
    after: jiff::Timestamp,
) -> Result<Option<jiff::Timestamp>> {
    let local_time = match rule {
        ScheduleRule::Daily { local_time }
        | ScheduleRule::Weekdays { local_time }
        | ScheduleRule::Weekly { local_time, .. } => local_time,
        _ => return Ok(None),
    };
    let time = local_time
        .parse::<jiff::civil::Time>()
        .map_err(|error| ReactorError::InvalidSchedule(format!("invalid local time: {error}")))?;
    let mut date = timezone.to_datetime(after).date();
    for _ in 0..=7 {
        if date_matches(rule, date.weekday()) {
            let timestamp = timezone
                .to_ambiguous_zoned(date.to_datetime(time))
                .compatible()
                .map_err(|error| ReactorError::InvalidSchedule(error.to_string()))?
                .timestamp();
            if timestamp > after {
                return Ok(Some(timestamp));
            }
        }
        date = date
            .tomorrow()
            .map_err(|error| ReactorError::InvalidSchedule(error.to_string()))?;
    }
    Err(ReactorError::InvalidSchedule(
        "could not find the next recurring occurrence".to_owned(),
    ))
}

fn date_matches(rule: &ScheduleRule, weekday: jiff::civil::Weekday) -> bool {
    match rule {
        ScheduleRule::Daily { .. } => true,
        ScheduleRule::Weekdays { .. } => !matches!(
            weekday,
            jiff::civil::Weekday::Saturday | jiff::civil::Weekday::Sunday
        ),
        ScheduleRule::Weekly {
            weekday: expected, ..
        } => weekday == expected.to_jiff(),
        _ => false,
    }
}

fn parse_at(timezone: &jiff::tz::TimeZone, value: &str) -> Result<jiff::Timestamp> {
    let civil = value
        .parse::<jiff::civil::DateTime>()
        .map_err(|error| ReactorError::InvalidSchedule(format!("invalid local time: {error}")))?;
    timezone
        .to_ambiguous_zoned(civil)
        .compatible()
        .map(|zoned| zoned.timestamp())
        .map_err(|error| ReactorError::InvalidSchedule(error.to_string()))
}

fn load_timezone(name: &str) -> Result<jiff::tz::TimeZone> {
    if let Some(offset) = name.strip_prefix("UTC") {
        return parse_offset(offset).map(jiff::tz::TimeZone::fixed);
    }
    jiff::tz::TimeZone::get(name)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
}

fn parse_offset(value: &str) -> Result<jiff::tz::Offset> {
    let (sign, value) = match value.as_bytes().first() {
        Some(b'+') => (1, &value[1..]),
        Some(b'-') => (-1, &value[1..]),
        _ => {
            return Err(ReactorError::InvalidStoredData(
                "stored UTC offset is invalid".to_owned(),
            ));
        }
    };
    let mut parts = value.split(':');
    let hours = parts.next().and_then(|part| part.parse::<i32>().ok());
    let minutes = parts
        .next()
        .map(|part| part.parse::<i32>().ok())
        .unwrap_or(Some(0));
    let seconds = parts
        .next()
        .map(|part| part.parse::<i32>().ok())
        .unwrap_or(Some(0));
    if parts.next().is_some()
        || hours.is_some_and(|value| !(0..=25).contains(&value))
        || minutes.is_some_and(|value| !(0..60).contains(&value))
        || seconds.is_some_and(|value| !(0..60).contains(&value))
    {
        return Err(ReactorError::InvalidStoredData(
            "stored UTC offset is invalid".to_owned(),
        ));
    }
    let total = hours
        .zip(minutes)
        .zip(seconds)
        .and_then(|((hours, minutes), seconds)| {
            hours
                .checked_mul(3_600)?
                .checked_add(minutes.checked_mul(60)?)?
                .checked_add(seconds)?
                .checked_mul(sign)
        })
        .ok_or_else(|| {
            ReactorError::InvalidStoredData("stored UTC offset is invalid".to_owned())
        })?;
    jiff::tz::Offset::from_seconds(total)
        .map_err(|error| ReactorError::InvalidStoredData(error.to_string()))
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
    match &input.rule {
        ScheduleRule::At { local_time } => validate_time_text(local_time, "local time")?,
        ScheduleRule::Daily { local_time } | ScheduleRule::Weekdays { local_time } => {
            validate_time_text(local_time, "local time")?;
            local_time.parse::<jiff::civil::Time>().map_err(|error| {
                ReactorError::InvalidSchedule(format!("invalid local time: {error}"))
            })?;
        }
        ScheduleRule::Weekly { local_time, .. } => {
            validate_time_text(local_time, "local time")?;
            local_time.parse::<jiff::civil::Time>().map_err(|error| {
                ReactorError::InvalidSchedule(format!("invalid local time: {error}"))
            })?;
        }
        ScheduleRule::IntervalAfterSuccess { seconds }
            if !(1..=MAX_INTERVAL_SECONDS).contains(seconds) =>
        {
            return Err(ReactorError::InvalidSchedule(
                "success interval must be between 1 second and 365 days".to_owned(),
            ));
        }
        _ => {}
    }
    Ok(())
}

fn validate_time_text(value: &str, name: &str) -> Result<()> {
    if value.trim().is_empty() || value != value.trim() || value.len() > 64 {
        return Err(ReactorError::InvalidSchedule(format!(
            "{name} must contain 1 to 64 bytes without surrounding whitespace"
        )));
    }
    Ok(())
}

const fn default_deadline_seconds() -> u64 {
    DEFAULT_DEADLINE_SECONDS
}
