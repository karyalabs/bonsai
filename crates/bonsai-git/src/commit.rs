//! Creating and amending commits, and what the commit composer needs to know.

use crate::{Oid, Repository, Result};

/// How commits are signed, from `commit.gpgSign` and `gpg.format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signing {
    OpenPgp,
    Ssh,
    X509,
}

impl Signing {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenPgp => "GPG",
            Self::Ssh => "SSH",
            Self::X509 => "X.509",
        }
    }
}

impl Repository {
    /// Commits the index with `message`. Hooks and signing run as Git is
    /// configured; a failing hook's output is in the error. With `amend`, the
    /// last commit is replaced instead.
    pub fn commit(&self, message: &str, amend: bool) -> Result<Oid> {
        let mut args = vec!["commit", "--file=-", "--cleanup=strip"];
        if amend {
            args.push("--amend");
        }
        self.git().run(
            &self
                .git()
                .command(args)
                .cwd(self.root())
                .stdin(message.as_bytes().to_vec()),
        )?;
        let head = self
            .git()
            .run(&self.git().command(["rev-parse", "HEAD"]).cwd(self.root()))?;
        Ok(Oid::new(head.stdout_lossy().trim()))
    }

    /// The last commit's full message, or `None` before the first commit.
    pub fn last_commit_message(&self) -> Result<Option<String>> {
        Ok(self.recent_messages(1)?.into_iter().next())
    }

    /// The full message of `revision`, such as `ORIG_HEAD` after an undone
    /// commit.
    pub fn commit_message(&self, revision: &str) -> Result<String> {
        let output = self.git().run(
            &self
                .git()
                .command(["log", "-1", "--format=%B", revision, "--"])
                .cwd(self.root()),
        )?;
        Ok(output.stdout_lossy().trim_end().to_owned())
    }

    /// Up to `count` people who recently authored commits on any branch,
    /// most recent first, as `(name, email)`, leaving out the committer
    /// identity this repository uses. Offered as co-authors.
    pub fn recent_authors(&self, count: usize) -> Result<Vec<(String, String)>> {
        if self.head_is_unborn()? {
            return Ok(Vec::new());
        }
        let me = self
            .git()
            .run_unchecked(
                &self
                    .git()
                    .command(["config", "user.email"])
                    .cwd(self.root()),
            )?
            .stdout_lossy()
            .trim()
            .to_lowercase();
        let output = self.git().run(
            &self
                .git()
                .command(["log", "--all", "--max-count=1000", "--format=%an%x1f%ae"])
                .cwd(self.root()),
        )?;
        let mut authors: Vec<(String, String)> = Vec::new();
        for line in output.stdout_lossy().lines() {
            let Some((name, email)) = line.split_once('\x1f') else {
                continue;
            };
            let key = email.trim().to_lowercase();
            if key.is_empty()
                || key == me
                || key.contains("[bot]")
                || authors.iter().any(|(_, e)| e.to_lowercase() == key)
            {
                continue;
            }
            authors.push((name.trim().to_owned(), email.trim().to_owned()));
            if authors.len() == count {
                break;
            }
        }
        Ok(authors)
    }

    /// The text of the file `commit.template` names, which starts every
    /// commit message, or `None` when it isn't set or can't be read.
    pub fn commit_template(&self) -> Result<Option<String>> {
        let output = self.git().run_unchecked(
            &self
                .git()
                .command(["config", "--path", "commit.template"])
                .cwd(self.root()),
        )?;
        let path = output.stdout_lossy().trim().to_owned();
        if !output.status.success() || path.is_empty() {
            return Ok(None);
        }
        let path = std::path::PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            self.root().join(path)
        };
        Ok(std::fs::read_to_string(path)
            .ok()
            .map(|text| text.trim_end().to_owned())
            .filter(|text| !text.is_empty()))
    }

    /// The message Git prepared for the next commit: a merge's (`MERGE_MSG`,
    /// with its conflict notes as comments) or a squash merge's
    /// (`SQUASH_MSG`). `None` when there is none.
    #[must_use]
    pub fn prepared_message(&self) -> Option<String> {
        ["MERGE_MSG", "SQUASH_MSG"].iter().find_map(|name| {
            std::fs::read_to_string(self.git_dir().join(name))
                .ok()
                .map(|text| text.trim_end().to_owned())
                // A squash merge's conflicts leave a MERGE_MSG of comments
                // only; its message is in SQUASH_MSG.
                .filter(|text| {
                    text.lines()
                        .any(|line| !line.starts_with('#') && !line.trim().is_empty())
                })
        })
    }

    /// Messages of up to `count` recent commits on HEAD, newest first,
    /// without duplicates.
    pub fn recent_messages(&self, count: usize) -> Result<Vec<String>> {
        if self.head_is_unborn()? {
            return Ok(Vec::new());
        }
        let output = self.git().run(
            &self
                .git()
                .command([
                    "log".to_owned(),
                    "-z".to_owned(),
                    "--format=%B".to_owned(),
                    format!("--max-count={}", count * 2),
                ])
                .cwd(self.root()),
        )?;
        let mut messages: Vec<String> = Vec::new();
        for message in output.stdout_lossy().split('\0') {
            let message = message.trim();
            if !message.is_empty() && !messages.iter().any(|m| m == message) {
                messages.push(message.to_owned());
            }
            if messages.len() == count {
                break;
            }
        }
        Ok(messages)
    }

    /// How a new commit will be signed, or `None` when signing is off.
    pub fn commit_signing(&self) -> Result<Option<Signing>> {
        let config = |key: &str| -> Result<Option<String>> {
            let output = self.git().run_unchecked(
                &self
                    .git()
                    .command(["config", "--get", key])
                    .cwd(self.root()),
            )?;
            Ok(output
                .status
                .success()
                .then(|| output.stdout_lossy().trim().to_ascii_lowercase()))
        };
        let enabled = config("commit.gpgsign")?
            .is_some_and(|value| matches!(value.as_str(), "true" | "yes" | "on" | "1"));
        if !enabled {
            return Ok(None);
        }
        Ok(Some(match config("gpg.format")?.as_deref() {
            Some("ssh") => Signing::Ssh,
            Some("x509") => Signing::X509,
            _ => Signing::OpenPgp,
        }))
    }
}
