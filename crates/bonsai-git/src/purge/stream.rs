//! The fast-export stream filter at the heart of a purge (ADR 4).
//!
//! It drops file changes for purged paths, prunes commits that become empty
//! (keeping merges and commits that were empty to begin with), and rewrites
//! the parents and ref targets that pointed at pruned commits.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::pattern::{PurgeTarget, unquote};

/// The filtered stream and what changed.
#[derive(Debug, Default)]
pub(crate) struct Filtered {
    pub output: Vec<u8>,
    pub commits: usize,
    pub pruned: usize,
    /// Each commit's original ID and the mark (or ID) that stands for it in
    /// the new history; `None` when nothing does.
    pub originals: Vec<(String, Option<String>)>,
    /// Refs that end up with no commit and must be deleted.
    pub emptied_refs: Vec<String>,
    /// Blob IDs the dropped changes referred to.
    pub blobs: BTreeSet<String>,
    /// Every purged path seen, as bytes.
    pub paths: BTreeSet<Vec<u8>>,
}

struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lines<'a> {
    fn peek(&self) -> Option<&'a [u8]> {
        if self.pos >= self.data.len() {
            return None;
        }
        let rest = &self.data[self.pos..];
        Some(
            rest.iter()
                .position(|&b| b == b'\n')
                .map_or(rest, |n| &rest[..n]),
        )
    }

    fn next(&mut self) -> Option<&'a [u8]> {
        let line = self.peek()?;
        self.pos = (self.pos + line.len() + 1).min(self.data.len());
        Some(line)
    }

    /// Reads the body of a `data <n>` line, which follows it byte for byte.
    fn data(&mut self, header: &[u8]) -> Result<&'a [u8], String> {
        let count = std::str::from_utf8(&header[b"data ".len()..])
            .ok()
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| format!("unsupported data line {}", lossy(header)))?;
        let end = self
            .pos
            .checked_add(count)
            .filter(|&end| end <= self.data.len())
            .ok_or("data block runs past the end of the stream")?;
        let body = &self.data[self.pos..end];
        self.pos = end;
        // The LF after a body that does not end in one is optional.
        if !body.ends_with(b"\n") && self.data.get(end) == Some(&b'\n') {
            self.pos += 1;
        }
        Ok(body)
    }

    /// Skips the optional blank line that ends a command.
    fn end_command(&mut self) {
        if self.peek() == Some(b"") {
            self.next();
        }
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn push_line(out: &mut Vec<u8>, parts: &[&[u8]]) {
    for part in parts {
        out.extend_from_slice(part);
    }
    out.push(b'\n');
}

struct Filter<'t> {
    targets: &'t [PurgeTarget],
    /// What each pruned commit's mark now resolves to.
    remap: HashMap<String, Option<String>>,
    /// Whether each ref last pointed at something.
    refs: BTreeMap<String, bool>,
    result: Filtered,
}

impl Filter<'_> {
    fn resolve(&self, reference: &str) -> Option<String> {
        match self.remap.get(reference) {
            Some(target) => target.clone(),
            None => Some(reference.to_owned()),
        }
    }

    fn purged(&mut self, path_field: &[u8]) -> Result<bool, String> {
        let path = unquote(path_field).ok_or_else(|| format!("bad path {}", lossy(path_field)))?;
        if self.targets.iter().any(|t| t.matches(&path)) {
            self.result.paths.insert(path);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Whether a file change line survives.
    fn keep_change(&mut self, line: &[u8]) -> Result<bool, String> {
        if let Some(rest) = line.strip_prefix(b"M ") {
            // M <mode> <dataref> <path>
            let mut fields = rest.splitn(3, |&b| b == b' ');
            let (Some(_mode), Some(dataref), Some(path)) =
                (fields.next(), fields.next(), fields.next())
            else {
                return Err(format!("bad change {}", lossy(line)));
            };
            if self.purged(path)? {
                if !dataref.starts_with(b":") {
                    self.result.blobs.insert(lossy(dataref));
                }
                return Ok(false);
            }
            Ok(true)
        } else if let Some(path) = line.strip_prefix(b"D ") {
            Ok(!self.purged(path)?)
        } else if line == b"deleteall" || line.starts_with(b"N ") {
            Ok(true)
        } else {
            // Copies and renames only appear with -C/-M, which Bonsai never passes.
            Err(format!("unsupported change {}", lossy(line)))
        }
    }

    fn reset(&mut self, lines: &mut Lines<'_>, line: &[u8]) {
        let name = lossy(&line[b"reset ".len()..]);
        let target = match lines.peek().and_then(|l| l.strip_prefix(b"from ")) {
            Some(from) => {
                lines.next();
                self.resolve(&lossy(from))
            }
            None => None,
        };
        lines.end_command();
        self.emit_reset(&name, target.as_deref());
    }

    fn emit_reset(&mut self, name: &str, target: Option<&str>) {
        let out = &mut self.result.output;
        push_line(out, &[b"reset ", name.as_bytes()]);
        if let Some(target) = target {
            push_line(out, &[b"from ", target.as_bytes()]);
        }
        out.push(b'\n');
        self.refs.insert(name.to_owned(), target.is_some());
    }

    fn commit(&mut self, lines: &mut Lines<'_>, line: &[u8]) -> Result<(), String> {
        self.result.commits += 1;
        let name = lossy(&line[b"commit ".len()..]);
        let mut mark = None;
        let mut original = None;
        // Header lines and the message, copied as they are.
        let mut header = Vec::new();
        loop {
            let line = lines.next().ok_or("commit ends early")?;
            if let Some(m) = line.strip_prefix(b"mark ") {
                mark = Some(lossy(m));
            } else if let Some(oid) = line.strip_prefix(b"original-oid ") {
                original = Some(lossy(oid));
            }
            push_line(&mut header, &[line]);
            if line.starts_with(b"data ") {
                let body = lines.data(line)?;
                header.extend_from_slice(body);
                break;
            }
            if line.starts_with(b"gpgsig ") {
                // A signature carries its own data block.
                let data = lines.next().ok_or("signature ends early")?;
                push_line(&mut header, &[data]);
                header.extend_from_slice(lines.data(data)?);
            }
        }
        // A message without a final newline still ends at its byte count.
        if !header.ends_with(b"\n") {
            header.push(b'\n');
        }

        let mut parents = Vec::new();
        let mut original_parents = 0;
        let mut first_parent_vanished = false;
        while let Some(line) = lines.peek() {
            let reference = if let Some(from) = line.strip_prefix(b"from ") {
                from
            } else if let Some(merge) = line.strip_prefix(b"merge ") {
                merge
            } else {
                break;
            };
            lines.next();
            original_parents += 1;
            match self.resolve(&lossy(reference)) {
                Some(parent) => {
                    if !parents.contains(&parent) {
                        parents.push(parent);
                    }
                }
                None => first_parent_vanished |= original_parents == 1,
            }
        }

        let mut changes = Vec::new();
        let mut had_changes = false;
        while let Some(line) = lines.peek() {
            if line.is_empty() {
                lines.next();
                break;
            }
            if line.starts_with(b"commit ")
                || line.starts_with(b"reset ")
                || line.starts_with(b"tag ")
                || line == b"done"
            {
                break;
            }
            lines.next();
            had_changes = true;
            if self.keep_change(line)? {
                push_line(&mut changes, &[line]);
            }
        }

        let prune =
            changes.is_empty() && parents.len() <= 1 && (had_changes || original_parents > 1);
        if prune {
            self.result.pruned += 1;
            let target = parents.first().cloned();
            if let Some(mark) = mark {
                self.remap.insert(mark, target.clone());
            }
            if let Some(original) = original {
                self.result.originals.push((original, target.clone()));
            }
            self.emit_reset(&name, target.as_deref());
            return Ok(());
        }

        if let Some(original) = original {
            self.result.originals.push((original, mark.clone()));
        }
        let out = &mut self.result.output;
        if parents.is_empty() && original_parents > 0 {
            // Every parent vanished: clear the branch so this becomes a root.
            push_line(out, &[b"reset ", name.as_bytes()]);
        }
        push_line(out, &[b"commit ", name.as_bytes()]);
        out.extend_from_slice(&header);
        for (n, parent) in parents.iter().enumerate() {
            let keyword: &[u8] = if n == 0 { b"from " } else { b"merge " };
            push_line(out, &[keyword, parent.as_bytes()]);
        }
        if first_parent_vanished && !parents.is_empty() {
            // The changes were relative to a parent whose filtered tree is
            // empty; start from an empty tree instead of the new first parent.
            push_line(out, &[b"deleteall"]);
        }
        out.extend_from_slice(&changes);
        out.push(b'\n');
        self.refs.insert(name, true);
        Ok(())
    }

    fn tag(&mut self, lines: &mut Lines<'_>, line: &[u8]) -> Result<(), String> {
        let name = lossy(&line[b"tag ".len()..]);
        let mut body = Vec::new();
        let mut target = None;
        let mut mark = None;
        loop {
            let line = lines.next().ok_or("tag ends early")?;
            if let Some(from) = line.strip_prefix(b"from ") {
                target = self.resolve(&lossy(from));
                if let Some(t) = &target {
                    push_line(&mut body, &[b"from ", t.as_bytes()]);
                }
                continue;
            }
            if let Some(m) = line.strip_prefix(b"mark ") {
                mark = Some(lossy(m));
            }
            push_line(&mut body, &[line]);
            if line.starts_with(b"data ") {
                body.extend_from_slice(lines.data(line)?);
                break;
            }
        }
        lines.end_command();
        let reference = format!("refs/tags/{name}");
        if target.is_none() {
            // The tagged commit vanished, so the tag goes too.
            if let Some(mark) = mark {
                self.remap.insert(mark, None);
            }
            self.refs.insert(reference, false);
            return Ok(());
        }
        let out = &mut self.result.output;
        push_line(out, &[b"tag ", name.as_bytes()]);
        out.extend_from_slice(&body);
        if !out.ends_with(b"\n") {
            out.push(b'\n');
        }
        out.push(b'\n');
        self.refs.insert(reference, true);
        Ok(())
    }
}

/// Filters a `git fast-export --no-data --show-original-ids` stream.
pub(crate) fn filter(stream: &[u8], targets: &[PurgeTarget]) -> Result<Filtered, String> {
    let mut filter = Filter {
        targets,
        remap: HashMap::new(),
        refs: BTreeMap::new(),
        result: Filtered::default(),
    };
    let mut lines = Lines {
        data: stream,
        pos: 0,
    };
    let mut done = false;
    while let Some(line) = lines.next() {
        if line.starts_with(b"commit ") {
            filter.commit(&mut lines, line)?;
        } else if line.starts_with(b"reset ") {
            filter.reset(&mut lines, line);
        } else if line.starts_with(b"tag ") {
            filter.tag(&mut lines, line)?;
        } else if line == b"done" {
            push_line(&mut filter.result.output, &[line]);
            done = true;
            break;
        } else if line.starts_with(b"feature ") || line.is_empty() {
            push_line(&mut filter.result.output, &[line]);
        } else {
            return Err(format!("unsupported command {}", lossy(line)));
        }
    }
    if !done {
        return Err("the stream ended without `done`".to_owned());
    }
    filter.result.emptied_refs = filter
        .refs
        .into_iter()
        .filter_map(|(name, alive)| (!alive).then_some(name))
        .collect();
    Ok(filter.result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> Vec<PurgeTarget> {
        vec![PurgeTarget::Path(".env".into())]
    }

    fn run(stream: &str) -> Filtered {
        filter(stream.as_bytes(), &env()).unwrap()
    }

    fn text(filtered: &Filtered) -> String {
        String::from_utf8(filtered.output.clone()).unwrap()
    }

    const HEAD: &str = "author A <a@b> 1 +0000\ncommitter A <a@b> 1 +0000\n";

    #[test]
    fn drops_changes_and_prunes_commits_that_become_empty() {
        let stream = format!(
            "feature done\nreset refs/heads/main\n\
             commit refs/heads/main\nmark :1\noriginal-oid aaa\n{HEAD}data 5\nroot\nM 100644 b1 README\n\n\
             commit refs/heads/main\nmark :2\noriginal-oid bbb\n{HEAD}data 8\nadd env\nfrom :1\nM 100644 secret .env\n\n\
             commit refs/heads/main\nmark :3\noriginal-oid ccc\n{HEAD}data 7\nmore\nx\nfrom :2\nM 100644 b2 app\nD .env\n\n\
             commit refs/heads/main\nmark :4\noriginal-oid ddd\n{HEAD}data 6\nempty\nfrom :3\n\n\
             done\n"
        );
        let filtered = run(&stream);
        let out = text(&filtered);
        assert_eq!(filtered.commits, 4);
        assert_eq!(filtered.pruned, 1);
        assert!(!out.contains(".env"));
        assert!(!out.contains("add env"));
        // The pruned commit's child now hangs off its parent.
        assert!(out.contains("data 7\nmore\nx\nfrom :1\nM 100644 b2 app\n"));
        // An originally empty commit stays.
        assert!(out.contains("data 6\nempty\nfrom :3\n"));
        assert_eq!(
            filtered.originals[1],
            ("bbb".to_owned(), Some(":1".to_owned()))
        );
        assert_eq!(filtered.blobs, BTreeSet::from(["secret".to_owned()]));
        assert_eq!(filtered.emptied_refs, Vec::<String>::new());
        assert!(out.ends_with("done\n"));
    }

    #[test]
    fn merges_survive_and_tags_follow_pruned_commits() {
        let stream = format!(
            "feature done\n\
             commit refs/heads/main\nmark :1\n{HEAD}data 2\nr\nM 100644 b1 a\n\n\
             commit refs/heads/side\nmark :2\n{HEAD}data 2\ns\nfrom :1\nM 100644 b2 .env\n\n\
             commit refs/heads/main\nmark :3\n{HEAD}data 2\nm\nfrom :1\nmerge :2\nM 100644 b2 .env\n\n\
             reset refs/tags/light\nfrom :2\n\n\
             tag v1\nfrom :2\noriginal-oid ttt\ntagger A <a@b> 1 +0000\ndata 3\nv1\n\n\
             done\n"
        );
        let filtered = run(&stream);
        let out = text(&filtered);
        // The side commit is pruned; the merge loses its second parent and
        // its only change, so it is pruned too.
        assert_eq!(filtered.pruned, 2);
        assert!(out.contains("reset refs/tags/light\nfrom :1\n"));
        assert!(out.contains("tag v1\nfrom :1\noriginal-oid ttt\n"));
        assert!(out.contains("reset refs/heads/main\nfrom :1\n"));
    }

    #[test]
    fn a_real_merge_with_no_changes_is_kept() {
        let stream = format!(
            "feature done\n\
             commit refs/heads/main\nmark :1\n{HEAD}data 2\nr\nM 100644 b1 a\n\n\
             commit refs/heads/side\nmark :2\n{HEAD}data 2\ns\nfrom :1\nM 100644 b2 b\n\n\
             commit refs/heads/main\nmark :3\n{HEAD}data 2\nm\nfrom :1\nmerge :2\nM 100644 b2 b\nM 100644 e .env\n\n\
             done\n"
        );
        let filtered = run(&stream);
        assert_eq!(filtered.pruned, 0);
        assert!(text(&filtered).contains("from :1\nmerge :2\nM 100644 b2 b\n\n"));
    }

    #[test]
    fn a_branch_made_only_of_purged_commits_is_emptied() {
        let stream = format!(
            "feature done\nreset refs/heads/secrets\n\
             commit refs/heads/secrets\nmark :1\n{HEAD}data 2\nr\nM 100644 e .env\n\n\
             commit refs/heads/secrets\nmark :2\n{HEAD}data 2\ns\nfrom :1\nM 100644 e2 .env\n\n\
             tag gone\nfrom :2\ntagger A <a@b> 1 +0000\ndata 0\n\n\
             done\n"
        );
        let filtered = run(&stream);
        assert_eq!(filtered.pruned, 2);
        assert_eq!(
            filtered.emptied_refs,
            ["refs/heads/secrets", "refs/tags/gone"]
        );
        assert!(!text(&filtered).contains("tag gone"));
    }

    #[test]
    fn a_child_of_a_vanished_root_becomes_a_root() {
        let stream = format!(
            "feature done\n\
             commit refs/heads/main\nmark :1\n{HEAD}data 2\nr\nM 100644 e .env\n\n\
             commit refs/heads/main\nmark :2\n{HEAD}data 2\nc\nfrom :1\nM 100644 b a\n\n\
             done\n"
        );
        let out = text(&run(&stream));
        assert!(out.contains(
            "reset refs/heads/main\n\nreset refs/heads/main\ncommit refs/heads/main\nmark :2\n"
        ));
        assert!(!out.contains("from"));
    }

    #[test]
    fn messages_are_copied_byte_for_byte() {
        // A message containing lines that look like commands.
        let message = "subject\n\nM 100644 x .env\nfrom :9\n";
        let stream = format!(
            "feature done\ncommit refs/heads/main\nmark :1\n{HEAD}data {}\n{message}M 100644 b a\n\ndone\n",
            message.len()
        );
        let out = text(&run(&stream));
        assert!(out.contains(&format!("data {}\n{message}M 100644 b a\n", message.len())));
    }

    #[test]
    fn rejects_truncated_streams() {
        assert!(filter(b"feature done\n", &env()).is_err());
        assert!(filter(b"commit refs/heads/main\nmark :1\ndata 99\nshort", &env()).is_err());
    }
}
