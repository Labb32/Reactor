use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlanStatus {
    Draft,
    InReview,
    Approved,
}

impl PlanStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "DRAFT",
            Self::InReview => "IN_REVIEW",
            Self::Approved => "APPROVED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RunState {
    Waiting,
    Queued,
    Preparing,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    LimitReached,
    ReviewRequired,
    Missed,
}

impl RunState {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "WAITING" => Some(Self::Waiting),
            "QUEUED" => Some(Self::Queued),
            "PREPARING" => Some(Self::Preparing),
            "RUNNING" => Some(Self::Running),
            "SUCCEEDED" => Some(Self::Succeeded),
            "FAILED" => Some(Self::Failed),
            "CANCELLED" => Some(Self::Cancelled),
            "LIMIT_REACHED" => Some(Self::LimitReached),
            "REVIEW_REQUIRED" => Some(Self::ReviewRequired),
            "MISSED" => Some(Self::Missed),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "WAITING",
            Self::Queued => "QUEUED",
            Self::Preparing => "PREPARING",
            Self::Running => "RUNNING",
            Self::Succeeded => "SUCCEEDED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::LimitReached => "LIMIT_REACHED",
            Self::ReviewRequired => "REVIEW_REQUIRED",
            Self::Missed => "MISSED",
        }
    }

    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Waiting, Self::Queued | Self::Missed | Self::Cancelled)
                | (
                    Self::Queued,
                    Self::Preparing | Self::Cancelled | Self::Missed
                )
                | (
                    Self::Preparing,
                    Self::Succeeded | Self::Running
                        | Self::Failed
                        | Self::Cancelled
                        | Self::LimitReached
                        | Self::ReviewRequired
                )
                | (
                    Self::Running,
                    Self::Succeeded
                        | Self::Failed
                        | Self::Cancelled
                        | Self::LimitReached
                        | Self::ReviewRequired
                )
        )
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded
                | Self::Failed
                | Self::Cancelled
                | Self::LimitReached
                | Self::ReviewRequired
                | Self::Missed
        )
    }

    pub const fn can_clear(self, include_review: bool) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::LimitReached | Self::Missed
        ) || include_review && matches!(self, Self::ReviewRequired)
    }
}

impl fmt::Display for RunState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
