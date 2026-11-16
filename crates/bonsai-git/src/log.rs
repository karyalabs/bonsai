//! Paged history for the History view.

use crate::{Error, Oid, Repository, Result};

/// Which commits the history walks from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogScope {
    /// Every branch, remote branch and tag, plus HEAD.
    All,
    /// HEAD only.
    Head,
    /// These refs or revisions.
    Refs(Vec<String>),
}

/// Narrows a log to matching commits. Empty fields match everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogFilter {
    /// Words in the commit message, ignoring case.
    pub message: Option<String>,
    /// Part of the author's name or email, ignoring case.
    pub author: Option<String>,
    /// Commits that touched this path (a file or folder).
    pub path: Option<std::path::PathBuf>,
}

impl LogFilter {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message.is_none() && self.author.is_none() && self.path.is_none()
    }
}

/// A commit's full details, for the History view's detail pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitDetails {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    pub author_time: i64,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_time: i64,
    /// The whole message, summary and body.
    pub message: String,
    pub signature: SignatureStatus,
}

/// What Git says about a commit's signature (`%G?`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    Unsigned,
    Good,
    /// Good, but the key isn't trusted or is expired or revoked.
    GoodUntrusted,
    Bad,
    /// Signed, but Git could not check it, such as a missing key.
    Unknown,
}

impl SignatureStatus {
    fn parse(code: &str) -> Self {
        match code.trim() {
            "G" => Self::Good,
            "U" | "X" | "Y" | "R" => Self::GoodUntrusted,
            "B" => Self::Bad,
            "E" => Self::Unknown,
            _ => Self::Unsigned,
        }
    }
}

/// What a history row shows about a commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    pub oid: Oid,
    pub parents: Vec<Oid>,
    pub author_name: String,
    pub author_email: String,
    /// Seconds since the Unix epoch.
    pub author_time: i64,
    pub summary: String,
}

const FIELD: char = '\u{1f}';
const FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s";

fn parse(record: &str) -> Result<CommitInfo> {
    let mut fields = record.splitn(6, FIELD);
    let mut next = || {
        fields
            .next()
            .ok_or_else(|| Error::Parse(format!("git log: {record:?}")))
    };
    let oid = Oid::new(next()?.trim_start_matches('\n'));
    let parents = next()?.split_whitespace().map(Oid::new).collect();
    let author_name = next()?.to_owned();
    let author_email = next()?.to_owned();
    let author_time = next()?
        .parse()
        .map_err(|_| Error::Parse(format!("git log time: {record:?}")))?;
    let summary = next()?.to_owned();
    Ok(CommitInfo {
        oid,
        parents,
        author_name,
        author_email,
        author_time,
        summary,
    })
}

impl Repository {
    /// Up to `limit` commits in topological order, newest first, skipping the
    /// first `skip`. An unborn HEAD has no history.
    pub fn log(&self, scope: &LogScope, skip: usize, limit: usize) -> Result<Vec<CommitInfo>> {
        self.log_filtered(scope, &LogFilter::default(), skip, limit)
    }

    /// Like [`Self::log`], keeping only commits that match `filter`.
    pub fn log_filtered(
        &self,
        scope: &LogScope,
        filter: &LogFilter,
        skip: usize,
        limit: usize,
    ) -> Result<Vec<CommitInfo>> {
        if *scope == LogScope::Head && self.head_is_unborn()? {
            return Ok(Vec::new());
        }
        let mut args = vec![
            "log".to_owned(),
            "-z".to_owned(),
            "--topo-order".to_owned(),
            "--no-color".to_owned(),
            FORMAT.to_owned(),
            format!("--skip={skip}"),
            format!("--max-count={limit}"),
        ];
        if filter.message.is_some() || filter.author.is_some() {
            args.extend(
                ["--regexp-ignore-case", "--fixed-strings", "--all-match"].map(str::to_owned),
            );
        }
        if let Some(message) = &filter.message {
            args.push(format!("--grep={message}"));
        }
        if let Some(author) = &filter.author {
            args.push(format!("--author={author}"));
        }
        match scope {
            LogScope::All => {
                args.extend(["--branches", "--remotes", "--tags", "HEAD"].map(str::to_owned));
                if self.head_is_unborn()? {
                    args.pop();
                }
            }
            LogScope::Head => args.push("HEAD".to_owned()),
            LogScope::Refs(refs) => args.extend(refs.iter().cloned()),
        }
        args.push("--".to_owned());
        let mut args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        if let Some(path) = &filter.path {
            args.push(path.as_os_str().to_owned());
        }
        let output = self.git().run(&self.git().command(args).cwd(self.root()))?;
        output
            .stdout_lossy()
            .split('\0')
            .filter(|record| !record.trim().is_empty())
            .map(parse)
            .collect()
    }
}

impl Repository {
    /// The full details of one commit.
    pub fn commit_details(&self, oid: &Oid) -> Result<CommitDetails> {
        let format = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%cn%x1f%ce%x1f%ct%x1f%G?%x1f%B";
        let output = self.git().run(
            &self
                .git()
                .command(["show", "-s", "--no-color", format, oid.as_str(), "--"])
                .cwd(self.root()),
        )?;
        let text = output.stdout_lossy();
        let fields: Vec<&str> = text.splitn(10, FIELD).collect();
        let [hash, parents, an, ae, at, cn, ce, ct, signature, message] = fields[..] else {
            return Err(Error::Parse(format!(
                "unexpected commit record for {}",
                oid.as_str()
            )));
        };
        let time = |value: &str| {
            value
                .trim()
                .parse()
                .map_err(|_| Error::Parse(format!("bad commit time {value:?}")))
        };
        Ok(CommitDetails {
            oid: Oid::new(hash.trim()),
            parents: parents.split_whitespace().map(Oid::new).collect(),
            author_name: an.to_owned(),
            author_email: ae.to_owned(),
            author_time: time(at)?,
            committer_name: cn.to_owned(),
            committer_email: ce.to_owned(),
            committer_time: time(ct)?,
            message: message.trim_end().to_owned(),
            signature: SignatureStatus::parse(signature),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records_with_odd_summaries() {
        let info = parse(
            "abc\u{1f}p1 p2\u{1f}Ada\u{1f}ada@example.com\u{1f}1700000000\u{1f}Fix: a | b \u{1f} c",
        )
        .unwrap();
        assert_eq!(info.oid, Oid::new("abc"));
        assert_eq!(info.parents, [Oid::new("p1"), Oid::new("p2")]);
        assert_eq!(info.author_time, 1_700_000_000);
        assert_eq!(info.summary, "Fix: a | b \u{1f} c");
        assert!(parse("abc\u{1f}only").is_err());
    }
}
