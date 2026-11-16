//! Linked worktrees: more than one branch checked out at once, each in its
//! own folder.

use std::path::{Path, PathBuf};

use crate::{Oid, Repository, Result};

/// One working tree of the repository, as `git worktree list` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    /// `None` for a bare repository's entry.
    pub head: Option<Oid>,
    /// The checked-out branch's short name, or `None` when detached or bare.
    pub branch: Option<String>,
    pub bare: bool,
    /// The repository's own folder, as opposed to a linked worktree.
    pub main: bool,
    /// Why the worktree is locked, when it is (the reason may be empty).
    pub locked: Option<String>,
    /// Its folder is gone; `git worktree prune` would remove the entry.
    pub prunable: bool,
}

/// What a new worktree checks out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewWorktree {
    /// An existing local branch that is not checked out elsewhere.
    Branch(String),
    /// A new branch at `start` (a revision; `None` is HEAD).
    NewBranch { name: String, start: Option<String> },
    /// A revision, detached.
    Detached(String),
}

/// Parses `git worktree list --porcelain`.
#[must_use]
pub fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut list = Vec::new();
    for block in text.split("\n\n") {
        let mut worktree: Option<Worktree> = None;
        for line in block.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            if key == "worktree" {
                worktree = Some(Worktree {
                    path: PathBuf::from(value),
                    head: None,
                    branch: None,
                    bare: false,
                    main: list.is_empty(),
                    locked: None,
                    prunable: false,
                });
                continue;
            }
            let Some(worktree) = worktree.as_mut() else {
                continue;
            };
            match key {
                "HEAD" => worktree.head = Some(Oid::new(value)),
                "branch" => {
                    worktree.branch = Some(
                        value
                            .strip_prefix("refs/heads/")
                            .unwrap_or(value)
                            .to_owned(),
                    );
                }
                "bare" => worktree.bare = true,
                "locked" => worktree.locked = Some(value.to_owned()),
                "prunable" => worktree.prunable = true,
                _ => {}
            }
        }
        list.extend(worktree);
    }
    list
}

impl Repository {
    /// Every worktree, the main one first.
    ///
    /// # Errors
    /// When Git fails.
    pub fn worktrees(&self) -> Result<Vec<Worktree>> {
        let git = self.git();
        let output = git.run(
            &git.command(["worktree", "list", "--porcelain"])
                .cwd(self.root()),
        )?;
        Ok(parse_worktrees(&output.stdout_lossy()))
    }

    /// Creates a worktree at `path` (which must not exist or be empty).
    ///
    /// # Errors
    /// When Git refuses, for example because the branch is checked out
    /// elsewhere.
    pub fn add_worktree(&self, path: &Path, what: &NewWorktree) -> Result<()> {
        let mut args: Vec<String> = vec!["worktree".into(), "add".into()];
        match what {
            NewWorktree::Branch(name) => {
                args.push(path.to_string_lossy().into_owned());
                args.push(name.clone());
            }
            NewWorktree::NewBranch { name, start } => {
                args.push("-b".into());
                args.push(name.clone());
                args.push(path.to_string_lossy().into_owned());
                args.extend(start.clone());
            }
            NewWorktree::Detached(revision) => {
                args.push("--detach".into());
                args.push(path.to_string_lossy().into_owned());
                args.push(revision.clone());
            }
        }
        let git = self.git();
        git.run(&git.command(args).cwd(self.root()))?;
        Ok(())
    }

    /// Removes the linked worktree at `path` and its folder. Without
    /// `force`, Git refuses while it has changes or untracked files.
    ///
    /// # Errors
    /// When Git refuses.
    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        let mut args: Vec<String> = vec!["worktree".into(), "remove".into()];
        if force {
            args.push("--force".into());
        }
        args.push(path.to_string_lossy().into_owned());
        let git = self.git();
        git.run(&git.command(args).cwd(self.root()))?;
        Ok(())
    }

    /// Forgets worktrees whose folders were deleted.
    ///
    /// # Errors
    /// When Git fails.
    pub fn prune_worktrees(&self) -> Result<()> {
        let git = self.git();
        git.run(&git.command(["worktree", "prune"]).cwd(self.root()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_porcelain_list() {
        let text = "worktree /src/app\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\nworktree /src/app-login\nHEAD 2222222222222222222222222222222222222222\nbranch refs/heads/feature/login\nlocked on a USB drive\n\nworktree /tmp/gone\nHEAD 3333333333333333333333333333333333333333\ndetached\nprunable gitdir file points to non-existent location\n\n";
        let list = parse_worktrees(text);
        assert_eq!(list.len(), 3);
        assert!(list[0].main && !list[1].main);
        assert_eq!(list[0].branch.as_deref(), Some("main"));
        assert_eq!(list[1].branch.as_deref(), Some("feature/login"));
        assert_eq!(list[1].locked.as_deref(), Some("on a USB drive"));
        assert_eq!(list[2].branch, None);
        assert!(list[2].prunable);
        assert_eq!(list[2].path, PathBuf::from("/tmp/gone"));
    }
}
