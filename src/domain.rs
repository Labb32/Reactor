use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WorkspaceStatus {
    Registered,
    Ready,
    Degraded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlanStatus {
    Draft,
    InReview,
    Approved,
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
                    Self::Running | Self::Failed | Self::Cancelled
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
}

impl fmt::Display for RunState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
