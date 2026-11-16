//! Fetch, pull and push for the active repository, with progress and
//! cancel (CONTEXT.md: Sync).

use bonsai_git::{Head, PullMode, PushRequest, Snapshot};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled, Subscription, WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    confirm::{self, Confirm},
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        dialog::{DialogHandle, dialog},
        menu::{Menu, MenuItem, MenuState},
        progress::Progress,
        theme::UiTheme,
    },
    workspace::title_bar::{bar_button, ghost_trigger},
};

/// Where a push of the current branch goes, worked out from the snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushPlan {
    /// Push to the branch's upstream.
    Upstream(PushRequest),
    /// The branch has no upstream: publish it to one of these remotes.
    Publish {
        branch: String,
        remotes: Vec<String>,
    },
    /// Nothing to push: detached, unborn or no remotes.
    Unavailable(&'static str),
}

impl PushPlan {
    #[must_use]
    pub fn from_snapshot(snapshot: &Snapshot) -> Self {
        let Head::Branch { name, oid: Some(_) } = &snapshot.head else {
            return Self::Unavailable("Switch to a branch with commits to push.");
        };
        if snapshot.remotes.is_empty() {
            return Self::Unavailable("Add a remote to push to.");
        }
        let upstream = snapshot
            .branches
            .iter()
            .find(|b| b.name == *name)
            .and_then(|b| b.upstream.as_ref())
            .filter(|u| !u.gone);
        if let Some(upstream) = upstream {
            // The longest remote name that prefixes the upstream wins, since
            // remote names may contain `/`.
            if let Some(remote) = snapshot
                .remotes
                .iter()
                .filter(|r| upstream.name.starts_with(&format!("{}/", r.name)))
                .max_by_key(|r| r.name.len())
            {
                let remote_branch = &upstream.name[remote.name.len() + 1..];
                return Self::Upstream(
                    PushRequest::new(remote.name.clone(), name.clone()).to_branch(remote_branch),
                );
            }
        }
        let mut remotes: Vec<String> = snapshot.remotes.iter().map(|r| r.name.clone()).collect();
        // `origin` first, as Git's default.
        remotes.sort_by_key(|r| (r != "origin", r.clone()));
        Self::Publish {
            branch: name.clone(),
            remotes,
        }
    }
}

pub struct SyncBar {
    model: Entity<RepositoryModel>,
    pull_menu: Entity<MenuState>,
    push_menu: Entity<MenuState>,
    rebase: bool,
    force: DialogHandle,
    force_request: Option<PushRequest>,
    /// Why the last push did nothing.
    notice: Option<&'static str>,
    _events: Subscription,
}

impl SyncBar {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if *event == RepositoryEvent::SnapshotChanged {
                this.rebuild_menus(cx);
            }
            cx.notify();
        });
        let mut this = Self {
            model,
            pull_menu: cx.new(|cx| MenuState::new(Vec::new(), cx)),
            push_menu: cx.new(|cx| MenuState::new(Vec::new(), cx)),
            rebase: crate::settings::get(cx).pull == crate::settings::PullSetting::Rebase,
            force: DialogHandle::new(false),
            force_request: None,
            notice: None,
            _events: events,
        };
        this.rebuild_menus(cx);
        this
    }

    fn snapshot<'a>(&self, cx: &'a gpui_kit::App) -> Option<&'a Snapshot> {
        self.model.read(cx).snapshot()
    }

    pub fn fetch(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        self.model.update(cx, |model, cx| {
            model.run_network(
                "Fetch all remotes",
                |repository, progress, cancel| {
                    repository.fetch(None, true, Some(progress), Some(cancel))
                },
                cx,
            );
        });
    }

    pub fn pull(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        let mode = if self.rebase {
            PullMode::Rebase
        } else if crate::settings::get(cx).pull == crate::settings::PullSetting::FastForwardOnly {
            PullMode::FastForwardOnly
        } else {
            PullMode::Merge
        };
        let name = match mode {
            PullMode::Rebase => "Pull with rebase",
            PullMode::FastForwardOnly => "Pull (fast-forward only)",
            PullMode::Merge => "Pull",
        };
        self.model.update(cx, |model, cx| {
            model.run_network(
                name,
                move |repository, progress, cancel| {
                    repository.pull(mode, Some(progress), Some(cancel))
                },
                cx,
            );
        });
    }

    fn run_push(&mut self, request: PushRequest, cx: &mut Context<Self>) {
        let name = format!(
            "{} {} to {}",
            if request.force {
                "Force push"
            } else if request.set_upstream {
                "Publish"
            } else {
                "Push"
            },
            request.branch,
            request.remote
        );
        self.model.update(cx, |model, cx| {
            model.run_network(
                name,
                move |repository, progress, cancel| {
                    repository.push(&request, Some(progress), Some(cancel))
                },
                cx,
            );
        });
    }

    /// Pushes the current branch, publishing it to the first remote
    /// (`origin` when there is one) when it has no upstream. Returns why
    /// nothing was pushed.
    pub fn push(&mut self, cx: &mut Context<Self>) -> Option<&'static str> {
        let plan = self.snapshot(cx).map(PushPlan::from_snapshot)?;
        match plan {
            PushPlan::Upstream(request) => self.run_push(request, cx),
            PushPlan::Publish { branch, remotes } => {
                self.run_push(
                    PushRequest::new(remotes[0].clone(), branch).set_upstream(),
                    cx,
                );
            }
            PushPlan::Unavailable(reason) => return Some(reason),
        }
        None
    }

    /// Asks before a force push with lease to the upstream.
    pub fn confirm_force_push(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(PushPlan::Upstream(request)) = self.snapshot(cx).map(PushPlan::from_snapshot) {
            self.force_request = Some(request.force_with_lease());
            self.force.open(window, cx);
        }
    }

    fn rebuild_menus(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let pull = vec![
            MenuItem::new(
                "sync.pull.merge",
                if self.rebase { "Merge" } else { "Merge ✓" },
            )
            .on_click(set_rebase(&this, false)),
            MenuItem::new(
                "sync.pull.rebase",
                if self.rebase { "Rebase ✓" } else { "Rebase" },
            )
            .on_click(set_rebase(&this, true)),
        ];
        self.pull_menu
            .update(cx, |menu, cx| menu.set_items(pull, cx));

        let plan = self.snapshot(cx).map(PushPlan::from_snapshot);
        let mut push = Vec::new();
        match plan {
            Some(PushPlan::Upstream(request)) => {
                let target = format!(
                    "{}/{}",
                    request.remote,
                    request.remote_branch.as_deref().unwrap_or(&request.branch)
                );
                push.push(
                    MenuItem::new("sync.push.upstream", format!("Push to {target}"))
                        .icon(LucideIcon::ArrowUp)
                        .on_click(with(&this, move |bar, _, cx| {
                            bar.run_push(request.clone(), cx);
                        })),
                );
                push.push(MenuItem::separator());
                push.push(
                    MenuItem::new("sync.push.force", "Force Push with Lease…")
                        .icon(LucideIcon::TriangleAlert)
                        .on_click(with(&this, |bar, window, cx| {
                            bar.confirm_force_push(window, cx);
                        })),
                );
            }
            Some(PushPlan::Publish { branch, remotes }) => {
                for (i, remote) in remotes.into_iter().enumerate() {
                    let request = PushRequest::new(remote.clone(), branch.clone()).set_upstream();
                    push.push(
                        MenuItem::new(
                            SharedString::from(format!("sync.publish.{i}")),
                            format!("Publish {branch} to {remote}"),
                        )
                        .icon(LucideIcon::Upload)
                        .on_click(with(&this, move |bar, _, cx| {
                            bar.run_push(request.clone(), cx);
                        })),
                    );
                }
            }
            Some(PushPlan::Unavailable(reason)) => {
                push.push(MenuItem::new("sync.push.unavailable", reason).disabled(true));
            }
            None => {}
        }
        self.push_menu
            .update(cx, |menu, cx| menu.set_items(push, cx));
    }

    fn render_force(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let target = self
            .force_request
            .as_ref()
            .map(|r| {
                format!(
                    "{}/{}",
                    r.remote,
                    r.remote_branch.as_deref().unwrap_or(&r.branch)
                )
            })
            .unwrap_or_default();
        let popup = confirm::popup(
            "sync.force",
            format!("Force push to {target}?"),
            "This replaces the remote branch with yours. Bonsai uses a lease: if someone pushed commits you haven't fetched, the push stops instead of deleting their work.",
            cx,
        )
        .child(
            Confirm::new(
                "sync.force",
                "Force Push",
                |this: &mut Self, window, cx| {
                    this.force_request = None;
                    this.force.close(window, cx);
                },
                |this, window, cx| {
                    this.force.close(window, cx);
                    if let Some(request) = this.force_request.take() {
                        this.run_push(request, cx);
                    }
                },
            )
            .footer(cx),
        );
        dialog("sync.force", &self.force, popup, window, cx)
    }
}

fn with(
    this: &WeakEntity<SyncBar>,
    f: impl Fn(&mut SyncBar, &mut Window, &mut Context<SyncBar>) + 'static,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    let this = this.clone();
    move |_, window, cx| {
        let _ = this.update(cx, |bar, cx| f(bar, window, cx));
    }
}

fn set_rebase(
    this: &WeakEntity<SyncBar>,
    rebase: bool,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    with(this, move |bar, _, cx| {
        bar.rebase = rebase;
        bar.rebuild_menus(cx);
        cx.notify();
    })
}

fn plural(n: u32) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// A button and its options menu, drawn as one control.
fn split(button: impl IntoElement, menu: impl IntoElement) -> impl IntoElement {
    div().flex().items_center().child(button).child(menu)
}

impl Render for SyncBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let model = self.model.read(cx);
        let running = model.running().cloned();
        let progress = model.progress();
        let can_cancel = model.can_cancel();
        let (ahead, behind) = model
            .snapshot()
            .and_then(|s| match &s.head {
                Head::Branch { name, .. } => s.branches.iter().find(|b| b.name == *name),
                Head::Detached { .. } => None,
            })
            .and_then(|b| b.upstream.as_ref())
            .map_or((0, 0), |u| (u.ahead, u.behind));
        let icon = |name: LucideIcon| lucide(name).size(px(14.)).text_color(c.foreground);
        let count = |n: u32| {
            (n > 0).then(|| {
                div()
                    .px(px(5.))
                    .rounded(px(8.))
                    .bg(c.primary)
                    .text_color(c.primary_foreground)
                    .text_xs()
                    .child(n.to_string())
            })
        };
        let chevron = || {
            lucide(LucideIcon::ChevronDown)
                .size(px(12.))
                .text_color(c.muted_foreground)
        };
        let busy = running.is_some();

        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .when_some(self.notice.filter(|_| !busy), |this, notice| {
                this.child(
                    div()
                        .mr(px(8.))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(notice),
                )
            })
            .when_some(running, |this, running| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .mr(px(8.))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(progress.as_ref().map_or_else(
                            || format!("{running}…"),
                            |p| format!("{running}: {}", p.phase),
                        ))
                        .when_some(progress.and_then(|p| p.percent), |this, percent| {
                            this.child(
                                div().w(px(80.)).child(
                                    Progress::new("sync.progress").value(f64::from(percent)),
                                ),
                            )
                        })
                        .when(can_cancel, |this| {
                            this.child(
                                Button::new("sync.cancel")
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Xs)
                                    .label("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.model.update(cx, |model, _| model.cancel_running());
                                    })),
                            )
                        }),
                )
            })
            .child(
                bar_button("sync.fetch", "Fetch all remotes (⌥⌘F)", busy, &theme)
                    .child(icon(LucideIcon::RefreshCw))
                    .child("Fetch")
                    .on_click(cx.listener(|this, _, _, cx| this.fetch(cx))),
            )
            .child(split(
                bar_button(
                    "sync.pull",
                    if behind > 0 {
                        format!("Pull {behind} commit{} (⌥⌘L)", plural(behind))
                    } else {
                        "Pull (⌥⌘L)".to_owned()
                    },
                    busy,
                    &theme,
                )
                .child(icon(LucideIcon::ArrowDown))
                .child(if self.rebase { "Pull (Rebase)" } else { "Pull" })
                .children(count(behind))
                .on_click(cx.listener(|this, _, _, cx| this.pull(cx))),
                Menu::new(&self.pull_menu, "Pull options")
                    .trigger_style_with(ghost_trigger(&theme, ButtonSize::IconSm))
                    .trigger(chevron()),
            ))
            .child(split(
                bar_button(
                    "sync.push",
                    if ahead > 0 {
                        format!("Push {ahead} commit{} (⌥⌘P)", plural(ahead))
                    } else {
                        "Push (⌥⌘P)".to_owned()
                    },
                    busy,
                    &theme,
                )
                .child(icon(LucideIcon::ArrowUp))
                .child("Push")
                .children(count(ahead))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.notice = this.push(cx);
                    cx.notify();
                })),
                Menu::new(&self.push_menu, "Push options")
                    .trigger_style_with(ghost_trigger(&theme, ButtonSize::IconSm))
                    .trigger(chevron()),
            ))
            .child(self.render_force(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Branch, Git, Oid, Remote, Repository, Tracking};
    use gpui_kit::TestAppContext;

    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let g = Git::locate(None).unwrap();
        g.run(&g.command(args.iter().copied()).cwd(dir)).unwrap();
    }

    #[gpui_kit::test]
    fn publishes_then_pushes_to_the_upstream(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "-q", "--bare"]);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repository = Repository::init(&Git::locate(None).unwrap(), root, Some("main")).unwrap();
        for (key, value) in [
            ("user.name", "Bonsai"),
            ("user.email", "bonsai@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(root, &["config", key, value]);
        }
        git(root, &["commit", "-q", "--allow-empty", "-m", "base"]);
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (bar, cx) = cx.add_window_view(|_, cx| SyncBar::new(model.clone(), cx));
        cx.run_until_parked();

        let reason = bar.update(cx, SyncBar::push);
        assert_eq!(reason, Some("Add a remote to push to."));

        git(
            root,
            &["remote", "add", "origin", remote.path().to_str().unwrap()],
        );
        model.update(cx, RepositoryModel::refresh);
        cx.run_until_parked();
        assert_eq!(bar.update(cx, SyncBar::push), None);
        cx.run_until_parked();
        let upstream = |cx: &mut gpui_kit::VisualTestContext| {
            model.read_with(cx, |model, _| {
                model.snapshot().unwrap().branches[0]
                    .upstream
                    .as_ref()
                    .map(|u| (u.name.clone(), u.ahead))
            })
        };
        assert_eq!(upstream(cx), Some(("origin/main".into(), 0)));

        git(root, &["commit", "-q", "--allow-empty", "-m", "more"]);
        model.update(cx, RepositoryModel::refresh);
        cx.run_until_parked();
        assert_eq!(upstream(cx), Some(("origin/main".into(), 1)));
        bar.update(cx, SyncBar::push);
        cx.run_until_parked();
        assert_eq!(upstream(cx), Some(("origin/main".into(), 0)));
        bar.update(cx, SyncBar::fetch);
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            assert!(model.running().is_none());
            assert!(model.progress().is_none());
        });
    }

    fn snapshot(upstream: Option<&str>, remotes: &[&str]) -> Snapshot {
        Snapshot {
            head: Head::Branch {
                name: "main".into(),
                oid: Some(Oid::new("a".repeat(40))),
            },
            changes: Vec::new(),
            branches: vec![Branch {
                name: "main".into(),
                oid: Oid::new("a".repeat(40)),
                is_head: true,
                upstream: upstream.map(|name| Tracking {
                    name: name.into(),
                    gone: false,
                    ahead: 1,
                    behind: 0,
                }),
            }],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            remotes: remotes
                .iter()
                .map(|name| Remote {
                    name: (*name).into(),
                    fetch_url: None,
                    push_url: None,
                })
                .collect(),
            stashes: Vec::new(),
            worktrees: Vec::new(),
            operation: None,
        }
    }

    #[test]
    fn pushes_go_to_the_upstream_or_publish() {
        assert_eq!(
            PushPlan::from_snapshot(&snapshot(
                Some("team/origin/trunk"),
                &["team", "team/origin"]
            )),
            PushPlan::Upstream(PushRequest::new("team/origin", "main").to_branch("trunk"))
        );
        assert_eq!(
            PushPlan::from_snapshot(&snapshot(None, &["fork", "origin"])),
            PushPlan::Publish {
                branch: "main".into(),
                remotes: vec!["origin".into(), "fork".into()],
            }
        );
        assert!(matches!(
            PushPlan::from_snapshot(&snapshot(None, &[])),
            PushPlan::Unavailable(_)
        ));
    }
}
