//! Partial patches: the selected lines of one file's diff, ready for
//! `git apply`, so lines can be staged, unstaged or discarded on their own.

use std::fmt::Write as _;

use crate::{Repository, Result};

/// Which side of the diff a line number counts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineSide {
    /// The old file: removed and context lines.
    Old,
    /// The new file: added and context lines.
    New,
}

/// A line of the diff, by its number on one side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinePosition {
    pub side: LineSide,
    pub line: usize,
}

impl LinePosition {
    #[must_use]
    pub fn new(side: LineSide, line: usize) -> Self {
        Self { side, line }
    }
}

/// What applying a partial patch does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyMode {
    /// Adds the selected work tree changes to the index (from an unstaged
    /// diff).
    Stage,
    /// Takes the selected changes back out of the index (from a staged diff).
    Unstage,
    /// Reverts the selected changes in the work tree (from an unstaged diff).
    /// The index is left alone.
    Discard,
}

impl ApplyMode {
    /// Whether the patch is applied in reverse.
    fn reverse(self) -> bool {
        !matches!(self, Self::Stage)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Context,
    Removed,
    Added,
    /// `\ No newline at end of file`, which belongs to the line before it.
    NoNewline,
}

struct Line<'a> {
    kind: Kind,
    text: &'a str,
    old: usize,
    new: usize,
}

struct Hunk<'a> {
    old_start: usize,
    new_start: usize,
    /// What follows the second `@@`, such as a function name.
    tail: &'a str,
    lines: Vec<Line<'a>>,
}

/// Lines of `text`, each keeping its `\n` (and any `\r` before it).
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split_inclusive('\n')
}

/// Undoes Git's C-style quoting of a path in a patch header.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_owned();
    };
    let mut bytes = Vec::with_capacity(inner.len());
    let mut chars = inner.bytes().peekable();
    while let Some(b) = chars.next() {
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        match chars.next() {
            Some(b'n') => bytes.push(b'\n'),
            Some(b't') => bytes.push(b'\t'),
            Some(d @ b'0'..=b'7') => {
                let mut value = u32::from(d - b'0');
                for _ in 0..2 {
                    if let Some(&next @ b'0'..=b'7') = chars.peek() {
                        value = value * 8 + u32::from(next - b'0');
                        chars.next();
                    }
                }
                bytes.push(u8::try_from(value).unwrap_or(b'?'));
            }
            Some(other) => bytes.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The path a `---` or `+++` header names, without its `a/` or `b/`.
fn header_path(line: &str, prefix: &str) -> Option<String> {
    let rest = line.strip_prefix(prefix)?.trim_end_matches(['\n', '\r']);
    let rest = rest.split('\t').next().unwrap_or(rest);
    let path = unquote(rest);
    if path == "/dev/null" {
        return None;
    }
    path.get(2..).map(str::to_owned)
}

/// One file's section of `patch`: its header lines and its hunks' text.
fn file_section<'a>(patch: &'a str, file: &str) -> Option<(String, &'a str)> {
    let mut sections = Vec::new();
    let mut start = None;
    let mut offset = 0;
    for line in lines(patch) {
        if line.starts_with("diff --git ") {
            if let Some(start) = start {
                sections.push(&patch[start..offset]);
            }
            start = Some(offset);
        }
        offset += line.len();
    }
    if let Some(start) = start {
        sections.push(&patch[start..]);
    }
    sections.into_iter().find_map(|section| {
        let mut header = String::new();
        let mut names = Vec::new();
        let mut body_at = None;
        let mut at = 0;
        for line in lines(section) {
            if line.starts_with("@@") {
                body_at = Some(at);
                break;
            }
            names.extend(header_path(line, "--- ").or_else(|| header_path(line, "+++ ")));
            header.push_str(line);
            at += line.len();
        }
        (names.iter().any(|name| name == file)).then_some(())?;
        Some((header, &section[body_at?..]))
    })
}

fn parse_range(text: &str) -> Option<usize> {
    let start = text.split(',').next()?;
    start.parse().ok()
}

fn parse_hunks(body: &str) -> Option<Vec<Hunk<'_>>> {
    let mut hunks: Vec<Hunk<'_>> = Vec::new();
    let (mut old, mut new) = (0, 0);
    for line in lines(body) {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let (ranges, tail) = rest.split_once(" @@")?;
            let (old_range, new_range) = ranges.split_once(" +")?;
            old = parse_range(old_range)?;
            new = parse_range(new_range)?;
            hunks.push(Hunk {
                old_start: old,
                new_start: new,
                tail,
                lines: Vec::new(),
            });
            continue;
        }
        let hunk = hunks.last_mut()?;
        let (kind, text) = match line.as_bytes().first() {
            Some(b' ') => (Kind::Context, &line[1..]),
            Some(b'-') => (Kind::Removed, &line[1..]),
            Some(b'+') => (Kind::Added, &line[1..]),
            Some(b'\\') => (Kind::NoNewline, line),
            // An empty context line from a tool that trims trailing spaces.
            Some(b'\n' | b'\r') => (Kind::Context, line),
            _ => return None,
        };
        hunk.lines.push(Line {
            kind,
            text,
            old,
            new,
        });
        match kind {
            Kind::Context => {
                old += 1;
                new += 1;
            }
            Kind::Removed => old += 1,
            Kind::Added => new += 1,
            Kind::NoNewline => {}
        }
    }
    Some(hunks)
}

fn matches(line: &Line<'_>, position: LinePosition) -> bool {
    match position.side {
        LineSide::Old => {
            matches!(line.kind, Kind::Context | Kind::Removed) && line.old == position.line
        }
        LineSide::New => {
            matches!(line.kind, Kind::Context | Kind::Added) && line.new == position.line
        }
    }
}

/// The first and last changed line of each hunk of `file` in `patch`, in
/// diff order, for acting on a whole hunk with [`partial_patch`]. Empty when
/// the file isn't in the patch or has no hunks.
#[must_use]
pub fn hunk_changes(patch: &str, file: &str) -> Vec<(LinePosition, LinePosition)> {
    let Some((_, body)) = file_section(patch, file) else {
        return Vec::new();
    };
    let position = |line: &Line<'_>| match line.kind {
        Kind::Removed => Some(LinePosition::new(LineSide::Old, line.old)),
        Kind::Added => Some(LinePosition::new(LineSide::New, line.new)),
        Kind::Context | Kind::NoNewline => None,
    };
    parse_hunks(body)
        .unwrap_or_default()
        .iter()
        .filter_map(|hunk| {
            let first = hunk.lines.iter().find_map(position)?;
            let last = hunk.lines.iter().rev().find_map(position)?;
            Some((first, last))
        })
        .collect()
}

/// Builds a patch of only the changed lines between `from` and `to` (in the
/// order the diff lists them, either way round) in `file`'s section of
/// `patch`. Unselected removals become context and unselected additions
/// drop out (the other way round for [`ApplyMode::Stage`]'s reverse modes),
/// so the result applies with [`Repository::apply_patch`] in `mode`.
///
/// Returns `None` when the file isn't in the patch, has no hunks (such as a
/// binary file), or the selection holds no changed line.
#[must_use]
pub fn partial_patch(
    patch: &str,
    file: &str,
    from: LinePosition,
    to: LinePosition,
    mode: ApplyMode,
) -> Option<String> {
    let (header, body) = file_section(patch, file)?;
    let hunks = parse_hunks(body)?;
    let index_of = |position: LinePosition| {
        hunks
            .iter()
            .flat_map(|hunk| hunk.lines.iter())
            .position(|line| matches(line, position))
    };
    let (a, b) = (index_of(from)?, index_of(to)?);
    let selected = a.min(b)..=a.max(b);
    let reverse = mode.reverse();

    let mut out = header;
    let mut any = false;
    let mut index = 0;
    // How far the new side has moved from the old, from earlier hunks.
    let mut shift: isize = 0;
    for hunk in &hunks {
        let mut body = String::new();
        let (mut old_count, mut new_count) = (0_usize, 0_usize);
        let mut changed = false;
        // Whether the line a `\ No newline` marker would follow was kept.
        let mut last_kept = false;
        for line in &hunk.lines {
            let chosen = selected.contains(&index);
            index += 1;
            let keep_as = match (line.kind, chosen) {
                (Kind::NoNewline, _) => {
                    if last_kept {
                        body.push_str(line.text);
                    }
                    continue;
                }
                (Kind::Context, _) => Some(' '),
                (Kind::Removed, true) => Some('-'),
                (Kind::Added, true) => Some('+'),
                // Going forward, an unselected removal stays in the file and
                // an unselected addition never arrives; in reverse, the
                // target already has the additions and lacks the removals.
                (Kind::Removed, false) => (!reverse).then_some(' '),
                (Kind::Added, false) => reverse.then_some(' '),
            };
            last_kept = keep_as.is_some();
            let Some(prefix) = keep_as else { continue };
            match prefix {
                ' ' => {
                    old_count += 1;
                    new_count += 1;
                }
                '-' => {
                    old_count += 1;
                    changed = true;
                }
                _ => {
                    new_count += 1;
                    changed = true;
                }
            }
            body.push(prefix);
            body.push_str(line.text);
        }
        if !changed {
            continue;
        }
        any = true;
        // `git apply` finds the side it applies to at its original place:
        // the old side going forward, the new side in reverse. The other
        // side's start follows from the hunks kept before this one.
        let (old_start, new_start) = if reverse {
            let new_start = hunk.new_start;
            (
                other_start(new_start, new_count, old_count, -shift),
                new_start,
            )
        } else {
            let old_start = hunk.old_start;
            (
                old_start,
                other_start(old_start, old_count, new_count, shift),
            )
        };
        shift += isize::try_from(new_count).unwrap_or(0) - isize::try_from(old_count).unwrap_or(0);
        let _ = write!(
            out,
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@{}",
            hunk.tail
        );
        out.push_str(&body);
    }
    any.then_some(out)
}

/// The start on one side of a hunk from the start on the other. A side with
/// no lines names the line before the hunk; otherwise its first line.
fn other_start(start: usize, count: usize, other_count: usize, shift: isize) -> usize {
    let before = if count == 0 {
        start
    } else {
        start.saturating_sub(1)
    };
    let before = before.saturating_add_signed(shift);
    if other_count == 0 { before } else { before + 1 }
}

impl Repository {
    /// Applies a patch from [`partial_patch`] built for the same `mode`.
    pub fn apply_patch(&self, patch: &str, mode: ApplyMode) -> Result<()> {
        let mut args = vec!["apply", "--whitespace=nowarn", "--recount"];
        match mode {
            ApplyMode::Stage => args.push("--cached"),
            ApplyMode::Unstage => args.extend(["--cached", "--reverse"]),
            ApplyMode::Discard => args.push("--reverse"),
        }
        args.push("-");
        let git = self.git();
        git.run(
            &git.command(args)
                .cwd(self.work_tree()?)
                .stdin(patch.as_bytes().to_vec()),
        )
        .map(drop)
    }
}
