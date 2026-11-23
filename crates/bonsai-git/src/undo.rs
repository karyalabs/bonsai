//! Undo and redo for Bonsai's own operations (CONTEXT.md: Undo).
//!
//! Bonsai reads a small [`RefState`] before and after each operation, and
//! [`plan_undo`] turns the difference into [`UndoStep`]s that put it back:
//! a commit becomes a soft reset, a switch a switch back, a deleted branch a
//! branch again. Discarded files are hashed into the object database first,
//! so a discard can be undone too. Every step checks that what it changes is
//! still where the operation left it, so an undo never overwrites later work.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

use crate::{Error, Oid, Repository, ResetMode, Result};

/// How an undo resets HEAD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoReset {
    /// Keeps the index: a commit's changes come back staged.
    Soft,
    /// Resets the index but not the work tree.
    Mixed,
    /// Updates the files that differ, keeping uncommitted changes
    /// (`--keep`), and refuses when they would be overwritten.
    Keep,
}

impl UndoReset {
    fn flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Keep => "--keep",
        }
    }
}

/// Where HEAD points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeadRef {
    /// On a branch, by short name; `None` while it has no commits.
    Branch(String, Option<Oid>),
    Detached(Oid),
}

impl HeadRef {
    fn oid(&self) -> Option<&Oid> {
        match self {
            Self::Branch(_, oid) => oid.as_ref(),
            Self::Detached(oid) => Some(oid),
        }
    }

    fn branch(&self) -> Option<&str> {
        match self {
            Self::Branch(name, _) => Some(name),
            Self::Detached(_) => None,
        }
    }

    /// The same place: the same branch, or both detached.
    fn same_place(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Branch(a, _), Self::Branch(b, _)) => a == b,
            (Self::Detached(_), Self::Detached(_)) => true,
            _ => false,
        }
    }
}

/// The refs an operation can change, read cheaply before and after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefState {
    pub head: HeadRef,
    /// Branches and tags, by full name such as `refs/heads/main`.
    pub refs: BTreeMap<String, Oid>,
    /// Stashes, newest first, with their messages.
    pub stashes: Vec<(Oid, String)>,
    /// The newest HEAD reflog message, such as `commit: Fix the parser`.
    pub reflog: Option<String>,
    /// A merge, rebase or other multi-step operation is in progress.
    pub busy: bool,
}

/// A file's contents saved before a discard, or its absence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileBackup {
    /// Relative to the work tree.
    pub path: PathBuf,
    /// The blob holding its bytes; `None` when the file didn't exist.
    pub blob: Option<Oid>,
    pub executable: bool,
}

/// One change an undo makes. Each names what it expects to find, and
/// refuses to run when the repository has moved on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoStep {
    /// Resets HEAD from `expect` to `target`.
    Reset {
        expect: Oid,
        target: Oid,
        mode: UndoReset,
    },
    /// Switches from one branch, or detached commit, to another.
    Switch { expect: HeadRef, target: HeadRef },
    /// Points a branch or tag at `target`, creating or deleting it.
    SetRef {
        name: String,
        expect: Option<Oid>,
        target: Option<Oid>,
    },
    /// Renames a branch, keeping its upstream and settings.
    Rename { expect: String, target: String },
    /// Puts a dropped stash back on the list.
    RestoreStash { oid: Oid, message: String },
    /// Drops a stash restored by an undo.
    DropStash { oid: Oid, message: String },
    /// Pops the newest stash with its staged state, undoing a stash save.
    PopStash { oid: Oid },
    /// Writes files back as they were before a discard.
    RestoreFiles { files: Vec<FileBackup> },
}

/// The steps that undo one operation, named after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Undo {
    /// The operation, such as "Commit" or "Delete branch topic".
    pub name: String,
    pub steps: Vec<UndoStep>,
}

fn is_commit(reflog: Option<&str>) -> bool {
    reflog.is_some_and(|message| {
        message.starts_with("commit: ") || message.starts_with("commit (amend): ")
    })
}

/// The steps that take the repository from `after` back to `before`, in
/// order. Empty when there is nothing Bonsai can undo, such as a fetch, or
/// when undoing would lose work, such as a stash pop. `reset` names the
/// mode when the operation was a reset, so undoing it restores the index
/// and files the same way.
#[must_use]
pub fn plan_undo(
    before: &RefState,
    after: &RefState,
    worktree_changed: bool,
    reset: Option<ResetMode>,
) -> Vec<UndoStep> {
    if before.busy || after.busy {
        return Vec::new();
    }
    let mut early = Vec::new();
    let mut head = Vec::new();
    let mut late = Vec::new();
    let mut handled: Vec<String> = Vec::new();

    let removed: Vec<(&String, &Oid)> = before
        .refs
        .iter()
        .filter(|(name, _)| name.starts_with("refs/heads/") && !after.refs.contains_key(*name))
        .collect();
    let added: Vec<(&String, &Oid)> = after
        .refs
        .iter()
        .filter(|(name, _)| name.starts_with("refs/heads/") && !before.refs.contains_key(*name))
        .collect();
    let mut renamed = None;
    if let ([(old, old_oid)], [(new, new_oid)]) = (removed.as_slice(), added.as_slice())
        && old_oid == new_oid
    {
        let old = old.trim_start_matches("refs/heads/").to_owned();
        let new = new.trim_start_matches("refs/heads/").to_owned();
        handled.push(format!("refs/heads/{old}"));
        handled.push(format!("refs/heads/{new}"));
        renamed = Some((old.clone(), new.clone()));
        late.push(UndoStep::Rename {
            expect: new,
            target: old,
        });
    }

    let head_renamed = renamed.as_ref().is_some_and(|(old, new)| {
        before.head.branch() == Some(old) && after.head.branch() == Some(new)
    });
    if head_renamed {
        // Renaming the current branch moves HEAD with it.
    } else if before.head.same_place(&after.head) {
        match (before.head.oid(), after.head.oid()) {
            (Some(from), Some(to)) if from != to => {
                if let Some(branch) = after.head.branch() {
                    handled.push(format!("refs/heads/{branch}"));
                }
                let mode = match reset {
                    Some(ResetMode::Soft) => UndoReset::Soft,
                    Some(ResetMode::Mixed) => UndoReset::Mixed,
                    None if is_commit(after.reflog.as_deref()) => UndoReset::Soft,
                    Some(ResetMode::Hard) | None => UndoReset::Keep,
                };
                head.push(UndoStep::Reset {
                    expect: to.clone(),
                    target: from.clone(),
                    mode,
                });
            }
            (None, Some(_)) => return Vec::new(),
            _ => {}
        }
    } else {
        if before.head.oid().is_none() || after.head.oid().is_none() {
            return Vec::new();
        }
        // The branch left behind must be back where it was before switching
        // to it; the one switched to can only change once it's left.
        if let Some(branch) = before.head.branch() {
            let name = format!("refs/heads/{branch}");
            if before.refs.get(&name) != after.refs.get(&name) {
                early.push(set_ref(&name, before, after));
                handled.push(name);
            }
        }
        head.push(UndoStep::Switch {
            expect: after.head.clone(),
            target: before.head.clone(),
        });
    }

    let names: std::collections::BTreeSet<&String> =
        before.refs.keys().chain(after.refs.keys()).collect();
    for name in names {
        if handled.contains(name) || before.refs.get(name) == after.refs.get(name) {
            continue;
        }
        let step = set_ref(name, before, after);
        if after
            .head
            .branch()
            .is_some_and(|b| *name == format!("refs/heads/{b}"))
        {
            late.push(step);
        } else {
            early.push(step);
        }
    }

    let new_stashes: Vec<&(Oid, String)> = after
        .stashes
        .iter()
        .filter(|(oid, _)| !before.stashes.iter().any(|(b, _)| b == oid))
        .collect();
    let gone_stashes: Vec<&(Oid, String)> = before
        .stashes
        .iter()
        .filter(|(oid, _)| !after.stashes.iter().any(|(a, _)| a == oid))
        .collect();
    if !gone_stashes.is_empty() {
        if worktree_changed {
            // A pop: putting the stash back would leave its changes applied too.
            return Vec::new();
        }
        for (oid, message) in gone_stashes.iter().rev() {
            late.push(UndoStep::RestoreStash {
                oid: oid.clone(),
                message: message.clone(),
            });
        }
    }
    match new_stashes.as_slice() {
        [] => {}
        [(oid, _)] if worktree_changed && after.stashes.first().map(|s| &s.0) == Some(oid) => {
            late.push(UndoStep::PopStash { oid: oid.clone() });
        }
        _ => {
            for (oid, message) in new_stashes {
                late.push(UndoStep::DropStash {
                    oid: oid.clone(),
                    message: message.clone(),
                });
            }
        }
    }

    early.extend(head);
    early.extend(late);
    early
}

fn set_ref(name: &str, before: &RefState, after: &RefState) -> UndoStep {
    UndoStep::SetRef {
        name: name.to_owned(),
        expect: after.refs.get(name).cloned(),
        target: before.refs.get(name).cloned(),
    }
}

impl UndoStep {
    /// The step that reverses this one, for redo; `None` when it can't be
    /// reversed exactly. Restored files are backed up when they run.
    #[must_use]
    pub fn inverse(&self) -> Option<Self> {
        Some(match self {
            Self::Reset {
                expect,
                target,
                mode,
            } => Self::Reset {
                expect: target.clone(),
                target: expect.clone(),
                mode: *mode,
            },
            Self::Switch { expect, target } => Self::Switch {
                expect: target.clone(),
                target: expect.clone(),
            },
            Self::SetRef {
                name,
                expect,
                target,
            } => Self::SetRef {
                name: name.clone(),
                expect: target.clone(),
                target: expect.clone(),
            },
            Self::Rename { expect, target } => Self::Rename {
                expect: target.clone(),
                target: expect.clone(),
            },
            Self::RestoreStash { oid, message } => Self::DropStash {
                oid: oid.clone(),
                message: message.clone(),
            },
            Self::DropStash { oid, message } => Self::RestoreStash {
                oid: oid.clone(),
                message: message.clone(),
            },
            Self::PopStash { .. } | Self::RestoreFiles { .. } => return None,
        })
    }
}

impl Repository {
    fn undo_git<I, S>(&self, args: I) -> Result<crate::Output>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.git().run(&self.git().command(args).cwd(self.root()))
    }

    fn undo_git_unchecked<I, S>(&self, args: I) -> Result<crate::Output>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.git()
            .run_unchecked(&self.git().command(args).cwd(self.root()))
    }

    /// Reads HEAD, branches, tags and stashes.
    pub fn ref_state(&self) -> Result<RefState> {
        let refs = self
            .undo_git([
                "for-each-ref",
                "--format=%(objectname) %(refname)",
                "refs/heads",
                "refs/tags",
            ])?
            .stdout_lossy()
            .lines()
            .filter_map(|line| {
                let (oid, name) = line.split_once(' ')?;
                Some((name.to_owned(), Oid::new(oid)))
            })
            .collect();
        let symbolic = self.undo_git_unchecked(["symbolic-ref", "-q", "HEAD"])?;
        let oid = self.undo_git_unchecked(["rev-parse", "-q", "--verify", "HEAD"])?;
        let oid = oid
            .status
            .success()
            .then(|| Oid::new(oid.stdout_lossy().trim()));
        let head = if symbolic.status.success() {
            let name = symbolic.stdout_lossy();
            let name = name.trim();
            HeadRef::Branch(
                name.strip_prefix("refs/heads/").unwrap_or(name).to_owned(),
                oid,
            )
        } else {
            HeadRef::Detached(oid.ok_or_else(|| Error::Refused("HEAD is unreadable.".into()))?)
        };
        let stashes = self
            .undo_git_unchecked(["stash", "list", "--format=%H%x00%gs"])?
            .stdout_lossy()
            .lines()
            .filter_map(|line| {
                let (oid, message) = line.split_once('\0')?;
                Some((Oid::new(oid), message.to_owned()))
            })
            .collect();
        let reflog = self.undo_git_unchecked(["reflog", "-1", "--format=%gs"])?;
        let reflog = reflog
            .status
            .success()
            .then(|| reflog.stdout_lossy().trim().to_owned())
            .filter(|message| !message.is_empty());
        Ok(RefState {
            head,
            refs,
            stashes,
            reflog,
            busy: self.in_progress().is_some(),
        })
    }

    /// Saves the work tree contents of `paths` (files, or folders of them)
    /// as blobs, so [`UndoStep::RestoreFiles`] can write them back. A path
    /// that doesn't exist is recorded as absent.
    pub fn back_up_files(&self, paths: &[PathBuf]) -> Result<Vec<FileBackup>> {
        let root = self.root().to_path_buf();
        let mut files = Vec::new();
        for path in paths {
            collect_files(&root, path, &mut files);
        }
        files.sort();
        files.dedup();
        let existing: Vec<&PathBuf> = files
            .iter()
            .filter(|path| {
                root.join(path)
                    .symlink_metadata()
                    .is_ok_and(|m| m.is_file())
            })
            .collect();
        let mut blobs = Vec::new();
        if !existing.is_empty() {
            let mut stdin = Vec::new();
            for path in &existing {
                stdin.extend_from_slice(path.to_string_lossy().as_bytes());
                stdin.push(b'\n');
            }
            let output = self.git().run(
                &self
                    .git()
                    .command(["hash-object", "-w", "--no-filters", "--stdin-paths"])
                    .cwd(&root)
                    .stdin(stdin),
            )?;
            blobs = output
                .stdout_lossy()
                .lines()
                .map(|line| Oid::new(line.trim()))
                .collect();
        }
        let mut blobs = blobs.into_iter();
        Ok(files
            .into_iter()
            .map(|path| {
                let full = root.join(&path);
                let metadata = full
                    .symlink_metadata()
                    .ok()
                    .filter(std::fs::Metadata::is_file);
                let blob = metadata.as_ref().and_then(|_| blobs.next());
                FileBackup {
                    executable: metadata.as_ref().is_some_and(is_executable),
                    blob,
                    path,
                }
            })
            .collect())
    }

    /// Runs `undo`'s steps, after checking that each finds the repository
    /// as the operation left it. Returns the undo that reverses it, for
    /// redo, when every step can be reversed.
    pub fn apply_undo(&self, undo: &Undo) -> Result<Option<Undo>> {
        let state = self.ref_state()?;
        if state.busy {
            return Err(Error::Refused(
                "Finish the operation in progress before undoing.".into(),
            ));
        }
        self.check_undo(&undo.steps, &state)?;
        let mut redo = Some(Vec::new());
        for step in &undo.steps {
            let inverse = self.apply_step(step)?;
            match (&mut redo, inverse) {
                (Some(steps), Some(inverse)) => steps.push(inverse),
                _ => redo = None,
            }
        }
        Ok(redo.map(|mut steps| {
            steps.reverse();
            Undo {
                name: undo.name.clone(),
                steps,
            }
        }))
    }

    /// Checks the steps that read state up front, so nothing runs when the
    /// repository has moved on.
    fn check_undo(&self, steps: &[UndoStep], state: &RefState) -> Result<()> {
        let moved = |what: &str| {
            Err(Error::Refused(format!(
                "Bonsai can't undo this because {what} has changed since."
            )))
        };
        for step in steps {
            match step {
                UndoStep::Reset { expect, .. } if state.head.oid() != Some(expect) => {
                    return moved("HEAD");
                }
                UndoStep::Switch { expect, .. } if &state.head != expect => {
                    return moved("the checked-out branch");
                }
                UndoStep::SetRef { name, expect, .. }
                    if state.refs.get(name) != expect.as_ref() =>
                {
                    return moved(short_ref(name));
                }
                UndoStep::Rename { expect, target }
                    if !state.refs.contains_key(&format!("refs/heads/{expect}"))
                        || state.refs.contains_key(&format!("refs/heads/{target}")) =>
                {
                    return moved(&format!("branch {expect}"));
                }
                UndoStep::PopStash { oid } if state.stashes.first().map(|s| &s.0) != Some(oid) => {
                    return moved("the stash list");
                }
                UndoStep::DropStash { oid, .. }
                    if !state.stashes.iter().any(|(stash, _)| stash == oid) =>
                {
                    return moved("the stash list");
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn apply_step(&self, step: &UndoStep) -> Result<Option<UndoStep>> {
        match step {
            UndoStep::Reset { target, mode, .. } => {
                self.undo_git(["reset", "-q", mode.flag(), target.as_str()])?;
            }
            UndoStep::Switch { target, .. } => match target {
                HeadRef::Branch(name, _) => {
                    self.undo_git(["checkout", "-q", name.as_str(), "--"])?;
                }
                HeadRef::Detached(oid) => {
                    self.undo_git(["checkout", "-q", "--detach", oid.as_str(), "--"])?;
                }
            },
            UndoStep::SetRef {
                name,
                expect,
                target,
            } => {
                let zero = "0".repeat(
                    expect
                        .as_ref()
                        .or(target.as_ref())
                        .map_or(40, |oid| oid.as_str().len()),
                );
                let old = expect.as_ref().map_or(zero.as_str(), Oid::as_str);
                match target {
                    Some(target) => {
                        self.undo_git(["update-ref", name.as_str(), target.as_str(), old])?;
                    }
                    None => {
                        self.undo_git(["update-ref", "-d", name.as_str(), old])?;
                    }
                }
            }
            UndoStep::Rename { expect, target } => {
                self.undo_git(["branch", "-m", expect.as_str(), target.as_str()])?;
            }
            UndoStep::RestoreStash { oid, message } => {
                self.undo_git(["stash", "store", "-m", message.as_str(), oid.as_str()])?;
            }
            UndoStep::DropStash { oid, .. } => {
                let index = self
                    .ref_state()?
                    .stashes
                    .iter()
                    .position(|(stash, _)| stash == oid)
                    .ok_or_else(|| Error::Refused("The stash is gone.".into()))?;
                self.stash_drop(index, oid)?;
            }
            UndoStep::PopStash { oid } => {
                self.stash_ref(0, oid)?;
                self.undo_git(["stash", "pop", "-q", "--index", "stash@{0}"])?;
            }
            UndoStep::RestoreFiles { files } => {
                let paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
                let current = self.back_up_files(&paths)?;
                for file in files {
                    self.restore_file(file)?;
                }
                return Ok(Some(UndoStep::RestoreFiles { files: current }));
            }
        }
        Ok(step.inverse())
    }

    fn restore_file(&self, file: &FileBackup) -> Result<()> {
        let full = self.root().join(&file.path);
        let io = |source| Error::Io {
            path: full.clone(),
            source,
        };
        match &file.blob {
            Some(blob) => {
                let bytes = self.undo_git(["cat-file", "blob", blob.as_str()])?.stdout;
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).map_err(io)?;
                }
                std::fs::write(&full, bytes).map_err(io)?;
                set_executable(&full, file.executable).map_err(io)?;
            }
            None => match std::fs::remove_file(&full) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(io(error));
                }
                _ => {}
            },
        }
        Ok(())
    }
}

fn short_ref(name: &str) -> &str {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/tags/"))
        .unwrap_or(name)
}

fn collect_files(root: &Path, path: &Path, out: &mut Vec<PathBuf>) {
    let full = root.join(path);
    match full.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => {
            if path.file_name().is_some_and(|name| name == ".git") {
                return;
            }
            let Ok(entries) = std::fs::read_dir(&full) else {
                return;
            };
            for entry in entries.flatten() {
                collect_files(root, &path.join(entry.file_name()), out);
            }
        }
        Ok(metadata) if metadata.is_file() => out.push(path.to_path_buf()),
        // Symlinks aren't backed up; a missing path is recorded as absent.
        Ok(_) => {}
        Err(_) => out.push(path.to_path_buf()),
    }
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn set_executable(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    let mode = permissions.mode();
    let mode = if executable {
        mode | ((mode & 0o444) >> 2)
    } else {
        mode & !0o111
    };
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_executable(_: &Path, _: bool) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(c: char) -> Oid {
        Oid::new(c.to_string().repeat(40))
    }

    fn state(head: HeadRef, refs: &[(&str, char)]) -> RefState {
        RefState {
            head,
            refs: refs
                .iter()
                .map(|(name, c)| ((*name).to_owned(), oid(*c)))
                .collect(),
            stashes: Vec::new(),
            reflog: None,
            busy: false,
        }
    }

    fn on(branch: &str, c: char) -> HeadRef {
        HeadRef::Branch(branch.into(), Some(oid(c)))
    }

    #[test]
    fn a_commit_undoes_with_a_soft_reset() {
        let before = state(on("main", 'a'), &[("refs/heads/main", 'a')]);
        let mut after = state(on("main", 'b'), &[("refs/heads/main", 'b')]);
        after.reflog = Some("commit: Fix it".into());
        assert_eq!(
            plan_undo(&before, &after, true, None),
            [UndoStep::Reset {
                expect: oid('b'),
                target: oid('a'),
                mode: UndoReset::Soft
            }]
        );
        after.reflog = Some("pull: Fast-forward".into());
        assert_eq!(
            plan_undo(&before, &after, true, None),
            [UndoStep::Reset {
                expect: oid('b'),
                target: oid('a'),
                mode: UndoReset::Keep
            }]
        );
        assert_eq!(
            plan_undo(&before, &after, true, Some(ResetMode::Mixed)),
            [UndoStep::Reset {
                expect: oid('b'),
                target: oid('a'),
                mode: UndoReset::Mixed
            }]
        );
    }

    #[test]
    fn creating_and_switching_switches_back_then_deletes() {
        let before = state(on("main", 'a'), &[("refs/heads/main", 'a')]);
        let after = state(
            on("topic", 'a'),
            &[("refs/heads/main", 'a'), ("refs/heads/topic", 'a')],
        );
        assert_eq!(
            plan_undo(&before, &after, false, None),
            [
                UndoStep::Switch {
                    expect: on("topic", 'a'),
                    target: on("main", 'a'),
                },
                UndoStep::SetRef {
                    name: "refs/heads/topic".into(),
                    expect: Some(oid('a')),
                    target: None,
                },
            ]
        );
    }

    #[test]
    fn a_rename_renames_back() {
        let before = state(on("main", 'a'), &[("refs/heads/main", 'a')]);
        let after = state(on("trunk", 'a'), &[("refs/heads/trunk", 'a')]);
        assert_eq!(
            plan_undo(&before, &after, false, None),
            [UndoStep::Rename {
                expect: "trunk".into(),
                target: "main".into()
            }]
        );
    }

    #[test]
    fn deleted_tags_come_back_and_fetches_change_nothing() {
        let before = state(on("main", 'a'), &[("refs/tags/v1", 't')]);
        let after = state(on("main", 'a'), &[]);
        assert_eq!(
            plan_undo(&before, &after, false, None),
            [UndoStep::SetRef {
                name: "refs/tags/v1".into(),
                expect: None,
                target: Some(oid('t')),
            }]
        );
        assert_eq!(plan_undo(&before, &before, false, None), []);
    }

    #[test]
    fn stashes_drop_and_save_but_pops_and_busy_states_dont_undo() {
        let mut before = state(on("main", 'a'), &[]);
        before.stashes = vec![(oid('s'), "On main: wip".into())];
        let after = state(on("main", 'a'), &[]);
        assert_eq!(
            plan_undo(&before, &after, false, None),
            [UndoStep::RestoreStash {
                oid: oid('s'),
                message: "On main: wip".into()
            }]
        );
        assert_eq!(plan_undo(&before, &after, true, None), []);
        assert_eq!(
            plan_undo(&after, &before, true, None),
            [UndoStep::PopStash { oid: oid('s') }]
        );
        let mut busy = after.clone();
        busy.busy = true;
        assert_eq!(plan_undo(&before, &busy, false, None), []);
    }

    #[test]
    fn steps_reverse_for_redo() {
        let step = UndoStep::Switch {
            expect: on("topic", 'a'),
            target: on("main", 'a'),
        };
        assert_eq!(step.inverse().unwrap().inverse().unwrap(), step);
        assert_eq!(UndoStep::PopStash { oid: oid('s') }.inverse(), None);
    }
}
