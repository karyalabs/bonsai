//! The main window: repository tabs, the sidebar, the list and detail panes,
//! and the command palette.

use std::path::{Path, PathBuf};

use bonsai_git::{Git, Head, Repository, Snapshot};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    commands::{self, COMMANDS},
    repository::{RepositoryEvent, RepositoryModel},
    session::Session,
    ui::{
        button::{Button, ButtonVariant},
        command::{CommandEvent, CommandItem, CommandState, command_dialog_popup},
        dialog::{DialogHandle, dialog_with},
        empty::{
            Empty, EmptyContent, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant,
            EmptyTitle,
        },
        resizable::{PaneLimits, Resizable},
        sidebar::{Sidebar, SidebarItem, sidebar_badge, sidebar_group_label},
        tabs::{Tab, Tabs},
        theme::UiTheme,
    },
};

/// Which view fills the list pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Changes,
    History,
}

struct RepositoryTab {
    id: u64,
    title: SharedString,
    model: Entity<RepositoryModel>,
    _events: Subscription,
}

pub struct Workspace {
    git: Result<Git, SharedString>,
    tabs: Vec<RepositoryTab>,
    active: usize,
    next_tab_id: u64,
    section: Section,
    session: Session,
    session_path: Option<PathBuf>,
    palette: DialogHandle,
    message: Option<SharedString>,
    watch: bool,
    focus: FocusHandle,
}

impl Workspace {
    /// Builds the window's root view, reopening the tabs `session` lists.
    /// `session_path` is where changes are saved; `None` keeps them in memory.
    pub fn new(
        session: Session,
        session_path: Option<PathBuf>,
        watch: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let git = Git::locate(None).map_err(|error| SharedString::from(error.to_string()));
        let mut this = Self {
            git,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            section: Section::Changes,
            session: session.clone(),
            session_path,
            palette: DialogHandle::new(false),
            message: None,
            watch,
            focus,
        };
        for path in &session.tabs {
            // A repository that moved or was deleted is dropped quietly.
            let _ = this.open_path(path, false, cx);
        }
        this.active = session.active_tab.min(this.tabs.len().saturating_sub(1));
        this.save_session();
        this
    }

    /// Opens the repository containing `path` in a new tab, or switches to its
    /// tab when it is already open.
    pub fn open_path(
        &mut self,
        path: &Path,
        save: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), SharedString> {
        let git = self.git.clone()?;
        let repository = Repository::discover(&git, path)
            .map_err(|error| SharedString::from(error.to_string()))?;
        let root = canonical(repository.root());
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| canonical(tab.model.read(cx).repository().root()) == root)
        {
            self.active = index;
        } else {
            let title = tab_title(repository.root());
            let watch = self.watch;
            let model = cx.new(|cx| RepositoryModel::new(repository, watch, cx));
            let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
                if let RepositoryEvent::OperationFinished {
                    error: Some(error), ..
                } = event
                {
                    this.message = Some(error.clone());
                }
                cx.notify();
            });
            self.tabs.push(RepositoryTab {
                id: self.next_tab_id,
                title,
                model,
                _events: events,
            });
            self.next_tab_id += 1;
            self.active = self.tabs.len() - 1;
        }
        if save {
            self.save_session();
        }
        cx.notify();
        Ok(())
    }

    #[must_use]
    pub fn tab_titles(&self) -> Vec<SharedString> {
        self.tabs.iter().map(|tab| tab.title.clone()).collect()
    }

    #[must_use]
    pub fn active_index(&self) -> usize {
        self.active
    }

    #[must_use]
    pub fn section(&self) -> Section {
        self.section
    }

    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    fn active_model(&self) -> Option<&Entity<RepositoryModel>> {
        self.tabs.get(self.active).map(|tab| &tab.model)
    }

    fn save_session(&mut self) {
        self.session.active_tab = self.active;
        // Paths are read when saving so the session follows the tabs exactly.
        if let Some(path) = &self.session_path
            && let Err(error) = self.session.save(path)
        {
            self.message = Some(format!("Bonsai could not save its window layout: {error}").into());
        }
    }

    fn sync_session_tabs(&mut self, cx: &Context<Self>) {
        self.session.tabs = self
            .tabs
            .iter()
            .map(|tab| tab.model.read(cx).repository().root().to_path_buf())
            .collect();
        self.save_session();
    }

    fn select_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            self.save_session();
            cx.notify();
        }
    }

    fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.len() {
            return;
        }
        self.tabs.remove(index);
        if self.active > index || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        self.sync_session_tabs(cx);
        cx.notify();
    }

    fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                for path in paths {
                    if let Err(error) = this.open_path(&path, false, cx) {
                        this.message = Some(error);
                    }
                }
                this.sync_session_tabs(cx);
                cx.notify();
            });
        })
        .detach();
    }

    // Action handlers.

    fn on_open(&mut self, _: &commands::OpenRepository, _: &mut Window, cx: &mut Context<Self>) {
        self.prompt_open(cx);
    }

    fn on_close_tab(
        &mut self,
        _: &commands::CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.is_empty() {
            window.remove_window();
        } else {
            self.close_tab(self.active, cx);
        }
    }

    fn on_next_tab(&mut self, _: &commands::NextTab, _: &mut Window, cx: &mut Context<Self>) {
        if !self.tabs.is_empty() {
            self.select_tab((self.active + 1) % self.tabs.len(), cx);
        }
    }

    fn on_previous_tab(
        &mut self,
        _: &commands::PreviousTab,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.tabs.is_empty() {
            self.select_tab((self.active + self.tabs.len() - 1) % self.tabs.len(), cx);
        }
    }

    fn on_palette(
        &mut self,
        _: &commands::ShowCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette.open(window, cx);
        cx.notify();
    }

    fn on_toggle_sidebar(
        &mut self,
        _: &commands::ToggleSidebar,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session.sidebar_open = !self.session.sidebar_open;
        self.save_session();
        cx.notify();
    }

    fn on_changes(&mut self, _: &commands::ShowChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.section = Section::Changes;
        cx.notify();
    }

    fn on_history(&mut self, _: &commands::ShowHistory, _: &mut Window, cx: &mut Context<Self>) {
        self.section = Section::History;
        cx.notify();
    }

    fn on_refresh(&mut self, _: &commands::Refresh, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(model) = self.active_model() {
            model.update(cx, RepositoryModel::refresh);
        }
    }

    // Rendering.

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx);
        let selected = self
            .tabs
            .get(self.active)
            .map(|tab| SharedString::from(tab.id.to_string()))
            .unwrap_or_default();
        let mut tabs = Tabs::new("workspace.tabs")
            .aria_label("Open repositories")
            .selected(selected)
            .on_change(cx.listener(|this, value: &SharedString, _, cx| {
                if let Some(index) = this
                    .tabs
                    .iter()
                    .position(|t| t.id.to_string() == value.as_ref())
                {
                    this.select_tab(index, cx);
                }
            }));
        for (index, tab) in self.tabs.iter().enumerate() {
            let id = tab.id;
            tabs = tabs.item(
                Tab::new(("workspace.tab", id), id.to_string(), tab.title.clone()).trailing(
                    Button::new(("workspace.tab.close", id))
                        .variant(ButtonVariant::Ghost)
                        .aria_label(format!("Close {}", tab.title))
                        .child(
                            lucide(LucideIcon::X)
                                .size(px(12.))
                                .text_color(theme.colors.muted_foreground),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(index) = this.tabs.iter().position(|t| t.id == id) {
                                this.close_tab(index, cx);
                            }
                        })),
                ),
            );
            let _ = index;
        }
        div()
            .flex()
            .items_center()
            .px(px(8.))
            .py(px(4.))
            .border_b_1()
            .border_color(theme.colors.border)
            .child(tabs)
    }

    fn render_sidebar(
        &self,
        snapshot: Option<&Snapshot>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx);
        let c = &theme.colors;
        let changes = snapshot.map_or(0, |s| s.changes.len());
        let head = snapshot.map(|s| match &s.head {
            Head::Branch { name, oid: None } => format!("{name} (no commits yet)"),
            Head::Branch { name, .. } => name.clone(),
            Head::Detached { oid } => format!("Detached at {}", oid.short()),
        });
        let mut sidebar = Sidebar::new("workspace.sidebar", "Repository")
            .content_padding(px(8.))
            .header(
                div()
                    .px(px(8.))
                    .pt(px(8.))
                    .pb(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_sm()
                            .text_color(c.sidebar_accent_foreground)
                            .child(
                                self.tabs
                                    .get(self.active)
                                    .map(|t| t.title.clone())
                                    .unwrap_or_default(),
                            ),
                    )
                    .when_some(head, |this, head| {
                        this.child(div().text_xs().text_color(c.sidebar_foreground).child(head))
                    }),
            )
            .child(
                SidebarItem::new("workspace.section.changes", "Changes")
                    .icon(lucide(LucideIcon::FilePen).size(px(16.)))
                    .when(changes > 0, |item| {
                        item.trailing(sidebar_badge(changes.to_string(), cx))
                    })
                    .selected(self.section == Section::Changes)
                    .on_activate(cx.listener(|this, _, _, cx| {
                        this.section = Section::Changes;
                        cx.notify();
                    })),
            )
            .child(
                SidebarItem::new("workspace.section.history", "History")
                    .icon(lucide(LucideIcon::GitGraph).size(px(16.)))
                    .selected(self.section == Section::History)
                    .on_activate(cx.listener(|this, _, _, cx| {
                        this.section = Section::History;
                        cx.notify();
                    })),
            );
        if let Some(snapshot) = snapshot {
            sidebar = sidebar.child(div().h(px(12.)));
            sidebar = sidebar.child(sidebar_group_label("BRANCHES", cx));
            for branch in &snapshot.branches {
                let mut item = SidebarItem::new(
                    SharedString::from(format!("branch:{}", branch.name)),
                    branch.name.clone(),
                )
                .icon(lucide(LucideIcon::GitBranch).size(px(16.)))
                .selected(branch.is_head);
                if let Some(upstream) = &branch.upstream
                    && (upstream.ahead > 0 || upstream.behind > 0)
                {
                    item = item.trailing(
                        div()
                            .text_xs()
                            .text_color(c.sidebar_foreground)
                            .child(format!("↑{} ↓{}", upstream.ahead, upstream.behind)),
                    );
                }
                sidebar = sidebar.child(item);
            }
            for (label, names, icon) in [
                (
                    "REMOTES",
                    snapshot
                        .remotes
                        .iter()
                        .map(|r| r.name.clone())
                        .collect::<Vec<_>>(),
                    LucideIcon::Cloud,
                ),
                (
                    "TAGS",
                    snapshot.tags.iter().map(|t| t.name.clone()).collect(),
                    LucideIcon::Tag,
                ),
                (
                    "STASHES",
                    snapshot.stashes.iter().map(|s| s.message.clone()).collect(),
                    LucideIcon::Archive,
                ),
            ] {
                if names.is_empty() {
                    continue;
                }
                sidebar = sidebar
                    .child(div().h(px(12.)))
                    .child(sidebar_group_label(label, cx));
                for (index, name) in names.into_iter().enumerate() {
                    sidebar = sidebar.child(
                        SidebarItem::new(SharedString::from(format!("{label}:{index}")), name)
                            .icon(lucide(icon).size(px(16.))),
                    );
                }
            }
        }
        div().size_full().bg(c.sidebar).child(sidebar)
    }

    fn render_list(
        &self,
        snapshot: Option<&Snapshot>,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx);
        let c = &theme.colors;
        let body = match (self.section, snapshot, error) {
            (_, None, Some(error)) => message_panel(
                LucideIcon::TriangleAlert,
                "Bonsai could not read this repository",
                error.clone(),
                cx,
            )
            .into_any_element(),
            (_, None, None) => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Reading the repository…")
                .into_any_element(),
            (Section::Changes, Some(snapshot), _) if snapshot.changes.is_empty() => message_panel(
                LucideIcon::CircleCheck,
                "No changes",
                "The work tree matches the last commit.".into(),
                cx,
            )
            .into_any_element(),
            (Section::Changes, Some(snapshot), _) => div()
                .flex()
                .flex_col()
                .p(px(8.))
                .children(snapshot.changes.iter().take(500).map(|change| {
                    div()
                        .px(px(8.))
                        .py(px(4.))
                        .text_sm()
                        .text_color(c.foreground)
                        .child(change.path.to_string_lossy().into_owned())
                }))
                .into_any_element(),
            (Section::History, Some(_), _) => message_panel(
                LucideIcon::GitGraph,
                "History",
                "The commit graph appears here.".into(),
                cx,
            )
            .into_any_element(),
        };
        div()
            .id("workspace.list")
            .size_full()
            .overflow_y_scroll()
            .bg(c.background)
            .child(body)
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx);
        div()
            .size_full()
            .border_l_1()
            .border_color(theme.colors.border)
            .bg(theme.colors.background)
            .child(message_panel(
                LucideIcon::FileDiff,
                "Nothing selected",
                "Select a change or a commit to see its diff.".into(),
                cx,
            ))
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx);
        let description: SharedString = match &self.git {
            Ok(_) => "Open a folder that contains a Git repository.".into(),
            Err(error) => error.clone(),
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Empty::new()
                    .w_full()
                    .max_w(px(420.))
                    .header(
                        EmptyHeader::new()
                            .media(
                                EmptyMedia::new()
                                    .with_variant(EmptyMediaVariant::Icon)
                                    .child(
                                        lucide(LucideIcon::GitFork)
                                            .size(px(16.))
                                            .text_color(theme.colors.foreground),
                                    ),
                            )
                            .title(EmptyTitle::new().child("No repository open"))
                            .description(EmptyDescription::new().child(description)),
                    )
                    .content(
                        EmptyContent::new().child(
                            Button::new("welcome.open")
                                .label("Open Repository…")
                                .disabled(self.git.is_err())
                                .on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx))),
                        ),
                    ),
            )
    }

    fn render_palette(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let handle = self.palette.clone();
        let workspace = cx.entity().downgrade();
        dialog_with(
            "workspace.palette",
            &self.palette,
            move |window, cx| {
                let state = window.use_keyed_state("workspace.palette.state", cx, |window, cx| {
                    let state = cx.new(|cx| CommandState::new(palette_items(), window, cx));
                    let handle = handle.clone();
                    let workspace = workspace.clone();
                    let subscription = cx.subscribe_in(
                        &state,
                        window,
                        move |_, _, event, window, cx| match event {
                            CommandEvent::Confirm(id) => {
                                handle.close(window, cx);
                                if let Some(command) = commands::find(id) {
                                    if let Some(workspace) = workspace.upgrade() {
                                        let focus = workspace.read(cx).focus.clone();
                                        window.focus(&focus, cx);
                                    }
                                    window.dispatch_action((command.action)(), cx);
                                }
                            }
                            CommandEvent::Cancel => handle.close(window, cx),
                            CommandEvent::Query(_) => {}
                        },
                    );
                    (state, subscription)
                });
                let state = state.read(cx).0.clone();
                state.update(cx, |state, cx| state.focus(window, cx));
                command_dialog_popup("workspace.palette.popup", &state, cx)
            },
            window,
            cx,
        )
    }
}

fn palette_items() -> Vec<CommandItem> {
    COMMANDS
        .iter()
        .filter(|command| command.palette)
        .map(|command| {
            let mut item = CommandItem::new(command.id, command.label)
                .group(command.group)
                .keywords(command.keywords.iter().copied());
            if let Some(keys) = command.keys.first() {
                item = item.shortcut(*keys);
            }
            item
        })
        .collect()
}

fn message_panel(
    icon: LucideIcon,
    title: &'static str,
    description: SharedString,
    cx: &Context<Workspace>,
) -> impl IntoElement {
    let theme = UiTheme::read(cx);
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(
            Empty::new().w_full().max_w(px(360.)).header(
                EmptyHeader::new()
                    .media(
                        EmptyMedia::new()
                            .with_variant(EmptyMediaVariant::Icon)
                            .child(
                                lucide(icon)
                                    .size(px(16.))
                                    .text_color(theme.colors.foreground),
                            ),
                    )
                    .title(EmptyTitle::new().child(title))
                    .description(EmptyDescription::new().child(description)),
            ),
        )
}

fn tab_title(root: &Path) -> SharedString {
    root.file_name()
        .map_or_else(|| root.to_string_lossy(), |name| name.to_string_lossy())
        .into_owned()
        .into()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let body = match self.active_model().cloned() {
            None => self.render_welcome(cx).into_any_element(),
            Some(model) => {
                let model = model.read(cx);
                let snapshot = model.snapshot().cloned();
                let error = model.snapshot_error().cloned();
                let list = self.render_list(snapshot.as_ref(), error, cx);
                let detail = self.render_detail(cx);
                let panes = Resizable::new(
                    "workspace.list-split",
                    "Resize list and detail",
                    px(self.session.list_width),
                )
                .first(list)
                .second(detail)
                .first_limits(PaneLimits::new(px(280.), px(1400.)))
                .second_limits(PaneLimits::new(px(320.), px(2000.)))
                .on_resize(cx.listener(|this, width: &gpui_kit::Pixels, _, cx| {
                    this.session.list_width = f32::from(*width);
                    this.save_session();
                    cx.notify();
                }));
                if self.session.sidebar_open {
                    Resizable::new(
                        "workspace.sidebar-split",
                        "Resize sidebar",
                        px(self.session.sidebar_width),
                    )
                    .first(self.render_sidebar(snapshot.as_ref(), cx))
                    .second(panes)
                    .first_limits(PaneLimits::new(px(180.), px(480.)))
                    .second_limits(PaneLimits::new(px(600.), px(4000.)))
                    .on_resize(cx.listener(|this, width: &gpui_kit::Pixels, _, cx| {
                        this.session.sidebar_width = f32::from(*width);
                        this.save_session();
                        cx.notify();
                    }))
                    .into_any_element()
                } else {
                    panes.into_any_element()
                }
            }
        };

        div()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_open))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_previous_tab))
            .on_action(cx.listener(Self::on_palette))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_changes))
            .on_action(cx.listener(Self::on_history))
            .on_action(cx.listener(Self::on_refresh))
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .font_family(theme.fonts.body.clone())
            .when(!self.tabs.is_empty(), |this| {
                this.child(self.render_tabs(cx))
            })
            .when_some(self.message.clone(), |this, message| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .px(px(12.))
                        .py(px(6.))
                        .bg(c.muted)
                        .text_sm()
                        .child(message)
                        .child(
                            Button::new("workspace.message.dismiss")
                                .variant(ButtonVariant::Ghost)
                                .label("Dismiss")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.message = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_palette(window, cx))
    }
}

#[cfg(test)]
mod tests;
