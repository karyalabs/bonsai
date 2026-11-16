//! Cloning a repository from a URL, with progress and cancellation.

use std::path::Path;

use crate::{CancelToken, Error, Git, Repository, Result};

/// One progress update from `git clone --progress`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneProgress {
    /// Git's name for the phase, such as "Receiving objects".
    pub phase: String,
    /// Percent done in this phase, when Git reports one.
    pub percent: Option<u8>,
    /// Items done and total in this phase, when Git reports them.
    pub done: Option<u64>,
    pub total: Option<u64>,
}

impl CloneProgress {
    /// Parses a progress line such as
    /// `Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s` or
    /// `remote: Counting objects: 12, done.`. Other lines give `None`.
    #[must_use]
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim().strip_prefix("remote:").unwrap_or(line).trim();
        let (phase, rest) = line.split_once(':')?;
        if phase.is_empty() || !phase.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') {
            return None;
        }
        let rest = rest.trim();
        let mut progress = Self {
            phase: phase.to_owned(),
            percent: None,
            done: None,
            total: None,
        };
        if let Some((percent, after)) = rest.split_once('%') {
            progress.percent = percent.trim().parse::<u8>().ok().filter(|p| *p <= 100);
            if let Some(counts) = after
                .trim()
                .strip_prefix('(')
                .and_then(|c| c.split_once(')'))
                .map(|(c, _)| c)
                && let Some((done, total)) = counts.split_once('/')
            {
                progress.done = done.parse().ok();
                progress.total = total.parse().ok();
            }
        } else {
            let count = rest.split(|c: char| !c.is_ascii_digit()).next()?;
            progress.done = Some(count.parse().ok()?);
        }
        Some(progress)
    }

    /// Progress through the whole clone, from 0.0 to 1.0, weighting the
    /// phases that take most of the time.
    #[must_use]
    pub fn overall(&self) -> Option<f32> {
        let fraction = f32::from(self.percent?) / 100.;
        let (start, span) = match self.phase.as_str() {
            "Counting objects" | "Compressing objects" | "Enumerating objects" => (0., 0.05),
            "Receiving objects" => (0.05, 0.75),
            "Resolving deltas" => (0.8, 0.15),
            "Updating files" | "Checking out files" => (0.95, 0.05),
            _ => return None,
        };
        Some(start + span * fraction)
    }
}

/// The folder name `git clone` would create for `url`: its last path
/// component without `.git`. `None` when the URL has no usable name.
#[must_use]
pub fn clone_name(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let trimmed = trimmed.trim_end_matches('/');
    let name = trimmed.rsplit(['/', ':', '\\']).next()?;
    (!name.is_empty() && name != "." && name != "..").then(|| name.to_owned())
}

impl Repository {
    /// Clones `url` into `destination`, which must not exist or be empty, and
    /// opens it. `progress` receives each update. A cancelled clone removes
    /// the folder it created.
    pub fn clone_from(
        git: &Git,
        url: &str,
        destination: &Path,
        progress: impl Fn(CloneProgress) + Send + Sync + 'static,
        cancel: Option<CancelToken>,
    ) -> Result<Self> {
        let existed = destination.exists();
        let mut command = git
            .command([
                "clone".into(),
                "--progress".into(),
                "--".into(),
                url.to_owned().into(),
                destination.as_os_str().to_owned(),
            ])
            .on_stderr(move |line| {
                if let Some(update) = CloneProgress::parse(line) {
                    progress(update);
                }
            });
        if let Some(cancel) = cancel {
            command = command.cancel(cancel);
        }
        match git.run(&command) {
            Ok(_) => Self::open(git, destination),
            Err(error) => {
                if !existed && matches!(error, Error::Cancelled | Error::Failed { .. }) {
                    // Git removes its own partial clone on failure, but not
                    // when it is killed.
                    let _ = std::fs::remove_dir_all(destination);
                }
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_lines() {
        assert_eq!(
            CloneProgress::parse("Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s"),
            Some(CloneProgress {
                phase: "Receiving objects".into(),
                percent: Some(45),
                done: Some(450),
                total: Some(1000),
            })
        );
        assert_eq!(
            CloneProgress::parse("remote: Counting objects: 12, done."),
            Some(CloneProgress {
                phase: "Counting objects".into(),
                percent: None,
                done: Some(12),
                total: None,
            })
        );
        assert_eq!(CloneProgress::parse("Cloning into 'bonsai'..."), None);
        assert_eq!(CloneProgress::parse("fatal: repository not found"), None);
        let resolving = CloneProgress::parse("Resolving deltas: 100% (5/5), done.").unwrap();
        assert!((resolving.overall().unwrap() - 0.95).abs() < 1e-6);
    }

    #[test]
    fn derives_folder_names_like_git() {
        for (url, name) in [
            ("https://github.com/karyalabs/bonsai.git", Some("bonsai")),
            ("https://github.com/karyalabs/bonsai/", Some("bonsai")),
            ("git@github.com:karyalabs/bonsai.git", Some("bonsai")),
            ("ssh://git@host:2222/team/app.git/", Some("app")),
            ("file:///Users/me/work/project", Some("project")),
            ("/Users/me/work/project/.git", Some("project")),
            ("https://", None),
            ("", None),
        ] {
            assert_eq!(clone_name(url).as_deref(), name, "{url}");
        }
    }
}
