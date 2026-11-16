//! Purging paths from all history (ADR 4, CONTEXT.md: Purge, Safety backup).
//!
//! A purge scans history for the paths, writes a safety backup bundle,
//! rewrites every local branch and tag through `git fast-export`, a Rust
//! stream filter and `git fast-import`, then expires reflogs, runs `git gc`
//! and verifies nothing reachable still has the paths. It never pushes. The
//! files stay on disk; they become untracked.

mod pattern;
mod stream;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    ffi::OsString,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub use pattern::PurgeTarget;
use pattern::bytes_to_os;

/// A path from Git's raw bytes.
pub(crate) fn bytes_to_path(bytes: &[u8]) -> PathBuf {
    PathBuf::from(bytes_to_os(bytes.to_vec()))
}

use crate::{Error, InProgress, Oid, Repository, Result};

/// Why a purge cannot start now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurgeBlocker {
    /// Tracked files have uncommitted changes.
    UncommittedChanges,
    /// A merge, rebase or similar is in progress.
    OperationInProgress(InProgress),
    /// The repository has other worktrees, which might be mid-operation and
    /// whose index Bonsai would not update.
    OtherWorktrees(usize),
    /// There is no history to purge.
    NoCommits,
}

impl PurgeBlocker {
    /// A sentence for the purge sheet.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UncommittedChanges => {
                "Commit or stash your changes first. Purging rewrites every branch, so the work tree must be clean."
                    .to_owned()
            }
            Self::OperationInProgress(operation) => {
                let name = match operation {
                    InProgress::Merge => "merge",
                    InProgress::Rebase => "rebase",
                    InProgress::ApplyMailbox => "patch series",
                    InProgress::CherryPick => "cherry-pick",
                    InProgress::Revert => "revert",
                    InProgress::Bisect => "bisect",
                };
                format!("Finish or abort the {name} in progress first.")
            }
            Self::OtherWorktrees(count) => format!(
                "This repository has {count} other worktree{}. Remove them before purging.",
                if *count == 1 { "" } else { "s" }
            ),
            Self::NoCommits => "This repository has no commits yet.".to_owned(),
        }
    }
}

/// What a purge would touch, for the confirmation sheet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgeScan {
    /// Commits that add, change or delete a matching path, newest first.
    pub commits: Vec<Oid>,
    /// Every matching path that ever existed.
    pub paths: Vec<PathBuf>,
    /// Branches, tags and remote-tracking refs whose history has the paths.
    pub refs: Vec<String>,
    /// Stash entries (0 for `stash@{0}`) built on that history. The purge
    /// drops them, since rewriting a stash is not supported.
    pub stashes: Vec<usize>,
    pub blockers: Vec<PurgeBlocker>,
}

impl PurgeScan {
    /// Whether the paths are anywhere in history.
    #[must_use]
    pub fn found(&self) -> bool {
        !self.commits.is_empty()
    }
}

/// The phase a running purge is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurgeStep {
    Backup,
    Rewrite,
    Cleanup,
    Verify,
}

/// A ref a purge or restore moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefChange {
    pub name: String,
    pub old: Option<Oid>,
    pub new: Option<Oid>,
}

/// The outcome of a purge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurgeReport {
    /// The safety backup bundle.
    pub backup: PathBuf,
    /// Commits that now have a new ID.
    pub commits_rewritten: usize,
    /// Commits that only touched the paths and were removed.
    pub commits_removed: usize,
    pub refs: Vec<RefChange>,
    pub stashes_dropped: usize,
    /// The paths that were removed.
    pub paths: Vec<PathBuf>,
    /// File versions deleted from the object database.
    pub blobs_removed: usize,
    /// File versions kept because another path has the same contents.
    pub blobs_shared: usize,
}

const STASH_BACKUP: &str = "refs/bonsai/purge-stash/";

fn pathspecs(targets: &[PurgeTarget]) -> Vec<OsString> {
    targets.iter().flat_map(PurgeTarget::pathspecs).collect()
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

impl Repository {
    fn git_out<I, S>(&self, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Ok(self
            .git()
            .run(&self.git().command(args).cwd(self.root()))?
            .stdout)
    }

    /// Every path any commit ever added, sorted, for the purge picker.
    pub fn history_paths(&self) -> Result<Vec<PathBuf>> {
        let output = self.git_out([
            "log",
            "--exclude=refs/stash",
            "--all",
            "--no-merges",
            "--no-renames",
            "--diff-filter=A",
            "--name-only",
            "-z",
            "--format=",
        ])?;
        let paths: BTreeSet<&[u8]> = output
            .split(|&b| b == 0)
            .map(|p| p.strip_prefix(b"\n").unwrap_or(p))
            .filter(|p| !p.is_empty())
            .collect();
        Ok(paths
            .into_iter()
            .map(|p| PathBuf::from(bytes_to_os(p.to_vec())))
            .collect())
    }

    fn purge_blockers(&self) -> Result<Vec<PurgeBlocker>> {
        let mut blockers = Vec::new();
        if self.work_dir().is_some() {
            let status = self.git_out(["status", "--porcelain", "-z", "--untracked-files=no"])?;
            if !status.is_empty() {
                blockers.push(PurgeBlocker::UncommittedChanges);
            }
        }
        if let Some(operation) = self.in_progress() {
            blockers.push(PurgeBlocker::OperationInProgress(operation));
        }
        let worktrees = self.git_out(["worktree", "list", "--porcelain"])?;
        let others = String::from_utf8_lossy(&worktrees)
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count()
            .saturating_sub(1);
        if others > 0 {
            blockers.push(PurgeBlocker::OtherWorktrees(others));
        }
        if self.refs()?.is_empty() {
            blockers.push(PurgeBlocker::NoCommits);
        }
        Ok(blockers)
    }

    /// Every ref except stashes and Bonsai's own, with its target.
    fn refs(&self) -> Result<BTreeMap<String, Oid>> {
        let output = self.git_out(["for-each-ref", "--format=%(objectname) %(refname)"])?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(_, name)| *name != "refs/stash" && !name.starts_with("refs/bonsai/"))
            .map(|(oid, name)| (name.to_owned(), Oid::new(oid)))
            .collect())
    }

    fn stash_oids(&self) -> Result<Vec<Oid>> {
        let output = self.git().run_unchecked(
            &self
                .git()
                .command(["stash", "list", "--format=%H"])
                .cwd(self.root()),
        )?;
        Ok(output
            .stdout_lossy()
            .lines()
            .filter(|l| !l.is_empty())
            .map(Oid::new)
            .collect())
    }

    /// Finds where the targets appear in history and what would block a purge.
    pub fn purge_scan(&self, targets: &[PurgeTarget]) -> Result<PurgeScan> {
        let blockers = self.purge_blockers()?;
        if targets.is_empty() || blockers.contains(&PurgeBlocker::NoCommits) {
            return Ok(PurgeScan {
                blockers,
                ..PurgeScan::default()
            });
        }
        let specs = pathspecs(targets);
        let walk = |format: &str, extra: &[&str]| -> Result<Vec<u8>> {
            let mut args: Vec<OsString> = [
                "log",
                "--exclude=refs/stash",
                "--exclude=refs/bonsai/*",
                "--all",
                "--full-history",
                "--no-merges",
                "--no-renames",
                format,
            ]
            .iter()
            .chain(extra)
            .map(OsString::from)
            .collect();
            args.push("--".into());
            args.extend(specs.iter().cloned());
            self.git_out(args)
        };

        let commits: Vec<Oid> = String::from_utf8_lossy(&walk("--format=%H", &[])?)
            .lines()
            .filter(|l| !l.is_empty())
            .map(Oid::new)
            .collect();

        let names = walk("--format=", &["--name-only", "-z"])?;
        let paths: BTreeSet<&[u8]> = names
            .split(|&b| b == 0)
            .map(|p| p.strip_prefix(b"\n").unwrap_or(p))
            .filter(|p| !p.is_empty() && targets.iter().any(|t| t.matches(p)))
            .collect();
        let paths = paths
            .into_iter()
            .map(|p| PathBuf::from(bytes_to_os(p.to_vec())))
            .collect();

        let mut refs = BTreeSet::new();
        for chunk in commits.chunks(200) {
            let mut args: Vec<OsString> = vec!["for-each-ref".into(), "--format=%(refname)".into()];
            for oid in chunk {
                args.push("--contains".into());
                args.push(oid.as_str().into());
            }
            let output = self.git_out(args)?;
            refs.extend(
                String::from_utf8_lossy(&output)
                    .lines()
                    .filter(|r| *r != "refs/stash" && !r.starts_with("refs/bonsai/"))
                    .map(str::to_owned),
            );
        }

        let mut stashes = Vec::new();
        if !commits.is_empty() {
            for (index, oid) in self.stash_oids()?.iter().enumerate() {
                let mut args: Vec<OsString> = [
                    "log",
                    "--full-history",
                    "--no-merges",
                    "-1",
                    "--format=%H",
                    oid.as_str(),
                    "--",
                ]
                .iter()
                .map(OsString::from)
                .collect();
                args.extend(specs.iter().cloned());
                if !self.git_out(args)?.is_empty() {
                    stashes.push(index);
                }
            }
        }

        Ok(PurgeScan {
            commits,
            paths,
            refs: refs.into_iter().collect(),
            stashes,
            blockers,
        })
    }

    /// Writes a bundle of every ref, plus the given stash entries, to
    /// `backup_dir`.
    fn write_backup(&self, backup_dir: &Path, stashes: &[(usize, Oid)]) -> Result<PathBuf> {
        std::fs::create_dir_all(backup_dir).map_err(io_error(backup_dir))?;
        let name: String = self
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
            .trim_end_matches(".git")
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || "-_.".contains(c) {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let name = if name.is_empty() {
            "repository".to_owned()
        } else {
            name
        };
        let stamp = unix_time();
        let mut path = backup_dir.join(format!("{name}-{stamp}.bundle"));
        let mut n = 2;
        while path.exists() {
            path = backup_dir.join(format!("{name}-{stamp}-{n}.bundle"));
            n += 1;
        }

        // Stash entries live in a reflog, which bundles skip; give them refs
        // for the length of the bundle.
        for (index, oid) in stashes {
            self.git_out([
                "update-ref",
                &format!("{STASH_BACKUP}{index}"),
                oid.as_str(),
            ])?;
        }
        let bundled = self.git_out([
            OsString::from("bundle"),
            "create".into(),
            "--quiet".into(),
            path.as_os_str().to_owned(),
            "--all".into(),
        ]);
        let cleaned = self.delete_stash_backup_refs();
        bundled?;
        cleaned?;
        Ok(path)
    }

    fn delete_stash_backup_refs(&self) -> Result<()> {
        let output = self.git_out(["for-each-ref", "--format=%(refname)", STASH_BACKUP])?;
        for name in String::from_utf8_lossy(&output).lines() {
            self.git_out(["update-ref", "-d", name])?;
        }
        Ok(())
    }

    /// Removes the targets from every commit reachable from a local branch,
    /// tag or remote-tracking ref, after writing a safety backup to
    /// `backup_dir`. Stash entries built on the old history are dropped
    /// (they are in the backup). `progress` hears each phase as it starts.
    pub fn purge(
        &self,
        targets: &[PurgeTarget],
        backup_dir: &Path,
        progress: &dyn Fn(PurgeStep),
    ) -> Result<PurgeReport> {
        let scan = self.purge_scan(targets)?;
        if let Some(blocker) = scan.blockers.first() {
            return Err(Error::Purge {
                message: blocker.message(),
                backup: None,
            });
        }
        if !scan.found() {
            return Err(Error::Purge {
                message: "Nothing in this repository's history matches.".to_owned(),
                backup: None,
            });
        }

        progress(PurgeStep::Backup);
        let stash_oids = self.stash_oids()?;
        let dropped: Vec<(usize, Oid)> = scan
            .stashes
            .iter()
            .filter_map(|&i| stash_oids.get(i).map(|oid| (i, oid.clone())))
            .collect();
        let backup = self.write_backup(backup_dir, &dropped)?;
        let failed = |error: Error| Error::Purge {
            message: error.to_string(),
            backup: Some(backup.clone()),
        };

        progress(PurgeStep::Rewrite);
        let refs_before = self.refs().map_err(failed)?;
        let detached_head = self.detached_head().map_err(failed)?;
        let export = self
            .git_out([
                "-c",
                "core.quotePath=true",
                "fast-export",
                "--no-data",
                "--signed-tags=strip",
                "--tag-of-filtered-object=rewrite",
                "--reencode=yes",
                "--show-original-ids",
                "--use-done-feature",
                "--exclude=refs/stash",
                "--exclude=refs/bonsai/*",
                "--all",
            ])
            .map_err(failed)?;
        let filtered = stream::filter(&export, targets).map_err(|message| Error::Purge {
            message: format!("Bonsai could not read Git's history export: {message}"),
            backup: Some(backup.clone()),
        })?;
        drop(export);
        let marks = self.git_dir().join("bonsai-purge-marks");
        let mut import: Vec<OsString> =
            vec!["fast-import".into(), "--force".into(), "--quiet".into()];
        let mut flag = OsString::from("--export-marks=");
        flag.push(&marks);
        import.push(flag);
        self.git()
            .run(
                &self
                    .git()
                    .command(import)
                    .cwd(self.root())
                    .stdin(filtered.output.clone()),
            )
            .map_err(failed)?;
        let mark_oids = read_marks(&marks);
        let _ = std::fs::remove_file(&marks);

        progress(PurgeStep::Cleanup);
        let old_to_new: HashMap<&str, Option<String>> = filtered
            .originals
            .iter()
            .map(|(old, rep)| {
                let new = rep.as_ref().and_then(|r| {
                    if r.starts_with(':') {
                        mark_oids.get(r).cloned()
                    } else {
                        Some(r.clone())
                    }
                });
                (old.as_str(), new)
            })
            .collect();
        self.clean_up(
            &filtered.emptied_refs,
            detached_head.as_ref(),
            &old_to_new,
            &dropped,
        )
        .map_err(failed)?;

        progress(PurgeStep::Verify);
        let (blobs_removed, blobs_shared) = self
            .verify_purge(targets, &filtered.blobs)
            .map_err(failed)?;

        let refs_after = self.refs().map_err(failed)?;
        let refs = ref_changes(&refs_before, &refs_after);
        let commits_rewritten = old_to_new
            .iter()
            .filter(|(old, new)| new.as_deref().is_some_and(|new| new != **old))
            .count();
        Ok(PurgeReport {
            backup,
            commits_rewritten,
            commits_removed: filtered.pruned,
            refs,
            stashes_dropped: dropped.len(),
            paths: filtered
                .paths
                .into_iter()
                .map(|p| PathBuf::from(bytes_to_os(p)))
                .collect(),
            blobs_removed,
            blobs_shared,
        })
    }

    fn detached_head(&self) -> Result<Option<Oid>> {
        let symbolic = self.git().run_unchecked(
            &self
                .git()
                .command(["symbolic-ref", "-q", "HEAD"])
                .cwd(self.root()),
        )?;
        if symbolic.status.success() {
            return Ok(None);
        }
        let oid = self.git_out(["rev-parse", "--verify", "HEAD"])?;
        Ok(Some(Oid::new(String::from_utf8_lossy(&oid).trim())))
    }

    fn clean_up(
        &self,
        emptied_refs: &[String],
        detached_head: Option<&Oid>,
        old_to_new: &HashMap<&str, Option<String>>,
        dropped: &[(usize, Oid)],
    ) -> Result<()> {
        for name in emptied_refs {
            self.git_out(["update-ref", "-d", name])?;
        }
        if let Some(Some(new)) = detached_head.and_then(|oid| old_to_new.get(oid.as_str())) {
            self.git_out(["update-ref", "--no-deref", "HEAD", new])?;
        }
        if self.work_dir().is_some() {
            if self.head_is_unborn()? {
                self.git_out(["read-tree", "--empty"])?;
            } else {
                // Mixed reset: the index follows the new HEAD and purged files
                // stay on disk, now untracked.
                self.git_out(["reset", "-q"])?;
            }
        }
        // Newest last, so earlier indices stay valid while dropping.
        for (index, _) in dropped.iter().rev() {
            self.git_out(["stash", "drop", "-q", &format!("stash@{{{index}}}")])?;
        }
        // ORIG_HEAD and FETCH_HEAD can still name old commits.
        let _ = self.git().run_unchecked(
            &self
                .git()
                .command(["update-ref", "-d", "ORIG_HEAD"])
                .cwd(self.root()),
        );
        let _ = std::fs::remove_file(self.git_dir().join("FETCH_HEAD"));

        // Expiring reflogs also empties the stash, whose entries live in its
        // reflog, so the remaining stashes are stored again afterwards.
        let kept = self.git().run_unchecked(
            &self
                .git()
                .command(["stash", "list", "--format=%H%x1f%gs"])
                .cwd(self.root()),
        )?;
        let kept: Vec<(String, String)> = kept
            .stdout_lossy()
            .lines()
            .filter_map(|l| l.split_once('\x1f'))
            .map(|(oid, message)| (oid.to_owned(), message.to_owned()))
            .collect();
        self.git_out([
            "reflog",
            "expire",
            "--expire=now",
            "--expire-unreachable=now",
            "--all",
        ])?;
        if !kept.is_empty() {
            self.git_out(["update-ref", "-d", "refs/stash"])?;
            for (oid, message) in kept.iter().rev() {
                self.git_out(["stash", "store", "-m", message, oid])?;
            }
        }
        self.git_out(["gc", "--prune=now", "--quiet"])?;
        Ok(())
    }

    /// Fails if any reachable commit or object still has a target path.
    /// Returns how many of the dropped blobs are gone and how many remain
    /// because another path shares their contents.
    fn verify_purge(
        &self,
        targets: &[PurgeTarget],
        blobs: &BTreeSet<String>,
    ) -> Result<(usize, usize)> {
        let still = |message: String| Error::Purge {
            message,
            backup: None,
        };
        let mut args: Vec<OsString> = [
            "log",
            "--all",
            "--reflog",
            "--full-history",
            "--no-merges",
            "--format=%H",
            "--",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        args.extend(pathspecs(targets));
        let commits = self.git_out(args)?;
        if !commits.is_empty() {
            return Err(still(format!(
                "The paths are still in history after the rewrite, for example in commit {}.",
                String::from_utf8_lossy(&commits[..commits.len().min(12)])
            )));
        }
        let objects = self.git_out(["rev-list", "--all", "--reflog", "--objects"])?;
        for line in objects.split(|&b| b == b'\n') {
            if let Some(space) = line.iter().position(|&b| b == b' ') {
                let path = &line[space + 1..];
                if targets.iter().any(|t| t.matches(path)) {
                    return Err(still(format!(
                        "{} is still reachable after the rewrite.",
                        String::from_utf8_lossy(path)
                    )));
                }
            }
        }
        if blobs.is_empty() {
            return Ok((0, 0));
        }
        let mut input = String::new();
        for blob in blobs {
            input.push_str(blob);
            input.push('\n');
        }
        let output = self.git().run(
            &self
                .git()
                .command(["cat-file", "--batch-check"])
                .cwd(self.root())
                .stdin(input),
        )?;
        let missing = output
            .stdout_lossy()
            .lines()
            .filter(|l| l.ends_with(" missing"))
            .count();
        Ok((missing, blobs.len() - missing))
    }

    /// Puts back every branch, tag and remote-tracking ref saved in a purge's
    /// backup bundle, and the stash entries the purge dropped. Refs created
    /// since are left alone.
    pub fn restore_backup(&self, bundle: &Path) -> Result<Vec<RefChange>> {
        let before = self.refs()?;
        self.git_out([
            OsString::from("fetch"),
            "--quiet".into(),
            "--force".into(),
            "--no-prune".into(),
            "--no-write-fetch-head".into(),
            "--update-head-ok".into(),
            bundle.as_os_str().to_owned(),
            "refs/*:refs/*".into(),
            "^refs/stash".into(),
        ])?;
        let saved = self.git_out([
            "for-each-ref",
            "--format=%(refname) %(objectname) %(subject)",
            STASH_BACKUP,
        ])?;
        let mut stashes: Vec<(usize, String, String)> = String::from_utf8_lossy(&saved)
            .lines()
            .filter_map(|line| {
                let (name, rest) = line.split_once(' ')?;
                let (oid, subject) = rest.split_once(' ').unwrap_or((rest, ""));
                let index = name.strip_prefix(STASH_BACKUP)?.parse().ok()?;
                Some((index, oid.to_owned(), subject.to_owned()))
            })
            .collect();
        // Highest index first, so the lowest ends up on top.
        stashes.sort_by_key(|(index, _, _)| std::cmp::Reverse(*index));
        for (_, oid, subject) in &stashes {
            self.git_out(["stash", "store", "-m", subject, oid])?;
        }
        self.delete_stash_backup_refs()?;
        if self.work_dir().is_some() && !self.head_is_unborn()? {
            self.git_out(["reset", "-q"])?;
        }
        Ok(ref_changes(&before, &self.refs()?))
    }
}

fn read_marks(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(mark, oid)| (mark.to_owned(), oid.to_owned()))
        .collect()
}

fn ref_changes(before: &BTreeMap<String, Oid>, after: &BTreeMap<String, Oid>) -> Vec<RefChange> {
    let names: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    names
        .into_iter()
        .filter_map(|name| {
            let old = before.get(name);
            let new = after.get(name);
            (old != new).then(|| RefChange {
                name: name.clone(),
                old: old.cloned(),
                new: new.cloned(),
            })
        })
        .collect()
}
