//! Where HEAD has been, from its reflog, so lost commits can be found and
//! restored.

use std::collections::HashSet;

use crate::{Oid, Repository, Result};

/// What moved HEAD, from the start of a reflog message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReflogAction {
    Commit,
    Amend,
    Checkout,
    Reset,
    Rebase,
    Merge,
    Pull,
    CherryPick,
    Revert,
    Other,
}

/// One move of HEAD, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflogEntry {
    /// Where HEAD pointed after the move.
    pub oid: Oid,
    /// `HEAD@{n}`: how many moves ago.
    pub index: usize,
    /// Seconds since the Unix epoch.
    pub time: i64,
    pub action: ReflogAction,
    /// Git's reflog message, such as `checkout: moving from main to f`.
    pub message: String,
    /// The commit's summary.
    pub summary: String,
    /// Whether no branch, tag, remote branch, stash or HEAD reaches the
    /// commit, so it is gone once the reflog expires.
    pub lost: bool,
}

impl ReflogEntry {
    /// The message in words, such as `Switched from main to f`.
    #[must_use]
    pub fn describe(&self) -> String {
        describe(&self.message)
    }
}

fn action(message: &str) -> ReflogAction {
    let head = message.split(':').next().unwrap_or_default();
    let word = head.split_whitespace().next().unwrap_or_default();
    match word {
        "commit" if head.contains("amend") => ReflogAction::Amend,
        "commit" if head.contains("merge") => ReflogAction::Merge,
        "commit" => ReflogAction::Commit,
        "checkout" | "switch" => ReflogAction::Checkout,
        "reset" => ReflogAction::Reset,
        "rebase" => ReflogAction::Rebase,
        "pull" if head.contains("rebase") => ReflogAction::Rebase,
        "pull" => ReflogAction::Pull,
        "merge" => ReflogAction::Merge,
        "cherry-pick" => ReflogAction::CherryPick,
        "revert" => ReflogAction::Revert,
        _ => ReflogAction::Other,
    }
}

/// A reflog message in words.
#[must_use]
pub fn describe(message: &str) -> String {
    let (head, rest) = message
        .split_once(": ")
        .map_or((message, ""), |(h, r)| (h, r.trim()));
    if let Some(moves) = rest.strip_prefix("moving from ")
        && let Some((from, to)) = moves.split_once(" to ")
        && head.starts_with("checkout")
    {
        return format!("Switched from {from} to {to}");
    }
    let said = match action(message) {
        ReflogAction::Commit if head.contains("initial") => "Committed the first commit",
        ReflogAction::Commit => "Committed",
        ReflogAction::Amend => "Amended",
        ReflogAction::Reset => {
            return match rest.strip_prefix("moving to ") {
                Some(target) => format!("Reset to {target}"),
                None => "Reset".to_owned(),
            };
        }
        ReflogAction::Rebase => {
            if head.contains("finish") {
                "Finished a rebase"
            } else if head.contains("start") {
                "Started a rebase"
            } else if head.contains("abort") {
                "Aborted a rebase"
            } else {
                "Rebased"
            }
        }
        ReflogAction::Merge if head.starts_with("merge ") => {
            return format!("Merged {}", head.trim_start_matches("merge ").trim());
        }
        ReflogAction::Merge => "Merged",
        ReflogAction::Pull => "Pulled",
        ReflogAction::CherryPick => "Cherry-picked",
        ReflogAction::Revert => "Reverted",
        ReflogAction::Checkout => "Switched",
        ReflogAction::Other => {
            let mut chars = message.chars();
            return chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default();
        }
    };
    said.to_owned()
}

const FIELD: char = '\u{1f}';

impl Repository {
    /// HEAD's latest `limit` moves, newest first. A repository whose HEAD
    /// has no reflog, such as a new one, has none.
    ///
    /// # Errors
    /// When Git can't tell which commits are still reachable.
    pub fn reflog(&self, limit: usize) -> Result<Vec<ReflogEntry>> {
        let limit = format!("-n{limit}");
        let output = self.git().run_unchecked(
            &self
                .git()
                .command([
                    "log",
                    "-g",
                    "-z",
                    "--date=unix",
                    "--format=%H%x1f%gd%x1f%gs%x1f%s",
                    &limit,
                    "HEAD",
                    "--",
                ])
                .cwd(self.root()),
        )?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let text = output.stdout_lossy();
        let mut entries: Vec<ReflogEntry> = text
            .split('\0')
            .filter(|r| !r.trim().is_empty())
            .enumerate()
            .filter_map(|(index, record)| {
                let mut fields = record.trim_start_matches('\n').splitn(4, FIELD);
                let oid = Oid::new(fields.next()?);
                let selector = fields.next()?;
                let time = selector
                    .rsplit_once("@{")
                    .and_then(|(_, t)| t.trim_end_matches('}').parse().ok())
                    .unwrap_or(0);
                let message = fields.next()?.to_owned();
                let summary = fields.next().unwrap_or_default().to_owned();
                Some(ReflogEntry {
                    oid,
                    index,
                    time,
                    action: action(&message),
                    message,
                    summary,
                    lost: false,
                })
            })
            .collect();
        let lost = self.unreachable(entries.iter().map(|e| &e.oid))?;
        for entry in &mut entries {
            entry.lost = lost.contains(&entry.oid);
        }
        Ok(entries)
    }

    /// Which of `oids` no ref or HEAD reaches.
    fn unreachable<'a>(&self, oids: impl Iterator<Item = &'a Oid>) -> Result<HashSet<Oid>> {
        let wanted: HashSet<&Oid> = oids.collect();
        if wanted.is_empty() {
            return Ok(HashSet::new());
        }
        let mut args: Vec<&str> = vec!["rev-list"];
        args.extend(wanted.iter().map(|oid| oid.as_str()));
        args.extend(["--not", "--all"]);
        let output = self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(output
            .stdout_lossy()
            .lines()
            .map(Oid::new)
            .filter(|oid| wanted.contains(oid))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_reflog_messages_in_words() {
        for (message, said) in [
            (
                "checkout: moving from main to feature",
                "Switched from main to feature",
            ),
            ("reset: moving to HEAD~1", "Reset to HEAD~1"),
            ("commit (amend): Fix typo", "Amended"),
            ("commit (initial): First", "Committed the first commit"),
            ("commit: Add a thing", "Committed"),
            (
                "rebase (finish): returning to refs/heads/main",
                "Finished a rebase",
            ),
            ("rebase -i (start): checkout main", "Started a rebase"),
            ("pull --rebase (finish): returning", "Finished a rebase"),
            ("merge feature: Fast-forward", "Merged feature"),
            ("cherry-pick: Add a thing", "Cherry-picked"),
            ("branch: Created from HEAD", "Branch: Created from HEAD"),
        ] {
            assert_eq!(describe(message), said, "{message}");
        }
        assert_eq!(action("pull: Fast-forward"), ReflogAction::Pull);
        assert_eq!(action("commit (merge): Merge x"), ReflogAction::Merge);
    }
}
