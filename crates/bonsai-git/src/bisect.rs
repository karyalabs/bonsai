//! Bisecting: a binary search through history for the commit that
//! introduced a bug (CONTEXT.md: Bisect).

use std::ffi::OsString;

use crate::{Error, InProgress, Oid, Repository, Result};

/// How the commit being tested behaved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BisectVerdict {
    Good,
    Bad,
    /// It can't be tested, for example because it doesn't build.
    Skip,
}

impl BisectVerdict {
    fn command(self) -> &'static str {
        match self {
            Self::Good => "good",
            Self::Bad => "bad",
            Self::Skip => "skip",
        }
    }
}

/// A commit named in a bisect, with its summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BisectCommit {
    pub oid: Oid,
    pub summary: String,
}

/// Where a bisect stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BisectState {
    /// The newest commit known to be bad.
    pub bad: Option<Oid>,
    /// Commits known to be good.
    pub good: Vec<Oid>,
    pub skipped: Vec<Oid>,
    /// The commit checked out for testing.
    pub current: Option<BisectCommit>,
    /// Commits that could still be the first bad one, counting `bad`.
    pub candidates: usize,
    /// The first bad commit, once found.
    pub found: Option<BisectCommit>,
}

impl BisectState {
    /// Roughly how many more tests it takes: log2 of the candidates.
    #[must_use]
    pub fn steps_left(&self) -> u32 {
        if self.found.is_some() || self.candidates <= 1 {
            0
        } else {
            (self.candidates - 1).bit_width()
        }
    }

    /// Whether Git still needs a good or a bad commit before it can search.
    #[must_use]
    pub fn needs_more_marks(&self) -> bool {
        self.found.is_none() && (self.bad.is_none() || self.good.is_empty())
    }
}

impl Repository {
    fn bisect_git<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Ok(self
            .git()
            .run(&self.git().command(args).cwd(self.root()))?
            .stdout_lossy())
    }

    fn refuse_if_busy(&self) -> Result<()> {
        if let Some(operation) = self.in_progress() {
            return Err(Error::Refused(format!(
                "Finish the {} in progress before bisecting.",
                match operation {
                    InProgress::Bisect => "bisect",
                    InProgress::Merge => "merge",
                    InProgress::Rebase | InProgress::ApplyMailbox => "rebase",
                    InProgress::CherryPick => "cherry-pick",
                    InProgress::Revert => "revert",
                }
            )));
        }
        Ok(())
    }

    fn bisect_start_empty(&self) -> Result<()> {
        self.refuse_if_busy()?;
        self.bisect_git(["bisect", "start"])?;
        Ok(())
    }

    /// Starts a bisect between a bad commit and one or more good ones, and
    /// checks out the first commit to test.
    pub fn bisect_start(&self, bad: &str, good: &[&str]) -> Result<BisectState> {
        self.refuse_if_busy()?;
        let mut args: Vec<OsString> = vec!["bisect".into(), "start".into(), bad.into()];
        args.extend(good.iter().map(OsString::from));
        args.push("--".into());
        self.bisect_git(args)?;
        self.require_bisect()
    }

    fn require_bisect(&self) -> Result<BisectState> {
        self.bisect_state()?
            .ok_or_else(|| Error::Refused("No bisect is in progress.".to_owned()))
    }

    /// Marks the commit being tested, and checks out the next one.
    pub fn bisect_mark(&self, verdict: BisectVerdict) -> Result<BisectState> {
        self.bisect_git(["bisect", verdict.command()])?;
        self.require_bisect()
    }

    /// Marks a given commit, such as one picked in History, starting a
    /// bisect first when none is in progress.
    pub fn bisect_mark_commit(&self, oid: &str, verdict: BisectVerdict) -> Result<BisectState> {
        if self.in_progress() != Some(InProgress::Bisect) {
            self.bisect_start_empty()?;
        }
        self.bisect_git(["bisect", verdict.command(), oid])?;
        self.require_bisect()
    }

    /// Runs `command` through `sh -c` at each step: exit 0 means good, 125
    /// skip, and any other code up to 127 bad. Returns once Git finds the
    /// first bad commit or gives up.
    pub fn bisect_run(&self, command: &str) -> Result<BisectState> {
        self.bisect_git(["bisect", "run", "sh", "-c", command])?;
        self.require_bisect()
    }

    /// Ends the bisect and returns to the branch it started from.
    pub fn bisect_reset(&self) -> Result<()> {
        self.bisect_git(["bisect", "reset"])?;
        Ok(())
    }

    /// The bisect in progress, or `None`.
    pub fn bisect_state(&self) -> Result<Option<BisectState>> {
        if self.in_progress() != Some(InProgress::Bisect) {
            return Ok(None);
        }
        let refs = self.bisect_git([
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/bisect/",
        ])?;
        let mut state = BisectState::default();
        for line in refs.lines() {
            let Some((oid, name)) = line.split_once(' ') else {
                continue;
            };
            let oid = Oid::new(oid);
            if name == "refs/bisect/bad" {
                state.bad = Some(oid);
            } else if name.starts_with("refs/bisect/good-") {
                state.good.push(oid);
            } else if name.starts_with("refs/bisect/skip-") {
                state.skipped.push(oid);
            }
        }
        let log = self.bisect_git(["bisect", "log"])?;
        if let Some(found) = log
            .lines()
            .find_map(|line| line.strip_prefix("# first bad commit: ["))
            && let Some((oid, summary)) = found.split_once("] ")
        {
            state.found = Some(BisectCommit {
                oid: Oid::new(oid),
                summary: summary.to_owned(),
            });
        }
        let head = self.bisect_git(["log", "-1", "--format=%H%x00%s", "HEAD"])?;
        if let Some((oid, summary)) = head.trim_end_matches('\n').split_once('\0') {
            state.current = Some(BisectCommit {
                oid: Oid::new(oid),
                summary: summary.to_owned(),
            });
        }
        if let Some(bad) = &state.bad {
            let mut args = vec![
                "rev-list".to_owned(),
                "--count".to_owned(),
                bad.as_str().to_owned(),
            ];
            if !state.good.is_empty() {
                args.push("--not".to_owned());
                args.extend(state.good.iter().map(|g| g.as_str().to_owned()));
            }
            state.candidates = self.bisect_git(args)?.trim().parse().unwrap_or_default();
        }
        Ok(Some(state))
    }
}
