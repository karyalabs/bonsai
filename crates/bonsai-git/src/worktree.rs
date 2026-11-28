//! Staging, unstaging, discarding and ignoring work tree changes.

use std::{
    ffi::OsString,
    io::Write as _,
    path::{Path, PathBuf},
};

use crate::{Error, Repository, Result};

/// How to ignore a path (CONTEXT.md: Ignore).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IgnoreRule {
    /// Exactly this file or folder, anchored at the work tree root.
    Path(PathBuf),
    /// Every file with this extension (without the dot), anywhere.
    Extension(String),
    /// Everything inside this folder, anchored at the root.
    Folder(PathBuf),
}

impl IgnoreRule {
    /// The `.gitignore` line for this rule, with special characters escaped.
    #[must_use]
    pub fn pattern(&self) -> String {
        match self {
            Self::Path(path) => format!("/{}", escape(&slashed(path))),
            Self::Extension(extension) => format!("*.{}", escape(extension)),
            Self::Folder(path) => format!("/{}/", escape(slashed(path).trim_end_matches('/'))),
        }
    }
}

fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Escapes the characters `.gitignore` treats specially. Every pattern
/// starts with `/` or `*.`, so a leading `#` or `!` never needs escaping.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    // Trailing spaces are dropped by Git unless escaped.
    if out.ends_with(' ') {
        out.pop();
        out.push_str("\\ ");
    }
    out
}

/// NUL-separated paths for `--pathspec-from-file=- --pathspec-file-nul`.
fn pathspec(paths: &[PathBuf]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for path in paths {
        bytes.extend_from_slice(path.as_os_str().as_encoded_bytes());
        bytes.push(0);
    }
    bytes
}

const PATHSPEC_ARGS: [&str; 2] = ["--pathspec-from-file=-", "--pathspec-file-nul"];

impl Repository {
    pub(crate) fn work_tree(&self) -> Result<&Path> {
        self.work_dir()
            .ok_or_else(|| Error::NotARepository(self.git_dir().to_path_buf()))
    }

    fn run_with_paths(&self, args: &[&str], paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let root = self.work_tree()?;
        let mut all: Vec<OsString> = args.iter().map(OsString::from).collect();
        all.extend(PATHSPEC_ARGS.iter().map(OsString::from));
        self.git()
            .run(&self.git().command(all).cwd(root).stdin(pathspec(paths)))?;
        Ok(())
    }

    /// Stages these paths (relative to the work tree), including deletions
    /// and untracked files.
    pub fn stage(&self, paths: &[PathBuf]) -> Result<()> {
        self.run_with_paths(&["add", "--all"], paths)
    }

    /// Stages every change in the work tree.
    pub fn stage_all(&self) -> Result<()> {
        let root = self.work_tree()?;
        self.git()
            .run(&self.git().command(["add", "--all"]).cwd(root))?;
        Ok(())
    }

    /// Stages every change to a tracked file, leaving untracked files out
    /// (`git add --update`).
    pub fn stage_tracked(&self) -> Result<()> {
        let root = self.work_tree()?;
        self.git()
            .run(&self.git().command(["add", "--update"]).cwd(root))?;
        Ok(())
    }

    /// Unstages these paths, keeping their work tree contents.
    pub fn unstage(&self, paths: &[PathBuf]) -> Result<()> {
        if self.head_is_unborn()? {
            // Nothing to restore from: remove the entries from the index.
            self.run_with_paths(&["rm", "--cached", "-r", "-q", "--ignore-unmatch"], paths)
        } else {
            self.run_with_paths(&["restore", "--staged"], paths)
        }
    }

    /// Stops tracking these paths and ignores each one, keeping the files
    /// on disk. The removal is staged, ready to commit.
    pub fn stop_tracking(&self, paths: &[PathBuf]) -> Result<()> {
        self.run_with_paths(&["rm", "--cached", "-r", "-q", "--ignore-unmatch"], paths)?;
        for path in paths {
            self.ignore(&IgnoreRule::Path(path.clone()))?;
        }
        Ok(())
    }

    /// Unstages everything.
    pub fn unstage_all(&self) -> Result<()> {
        let root = self.work_tree()?;
        let args: &[&str] = if self.head_is_unborn()? {
            &["rm", "--cached", "-r", "-q", "--ignore-unmatch", "--", "."]
        } else {
            &["restore", "--staged", "--", "."]
        };
        self.git().run(&self.git().command(args).cwd(root))?;
        Ok(())
    }

    /// Throws away work tree changes to these tracked paths, restoring the
    /// staged version (or the committed one when nothing is staged).
    pub fn discard(&self, paths: &[PathBuf]) -> Result<()> {
        self.run_with_paths(&["restore", "--worktree"], paths)
    }

    /// Puts these paths in the work tree back to how `revision` has them,
    /// leaving the index alone, so Changes shows the difference as
    /// unstaged edits.
    pub fn restore_from(&self, revision: &str, paths: &[PathBuf]) -> Result<()> {
        let source = format!("--source={revision}");
        self.run_with_paths(&["restore", source.as_str(), "--worktree"], paths)
    }

    /// Deletes these untracked files from disk.
    pub fn delete_untracked(&self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let root = self.work_tree()?;
        // `clean` has no --pathspec-from-file; batches keep argv short.
        for chunk in paths.chunks(256) {
            let mut args: Vec<OsString> = ["clean", "-f", "-q", "--"]
                .iter()
                .map(OsString::from)
                .collect();
            args.extend(chunk.iter().map(|p| p.as_os_str().to_owned()));
            self.git().run(&self.git().command(args).cwd(root))?;
        }
        Ok(())
    }

    /// Appends `rule` to the root `.gitignore`, creating it if needed.
    /// A rule already present is not added twice.
    pub fn ignore(&self, rule: &IgnoreRule) -> Result<()> {
        let path = self.work_tree()?.join(".gitignore");
        let pattern = rule.pattern();
        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(Error::Io { path, source }),
        };
        if existing.lines().any(|line| line.trim_end() == pattern) {
            return Ok(());
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        let separator = if existing.is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        writeln!(file, "{separator}{pattern}").map_err(|source| Error::Io { path, source })?;
        Ok(())
    }

    /// Whether HEAD names a branch with no commits yet.
    pub fn head_is_unborn(&self) -> Result<bool> {
        let output = self.git().run_unchecked(
            &self
                .git()
                .command(["rev-parse", "--verify", "--quiet", "HEAD"])
                .cwd(self.root()),
        )?;
        Ok(!output.status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_patterns_are_anchored_and_escaped() {
        assert_eq!(
            IgnoreRule::Path("config/.env".into()).pattern(),
            "/config/.env"
        );
        assert_eq!(IgnoreRule::Extension("log".into()).pattern(), "*.log");
        assert_eq!(IgnoreRule::Folder("target/".into()).pattern(), "/target/");
        assert_eq!(
            IgnoreRule::Path("a[1]*.txt".into()).pattern(),
            "/a\\[1]\\*.txt"
        );
        assert_eq!(IgnoreRule::Extension("!x".into()).pattern(), "*.!x");
        assert_eq!(
            IgnoreRule::Path("trailing ".into()).pattern(),
            "/trailing\\ "
        );
    }
}
