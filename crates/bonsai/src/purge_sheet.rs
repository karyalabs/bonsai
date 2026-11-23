//! The Purge from History sheet: pick paths, preview what changes, confirm
//! by typing the repository's name, purge, then force push the rewritten
//! branches or restore the backup (CONTEXT.md: Purge, Safety backup).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};

use bonsai_git::{
    HistoryScan, PurgeReport, PurgeScan, PurgeStep, PurgeTarget, RefChange, Repository,
    RewrittenBranch, ScanFinding, ScanOptions,
};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use gpui_icons::{LucideIcon, lucide};

use crate::{
    confirm::Confirm,
    file_icon::file_icon,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        checkbox::Checkbox,
        dialog::{dialog_description, dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
        tree::{Tree, TreeEvent, TreeNode, TreeRow, TreeRowKind, TreeState},
    },
};

/// A finished force push: the remote and Git's error, if any.
type PushOutcome = (String, Result<(), String>);

/// Up to this many matching paths, the picker opens every folder.
const EXPAND_LIMIT: usize = 300;

/// Paths in history, keyed by their tree leaf id.
type Matches = BTreeMap<String, PathBuf>;

/// The picker's tree of `paths`. A leaf's id is its path; a folder's is its
/// path with a trailing `/`, so a folder id is a prefix of everything in it.
#[must_use]
pub fn path_nodes<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<TreeNode> {
    #[derive(Default)]
    struct Dir<'a> {
        dirs: BTreeMap<&'a str, Dir<'a>>,
        files: Vec<(&'a str, &'a str)>,
    }
    fn nodes(prefix: &str, dir: Dir<'_>) -> Vec<TreeNode> {
        let mut out: Vec<TreeNode> = dir
            .dirs
            .into_iter()
            .map(|(name, child)| {
                let id = format!("{prefix}{name}/");
                let children = nodes(&id, child);
                TreeNode::folder(id, name.to_owned(), children)
            })
            .collect();
        out.extend(
            dir.files
                .into_iter()
                .map(|(name, path)| TreeNode::leaf(path.to_owned(), name.to_owned())),
        );
        out
    }
    let mut root = Dir::default();
    for path in paths {
        let mut parts: Vec<&str> = path.split('/').collect();
        let name = parts.pop().unwrap_or_default();
        let mut dir = &mut root;
        for part in parts {
            dir = dir.dirs.entry(part).or_default();
        }
        dir.files.push((name, path));
    }
    nodes("", root)
}

/// The matching paths a tree row stands for: itself, or all under a folder.
fn under<'a>(matches: &'a Matches, id: &'a str) -> impl Iterator<Item = &'a PathBuf> + 'a {
    let folder = id.ends_with('/');
    matches
        .range::<str, _>((std::ops::Bound::Included(id), std::ops::Bound::Unbounded))
        .take_while(move |(path, _)| {
            if folder {
                path.starts_with(id)
            } else {
                path.as_str() == id
            }
        })
        .map(|(_, path)| path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurgeEvent {
    Dismissed,
}

/// The history scan behind the Suggested list.
#[derive(Clone, Debug, Default)]
pub enum Suggestions {
    #[default]
    NotScanned,
    Scanning,
    Ready(HistoryScan),
    Failed(SharedString),
}

#[derive(Clone, Debug)]
pub enum Phase {
    Pick,
    Scanning,
    Preview(PurgeScan),
    Running,
    Restoring,
    Done(PurgeReport),
    Failed(SharedString),
    Restored(usize),
}

/// Remote branches a purge rewrote, by remote, for the force push step.
#[must_use]
pub fn rewritten_by_remote(
    changes: &[RefChange],
    remotes: &[String],
) -> BTreeMap<String, Vec<RewrittenBranch>> {
    let mut by_remote: BTreeMap<String, Vec<RewrittenBranch>> = BTreeMap::new();
    for change in changes {
        let (Some(old), Some(new)) = (&change.old, &change.new) else {
            continue;
        };
        let Some(rest) = change.name.strip_prefix("refs/remotes/") else {
            continue;
        };
        // Longest matching remote name, since names may contain `/`.
        let Some(remote) = remotes
            .iter()
            .filter(|r| rest.starts_with(&format!("{r}/")))
            .max_by_key(|r| r.len())
        else {
            continue;
        };
        let name = &rest[remote.len() + 1..];
        if name == "HEAD" {
            continue;
        }
        by_remote
            .entry(remote.clone())
            .or_default()
            .push(RewrittenBranch {
                name: name.to_owned(),
                old: old.to_string(),
                new: new.to_string(),
            });
    }
    by_remote
}

pub struct PurgeSheet {
    model: Entity<RepositoryModel>,
    backup_dir: PathBuf,
    phase: Phase,
    /// Every path in history, once loaded.
    history: Option<Vec<PathBuf>>,
    query: Entity<InputState>,
    confirm: Entity<InputState>,
    /// The history paths matching the query, shown in `tree`.
    matches: Rc<Matches>,
    tree: TreeState,
    selected: BTreeSet<PathBuf>,
    patterns: Vec<String>,
    /// Whether the picker shows suggestions instead of every path.
    suggested: bool,
    suggestions: Suggestions,
    /// Filled by the purge operation for the sheet to pick up.
    outcome: Arc<Mutex<Option<Result<PurgeReport, String>>>>,
    step: Arc<Mutex<Option<PurgeStep>>>,
    restored: Arc<Mutex<Option<Result<usize, String>>>>,
    pushed: Arc<Mutex<Vec<PushOutcome>>>,
    /// Push results per remote: `None` while pushing.
    pushes: BTreeMap<String, Option<Result<(), SharedString>>>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PurgeEvent> for PurgeSheet {}

impl PurgeSheet {
    pub fn new(
        model: Entity<RepositoryModel>,
        backup_dir: PathBuf,
        initial: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search history, or a pattern like *.pem")
        });
        let confirm = cx.new(|cx| InputState::new(window, cx).placeholder("Repository name"));
        let subscriptions = vec![
            cx.subscribe_in(&query, window, |this, _, event, _, cx| match event {
                InputEvent::Change => this.refresh_tree(cx),
                InputEvent::PressEnter { .. } => this.add_pattern(cx),
                _ => {}
            }),
            cx.subscribe_in(&confirm, window, |this, _, event, _, cx| match event {
                InputEvent::Change => cx.notify(),
                InputEvent::PressEnter { .. } => this.purge(cx),
                _ => {}
            }),
            cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
                if matches!(event, RepositoryEvent::OperationFinished { .. }) {
                    this.pick_up_outcome(cx);
                }
            }),
        ];
        query.update(cx, |input, cx| input.focus(window, cx));
        let mut this = Self {
            model,
            backup_dir,
            phase: Phase::Pick,
            history: None,
            query,
            confirm,
            matches: Rc::default(),
            tree: TreeState::new(Vec::new(), []).expect("an empty tree has no duplicates"),
            selected: initial.into_iter().collect(),
            patterns: Vec::new(),
            suggested: false,
            suggestions: Suggestions::NotScanned,
            outcome: Arc::new(Mutex::new(None)),
            step: Arc::new(Mutex::new(None)),
            restored: Arc::new(Mutex::new(None)),
            pushed: Arc::new(Mutex::new(Vec::new())),
            pushes: BTreeMap::new(),
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.load_history(cx);
        this
    }

    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(
            self.phase,
            Phase::Running | Phase::Scanning | Phase::Restoring
        )
    }

    fn repository(&self, cx: &gpui_kit::App) -> Repository {
        self.model.read(cx).repository().clone()
    }

    fn repository_name(&self, cx: &gpui_kit::App) -> String {
        self.repository(cx)
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn load_history(&mut self, cx: &mut Context<Self>) {
        let repository = self.repository(cx);
        let read = cx.background_spawn(async move { repository.history_paths() });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let paths = read.await.unwrap_or_default();
            let _ = this.update(cx, |this, cx| {
                this.history = Some(paths);
                this.refresh_tree(cx);
            });
        }));
    }

    #[must_use]
    pub fn suggestions(&self) -> &Suggestions {
        &self.suggestions
    }

    /// Switches the picker between every path and the scan's suggestions,
    /// scanning the first time.
    pub fn show_suggested(&mut self, suggested: bool, cx: &mut Context<Self>) {
        self.suggested = suggested;
        if suggested && matches!(self.suggestions, Suggestions::NotScanned) {
            self.scan_suggestions(cx);
        }
        cx.notify();
    }

    /// Looks through history for secrets and large files.
    pub fn scan_suggestions(&mut self, cx: &mut Context<Self>) {
        self.suggestions = Suggestions::Scanning;
        let repository = self.repository(cx);
        let scan =
            cx.background_spawn(async move { repository.scan_history(&ScanOptions::default()) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.suggestions = match result {
                    Ok(scan) => Suggestions::Ready(scan),
                    Err(error) => Suggestions::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Checks every suggestion, or clears them all when all are checked.
    pub fn toggle_all_suggestions(&mut self, cx: &mut Context<Self>) {
        let Suggestions::Ready(scan) = &self.suggestions else {
            return;
        };
        let paths: Vec<PathBuf> = scan.findings.iter().map(|f| f.path.clone()).collect();
        if paths.iter().all(|p| self.selected.contains(p)) {
            for path in &paths {
                self.selected.remove(path);
            }
        } else {
            self.selected.extend(paths);
        }
        cx.notify();
    }

    /// Rebuilds the tree from the history paths matching the query.
    fn refresh_tree(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_lowercase();
        let matches: Matches = self
            .history
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|path| (path.to_string_lossy().into_owned(), path))
            .filter(|(text, _)| query.is_empty() || text.to_lowercase().contains(&query))
            .map(|(text, path)| (text, path.clone()))
            .collect();
        let nodes = path_nodes(matches.keys().map(String::as_str));
        let mut expanded = Vec::new();
        if matches.len() <= EXPAND_LIMIT {
            for path in matches.keys() {
                let mut at = 0;
                while let Some(slash) = path[at..].find('/') {
                    at += slash + 1;
                    expanded.push(SharedString::from(path[..at].to_owned()));
                }
            }
        }
        self.tree = TreeState::new(nodes, expanded).expect("paths are unique");
        self.matches = Rc::new(matches);
        cx.notify();
    }

    /// The tree as the picker shows it.
    #[must_use]
    pub fn tree(&self) -> &TreeState {
        &self.tree
    }

    /// Checks or clears everything a tree row stands for: all of it is
    /// checked afterwards unless all of it was before.
    pub fn toggle_row(&mut self, id: &str, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = under(&self.matches, id).cloned().collect();
        if paths.iter().all(|path| self.selected.contains(path)) {
            for path in &paths {
                self.selected.remove(path);
            }
        } else {
            self.selected.extend(paths);
        }
        cx.notify();
    }

    #[must_use]
    pub fn targets(&self) -> Vec<PurgeTarget> {
        self.selected
            .iter()
            .cloned()
            .map(PurgeTarget::Path)
            .chain(self.patterns.iter().cloned().map(PurgeTarget::Glob))
            .collect()
    }

    pub fn toggle(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !self.selected.remove(&path) {
            self.selected.insert(path);
        }
        cx.notify();
    }

    /// Adds the query as a glob pattern when it has wildcards.
    pub fn add_pattern(&mut self, cx: &mut Context<Self>) {
        let query = self.query.read(cx).value().trim().to_owned();
        if query.contains(['*', '?', '[']) && !self.patterns.contains(&query) {
            self.patterns.push(query);
            cx.notify();
        }
    }

    /// Scans history for the chosen targets and shows the preview.
    pub fn preview(&mut self, cx: &mut Context<Self>) {
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        self.phase = Phase::Scanning;
        let repository = self.repository(cx);
        let scan = cx.background_spawn(async move { repository.purge_scan(&targets) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.phase = match result {
                    Ok(scan) => Phase::Preview(scan),
                    Err(error) => Phase::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn confirmed(&self, cx: &gpui_kit::App) -> bool {
        self.confirm.read(cx).value().trim() == self.repository_name(cx)
    }

    /// Runs the purge once the name is typed and nothing blocks it.
    pub fn purge(&mut self, cx: &mut Context<Self>) {
        let Phase::Preview(scan) = &self.phase else {
            return;
        };
        if !scan.blockers.is_empty() || !scan.found() || !self.confirmed(cx) {
            return;
        }
        self.phase = Phase::Running;
        let targets = self.targets();
        let backup_dir = self.backup_dir.clone();
        let outcome = self.outcome.clone();
        let step = self.step.clone();
        self.model.update(cx, |model, cx| {
            model.run(
                "Purge from history",
                move |repository| {
                    let report = repository
                        .purge(&targets, &backup_dir, &|s| {
                            if let Ok(mut slot) = step.lock() {
                                *slot = Some(s);
                            }
                        })
                        .map_err(|e| e.to_string());
                    if let Ok(mut slot) = outcome.lock() {
                        *slot = Some(report);
                    }
                    // The sheet shows the outcome, success or not.
                    Ok(())
                },
                cx,
            );
        });
        // Redraw the step while the purge runs.
        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                let running = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(this.phase, Phase::Running)
                    })
                    .unwrap_or(false);
                if !running {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn pick_up_outcome(&mut self, cx: &mut Context<Self>) {
        let outcome = self.outcome.lock().ok().and_then(|mut slot| slot.take());
        if let Some(outcome) = outcome {
            self.phase = match outcome {
                Ok(report) => Phase::Done(report),
                Err(error) => Phase::Failed(error.into()),
            };
        }
        let restored = self.restored.lock().ok().and_then(|mut slot| slot.take());
        if let Some(restored) = restored {
            self.phase = match restored {
                Ok(count) => Phase::Restored(count),
                Err(error) => Phase::Failed(error.into()),
            };
        }
        let pushed = self
            .pushed
            .lock()
            .map(|mut list| std::mem::take(&mut *list))
            .unwrap_or_default();
        for (remote, result) in pushed {
            self.pushes.insert(remote, Some(result.map_err(Into::into)));
        }
        cx.notify();
    }

    /// Force pushes one remote's rewritten branches, each with a lease on
    /// its pre-purge commit.
    pub fn push_remote(&mut self, remote: String, cx: &mut Context<Self>) {
        let Phase::Done(report) = &self.phase else {
            return;
        };
        let remotes = self.remote_names(cx);
        let Some(branches) = rewritten_by_remote(&report.refs, &remotes).remove(&remote) else {
            return;
        };
        self.pushes.insert(remote.clone(), None);
        let name = format!("Force push rewritten branches to {remote}");
        let results = self.pushed.clone();
        self.model.update(cx, |model, cx| {
            model.run_network(
                name,
                move |repository, progress, cancel| {
                    let outcome = repository
                        .push_rewritten(&remote, &branches, Some(progress), Some(cancel))
                        .map_err(|e| e.to_string());
                    if let Ok(mut results) = results.lock() {
                        results.push((remote, outcome));
                    }
                    Ok(())
                },
                cx,
            );
        });
        cx.notify();
    }

    fn remote_names(&self, cx: &gpui_kit::App) -> Vec<String> {
        self.model
            .read(cx)
            .snapshot()
            .map(|s| s.remotes.iter().map(|r| r.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Restores a backup bundle: the one this purge wrote, or one the user
    /// picks.
    pub fn restore(&mut self, bundle: PathBuf, cx: &mut Context<Self>) {
        let slot = self.restored.clone();
        self.phase = Phase::Restoring;
        self.model.update(cx, |model, cx| {
            model.run(
                "Restore from backup",
                move |repository| {
                    let restored = repository
                        .restore_backup(&bundle)
                        .map(|changes| changes.len())
                        .map_err(|e| e.to_string());
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(restored);
                    }
                    Ok(())
                },
                cx,
            );
        });
        cx.notify();
    }

    fn choose_backup(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Restore".into()),
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update(cx, |this, cx| this.restore(path, cx));
            }
        }));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(PurgeEvent::Dismissed);
    }

    fn render_pick(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let query = self.query.read(cx).value().trim().to_lowercase();
        let is_pattern = query.contains(['*', '?', '[']);
        let chosen = self.selected.len() + self.patterns.len();
        let list = div()
            .h(px(300.))
            .flex()
            .flex_col()
            .border_1()
            .border_color(c.border)
            .rounded(px(6.))
            .overflow_hidden()
            .child(if self.suggested {
                self.render_suggestions(&theme, &query, cx)
            } else if self.history.is_none() {
                div()
                    .p(px(8.))
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child("Reading history…")
                    .into_any_element()
            } else {
                self.render_tree(&theme, cx).into_any_element()
            });
        let view = cx.entity().downgrade();
        let mode = ToggleGroup::new("purge.mode")
            .aria_label("Show")
            .segmented(true)
            .value([if self.suggested { "suggested" } else { "all" }])
            .item(ToggleGroupItem::new("purge.mode.all", "all").child("All Paths"))
            .item(ToggleGroupItem::new("purge.mode.suggested", "suggested").child("Suggested"))
            .on_change(move |values, _, cx| {
                if let Some(value) = values.first() {
                    let suggested = value == "suggested";
                    let _ = view.update(cx, |view, cx| view.show_suggested(suggested, cx));
                }
            });
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(dialog_title("purge.title", cx).child("Purge from History"))
            .child(stepper(0, &theme))
            .child(dialog_description("purge.description", cx).child(
                "Remove files such as a committed .env from every commit on every branch and tag. The files stay on disk, untracked.",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.query).aria_label("Search history")),
                    )
                    .child(mode),
            )
            .when(is_pattern && !self.suggested, |this| {
                this.child(
                    Button::new("purge.add-pattern")
                        .variant(ButtonVariant::Outline)
                        .label(format!("Purge everything matching {query}"))
                        .on_click(cx.listener(|this, _, _, cx| this.add_pattern(cx))),
                )
            })
            .child(list)
            .when(!self.patterns.is_empty(), |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("Patterns: {}", self.patterns.join(", "))),
                )
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("purge.restore")
                            .variant(ButtonVariant::Ghost)
                            .label("Restore a Backup…")
                            .on_click(cx.listener(|this, _, _, cx| this.choose_backup(cx))),
                    )
                    .child(
                        Button::new("purge.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    )
                    .child(
                        Button::new("purge.next")
                            .label(if chosen > 1 {
                                format!("Preview {chosen} Paths")
                            } else {
                                "Preview".to_owned()
                            })
                            .disabled(chosen == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.preview(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_suggestions(
        &self,
        theme: &UiTheme,
        query: &str,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let c = &theme.colors;
        let note = |text: String| {
            div()
                .p(px(8.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child(text)
                .into_any_element()
        };
        let scan = match &self.suggestions {
            Suggestions::NotScanned | Suggestions::Scanning => {
                return note(
                    "Looking through every version of every file for secrets and large files…"
                        .to_owned(),
                );
            }
            Suggestions::Failed(error) => return note(format!("The scan failed: {error}")),
            Suggestions::Ready(scan) => scan,
        };
        if scan.findings.is_empty() {
            return div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .size_full()
                .p(px(16.))
                .child(
                    lucide(LucideIcon::ShieldCheck)
                        .size(px(22.))
                        .text_color(c.success),
                )
                .child(
                    div()
                        .text_sm()
                        .child("Nothing in history looks like a secret or a large file."),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("Read {} file versions.", scan.blobs_read)),
                )
                .into_any_element();
        }
        let shown: Vec<&ScanFinding> = scan
            .findings
            .iter()
            .filter(|f| query.is_empty() || f.path.to_string_lossy().to_lowercase().contains(query))
            .collect();
        let all_checked = scan
            .findings
            .iter()
            .all(|f| self.selected.contains(&f.path));
        let secrets = scan.findings.iter().filter(|f| f.is_secret()).count();
        let large = scan.findings.len() - secrets;
        let mut summary = Vec::new();
        if secrets > 0 {
            summary.push(format!(
                "{secrets} possible secret{}",
                if secrets == 1 { "" } else { "s" }
            ));
        }
        if large > 0 {
            summary.push(format!(
                "{large} large file{}",
                if large == 1 { "" } else { "s" }
            ));
        }
        let header = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .px(px(8.))
            .py(px(6.))
            .border_b_1()
            .border_color(c.border)
            .text_xs()
            .text_color(c.muted_foreground)
            .child(
                Checkbox::new("purge.suggested.all")
                    .checked(all_checked)
                    .indeterminate(
                        !all_checked
                            && scan
                                .findings
                                .iter()
                                .any(|f| self.selected.contains(&f.path)),
                    )
                    .aria_label("Check every suggestion")
                    .on_change(cx.listener(|this, _, _, cx| this.toggle_all_suggestions(cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("Found {}", summary.join(" and "))),
            )
            .when(!scan.contents_complete, |this| {
                this.child(div().flex_shrink_0().child("Contents partly read"))
            })
            .child(
                div().flex_shrink_0().child(
                    Button::new("purge.rescan")
                        .variant(ButtonVariant::Ghost)
                        .label("Scan Again")
                        .on_click(cx.listener(|this, _, _, cx| this.scan_suggestions(cx))),
                ),
            );
        let mut rows = div()
            .id("purge.suggested.list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .p(px(4.));
        for (i, finding) in shown.into_iter().enumerate() {
            let path = finding.path.clone();
            let checked = self.selected.contains(&path);
            let tone = if finding.is_secret() {
                c.destructive
            } else {
                c.warning
            };
            let reasons = finding
                .reasons
                .iter()
                .map(bonsai_git::ScanReason::label)
                .collect::<Vec<_>>()
                .join(" · ");
            rows = rows.child(
                div()
                    .id(("purge.suggested.row", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(5.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .hover(|this| this.bg(c.muted))
                    .on_click({
                        let path = path.clone();
                        cx.listener(move |this, _, _, cx| this.toggle(path.clone(), cx))
                    })
                    .child(
                        div()
                            .id(("purge.suggested.check-wrap", i))
                            .flex_none()
                            // The row toggles too; stop the click there so it counts once.
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .child(
                                Checkbox::new(("purge.suggested.check", i))
                                    .checked(checked)
                                    .aria_label(path.to_string_lossy().into_owned())
                                    .on_change({
                                        let path = path.clone();
                                        cx.listener(move |this, _, _, cx| {
                                            this.toggle(path.clone(), cx);
                                        })
                                    }),
                            ),
                    )
                    .child(
                        lucide(if finding.is_secret() {
                            LucideIcon::KeyRound
                        } else {
                            LucideIcon::HardDrive
                        })
                        .size(px(14.))
                        .flex_none()
                        .text_color(tone),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_sm()
                                    .truncate()
                                    .font_family(theme.fonts.mono.clone())
                                    .child(path.to_string_lossy().into_owned()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(c.muted_foreground)
                                    .child(reasons),
                            ),
                    )
                    .when(finding.in_head, |this| {
                        this.child(
                            div()
                                .flex_shrink_0()
                                .px(px(6.))
                                .py(px(1.))
                                .rounded(px(4.))
                                .bg(c.muted)
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child("In current files"),
                        )
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(rows)
            .into_any_element()
    }

    fn render_tree(&self, theme: &UiTheme, cx: &mut Context<Self>) -> Tree {
        let matches = self.matches.clone();
        let selected = Rc::new(self.selected.clone());
        let row_theme = theme.clone();
        let view = cx.entity().downgrade();
        let toggle_view = view.clone();
        Tree::new(
            "purge.paths",
            "Paths in history",
            self.tree.clone(),
            move |row: &TreeRow, _, _| {
                let c = &row_theme.colors;
                let (mut any, mut all) = (false, true);
                for path in under(&matches, &row.id) {
                    if selected.contains(path) {
                        any = true;
                    } else {
                        all = false;
                    }
                }
                let folder = matches!(row.kind, TreeRowKind::Folder { .. });
                let id = row.id.clone();
                let view = toggle_view.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .text_sm()
                    .child(
                        Checkbox::new(SharedString::from(format!("purge.check.{}", row.id)))
                            .checked(any && all)
                            .indeterminate(any && !all)
                            .aria_label(row.id.clone())
                            .on_change(move |_, _, cx| {
                                let _ = view.update(cx, |view, cx| view.toggle_row(&id, cx));
                            }),
                    )
                    .child(
                        lucide(if folder {
                            LucideIcon::Folder
                        } else {
                            file_icon(Path::new(row.id.as_ref()))
                        })
                        .size(px(14.))
                        .flex_none()
                        .text_color(c.muted_foreground),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .when(folder, |this| this.text_color(c.muted_foreground))
                            .child(row.label.clone()),
                    )
                    .into_any_element()
            },
        )
        .empty(
            div()
                .p(px(8.))
                .text_sm()
                .text_color(theme.colors.muted_foreground)
                .child("No path in history matches."),
        )
        .on_event(move |event, _, cx| {
            if let TreeEvent::Activate(id) = event
                && !id.ends_with('/')
            {
                let _ = view.update(cx, |view, cx| view.toggle_row(id, cx));
            }
        })
    }

    fn render_preview(&self, scan: &PurgeScan, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let name = self.repository_name(cx);
        let ready = scan.blockers.is_empty() && scan.found();
        let can_purge = ready && self.confirmed(cx);
        let pills = |items: Vec<String>, limit: usize| {
            let extra = items.len().saturating_sub(limit);
            div()
                .flex()
                .flex_wrap()
                .gap(px(4.))
                .children(items.into_iter().take(limit).map(|item| pill(item, &theme)))
                .when(extra > 0, |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(format!("and {extra} more")),
                    )
                })
        };
        let label = |text: &str| {
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(c.muted_foreground)
                .child(text.to_uppercase())
        };
        let paths: Vec<String> = scan
            .paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        let refs: Vec<String> = scan.refs.iter().map(|r| short_ref(r)).collect();
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(dialog_title("purge.title", cx).child("Purge from History"))
            .child(stepper(1, &theme))
            .when(!scan.found(), |this| {
                this.child(dialog_description("purge.none", cx).child("Nothing in this repository's history matches."))
            })
            .when(scan.found(), |this| {
                this.child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(stat(scan.commits.len(), "commits to rewrite", &theme))
                        .child(stat(scan.refs.len(), "branches and tags", &theme))
                        .child(stat(scan.stashes.len(), "stashes to drop", &theme)),
                )
                .child(div().flex().flex_col().gap(px(6.)).child(label("Files")).child(pills(paths, 12)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(label("Branches and tags"))
                        .child(pills(refs, 12)),
                )
                .child(callout(
                    LucideIcon::TriangleAlert,
                    c.warning,
                    &[
                        "Commit hashes change from the first affected commit onward, and signatures on rewritten commits and tags are dropped.",
                        "Bonsai saves a backup first and never pushes on its own. Anyone who already cloned keeps the old commits, so rotate the secret too.",
                    ],
                    &theme,
                ))
            })
            .children(scan.blockers.iter().map(|b| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_sm()
                    .text_color(c.destructive)
                    .child(lucide(LucideIcon::CircleAlert).size(px(14.))
.flex_none().text_color(c.destructive))
                    .child(b.message())
            }))
            .when(ready, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .text_sm()
                        .child(format!("Type {name} to confirm."))
                        .child(Input::new(&self.confirm).aria_label("Repository name")),
                )
            })
            .child(
                Confirm::new(
                    "purge",
                    "Purge",
                    |this: &mut Self, _, cx| {
                        this.phase = Phase::Pick;
                        cx.notify();
                    },
                    |this, _, cx| this.purge(cx),
                )
                .cancel_label("Back")
                .enabled(can_purge)
                .footer(cx),
            )
            .into_any_element()
    }

    fn render_done(&self, report: &PurgeReport, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let remotes = self.remote_names(cx);
        let by_remote = rewritten_by_remote(&report.refs, &remotes);
        let backup = report.backup.clone();
        let mut summary = vec![format!(
            "Removed {} from history: {} commits rewritten, {} removed.",
            report
                .paths
                .iter()
                .take(3)
                .map(|p| p.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(", "),
            report.commits_rewritten,
            report.commits_removed
        )];
        if report.blobs_shared > 0 {
            summary.push(format!(
                "{} file versions stay because other paths have the same contents.",
                report.blobs_shared
            ));
        }
        if report.stashes_dropped > 0 {
            summary.push(format!("{} stashes were dropped.", report.stashes_dropped));
        }
        let mut pushes = div().flex().flex_col().gap(px(6.));
        for (remote, branches) in &by_remote {
            let names = branches
                .iter()
                .map(|b| b.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let state = self.pushes.get(remote);
            let target = remote.clone();
            pushes = pushes.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .text_sm()
                    .child(div().min_w_0().child(format!("{remote}: {names}")))
                    .child(match state {
                        None => Button::new(SharedString::from(format!("purge.push.{remote}")))
                            .variant(ButtonVariant::Destructive)
                            .label("Force Push with Lease")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.push_remote(target.clone(), cx);
                            }))
                            .into_any_element(),
                        Some(None) => div().text_xs().child("Pushing…").into_any_element(),
                        Some(Some(Ok(()))) => div().text_xs().child("Pushed").into_any_element(),
                        Some(Some(Err(error))) => div()
                            .text_xs()
                            .text_color(c.destructive)
                            .child(error.clone())
                            .into_any_element(),
                    }),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(lucide(LucideIcon::CircleCheck).size(px(18.)).text_color(c.success))
                    .child(dialog_title("purge.title", cx).child("History Purged")),
            )
            .child(div().flex().flex_col().gap(px(4.)).text_sm().children(summary))
            .child(
                div()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(format!("Backup: {}", backup.display())),
            )
            .when(!by_remote.is_empty(), |this| {
                this.child(
                    div()
                        .text_sm()
                        .child("These remote branches still have the old commits. Force push to replace them; the lease stops if someone pushed since your last fetch."),
                )
                .child(pushes)
            })
            .child(callout(
                LucideIcon::KeyRound,
                c.warning,
                &["Rotate the secret now. Hosting services can keep old commits in caches, forks and pull request refs even after a force push."],
                &theme,
            ))
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("purge.reveal")
                            .variant(ButtonVariant::Ghost)
                            .label("Show Backup")
                            .on_click({
                                let backup = backup.clone();
                                move |_, _, cx| cx.reveal_path(&backup)
                            }),
                    )
                    .child(
                        Button::new("purge.undo")
                            .variant(ButtonVariant::Outline)
                            .label("Restore Original History")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.restore(backup.clone(), cx);
                            })),
                    )
                    .child(
                        Button::new("purge.done")
                            .label("Done")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
            )
            .into_any_element()
    }
}

/// The three steps across the top of the sheet, with `current` highlighted
/// and the ones before it checked.
fn stepper(current: usize, theme: &UiTheme) -> gpui_kit::Div {
    let c = &theme.colors;
    let mut row = div().flex().items_center().gap(px(8.));
    for (i, label) in ["Choose files", "Review", "Rewrite"]
        .into_iter()
        .enumerate()
    {
        if i > 0 {
            row = row.child(div().w(px(24.)).h(px(1.)).bg(c.border));
        }
        let done = i < current;
        let active = i == current;
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .text_xs()
                .text_color(if active {
                    c.foreground
                } else {
                    c.muted_foreground
                })
                .when(active, |this| {
                    this.font_weight(gpui_kit::FontWeight::MEDIUM)
                })
                .child(
                    div()
                        .size(px(18.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(done, |this| {
                            this.bg(c.primary).child(
                                lucide(LucideIcon::Check)
                                    .size(px(10.))
                                    .text_color(c.primary_foreground),
                            )
                        })
                        .when(active, |this| {
                            this.bg(c.primary)
                                .text_color(c.primary_foreground)
                                .child((i + 1).to_string())
                        })
                        .when(!done && !active, |this| {
                            this.border_1()
                                .border_color(c.border)
                                .child((i + 1).to_string())
                        }),
                )
                .child(label),
        );
    }
    row
}

/// A tinted note with an icon: a warning before a rewrite, advice after.
pub(crate) fn callout(
    icon: LucideIcon,
    tint: gpui_kit::Rgba,
    lines: &[&str],
    theme: &UiTheme,
) -> gpui_kit::Div {
    div()
        .flex()
        .gap(px(10.))
        .p(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(tint.opacity(0.35))
        .bg(tint.opacity(0.08))
        .child(
            lucide(icon)
                .size(px(14.))
                .flex_none()
                .mt(px(1.))
                .text_color(tint),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(4.))
                .text_xs()
                .text_color(theme.colors.foreground)
                .children(lines.iter().map(|line| (*line).to_owned())),
        )
}

/// A big number with a label under it.
fn stat(value: usize, label: &str, theme: &UiTheme) -> gpui_kit::Div {
    div()
        .flex_1()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(10.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.colors.border)
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(value.to_string()),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.colors.muted_foreground)
                .child(label.to_owned()),
        )
}

/// A path or ref as a small monospace pill.
fn pill(text: String, theme: &UiTheme) -> gpui_kit::Div {
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(theme.colors.muted)
        .font_family(theme.fonts.mono.clone())
        .text_xs()
        .child(text)
}

fn short_ref(name: &str) -> String {
    name.strip_prefix("refs/heads/")
        .or_else(|| name.strip_prefix("refs/remotes/"))
        .or_else(|| name.strip_prefix("refs/tags/"))
        .unwrap_or(name)
        .to_owned()
}

/// The default backup folder beside the session file.
#[must_use]
pub fn backup_dir(session_path: Option<&Path>) -> PathBuf {
    session_path
        .and_then(Path::parent)
        .map_or_else(|| std::env::temp_dir().join("bonsai"), Path::to_path_buf)
        .join("backups")
}

impl Render for PurgeSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let status = |text: String| {
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(dialog_title("purge.title", cx).child("Purge from History"))
                .child(div().text_sm().text_color(c.muted_foreground).child(text))
                .into_any_element()
        };
        let body = match self.phase.clone() {
            Phase::Pick => self.render_pick(cx),
            Phase::Scanning => status("Looking through history…".to_owned()),
            Phase::Restoring => status("Restoring the backup…".to_owned()),
            Phase::Running => {
                let step = self.step.lock().ok().and_then(|s| *s);
                let order = [
                    (PurgeStep::Backup, "Save a backup of every branch and tag"),
                    (PurgeStep::Rewrite, "Rewrite history"),
                    (PurgeStep::Cleanup, "Expire reflogs and remove old objects"),
                    (PurgeStep::Verify, "Check nothing still has the files"),
                ];
                let current = step.and_then(|step| order.iter().position(|(s, _)| *s == step));
                let mut list = div().flex().flex_col().gap(px(8.));
                for (i, (_, label)) in order.iter().enumerate() {
                    let (icon, color) = match current {
                        Some(at) if i < at => (LucideIcon::CircleCheck, c.success),
                        Some(at) if i == at => (LucideIcon::LoaderCircle, c.foreground),
                        _ => (LucideIcon::Circle, c.muted_foreground),
                    };
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .text_sm()
                            .text_color(if current.is_some_and(|at| i <= at) {
                                c.foreground
                            } else {
                                c.muted_foreground
                            })
                            .child(lucide(icon).size(px(14.)).text_color(color))
                            .child(*label),
                    );
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .child(dialog_title("purge.title", cx).child("Purge from History"))
                    .child(stepper(2, &theme))
                    .child(
                        div()
                            .text_sm()
                            .text_color(c.muted_foreground)
                            .child(if step.is_none() {
                                "Waiting for other operations to finish…"
                            } else {
                                "Keep Bonsai open until this finishes."
                            }),
                    )
                    .child(list)
                    .into_any_element()
            }
            Phase::Preview(scan) => self.render_preview(&scan, cx),
            Phase::Done(report) => self.render_done(&report, cx),
            Phase::Failed(error) => div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(dialog_title("purge.title", cx).child("Purge Failed"))
                .child(div().text_sm().text_color(c.destructive).child(error))
                .child(
                    dialog_footer(cx).child(
                        Button::new("purge.close")
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
                )
                .into_any_element(),
            Phase::Restored(count) => div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(dialog_title("purge.title", cx).child("History Restored"))
                .child(div().text_sm().child(format!(
                    "{count} branches, tags and remote branches are back where they were before the purge."
                )))
                .child(
                    dialog_footer(cx).child(
                        Button::new("purge.close")
                            .label("Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                    ),
                )
                .into_any_element(),
        };
        div().w_full().child(body)
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Oid};
    use gpui_kit::TestAppContext;

    use super::*;

    fn git(dir: &Path, args: &[&str]) -> String {
        let g = Git::locate(None).unwrap();
        g.run(&g.command(args.iter().copied()).cwd(dir))
            .unwrap()
            .stdout_lossy()
    }

    #[test]
    fn groups_rewritten_remote_branches_by_remote() {
        let change = |name: &str, old: Option<&str>, new: Option<&str>| RefChange {
            name: name.into(),
            old: old.map(Oid::new),
            new: new.map(Oid::new),
        };
        let changes = [
            change("refs/heads/main", Some("a"), Some("b")),
            change("refs/remotes/origin/main", Some("a"), Some("b")),
            change("refs/remotes/origin/HEAD", Some("a"), Some("b")),
            change("refs/remotes/team/x/feature", Some("c"), Some("d")),
            change("refs/remotes/origin/gone", Some("e"), None),
        ];
        let grouped =
            rewritten_by_remote(&changes, &["origin".into(), "team".into(), "team/x".into()]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped["origin"].len(), 1);
        assert_eq!(grouped["team/x"][0].name, "feature");
    }

    #[test]
    fn the_picker_nests_paths_under_folders() {
        let nodes = path_nodes(["a/b/c.txt", "a/d.txt", "e.txt"]);
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_ref()).collect();
        assert_eq!(ids, ["a/", "e.txt"]);
        let crate::ui::tree::TreeChildren::Loaded(a) = &nodes[0].children else {
            panic!("a/ is a folder");
        };
        let ids: Vec<&str> = a.iter().map(|n| n.id.as_ref()).collect();
        assert_eq!(ids, ["a/b/", "a/d.txt"]);
    }

    #[test]
    fn a_folder_row_covers_only_what_is_under_it() {
        let matches: Matches = ["a/b.txt", "a/c.txt", "ab.txt", "a"]
            .into_iter()
            .map(|p| (p.to_owned(), PathBuf::from(p)))
            .collect();
        let under_a: Vec<_> = under(&matches, "a/").collect();
        assert_eq!(under_a, [Path::new("a/b.txt"), Path::new("a/c.txt")]);
        let file: Vec<_> = under(&matches, "a").collect();
        assert_eq!(file, [Path::new("a")]);
    }

    #[gpui_kit::test]
    fn purges_after_typed_confirmation_and_restores(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("demo");
        std::fs::create_dir(&root).unwrap();
        let repository =
            Repository::init(&Git::locate(None).unwrap(), &root, Some("main")).unwrap();
        for (key, value) in [
            ("user.name", "Bonsai"),
            ("user.email", "bonsai@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(&root, &["config", key, value]);
        }
        std::fs::write(root.join("app.txt"), "app").unwrap();
        std::fs::write(root.join(".env"), "SECRET").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "init"]);
        let before = git(&root, &["rev-parse", "main"]);

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let backups = dir.path().join("backups");
        let (sheet, cx) = cx.add_window_view(|window, cx| {
            PurgeSheet::new(
                model.clone(),
                backups.clone(),
                Some(".env".into()),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.targets(), [PurgeTarget::Path(".env".into())]);
            assert_eq!(sheet.history.as_deref().map(<[PathBuf]>::len), Some(2));
        });
        // Checking a folder in the tree takes every path under it, and a
        // second check clears them again.
        sheet.update(cx, |sheet, cx| {
            sheet.history = Some(vec![
                ".env".into(),
                "keys/a.pem".into(),
                "keys/b.pem".into(),
            ]);
            sheet.refresh_tree(cx);
            assert_eq!(sheet.tree().visible_len(), 4);
            sheet.toggle_row("keys/", cx);
            assert_eq!(sheet.targets().len(), 3);
            sheet.toggle_row("keys/", cx);
            assert_eq!(sheet.targets(), [PurgeTarget::Path(".env".into())]);
        });

        sheet.update(cx, PurgeSheet::preview);
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            let Phase::Preview(scan) = sheet.phase() else {
                panic!("expected a preview, got {:?}", sheet.phase());
            };
            assert_eq!(scan.commits.len(), 1);
        });
        // Nothing happens until the repository's name is typed.
        sheet.update(cx, PurgeSheet::purge);
        sheet.read_with(cx, |sheet, _| {
            assert!(matches!(sheet.phase(), Phase::Preview(_)));
        });
        let confirm = sheet.read_with(cx, |sheet, _| sheet.confirm.clone());
        confirm.update_in(cx, |input, window, cx| input.set_value("demo", window, cx));
        sheet.update(cx, PurgeSheet::purge);
        cx.run_until_parked();
        let backup = sheet.read_with(cx, |sheet, _| {
            let Phase::Done(report) = sheet.phase() else {
                panic!("expected done, got {:?}", sheet.phase());
            };
            assert_eq!(report.paths, [PathBuf::from(".env")]);
            report.backup.clone()
        });
        assert!(backup.starts_with(&backups));
        assert_ne!(git(&root, &["rev-parse", "main"]), before);
        assert!(root.join(".env").exists());

        sheet.update(cx, |sheet, cx| sheet.restore(backup, cx));
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert!(matches!(sheet.phase(), Phase::Restored(1)));
        });
        assert_eq!(git(&root, &["rev-parse", "main"]), before);
    }

    #[gpui_kit::test]
    fn suggests_secrets_found_in_history(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("demo");
        std::fs::create_dir(&root).unwrap();
        let repository =
            Repository::init(&Git::locate(None).unwrap(), &root, Some("main")).unwrap();
        for (key, value) in [
            ("user.name", "Ada Lovelace"),
            ("user.email", "ada@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(&root, &["config", key, value]);
        }
        std::fs::write(root.join("app.txt"), "app").unwrap();
        std::fs::write(root.join(".env"), "SECRET").unwrap();
        std::fs::write(root.join("deploy.pem"), "-----BEGIN PRIVATE KEY-----\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "init"]);

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (sheet, cx) = cx.add_window_view(|window, cx| {
            PurgeSheet::new(model.clone(), dir.path().join("backups"), None, window, cx)
        });
        sheet.update(cx, |sheet, cx| sheet.show_suggested(true, cx));
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            let Suggestions::Ready(scan) = sheet.suggestions() else {
                panic!("expected a scan, got {:?}", sheet.suggestions());
            };
            let paths: Vec<_> = scan.findings.iter().map(|f| f.path.clone()).collect();
            assert_eq!(paths, [PathBuf::from("deploy.pem"), PathBuf::from(".env")]);
        });
        sheet.update(cx, PurgeSheet::toggle_all_suggestions);
        sheet.read_with(cx, |sheet, _| assert_eq!(sheet.targets().len(), 2));
        sheet.update(cx, PurgeSheet::toggle_all_suggestions);
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.targets(), Vec::<PurgeTarget>::new());
        });
    }
}
