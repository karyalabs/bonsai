//! The Reflog view: everywhere HEAD has been, newest first, so a commit
//! lost to a reset, an amend, a rebase or a deleted branch can be found and
//! restored as a branch.

use std::time::{Duration, SystemTime};

use bonsai_git::{Head, Oid, ReflogAction, ReflogEntry, ResetMode};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    activity_panel::ago,
    refs::{Checkout, RefDialog, RefsPanel},
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        context_menu::context_menu,
        menu::{MenuItem, MenuState},
        switch::Switch,
        theme::UiTheme,
    },
};

/// Moves of HEAD the view reads at most.
pub const LIMIT: usize = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReflogEvent {
    /// Show this commit's diff, or nothing.
    Selected(Option<(Oid, SharedString)>),
    /// Show this commit in History.
    ShowInHistory(Oid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Idle,
    Loading,
    Ready,
    Failed(SharedString),
}

pub struct ReflogView {
    model: Entity<RepositoryModel>,
    refs: Entity<RefsPanel>,
    entries: Vec<ReflogEntry>,
    status: Status,
    /// The selected move, by its `HEAD@{n}` index.
    selected: Option<usize>,
    /// Shows only moves to commits nothing else reaches.
    only_lost: bool,
    /// Whether the view is shown, so it reloads only when it is.
    active: bool,
    /// Whether the repository changed since the last read.
    stale: bool,
    menu: Entity<MenuState>,
    load: Option<Task<()>>,
    _model: Subscription,
}

impl EventEmitter<ReflogEvent> for ReflogView {}

impl ReflogView {
    pub fn new(
        model: Entity<RepositoryModel>,
        refs: Entity<RefsPanel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if matches!(event, RepositoryEvent::SnapshotChanged) {
                this.stale = true;
                if this.active {
                    this.load(cx);
                }
            }
        });
        Self {
            model,
            refs,
            entries: Vec::new(),
            status: Status::Idle,
            selected: None,
            only_lost: false,
            active: false,
            stale: true,
            menu: cx.new(|cx| MenuState::new(Vec::new(), cx)),
            load: None,
            _model: subscription,
        }
    }

    /// Called when the view is shown: reads the reflog if it changed, and
    /// shows the selected commit again.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if self.stale {
            self.load(cx);
        }
        self.emit_selection(cx);
    }

    /// Called when another view is shown.
    pub fn deactivate(&mut self) {
        self.active = false;
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.stale = false;
        if self.status == Status::Idle {
            self.status = Status::Loading;
        }
        let repository = self.model.read(cx).repository().clone();
        let task = cx.background_spawn(async move { repository.reflog(LIMIT) });
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        this.status = Status::Ready;
                        // Keep the selection on the same commit when the
                        // list shifts under it.
                        if let Some(index) = this.selected
                            && this.entries.get(index).is_none()
                        {
                            this.selected = None;
                        }
                    }
                    Err(error) => this.status = Status::Failed(error.to_string().into()),
                }
                this.rebuild_menu(cx);
                cx.notify();
            });
        }));
    }

    #[must_use]
    pub fn entries(&self) -> &[ReflogEntry] {
        &self.entries
    }

    /// The moves the list shows.
    fn shown(&self) -> impl Iterator<Item = &ReflogEntry> {
        self.entries
            .iter()
            .filter(move |e| !self.only_lost || e.lost)
    }

    #[must_use]
    pub fn selected(&self) -> Option<&ReflogEntry> {
        self.selected.and_then(|i| self.entries.get(i))
    }

    pub fn select(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        if self.selected == index {
            return;
        }
        self.selected = index;
        self.rebuild_menu(cx);
        self.emit_selection(cx);
        cx.notify();
    }

    pub fn set_only_lost(&mut self, only_lost: bool, cx: &mut Context<Self>) {
        self.only_lost = only_lost;
        if only_lost && self.selected().is_some_and(|e| !e.lost) {
            self.select(None, cx);
        }
        cx.notify();
    }

    pub fn emit_selection(&mut self, cx: &mut Context<Self>) {
        let selected = self
            .selected()
            .map(|e| (e.oid.clone(), SharedString::from(e.summary.clone())));
        cx.emit(ReflogEvent::Selected(selected));
    }

    fn current_branch(&self, cx: &gpui_kit::App) -> Option<String> {
        match &self.model.read(cx).snapshot()?.head {
            Head::Branch { name, .. } => Some(name.clone()),
            Head::Detached { .. } => None,
        }
    }

    /// Asks for a name and makes a branch at the selected commit.
    pub fn restore_as_branch(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected() else {
            return;
        };
        let start = entry.oid.as_str().to_owned();
        self.refs.update(cx, |refs, cx| {
            refs.show(RefDialog::CreateBranch { start: Some(start) }, cx);
        });
    }

    /// Cherry-picks the selected commit onto the current branch.
    pub fn cherry_pick(&mut self, cx: &mut Context<Self>) {
        let Some(oid) = self.selected().map(|e| e.oid.clone()) else {
            return;
        };
        let name = format!("Cherry-pick {}", short(&oid));
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.cherry_pick(&[oid.as_str()]).map(drop),
                cx,
            );
        });
    }

    fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.selected().cloned() else {
            self.menu
                .update(cx, |menu, cx| menu.set_items(Vec::new(), cx));
            return;
        };
        let this = cx.entity().downgrade();
        let refs = self.refs.downgrade();
        let hash = entry.oid.as_str().to_owned();
        let with_refs = |f: RefsAction| {
            let refs = refs.clone();
            move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
                let _ = refs.update(cx, |refs, cx| f(refs, cx));
            }
        };
        let mut items = vec![
            MenuItem::new("reflog.restore", "Restore as Branch…")
                .icon(LucideIcon::GitBranchPlus)
                .on_click({
                    let this = this.clone();
                    move |_, _, cx| {
                        let _ = this.update(cx, Self::restore_as_branch);
                    }
                }),
            MenuItem::new("reflog.checkout", "Check Out (Detached)")
                .icon(LucideIcon::GitCommitHorizontal)
                .on_click(with_refs({
                    let hash = hash.clone();
                    Box::new(move |refs, cx| refs.checkout(Checkout::Detached(hash.clone()), cx))
                })),
        ];
        if !entry.lost {
            let oid = entry.oid.clone();
            let this = this.clone();
            items.push(
                MenuItem::new("reflog.history", "Show in History")
                    .icon(LucideIcon::GitGraph)
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |_, cx| {
                            cx.emit(ReflogEvent::ShowInHistory(oid.clone()));
                        });
                    }),
            );
        }
        items.push(
            MenuItem::new("reflog.copy", "Copy Hash")
                .icon(LucideIcon::Copy)
                .on_click({
                    let hash = hash.clone();
                    move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()))
                }),
        );
        if let Some(current) = self.current_branch(cx) {
            let reset = |id: &'static str, label: &'static str, mode: ResetMode| {
                let hash = hash.clone();
                MenuItem::new(id, label).on_click(with_refs(Box::new(move |refs, cx| {
                    refs.reset(hash.clone(), mode, cx);
                })))
            };
            items.extend([
                MenuItem::separator(),
                MenuItem::new("reflog.cherry-pick", format!("Cherry-pick onto {current}"))
                    .icon(LucideIcon::Cherry)
                    .on_click({
                        let this = this.clone();
                        move |_, _, cx| {
                            let _ = this.update(cx, Self::cherry_pick);
                        }
                    }),
                MenuItem::submenu(
                    "reflog.reset",
                    format!("Reset {current} to Here"),
                    vec![
                        reset(
                            "reflog.reset.soft",
                            "Soft: keep changes staged",
                            ResetMode::Soft,
                        ),
                        reset(
                            "reflog.reset.mixed",
                            "Mixed: keep changes unstaged",
                            ResetMode::Mixed,
                        ),
                        MenuItem::new("reflog.reset.hard", "Hard: discard changes…").on_click(
                            with_refs({
                                let hash = hash.clone();
                                Box::new(move |refs, cx| {
                                    refs.show(
                                        RefDialog::ResetHard {
                                            target: hash.clone(),
                                        },
                                        cx,
                                    );
                                })
                            }),
                        ),
                    ],
                )
                .icon(LucideIcon::RotateCcw),
            ]);
        }
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }
}

/// A menu item's work on the refs panel.
type RefsAction = Box<dyn Fn(&mut RefsPanel, &mut Context<RefsPanel>)>;

fn short(oid: &Oid) -> String {
    oid.as_str().chars().take(7).collect()
}

fn icon(action: ReflogAction) -> LucideIcon {
    match action {
        ReflogAction::Commit => LucideIcon::GitCommitHorizontal,
        ReflogAction::Amend => LucideIcon::PencilLine,
        ReflogAction::Checkout => LucideIcon::ArrowRightLeft,
        ReflogAction::Reset => LucideIcon::RotateCcw,
        ReflogAction::Rebase => LucideIcon::ListChecks,
        ReflogAction::Merge => LucideIcon::GitMerge,
        ReflogAction::Pull => LucideIcon::ArrowDown,
        ReflogAction::CherryPick => LucideIcon::Cherry,
        ReflogAction::Revert => LucideIcon::Undo2,
        ReflogAction::Other => LucideIcon::Dot,
    }
}

fn time(seconds: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

impl Render for ReflogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let lost_count = self.entries.iter().filter(|e| e.lost).count();
        let mut list = div()
            .id("reflog.list")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .py(px(4.));
        let shown: Vec<ReflogEntry> = self.shown().cloned().collect();
        for entry in shown {
            let index = entry.index;
            let is_selected = self.selected == Some(index);
            let color = if entry.lost {
                c.warning
            } else {
                c.muted_foreground
            };
            list = list.child(
                div()
                    .id(("reflog.row", index))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .mx(px(6.))
                    .my(px(1.))
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(is_selected, |this| this.bg(c.accent))
                    .hover(|s| s.bg(c.muted))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(Some(index), cx)))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| this.select(Some(index), cx)),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .size(px(26.))
                            .rounded(px(999.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(color.opacity(0.14))
                            .child(lucide(icon(entry.action)).size(px(14.)).text_color(color)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(1.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .child(entry.describe()),
                                    )
                                    .when(entry.lost, |this| {
                                        this.child(
                                            div()
                                                .flex_shrink_0()
                                                .px(px(6.))
                                                .rounded(px(999.))
                                                .bg(c.warning.opacity(0.15))
                                                .text_color(c.warning)
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .child("Not on any branch"),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_xs()
                                    .text_color(c.muted_foreground)
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .font_family(theme.fonts.mono.clone())
                                            .child(short(&entry.oid)),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_ellipsis()
                                            .child(entry.summary.clone()),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap(px(1.))
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(ago(time(entry.time)))
                            .child(
                                div()
                                    .font_family(theme.fonts.mono.clone())
                                    .child(format!("HEAD@{{{index}}}")),
                            ),
                    ),
            );
        }
        let empty = match &self.status {
            Status::Idle | Status::Loading => Some(("Reading the reflog…".to_owned(), None)),
            Status::Failed(error) => Some((error.to_string(), None)),
            Status::Ready if self.entries.is_empty() => Some((
                "Nothing yet".to_owned(),
                Some(
                    "Every commit, switch, reset and rebase appears here, so work they replace can be found again.",
                ),
            )),
            Status::Ready if self.only_lost && lost_count == 0 => Some((
                "No lost commits".to_owned(),
                Some("Every commit HEAD has been on is still on a branch, tag or remote."),
            )),
            Status::Ready => None,
        };
        let body = match empty {
            Some((title, detail)) => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .px(px(24.))
                .py(px(48.))
                .child(
                    lucide(LucideIcon::ArchiveRestore)
                        .size(px(22.))
                        .text_color(c.muted_foreground),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(title),
                )
                .when_some(detail, |this, detail| {
                    this.child(
                        div()
                            .max_w(px(300.))
                            .text_center()
                            .text_sm()
                            .text_color(c.muted_foreground)
                            .child(detail),
                    )
                })
                .into_any_element(),
            None => context_menu(&self.menu, "Reflog actions", list)
                .size_full()
                .into_any_element(),
        };
        let restore = self.selected().map(|e| e.lost);
        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(44.))
            .px(px(12.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Reflog"),
                    )
                    .when(lost_count > 0, |this| {
                        this.child(div().text_xs().text_color(c.muted_foreground).child(
                            if lost_count == 1 {
                                "1 lost commit".to_owned()
                            } else {
                                format!("{lost_count} lost commits")
                            },
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("Only Lost")
                    .child(
                        Switch::new("reflog.only-lost")
                            .checked(self.only_lost)
                            .aria_label("Show only lost commits")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_only_lost(*checked, cx);
                            })),
                    ),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
            .when_some(restore, |this, lost| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(c.border)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(if lost {
                                    "Git deletes this commit when the reflog expires. Restore it to keep it."
                                } else {
                                    "This commit is still on a branch, tag or remote."
                                }),
                        )
                        .child(
                            div().flex_shrink_0().child(
                            Button::new("reflog.restore-button")
                                .variant(if lost {
                                    ButtonVariant::Default
                                } else {
                                    ButtonVariant::Outline
                                })
                                .size(ButtonSize::Sm)
                                .child(lucide(LucideIcon::GitBranchPlus).size(px(14.)).text_color(
                                    if lost {
                                        c.primary_foreground
                                    } else {
                                        c.foreground
                                    },
                                ))
                                .label("Restore as Branch…")
                                .on_click(cx.listener(|this, _, _, cx| this.restore_as_branch(cx))),
                            ),
                        ),
                )
            })
    }
}
