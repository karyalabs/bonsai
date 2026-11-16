//! Parses `git for-each-ref` with Bonsai's field format.

use super::{Branch, Oid, RemoteBranch, Tag, Tracking};

/// Fields separated by U+001F, one ref per line. Ref names cannot contain
/// either character.
pub(crate) const FORMAT: &str = "--format=%(refname)%1f%(objectname)%1f%(objecttype)%1f\
%(*objectname)%1f%(upstream)%1f%(upstream:track,nobracket)%1f%(HEAD)%1f%(symref)";

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Refs {
    pub branches: Vec<Branch>,
    pub remote_branches: Vec<RemoteBranch>,
    pub tags: Vec<Tag>,
}

pub(crate) fn parse(output: &str) -> Refs {
    let mut refs = Refs::default();
    for line in output.lines() {
        let fields: Vec<&str> = line.split('\u{1f}').collect();
        let [name, oid, kind, peeled, upstream, track, head, symref] = fields[..] else {
            continue;
        };
        if let Some(short) = name.strip_prefix("refs/heads/") {
            refs.branches.push(Branch {
                name: short.to_owned(),
                oid: Oid::new(oid),
                is_head: head == "*",
                upstream: (!upstream.is_empty()).then(|| Tracking {
                    name: upstream
                        .strip_prefix("refs/remotes/")
                        .unwrap_or(upstream)
                        .to_owned(),
                    gone: track == "gone",
                    ahead: count(track, "ahead "),
                    behind: count(track, "behind "),
                }),
            });
        } else if let Some(short) = name.strip_prefix("refs/remotes/") {
            // `origin/HEAD` is a pointer, not a branch.
            if !symref.is_empty() {
                continue;
            }
            let Some((remote, branch)) = short.split_once('/') else {
                continue;
            };
            refs.remote_branches.push(RemoteBranch {
                remote: remote.to_owned(),
                name: branch.to_owned(),
                oid: Oid::new(oid),
            });
        } else if let Some(short) = name.strip_prefix("refs/tags/") {
            let annotated = kind == "tag";
            refs.tags.push(Tag {
                name: short.to_owned(),
                target: Oid::new(if annotated && !peeled.is_empty() {
                    peeled
                } else {
                    oid
                }),
                annotated,
            });
        }
    }
    refs
}

fn count(track: &str, label: &str) -> u32 {
    track
        .split(", ")
        .find_map(|part| part.strip_prefix(label)?.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(fields: [&str; 8]) -> String {
        fields.join("\u{1f}")
    }

    #[test]
    fn parses_branches_remote_branches_and_tags() {
        let output = [
            line([
                "refs/heads/main",
                "aa",
                "commit",
                "",
                "refs/remotes/origin/main",
                "ahead 2, behind 3",
                "*",
                "",
            ]),
            line([
                "refs/heads/old",
                "bb",
                "commit",
                "",
                "refs/remotes/origin/old",
                "gone",
                " ",
                "",
            ]),
            line(["refs/heads/feature/x", "cc", "commit", "", "", "", " ", ""]),
            line([
                "refs/remotes/origin/HEAD",
                "aa",
                "commit",
                "",
                "",
                "",
                " ",
                "refs/remotes/origin/main",
            ]),
            line([
                "refs/remotes/origin/team/y",
                "dd",
                "commit",
                "",
                "",
                "",
                " ",
                "",
            ]),
            line(["refs/tags/v1", "ee", "tag", "ff", "", "", " ", ""]),
            line(["refs/tags/light", "aa", "commit", "", "", "", " ", ""]),
            line(["refs/notes/commits", "99", "commit", "", "", "", " ", ""]),
        ]
        .join("\n");
        let refs = parse(&output);

        assert_eq!(refs.branches.len(), 3);
        let main = &refs.branches[0];
        assert!(main.is_head);
        let tracking = main.upstream.as_ref().unwrap();
        assert_eq!(tracking.name, "origin/main");
        assert_eq!(
            (tracking.ahead, tracking.behind, tracking.gone),
            (2, 3, false)
        );
        assert!(refs.branches[1].upstream.as_ref().unwrap().gone);
        assert_eq!(refs.branches[2].name, "feature/x");
        assert!(refs.branches[2].upstream.is_none());

        assert_eq!(refs.remote_branches.len(), 1);
        assert_eq!(refs.remote_branches[0].remote, "origin");
        assert_eq!(refs.remote_branches[0].name, "team/y");

        assert_eq!(refs.tags[0].target, Oid::new("ff"));
        assert!(refs.tags[0].annotated);
        assert_eq!(refs.tags[1].target, Oid::new("aa"));
        assert!(!refs.tags[1].annotated);
    }
}
