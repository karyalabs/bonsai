//! `git blame`: which commit last changed each line of a file.

use std::{collections::HashMap, path::Path};

use crate::{Oid, Repository, Result};

/// A commit that last changed at least one line of the blamed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameCommit {
    pub oid: Oid,
    pub author_name: String,
    pub author_email: String,
    /// Seconds since the Unix epoch.
    pub author_time: i64,
    pub summary: String,
    /// The line is older than the history Git looked at (a shallow clone's
    /// boundary).
    pub boundary: bool,
}

impl BlameCommit {
    /// Lines changed in the work tree or index have no commit yet.
    #[must_use]
    pub fn is_uncommitted(&self) -> bool {
        self.oid.as_str().bytes().all(|b| b == b'0')
    }
}

/// One line of the file and the commit that last changed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// Index into [`Blame::commits`].
    pub commit: usize,
    /// The line's number in that commit's version of the file, from 1.
    pub original_line: u32,
    pub text: String,
}

/// Every line of a file with the commit that last changed it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    pub lines: Vec<BlameLine>,
}

impl Blame {
    /// The commit that last changed line `index` (from 0).
    #[must_use]
    pub fn commit_of(&self, index: usize) -> Option<&BlameCommit> {
        self.lines.get(index).map(|line| &self.commits[line.commit])
    }

    /// Whether line `index` starts a run of lines from one commit.
    #[must_use]
    pub fn starts_run(&self, index: usize) -> bool {
        index == 0
            || self
                .lines
                .get(index)
                .zip(self.lines.get(index - 1))
                .is_some_and(|(line, before)| line.commit != before.commit)
    }

    /// Author times of the oldest and newest commits, for shading by age.
    #[must_use]
    pub fn time_range(&self) -> (i64, i64) {
        let times = self
            .commits
            .iter()
            .filter(|c| !c.is_uncommitted())
            .map(|c| c.author_time);
        let (min, max) = times.fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
        if min > max { (0, 0) } else { (min, max) }
    }
}

/// Parses `git blame --porcelain`.
///
/// # Errors
/// When a line is not in the porcelain format.
pub fn parse_porcelain(text: &str) -> std::result::Result<Blame, String> {
    let mut blame = Blame::default();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut current: Option<(usize, u32)> = None;
    for line in text.split('\n') {
        if let Some(content) = line.strip_prefix('\t') {
            let (commit, original_line) =
                current.ok_or_else(|| "a line with no header".to_owned())?;
            blame.lines.push(BlameLine {
                commit,
                original_line,
                text: content.to_owned(),
            });
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            let original_line = value
                .split(' ')
                .next()
                .and_then(|n| n.parse().ok())
                .ok_or_else(|| format!("bad header: {line}"))?;
            let commit = *index_of.entry(key.to_owned()).or_insert_with(|| {
                blame.commits.push(BlameCommit {
                    oid: Oid::new(key),
                    author_name: String::new(),
                    author_email: String::new(),
                    author_time: 0,
                    summary: String::new(),
                    boundary: false,
                });
                blame.commits.len() - 1
            });
            current = Some((commit, original_line));
            continue;
        }
        let Some((commit, _)) = current else { continue };
        let commit = &mut blame.commits[commit];
        match key {
            "author" => value.clone_into(&mut commit.author_name),
            "author-mail" => {
                value
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .clone_into(&mut commit.author_email);
            }
            "author-time" => commit.author_time = value.parse().unwrap_or(0),
            "summary" => value.clone_into(&mut commit.summary),
            "boundary" => commit.boundary = true,
            _ => {}
        }
    }
    Ok(blame)
}

impl Repository {
    /// Blames `path` as `rev` has it, or as the work tree has it when `rev`
    /// is `None` (uncommitted lines then have an all-zero commit).
    ///
    /// # Errors
    /// When Git fails, for example because `rev` has no such file.
    pub fn blame(&self, path: &Path, rev: Option<&str>) -> Result<Blame> {
        let mut args = vec!["blame".to_owned(), "--porcelain".to_owned()];
        if let Some(rev) = rev {
            args.push(rev.to_owned());
        }
        args.push("--".to_owned());
        args.push(path.to_string_lossy().into_owned());
        let output = self.git().run(&self.git().command(args).cwd(self.root()))?;
        parse_porcelain(&output.stdout_lossy()).map_err(crate::Error::Parse)
    }
}
