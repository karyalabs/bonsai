//! Saving, applying, popping, dropping and reading stashes (CONTEXT.md:
//! Stash).

use std::path::PathBuf;

use crate::{DiffTarget, Error, Integration, Oid, Repository, Result};

/// The files a stash holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StashContents {
    /// Tracked files the stash changes.
    pub tracked: Vec<PathBuf>,
    /// Untracked files saved with the stash.
    pub untracked: Vec<PathBuf>,
}

fn nul_paths(bytes: &[u8]) -> Vec<PathBuf> {
    bytes
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(crate::purge::bytes_to_path)
        .collect()
}

impl Repository {
    /// The `stash@{n}` name for `index`, after checking it still names
    /// `oid`, since the list shifts as stashes are saved and dropped.
    pub(crate) fn stash_ref(&self, index: usize, oid: &Oid) -> Result<String> {
        let name = format!("stash@{{{index}}}");
        let output = self.git().run_unchecked(
            &self
                .git()
                .command(["rev-parse", "--verify", "-q", &name])
                .cwd(self.root()),
        )?;
        if output.stdout_lossy().trim() == oid.as_str() {
            Ok(name)
        } else {
            Err(Error::Refused(
                "The stash list changed. Refresh and try again.".to_owned(),
            ))
        }
    }

    /// Stashes uncommitted changes with a message (Git writes one when it
    /// is empty), including untracked files when asked.
    pub fn stash_save(&self, message: &str, include_untracked: bool) -> Result<()> {
        self.stash_paths(message, include_untracked, &[])
    }

    /// Stashes only the changes to `paths` (all changes when empty), leaving
    /// the rest in the working tree.
    pub fn stash_paths(
        &self,
        message: &str,
        include_untracked: bool,
        paths: &[PathBuf],
    ) -> Result<()> {
        let mut args = vec!["stash", "push"];
        if include_untracked {
            args.push("--include-untracked");
        }
        if !message.trim().is_empty() {
            args.extend(["-m", message.trim()]);
        }
        let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        if !paths.is_empty() {
            args.push("--".into());
            args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
        }
        self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(())
    }

    /// Applies a stash to the work tree; `pop` also drops it when it applied
    /// cleanly. Conflicts leave markers in the files and keep the stash.
    pub fn stash_apply(&self, index: usize, oid: &Oid, pop: bool) -> Result<Integration> {
        let name = self.stash_ref(index, oid)?;
        let verb = if pop { "pop" } else { "apply" };
        match self
            .git()
            .run(&self.git().command(["stash", verb, &name]).cwd(self.root()))
        {
            Ok(_) => Ok(Integration::Completed),
            Err(error @ Error::Failed { .. }) => {
                let conflicts = self.git().run(
                    &self
                        .git()
                        .command(["diff", "--name-only", "--diff-filter=U", "-z"])
                        .cwd(self.root()),
                )?;
                if conflicts.stdout.is_empty() {
                    Err(error)
                } else {
                    Ok(Integration::Conflicts)
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Starts branch `name` at the commit the stash was made on, applies
    /// the stash there and drops it, as `git stash branch` does. A stash
    /// that no longer applies where it was made never fails this way.
    pub fn stash_branch(&self, name: &str, index: usize, oid: &Oid) -> Result<()> {
        let stash = self.stash_ref(index, oid)?;
        self.git().run(
            &self
                .git()
                .command(["stash", "branch", name, &stash])
                .cwd(self.root()),
        )?;
        Ok(())
    }

    pub fn stash_drop(&self, index: usize, oid: &Oid) -> Result<()> {
        let name = self.stash_ref(index, oid)?;
        self.git().run(
            &self
                .git()
                .command(["stash", "drop", "-q", &name])
                .cwd(self.root()),
        )?;
        Ok(())
    }

    /// The tracked and untracked files a stash holds.
    pub fn stash_contents(&self, oid: &Oid) -> Result<StashContents> {
        let tracked = self.git().run(
            &self
                .git()
                .command([
                    "diff",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    &format!("{oid}^1"),
                    oid.as_str(),
                ])
                .cwd(self.root()),
        )?;
        // The untracked files live in a third parent, when there is one.
        let untracked = self.git().run_unchecked(
            &self
                .git()
                .command(["ls-tree", "-r", "-z", "--name-only", &format!("{oid}^3")])
                .cwd(self.root()),
        )?;
        Ok(StashContents {
            tracked: nul_paths(&tracked.stdout),
            untracked: if untracked.status.success() {
                nul_paths(&untracked.stdout)
            } else {
                Vec::new()
            },
        })
    }
}

/// The diff of a stash's tracked changes.
#[must_use]
pub fn stash_diff(oid: &Oid) -> DiffTarget {
    DiffTarget::Range {
        from: Oid::new(format!("{oid}^1")),
        to: oid.clone(),
    }
}
