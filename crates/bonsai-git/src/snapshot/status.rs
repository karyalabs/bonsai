//! Parses `git status --porcelain=v2 -z --branch --show-stash`.

use std::path::PathBuf;

use super::{Change, ChangeKind, ConflictKind, EntryStatus, Oid};
use crate::{Error, Result, path::path_from_bytes};

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Status {
    /// `None` while HEAD is unborn.
    pub oid: Option<Oid>,
    /// `None` when HEAD is detached.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead_behind: Option<(u32, u32)>,
    pub changes: Vec<Change>,
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Status> {
    let mut status = Status::default();
    let mut fields = bytes.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        match entry.first() {
            Some(b'#') => parse_header(&mut status, entry)?,
            Some(b'1') => status.changes.push(parse_ordinary(entry, None)?),
            Some(b'2') => {
                let original = fields
                    .next()
                    .ok_or_else(|| malformed("rename without its original path"))?;
                status
                    .changes
                    .push(parse_ordinary(entry, Some(path_from_bytes(original)))?);
            }
            Some(b'u') => status.changes.push(parse_unmerged(entry)?),
            Some(b'?') => status.changes.push(simple(entry, ChangeKind::Untracked)?),
            Some(b'!') => status.changes.push(simple(entry, ChangeKind::Ignored)?),
            _ => return Err(malformed(&String::from_utf8_lossy(entry))),
        }
    }
    Ok(status)
}

fn parse_header(status: &mut Status, entry: &[u8]) -> Result<()> {
    let line = String::from_utf8_lossy(entry);
    let mut words = line.splitn(3, ' ');
    let _hash = words.next();
    let key = words.next().unwrap_or_default();
    let value = words.next().unwrap_or_default();
    match key {
        "branch.oid" if value != "(initial)" => status.oid = Some(Oid::new(value)),
        "branch.head" if value != "(detached)" => status.head = Some(value.to_owned()),
        "branch.upstream" => status.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let mut parts = value.split(' ');
            let ahead = parts.next().and_then(|p| p.strip_prefix('+')?.parse().ok());
            let behind = parts.next().and_then(|p| p.strip_prefix('-')?.parse().ok());
            if let (Some(ahead), Some(behind)) = (ahead, behind) {
                status.ahead_behind = Some((ahead, behind));
            } else {
                return Err(malformed(&line));
            }
        }
        _ => {}
    }
    Ok(())
}

/// `1 XY sub mH mI mW hH hI path` or `2 XY sub mH mI mW hH hI Xscore path`.
fn parse_ordinary(entry: &[u8], original_path: Option<PathBuf>) -> Result<Change> {
    let field_count = if original_path.is_some() { 10 } else { 9 };
    let fields: Vec<&[u8]> = entry.splitn(field_count, |b| *b == b' ').collect();
    if fields.len() != field_count {
        return Err(malformed(&String::from_utf8_lossy(entry)));
    }
    let (index, work_tree) = xy(fields[1])?;
    Ok(Change {
        path: path_from_bytes(fields[field_count - 1]),
        original_path,
        kind: ChangeKind::Tracked { index, work_tree },
        submodule: fields[2].first() == Some(&b'S'),
    })
}

/// `u XY sub m1 m2 m3 mW h1 h2 h3 path`.
fn parse_unmerged(entry: &[u8]) -> Result<Change> {
    let fields: Vec<&[u8]> = entry.splitn(11, |b| *b == b' ').collect();
    if fields.len() != 11 {
        return Err(malformed(&String::from_utf8_lossy(entry)));
    }
    let conflict = match fields[1] {
        b"DD" => ConflictKind::BothDeleted,
        b"AU" => ConflictKind::AddedByUs,
        b"UD" => ConflictKind::DeletedByThem,
        b"UA" => ConflictKind::AddedByThem,
        b"DU" => ConflictKind::DeletedByUs,
        b"AA" => ConflictKind::BothAdded,
        b"UU" => ConflictKind::BothModified,
        other => return Err(malformed(&String::from_utf8_lossy(other))),
    };
    Ok(Change {
        path: path_from_bytes(fields[10]),
        original_path: None,
        kind: ChangeKind::Conflicted(conflict),
        submodule: fields[2].first() == Some(&b'S'),
    })
}

fn simple(entry: &[u8], kind: ChangeKind) -> Result<Change> {
    let path = entry
        .get(2..)
        .ok_or_else(|| malformed(&String::from_utf8_lossy(entry)))?;
    Ok(Change {
        path: path_from_bytes(path),
        original_path: None,
        kind,
        submodule: false,
    })
}

fn xy(field: &[u8]) -> Result<(Option<EntryStatus>, Option<EntryStatus>)> {
    let [x, y] = field else {
        return Err(malformed(&String::from_utf8_lossy(field)));
    };
    Ok((entry_status(*x)?, entry_status(*y)?))
}

fn entry_status(code: u8) -> Result<Option<EntryStatus>> {
    Ok(Some(match code {
        b'.' => return Ok(None),
        b'M' => EntryStatus::Modified,
        b'T' => EntryStatus::TypeChanged,
        b'A' => EntryStatus::Added,
        b'D' => EntryStatus::Deleted,
        b'R' => EntryStatus::Renamed,
        b'C' => EntryStatus::Copied,
        other => return Err(malformed(&format!("status code {}", other as char))),
    }))
}

fn malformed(what: &str) -> Error {
    Error::Parse(format!("git status: {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "1111111111111111111111111111111111111111";

    #[test]
    fn parses_headers_and_every_entry_kind() {
        let input = format!(
            "# branch.oid {OID}\0# branch.head main\0# branch.upstream origin/main\0\
             # branch.ab +2 -1\0# stash 3\0\
             1 M. N... 100644 100644 100644 {OID} {OID} src/lib.rs\0\
             1 .D N... 100644 100644 000000 {OID} {OID} gone file.txt\0\
             2 R. N... 100644 100644 100644 {OID} {OID} R100 new.rs\0old.rs\0\
             u UU N... 100644 100644 100644 100644 {OID} {OID} {OID} both.txt\0\
             1 .M S.M. 160000 160000 160000 {OID} {OID} vendor/lib\0\
             ? notes.md\0! target\0"
        );
        let status = parse(input.as_bytes()).unwrap();
        assert_eq!(status.oid, Some(Oid::new(OID)));
        assert_eq!(status.head.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!(status.ahead_behind, Some((2, 1)));

        let c = &status.changes;
        assert_eq!(c.len(), 7);
        assert_eq!(
            c[0].kind,
            ChangeKind::Tracked {
                index: Some(EntryStatus::Modified),
                work_tree: None
            }
        );
        assert_eq!(c[1].path, PathBuf::from("gone file.txt"));
        assert_eq!(c[2].path, PathBuf::from("new.rs"));
        assert_eq!(c[2].original_path, Some(PathBuf::from("old.rs")));
        assert_eq!(
            c[3].kind,
            ChangeKind::Conflicted(ConflictKind::BothModified)
        );
        assert!(c[4].submodule);
        assert_eq!(c[5].kind, ChangeKind::Untracked);
        assert_eq!(c[6].kind, ChangeKind::Ignored);
    }

    #[test]
    fn unborn_and_detached_heads() {
        let unborn = parse(b"# branch.oid (initial)\0# branch.head main\0").unwrap();
        assert_eq!(unborn.oid, None);
        assert_eq!(unborn.head.as_deref(), Some("main"));

        let detached =
            parse(format!("# branch.oid {OID}\0# branch.head (detached)\0").as_bytes()).unwrap();
        assert_eq!(detached.head, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"1 M.\0").is_err());
        assert!(parse(b"2 R. N... 1 1 1 a b R100 only-new\0").is_err());
        assert!(parse(b"zzz\0").is_err());
    }
}
