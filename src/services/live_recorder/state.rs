use serde::Serialize;

use super::MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum UnexpectedExitAction {
    CompleteAfterOfflineConfirmation,
    Recover,
    FailRecoverable,
}

pub(super) fn unexpected_exit_action(
    room_is_offline: Option<bool>,
    restart_attempts: u32,
) -> UnexpectedExitAction {
    match room_is_offline {
        Some(true) => UnexpectedExitAction::CompleteAfterOfflineConfirmation,
        Some(false) | None if restart_attempts < MAX_UNEXPECTED_EXIT_RECOVERY_ATTEMPTS => {
            UnexpectedExitAction::Recover
        }
        Some(false) | None => UnexpectedExitAction::FailRecoverable,
    }
}

/// 录制状态。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingStatus {
    Starting,
    Recording,
    Stopping,
    Finalizing,
    Stopped,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for RecordingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "starting"),
            Self::Recording => write!(f, "recording"),
            Self::Stopping => write!(f, "stopping"),
            Self::Finalizing => write!(f, "finalizing"),
            Self::Stopped => write!(f, "stopped"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordingTrigger {
    Manual,
    Auto,
}

impl RecordingTrigger {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }
}
