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
