//! Single-line status display, replacing reco-gui's toast stack.
//!
//! This tool only ever needs to tell the user about one thing at a
//! time (the last action's outcome), so a queue/TTL/dismiss system is
//! unnecessary — callers just overwrite the status line.

/// Severity of a status message, used to color the status line in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Info,
    Warn,
    Error,
}

/// The current status line: message text plus its severity.
#[derive(Debug, Clone)]
pub struct StatusLine {
    pub text: String,
    pub severity: Severity,
}

impl StatusLine {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: Severity::Info,
        }
    }

    pub fn warn(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: Severity::Warn,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            severity: Severity::Error,
        }
    }
}
