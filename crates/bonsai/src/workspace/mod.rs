//! The main window: repository tabs, the sidebar, the list and detail panes,
//! and the command palette.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

mod branch_picker;
mod clone;
pub(crate) mod title_bar;

use bonsai_git::{DiffTarget, Git, Oid, Repository, Snapshot};
pub use branch_picker::{BranchChoice, branch_items, parse_choice};
pub use clone::{CloneDialog, CloneEvent};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable, Image, ImageFormat,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement as _, Styled, StyledImage as _, Subscription, Task,
    Window, div, img, prelude::FluentBuilder as _, px,
};
pub use title_bar::BranchSummary;

use crate::{
    accounts::Accounts,
    accounts_sheet::{AccountsEvent, AccountsSheet},
    activity_panel::ActivityPanel,
    changes::{ChangesEvent, ChangesView},
    commands::{self, COMMANDS},
    create_pull_sheet::{CreatePullEvent, CreatePullSheet},
    diff_pane::{DiffPane, DiffPaneEvent},
    files::{FilesEvent, FilesView, Revision},
    history::{EditCommits, HistoryEvent, HistoryView},
    hosted::{HostedEnv, HostedEvent, HostedKind, HostedView},
    new_issue_sheet::{NewIssueEvent, NewIssueSheet},
    publish_sheet::{PublishEvent, PublishSheet},
    purge_sheet::{PurgeEvent, PurgeSheet, backup_dir},
    rebase_sheet::{RebaseEvent, RebaseSheet},
    refs::{RefDialog, RefsEvent, RefsPanel},
    remote_browser::{RemoteBrowser, RemoteBrowserEvent},
    repository::{RepositoryEvent, RepositoryModel},
    session::Session,
    settings_sheet::{SettingsEvent, SettingsSheet},
    sync_bar::SyncBar,
    ui::{
        button::{Button, ButtonVariant},
        command::{CommandEvent, CommandItem, CommandState, command_dialog_popup},
        dialog::{DialogHandle, dialog, dialog_popup, dialog_with},
        empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle},
        menu::MenuState,
        resizable::{PaneLimits, Resizable},
        sidebar::{Sidebar, SidebarItem, sidebar_badge},
        tabs::{Tab, Tabs},
        theme::{ThemeMode, UiTheme},
        toast::{Notice, ToastPlacement, ToastState},
    },
};

/// Which view fills the list pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Changes,
    History,
    Files,
    Activity,
    PullRequests,
    Issues,
}

/// A sheet a view asked the workspace to open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SheetRequest {
    Accounts,
    CreatePull,
    NewIssue,
}

impl Section {
    fn hosted(self) -> Option<HostedKind> {
        match self {
            Self::PullRequests => Some(HostedKind::PullRequests),
            Self::Issues => Some(HostedKind::Issues),
            _ => None,
        }
    }
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
    history: Entity<HistoryView>,
    files: Entity<FilesView>,
    /// Built the first time their section opens.
    pulls: Option<(Entity<HostedView>, Subscription)>,
    issues: Option<(Entity<HostedView>, Subscription)>,
    _events: Subscription,
    _history_events: Subscription,
    _changes_events: Subscription,
    _refs_events: Subscription,
    _diff_events: Subscription,
    _files_events: Subscription,
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
    rebase_handle: DialogHandle,
    rebase: Option<Entity<RebaseSheet>>,
    rebase_events: Option<Subscription>,
    /// Edit Commits asked for from History, opened on the next render.
    rebase_request: Option<(Entity<RepositoryModel>, EditCommits)>,
    accounts_handle: DialogHandle,
    accounts: Option<Entity<AccountsSheet>>,
    accounts_events: Option<Subscription>,
    settings_handle: DialogHandle,
    settings: Option<(Entity<SettingsSheet>, Subscription)>,
    /// Fetches open repositories every few minutes, as Settings says.
    auto_fetch: Option<Task<()>>,
    publish_handle: DialogHandle,
    publish: Option<(Entity<PublishSheet>, Subscription)>,
    create_pull_handle: DialogHandle,
    create_pull: Option<(Entity<CreatePullSheet>, Subscription)>,
    new_issue_handle: DialogHandle,
    new_issue: Option<(Entity<NewIssueSheet>, Subscription)>,
    /// The Welcome screen's remote repositories, built on first show and
    /// again after the accounts change.
    remote_browser: Option<(Entity<RemoteBrowser>, Subscription)>,
    /// A path to open Purge from History for on the next render.
    purge_requested: Option<PathBuf>,
    /// Accounts and HTTP for pull requests and issues; built when first
    /// needed unless a test set them.
    hosted_env: Option<HostedEnv>,
    /// A sheet a view asked for, opened on the next render.
    sheet_request: Option<SheetRequest>,
    /// Shows the welcome screen in place of the active tab (New Tab).
    welcome: bool,
    message: Option<SharedString>,
    /// A message that came from a failed operation, whose command the
    /// activity log has; the banner offers Show Details while it shows.
    message_details: Option<SharedString>,
    /// The message last shown as a toast, so each one is shown once.
    toasted: Option<SharedString>,
    toasts: Entity<ToastState>,
    repo_menu: Entity<MenuState>,
    repo_menu_key: title_bar::RepoMenuKey,
    branch_handle: DialogHandle,
    branch_picker: Option<(Entity<CommandState>, Subscription)>,
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
        let git = Git::locate(crate::settings::get(cx).git_path.as_deref())
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
            rebase_handle: DialogHandle::new(false),
            rebase: None,
            rebase_events: None,
            rebase_request: None,
            accounts_handle: DialogHandle::new(false),
            accounts: None,
            accounts_events: None,
            settings_handle: DialogHandle::new(false),
            settings: None,
            auto_fetch: None,
            publish_handle: DialogHandle::new(false),
            publish: None,
            create_pull_handle: DialogHandle::new(false),
            create_pull: None,
            new_issue_handle: DialogHandle::new(false),
            new_issue: None,
            remote_browser: None,
            purge_requested: None,
            hosted_env: None,
            sheet_request: None,
            welcome: false,
            message: None,
            message_details: None,
            toasted: None,
            toasts: cx.new(|cx| {
                let mut toasts = ToastState::new(cx);
                toasts.set_placement(ToastPlacement::BottomRight, cx);
                toasts
            }),
            repo_menu: cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(300.))),
            repo_menu_key: title_bar::RepoMenuKey::default(),
            branch_handle: DialogHandle::new(false),
            branch_picker: None,
            watch,
            focus,
        };
        for path in &session.tabs {
            // A repository that moved or was deleted is dropped quietly.
            let _ = this.open_path(path, false, cx);
        }
        this.active = session.active_tab.min(this.tabs.len().saturating_sub(1));
        this.save_session();
        // Only a watched window (the app, not tests) fetches on its own.
        if watch {
            this.auto_fetch = Some(cx.spawn(async move |this, cx| {
                let mut minutes = 0;
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(60))
                        .await;
                    minutes += 1;
                    let Ok(()) = this.update(cx, |this, cx| {
                        let every = crate::settings::get(cx).auto_fetch_minutes;
                        if every > 0 && minutes >= every {
                            minutes = 0;
                            this.fetch_all_in_background(cx);
                        }
                    }) else {
                        break;
                    };
                }
            }));
        }
        this
    }

    /// Fetches every open repository quietly: a failure, such as being
    /// offline, shows in the activity log but not in the banner.
    pub fn fetch_all_in_background(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            tab.model.update(cx, RepositoryModel::fetch_in_background);
        }
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
                match event {
                    RepositoryEvent::OperationFinished {
                        error: Some(error),
                        quiet: false,
                        ..
                    } => {
                        this.message = Some(error.clone());
                        this.message_details = Some(error.clone());
                    }
                    RepositoryEvent::OperationFinished {
                        name,
                        error: None,
                        quiet: false,
                    } => {
                        if let Some(title) = success_title(name) {
                            this.toasts.update(cx, |toasts, cx| {
                                toasts.push(
                                    "workspace.done",
                                    Notice::new(title)
                                        .description("Done.")
                                        .timeout(std::time::Duration::from_secs(3)),
                                    cx,
                                );
                            });
                        }
                    }
                    _ => {}
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
                        if this.section != Section::History {
                            let _ = diff_target.update(cx, |diff, cx| {
                                diff.show(target.clone(), title.clone(), cx);
                            });
                        }
                    }
                    ChangesEvent::Purge(path) => {
                        this.purge_requested = Some(path.clone());
                        cx.notify();
                    }
                },
            );
            let activity = cx.new(|cx| ActivityPanel::new(model.clone(), cx));
            let history = cx.new(|cx| HistoryView::new(model.clone(), refs.clone(), cx));
            let diff_target = diff.downgrade();
            let history_model = model.clone();
            let history_events =
                cx.subscribe(&history, move |this, _, event: &HistoryEvent, cx| {
                    let selected = match event {
                        HistoryEvent::Selected(selected) => selected,
                        HistoryEvent::EditCommits(request) => {
                            this.rebase_request = Some((history_model.clone(), request.clone()));
                            cx.notify();
                            return;
                        }
                        HistoryEvent::BrowseFiles(oid) => {
                            this.browse_files(oid, cx);
                            return;
                        }
                    };
                    if this.section == Section::History {
                        let (target, title) = selected
                            .clone()
                            .map_or((None, SharedString::default()), |(oid, summary)| {
                                (Some(DiffTarget::Commit(oid)), summary)
                            });
                        let _ = diff_target.update(cx, |diff, cx| diff.show(target, title, cx));
                    }
                });
            let diff_target = diff.downgrade();
            let refs_events =
                cx.subscribe(&refs, move |this, refs_entity, event: &RefsEvent, cx| {
                    match event {
                        RefsEvent::ShowDiff(target, title) => {
                            this.section = Section::Changes;
                            let _ = diff_target.update(cx, |diff, cx| {
                                diff.show(Some(target.clone()), title.clone(), cx);
                            });
                        }
                        RefsEvent::ShowInHistory(oid) => {
                            if let Some(history) = this
                                .tabs
                                .iter()
                                .find(|tab| tab.refs == refs_entity)
                                .map(|tab| tab.history.clone())
                            {
                                history.update(cx, |history, cx| history.reveal(oid, cx));
                                this.set_section(Section::History, cx);
                            }
                        }
                        RefsEvent::OpenRepository(path) => {
                            if let Err(error) = this.open_path(path, true, cx) {
                                this.message = Some(error);
                            }
                        }
                    }
                    cx.notify();
                });
            let history_target = history.downgrade();
            let diff_events = cx.subscribe(&diff, move |this, _, event: &DiffPaneEvent, cx| {
                let DiffPaneEvent::ShowInHistory(oid) = event;
                let _ = history_target.update(cx, |history, cx| history.reveal(oid, cx));
                this.set_section(Section::History, cx);
            });
            let files = cx.new(|cx| FilesView::new(model.clone(), cx));
            let files_history = history.downgrade();
            let files_events = cx.subscribe(&files, move |this, _, event: &FilesEvent, cx| {
                match event {
                    FilesEvent::Purge(path) => this.purge_requested = Some(path.clone()),
                    FilesEvent::History(path) => {
                        let search = format!("path:{}", path.to_string_lossy());
                        let _ =
                            files_history.update(cx, |history, cx| history.set_search(&search, cx));
                        this.set_section(Section::History, cx);
                    }
                    FilesEvent::ShowCommit(oid) => {
                        let _ = files_history.update(cx, |history, cx| history.reveal(oid, cx));
                        this.set_section(Section::History, cx);
                    }
                }
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
                history,
                files,
                pulls: None,
                issues: None,
                _changes_events: changes_events,
                _history_events: history_events,
                _refs_events: refs_events,
                _diff_events: diff_events,
                _files_events: files_events,
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

    /// Opens Edit Commits (an interactive rebase) for `model`.
    pub fn show_rebase(
        &mut self,
        model: Entity<RepositoryModel>,
        request: EditCommits,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sheet = cx.new(|cx| RebaseSheet::new(model, request.base, request.preset, window, cx));
        self.rebase_events = Some(cx.subscribe_in(
            &sheet,
            window,
            |this, _, event: &RebaseEvent, window, cx| match event {
                RebaseEvent::Dismissed => {
                    this.rebase_handle.close(window, cx);
                    this.rebase = None;
                    cx.notify();
                }
            },
        ));
        self.rebase = Some(sheet);
        self.rebase_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn rebase_sheet(&self) -> Option<&Entity<RebaseSheet>> {
        self.rebase.as_ref()
    }

    fn render_rebase(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = dialog_popup("workspace.rebase.popup", "Edit Commits", cx)
            .max_w(px(760.))
            .when_some(self.rebase.clone(), gpui_kit::ParentElement::child);
        dialog("workspace.rebase", &self.rebase_handle, popup, window, cx)
            .on_cancel(move |_, _, _| true)
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

    /// Where the accounts list is kept: beside the session file.
    fn accounts_path(&self) -> PathBuf {
        let dir = self
            .session_path
            .as_deref()
            .and_then(Path::parent)
            .map_or_else(crate::session::data_dir, Path::to_path_buf);
        crate::accounts::accounts_path(&dir)
    }

    /// Opens the Accounts sheet with the Keychain and the system's HTTP.
    pub fn show_accounts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let accounts = Accounts::load(
            self.accounts_path(),
            std::sync::Arc::from(bonsai_keychain::default_store()),
        );
        let client = std::sync::Arc::new(crate::http::CurlClient::default());
        self.show_accounts_with(accounts, client, window, cx);
    }

    pub fn show_accounts_with(
        &mut self,
        accounts: Accounts,
        client: std::sync::Arc<dyn bonsai_providers::HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sheet = cx.new(|cx| AccountsSheet::new(accounts, client, window, cx));
        self.accounts_events = Some(cx.subscribe_in(
            &sheet,
            window,
            |this, _, event: &AccountsEvent, window, cx| match event {
                AccountsEvent::Dismissed => {
                    this.accounts_handle.close(window, cx);
                    this.accounts = None;
                    this.remote_browser = None;
                    if let Some(view) = this.hosted_view() {
                        view.update(cx, HostedView::activate);
                    }
                    cx.notify();
                }
            },
        ));
        self.accounts = Some(sheet);
        self.accounts_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn accounts_sheet(&self) -> Option<&Entity<AccountsSheet>> {
        self.accounts.as_ref()
    }

    pub fn show_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sheet = cx.new(|cx| SettingsSheet::new(window, cx));
        let events = cx.subscribe_in(
            &sheet,
            window,
            |this, _, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Dismissed => {
                    this.settings_handle.close(window, cx);
                    this.settings = None;
                    cx.notify();
                }
            },
        );
        self.settings = Some((sheet, events));
        self.settings_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn settings_sheet(&self) -> Option<&Entity<SettingsSheet>> {
        self.settings.as_ref().map(|(sheet, _)| sheet)
    }

    fn on_settings(
        &mut self,
        _: &commands::ShowSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_settings(window, cx);
    }

    fn render_settings(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = dialog_popup("workspace.settings.popup", "Settings", cx)
            .max_w(px(560.))
            .when_some(
                self.settings.as_ref().map(|(sheet, _)| sheet.clone()),
                gpui_kit::ParentElement::child,
            );
        let this = cx.entity().downgrade();
        dialog(
            "workspace.settings",
            &self.settings_handle,
            popup,
            window,
            cx,
        )
        .on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |workspace, cx| {
                workspace.settings = None;
                cx.notify();
            });
            true
        })
    }

    fn on_terminal(
        &mut self,
        _: &commands::OpenInTerminal,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(model) = self.active_model().filter(|_| !self.welcome) {
            let root = model.read(cx).repository().root().to_path_buf();
            if let Err(error) = crate::settings::open_terminal(&root, cx) {
                self.message = Some(error.into());
                cx.notify();
            }
        }
    }

    fn on_accounts(
        &mut self,
        _: &commands::ManageAccounts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_accounts(window, cx);
    }

    fn render_accounts(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = dialog_popup("workspace.accounts.popup", "Accounts", cx)
            .max_w(px(540.))
            .when_some(self.accounts.clone(), gpui_kit::ParentElement::child);
        let this = cx.entity().downgrade();
        dialog(
            "workspace.accounts",
            &self.accounts_handle,
            popup,
            window,
            cx,
        )
        .on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |workspace, cx| {
                workspace.accounts = None;
                workspace.remote_browser = None;
                cx.notify();
            });
            true
        })
    }

    /// Builds the Welcome screen's remote browser with the Keychain and the
    /// system's HTTP.
    fn ensure_remote_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Without a session file (tests), there is no accounts file either.
        if self.remote_browser.is_some() || self.git.is_err() || self.session_path.is_none() {
            return;
        }
        let accounts = Accounts::load(
            self.accounts_path(),
            std::sync::Arc::from(bonsai_keychain::default_store()),
        );
        let client = std::sync::Arc::new(crate::http::CurlClient::default());
        self.set_remote_browser(accounts, client, window, cx);
    }

    pub fn set_remote_browser(
        &mut self,
        accounts: Accounts,
        client: std::sync::Arc<dyn bonsai_providers::HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let recents = self.session.recents.clone();
        let browser = cx.new(|cx| RemoteBrowser::new(accounts, client, recents, window, cx));
        let events = cx.subscribe_in(
            &browser,
            window,
            |this, _, event: &RemoteBrowserEvent, window, cx| match event {
                RemoteBrowserEvent::Clone(url) => this.show_clone(Some(url), window, cx),
                RemoteBrowserEvent::Open(path) => this.open_recent(path, cx),
                RemoteBrowserEvent::ManageAccounts => this.show_accounts(window, cx),
            },
        );
        self.remote_browser = Some((browser, events));
        cx.notify();
    }

    #[must_use]
    pub fn remote_browser(&self) -> Option<&Entity<RemoteBrowser>> {
        self.remote_browser.as_ref().map(|(browser, _)| browser)
    }

    /// Opens Publish for the active repository.
    pub fn show_publish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let accounts = Accounts::load(
            self.accounts_path(),
            std::sync::Arc::from(bonsai_keychain::default_store()),
        );
        let client = std::sync::Arc::new(crate::http::CurlClient::default());
        self.show_publish_with(accounts, client, window, cx);
    }

    pub fn show_publish_with(
        &mut self,
        accounts: Accounts,
        client: std::sync::Arc<dyn bonsai_providers::HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.active_model().cloned().filter(|_| !self.welcome) else {
            return;
        };
        let sheet = cx.new(|cx| PublishSheet::new(model, accounts, client, window, cx));
        let events = cx.subscribe_in(
            &sheet,
            window,
            |this, _, event: &PublishEvent, window, cx| {
                this.publish_handle.close(window, cx);
                this.publish = None;
                if matches!(event, PublishEvent::ManageAccounts) {
                    this.show_accounts(window, cx);
                }
                cx.notify();
            },
        );
        self.publish = Some((sheet, events));
        self.publish_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn publish_sheet(&self) -> Option<&Entity<PublishSheet>> {
        self.publish.as_ref().map(|(sheet, _)| sheet)
    }

    fn on_publish(
        &mut self,
        _: &commands::PublishRepository,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_publish(window, cx);
    }

    fn on_create_pull_request(
        &mut self,
        _: &commands::CreatePullRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_create_pull(window, cx);
    }

    /// Opens New Pull Request for the active repository's hosted remote,
    /// showing Pull Requests behind it.
    pub fn show_create_pull(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome || self.tabs.is_empty() {
            return;
        }
        self.set_section(Section::PullRequests, cx);
        let Some(view) = self.ensure_hosted(cx) else {
            return;
        };
        let Some(target) = view.read(cx).target().cloned() else {
            // The section explains what is missing: a remote or an account.
            return;
        };
        let env = self.hosted_env();
        let accounts = Accounts::load(env.accounts_path.clone(), env.store.clone());
        let model = self.tabs[self.active].model.clone();
        let sheet = cx.new(|cx| {
            CreatePullSheet::new(model, target, &accounts, env.client.clone(), window, cx)
        });
        let list = view.downgrade();
        let events = cx.subscribe_in(
            &sheet,
            window,
            move |this, _, event: &CreatePullEvent, window, cx| match event {
                CreatePullEvent::Dismissed => {
                    this.create_pull_handle.close(window, cx);
                    this.create_pull = None;
                    cx.notify();
                }
                CreatePullEvent::Created(number) => {
                    let number = *number;
                    let _ = list.update(cx, |list, cx| {
                        list.select(number, cx);
                        list.reload(cx);
                    });
                }
            },
        );
        self.create_pull = Some((sheet, events));
        self.create_pull_handle.open(window, cx);
        cx.notify();
    }

    fn on_new_issue(
        &mut self,
        _: &commands::NewIssue,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_new_issue(window, cx);
    }

    /// Opens New Issue for the active repository's hosted remote, showing
    /// Issues behind it.
    pub fn show_new_issue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.welcome || self.tabs.is_empty() {
            return;
        }
        self.set_section(Section::Issues, cx);
        let Some(view) = self.ensure_hosted(cx) else {
            return;
        };
        let Some(target) = view.read(cx).target().cloned() else {
            return;
        };
        let env = self.hosted_env();
        let accounts = Accounts::load(env.accounts_path.clone(), env.store.clone());
        let sheet =
            cx.new(|cx| NewIssueSheet::new(target, &accounts, env.client.clone(), window, cx));
        let list = view.downgrade();
        let events = cx.subscribe_in(
            &sheet,
            window,
            move |this, _, event: &NewIssueEvent, window, cx| match event {
                NewIssueEvent::Dismissed => {
                    this.new_issue_handle.close(window, cx);
                    this.new_issue = None;
                    cx.notify();
                }
                NewIssueEvent::Created(number) => {
                    let number = *number;
                    let _ = list.update(cx, |list, cx| {
                        list.select(number, cx);
                        list.reload(cx);
                    });
                }
            },
        );
        self.new_issue = Some((sheet, events));
        self.new_issue_handle.open(window, cx);
        cx.notify();
    }

    #[must_use]
    pub fn new_issue_sheet(&self) -> Option<&Entity<NewIssueSheet>> {
        self.new_issue.as_ref().map(|(sheet, _)| sheet)
    }

    fn render_new_issue(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = dialog_popup("workspace.new-issue.popup", "New Issue", cx)
            .max_w(px(560.))
            .when_some(
                self.new_issue_sheet().cloned(),
                gpui_kit::ParentElement::child,
            );
        let this = cx.entity().downgrade();
        dialog(
            "workspace.new-issue",
            &self.new_issue_handle,
            popup,
            window,
            cx,
        )
        .on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |workspace, cx| {
                workspace.new_issue = None;
                cx.notify();
            });
            true
        })
    }

    #[must_use]
    pub fn create_pull_sheet(&self) -> Option<&Entity<CreatePullSheet>> {
        self.create_pull.as_ref().map(|(sheet, _)| sheet)
    }

    fn render_create_pull(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = dialog_popup("workspace.create-pull.popup", "New Pull Request", cx)
            .max_w(px(560.))
            .when_some(
                self.create_pull_sheet().cloned(),
                gpui_kit::ParentElement::child,
            );
        let this = cx.entity().downgrade();
        dialog(
            "workspace.create-pull",
            &self.create_pull_handle,
            popup,
            window,
            cx,
        )
        .on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |workspace, cx| {
                workspace.create_pull = None;
                cx.notify();
            });
            true
        })
    }

    fn render_publish(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = dialog_popup("workspace.publish.popup", "Publish Repository", cx)
            .max_w(px(520.))
            .when_some(
                self.publish_sheet().cloned(),
                gpui_kit::ParentElement::child,
            );
        let this = cx.entity().downgrade();
        dialog("workspace.publish", &self.publish_handle, popup, window, cx).on_cancel(
            move |_, _, cx| {
                let _ = this.update(cx, |workspace, cx| {
                    workspace.publish = None;
                    cx.notify();
                });
                true
            },
        )
    }

    fn render_purge(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let content = self.purge.clone();
        let popup = dialog_popup("workspace.purge.popup", "Purge from History", cx)
            .max_w(px(620.))
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

    /// Switches the list pane, and the diff pane to what that section has
    /// selected.
    pub fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        if let Some(tab) = self.tabs.get(self.active) {
            match section {
                Section::History => tab.history.update(cx, HistoryView::emit_selection),
                Section::Changes | Section::Activity => {
                    tab.changes.update(cx, ChangesView::refocus);
                }
                Section::Files => tab.files.update(cx, FilesView::activate),
                Section::PullRequests | Section::Issues => {}
            }
        }
        if let Some(view) = self.ensure_hosted(cx) {
            view.update(cx, HostedView::activate);
        }
        cx.notify();
    }

    /// Accounts and HTTP for pull requests and issues, from now on.
    pub fn set_hosted_env(&mut self, env: HostedEnv) {
        self.hosted_env = Some(env);
    }

    fn hosted_env(&mut self) -> HostedEnv {
        if self.hosted_env.is_none() {
            self.hosted_env = Some(HostedEnv {
                accounts_path: self.accounts_path(),
                store: std::sync::Arc::from(bonsai_keychain::default_store()),
                client: crate::http::default_client(),
            });
        }
        self.hosted_env.clone().expect("set above")
    }

    /// The active tab's view for the current section, when that section is
    /// pull requests or issues, building it on first use.
    fn ensure_hosted(&mut self, cx: &mut Context<Self>) -> Option<Entity<HostedView>> {
        let kind = self.section.hosted()?;
        let active = self.active;
        let model = self.tabs.get(active)?.model.clone();
        let existing = {
            let tab = &self.tabs[active];
            match kind {
                HostedKind::PullRequests => &tab.pulls,
                HostedKind::Issues => &tab.issues,
            }
            .as_ref()
            .map(|(view, _)| view.clone())
        };
        if let Some(view) = existing {
            return Some(view);
        }
        let env = self.hosted_env();
        let view = cx.new(|cx| HostedView::new(kind, model, env, cx));
        let events = cx.subscribe(&view, |this, _, event: &HostedEvent, cx| match event {
            HostedEvent::ManageAccounts => {
                this.sheet_request = Some(SheetRequest::Accounts);
                cx.notify();
            }
            HostedEvent::CreateIssue => {
                this.sheet_request = Some(SheetRequest::NewIssue);
                cx.notify();
            }
            HostedEvent::CreatePullRequest => {
                this.sheet_request = Some(SheetRequest::CreatePull);
                cx.notify();
            }
        });
        view.update(cx, HostedView::activate);
        let tab = &mut self.tabs[active];
        let slot = match kind {
            HostedKind::PullRequests => &mut tab.pulls,
            HostedKind::Issues => &mut tab.issues,
        };
        *slot = Some((view.clone(), events));
        Some(view)
    }

    /// Shows `oid`'s files in Files.
    pub fn browse_files(&mut self, oid: &Oid, cx: &mut Context<Self>) {
        let Some(files) = self.files_view() else {
            return;
        };
        let label: String = oid.as_str().chars().take(7).collect();
        files.update(cx, |files, cx| {
            files.set_revision(
                Some(Revision {
                    spec: oid.as_str().to_owned(),
                    label: label.into(),
                }),
                cx,
            );
        });
        self.set_section(Section::Files, cx);
        cx.notify();
    }

    /// The active tab's Files view.
    #[must_use]
    pub fn files_view(&self) -> Option<Entity<FilesView>> {
        self.tabs.get(self.active).map(|tab| tab.files.clone())
    }

    /// The active tab's view for the current section, if it is built.
    #[must_use]
    pub fn hosted_view(&self) -> Option<Entity<HostedView>> {
        let tab = self.tabs.get(self.active)?;
        match self.section.hosted()? {
            HostedKind::PullRequests => tab.pulls.as_ref(),
            HostedKind::Issues => tab.issues.as_ref(),
        }
        .map(|(view, _)| view.clone())
    }

    fn on_changes(&mut self, _: &commands::ShowChanges, _: &mut Window, cx: &mut Context<Self>) {
        self.set_section(Section::Changes, cx);
        cx.notify();
    }

    fn on_history(&mut self, _: &commands::ShowHistory, _: &mut Window, cx: &mut Context<Self>) {
        self.set_section(Section::History, cx);
        cx.notify();
    }

    fn on_files(&mut self, _: &commands::ShowFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.set_section(Section::Files, cx);
        cx.notify();
    }

    fn on_activity(&mut self, _: &commands::ShowActivity, _: &mut Window, cx: &mut Context<Self>) {
        self.set_section(Section::Activity, cx);
        cx.notify();
    }

    fn on_pull_requests(
        &mut self,
        _: &commands::ShowPullRequests,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_section(Section::PullRequests, cx);
    }

    fn on_issues(&mut self, _: &commands::ShowIssues, _: &mut Window, cx: &mut Context<Self>) {
        self.set_section(Section::Issues, cx);
    }

    /// Opens the activity log at the command behind the error banner.
    pub fn show_message_details(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            tab.activity
                .update(cx, ActivityPanel::select_latest_failure);
        }
        self.set_section(Section::Activity, cx);
        cx.notify();
    }

    fn on_commit(&mut self, _: &commands::Commit, _: &mut Window, cx: &mut Context<Self>) {
        if self.welcome {
            return;
        }
        if let Some(changes) = self.tabs.get(self.active).map(|tab| tab.changes.clone()) {
            self.set_section(Section::Changes, cx);
            changes.update(cx, ChangesView::commit);
        }
    }

    fn on_commit_and_push(
        &mut self,
        _: &commands::CommitAndPush,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.welcome {
            return;
        }
        if let Some(changes) = self.tabs.get(self.active).map(|tab| tab.changes.clone()) {
            self.set_section(Section::Changes, cx);
            changes.update(cx, ChangesView::commit_and_push);
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

    fn on_push(&mut self, _: &commands::Push, window: &mut Window, cx: &mut Context<Self>) {
        let no_remotes = self
            .active_model()
            .and_then(|m| m.read(cx).snapshot())
            .is_some_and(|s| s.remotes.is_empty());
        if no_remotes {
            self.show_publish(window, cx);
            return;
        }
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

    /// Shows the current message, once, as a toast that stays until it is
    /// dismissed; dismissing it clears the message.
    fn show_message_toast(&mut self, cx: &mut Context<Self>) {
        if self.message == self.toasted {
            return;
        }
        self.toasted = self.message.clone();
        let Some(message) = self.message.clone() else {
            self.toasts.update(cx, |toasts, cx| {
                toasts.dismiss(&SharedString::from("workspace.message"), cx);
            });
            return;
        };
        let (title, description) = split_message(&message);
        // The full output is one click away, in Show Details.
        let mut lines: Vec<&str> = description
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        if lines.len() > 3 {
            lines.truncate(3);
            lines.push("…");
        }
        let description = lines.join("\n");
        let mut notice = Notice::new(title)
            .description(description)
            .persistent()
            .cancel("Dismiss");
        let this = cx.entity().downgrade();
        if self.message_details.as_ref() == Some(&message) {
            let this = this.clone();
            notice = notice.action("Show Details", move |_, cx| {
                let _ = this.update(cx, Self::show_message_details);
            });
        }
        let notice = notice.on_dismiss(move |_, cx| {
            let _ = this.update(cx, |this, cx| {
                if this.toasted == this.message {
                    this.message = None;
                    this.toasted = None;
                    cx.notify();
                }
            });
        });
        self.toasts.update(cx, |toasts, cx| {
            toasts.dismiss(&SharedString::from("workspace.message"), cx);
            toasts.push("workspace.message", notice, cx);
        });
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
            .bg(theme.colors.sidebar)
            .child(div().flex_1().min_w_0().child(tabs))
    }

    fn render_sidebar(
        &self,
        snapshot: Option<&Snapshot>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx);
        let c = &theme.colors;
        let changes = snapshot.map_or(0, |s| s.changes.len());
        let mut sidebar = Sidebar::new("workspace.sidebar", "Repository")
            .content_padding(px(8.))
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
                        this.set_section(Section::Changes, cx);
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
                        this.set_section(Section::History, cx);
                        cx.notify();
                    })),
            )
            .child(
                SidebarItem::new("workspace.section.files", "Files")
                    .icon(
                        lucide(LucideIcon::FolderTree)
                            .size(px(16.))
                            .text_color(c.sidebar_foreground),
                    )
                    .selected(self.section == Section::Files)
                    .on_activate(cx.listener(|this, _, _, cx| {
                        this.set_section(Section::Files, cx);
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
                        this.set_section(Section::Activity, cx);
                        cx.notify();
                    })),
            );
        // Pull requests and issues, for a repository with a hosted remote.
        let hosted = snapshot.is_some_and(|s| {
            s.remotes.iter().any(|r| {
                r.fetch_url
                    .as_deref()
                    .and_then(bonsai_providers::hosted_repository)
                    .is_some()
            })
        });
        if hosted {
            sidebar = sidebar
                .child(
                    SidebarItem::new("workspace.section.pull-requests", "Pull Requests")
                        .icon(
                            lucide(LucideIcon::GitPullRequest)
                                .size(px(16.))
                                .text_color(c.sidebar_foreground),
                        )
                        .selected(self.section == Section::PullRequests)
                        .on_activate(cx.listener(|this, _, _, cx| {
                            this.set_section(Section::PullRequests, cx);
                        })),
                )
                .child(
                    SidebarItem::new("workspace.section.issues", "Issues")
                        .icon(
                            lucide(LucideIcon::CircleDot)
                                .size(px(16.))
                                .text_color(c.sidebar_foreground),
                        )
                        .selected(self.section == Section::Issues)
                        .on_activate(cx.listener(|this, _, _, cx| {
                            this.set_section(Section::Issues, cx);
                        })),
                );
        }
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
            (Section::History, Some(_), _) => match self.tabs.get(self.active) {
                Some(tab) => tab.history.clone().into_any_element(),
                None => div().into_any_element(),
            },
            (Section::Files, Some(_), _) => match self.tabs.get(self.active) {
                Some(tab) => tab.files.clone().into_any_element(),
                None => div().into_any_element(),
            },
            (Section::PullRequests | Section::Issues, Some(_), _) => match self.hosted_view() {
                Some(view) => view.into_any_element(),
                None => div().into_any_element(),
            },
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
            .map(|this| match self.hosted_view() {
                Some(view) => this.child(HostedView::render_detail(&view, cx)),
                None if self.section == Section::Files => match self.tabs.get(self.active) {
                    Some(tab) => this.child(FilesView::render_detail(&tab.files, cx)),
                    None => this,
                },
                None => match self.tabs.get(self.active) {
                    Some(tab) => this.child(tab.diff.clone()),
                    None => this,
                },
            })
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let available = self.git.is_ok();
        let tile = |id: &'static str,
                    icon: LucideIcon,
                    title: &'static str,
                    description: &'static str,
                    keys: &'static str| {
            div()
                .id(id)
                .flex_1()
                .min_w(px(160.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .p(px(14.))
                .rounded(px(10.))
                .border_1()
                .border_color(c.border)
                .bg(c.background)
                .when(available, |this| {
                    this.cursor_pointer()
                        .hover(|this| this.bg(c.accent).border_color(c.ring))
                })
                .when(!available, |this| this.opacity(0.5))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .size(px(32.))
                                .rounded(px(8.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(c.primary.opacity(0.1))
                                .child(lucide(icon).size(px(17.)).text_color(c.primary)),
                        )
                        .child(
                            div()
                                .px(px(5.))
                                .rounded(px(4.))
                                .border_1()
                                .border_color(c.border)
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(keys),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(description),
                        ),
                )
        };
        let actions = div()
            .flex()
            .flex_wrap()
            .gap(px(12.))
            .child(
                tile(
                    "welcome.open",
                    LucideIcon::FolderOpen,
                    "Open Repository",
                    "A Git folder on this Mac",
                    "⌘O",
                )
                .when(available, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx)))
                }),
            )
            .child(
                tile(
                    "welcome.clone",
                    LucideIcon::Download,
                    "Clone",
                    "From GitHub, GitLab or any URL",
                    "⇧⌘O",
                )
                .when(available, |this| {
                    this.on_click(
                        cx.listener(|this, _, window, cx| this.show_clone(None, window, cx)),
                    )
                }),
            )
            .child(
                tile(
                    "welcome.new",
                    LucideIcon::FolderPlus,
                    "New Repository",
                    "Start one in a new folder",
                    "⌘N",
                )
                .when(available, |this| {
                    this.on_click(cx.listener(|this, _, _, cx| this.prompt_new(cx)))
                }),
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
            .items_center()
            .gap(px(14.))
            .child(
                img(bonsai_mark(theme.mode))
                    .size(px(52.))
                    .flex_none()
                    .object_fit(ObjectFit::Contain),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Welcome to Bonsai"),
                    )
                    .child(div().text_sm().text_color(c.muted_foreground).child(match &self.git {
                        Ok(_) => SharedString::from(
                            "Open a repository on this Mac, clone one from any Git server, or start a new one.",
                        ),
                        Err(error) => error.clone(),
                    })),
            );
        div()
            .id("welcome")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .px(px(24.))
                    .py(px(64.))
                    .flex()
                    .flex_col()
                    .gap(px(28.))
                    .child(header)
                    .child(actions)
                    .when_some(
                        self.remote_browser().cloned(),
                        gpui_kit::ParentElement::child,
                    )
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
                                .child(
                                    div()
                                        .p(px(4.))
                                        .rounded(px(10.))
                                        .border_1()
                                        .border_color(c.border)
                                        .child(list),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(
                                lucide(LucideIcon::ArrowDownToLine)
                                    .size(px(13.))
                                    .text_color(c.muted_foreground),
                            )
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
            .max_w(px(520.))
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

/// The toast for a finished operation worth confirming, such as a push; the
/// small, frequent ones (staging) show nothing.
#[must_use]
pub fn success_title(operation: &str) -> Option<String> {
    const CONFIRMED: [&str; 12] = [
        "Fetch",
        "Pull",
        "Push",
        "Commit",
        "Amend",
        "Switch",
        "Merge",
        "Rebase",
        "Cherry-pick",
        "Revert",
        "Stash",
        "Create",
    ];
    CONFIRMED
        .iter()
        .any(|prefix| operation.starts_with(prefix))
        .then(|| operation.to_owned())
}

/// Splits an error into a short title (its first line, or first sentence)
/// and the rest.
#[must_use]
pub fn split_message(message: &str) -> (String, String) {
    let message = message.trim();
    let (first, rest) = message.split_once('\n').unwrap_or((message, ""));
    if first.chars().count() <= 90 {
        return (first.to_owned(), rest.trim().to_owned());
    }
    match first.find(". ") {
        Some(end) if end < 90 => (
            first[..=end].to_owned(),
            format!("{}\n{rest}", first[end + 2..].trim())
                .trim()
                .to_owned(),
        ),
        _ => (first.to_owned(), rest.trim().to_owned()),
    }
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
        if let Some((model, request)) = self.rebase_request.take() {
            self.show_rebase(model, request, window, cx);
        }
        match self.sheet_request.take() {
            Some(SheetRequest::Accounts) => self.show_accounts(window, cx),
            Some(SheetRequest::CreatePull) => self.show_create_pull(window, cx),
            Some(SheetRequest::NewIssue) => self.show_new_issue(window, cx),
            None => {}
        }
        if !self.welcome {
            self.ensure_hosted(cx);
        }
        if self.welcome || self.tabs.is_empty() {
            self.ensure_remote_browser(window, cx);
        }
        if let Some(path) = self.purge_requested.take() {
            self.show_purge(Some(path), window, cx);
        }
        self.show_message_toast(cx);
        let title_bar = self.render_title_bar(window, cx);
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
            .on_action(cx.listener(Self::on_files))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_commit))
            .on_action(cx.listener(Self::on_commit_and_push))
            .on_action(cx.listener(Self::on_fetch))
            .on_action(cx.listener(Self::on_pull))
            .on_action(cx.listener(Self::on_push))
            .on_action(cx.listener(Self::on_purge))
            .on_action(cx.listener(Self::on_stash))
            .on_action(cx.listener(Self::on_activity))
            .on_action(cx.listener(Self::on_pull_requests))
            .on_action(cx.listener(Self::on_issues))
            .on_action(cx.listener(Self::on_accounts))
            .on_action(cx.listener(Self::on_settings))
            .on_action(cx.listener(Self::on_terminal))
            .on_action(cx.listener(Self::on_publish))
            .on_action(cx.listener(Self::on_create_pull_request))
            .on_action(cx.listener(Self::on_new_issue))
            .on_action(cx.listener(Self::on_switch_branch))
            .on_action(cx.listener(Self::on_new_branch))
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .font_family(theme.fonts.body.clone())
            .child(title_bar)
            .when(self.tabs.len() > 1, |this| this.child(self.render_tabs(cx)))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_palette(window, cx))
            .child(self.render_clone(window, cx))
            .child(self.render_purge(window, cx))
            .child(self.render_rebase(window, cx))
            .child(self.render_accounts(window, cx))
            .child(self.render_settings(window, cx))
            .child(self.render_publish(window, cx))
            .child(self.render_create_pull(window, cx))
            .child(self.render_new_issue(window, cx))
            .child(self.render_branch_picker(window, cx))
            .child(self.toasts.clone())
    }
}

#[cfg(test)]
mod tests;

/// The Bonsai logo for the Welcome screen, drawn for the theme's mode.
fn bonsai_mark(mode: ThemeMode) -> Arc<Image> {
    static LIGHT: LazyLock<Arc<Image>> = LazyLock::new(|| {
        let svg = include_bytes!("../../../../Assets/Brand/bonsai-mark.svg");
        Arc::new(Image::from_bytes(ImageFormat::Svg, svg.to_vec()))
    });
    static DARK: LazyLock<Arc<Image>> = LazyLock::new(|| {
        let svg = include_bytes!("../../../../Assets/Brand/bonsai-mark-dark.svg");
        Arc::new(Image::from_bytes(ImageFormat::Svg, svg.to_vec()))
    });
    match mode {
        ThemeMode::Dark => DARK.clone(),
        ThemeMode::Light => LIGHT.clone(),
    }
}
