use std::path::PathBuf;

use crate::{CommandRecord, GitVersion, RemoteFailure};

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
    #[error("Bonsai could not update {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{message}{}", backup_note(.backup.as_deref()))]
    Purge {
        message: String,
        /// The safety backup, when one was written before the failure.
        backup: Option<PathBuf>,
    },
    #[error("Unexpected output from Git: {0}")]
    Parse(String),
}

impl Error {
    /// The recognized cause when a network operation failed.
    #[must_use]
    pub fn remote_failure(&self) -> Option<RemoteFailure> {
        match self {
            Self::Failed { record } if is_network(record) => {
                RemoteFailure::classify(&record.stderr)
            }
            _ => None,
        }
    }
}

fn backup_note(backup: Option<&std::path::Path>) -> String {
    backup.map_or_else(String::new, |path| {
        format!(
            "\n\nThe original history is saved in {}. Restore it from the purge sheet.",
            path.display()
        )
    })
}

fn failure_message(record: &CommandRecord) -> String {
    let stderr = record.stderr.trim();
    let explanation = is_network(record)
        .then(|| {
            RemoteFailure::classify(stderr)
                .map(RemoteFailure::message)
                .or_else(|| crate::SyncFailure::classify(stderr).map(crate::SyncFailure::message))
        })
        .flatten();
    if let Some(explanation) = explanation {
        format!("{explanation}\n\n{stderr}")
    } else if stderr.is_empty() {
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

/// Whether the command talks to a remote, so its failure may be a remote one.
fn is_network(record: &CommandRecord) -> bool {
    record.args.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "clone" | "fetch" | "pull" | "push" | "ls-remote"
        )
    })
}
