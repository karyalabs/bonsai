//! Patches for the diff pane: work tree, index, commits and ranges.

use std::{ffi::OsString, path::PathBuf};

use crate::{Error, Oid, Repository, Result};

/// What to compare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffTarget {
    /// Work tree against the index, for tracked paths (all when empty).
    Unstaged(Vec<PathBuf>),
    /// Index against HEAD (or the empty tree before the first commit).
    Staged(Vec<PathBuf>),
    /// An untracked file against nothing.
    Untracked(PathBuf),
    /// A commit against its first parent (or the empty tree for a root).
    Commit(Oid),
    /// Everything between two commits, as `git diff from to`.
    Range { from: Oid, to: Oid },
}

/// How whitespace changes are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Whitespace {
    #[default]
    Show,
    /// `-b`: ignore changes in the amount of whitespace.
    IgnoreChanges,
    /// `-w`: ignore all whitespace.
    IgnoreAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiffOptions {
    pub whitespace: Whitespace,
    /// Lines of context around each change.
    pub context: u32,
    /// Patches larger than this are not returned; `None` loads any size.
    pub max_bytes: Option<usize>,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            whitespace: Whitespace::Show,
            context: 3,
            max_bytes: Some(4 * 1024 * 1024),
        }
    }
}

/// A loaded diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffOutput {
    /// A unified patch, possibly empty when nothing differs.
    Patch(String),
    /// The patch exceeds `max_bytes`; load again with `max_bytes: None`.
    TooLarge { bytes: usize },
}

impl Repository {
    /// Builds the patch for `target`.
    pub fn diff(&self, target: &DiffTarget, options: DiffOptions) -> Result<DiffOutput> {
        let mut args: Vec<OsString> = Vec::new();
        let push = |args: &mut Vec<OsString>, values: &[&str]| {
            args.extend(values.iter().map(OsString::from));
        };
        let common = |args: &mut Vec<OsString>| {
            push(
                args,
                &[
                    "--no-color",
                    "--no-ext-diff",
                    "--patch",
                    "--find-renames",
                    "--binary",
                ],
            );
            args.push(format!("--unified={}", options.context).into());
            match options.whitespace {
                Whitespace::Show => {}
                Whitespace::IgnoreChanges => push(args, &["--ignore-space-change"]),
                Whitespace::IgnoreAll => push(args, &["--ignore-all-space"]),
            }
        };
        let paths = |args: &mut Vec<OsString>, paths: &[PathBuf]| {
            args.push("--".into());
            args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
        };
        // `--no-index` exits 1 when files differ; other targets exit 0.
        let mut differ_exit_ok = false;
        match target {
            DiffTarget::Unstaged(list) => {
                push(&mut args, &["diff"]);
                common(&mut args);
                paths(&mut args, list);
            }
            DiffTarget::Staged(list) => {
                push(&mut args, &["diff", "--cached"]);
                common(&mut args);
                paths(&mut args, list);
            }
            DiffTarget::Untracked(path) => {
                push(&mut args, &["diff", "--no-index"]);
                common(&mut args);
                args.push("--".into());
                args.push("/dev/null".into());
                args.push(path.as_os_str().to_owned());
                differ_exit_ok = true;
            }
            DiffTarget::Commit(oid) => {
                push(
                    &mut args,
                    &["show", "--format=", "--diff-merges=first-parent"],
                );
                common(&mut args);
                args.push(oid.as_str().into());
            }
            DiffTarget::Range { from, to } => {
                push(&mut args, &["diff"]);
                common(&mut args);
                args.push(from.as_str().into());
                args.push(to.as_str().into());
            }
        }
        let output = self
            .git()
            .run_unchecked(&self.git().command(args).cwd(self.root()))?;
        let ok = output.status.success() || (differ_exit_ok && output.status.code() == Some(1));
        if !ok {
            return Err(Error::Failed {
                record: Box::new(output.record),
            });
        }
        if let Some(limit) = options.max_bytes
            && output.stdout.len() > limit
        {
            return Ok(DiffOutput::TooLarge {
                bytes: output.stdout.len(),
            });
        }
        let patch = String::from_utf8_lossy(&output.stdout).into_owned();
        Ok(DiffOutput::Patch(patch))
    }
}
