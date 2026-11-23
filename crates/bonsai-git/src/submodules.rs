//! Submodules: other repositories pinned at a commit inside this one
//! (CONTEXT.md: Submodule).

use std::path::{Path, PathBuf};

use crate::{Oid, Repository, Result};

/// How a submodule's checkout compares with the commit this repository
/// records for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmoduleState {
    /// Not cloned yet: `git submodule update --init` fetches it.
    NotInitialized,
    /// Checked out at the recorded commit.
    UpToDate,
    /// Checked out at another commit than the one recorded.
    OtherCommit,
    /// The recorded commit is in conflict after a merge.
    Conflicted,
}

/// One submodule, from `git submodule status` and `.gitmodules`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submodule {
    /// Where it lives, relative to the repository root.
    pub path: PathBuf,
    /// Its name in `.gitmodules`, usually the path.
    pub name: String,
    pub url: Option<String>,
    /// The branch `.gitmodules` asks updates to follow, if any.
    pub branch: Option<String>,
    /// The commit checked out, or the one recorded when not initialized.
    pub oid: Oid,
    /// `git describe`'s name for the checked-out commit, such as a tag.
    pub describe: Option<String>,
    pub state: SubmoduleState,
}

/// A submodule's recorded commit changing, read from a diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmoduleMove {
    pub path: PathBuf,
    /// `None` when the submodule is new.
    pub from: Option<Oid>,
    /// `None` when the submodule is removed.
    pub to: Option<Oid>,
    /// Whether the submodule's own files have uncommitted changes.
    pub dirty: bool,
    /// Commits `to` has that `from` doesn't, newest first.
    pub added: Vec<SubmoduleCommit>,
    /// Commits `from` has that `to` doesn't: the move went back.
    pub removed: Vec<SubmoduleCommit>,
    /// Whether the commits could be read; not when it isn't cloned.
    pub readable: bool,
}

/// One commit inside a submodule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmoduleCommit {
    pub oid: Oid,
    pub summary: String,
    pub author: String,
}

/// The old and new commits from a submodule's patch: the
/// `-Subproject commit …` and `+Subproject commit …` lines, if that is all
/// the patch changes.
#[must_use]
pub fn parse_submodule_patch(patch: &str) -> Option<(Option<Oid>, Option<Oid>, bool)> {
    let mut from = None;
    let mut to = None;
    let mut dirty = false;
    let mut seen = false;
    for line in patch.lines() {
        let (sign, rest) = match line.as_bytes().first() {
            Some(b'-') if !line.starts_with("---") => ('-', &line[1..]),
            Some(b'+') if !line.starts_with("+++") => ('+', &line[1..]),
            _ => continue,
        };
        let commit = rest.strip_prefix("Subproject commit ")?;
        seen = true;
        let (oid, suffix) = commit.split_at(commit.find('-').unwrap_or(commit.len()));
        let oid = Some(Oid::new(oid.trim()));
        if sign == '-' {
            from = oid;
        } else {
            to = oid;
            dirty |= suffix == "-dirty";
        }
    }
    seen.then_some((from, to, dirty))
}

/// Parses `git submodule status` lines such as
/// `+1a2b… vendor/lib (v1.2-3-g1a2b)`.
#[must_use]
pub fn parse_submodule_status(text: &str) -> Vec<(PathBuf, Oid, Option<String>, SubmoduleState)> {
    text.lines()
        .filter_map(|line| {
            let mut chars = line.chars();
            let state = match chars.next()? {
                '-' => SubmoduleState::NotInitialized,
                '+' => SubmoduleState::OtherCommit,
                'U' => SubmoduleState::Conflicted,
                _ => SubmoduleState::UpToDate,
            };
            let rest = chars.as_str();
            let (oid, rest) = rest.split_once(' ')?;
            let (path, describe) = match rest.rsplit_once(" (") {
                Some((path, describe)) if describe.ends_with(')') => {
                    (path, Some(describe.trim_end_matches(')').to_owned()))
                }
                _ => (rest, None),
            };
            Some((PathBuf::from(path), Oid::new(oid), describe, state))
        })
        .collect()
}

/// Reads `git config -f .gitmodules --list` into name → (key, value).
fn parse_gitmodules(text: &str) -> Vec<(String, String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let rest = key.strip_prefix("submodule.")?;
            let (name, field) = rest.rsplit_once('.')?;
            Some((name.to_owned(), field.to_owned(), value.to_owned()))
        })
        .collect()
}

impl Repository {
    fn submodule_git<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<std::ffi::OsString>,
    {
        Ok(self
            .git()
            .run(&self.git().command(args).cwd(self.root()))?
            .stdout_lossy())
    }

    /// Every submodule, by path. Empty without a `.gitmodules` file.
    pub fn submodules(&self) -> Result<Vec<Submodule>> {
        if !self.root().join(".gitmodules").is_file() {
            return Ok(Vec::new());
        }
        let status = self.submodule_git(["submodule", "status"])?;
        let config = self
            .git()
            .run_unchecked(
                &self
                    .git()
                    .command(["config", "-f", ".gitmodules", "--list"])
                    .cwd(self.root()),
            )?
            .stdout_lossy();
        let config = parse_gitmodules(&config);
        let field = |name: &str, key: &str| {
            config
                .iter()
                .find(|(n, k, _)| n == name && k == key)
                .map(|(_, _, v)| v.clone())
        };
        Ok(parse_submodule_status(&status)
            .into_iter()
            .map(|(path, oid, describe, state)| {
                let text = path.to_string_lossy();
                let name = config
                    .iter()
                    .find(|(_, k, v)| k == "path" && *v == text)
                    .map_or_else(|| text.clone().into_owned(), |(n, _, _)| n.clone());
                Submodule {
                    url: field(&name, "url"),
                    branch: field(&name, "branch"),
                    name,
                    path,
                    oid,
                    describe,
                    state,
                }
            })
            .collect())
    }

    /// What moving the submodule at `path` from one commit to another
    /// brings in or takes out, read from its own repository.
    #[must_use]
    pub fn submodule_move(
        &self,
        path: &Path,
        from: Option<Oid>,
        to: Option<Oid>,
        dirty: bool,
    ) -> SubmoduleMove {
        let folder = self.root().join(path);
        let log = |range: String| -> Option<Vec<SubmoduleCommit>> {
            let output = self
                .git()
                .run(
                    &self
                        .git()
                        .command(["log", "--max-count=200", "--format=%H%x00%s%x00%an", &range])
                        .cwd(&folder),
                )
                .ok()?;
            Some(
                output
                    .stdout_lossy()
                    .lines()
                    .filter_map(|line| {
                        let mut fields = line.split('\0');
                        Some(SubmoduleCommit {
                            oid: Oid::new(fields.next()?),
                            summary: fields.next()?.to_owned(),
                            author: fields.next()?.to_owned(),
                        })
                    })
                    .collect(),
            )
        };
        let initialized = folder.join(".git").exists();
        let (added, removed) = match (&from, &to) {
            (Some(from), Some(to)) if initialized => {
                (log(format!("{from}..{to}")), log(format!("{to}..{from}")))
            }
            (None, Some(to)) if initialized => (log(to.to_string()), Some(Vec::new())),
            _ => (None, None),
        };
        let readable = added.is_some() && removed.is_some();
        SubmoduleMove {
            path: path.to_path_buf(),
            from,
            to,
            dirty,
            added: added.unwrap_or_default(),
            removed: removed.unwrap_or_default(),
            readable,
        }
    }

    /// Clones any missing submodules among `paths` (all when empty) and
    /// checks each out at its recorded commit, nested ones too.
    pub fn update_submodules(&self, paths: &[PathBuf]) -> Result<()> {
        let mut args: Vec<std::ffi::OsString> =
            ["submodule", "update", "--init", "--recursive", "--"]
                .into_iter()
                .map(Into::into)
                .collect();
        args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
        self.submodule_git(args)?;
        Ok(())
    }

    /// Fetches the newest commit of each submodule's branch (its remote's
    /// default unless `.gitmodules` names one) and checks it out. The new
    /// commits show up as changes to commit.
    pub fn update_submodules_remote(&self, paths: &[PathBuf]) -> Result<()> {
        let mut args: Vec<std::ffi::OsString> = ["submodule", "update", "--init", "--remote", "--"]
            .into_iter()
            .map(Into::into)
            .collect();
        args.extend(paths.iter().map(|p| p.as_os_str().to_owned()));
        self.submodule_git(args)?;
        Ok(())
    }

    /// Adds `url` as a submodule at `path`, staged and ready to commit.
    pub fn add_submodule(&self, url: &str, path: &Path) -> Result<()> {
        self.submodule_git([
            std::ffi::OsStr::new("submodule"),
            "add".as_ref(),
            "--".as_ref(),
            url.as_ref(),
            path.as_os_str(),
        ])?;
        Ok(())
    }

    /// Removes the submodule at `path`: its checkout, its `.gitmodules`
    /// entry and the recorded commit, staged and ready to commit.
    pub fn remove_submodule(&self, path: &Path) -> Result<()> {
        self.submodule_git([
            std::ffi::OsStr::new("submodule"),
            "deinit".as_ref(),
            "--force".as_ref(),
            "--".as_ref(),
            path.as_os_str(),
        ])?;
        self.submodule_git([
            std::ffi::OsStr::new("rm"),
            "-q".as_ref(),
            "--force".as_ref(),
            "--".as_ref(),
            path.as_os_str(),
        ])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_submodule_status() {
        let text = "-1111111111111111111111111111111111111111 vendor/a\n\
                    +2222222222222222222222222222222222222222 vendor/b c (v1.2-3-g2222222)\n \
                    3333333333333333333333333333333333333333 libs/c (heads/main)\n";
        let parsed = parse_submodule_status(text);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, PathBuf::from("vendor/a"));
        assert_eq!(parsed[0].3, SubmoduleState::NotInitialized);
        assert_eq!(parsed[1].0, PathBuf::from("vendor/b c"));
        assert_eq!(parsed[1].2.as_deref(), Some("v1.2-3-g2222222"));
        assert_eq!(parsed[1].3, SubmoduleState::OtherCommit);
        assert_eq!(parsed[2].3, SubmoduleState::UpToDate);
    }

    #[test]
    fn reads_a_submodule_patch() {
        let patch = "diff --git a/lib b/lib\nindex 1111111..2222222 160000\n--- a/lib\n+++ b/lib\n@@ -1 +1 @@\n-Subproject commit 1111111111111111111111111111111111111111\n+Subproject commit 2222222222222222222222222222222222222222-dirty\n";
        let (from, to, dirty) = parse_submodule_patch(patch).unwrap();
        assert_eq!(
            from.unwrap().as_str(),
            "1111111111111111111111111111111111111111"
        );
        assert_eq!(
            to.unwrap().as_str(),
            "2222222222222222222222222222222222222222"
        );
        assert!(dirty);
        assert_eq!(
            parse_submodule_patch("--- a/x\n+++ b/x\n-old\n+new\n"),
            None
        );
    }

    #[test]
    fn parses_gitmodules() {
        let text = "submodule.vendor/lib.path=vendor/lib\nsubmodule.vendor/lib.url=https://example.com/lib.git\n";
        assert_eq!(
            parse_gitmodules(text),
            [
                ("vendor/lib".into(), "path".into(), "vendor/lib".into()),
                (
                    "vendor/lib".into(),
                    "url".into(),
                    "https://example.com/lib.git".into()
                ),
            ]
        );
    }
}
