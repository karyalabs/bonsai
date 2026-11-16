//! Commit graph lane layout for Bonsai.
//!
//! Commits arrive newest first in topological order, a page at a time. Each
//! commit gets a lane and the edge segments that enter its row from above and
//! leave it below, in the shape gpuicn's Commit graph draws: a row's incoming
//! edges run from the top edge to the node's height, and outgoing edges from
//! the node's height to the bottom edge. The layout keeps its lanes between
//! pages, so a page appended later continues the graph without re-laying the
//! rows already shown.

/// One commit to lay out: its ID and its parents' IDs, first parent first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit<Id> {
    pub id: Id,
    pub parents: Vec<Id>,
}

/// One edge segment in a row, between two lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    /// Palette index; each new branch line takes the next one.
    pub color: usize,
}

/// A laid-out row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The lane the commit's node sits in.
    pub lane: usize,
    pub color: usize,
    /// More than one parent.
    pub merge: bool,
    /// Segments from the row's top edge to the node's height.
    pub incoming: Vec<Edge>,
    /// Segments from the node's height to the row's bottom edge.
    pub outgoing: Vec<Edge>,
}

impl Row {
    /// One more than the widest lane this row draws.
    #[must_use]
    pub fn width(&self) -> usize {
        self.incoming
            .iter()
            .chain(&self.outgoing)
            .flat_map(|e| [e.from, e.to])
            .chain([self.lane])
            .max()
            .map_or(1, |lane| lane + 1)
    }
}

/// A lane waiting for a commit: the ID it expects and its color.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Lane<Id> {
    expects: Id,
    color: usize,
}

/// Lane state carried from one row (and one page) to the next.
#[derive(Clone, Debug)]
pub struct Layout<Id> {
    lanes: Vec<Option<Lane<Id>>>,
    colors: usize,
}

impl<Id> Default for Layout<Id> {
    fn default() -> Self {
        Self {
            lanes: Vec::new(),
            colors: 0,
        }
    }
}

impl<Id: Clone + PartialEq> Layout<Id> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Lanes open below the last row, including empty gaps.
    #[must_use]
    pub fn open_lanes(&self) -> usize {
        self.lanes.len()
    }

    fn take_color(&mut self) -> usize {
        let color = self.colors;
        self.colors += 1;
        color
    }

    fn free_lane(&mut self) -> usize {
        if let Some(free) = self.lanes.iter().position(Option::is_none) {
            free
        } else {
            self.lanes.push(None);
            self.lanes.len() - 1
        }
    }

    /// Lays out the next commit.
    pub fn push(&mut self, commit: &Commit<Id>) -> Row {
        // Lanes that were waiting for this commit converge on its node.
        let waiting: Vec<usize> = self
            .lanes
            .iter()
            .enumerate()
            .filter_map(|(l, lane)| {
                lane.as_ref()
                    .filter(|lane| lane.expects == commit.id)
                    .map(|_| l)
            })
            .collect();
        let (lane, color) = if let Some(&first) = waiting.first() {
            (first, self.lanes[first].as_ref().map_or(0, |l| l.color))
        } else {
            // A branch tip nothing below has pointed at yet: a new line.
            let color = self.take_color();
            (self.free_lane(), color)
        };

        let mut incoming = Vec::new();
        for (l, slot) in self.lanes.iter().enumerate() {
            let Some(open) = slot else { continue };
            if waiting.contains(&l) {
                incoming.push(Edge {
                    from: l,
                    to: lane,
                    color: open.color,
                });
            } else {
                incoming.push(Edge {
                    from: l,
                    to: l,
                    color: open.color,
                });
            }
        }
        for &l in &waiting {
            self.lanes[l] = None;
        }
        if lane >= self.lanes.len() {
            self.lanes.resize(lane + 1, None);
        }

        // Lanes still open pass straight through.
        let passing: Vec<usize> = (0..self.lanes.len())
            .filter(|&l| self.lanes[l].is_some())
            .collect();
        let mut targets = Vec::with_capacity(commit.parents.len());
        for (n, parent) in commit.parents.iter().enumerate() {
            let existing = self
                .lanes
                .iter()
                .position(|slot| slot.as_ref().is_some_and(|open| open.expects == *parent));
            let target = match existing {
                // The first parent continues this commit's line, even when
                // another line also leads there: the two converge at the
                // parent's row instead of bending here.
                _ if n == 0 => {
                    self.lanes[lane] = Some(Lane {
                        expects: parent.clone(),
                        color,
                    });
                    lane
                }
                // Another line already leads to this merged-in parent: join it.
                Some(l) => l,
                // A merged-in parent starts its own line.
                None => {
                    let color = self.take_color();
                    let free = self.free_lane();
                    self.lanes[free] = Some(Lane {
                        expects: parent.clone(),
                        color,
                    });
                    free
                }
            };
            if !targets.contains(&target) {
                targets.push(target);
            }
        }

        let mut outgoing = Vec::new();
        for &l in &passing {
            if let Some(open) = &self.lanes[l] {
                outgoing.push(Edge {
                    from: l,
                    to: l,
                    color: open.color,
                });
            }
        }
        for &l in &targets {
            if let Some(open) = &self.lanes[l] {
                outgoing.push(Edge {
                    from: lane,
                    to: l,
                    color: open.color,
                });
            }
        }
        while self.lanes.last().is_some_and(Option::is_none) {
            self.lanes.pop();
        }
        Row {
            lane,
            color,
            merge: commit.parents.len() > 1,
            incoming,
            outgoing,
        }
    }

    /// Lays out a page of commits, continuing from the previous page.
    pub fn extend<'a>(&mut self, commits: impl IntoIterator<Item = &'a Commit<Id>>) -> Vec<Row>
    where
        Id: 'a,
    {
        commits
            .into_iter()
            .map(|commit| self.push(commit))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &'static str, parents: &[&'static str]) -> Commit<&'static str> {
        Commit {
            id,
            parents: parents.to_vec(),
        }
    }

    /// A compact text picture of the layout, one line per row:
    /// `lane:color [in from>to…] [out from>to…]`.
    fn picture(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                let edges = |edges: &[Edge]| {
                    edges
                        .iter()
                        .map(|e| format!("{}>{}", e.from, e.to))
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!(
                    "{}{}:{} in[{}] out[{}]",
                    if row.merge { "M" } else { "" },
                    row.lane,
                    row.color,
                    edges(&row.incoming),
                    edges(&row.outgoing)
                )
            })
            .collect()
    }

    #[test]
    fn linear_history_stays_in_one_lane() {
        let commits = [c("c", &["b"]), c("b", &["a"]), c("a", &[])];
        let rows = Layout::new().extend(&commits);
        assert_eq!(
            picture(&rows),
            [
                "0:0 in[] out[0>0]",
                "0:0 in[0>0] out[0>0]",
                "0:0 in[0>0] out[]",
            ]
        );
    }

    #[test]
    fn a_feature_branch_forks_and_merges_back() {
        // m merges f2 into main; f1-f2 branch from a; b is on main.
        let commits = [
            c("m", &["b", "f2"]),
            c("f2", &["f1"]),
            c("b", &["a"]),
            c("f1", &["a"]),
            c("a", &[]),
        ];
        let rows = Layout::new().extend(&commits);
        assert_eq!(
            picture(&rows),
            [
                "M0:0 in[] out[0>0,0>1]",
                "1:1 in[0>0,1>1] out[0>0,1>1]",
                "0:0 in[0>0,1>1] out[1>1,0>0]",
                "1:1 in[0>0,1>1] out[0>0,1>1]",
                "0:0 in[0>0,1>0] out[]",
            ]
        );
    }

    #[test]
    fn octopus_merges_open_one_lane_per_extra_parent() {
        let commits = [
            c("o", &["a", "b", "d"]),
            c("d", &["root"]),
            c("b", &["root"]),
            c("a", &["root"]),
            c("root", &[]),
        ];
        let rows = Layout::new().extend(&commits);
        assert_eq!(rows[0].outgoing.len(), 3);
        assert_eq!(rows[0].width(), 3);
        assert!(rows[0].merge);
        // Each line runs straight down and converges on the root.
        assert_eq!(picture(&rows)[3], "0:0 in[0>0,1>1,2>2] out[1>1,2>2,0>0]");
        assert_eq!(picture(&rows)[4], "0:0 in[0>0,1>0,2>0] out[]");
    }

    #[test]
    fn crossing_merges_reuse_freed_lanes() {
        // Two branches merged into each other: x merges y's line, y merges x's.
        let commits = [
            c("top", &["x", "y"]),
            c("x", &["x1", "y1"]),
            c("y", &["y1", "x1"]),
            c("x1", &["base"]),
            c("y1", &["base"]),
            c("base", &[]),
        ];
        let rows = Layout::new().extend(&commits);
        // Every edge stays inside the lanes that were open; nothing panics and
        // the last row closes the graph.
        assert!(rows.iter().all(|r| r.width() <= 3));
        assert_eq!(rows.last().unwrap().outgoing, []);
        // y's merged-in parent x1 joins x's lane instead of opening a fourth.
        assert_eq!(
            picture(&rows)[2],
            "M1:1 in[0>0,1>1,2>2] out[0>0,2>2,1>1,1>0]"
        );
    }

    #[test]
    fn pages_continue_the_same_graph() {
        let commits = [
            c("m", &["b", "f2"]),
            c("f2", &["f1"]),
            c("b", &["a"]),
            c("f1", &["a"]),
            c("a", &[]),
        ];
        let whole = Layout::new().extend(&commits);
        let mut paged = Layout::new();
        let mut rows = paged.extend(&commits[..2]);
        assert_eq!(paged.open_lanes(), 2);
        rows.extend(paged.extend(&commits[2..]));
        assert_eq!(rows, whole);
    }

    #[test]
    fn many_branch_tips_take_many_lanes_without_panicking() {
        let mut commits: Vec<Commit<String>> = (0..40)
            .map(|i| Commit {
                id: format!("tip{i}"),
                parents: vec!["base".to_owned()],
            })
            .collect();
        commits.push(Commit {
            id: "base".to_owned(),
            parents: Vec::new(),
        });
        let rows = Layout::new().extend(&commits);
        assert_eq!(rows[39].lane, 39);
        assert_eq!(rows[40].incoming.len(), 40);
        assert_eq!(rows[40].lane, 0);
    }
}
