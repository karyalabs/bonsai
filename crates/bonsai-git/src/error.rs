use std::path::PathBuf;

use crate::{CommandRecord, GitVersion};

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "Git was not found. Install the Xcode Command Line Tools or set the Git path in Settings."
    )]
    GitNotFound,
    #[error("Git {found} is too old. Bonsai needs Git {required} or later.")]
    GitTooOld {
        found: GitVersion,
        required: GitVersion,
    },
    #[error("Git could not be started: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("{}", failure_message(.record))]
    Failed { record: Box<CommandRecord> },
    #[error("The Git command was cancelled.")]
    Cancelled,
    #[error("{} is not a Git repository.", .0.display())]
    NotARepository(PathBuf),
    #[error("Unexpected output from Git: {0}")]
    Parse(String),
}

fn failure_message(record: &CommandRecord) -> String {
    let stderr = record.stderr.trim();
    if stderr.is_empty() {
        format!(
            "`git {}` failed with exit code {}.",
            record.args.join(" "),
            record
                .exit_code
                .map_or_else(|| "unknown".to_owned(), |c| c.to_string())
        )
    } else {
        stderr.to_owned()
    }
}
