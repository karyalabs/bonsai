//! Merge, rebase, cherry-pick, revert and reset, and continuing, skipping or
//! aborting an operation that stopped (CONTEXT.md: Operation in progress).

use std::{ffi::OsString, path::PathBuf};

use crate::{Error, InProgress, Repository, Result};

/// How a merge may combine histories.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MergeMode {
    /// Fast-forward when possible, otherwise create a merge commit.
    #[default]
    Default,
    FastForwardOnly,
    /// Always create a merge commit.
    NoFastForward,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetMode {
    /// Move the branch only; changes stay staged.
    Soft,
    /// Move the branch and the index; changes stay in the work tree.
    Mixed,
    /// Move the branch and discard every uncommitted change.
    Hard,
}

/// How an integration ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Integration {
    Completed,
    /// Stopped with conflicts; the operation is in progress.
    Conflicts,
}

/// Which side's version resolves a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ours,
    Theirs,
}

impl Side {
    /// What the side means to a person during `operation`. During a rebase
    /// "ours" is the branch being rebased onto, and "theirs" the user's own
    /// commit being replayed.
    #[must_use]
    pub fn label(self, operation: Option<InProgress>) -> &'static str {
        match (self, operation) {
            (Self::Ours, Some(InProgress::Rebase)) => "Keep Upstream Version",
            (Self::Theirs, Some(InProgress::Rebase)) => "Keep Your Commit's Version",
            (Self::Ours, _) => "Keep Current Branch's Version",
            (Self::Theirs, _) => "Keep Incoming Version",
        }
    }

    fn flag(self) -> &'static str {
        match self {
            Self::Ours => "--ours",
            Self::Theirs => "--theirs",
        }
    }
}

/// What can be done with an operation in progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationAction {
    Continue,
    Skip,
    Abort,
}

impl OperationAction {
    /// Whether this action applies to `operation`.
    #[must_use]
    pub fn applies_to(self, operation: InProgress) -> bool {
        !matches!(
            (self, operation),
            (Self::Skip, InProgress::Merge | InProgress::Bisect)
                | (Self::Continue, InProgress::Bisect)
        )
    }
}

impl Repository {
    /// Runs an integrating command; a stop with conflicts is not an error.
    fn integrate(&self, args: Vec<OsString>) -> Result<Integration> {
        let command = self
            .git()
            .command(args)
            .cwd(self.root())
            // Accept Git's prepared messages instead of opening an editor.
            .env("GIT_EDITOR", "true")
            .env("GIT_SEQUENCE_EDITOR", "true");
        match self.git().run(&command) {
            Ok(_) => Ok(Integration::Completed),
            Err(error @ Error::Failed { .. }) => {
                if self.in_progress().is_some() && self.has_conflicts()? {
                    Ok(Integration::Conflicts)
                } else {
                    Err(error)
                }
            }
            Err(error) => Err(error),
        }
    }

    fn has_conflicts(&self) -> Result<bool> {
        let output = self.git().run(
            &self
                .git()
                .command(["diff", "--name-only", "--diff-filter=U", "-z"])
                .cwd(self.root()),
        )?;
        Ok(!output.stdout.is_empty())
    }

    /// Merges `revision` into the current branch.
    pub fn merge(&self, revision: &str, mode: MergeMode) -> Result<Integration> {
        let mut args: Vec<OsString> = vec!["merge".into(), "--no-edit".into()];
        match mode {
            MergeMode::Default => {}
            MergeMode::FastForwardOnly => args.push("--ff-only".into()),
            MergeMode::NoFastForward => args.push("--no-ff".into()),
        }
        args.push(revision.into());
        self.integrate(args)
    }

    /// Replays the current branch's own commits on top of `onto`.
    pub fn rebase(&self, onto: &str) -> Result<Integration> {
        self.integrate(vec!["rebase".into(), onto.into()])
    }

    /// Applies these commits, oldest first, on the current branch.
    pub fn cherry_pick(&self, commits: &[&str]) -> Result<Integration> {
        let mut args: Vec<OsString> = vec!["cherry-pick".into()];
        args.extend(commits.iter().map(OsString::from));
        self.integrate(args)
    }

    /// Creates a commit that undoes `commit`.
    pub fn revert(&self, commit: &str) -> Result<Integration> {
        self.integrate(vec!["revert".into(), "--no-edit".into(), commit.into()])
    }

    /// Moves the current branch to `revision`.
    pub fn reset(&self, revision: &str, mode: ResetMode) -> Result<()> {
        let mode = match mode {
            ResetMode::Soft => "--soft",
            ResetMode::Mixed => "--mixed",
            ResetMode::Hard => "--hard",
        };
        self.git().run(
            &self
                .git()
                .command(["reset", "-q", mode, revision])
                .cwd(self.root()),
        )?;
        Ok(())
    }

    /// Continues, skips or aborts the operation in progress.
    pub fn resolve_operation(&self, action: OperationAction) -> Result<Integration> {
        let Some(operation) = self.in_progress() else {
            return Ok(Integration::Completed);
        };
        let command = match operation {
            InProgress::Merge => "merge",
            InProgress::Rebase => "rebase",
            InProgress::ApplyMailbox => "am",
            InProgress::CherryPick => "cherry-pick",
            InProgress::Revert => "revert",
            InProgress::Bisect => {
                self.git()
                    .run(&self.git().command(["bisect", "reset"]).cwd(self.root()))?;
                return Ok(Integration::Completed);
            }
        };
        let flag = match action {
            OperationAction::Continue => "--continue",
            OperationAction::Skip => "--skip",
            OperationAction::Abort => "--abort",
        };
        let result = self.integrate(vec![command.into(), flag.into()]);
        if action == OperationAction::Abort {
            return result;
        }
        match result {
            // Continuing a sequence can stop at the next conflict.
            Ok(Integration::Completed) if self.in_progress().is_some() => {
                Ok(Integration::Conflicts)
            }
            other => other,
        }
    }

    /// Resolves conflicted paths with one side's version and marks them
    /// resolved. A path the side deleted is removed.
    pub fn resolve_with(&self, paths: &[PathBuf], side: Side) -> Result<()> {
        for path in paths {
            let checkout = self.git().run_unchecked(
                &self
                    .git()
                    .command([
                        OsString::from("checkout"),
                        side.flag().into(),
                        "--".into(),
                        path.as_os_str().to_owned(),
                    ])
                    .cwd(self.root()),
            )?;
            let args: Vec<OsString> = if checkout.status.success() {
                vec!["add".into(), "--".into(), path.as_os_str().to_owned()]
            } else {
                vec![
                    "rm".into(),
                    "-q".into(),
                    "--".into(),
                    path.as_os_str().to_owned(),
                ]
            };
            self.git().run(&self.git().command(args).cwd(self.root()))?;
        }
        Ok(())
    }

    /// Marks conflicted paths resolved as they are in the work tree.
    pub fn mark_resolved(&self, paths: &[PathBuf]) -> Result<()> {
        self.stage(paths)
    }
}
