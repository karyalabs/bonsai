//! The Compare view: two revisions side by side, as a pull request from one
//! into the other would see them. It lists the commits each has that the
//! other lacks, and shows either one commit's diff or everything the head
//! changed since they split.

use std::time::{Duration, SystemTime};

use bonsai_git::{CommitInfo, Comparison, DiffTarget, Head, Oid, Snapshot};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    activity_panel::ago,
    avatar::avatar,
    refs::RefsPanel,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant, style_button},
        context_menu::context_menu,
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
    },
};

/// Commits listed at most on each side.
pub const LIMIT: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompareEvent {
    /// Show this diff, with this title, or nothing.
    Selected(Option<(DiffTarget, SharedString)>),
    /// Show this commit in History.
    ShowInHistory(Oid),
}

/// What the detail pane shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    /// Everything the head changed since the merge base.
    AllChanges,
    Commit(Oid),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for both sides to be chosen.
    Idle,
    Loading,
    Ready(Box<Comparison>),
    Failed(SharedString),
}

/// Which side a picker chooses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Base,
    Head,
}

pub struct CompareView {
    model: Entity<RepositoryModel>,
    refs: Entity<RefsPanel>,
    base: Option<String>,
    head: Option<String>,
    status: Status,
    selected: Option<Selection>,
    active: bool,
    stale: bool,
    base_menu: Entity<MenuState>,
    head_menu: Entity<MenuState>,
    menu: Entity<MenuState>,
    load: Option<Task<()>>,
    _model: Subscription,
}

impl EventEmitter<CompareEvent> for CompareView {}

impl CompareView {
    pub fn new(
        model: Entity<RepositoryModel>,
        refs: Entity<RefsPanel>,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if matches!(event, RepositoryEvent::SnapshotChanged) {
                this.stale = true;
                this.rebuild_pickers(cx);
                if this.active {
                    this.load(cx);
                }
            }
        });
        let mut view = Self {
            model,
            refs,
            base: None,
            head: None,
            status: Status::Idle,
            selected: None,
            active: false,
            stale: true,
            base_menu: cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(260.))),
            head_menu: cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(260.))),
            menu: cx.new(|cx| MenuState::new(Vec::new(), cx)),
            load: None,
            _model: subscription,
        };
        view.rebuild_pickers(cx);
        view
    }

    #[must_use]
    pub fn base(&self) -> Option<&str> {
        self.base.as_deref()
    }

    #[must_use]
    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    #[must_use]
    pub fn comparison(&self) -> Option<&Comparison> {
        match &self.status {
            Status::Ready(comparison) => Some(comparison),
            _ => None,
        }
    }

    #[must_use]
    pub fn selected(&self) -> Option<&Selection> {
        self.selected.as_ref()
    }

    /// Called when the view is shown. With nothing chosen yet, compares
    /// the current branch with the repository's main branch.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        self.active = true;
        if self.base.is_none() && self.head.is_none() {
            let (base, head) = self.defaults(cx);
            self.base = base;
            self.head = head;
            self.rebuild_pickers(cx);
            self.stale = true;
        }
        if self.stale {
            self.load(cx);
        }
        self.emit_selection(cx);
    }

    pub fn deactivate(&mut self) {
        self.active = false;
    }

    /// The main branch (`main`, `master`, or the first one), and the
    /// current branch when it is another one.
    fn defaults(&self, cx: &gpui_kit::App) -> (Option<String>, Option<String>) {
        let Some(snapshot) = self.model.read(cx).snapshot() else {
            return (None, None);
        };
        let names: Vec<&str> = snapshot.branches.iter().map(|b| b.name.as_str()).collect();
        let main = ["main", "master", "trunk", "develop"]
            .into_iter()
            .find(|n| names.contains(n))
            .or_else(|| names.first().copied())
            .map(str::to_owned);
        let current = match &snapshot.head {
            Head::Branch { name, .. } => Some(name.clone()),
            Head::Detached { .. } => None,
        };
        match (main, current) {
            (Some(main), Some(current)) if main != current => (Some(main), Some(current)),
            (main, _) => (main, None),
        }
    }

    /// Compares `head` against `base`.
    pub fn set(&mut self, base: Option<String>, head: Option<String>, cx: &mut Context<Self>) {
        self.base = base;
        self.head = head;
        self.selected = None;
        self.rebuild_pickers(cx);
        self.load(cx);
        self.emit_selection(cx);
        cx.notify();
    }

    /// Swaps the base and the head.
    pub fn swap(&mut self, cx: &mut Context<Self>) {
        self.set(self.head.clone(), self.base.clone(), cx);
    }

    fn set_side(&mut self, side: Side, revision: String, cx: &mut Context<Self>) {
        match side {
            Side::Base => self.set(Some(revision), self.head.clone(), cx),
            Side::Head => self.set(self.base.clone(), Some(revision), cx),
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        self.stale = false;
        let (Some(base), Some(head)) = (self.base.clone(), self.head.clone()) else {
            self.status = Status::Idle;
            self.load = None;
            cx.notify();
            return;
        };
        if !matches!(self.status, Status::Ready(_)) {
            self.status = Status::Loading;
        }
        let repository = self.model.read(cx).repository().clone();
        let task = cx.background_spawn(async move { repository.compare(&base, &head, LIMIT) });
        self.load = Some(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(comparison) => {
                        let gone = match &this.selected {
                            Some(Selection::Commit(oid)) => !comparison
                                .ahead
                                .iter()
                                .chain(&comparison.behind)
                                .any(|c| &c.oid == oid),
                            _ => false,
                        };
                        // With nothing new on the head, show the first
                        // commit it lacks instead of an empty diff.
                        let first = if comparison.ahead_count > 0 {
                            Some(Selection::AllChanges)
                        } else {
                            comparison
                                .behind
                                .first()
                                .map(|c| Selection::Commit(c.oid.clone()))
                        };
                        let all_empty = this.selected == Some(Selection::AllChanges)
                            && comparison.ahead_count == 0;
                        this.status = Status::Ready(Box::new(comparison));
                        if this.selected.is_none() || gone || all_empty {
                            this.selected = first;
                        }
                    }
                    Err(error) => {
                        this.status = Status::Failed(error.to_string().into());
                        this.selected = None;
                    }
                }
                this.rebuild_menu(cx);
                if this.active {
                    this.emit_selection(cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn select(&mut self, selection: Option<Selection>, cx: &mut Context<Self>) {
        if self.selected == selection {
            return;
        }
        self.selected = selection;
        self.rebuild_menu(cx);
        self.emit_selection(cx);
        cx.notify();
    }

    fn find(&self, oid: &Oid) -> Option<&CommitInfo> {
        let comparison = self.comparison()?;
        comparison
            .ahead
            .iter()
            .chain(&comparison.behind)
            .find(|c| &c.oid == oid)
    }

    pub fn emit_selection(&mut self, cx: &mut Context<Self>) {
        let shown = match (&self.selected, self.comparison()) {
            (Some(Selection::AllChanges), Some(comparison)) => {
                comparison.merge_base.clone().map(|from| {
                    let title = format!(
                        "{} since {}",
                        self.head.clone().unwrap_or_default(),
                        self.base.clone().unwrap_or_default()
                    );
                    (
                        DiffTarget::Range {
                            from,
                            to: comparison.head_oid.clone(),
                        },
                        SharedString::from(title),
                    )
                })
            }
            (Some(Selection::Commit(oid)), Some(_)) => self.find(oid).map(|commit| {
                (
                    DiffTarget::Commit(oid.clone()),
                    SharedString::from(commit.summary.clone()),
                )
            }),
            _ => None,
        };
        cx.emit(CompareEvent::Selected(shown));
    }

    fn current_branch(&self, cx: &gpui_kit::App) -> Option<String> {
        match &self.model.read(cx).snapshot()?.head {
            Head::Branch { name, .. } => Some(name.clone()),
            Head::Detached { .. } => None,
        }
    }

    /// Merges the head into the base, when the base is the current branch.
    pub fn merge(&mut self, cx: &mut Context<Self>) {
        let Some(head) = self.head.clone() else {
            return;
        };
        if self.base.is_none() || self.base != self.current_branch(cx) {
            return;
        }
        self.refs.update(cx, |refs, cx| refs.merge(head, cx));
    }

    /// Cherry-picks a commit onto the current branch.
    pub fn cherry_pick(&mut self, oid: Oid, cx: &mut Context<Self>) {
        let name = format!("Cherry-pick {}", short(&oid));
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.cherry_pick(&[oid.as_str()]).map(drop),
                cx,
            );
        });
    }

    fn rebuild_pickers(&mut self, cx: &mut Context<Self>) {
        let snapshot = self.model.read(cx).snapshot().cloned();
        for side in [Side::Base, Side::Head] {
            let current = match side {
                Side::Base => self.base.clone(),
                Side::Head => self.head.clone(),
            };
            let items = picker_items(side, current.as_deref(), snapshot.as_ref(), cx);
            let menu = match side {
                Side::Base => &self.base_menu,
                Side::Head => &self.head_menu,
            };
            menu.update(cx, |menu, cx| menu.set_items(items, cx));
        }
    }

    fn rebuild_menu(&mut self, cx: &mut Context<Self>) {
        let Some(Selection::Commit(oid)) = self.selected.clone() else {
            self.menu
                .update(cx, |menu, cx| menu.set_items(Vec::new(), cx));
            return;
        };
        let this = cx.entity().downgrade();
        let hash = oid.as_str().to_owned();
        let mut items = vec![
            MenuItem::new("compare.history", "Show in History")
                .icon(LucideIcon::GitGraph)
                .on_click({
                    let this = this.clone();
                    let oid = oid.clone();
                    move |_, _, cx| {
                        let _ = this.update(cx, |_, cx| {
                            cx.emit(CompareEvent::ShowInHistory(oid.clone()));
                        });
                    }
                }),
            MenuItem::new("compare.copy", "Copy Hash")
                .icon(LucideIcon::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(hash.clone()));
                }),
        ];
        if let Some(current) = self.current_branch(cx) {
            let merge = self.find(&oid).is_some_and(|c| c.parents.len() > 1);
            items.extend([
                MenuItem::separator(),
                MenuItem::new("compare.cherry-pick", format!("Cherry-pick onto {current}"))
                    .icon(LucideIcon::Cherry)
                    .disabled(merge)
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, |view, cx| view.cherry_pick(oid.clone(), cx));
                    }),
            ]);
        }
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }
}

/// The branches, remote branches and tags a picker offers.
fn picker_items(
    side: Side,
    current: Option<&str>,
    snapshot: Option<&Snapshot>,
    cx: &mut Context<CompareView>,
) -> Vec<MenuItem> {
    let Some(snapshot) = snapshot else {
        return Vec::new();
    };
    let this = cx.entity().downgrade();
    let prefix = match side {
        Side::Base => "compare.base",
        Side::Head => "compare.head",
    };
    let pick = |revision: String| {
        let this = this.clone();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut gpui_kit::App| {
            let _ = this.update(cx, |view, cx| view.set_side(side, revision.clone(), cx));
        }
    };
    let mut items = Vec::new();
    let group =
        |label: &'static str, icon: LucideIcon, names: Vec<String>, items: &mut Vec<MenuItem>| {
            if names.is_empty() {
                return;
            }
            if !items.is_empty() {
                items.push(MenuItem::separator());
            }
            items.push(MenuItem::label(
                SharedString::from(format!("{prefix}.{label}")),
                label,
            ));
            for name in names {
                items.push(
                    MenuItem::new(
                        SharedString::from(format!("{prefix}.{label}.{name}")),
                        name.clone(),
                    )
                    .icon(icon)
                    .checked(current == Some(name.as_str()))
                    .on_click(pick(name)),
                );
            }
        };
    group(
        "Branches",
        LucideIcon::GitBranch,
        snapshot.branches.iter().map(|b| b.name.clone()).collect(),
        &mut items,
    );
    group(
        "Remote Branches",
        LucideIcon::Cloud,
        snapshot
            .remote_branches
            .iter()
            .filter(|b| b.name != "HEAD")
            .map(|b| format!("{}/{}", b.remote, b.name))
            .collect(),
        &mut items,
    );
    group(
        "Tags",
        LucideIcon::Tag,
        snapshot.tags.iter().map(|t| t.name.clone()).collect(),
        &mut items,
    );
    items
}

fn short(oid: &Oid) -> String {
    oid.as_str().chars().take(7).collect()
}

fn time(seconds: i64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).unwrap_or(0))
}

fn commits(count: usize) -> String {
    if count == 1 {
        "1 commit".to_owned()
    } else {
        format!("{count} commits")
    }
}

impl CompareView {
    fn render_commit(
        &self,
        commit: &CommitInfo,
        theme: &crate::ui::theme::UiTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = &theme.colors;
        let oid = commit.oid.clone();
        let selected = self.selected == Some(Selection::Commit(oid.clone()));
        let pick = oid.clone();
        let right = oid.clone();
        div()
            .id(SharedString::from(format!("compare.commit.{oid}")))
            .flex()
            .items_center()
            .gap(px(10.))
            .mx(px(6.))
            .my(px(1.))
            .px(px(8.))
            .py(px(6.))
            .rounded(px(6.))
            .cursor_pointer()
            .when(selected, |this| this.bg(c.accent))
            .hover(|s| s.bg(c.muted))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select(Some(Selection::Commit(pick.clone())), cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.select(Some(Selection::Commit(right.clone())), cx);
                }),
            )
            .child(
                avatar(
                    &commit.author_name,
                    &commit.author_email,
                    px(22.),
                    theme.mode == crate::ui::theme::ThemeMode::Dark,
                )
                .flex_shrink_0(),
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
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_sm()
                            .child(commit.summary.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(
                                div()
                                    .font_family(theme.fonts.mono.clone())
                                    .child(short(&commit.oid)),
                            )
                            .child(
                                div()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(format!(
                                        "{} · {}",
                                        commit.author_name,
                                        ago(time(commit.author_time))
                                    )),
                            ),
                    ),
            )
    }
}

fn group_header(
    icon: LucideIcon,
    title: String,
    detail: String,
    theme: &crate::ui::theme::UiTheme,
) -> impl IntoElement + use<> {
    let c = &theme.colors;
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(14.))
        .pt(px(12.))
        .pb(px(4.))
        .child(lucide(icon).size(px(13.)).text_color(c.muted_foreground))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(c.foreground)
                .child(title),
        )
        .child(div().text_xs().text_color(c.muted_foreground).child(detail))
}

impl Render for CompareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let base_label: SharedString = self
            .base
            .clone()
            .map_or_else(|| "Choose a base".into(), Into::into);
        let head_label: SharedString = self
            .head
            .clone()
            .map_or_else(|| "Choose a branch".into(), Into::into);
        let picker = |label: &'static str, value: SharedString, menu: &Entity<MenuState>| {
            let style = theme.clone();
            Menu::new(menu, label)
                .trigger_style_with(move |button| {
                    style_button(
                        button,
                        false,
                        ButtonVariant::Outline,
                        ButtonSize::Sm,
                        &style,
                    )
                })
                .trigger(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .max_w(px(170.))
                        .child(
                            lucide(LucideIcon::GitBranch)
                                .size(px(13.))
                                .flex_none()
                                .text_color(c.muted_foreground),
                        )
                        .child(div().min_w_0().truncate().child(value))
                        .child(
                            lucide(LucideIcon::ChevronDown)
                                .size(px(12.))
                                .flex_none()
                                .text_color(c.muted_foreground),
                        ),
                )
        };
        let toolbar = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(6.))
            .px(px(12.))
            .py(px(8.))
            .border_b_1()
            .border_color(c.border)
            .child(picker("Base", base_label, &self.base_menu))
            .child(
                Button::new("compare.swap")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::IconSm)
                    .aria_label("Swap")
                    .disabled(self.base.is_none() && self.head.is_none())
                    .child(
                        lucide(LucideIcon::ArrowLeftRight)
                            .size(px(14.))
                            .text_color(c.foreground),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.swap(cx))),
            )
            .child(picker("Compare", head_label, &self.head_menu));
        let body = match &self.status {
            Status::Idle => message(
                LucideIcon::GitCompareArrows,
                "Choose two branches",
                "Pick a base and a branch to see the commits each has and what the branch changed.",
                &theme,
            ),
            Status::Loading => message(LucideIcon::GitCompareArrows, "Comparing…", "", &theme),
            Status::Failed(error) => message(
                LucideIcon::TriangleAlert,
                "Couldn't compare these",
                error,
                &theme,
            ),
            Status::Ready(comparison) => {
                let comparison = comparison.clone();
                self.render_comparison(&comparison, &theme, cx)
            }
        };
        let can_merge = self.comparison().is_some_and(|c| c.ahead_count > 0)
            && self.base.is_some()
            && self.base == self.current_branch(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
            .when(can_merge, |this| {
                let head = self.head.clone().unwrap_or_default();
                let base = self.base.clone().unwrap_or_default();
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_end()
                        .px(px(12.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(c.border)
                        .child(
                            Button::new("compare.merge")
                                .variant(ButtonVariant::Default)
                                .size(ButtonSize::Sm)
                                .child(
                                    lucide(LucideIcon::GitMerge)
                                        .size(px(14.))
                                        .text_color(c.primary_foreground),
                                )
                                .label(format!("Merge {head} into {base}"))
                                .on_click(cx.listener(|this, _, _, cx| this.merge(cx))),
                        ),
                )
            })
    }
}

impl CompareView {
    fn render_comparison(
        &self,
        comparison: &Comparison,
        theme: &crate::ui::theme::UiTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let c = &theme.colors;
        let head = self.head.clone().unwrap_or_default();
        let base = self.base.clone().unwrap_or_default();
        if comparison.ahead_count == 0 && comparison.behind_count == 0 {
            return message(
                LucideIcon::Equal,
                "Nothing to compare",
                &format!("{head} and {base} are at the same commit."),
                theme,
            );
        }
        let all_selected = self.selected == Some(Selection::AllChanges);
        let summary = div()
            .id("compare.all")
            .m(px(8.))
            .p(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(if all_selected { c.primary } else { c.border })
            .when(all_selected, |this| this.bg(c.accent))
            .when(comparison.ahead_count > 0, |this| {
                this.cursor_pointer()
                    .hover(|s| s.bg(c.muted))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.select(Some(Selection::AllChanges), cx);
                    }))
            })
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .child(stat(
                        LucideIcon::ArrowUp,
                        comparison.ahead_count,
                        "ahead",
                        c.success,
                        theme,
                    ))
                    .child(stat(
                        LucideIcon::ArrowDown,
                        comparison.behind_count,
                        "behind",
                        c.warning,
                        theme,
                    )),
            )
            .child(match &comparison.merge_base {
                Some(_) if comparison.ahead_count == 0 => div()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child(format!("{head} has nothing {base} lacks.")),
                Some(_) => div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_sm()
                    .child(
                        lucide(LucideIcon::FileDiff)
                            .size(px(14.))
                            .text_color(c.muted_foreground),
                    )
                    .child(format!(
                        "{} changed",
                        if comparison.files_changed == 1 {
                            "1 file".to_owned()
                        } else {
                            format!("{} files", comparison.files_changed)
                        }
                    ))
                    .child(
                        div()
                            .text_color(c.success)
                            .child(format!("+{}", comparison.insertions)),
                    )
                    .child(
                        div()
                            .text_color(c.destructive)
                            .child(format!("−{}", comparison.deletions)),
                    ),
                None => div()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child("These histories share no commit."),
            })
            .when(comparison.ahead_count > 0, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("All changes on {head} since it left {base}")),
                )
            });
        let mut list = div()
            .id("compare.list")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .pb(px(8.))
            .child(summary);
        if comparison.ahead_count > 0 {
            list = list.child(group_header(
                LucideIcon::ArrowUp,
                format!("Only on {head}"),
                more(comparison.ahead_count, comparison.ahead.len()),
                theme,
            ));
            for commit in &comparison.ahead {
                list = list.child(self.render_commit(commit, theme, cx));
            }
        }
        if comparison.behind_count > 0 {
            list = list.child(group_header(
                LucideIcon::ArrowDown,
                format!("Only on {base}"),
                more(comparison.behind_count, comparison.behind.len()),
                theme,
            ));
            for commit in &comparison.behind {
                list = list.child(self.render_commit(commit, theme, cx));
            }
        }
        context_menu(&self.menu, "Commit actions", list)
            .size_full()
            .into_any_element()
    }
}

/// "3 commits", or "1,000 of 1,204 commits" past the limit.
fn more(count: usize, shown: usize) -> String {
    if shown < count {
        format!("{shown} of {}", commits(count))
    } else {
        commits(count)
    }
}

fn stat(
    icon: LucideIcon,
    count: usize,
    label: &'static str,
    color: gpui_kit::Rgba,
    theme: &crate::ui::theme::UiTheme,
) -> impl IntoElement + use<> {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(lucide(icon).size(px(14.)).text_color(color))
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child(count.to_string()),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme.colors.muted_foreground)
                .child(label),
        )
}

fn message(
    icon: LucideIcon,
    title: &str,
    detail: &str,
    theme: &crate::ui::theme::UiTheme,
) -> gpui_kit::AnyElement {
    let c = &theme.colors;
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .px(px(24.))
        .py(px(48.))
        .child(lucide(icon).size(px(22.)).text_color(c.muted_foreground))
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(title.to_owned()),
        )
        .when(!detail.is_empty(), |this| {
            this.child(
                div()
                    .max_w(px(320.))
                    .text_center()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child(detail.to_owned()),
            )
        })
        .into_any_element()
}
