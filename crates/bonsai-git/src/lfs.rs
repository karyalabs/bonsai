//! Git LFS: large files stored outside the repository, with a small pointer
//! committed in their place (CONTEXT.md: LFS).

use std::path::Path;

use crate::{Error, Repository, Result};

/// The pointer Git LFS commits instead of a file's contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LfsPointer {
    /// The SHA-256 of the contents, in hex.
    pub oid: String,
    pub size: u64,
}

impl LfsPointer {
    /// The first characters of the hash, for display.
    #[must_use]
    pub fn short(&self) -> &str {
        &self.oid[..self.oid.len().min(10)]
    }
}

/// Reads a pointer file's text, or `None` when it isn't one.
#[must_use]
pub fn parse_lfs_pointer(text: &str) -> Option<LfsPointer> {
    let mut lines = text.lines();
    if !lines
        .next()?
        .starts_with("version https://git-lfs.github.com/spec/")
    {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in lines {
        if let Some(hash) = line.strip_prefix("oid sha256:") {
            oid = Some(hash.trim().to_owned());
        } else if let Some(bytes) = line.strip_prefix("size ") {
            size = bytes.trim().parse().ok();
        }
    }
    Some(LfsPointer {
        oid: oid?,
        size: size?,
    })
}

/// The pointers on each side of a one-file patch, when either side is an
/// LFS pointer. `None` for an ordinary file.
#[must_use]
pub fn parse_lfs_patch(patch: &str) -> Option<(Option<LfsPointer>, Option<LfsPointer>)> {
    let mut old = String::new();
    let mut new = String::new();
    let mut in_hunk = false;
    for line in patch.lines() {
        if line.starts_with("@@") {
            in_hunk = true;
            continue;
        }
        if !in_hunk || line.starts_with('\\') {
            continue;
        }
        let (side, text) = line.split_at(line.len().min(1));
        match side {
            " " => {
                old.push_str(text);
                old.push('\n');
                new.push_str(text);
                new.push('\n');
            }
            "-" => {
                old.push_str(text);
                old.push('\n');
            }
            "+" => {
                new.push_str(text);
                new.push('\n');
            }
            _ => in_hunk = false,
        }
    }
    let before = parse_lfs_pointer(&old);
    let after = parse_lfs_pointer(&new);
    (before.is_some() || after.is_some()).then_some((before, after))
}

/// The patterns `.gitattributes` stores with Git LFS, such as `*.psd`.
#[must_use]
pub fn lfs_patterns(gitattributes: &str) -> Vec<String> {
    gitattributes
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pattern = words.next()?;
            words
                .any(|attribute| attribute == "filter=lfs")
                .then(|| pattern.to_owned())
        })
        .collect()
}

/// The pattern that tracks every file with `path`'s extension, such as
/// `*.psd`, or the path itself when it has none.
#[must_use]
pub fn lfs_pattern_for(path: &Path, all_of_type: bool) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(extension) if all_of_type => format!("*.{extension}"),
        _ => path.to_string_lossy().replace(' ', "[[:space:]]"),
    }
}

impl Repository {
    fn lfs_git<I, S>(&self, args: I) -> Result<crate::Output>
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        self.git().run(&self.git().command(args).cwd(self.root()))
    }

    /// The installed Git LFS version, such as `3.4.1`, or `None` when it
    /// isn't installed.
    #[must_use]
    pub fn lfs_version(&self) -> Option<String> {
        let output = self
            .git()
            .run_unchecked(&self.git().command(["lfs", "version"]).cwd(self.root()))
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = output.stdout_lossy();
        let version = text.split_whitespace().next()?.strip_prefix("git-lfs/")?;
        Some(version.to_owned())
    }

    /// The patterns the root `.gitattributes` stores with Git LFS.
    pub fn lfs_tracked(&self) -> Result<Vec<String>> {
        let path = self.root().join(".gitattributes");
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(lfs_patterns(&text)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(source) => Err(Error::Io { path, source }),
        }
    }

    fn require_lfs(&self) -> Result<()> {
        if self.lfs_version().is_none() {
            return Err(Error::Refused(
                "Git LFS isn't installed. Install it with `brew install git-lfs`, then try again."
                    .into(),
            ));
        }
        Ok(())
    }

    /// Stores files matching `pattern` with Git LFS from now on: sets up
    /// LFS in this repository if needed and adds the pattern to
    /// `.gitattributes`, staged and ready to commit. Files already
    /// committed stay as they are in history until they change.
    pub fn lfs_track(&self, pattern: &str) -> Result<()> {
        self.require_lfs()?;
        self.lfs_git(["lfs", "install", "--local"])?;
        self.lfs_git(["lfs", "track", "--", pattern])?;
        self.lfs_git(["add", "--", ".gitattributes"])?;
        Ok(())
    }

    /// Stops storing files matching `pattern` with Git LFS, staging the
    /// change to `.gitattributes`.
    pub fn lfs_untrack(&self, pattern: &str) -> Result<()> {
        self.require_lfs()?;
        self.lfs_git(["lfs", "untrack", "--", pattern])?;
        self.lfs_git(["add", "--", ".gitattributes"])?;
        Ok(())
    }

    /// Downloads the LFS files the checked-out commit needs.
    pub fn lfs_pull(&self) -> Result<()> {
        self.require_lfs()?;
        self.lfs_git(["lfs", "pull"])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const POINTER: &str = "version https://git-lfs.github.com/spec/v1\noid sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\nsize 3\n";

    #[test]
    fn reads_pointers_and_ignores_other_text() {
        let pointer = parse_lfs_pointer(POINTER).unwrap();
        assert_eq!(pointer.size, 3);
        assert_eq!(pointer.short(), "ba7816bf8f");
        assert_eq!(parse_lfs_pointer("hello\n"), None);
    }

    #[test]
    fn reads_both_sides_of_a_pointer_patch() {
        let patch = "diff --git a/a.psd b/a.psd\nindex 80d7d50..9a95d3b 100644\n--- a/a.psd\n+++ b/a.psd\n@@ -1,3 +1,3 @@\n version https://git-lfs.github.com/spec/v1\n-oid sha256:aaaa\n-size 3\n+oid sha256:bbbb\n+size 6\n";
        let (before, after) = parse_lfs_patch(patch).unwrap();
        assert_eq!(before.unwrap().size, 3);
        let after = after.unwrap();
        assert_eq!(after.oid, "bbbb");
        assert_eq!(after.size, 6);
        let added = "--- /dev/null\n+++ b/a.psd\n@@ -0,0 +1,3 @@\n+version https://git-lfs.github.com/spec/v1\n+oid sha256:cccc\n+size 9\n";
        let (before, after) = parse_lfs_patch(added).unwrap();
        assert_eq!(before, None);
        assert_eq!(after.unwrap().size, 9);
        assert_eq!(parse_lfs_patch("@@ -1 +1 @@\n-old\n+new\n"), None);
    }

    #[test]
    fn finds_lfs_patterns_and_suggests_them() {
        let attributes = "# art\n*.psd filter=lfs diff=lfs merge=lfs -text\n*.txt text\nassets/big.bin filter=lfs diff=lfs merge=lfs -text\n";
        assert_eq!(lfs_patterns(attributes), ["*.psd", "assets/big.bin"]);
        assert_eq!(lfs_pattern_for(Path::new("art/cover.psd"), true), "*.psd");
        assert_eq!(
            lfs_pattern_for(Path::new("art/my cover.psd"), false),
            "art/my[[:space:]]cover.psd"
        );
    }
}
