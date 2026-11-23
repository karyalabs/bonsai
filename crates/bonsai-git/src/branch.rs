//! Creating, renaming, deleting and switching branches, and managing tags.

use std::{ffi::OsString, path::PathBuf};

use crate::{Error, Repository, Result};

/// Whether `name` is a valid branch or tag name under Git's ref rules
/// (`git check-ref-format`), so dialogs can validate as the user types.
#[must_use]
pub fn is_valid_ref_name(name: &str) -> bool {
    if name.is_empty()
        || name == "@"
        || name.starts_with('-')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
    {
        return false;
    }
    if name
        .chars()
        .any(|c| c.is_ascii_control() || " ~^:?*[\\".contains(c))
    {
        return false;
    }
    name.split('/').all(|component| {
        // Git's `.lock` rule is case-sensitive.
        !component.is_empty()
            && !component.starts_with('.')
            && component
                .rsplit_once('.')
                .is_none_or(|(_, ext)| ext != "lock")
    })
}

/// What stopped a branch switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckoutBlocker {
    /// Uncommitted changes to these files would be overwritten.
    LocalChanges(Vec<PathBuf>),
    /// These untracked files would be overwritten.
    UntrackedFiles(Vec<PathBuf>),
}

impl CheckoutBlocker {
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        match self {
            Self::LocalChanges(paths) | Self::UntrackedFiles(paths) => paths,
        }
    }
}

fn failed_stderr(error: &Error) -> Option<&str> {
    match error {
        Error::Failed { record } => Some(&record.stderr),
        _ => None,
    }
}

impl Error {
    /// A branch delete refused because the branch is not merged.
    #[must_use]
    pub fn is_not_fully_merged(&self) -> bool {
        failed_stderr(self).is_some_and(|stderr| stderr.contains("is not fully merged"))
    }

    /// A switch refused because it would overwrite files.
    #[must_use]
    pub fn checkout_blocker(&self) -> Option<CheckoutBlocker> {
        let stderr = failed_stderr(self)?;
        let untracked = stderr.contains("untracked working tree files would be");
        if !untracked && !stderr.contains("Your local changes to the following files would be") {
            return None;
        }
        let paths: Vec<PathBuf> = stderr
            .lines()
            .filter_map(|line| line.strip_prefix('\t'))
            .map(|path| PathBuf::from(path.trim()))
            .collect();
        Some(if untracked {
            CheckoutBlocker::UntrackedFiles(paths)
        } else {
            CheckoutBlocker::LocalChanges(paths)
        })
    }
}

impl Repository {
    fn run_args<I, S>(&self, args: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(())
    }

    /// Creates `name` at `start` (any revision; HEAD when `None`), and
    /// switches to it when `checkout` is set.
    pub fn create_branch(&self, name: &str, start: Option<&str>, checkout: bool) -> Result<()> {
        let mut args = if checkout {
            vec!["switch", "--no-track", "-c", name]
        } else {
            vec!["branch", "--no-track", "--", name]
        };
        args.extend(start);
        self.run_args(args)
    }

    pub fn rename_branch(&self, old: &str, new: &str) -> Result<()> {
        self.run_args(["branch", "-m", "--", old, new])
    }

    /// Deletes a local branch. Without `force`, Git refuses a branch whose
    /// commits are not merged; check [`Error::is_not_fully_merged`].
    pub fn delete_branch(&self, name: &str, force: bool) -> Result<()> {
        self.run_args(["branch", if force { "-D" } else { "-d" }, "--", name])
    }

    /// Switches to a local branch. Check [`Error::checkout_blocker`] when
    /// local changes are in the way.
    pub fn checkout_branch(&self, name: &str) -> Result<()> {
        self.run_args(["switch", "--no-guess", name])
    }

    /// Creates `local` tracking `remote/branch` and switches to it.
    pub fn checkout_remote_branch(&self, remote: &str, branch: &str, local: &str) -> Result<()> {
        self.run_args([
            "switch",
            "-c",
            local,
            "--track",
            &format!("{remote}/{branch}"),
        ])
    }

    /// Detaches HEAD at a revision.
    pub fn checkout_detached(&self, revision: &str) -> Result<()> {
        self.run_args(["switch", "--detach", revision])
    }

    /// Sets (`Some("origin/main")`) or removes a branch's upstream.
    pub fn set_upstream(&self, branch: &str, upstream: Option<&str>) -> Result<()> {
        match upstream {
            Some(upstream) => {
                self.run_args(["branch", &format!("--set-upstream-to={upstream}"), branch])
            }
            None => self.run_args(["branch", "--unset-upstream", branch]),
        }
    }

    /// Creates a tag at `target` (HEAD when `None`). A message makes it an
    /// annotated tag.
    pub fn create_tag(
        &self,
        name: &str,
        target: Option<&str>,
        message: Option<&str>,
    ) -> Result<()> {
        let mut args = vec!["tag"];
        if message.is_some() {
            args.extend(["-a", "--cleanup=strip", "-F", "-"]);
        }
        args.extend(["--", name]);
        args.extend(target);
        let mut command = self.git().command(args).cwd(self.root());
        if let Some(message) = message {
            command = command.stdin(message.as_bytes().to_vec());
        }
        self.git().run(&command)?;
        Ok(())
    }

    pub fn delete_tag(&self, name: &str) -> Result<()> {
        self.run_args(["tag", "-d", "--", name])
    }

    /// Stashes every change, untracked files included, with a message.
    pub fn stash_all(&self, message: &str) -> Result<()> {
        self.run_args(["stash", "push", "--include-untracked", "-m", message])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_names_follow_git_rules() {
        for good in ["main", "feature/login", "v1.2.3", "fix-#12", "ünïcode"] {
            assert!(is_valid_ref_name(good), "{good}");
        }
        for bad in [
            "", "@", "-x", "a..b", "a b", "a~1", "a^", "a:b", "a?", "a*", "a[b", "a\\b", "a/",
            "/a", "a//b", ".hidden", "a/.b", "a.lock", "a.", "a@{1}", "tab\t",
        ] {
            assert!(!is_valid_ref_name(bad), "{bad:?}");
        }
    }
}
