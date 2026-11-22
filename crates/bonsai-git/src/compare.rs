//! How two revisions differ: the commits each has that the other lacks,
//! where they split, and the size of the change since then.

use crate::{CommitInfo, LogFilter, LogScope, Oid, Repository, Result};

/// `head` against `base`, as a pull request from `head` into `base` sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comparison {
    /// The commit `head` names.
    pub head_oid: Oid,
    /// The newest commit both have; `None` for unrelated histories.
    pub merge_base: Option<Oid>,
    /// Commits on `head` that `base` lacks, newest first, up to the limit.
    pub ahead: Vec<CommitInfo>,
    /// Commits on `base` that `head` lacks, newest first, up to the limit.
    pub behind: Vec<CommitInfo>,
    /// How many commits each side has, even past the limit.
    pub ahead_count: usize,
    pub behind_count: usize,
    /// What `head` changed since the merge base.
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
}

impl Repository {
    /// Compares `head` with `base`, listing up to `limit` commits a side.
    ///
    /// # Errors
    /// When either revision doesn't exist.
    pub fn compare(&self, base: &str, head: &str, limit: usize) -> Result<Comparison> {
        let head_oid = self.git().run(
            &self
                .git()
                .command(["rev-parse", "--verify", "-q", &format!("{head}^{{commit}}")])
                .cwd(self.root()),
        )?;
        let head_oid = Oid::new(head_oid.stdout_lossy().trim());
        let counts = self.git().run(
            &self
                .git()
                .command([
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{base}...{head}"),
                    "--",
                ])
                .cwd(self.root()),
        )?;
        let counts = counts.stdout_lossy();
        let mut counts = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
        let behind_count = counts.next().unwrap_or(0);
        let ahead_count = counts.next().unwrap_or(0);
        let merge_base = self.git().run_unchecked(
            &self
                .git()
                .command(["merge-base", base, head])
                .cwd(self.root()),
        )?;
        let merge_base = merge_base
            .status
            .success()
            .then(|| merge_base.stdout_lossy().trim().to_owned())
            .filter(|oid| !oid.is_empty())
            .map(Oid::new);
        let side = |from: &str, not: &str| {
            self.log_filtered(
                &LogScope::Refs(vec![from.to_owned(), format!("^{not}")]),
                &LogFilter::default(),
                0,
                limit,
            )
        };
        let ahead = side(head, base)?;
        let behind = side(base, head)?;
        let (mut files_changed, mut insertions, mut deletions) = (0, 0, 0);
        if let Some(merge_base) = &merge_base {
            let stat = self.git().run(
                &self
                    .git()
                    .command(["diff", "--numstat", "-z", merge_base.as_str(), head])
                    .cwd(self.root()),
            )?;
            let stat = stat.stdout_lossy();
            // Renames put their two paths in their own NUL-separated fields
            // after an empty one; only records with counts are files.
            for record in stat.split('\0') {
                let mut fields = record.split('\t');
                let (Some(added), Some(removed)) = (fields.next(), fields.next()) else {
                    continue;
                };
                files_changed += 1;
                insertions += added.parse::<usize>().unwrap_or(0);
                deletions += removed.parse::<usize>().unwrap_or(0);
            }
        }
        Ok(Comparison {
            head_oid,
            merge_base,
            ahead,
            behind,
            ahead_count,
            behind_count,
            files_changed,
            insertions,
            deletions,
        })
    }
}
