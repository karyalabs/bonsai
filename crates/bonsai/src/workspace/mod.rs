//! The main window: repository tabs, the sidebar, the list and detail panes,
//! and the command palette.

use std::path::{Path, PathBuf};

mod clone;

use bonsai_git::{Git, Head, Repository, Snapshot};
pub use clone::{CloneDialog, CloneEvent};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    activity_panel::ActivityPanel,
    changes::{ChangesEvent, ChangesView},
    commands::{self, COMMANDS},
    diff_pane::DiffPane,
    purge_sheet::{PurgeEvent, PurgeSheet, backup_dir},
    refs::{RefDialog, RefsEvent, RefsPanel},
    repository::{RepositoryEvent, RepositoryModel},
    session::Session,
    sync_bar::SyncBar,
    ui::{
        button::{Button, ButtonVariant},
        command::{CommandEvent, CommandItem, CommandState, command_dialog_popup},
        dialog::{DialogHandle, dialog, dialog_popup, dialog_with},
        empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle},
        resizable::{PaneLimits, Resizable},
        sidebar::{Sidebar, SidebarItem, sidebar_badge},
        tabs::{Tab, Tabs},
        theme::UiTheme,
    },
};

/// Which view fills the list pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Changes,
    History,
    Activity,
}

struct RepositoryTab {
    id: u64,
    title: SharedString,
    model: Entity<RepositoryModel>,
    changes: Entity<ChangesView>,
    diff: Entity<DiffPane>,
    refs: Entity<RefsPanel>,
    sync: Entity<SyncBar>,
    activity: Entity<ActivityPanel>,
    _events: Subscription,
    _changes_events: Subscription,
    _refs_events: Subscription,
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
    clone_handle: DialogHandle,
    clone: Option<Entity<CloneDialog>>,
    clone_events: Option<Subscription>,
    purge_handle: DialogHandle,
    purge: Option<Entity<PurgeSheet>>,
    purge_events: Option<Subscription>,
    /// A path to open Purge from History for on the next render.
    purge_requested: Option<PathBuf>,
    /// Shows the welcome screen in place of the active tab (New Tab).
    welcome: bool,
    message: Option<SharedString>,
    /// A message that came from a failed operation, whose command the
    /// activity log has; the banner offers Show Details while it shows.
    message_details: Option<SharedString>,
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
        let git = Git::locate(None)
            .map(crate::askpass::configure)
            .map_err(|error| SharedString::from(error.to_string()));
        let mut this = Self {
            git,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            section: Section::Changes,
            session: session.clone(),
            session_path,
            palette: DialogHandle::new(false),
            clone_handle: DialogHandle::new(false),
            clone: None,
            clone_events: None,
            purge_handle: DialogHandle::new(false),
            purge: None,
            purge_events: None,
            purge_requested: None,
            welcome: false,
            message: None,
            message_details: None,
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
                    this.message_details = Some(error.clone());
                }
                cx.notify();
            });
            let changes = cx.new(|cx| ChangesView::new(model.clone(), cx));
            let diff = cx.new(|cx| DiffPane::new(model.clone(), cx));
            let refs = cx.new(|cx| RefsPanel::new(model.clone(), cx));
            let sync = cx.new(|cx| SyncBar::new(model.clone(), cx));
            let diff_target = diff.downgrade();
            let changes_events = cx.subscribe(
                &changes,
                move |this, _, event: &ChangesEvent, cx| match event {
                    ChangesEvent::Focused(target, title) => {
                        let _ = diff_target
                            .update(cx, |diff, cx| diff.show(target.clone(), title.clone(), cx));
                    }
                    ChangesEvent::Purge(path) => {
                        this.purge_requested = Some(path.clone());
                        cx.notify();
                    }
                },
            );
            let activity = cx.new(|cx| ActivityPanel::new(model.clone(), cx));
            let diff_target = diff.downgrade();
            let refs_events = cx.subscribe(&refs, move |this, _, event: &RefsEvent, cx| {
                let RefsEvent::ShowDiff(target, title) = event;
                this.section = Section::Changes;
                let _ = diff_target.update(cx, |diff, cx| {
                    diff.show(Some(target.clone()), title.clone(), cx);
                });
                cx.notify();
            });
            self.tabs.push(RepositoryTab {
                id: self.next_tab_id,
                title,
                model,
                changes,
                diff,
                refs,
                sync,
                activity,
                _changes_events: changes_events,
                _refs_events: refs_events,
                _events: events,
            });
            self.next_tab_id += 1;
            self.active = self.tabs.len() - 1;
        }
        self.welcome = false;
        if save {
            let root = self.tabs[self.active]
                .model
                .read(cx)
                .repository()
                .root()
                .to_path_buf();
            self.session.remember(&root);
            self.sync_session_tabs(cx);
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
        if index < self.tabs.len() && (index != self.active || self.welcome) {
            self.active = index;
            self.welcome = false;
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
                this.open_paths(&paths, cx);
            });
        })
        .detach();
    }

    /// Opens each path in a tab and remembers it, reporting the first failure.
    pub fn open_paths(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        for path in paths {
            if let Err(error) = self.open_path(path, true, cx) {
                self.message = Some(error);
            }
        }
        cx.notify();
    }

    /// Opens a recent repository, or explains that it moved.
    fn open_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path.exists() {
            if let Err(error) = self.open_path(path, true, cx) {
                self.message = Some(error);
            }
        } else {
            self.message = Some(
                format!(
                    "{} no longer exists. Remove it from Recent, or open it from its new location.",
                    path.display()
                )
                .into(),
            );
        }
        cx.notify();
    }

    fn forget_recent(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.session.forget(path);
        self.save_session();
        cx.notify();
    }

    fn prompt_new(&mut self, cx: &mut Context<Self>) {
        let Ok(git) = self.git.clone() else { return };
        let directory = self
            .session
            .clone_parent
            .clone()
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let path = cx.prompt_for_new_path(&directory, Some("new-repository"));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = path.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                match Repository::init(&git, &path, None) {
                    Ok(repository) => {
                        let root = repository.root().to_path_buf();
                        if let Err(error) = this.open_path(&root, true, cx) {
                            this.message = Some(error);
                        }
                    }
                    Err(error) => this.message = Some(error.to_string().into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the Clone dialog, optionally filled with `url`.
    /// Opens Purge from History for the active repository.
    pub fn show_purge(
        &mut self,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.active_model().cloned().filter(|_| !self.welcome) else {
            return;
        };
        let backups = backup_dir(self.session_path.as_deref());
        let sheet = cx.new(|cx| PurgeSheet::new(model, backups, path, window, cx));
        self.purge_events = Some(cx.subscribe_in(
            &sheet,
            window,
            |this, _, event: &PurgeEvent, window, cx| match event {
                PurgeEvent::Dismissed => {
                    this.purge_handle.close(window, cx);
                    this.purge = None;
                    cx.notify();
                }
            },
        ));
        self.purge = Some(sheet);
        self.purge_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn purge_sheet(&self) -> Option<&Entity<PurgeSheet>> {
        self.purge.as_ref()
    }

    fn on_purge(
        &mut self,
        _: &commands::PurgeFromHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_purge(None, window, cx);
    }

    fn render_purge(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let content = self.purge.clone();
        let popup = dialog_popup("workspace.purge.popup", "Purge from History", cx)
            .when_some(content.clone(), gpui_kit::ParentElement::child);
        dialog("workspace.purge", &self.purge_handle, popup, window, cx).on_cancel(
            move |_, _, cx| {
                // A running purge can't be interrupted safely.
                !content
                    .as_ref()
                    .is_some_and(|sheet| sheet.read(cx).is_running())
            },
        )
    }

    pub fn show_clone(&mut self, url: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(git) = self.git.clone() else { return };
        let parent = self
            .session
            .clone_parent
            .clone()
            .filter(|p| p.is_dir())
            .or_else(home_dir)
            .unwrap_or_else(|| PathBuf::from("/"));
        let dialog = cx.new(|cx| CloneDialog::new(git, parent, window, cx));
        if let Some(url) = url {
            dialog.update(cx, |dialog, cx| dialog.set_url(url, window, cx));
        }
        self.clone_events = Some(cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &CloneEvent, window, cx| match event {
                CloneEvent::Cloned(path) => {
                    this.clone_handle.close(window, cx);
                    this.clone = None;
                    if let Err(error) = this.open_path(path, true, cx) {
                        this.message = Some(error);
                    }
                }
                CloneEvent::ParentChanged(parent) => {
                    this.session.clone_parent = Some(parent.clone());
                    this.save_session();
                }
                CloneEvent::Dismissed => {
                    this.clone_handle.close(window, cx);
                    this.clone = None;
                    cx.notify();
                }
            },
        ));
        self.clone = Some(dialog);
        self.clone_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn clone_dialog(&self) -> Option<&Entity<CloneDialog>> {
        self.clone.as_ref()
    }

    #[must_use]
    pub fn is_showing_welcome(&self) -> bool {
        self.welcome || self.tabs.is_empty()
    }

    // Action handlers.

    fn on_new(&mut self, _: &commands::NewRepository, _: &mut Window, cx: &mut Context<Self>) {
        self.prompt_new(cx);
    }

    fn on_clone(
        &mut self,
        _: &commands::CloneRepository,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_clone(None, window, cx);
    }

    fn on_welcome(&mut self, _: &commands::ShowWelcome, _: &mut Window, cx: &mut Context<Self>) {
        self.welcome = true;
        cx.notify();
    }

    fn on_open(&mut self, _: &commands::OpenRepository, _: &mut Window, cx: &mut Context<Self>) {
        self.prompt_open(cx);
    }

    fn on_close_tab(
        &mut self,
        _: &commands::CloseTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.welcome && !self.tabs.is_empty() {
            self.welcome = false;
            cx.notify();
        } else if self.tabs.is_empty() {
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

    fn on_activity(&mut self, _: &commands::ShowActivity, _: &mut Window, cx: &mut Context<Self>) {
        self.section = Section::Activity;
        cx.notify();
    }

    /// Opens the activity log at the command behind the error banner.
    pub fn show_message_details(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.activity
                .update(cx, ActivityPanel::select_latest_failure);
        }
        self.section = Section::Activity;
        cx.notify();
    }

    fn on_commit(&mut self, _: &commands::Commit, _: &mut Window, cx: &mut Context<Self>) {
        if self.welcome {
            return;
        }
        if let Some(tab) = self.tabs.get(self.active) {
            self.section = Section::Changes;
            tab.changes.update(cx, ChangesView::commit);
        }
    }

    fn active_sync(&self) -> Option<Entity<SyncBar>> {
        self.tabs
            .get(self.active)
            .filter(|_| !self.welcome)
            .map(|tab| tab.sync.clone())
    }

    fn on_fetch(&mut self, _: &commands::Fetch, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(sync) = self.active_sync() {
            sync.update(cx, SyncBar::fetch);
        }
    }

    fn on_stash(&mut self, _: &commands::StashChanges, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active).filter(|_| !self.welcome) {
            tab.refs
                .update(cx, |refs, cx| refs.show(RefDialog::StashChanges, cx));
        }
    }

    fn on_pull(&mut self, _: &commands::Pull, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(sync) = self.active_sync() {
            sync.update(cx, SyncBar::pull);
        }
    }

    fn on_push(&mut self, _: &commands::Push, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(sync) = self.active_sync() {
            let reason = sync.update(cx, SyncBar::push);
            if let Some(reason) = reason {
                self.message = Some(reason.into());
                cx.notify();
            }
        }
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
            .filter(|_| !self.welcome)
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
            .child(div().flex_1().min_w_0().child(tabs))
            .when_some(
                self.tabs.get(self.active).filter(|_| !self.welcome),
                |this, tab| this.child(tab.sync.clone()),
            )
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
                    .icon(
                        lucide(LucideIcon::FilePen)
                            .size(px(16.))
                            .text_color(c.sidebar_foreground),
                    )
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
                    .icon(
                        lucide(LucideIcon::GitGraph)
                            .size(px(16.))
                            .text_color(c.sidebar_foreground),
                    )
                    .selected(self.section == Section::History)
                    .on_activate(cx.listener(|this, _, _, cx| {
                        this.section = Section::History;
                        cx.notify();
                    })),
            )
            .child(
                SidebarItem::new("workspace.section.activity", "Activity")
                    .icon(
                        lucide(LucideIcon::ScrollText)
                            .size(px(16.))
                            .text_color(c.sidebar_foreground),
                    )
                    .selected(self.section == Section::Activity)
                    .on_activate(cx.listener(|this, _, _, cx| {
                        this.section = Section::Activity;
                        cx.notify();
                    })),
            );
        if let Some(tab) = self.tabs.get(self.active) {
            sidebar = sidebar.child(tab.refs.clone());
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
            (Section::Changes, Some(_), _) => match self.tabs.get(self.active) {
                Some(tab) => tab.changes.clone().into_any_element(),
                None => div().into_any_element(),
            },
            (Section::Activity, Some(_), _) => match self.tabs.get(self.active) {
                Some(tab) => tab.activity.clone().into_any_element(),
                None => div().into_any_element(),
            },
            (Section::History, Some(_), _) => message_panel(
                LucideIcon::GitGraph,
                "History",
                "The commit graph appears here.".into(),
                cx,
            )
            .into_any_element(),
        };
        // The Changes view scrolls its own virtualized list.
        div()
            .id("workspace.list")
            .size_full()
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
            .when_some(self.tabs.get(self.active), |this, tab| {
                this.child(tab.diff.clone())
            })
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let available = self.git.is_ok();
        let action =
            |id: &'static str, icon: LucideIcon, label: &'static str, hint: &'static str| {
                Button::new(id)
                    .variant(ButtonVariant::Outline)
                    .disabled(!available)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(lucide(icon).size(px(16.)).text_color(c.foreground))
                            .child(label)
                            .child(div().text_xs().text_color(c.muted_foreground).child(hint)),
                    )
            };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .child(
                action("welcome.open", LucideIcon::FolderOpen, "Open…", "⌘O")
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx))),
            )
            .child(
                action("welcome.new", LucideIcon::FolderPlus, "New…", "⌘N")
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_new(cx))),
            )
            .child(
                action("welcome.clone", LucideIcon::Download, "Clone…", "⇧⌘O")
                    .on_click(cx.listener(|this, _, window, cx| this.show_clone(None, window, cx))),
            );
        let recents = self.session.recents.clone();
        let mut list = div().flex().flex_col().gap(px(2.));
        for (index, path) in recents.iter().enumerate() {
            let missing = !path.exists();
            let open_path = path.clone();
            let forget_path = path.clone();
            let name = tab_title(path);
            list = list.child(
                div()
                    .id(("welcome.recent", index))
                    .group("recent")
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(theme.radius.md)
                    .cursor_pointer()
                    .hover(|this| this.bg(c.accent))
                    .on_click(cx.listener(move |this, _, _, cx| this.open_recent(&open_path, cx)))
                    .child(
                        lucide(if missing {
                            LucideIcon::FolderX
                        } else {
                            LucideIcon::FolderGit2
                        })
                        .size(px(16.))
                        .text_color(c.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_sm()
                                    .when(missing, |this| this.text_color(c.muted_foreground))
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(c.muted_foreground)
                                    .child(if missing {
                                        format!("Missing · {}", path.display())
                                    } else {
                                        path.display().to_string()
                                    }),
                            ),
                    )
                    .child(
                        Button::new(("welcome.recent.remove", index))
                            .variant(ButtonVariant::Ghost)
                            .aria_label(format!("Remove {} from Recent", path.display()))
                            .child(
                                lucide(LucideIcon::X)
                                    .size(px(12.))
                                    .text_color(c.muted_foreground),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.forget_recent(&forget_path, cx);
                            })),
                    ),
            );
        }
        let header = div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Bonsai"),
            )
            .child(div().text_sm().text_color(c.muted_foreground).child(match &self.git {
                Ok(_) => SharedString::from(
                    "Open a repository on this Mac, start a new one, or clone one from any Git server.",
                ),
                Err(error) => error.clone(),
            }));
        div()
            .id("welcome")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(560.))
                    .px(px(24.))
                    .py(px(64.))
                    .flex()
                    .flex_col()
                    .gap(px(24.))
                    .child(header)
                    .child(actions)
                    .when(!recents.is_empty(), |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .text_color(c.muted_foreground)
                                                .child("RECENT"),
                                        )
                                        .child(
                                            Button::new("welcome.recent.clear")
                                                .variant(ButtonVariant::Ghost)
                                                .label("Clear")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.session.recents.clear();
                                                    this.save_session();
                                                    cx.notify();
                                                })),
                                        ),
                                )
                                .child(list),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child("You can also drop a repository folder onto this window."),
                    ),
            )
    }

    fn render_clone(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let content = self.clone.clone();
        let popup = dialog_popup("workspace.clone.popup", "Clone Repository", cx)
            .when_some(content.clone(), gpui_kit::ParentElement::child);
        let cancel_target = content;
        dialog("workspace.clone", &self.clone_handle, popup, window, cx).on_cancel(
            move |_, _, cx| {
                // Escape stops a running clone first, and closes on the next press.
                match &cancel_target {
                    Some(dialog) if dialog.read(cx).is_running() => {
                        dialog.update(cx, CloneDialog::cancel);
                        false
                    }
                    _ => true,
                }
            },
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
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
        if let Some(path) = self.purge_requested.take() {
            self.show_purge(Some(path), window, cx);
        }
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let model = self.active_model().cloned().filter(|_| !self.welcome);
        let body = match model {
            None => self.render_welcome(cx).into_any_element(),
            Some(model) => {
                let model = model.read(cx);
                let snapshot = model.snapshot().cloned();
                let error = model.snapshot_error().cloned();
                let activity = self.section == Section::Activity && snapshot.is_some();
                let list = self.render_list(snapshot.as_ref(), error, cx);
                let detail = self.render_detail(cx);
                // The activity log lays out its own list and detail.
                let panes = if activity {
                    div().size_full().child(list).into_any_element()
                } else {
                    Resizable::new(
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
                    }))
                    .into_any_element()
                };
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
            .on_action(cx.listener(Self::on_new))
            .on_action(cx.listener(Self::on_clone))
            .on_action(cx.listener(Self::on_welcome))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.open_paths(paths.paths(), cx);
            }))
            .on_action(cx.listener(Self::on_close_tab))
            .on_action(cx.listener(Self::on_next_tab))
            .on_action(cx.listener(Self::on_previous_tab))
            .on_action(cx.listener(Self::on_palette))
            .on_action(cx.listener(Self::on_toggle_sidebar))
            .on_action(cx.listener(Self::on_changes))
            .on_action(cx.listener(Self::on_history))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_commit))
            .on_action(cx.listener(Self::on_fetch))
            .on_action(cx.listener(Self::on_pull))
            .on_action(cx.listener(Self::on_push))
            .on_action(cx.listener(Self::on_purge))
            .on_action(cx.listener(Self::on_stash))
            .on_action(cx.listener(Self::on_activity))
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
                        .child(div().flex_1().min_w_0().child(message.clone()))
                        .child(
                            div()
                                .flex()
                                .flex_shrink_0()
                                .gap(px(4.))
                                .when(self.message_details.as_ref() == Some(&message), |this| {
                                    this.child(
                                        Button::new("workspace.message.details")
                                            .variant(ButtonVariant::Ghost)
                                            .label("Show Details")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.show_message_details(cx);
                                            })),
                                    )
                                })
                                .child(
                                    Button::new("workspace.message.dismiss")
                                        .variant(ButtonVariant::Ghost)
                                        .label("Dismiss")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.message = None;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_palette(window, cx))
            .child(self.render_clone(window, cx))
            .child(self.render_purge(window, cx))
    }
}

#[cfg(test)]
mod tests;
