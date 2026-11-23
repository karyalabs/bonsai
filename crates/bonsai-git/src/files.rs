//! The files Git tracks, for the Files explorer.

use std::path::PathBuf;

use crate::{Error, Repository, Result};

/// A line that matched a search of the files' contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepHit {
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    pub text: String,
}

/// What a search of the files' contents found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GrepResults {
    pub hits: Vec<GrepHit>,
    /// More lines matched than were kept.
    pub truncated: bool,
}

/// The longest stretch of a matching line kept, so minified files stay
/// readable.
const MAX_HIT_CHARS: usize = 400;

/// Reads `git grep -n -z` output; `rev` is stripped from the front of
/// paths, as Git prefixes them with it.
fn parse_grep(output: &[u8], rev: Option<&str>, limit: usize) -> GrepResults {
    let mut results = GrepResults::default();
    let prefix = rev.map(|rev| format!("{rev}:"));
    for record in output.split(|&b| b == b'\n') {
        let mut fields = record.splitn(3, |&b| b == 0);
        let (Some(path), Some(line), Some(text)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(line) = std::str::from_utf8(line).ok().and_then(|l| l.parse().ok()) else {
            continue;
        };
        if results.hits.len() == limit {
            results.truncated = true;
            break;
        }
        let path = match &prefix {
            Some(prefix) => path.strip_prefix(prefix.as_bytes()).unwrap_or(path),
            None => path,
        };
        let text = String::from_utf8_lossy(text);
        let text = text.trim_end_matches('\r');
        results.hits.push(GrepHit {
            path: crate::purge::bytes_to_path(path),
            line,
            text: text.chars().take(MAX_HIT_CHARS).collect(),
        });
    }
    results
}

impl Repository {
    /// Every path in the index, sorted as Git sorts them. Paths deleted in
    /// the work tree but not yet staged are still listed.
    ///
    /// # Errors
    /// When `git ls-files` fails.
    pub fn tracked_files(&self) -> Result<Vec<PathBuf>> {
        self.files_at(None)
    }

    /// Every file in `rev`'s tree, or in the index for `None`.
    ///
    /// # Errors
    /// When Git can't list them, such as for an unknown revision.
    pub fn files_at(&self, rev: Option<&str>) -> Result<Vec<PathBuf>> {
        let args: Vec<&str> = match rev {
            Some(rev) => vec!["ls-tree", "-r", "-z", "--name-only", "--full-tree", rev],
            None => vec!["ls-files", "-z", "--full-name"],
        };
        let output = self.git().run(&self.git().command(args).cwd(self.root()))?;
        Ok(output
            .stdout
            .split(|&b| b == 0)
            .filter(|p| !p.is_empty())
            .map(crate::purge::bytes_to_path)
            .collect())
    }

    /// Lines of tracked files containing `query`, in the work tree or in
    /// `rev`'s tree. A query in lowercase matches any case. Binary files
    /// are skipped. Keeps the first `limit` lines.
    ///
    /// # Errors
    /// When Git can't search, such as for an unknown revision.
    pub fn grep(&self, query: &str, rev: Option<&str>, limit: usize) -> Result<GrepResults> {
        if query.is_empty() {
            return Ok(GrepResults::default());
        }
        let mut args = vec!["grep", "-n", "-z", "-I", "--full-name", "--fixed-strings"];
        if !query.chars().any(char::is_uppercase) {
            args.push("--ignore-case");
        }
        args.extend(["-e", query]);
        if let Some(rev) = rev {
            args.push(rev);
        }
        args.push("--");
        let output = self
            .git()
            .run_unchecked(&self.git().command(args).cwd(self.root()))?;
        // 1: nothing matched.
        match output.status.code() {
            Some(0) => Ok(parse_grep(&output.stdout, rev, limit)),
            Some(1) => Ok(GrepResults::default()),
            _ => Err(Error::Refused(format!(
                "Git couldn't search: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_grep_output_with_and_without_a_revision() {
        let output = b"src/main.rs\x002\x00    println!(\"Hello\");\nREADME.md\x0010\x00Hello\r\n";
        let results = parse_grep(output, None, 10);
        assert_eq!(results.hits.len(), 2);
        assert_eq!(results.hits[0].path, PathBuf::from("src/main.rs"));
        assert_eq!(results.hits[0].line, 2);
        assert_eq!(results.hits[1].text, "Hello");
        assert!(!results.truncated);

        let at_rev = parse_grep(
            b"HEAD:a.txt\x001\x00x\nHEAD:b.txt\x001\x00x\n",
            Some("HEAD"),
            1,
        );
        assert_eq!(at_rev.hits[0].path, PathBuf::from("a.txt"));
        assert!(at_rev.truncated);
    }
}
