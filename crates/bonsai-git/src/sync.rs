//! Fetch, pull and push, and managing remotes (CONTEXT.md: Remote, Sync).

use std::{ffi::OsString, sync::Arc};

use crate::{CancelToken, CloneProgress, Error, GitCommand, Repository, Result};

/// How `pull` combines the upstream's commits with the current branch's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PullMode {
    #[default]
    Merge,
    Rebase,
    /// Only fast-forward; fails when the branches diverged.
    FastForwardOnly,
}

/// The stash message for changes put aside during a pull.
const PULL_STASH: &str = "Saved before pull";

const PUT_ASIDE_NOTE: &str =
    "Your uncommitted changes wait in the stash \"Saved before pull\"; apply it once you're done.";

/// Why Git refused a push or pull, with what to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncFailure {
    /// The remote has commits the local branch lacks.
    Rejected,
    /// A force push with lease found the remote moved since the last fetch.
    StaleLease,
    /// A server-side hook or branch protection refused the push.
    RemoteRejected,
    /// The branches diverged and only a fast-forward was allowed.
    Diverged,
    /// The current branch has no upstream to pull from.
    NoUpstream,
    /// A pull stopped on conflicting changes, which wait in Changes.
    Conflicts,
}

impl SyncFailure {
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Rejected => {
                "The remote has commits you don't have yet. Pull first, then push again."
            }
            Self::StaleLease => {
                "The remote branch changed since your last fetch, so the force push was stopped to keep someone else's work. Fetch and look at the new commits first."
            }
            Self::RemoteRejected => {
                "The server refused the push, usually because of branch protection or a server hook. Its message is below."
            }
            Self::Diverged => {
                "Your branch and its upstream both have new commits. Pull with merge or rebase to combine them."
            }
            Self::NoUpstream => {
                "This branch doesn't track a remote branch yet. Publish it, or set its upstream."
            }
            Self::Conflicts => {
                "Pulled, but some files changed on both sides. Choose what to keep in Changes, then continue."
            }
        }
    }

    /// Recognizes Git's output; conflicts are reported on stdout, so pass
    /// both streams.
    pub(crate) fn classify(stderr: &str) -> Option<Self> {
        if stderr.contains("(stale info)") {
            Some(Self::StaleLease)
        } else if stderr.contains("[remote rejected]") || stderr.contains("hook declined") {
            Some(Self::RemoteRejected)
        } else if stderr.contains("(non-fast-forward)") || stderr.contains("(fetch first)") {
            Some(Self::Rejected)
        } else if stderr.contains("Not possible to fast-forward") {
            Some(Self::Diverged)
        } else if stderr.contains("There is no tracking information") {
            Some(Self::NoUpstream)
        } else if stderr.contains("CONFLICT (") || stderr.contains("could not apply") {
            Some(Self::Conflicts)
        } else {
            None
        }
    }
}

impl Error {
    /// The recognized reason a fetch, pull or push was refused.
    #[must_use]
    pub fn sync_failure(&self) -> Option<SyncFailure> {
        match self {
            Self::Failed { record } => {
                SyncFailure::classify(&format!("{}\n{}", record.stderr, record.stdout))
            }
            _ => None,
        }
    }
}

/// What to push where.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushRequest {
    pub remote: String,
    /// The local branch.
    pub branch: String,
    /// The branch name on the remote; the local name when `None`.
    pub remote_branch: Option<String>,
    /// Make the remote branch the local branch's upstream (publishing it).
    pub set_upstream: bool,
    /// Force with `--force-with-lease`, so commits Bonsai has not fetched are
    /// never overwritten. Bonsai has no plain force push.
    pub force: bool,
}

impl PushRequest {
    #[must_use]
    pub fn new(remote: impl Into<String>, branch: impl Into<String>) -> Self {
        Self {
            remote: remote.into(),
            branch: branch.into(),
            remote_branch: None,
            set_upstream: false,
            force: false,
        }
    }

    #[must_use]
    pub fn to_branch(mut self, remote_branch: impl Into<String>) -> Self {
        self.remote_branch = Some(remote_branch.into());
        self
    }

    #[must_use]
    pub fn set_upstream(mut self) -> Self {
        self.set_upstream = true;
        self
    }

    #[must_use]
    pub fn force_with_lease(mut self) -> Self {
        self.force = true;
        self
    }
}

/// A remote branch to move from its pre-purge commit to the rewritten one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewrittenBranch {
    /// The branch name on the remote.
    pub name: String,
    pub old: String,
    pub new: String,
}

/// Progress callback for network operations.
pub type Progress = Arc<dyn Fn(CloneProgress) + Send + Sync>;

fn with_progress(
    mut command: GitCommand,
    progress: Option<Progress>,
    cancel: Option<CancelToken>,
) -> GitCommand {
    if let Some(progress) = progress {
        command = command.on_stderr(move |line| {
            if let Some(update) = CloneProgress::parse(line) {
                progress(update);
            }
        });
    }
    if let Some(cancel) = cancel {
        command = command.cancel(cancel);
    }
    command
}

impl Repository {
    fn network(
        &self,
        args: Vec<OsString>,
        progress: Option<Progress>,
        cancel: Option<CancelToken>,
    ) -> Result<()> {
        let command = with_progress(self.git().command(args).cwd(self.root()), progress, cancel);
        self.git().run(&command)?;
        Ok(())
    }

    /// Fetches one remote, or every remote when `None`.
    pub fn fetch(
        &self,
        remote: Option<&str>,
        prune: bool,
        progress: Option<Progress>,
        cancel: Option<CancelToken>,
    ) -> Result<()> {
        let mut args: Vec<OsString> = vec!["fetch".into(), "--progress".into()];
        if prune {
            args.push("--prune".into());
        }
        match remote {
            Some(remote) => {
                args.push("--".into());
                args.push(remote.into());
            }
            None => args.push("--all".into()),
        }
        self.network(args, progress, cancel)
    }

    /// Fetches `source` (such as `refs/pull/7/head`) from a remote into the
    /// local branch `local`, then switches to it. When `local` is already
    /// checked out, it fast-forwards instead. A local branch that has moved
    /// on from the fetched commit is left alone, and Git says why.
    pub fn fetch_into_branch(
        &self,
        remote: &str,
        source: &str,
        local: &str,
        progress: Option<Progress>,
        cancel: Option<CancelToken>,
    ) -> Result<()> {
        let head = self.git().run_unchecked(
            &self
                .git()
                .command(["symbolic-ref", "--quiet", "--short", "HEAD"])
                .cwd(self.root()),
        )?;
        let current = head.status.success() && head.stdout_lossy().trim() == local;
        let target = if current {
            source.to_owned()
        } else {
            format!("{source}:refs/heads/{local}")
        };
        let args = vec![
            OsString::from("fetch"),
            "--progress".into(),
            "--".into(),
            remote.into(),
            target.into(),
        ];
        self.network(args, progress, cancel)?;
        let args: Vec<OsString> = if current {
            vec!["merge".into(), "--ff-only".into(), "FETCH_HEAD".into()]
        } else {
            vec!["switch".into(), "--no-guess".into(), local.into()]
        };
        self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(())
    }

    /// Pulls the current branch's upstream. Uncommitted changes to tracked
    /// files never block a pull: they're stashed first and put back after,
    /// staged parts staged again. If the pull stops on conflicts, or the
    /// changes clash with what came in, they wait in the stash instead.
    pub fn pull(
        &self,
        mode: PullMode,
        progress: Option<Progress>,
        cancel: Option<CancelToken>,
    ) -> Result<()> {
        let flag = match mode {
            PullMode::Merge => "--no-rebase",
            PullMode::Rebase => "--rebase",
            PullMode::FastForwardOnly => "--ff-only",
        };
        let args = ["pull", "--progress", "--no-edit", flag]
            .iter()
            .map(OsString::from)
            .collect();
        let saved = self.put_aside_for_pull()?;
        let pulled = self.network(args, progress, cancel);
        if !saved {
            return pulled;
        }
        if let Err(error) = pulled {
            if self.in_progress().is_none() {
                // Nothing changed: put the work back as it was.
                self.put_back_after_pull()?;
                return Err(error);
            }
            return Err(match error.sync_failure() {
                Some(SyncFailure::Conflicts) => Error::Refused(format!(
                    "{} {PUT_ASIDE_NOTE}",
                    SyncFailure::Conflicts.message()
                )),
                _ => error,
            });
        }
        if self.put_back_after_pull()? {
            Ok(())
        } else {
            Err(Error::Refused(format!(
                "Pulled, but your uncommitted changes touch the same lines as what came in. Resolve the conflicts in Changes; the stash \"{PULL_STASH}\" keeps a copy."
            )))
        }
    }

    /// Stashes uncommitted changes to tracked files before a pull; returns
    /// whether there were any.
    fn put_aside_for_pull(&self) -> Result<bool> {
        let status = self.git().run(
            &self
                .git()
                .command(["status", "--porcelain", "--untracked-files=no"])
                .cwd(self.root()),
        )?;
        if status.stdout.is_empty() {
            return Ok(false);
        }
        self.git().run(
            &self
                .git()
                .command(["stash", "push", "--quiet", "-m", PULL_STASH])
                .cwd(self.root()),
        )?;
        Ok(true)
    }

    /// Pops the stash [`Self::put_aside_for_pull`] made, restaging what was
    /// staged. Returns false, keeping the stash, when it doesn't apply
    /// cleanly.
    fn put_back_after_pull(&self) -> Result<bool> {
        let pop = |index: bool| {
            let mut args = vec!["stash", "pop", "--quiet"];
            if index {
                args.push("--index");
            }
            self.git()
                .run(&self.git().command(args).cwd(self.root()))
                .is_ok()
        };
        if pop(true) {
            return Ok(true);
        }
        // `--index` refuses when the staged parts no longer apply; a plain
        // pop still brings the work back, unstaged. A pop that conflicts
        // keeps the stash and leaves markers, which Changes shows.
        let conflicted = !self
            .git()
            .run(
                &self
                    .git()
                    .command(["diff", "--name-only", "--diff-filter=U"])
                    .cwd(self.root()),
            )?
            .stdout
            .is_empty();
        Ok(!conflicted && pop(false))
    }

    /// Pushes a local branch (CONTEXT.md: Push).
    pub fn push(
        &self,
        request: &PushRequest,
        progress: Option<Progress>,
        cancel: Option<CancelToken>,
    ) -> Result<()> {
        let mut args: Vec<OsString> = vec!["push".into(), "--progress".into()];
        let target = request.remote_branch.as_deref().unwrap_or(&request.branch);
        if request.set_upstream {
            args.push("--set-upstream".into());
        }
        if request.force {
            args.push(format!("--force-with-lease=refs/heads/{target}").into());
        }
        args.push("--".into());
        args.push(request.remote.clone().into());
        args.push(format!("refs/heads/{}:refs/heads/{target}", request.branch).into());
        self.network(args, progress, cancel)
    }

    /// Force pushes branches whose history a purge rewrote: each remote
    /// branch moves from `old` (its value before the rewrite) to `new`, and
    /// only if the server still has `old`. One atomic push per call.
    pub fn push_rewritten(
        &self,
        remote: &str,
        branches: &[RewrittenBranch],
        progress: Option<Progress>,
        cancel: Option<CancelToken>,
    ) -> Result<()> {
        if branches.is_empty() {
            return Ok(());
        }
        let mut args: Vec<OsString> = vec!["push".into(), "--progress".into(), "--atomic".into()];
        for branch in branches {
            args.push(
                format!(
                    "--force-with-lease=refs/heads/{}:{}",
                    branch.name, branch.old
                )
                .into(),
            );
        }
        args.push("--".into());
        args.push(remote.into());
        for branch in branches {
            args.push(format!("{}:refs/heads/{}", branch.new, branch.name).into());
        }
        self.network(args, progress, cancel)
    }

    /// Deletes a branch on a remote.
    pub fn delete_remote_branch(&self, remote: &str, branch: &str) -> Result<()> {
        self.network(
            vec![
                "push".into(),
                "--".into(),
                remote.into(),
                format!(":refs/heads/{branch}").into(),
            ],
            None,
            None,
        )
    }

    /// Pushes one tag to a remote.
    pub fn push_tag(&self, remote: &str, name: &str) -> Result<()> {
        self.network(
            vec![
                "push".into(),
                "--".into(),
                remote.into(),
                format!("refs/tags/{name}").into(),
            ],
            None,
            None,
        )
    }

    /// Deletes a tag on a remote; the local tag stays.
    pub fn delete_remote_tag(&self, remote: &str, name: &str) -> Result<()> {
        self.network(
            vec![
                "push".into(),
                "--".into(),
                remote.into(),
                format!(":refs/tags/{name}").into(),
            ],
            None,
            None,
        )
    }

    fn config_remote(&self, args: &[&str]) -> Result<()> {
        self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(())
    }

    /// Adds a remote. Any URL Git accepts works: HTTPS, SSH, `git://`, a
    /// path or `file://`.
    pub fn add_remote(&self, name: &str, url: &str) -> Result<()> {
        self.config_remote(&["remote", "add", "--", name, url])
    }

    pub fn set_remote_url(&self, name: &str, url: &str) -> Result<()> {
        self.config_remote(&["remote", "set-url", "--", name, url])
    }

    pub fn rename_remote(&self, old: &str, new: &str) -> Result<()> {
        self.config_remote(&["remote", "rename", "--", old, new])
    }

    pub fn remove_remote(&self, name: &str) -> Result<()> {
        self.config_remote(&["remote", "remove", "--", name])
    }
}
