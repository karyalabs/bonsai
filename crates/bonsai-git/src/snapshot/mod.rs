//! A point-in-time read of a repository (CONTEXT.md: Snapshot).

mod refs;
mod status;

use std::{fmt, path::PathBuf, thread};

use crate::{Error, Repository, Result};

/// A full object ID in hex (SHA-1 or SHA-256).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Oid(String);

impl Oid {
    #[must_use]
    pub fn new(hex: impl Into<String>) -> Self {
        Self(hex.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first seven characters, for display.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.0[..self.0.len().min(7)]
    }
}

impl fmt::Debug for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Oid({})", self.short())
    }
}

impl fmt::Display for Oid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything Bonsai shows about a repository at one moment. Views render a
/// snapshot; they never query Git directly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub head: Head,
    pub changes: Vec<Change>,
    pub branches: Vec<Branch>,
    pub remote_branches: Vec<RemoteBranch>,
    pub tags: Vec<Tag>,
    pub remotes: Vec<Remote>,
    pub stashes: Vec<Stash>,
    /// Every worktree, the main one first.
    pub worktrees: Vec<crate::Worktree>,
    /// Every submodule, by path; empty without `.gitmodules`.
    pub submodules: Vec<crate::Submodule>,
    pub operation: Option<InProgress>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Head {
    /// On a branch; `oid` is `None` while the branch is unborn (no commits).
    Branch {
        name: String,
        oid: Option<Oid>,
    },
    Detached {
        oid: Oid,
    },
}

/// One path that differs from HEAD (CONTEXT.md: Change).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    /// The path before a rename or copy.
    pub original_path: Option<PathBuf>,
    pub kind: ChangeKind,
    pub submodule: bool,
}

impl Change {
    /// Has changes in the index (and is not conflicted).
    #[must_use]
    pub fn is_staged(&self) -> bool {
        matches!(self.kind, ChangeKind::Tracked { index: Some(_), .. })
    }

    /// Has changes in the work tree that are not staged.
    #[must_use]
    pub fn is_unstaged(&self) -> bool {
        matches!(
            self.kind,
            ChangeKind::Tracked {
                work_tree: Some(_),
                ..
            }
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Tracked {
        index: Option<EntryStatus>,
        work_tree: Option<EntryStatus>,
    },
    Untracked,
    Ignored,
    Conflicted(ConflictKind),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryStatus {
    Modified,
    TypeChanged,
    Added,
    Deleted,
    Renamed,
    Copied,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    BothModified,
    BothAdded,
    BothDeleted,
    AddedByUs,
    AddedByThem,
    DeletedByUs,
    DeletedByThem,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    pub name: String,
    pub oid: Oid,
    pub is_head: bool,
    pub upstream: Option<Tracking>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tracking {
    /// Short name such as `origin/main`.
    pub name: String,
    /// The upstream branch no longer exists on the remote.
    pub gone: bool,
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteBranch {
    pub remote: String,
    pub name: String,
    pub oid: Oid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// The commit (or other object) the tag points at, peeled.
    pub target: Oid,
    pub annotated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub fetch_url: Option<String>,
    /// Only when a separate push URL is configured.
    pub push_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// 0 for `stash@{0}`.
    pub index: usize,
    pub oid: Oid,
    pub message: String,
}

/// A multi-step Git operation waiting to be continued or aborted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InProgress {
    Merge,
    Rebase,
    /// `git am` applying patches.
    ApplyMailbox,
    CherryPick,
    Revert,
    Bisect,
}

impl Repository {
    /// Reads a [`Snapshot`]. Status, refs, remotes and stashes are read in
    /// parallel.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let git = self.git();
        let root = self.root();
        let (status, refs, remotes, stashes, worktrees, submodules) = thread::scope(|scope| {
            let status = (!self.is_bare()).then(|| {
                scope.spawn(|| {
                    git.run(
                        &git.command([
                            "status",
                            "--porcelain=v2",
                            "-z",
                            "--branch",
                            "--untracked-files=all",
                        ])
                        .cwd(root),
                    )
                })
            });
            let refs =
                scope.spawn(|| git.run(&git.command(["for-each-ref", refs::FORMAT]).cwd(root)));
            let remotes = scope.spawn(|| {
                git.run_unchecked(
                    &git.command([
                        "config",
                        "-z",
                        "--get-regexp",
                        r"^remote\..*\.(url|pushurl)$",
                    ])
                    .cwd(root),
                )
            });
            let stashes = scope.spawn(|| {
                git.run_unchecked(
                    &git.command(["stash", "list", "-z", "--format=%H%x1f%gs"])
                        .cwd(root),
                )
            });
            let worktrees = scope.spawn(|| {
                git.run_unchecked(&git.command(["worktree", "list", "--porcelain"]).cwd(root))
            });
            let submodules = scope.spawn(|| self.submodules());
            (
                status.map(|h| h.join().expect("status thread")),
                refs.join().expect("refs thread"),
                remotes.join().expect("remotes thread"),
                stashes.join().expect("stash thread"),
                worktrees.join().expect("worktree thread"),
                submodules.join().expect("submodule thread"),
            )
        });

        let refs = refs::parse(&refs?.stdout_lossy());
        let head = match status.transpose()? {
            Some(output) => {
                let status = status::parse(&output.stdout)?;
                let head = match (status.head, status.oid) {
                    (Some(name), oid) => Head::Branch { name, oid },
                    (None, Some(oid)) => Head::Detached { oid },
                    (None, None) => return Err(Error::Parse("detached unborn HEAD".into())),
                };
                (head, status.changes)
            }
            None => (self.bare_head()?, Vec::new()),
        };

        Ok(Snapshot {
            head: head.0,
            changes: head.1,
            branches: refs.branches,
            remote_branches: refs.remote_branches,
            tags: refs.tags,
            // `git config` exits 1 when nothing matches.
            remotes: parse_remotes(&remotes?.stdout),
            stashes: parse_stashes(&stashes?.stdout_lossy()),
            worktrees: crate::parse_worktrees(&worktrees?.stdout_lossy()),
            // A submodule Git can't read shouldn't hide the repository.
            submodules: submodules.unwrap_or_default(),
            operation: self.in_progress(),
        })
    }

    fn bare_head(&self) -> Result<Head> {
        let git = self.git();
        let name = git
            .run_unchecked(
                &git.command(["symbolic-ref", "--short", "-q", "HEAD"])
                    .cwd(self.root()),
            )?
            .stdout_lossy();
        let oid = git.run_unchecked(
            &git.command(["rev-parse", "-q", "--verify", "HEAD"])
                .cwd(self.root()),
        )?;
        let oid = oid
            .status
            .success()
            .then(|| Oid::new(oid.stdout_lossy().trim()));
        let name = name.trim();
        Ok(match (name.is_empty(), oid) {
            (false, oid) => Head::Branch {
                name: name.to_owned(),
                oid,
            },
            (true, Some(oid)) => Head::Detached { oid },
            (true, None) => {
                return Err(Error::Parse("HEAD is neither a branch nor a commit".into()));
            }
        })
    }

    /// Detects an operation in progress from the state files Git leaves in
    /// the git dir.
    #[must_use]
    pub fn in_progress(&self) -> Option<InProgress> {
        let dir = self.git_dir();
        if dir.join("rebase-merge").is_dir() {
            Some(InProgress::Rebase)
        } else if dir.join("rebase-apply").is_dir() {
            Some(if dir.join("rebase-apply/applying").exists() {
                InProgress::ApplyMailbox
            } else {
                InProgress::Rebase
            })
        } else if dir.join("MERGE_HEAD").exists() {
            Some(InProgress::Merge)
        } else if dir.join("CHERRY_PICK_HEAD").exists() {
            Some(InProgress::CherryPick)
        } else if dir.join("REVERT_HEAD").exists() {
            Some(InProgress::Revert)
        } else if dir.join("BISECT_LOG").exists() {
            Some(InProgress::Bisect)
        } else {
            None
        }
    }
}

/// `git config -z --get-regexp` prints `key\nvalue\0` per entry.
fn parse_remotes(bytes: &[u8]) -> Vec<Remote> {
    let mut remotes: Vec<Remote> = Vec::new();
    for entry in bytes.split(|b| *b == 0).filter(|e| !e.is_empty()) {
        let entry = String::from_utf8_lossy(entry);
        let Some((key, value)) = entry.split_once('\n') else {
            continue;
        };
        let Some(rest) = key.strip_prefix("remote.") else {
            continue;
        };
        let Some((name, field)) = rest.rsplit_once('.') else {
            continue;
        };
        let index = remotes
            .iter()
            .position(|r| r.name == name)
            .unwrap_or_else(|| {
                remotes.push(Remote {
                    name: name.to_owned(),
                    fetch_url: None,
                    push_url: None,
                });
                remotes.len() - 1
            });
        let remote = &mut remotes[index];
        // The first URL is the one Git fetches from.
        match field {
            "url" if remote.fetch_url.is_none() => remote.fetch_url = Some(value.to_owned()),
            "pushurl" if remote.push_url.is_none() => remote.push_url = Some(value.to_owned()),
            _ => {}
        }
    }
    remotes
}

fn parse_stashes(output: &str) -> Vec<Stash> {
    output
        .split('\0')
        .filter(|e| !e.trim().is_empty())
        .enumerate()
        .filter_map(|(index, entry)| {
            let (oid, message) = entry.trim_start_matches('\n').split_once('\u{1f}')?;
            Some(Stash {
                index,
                oid: Oid::new(oid),
                message: message.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remotes_with_push_urls() {
        let input = b"remote.origin.url\nhttps://github.com/a/b.git\0\
remote.my.fork.url\ngit@github.com:me/b.git\0\
remote.origin.pushurl\nssh://push/b\0";
        let remotes = parse_remotes(input);
        assert_eq!(remotes.len(), 2);
        assert_eq!(remotes[0].name, "origin");
        assert_eq!(remotes[0].push_url.as_deref(), Some("ssh://push/b"));
        assert_eq!(remotes[1].name, "my.fork");
    }

    #[test]
    fn parses_stashes() {
        let stashes = parse_stashes("aa\u{1f}WIP on main: 1 one\0bb\u{1f}On main: tidy\0");
        assert_eq!(stashes.len(), 2);
        assert_eq!(stashes[1].index, 1);
        assert_eq!(stashes[1].message, "On main: tidy");
    }
}
