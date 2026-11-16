//! The Purge from History sheet: pick paths, preview what changes, confirm
//! by typing the repository's name, purge, then force push the rewritten
//! branches or restore the backup (CONTEXT.md: Purge, Safety backup).

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use bonsai_git::{
    PurgeReport, PurgeScan, PurgeStep, PurgeTarget, RefChange, Repository, RewrittenBranch,
};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    confirm::Confirm,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        checkbox::Checkbox,
        dialog::{dialog_description, dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

/// A finished force push: the remote and Git's error, if any.
type PushOutcome = (String, Result<(), String>);

/// How many matching paths the picker lists at once.
const LIST_LIMIT: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PurgeEvent {
    Dismissed,
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
    selected: BTreeSet<PathBuf>,
    patterns: Vec<String>,
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
                InputEvent::Change => cx.notify(),
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
            selected: initial.into_iter().collect(),
            patterns: Vec::new(),
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
                cx.notify();
            });
        }));
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
        let matches: Vec<PathBuf> = self
            .history
            .as_deref()
            .unwrap_or_default()
            .iter()
            .filter(|p| query.is_empty() || p.to_string_lossy().to_lowercase().contains(&query))
            .take(LIST_LIMIT)
            .cloned()
            .collect();
        let is_pattern = query.contains(['*', '?', '[']);
        let chosen = self.selected.len() + self.patterns.len();
        let mut list = div()
            .id("purge.paths")
            .h(px(220.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .border_1()
            .border_color(c.border)
            .rounded(px(6.));
        if self.history.is_none() {
            list = list.child(
                div()
                    .p(px(8.))
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child("Reading history…"),
            );
        }
        for (i, path) in matches.into_iter().enumerate() {
            let checked = self.selected.contains(&path);
            let label = path.to_string_lossy().into_owned();
            let toggle = path.clone();
            list = list.child(
                div()
                    .id(("purge.path", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(4.))
                    .text_sm()
                    .cursor_pointer()
                    .hover(|s| s.bg(c.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(toggle.clone(), cx)))
                    .child(
                        Checkbox::new(("purge.check", i))
                            .checked(checked)
                            .aria_label(label.clone())
                            .on_change(cx.listener({
                                let path = path.clone();
                                move |this, _: &bool, _, cx| this.toggle(path.clone(), cx)
                            })),
                    )
                    .child(div().min_w_0().truncate().child(label)),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(dialog_title("purge.title", cx).child("Purge from History"))
            .child(dialog_description("purge.description", cx).child(
                "Remove files such as a committed .env from every commit on every branch and tag. The files stay on disk, untracked.",
            ))
            .child(Input::new(&self.query).aria_label("Search history"))
            .when(is_pattern, |this| {
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

    fn render_preview(&self, scan: &PurgeScan, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let name = self.repository_name(cx);
        let ready = scan.blockers.is_empty() && scan.found();
        let can_purge = ready && self.confirmed(cx);
        let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
        let mut facts = vec![
            format!(
                "{} touch {}.",
                plural(scan.commits.len(), "commit"),
                scan.paths
                    .iter()
                    .take(5)
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
                    + if scan.paths.len() > 5 { ", …" } else { "" }
            ),
            format!(
                "Rewrites {}: {}.",
                plural(scan.refs.len(), "ref"),
                scan.refs
                    .iter()
                    .take(6)
                    .map(|r| short_ref(r))
                    .collect::<Vec<_>>()
                    .join(", ")
                    + if scan.refs.len() > 6 { ", …" } else { "" }
            ),
        ];
        if !scan.stashes.is_empty() {
            facts.push(format!(
                "Drops {} built on the old history (kept in the backup).",
                plural(scan.stashes.len(), "stash")
            ));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(dialog_title("purge.title", cx).child("Purge from History"))
            .when(!scan.found(), |this| {
                this.child(dialog_description("purge.none", cx).child("Nothing in this repository's history matches."))
            })
            .when(scan.found(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .text_sm()
                        .children(facts),
                )
                .child(
                    div()
                        .p(px(8.))
                        .rounded(px(6.))
                        .bg(c.muted)
                        .text_xs()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child("Commit hashes from the first affected commit onward change, and signatures on rewritten commits and tags are dropped.")
                        .child("Bonsai saves a backup first and never pushes on its own. Anyone who already cloned keeps the old commits, so rotate the secret too."),
                )
            })
            .children(scan.blockers.iter().map(|b| {
                div().text_sm().text_color(c.destructive).child(b.message())
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
            .child(dialog_title("purge.title", cx).child("History Purged"))
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
            .child(
                div()
                    .p(px(8.))
                    .rounded(px(6.))
                    .bg(c.muted)
                    .text_xs()
                    .child("Rotate the secret now. Hosting services can keep old commits in caches, forks and pull request refs even after a force push."),
            )
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
                status(
                    match step {
                        None => "Waiting for other operations to finish…",
                        Some(PurgeStep::Backup) => "Saving a backup of every branch and tag…",
                        Some(PurgeStep::Rewrite) => "Rewriting history…",
                        Some(PurgeStep::Cleanup) => "Expiring reflogs and removing old objects…",
                        Some(PurgeStep::Verify) => "Checking nothing still has the files…",
                    }
                    .to_owned(),
                )
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
}
