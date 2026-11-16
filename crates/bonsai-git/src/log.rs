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
        let output = self.git().run(&self.git().command(args).cwd(self.root()))?;
        output
            .stdout_lossy()
            .split('\0')
            .filter(|record| !record.trim().is_empty())
            .map(parse)
            .collect()
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
