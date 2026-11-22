//! Reading a conflicted file's marked regions and writing it back with each
//! region resolved, so conflicts can be resolved one at a time.

use std::path::Path;

use crate::{Error, InProgress, Repository, Result, Side};

/// A run of a conflicted file: text both sides agree on, or a conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Lines both sides agree on, each with its line ending.
    Common(Vec<String>),
    Conflict(Conflict),
}

/// One marked region: `<<<<<<<` ours `|||||||` base `=======` theirs
/// `>>>>>>>`. The base is there with `merge.conflictStyle` diff3 or zdiff3.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Conflict {
    /// What the markers say about each side, such as `HEAD` or a branch.
    pub ours_label: String,
    pub theirs_label: String,
    pub base_label: String,
    pub ours: Vec<String>,
    pub base: Option<Vec<String>>,
    pub theirs: Vec<String>,
}

/// How a conflict is resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    Ours,
    Theirs,
    /// Ours, then theirs.
    Both,
    /// Theirs, then ours.
    BothTheirsFirst,
    /// Neither side.
    Neither,
}

impl Conflict {
    /// The lines that replace the region under `resolution`.
    #[must_use]
    pub fn resolved(&self, resolution: &Resolution) -> Vec<String> {
        match resolution {
            Resolution::Ours => self.ours.clone(),
            Resolution::Theirs => self.theirs.clone(),
            Resolution::Both => join_lines(&self.ours, &self.theirs),
            Resolution::BothTheirsFirst => join_lines(&self.theirs, &self.ours),
            Resolution::Neither => Vec::new(),
        }
    }

    /// The region as Git wrote it, markers and all.
    fn marked(&self, ending: &str) -> Vec<String> {
        let mut lines = vec![marker('<', &self.ours_label, ending)];
        lines.extend(with_ending(&self.ours, ending));
        if let Some(base) = &self.base {
            lines.push(marker('|', &self.base_label, ending));
            lines.extend(with_ending(base, ending));
        }
        lines.push(marker('=', "", ending));
        lines.extend(with_ending(&self.theirs, ending));
        lines.push(marker('>', &self.theirs_label, ending));
        lines
    }
}

fn marker(c: char, label: &str, ending: &str) -> String {
    let mut line: String = std::iter::repeat_n(c, 7).collect();
    if !label.is_empty() {
        line.push(' ');
        line.push_str(label);
    }
    line.push_str(ending);
    line
}

/// Lines with a line ending, so a side whose last line has none still ends
/// before the next marker or side.
fn with_ending(lines: &[String], ending: &str) -> Vec<String> {
    lines
        .iter()
        .map(|l| {
            if l.ends_with('\n') {
                l.clone()
            } else {
                format!("{l}{ending}")
            }
        })
        .collect()
}

fn join_lines(first: &[String], second: &[String]) -> Vec<String> {
    let ending = first
        .iter()
        .chain(second)
        .find_map(|l| l.ends_with("\r\n").then_some("\r\n"))
        .unwrap_or("\n");
    let mut lines = with_ending(first, ending);
    lines.extend(second.iter().cloned());
    lines
}

/// What a marker line starts, if it is one: `<`, `|`, `=` or `>`, with the
/// rest of the line as its label.
fn marker_kind(line: &str) -> Option<(char, String)> {
    let first = line.chars().next()?;
    if !matches!(first, '<' | '|' | '=' | '>') {
        return None;
    }
    let body = line.trim_end_matches(['\n', '\r']);
    let run = body.chars().take_while(|&c| c == first).count();
    if run != 7 {
        return None;
    }
    let rest = &body[run..];
    match first {
        '=' if rest.is_empty() => Some(('=', String::new())),
        '=' => None,
        _ if rest.is_empty() || rest.starts_with(' ') => Some((first, rest.trim().to_owned())),
        _ => None,
    }
}

/// Splits a file into common text and conflicts. A file with unbalanced
/// markers is all common text, since resolving it by region could lose
/// lines.
#[must_use]
pub fn parse_conflicts(text: &str) -> Vec<Segment> {
    #[derive(PartialEq)]
    enum State {
        Common,
        Ours,
        Base,
        Theirs,
    }
    let mut segments = Vec::new();
    let mut common: Vec<String> = Vec::new();
    let mut current = Conflict::default();
    let mut state = State::Common;
    for line in text.split_inclusive('\n') {
        let kind = marker_kind(line);
        match (&state, kind) {
            (State::Common, Some(('<', label))) => {
                if !common.is_empty() {
                    segments.push(Segment::Common(std::mem::take(&mut common)));
                }
                current = Conflict {
                    ours_label: label,
                    ..Conflict::default()
                };
                state = State::Ours;
            }
            (State::Ours, Some(('|', label))) => {
                current.base = Some(Vec::new());
                current.base_label = label;
                state = State::Base;
            }
            (State::Ours | State::Base, Some(('=', _))) => state = State::Theirs,
            (State::Theirs, Some(('>', label))) => {
                current.theirs_label = label;
                segments.push(Segment::Conflict(std::mem::take(&mut current)));
                state = State::Common;
            }
            (State::Common, _) => common.push(line.to_owned()),
            (State::Ours, _) => current.ours.push(line.to_owned()),
            (State::Base, _) => {
                if let Some(base) = current.base.as_mut() {
                    base.push(line.to_owned());
                }
            }
            (State::Theirs, _) => current.theirs.push(line.to_owned()),
        }
    }
    if state != State::Common {
        return vec![Segment::Common(
            text.split_inclusive('\n').map(str::to_owned).collect(),
        )];
    }
    if !common.is_empty() {
        segments.push(Segment::Common(common));
    }
    segments
}

/// The file's text with each conflict resolved as `resolutions` says, in
/// order. A conflict without a resolution keeps its markers.
#[must_use]
pub fn resolve_conflicts(segments: &[Segment], resolutions: &[Option<Resolution>]) -> String {
    let ending = segments
        .iter()
        .find_map(|s| match s {
            Segment::Common(lines) => lines.iter().find(|l| l.ends_with('\n')),
            Segment::Conflict(c) => c.ours.iter().find(|l| l.ends_with('\n')),
        })
        .map_or("\n", |l| if l.ends_with("\r\n") { "\r\n" } else { "\n" });
    let mut out = String::new();
    let mut index = 0;
    for segment in segments {
        match segment {
            Segment::Common(lines) => lines.iter().for_each(|l| out.push_str(l)),
            Segment::Conflict(conflict) => {
                let lines = match resolutions.get(index).cloned().flatten() {
                    Some(resolution) => conflict.resolved(&resolution),
                    None => conflict.marked(ending),
                };
                for line in lines {
                    out.push_str(&line);
                }
                index += 1;
            }
        }
    }
    out
}

/// What each side means to a person during `operation`, in a word or two.
#[must_use]
pub fn side_name(side: Side, operation: Option<InProgress>) -> &'static str {
    match (side, operation) {
        (Side::Ours, Some(InProgress::Rebase)) => "Upstream",
        (Side::Theirs, Some(InProgress::Rebase)) => "Your Commit",
        (Side::Ours, _) => "Current",
        (Side::Theirs, _) => "Incoming",
    }
}

impl Repository {
    /// The conflicted file at `path` in the work tree, split into regions.
    ///
    /// # Errors
    /// When the file can't be read or isn't text.
    pub fn read_conflicts(&self, path: &Path) -> Result<Vec<Segment>> {
        let bytes = std::fs::read(self.root().join(path))
            .map_err(|e| Error::Refused(format!("Couldn't read {}: {e}", path.display())))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| Error::Refused(format!("{} isn't text.", path.display())))?;
        Ok(parse_conflicts(&text))
    }

    /// Writes the file with its conflicts resolved, and marks it resolved
    /// when every one is.
    ///
    /// # Errors
    /// When the file can't be written or staged.
    pub fn write_resolution(
        &self,
        path: &Path,
        segments: &[Segment],
        resolutions: &[Option<Resolution>],
    ) -> Result<()> {
        let text = resolve_conflicts(segments, resolutions);
        std::fs::write(self.root().join(path), text)
            .map_err(|e| Error::Refused(format!("Couldn't write {}: {e}", path.display())))?;
        let conflicts = segments
            .iter()
            .filter(|s| matches!(s, Segment::Conflict(_)))
            .count();
        let resolved = resolutions.iter().take(conflicts).flatten().count();
        if resolved == conflicts {
            self.mark_resolved(&[path.to_path_buf()])?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "fn main() {\n<<<<<<< HEAD\n    run(1);\n=======\n    run(2);\n    log();\n>>>>>>> feature\n}\n<<<<<<< HEAD\nours\n||||||| base\nbase\n=======\ntheirs\n>>>>>>> feature\n";

    #[test]
    fn splits_common_text_and_conflicts_with_their_base() {
        let segments = parse_conflicts(FILE);
        assert_eq!(segments.len(), 4);
        assert_eq!(segments[0], Segment::Common(vec!["fn main() {\n".into()]));
        let Segment::Conflict(first) = &segments[1] else {
            panic!("a conflict");
        };
        assert_eq!(first.ours_label, "HEAD");
        assert_eq!(first.theirs_label, "feature");
        assert_eq!(first.ours, ["    run(1);\n"]);
        assert_eq!(first.theirs, ["    run(2);\n", "    log();\n"]);
        assert_eq!(first.base, None);
        let Segment::Conflict(second) = &segments[3] else {
            panic!("a conflict");
        };
        assert_eq!(second.base.as_deref(), Some(&["base\n".to_owned()][..]));
    }

    #[test]
    fn writes_each_resolution_and_keeps_unresolved_markers() {
        let segments = parse_conflicts(FILE);
        assert_eq!(resolve_conflicts(&segments, &[]), FILE, "round-trips");
        let text = resolve_conflicts(
            &segments,
            &[Some(Resolution::Theirs), Some(Resolution::Both)],
        );
        assert_eq!(
            text,
            "fn main() {\n    run(2);\n    log();\n}\nours\ntheirs\n"
        );
        let text = resolve_conflicts(&segments, &[Some(Resolution::BothTheirsFirst), None]);
        assert!(
            text.starts_with(
                "fn main() {\n    run(2);\n    log();\n    run(1);\n}\n<<<<<<< HEAD\n"
            )
        );
        let text = resolve_conflicts(
            &segments,
            &[Some(Resolution::Neither), Some(Resolution::Ours)],
        );
        assert_eq!(text, "fn main() {\n}\nours\n");
    }

    #[test]
    fn keeps_crlf_and_a_side_without_a_final_newline() {
        let text = "a\r\n<<<<<<< HEAD\r\nx\r\n=======\r\ny\r\n>>>>>>> b\r\n";
        let segments = parse_conflicts(text);
        assert_eq!(resolve_conflicts(&segments, &[]), text);
        assert_eq!(
            resolve_conflicts(&segments, &[Some(Resolution::Both)]),
            "a\r\nx\r\ny\r\n"
        );
        let unended = "<<<<<<< HEAD\nx\n=======\ny";
        assert_eq!(parse_conflicts(unended).len(), 1, "unbalanced: all common");
        let conflict = Conflict {
            ours: vec!["x".into()],
            theirs: vec!["y\n".into()],
            ..Conflict::default()
        };
        assert_eq!(conflict.resolved(&Resolution::Both), ["x\n", "y\n"]);
    }

    #[test]
    fn ignores_lines_that_only_look_like_markers() {
        let text = "<<<<<<<< eight\n======= not\n=======\n>>>>>>>x\n";
        assert_eq!(parse_conflicts(text).len(), 1);
        assert!(matches!(parse_conflicts(text)[0], Segment::Common(_)));
    }
}
