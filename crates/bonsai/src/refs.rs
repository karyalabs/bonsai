//! Branches, remote branches, tags and stashes in the sidebar, with their
//! actions (CONTEXT.md: Branch, Tag, Stash).

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use bonsai_git::{
    Branch, CheckoutBlocker, DiffTarget, Head, Integration, MergeMode, NewWorktree, Oid, ResetMode,
    Snapshot, Stash, Submodule, SubmoduleState, Worktree, is_valid_ref_name, stash_diff,
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Render, SharedString, Styled, Subscription,
    WeakEntity, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    confirm::Confirm,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        checkbox::Checkbox,
        context_menu::context_menu,
        dialog::{DialogHandle, dialog, dialog_description, dialog_popup, dialog_title},
        input::{Input, InputEvent, InputState},
        menu::{MenuItem, MenuState},
        sidebar::{SidebarItem, sidebar_group_label},
        textarea::{Textarea, TextareaState},
        theme::UiTheme,
    },
};

/// A dialog the panel is showing or about to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefDialog {
    /// New branch at `start` (a revision; `None` is HEAD).
    CreateBranch {
        start: Option<String>,
    },
    RenameBranch {
        name: String,
    },
    /// `unmerged` once Git refused a plain delete.
    DeleteBranch {
        name: String,
        unmerged: bool,
    },
    CreateTag {
        target: Option<String>,
    },
    DeleteTag {
        name: String,
    },
    /// Reset the current branch to a revision, discarding changes.
    ResetHard {
        target: String,
    },
    AddRemote,
    EditRemoteUrl {
        name: String,
        url: String,
    },
    RenameRemote {
        name: String,
    },
    RemoveRemote {
        name: String,
    },
    DeleteRemoteBranch {
        remote: String,
        branch: String,
    },
    /// A switch was refused; offer to stash first.
    CheckoutBlocked {
        target: Checkout,
        blocker: CheckoutBlocker,
    },
    /// Stash the uncommitted changes with an optional message.
    StashChanges,
    DropStash {
        index: usize,
        oid: Oid,
        message: String,
    },
    /// Check a branch out in a new folder; `branch` prefills the name.
    AddWorktree {
        branch: Option<String>,
    },
    /// `dirty` once Git refused because the worktree has changes.
    RemoveWorktree {
        path: PathBuf,
        dirty: bool,
    },
    /// Clone a repository into a folder of this one.
    AddSubmodule,
    RemoveSubmodule {
        path: PathBuf,
    },
}

/// What the panel asks its owner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RefsEvent {
    /// Show this diff, with this title, in the diff pane.
    ShowDiff(DiffTarget, SharedString),
    /// Select this commit in the History view.
    ShowInHistory(Oid),
    /// Open this folder (a worktree) in a tab.
    OpenRepository(PathBuf),
    /// Compare `head` against `base`.
    Compare { base: String, head: String },
}

/// Something to switch to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Checkout {
    Branch(String),
    /// Create a local branch tracking a remote branch.
    Remote {
        remote: String,
        branch: String,
        local: String,
    },
    Detached(String),
}

impl Checkout {
    fn label(&self) -> String {
        match self {
            Self::Branch(name) | Self::Remote { local: name, .. } => name.clone(),
            Self::Detached(revision) => revision.clone(),
        }
    }

    fn run(&self, repository: &bonsai_git::Repository) -> bonsai_git::Result<()> {
        match self {
            Self::Branch(name) => repository.checkout_branch(name),
            Self::Remote {
                remote,
                branch,
                local,
            } => repository.checkout_remote_branch(remote, branch, local),
            Self::Detached(revision) => repository.checkout_detached(revision),
        }
    }
}

/// Text inputs, created on first render because they need a window.
struct Inputs {
    name: Entity<InputState>,
    url: Entity<InputState>,
    message: Entity<TextareaState>,
    stash_message: Entity<InputState>,
    folder: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

pub struct RefsPanel {
    model: Entity<RepositoryModel>,
    menu: Entity<MenuState>,
    handle: DialogHandle,
    dialog: Option<RefDialog>,
    /// Set when a dialog should open on the next render.
    open_requested: bool,
    /// A follow-up dialog an operation asks for (unmerged delete, blocked
    /// switch), picked up when the operation finishes.
    follow_up: Arc<Mutex<Option<RefDialog>>>,
    /// A worktree to open once the operation creating it succeeds.
    open_after: Arc<Mutex<Option<PathBuf>>>,
    inputs: Option<Inputs>,
    checkout_new: bool,
    stash_untracked: bool,
    collapsed_remotes: BTreeSet<String>,
    _events: Subscription,
}

impl EventEmitter<RefsEvent> for RefsPanel {}

impl RefsPanel {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if let RepositoryEvent::OperationFinished { error, .. } = event {
                let next = this.follow_up.lock().ok().and_then(|mut slot| slot.take());
                if let Some(next) = next {
                    this.show(next, cx);
                }
                let open = this.open_after.lock().ok().and_then(|mut slot| slot.take());
                if let Some(path) = open.filter(|_| error.is_none()) {
                    cx.emit(RefsEvent::OpenRepository(path));
                }
            }
            cx.notify();
        });
        Self {
            model,
            menu: cx.new(|cx| MenuState::new(Vec::new(), cx)),
            handle: DialogHandle::new(false),
            dialog: None,
            open_requested: false,
            follow_up: Arc::new(Mutex::new(None)),
            open_after: Arc::new(Mutex::new(None)),
            inputs: None,
            checkout_new: true,
            stash_untracked: true,
            collapsed_remotes: BTreeSet::new(),
            _events: events,
        }
    }

    #[must_use]
    pub fn dialog(&self) -> Option<&RefDialog> {
        self.dialog.as_ref()
    }

    /// Opens a dialog on the next render.
    pub fn show(&mut self, dialog: RefDialog, cx: &mut Context<Self>) {
        self.dialog = Some(dialog);
        self.open_requested = true;
        cx.notify();
    }

    fn snapshot<'a>(&self, cx: &'a gpui_kit::App) -> Option<&'a Snapshot> {
        self.model.read(cx).snapshot()
    }

    /// Switches, asking to stash first when local changes are in the way.
    pub fn checkout(&mut self, target: Checkout, cx: &mut Context<Self>) {
        let slot = self.follow_up.clone();
        let name = format!("Switch to {}", target.label());
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| match target.run(repository) {
                    Err(error) => match error.checkout_blocker() {
                        Some(blocker) => {
                            if let Ok(mut slot) = slot.lock() {
                                *slot = Some(RefDialog::CheckoutBlocked { target, blocker });
                            }
                            Ok(())
                        }
                        None => Err(error),
                    },
                    ok => ok,
                },
                cx,
            );
        });
    }

    fn stash_and_checkout(&mut self, target: Checkout, cx: &mut Context<Self>) {
        let name = format!("Stash and switch to {}", target.label());
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| {
                    repository.stash_all(&format!("Before switching to {}", target.label()))?;
                    target.run(repository)
                },
                cx,
            );
        });
    }

    /// Deletes a branch, asking before forcing one that is not merged.
    pub fn delete_branch(&mut self, name: String, force: bool, cx: &mut Context<Self>) {
        let slot = self.follow_up.clone();
        self.model.update(cx, |model, cx| {
            model.run(
                format!("Delete branch {name}"),
                move |repository| match repository.delete_branch(&name, force) {
                    Err(error) if !force && error.is_not_fully_merged() => {
                        if let Ok(mut slot) = slot.lock() {
                            *slot = Some(RefDialog::DeleteBranch {
                                name,
                                unmerged: true,
                            });
                        }
                        Ok(())
                    }
                    result => result,
                },
                cx,
            );
        });
    }

    fn root(&self, cx: &gpui_kit::App) -> PathBuf {
        self.model.read(cx).repository().root().to_path_buf()
    }

    /// The worktree with `branch` checked out, other than this one.
    fn worktree_of<'a>(
        &self,
        branch: &str,
        snapshot: &'a Snapshot,
        cx: &gpui_kit::App,
    ) -> Option<&'a Worktree> {
        let root = canonical(&self.root(cx));
        snapshot
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(branch) && canonical(&w.path) != root)
    }

    /// Where a new worktree goes: the folder the person typed, or a sibling
    /// of the main worktree named after the branch.
    fn worktree_folder(&self, cx: &gpui_kit::App) -> PathBuf {
        let typed = self
            .inputs
            .as_ref()
            .map(|i| i.folder.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        if !typed.is_empty() {
            return expand_home(&typed);
        }
        let main = self
            .snapshot(cx)
            .and_then(|s| s.worktrees.iter().find(|w| w.main))
            .map_or_else(|| self.root(cx), |w| w.path.clone());
        default_worktree_folder(&main, &self.name_value(cx))
    }

    /// Removes a worktree, asking before forcing one with changes.
    pub fn remove_worktree(&mut self, path: PathBuf, force: bool, cx: &mut Context<Self>) {
        let slot = self.follow_up.clone();
        let name = format!(
            "Remove worktree {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        self.run(
            name,
            move |repository| match repository.remove_worktree(&path, force) {
                Err(error) if !force && error.to_string().contains("--force") => {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some(RefDialog::RemoveWorktree { path, dirty: true });
                    }
                    Ok(())
                }
                result => result,
            },
            cx,
        );
    }

    fn worktree_menu(
        &self,
        worktree: &Worktree,
        current: bool,
        cx: &mut Context<Self>,
    ) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let path = worktree.path.clone();
        let mut items = Vec::new();
        if !current && !worktree.prunable {
            items.push(
                MenuItem::new("refs.worktree.open", "Open in Tab")
                    .icon(LucideIcon::AppWindow)
                    .on_click(with(&this, {
                        let path = path.clone();
                        move |_, _, cx| cx.emit(RefsEvent::OpenRepository(path.clone()))
                    })),
            );
        }
        if !worktree.prunable {
            let reveal = path.clone();
            items.push(
                MenuItem::new("refs.worktree.reveal", "Reveal in Finder")
                    .icon(LucideIcon::FolderOpen)
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
            );
        }
        let copy = path.to_string_lossy().into_owned();
        items.push(
            MenuItem::new("refs.worktree.copy", "Copy Path")
                .icon(LucideIcon::Copy)
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                }),
        );
        if worktree.prunable {
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new("refs.worktree.prune", "Forget Missing Worktrees")
                    .icon(LucideIcon::Eraser)
                    .on_click(with(&this, |view, _, cx| {
                        view.run(
                            "Prune worktrees".to_owned(),
                            bonsai_git::Repository::prune_worktrees,
                            cx,
                        );
                    })),
            );
        } else if !worktree.main && !current {
            items.push(MenuItem::separator());
            items.push(
                MenuItem::new("refs.worktree.remove", "Remove…")
                    .icon(LucideIcon::Trash2)
                    .disabled(worktree.locked.is_some())
                    .on_click(show(
                        &this,
                        RefDialog::RemoveWorktree { path, dirty: false },
                    )),
            );
        }
        items
    }

    /// Applies a stash; `pop` drops it too when it applied without
    /// conflicts. Conflicts show up in Changes, and the stash stays.
    pub fn apply_stash(&mut self, stash: &Stash, pop: bool, cx: &mut Context<Self>) {
        let (index, oid) = (stash.index, stash.oid.clone());
        let name = format!(
            "{} stash {}",
            if pop { "Pop" } else { "Apply" },
            stash.message
        );
        self.run(
            name,
            move |repository| match repository.stash_apply(index, &oid, pop)? {
                Integration::Completed => Ok(()),
                Integration::Conflicts if pop => Err(bonsai_git::Error::Refused(
                    "The stash conflicts with your changes. Resolve the conflicts in Changes; the stash was kept."
                        .to_owned(),
                )),
                Integration::Conflicts => Err(bonsai_git::Error::Refused(
                    "The stash conflicts with your changes. Resolve the conflicts in Changes."
                        .to_owned(),
                )),
            },
            cx,
        );
    }

    /// Shows a stash's tracked changes in the diff pane.
    pub fn view_stash(&mut self, stash: &Stash, cx: &mut Context<Self>) {
        cx.emit(RefsEvent::ShowDiff(
            stash_diff(&stash.oid),
            SharedString::from(stash.message.clone()),
        ));
    }

    fn stash_menu(&self, stash: &Stash, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let item = |id: &'static str, label: &'static str, icon: LucideIcon| {
            MenuItem::new(id, label).icon(icon)
        };
        let (apply, pop, view) = (stash.clone(), stash.clone(), stash.clone());
        vec![
            item("refs.stash.apply", "Apply", LucideIcon::ArchiveRestore).on_click(with(
                &this,
                move |view, _, cx| {
                    view.apply_stash(&apply, false, cx);
                },
            )),
            item(
                "refs.stash.pop",
                "Pop (Apply and Drop)",
                LucideIcon::PackageOpen,
            )
            .on_click(with(&this, move |view, _, cx| {
                view.apply_stash(&pop, true, cx);
            })),
            item("refs.stash.view", "View Changes", LucideIcon::FileDiff)
                .on_click(with(&this, move |panel, _, cx| panel.view_stash(&view, cx))),
            MenuItem::separator(),
            item("refs.stash.drop", "Drop…", LucideIcon::Trash2).on_click(show(
                &this,
                RefDialog::DropStash {
                    index: stash.index,
                    oid: stash.oid.clone(),
                    message: stash.message.clone(),
                },
            )),
        ]
    }

    /// The current branch's name, or `None` when detached or unborn.
    fn current_branch(&self, cx: &gpui_kit::App) -> Option<String> {
        match &self.snapshot(cx)?.head {
            Head::Branch { name, oid: Some(_) } => Some(name.clone()),
            _ => None,
        }
    }

    pub fn merge(&mut self, revision: String, cx: &mut Context<Self>) {
        let name = format!("Merge {revision}");
        self.run(
            name,
            move |repository| repository.merge(&revision, MergeMode::Default).map(|_| ()),
            cx,
        );
    }

    pub fn rebase(&mut self, onto: String, cx: &mut Context<Self>) {
        let name = format!("Rebase onto {onto}");
        self.run(
            name,
            move |repository| repository.rebase(&onto).map(|_| ()),
            cx,
        );
    }

    pub fn reset(&mut self, target: String, mode: ResetMode, cx: &mut Context<Self>) {
        let name = format!("Reset to {target}");
        self.run(name, move |repository| repository.reset(&target, mode), cx);
    }

    /// Merge, rebase and reset items for a revision other than HEAD.
    fn integrate_items(&self, revision: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let Some(current) = self.current_branch(cx) else {
            return Vec::new();
        };
        if current == revision {
            return Vec::new();
        }
        let this = cx.entity().downgrade();
        let rev = revision.to_owned();
        let reset = vec![
            MenuItem::new("refs.reset.soft", "Soft: keep changes staged").on_click(with(&this, {
                let rev = rev.clone();
                move |view, _, cx| view.reset(rev.clone(), ResetMode::Soft, cx)
            })),
            MenuItem::new("refs.reset.mixed", "Mixed: keep changes unstaged").on_click(with(
                &this,
                {
                    let rev = rev.clone();
                    move |view, _, cx| view.reset(rev.clone(), ResetMode::Mixed, cx)
                },
            )),
            MenuItem::new("refs.reset.hard", "Hard: discard changes…").on_click(show(
                &this,
                RefDialog::ResetHard {
                    target: rev.clone(),
                },
            )),
        ];
        vec![
            MenuItem::separator(),
            MenuItem::new("refs.compare", format!("Compare with {current}"))
                .icon(LucideIcon::GitCompareArrows)
                .on_click({
                    let this = this.clone();
                    let base = current.clone();
                    let head = rev.clone();
                    move |_, _, cx| {
                        let _ = this.update(cx, |_, cx| {
                            cx.emit(RefsEvent::Compare {
                                base: base.clone(),
                                head: head.clone(),
                            });
                        });
                    }
                }),
            MenuItem::new("refs.merge", format!("Merge {rev} into {current}"))
                .icon(LucideIcon::GitMerge)
                .on_click(with(&this, {
                    let rev = rev.clone();
                    move |view, _, cx| view.merge(rev.clone(), cx)
                })),
            MenuItem::new("refs.rebase", format!("Rebase {current} onto {rev}"))
                .icon(LucideIcon::GitPullRequestArrow)
                .on_click(with(&this, {
                    let rev = rev.clone();
                    move |view, _, cx| view.rebase(rev.clone(), cx)
                })),
            MenuItem::submenu("refs.reset", format!("Reset {current} to {rev}"), reset)
                .icon(LucideIcon::RotateCcw),
        ]
    }

    fn run(
        &self,
        name: String,
        operation: impl FnOnce(&bonsai_git::Repository) -> bonsai_git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.model
            .update(cx, |model, cx| model.run(name, operation, cx));
    }

    fn name_value(&self, cx: &gpui_kit::App) -> String {
        self.inputs
            .as_ref()
            .map(|i| i.name.read(cx).value().trim().to_owned())
            .unwrap_or_default()
    }

    fn url_value(&self, cx: &gpui_kit::App) -> String {
        self.inputs
            .as_ref()
            .map(|i| i.url.read(cx).value().trim().to_owned())
            .unwrap_or_default()
    }

    fn can_confirm(&self, cx: &gpui_kit::App) -> bool {
        match &self.dialog {
            Some(RefDialog::AddRemote) => {
                is_valid_ref_name(&self.name_value(cx)) && !self.url_value(cx).is_empty()
            }
            Some(RefDialog::EditRemoteUrl { url, .. }) => {
                let new = self.url_value(cx);
                !new.is_empty() && new != *url
            }
            Some(RefDialog::CreateBranch { .. } | RefDialog::CreateTag { .. }) => {
                is_valid_ref_name(&self.name_value(cx))
            }
            Some(RefDialog::RenameBranch { name } | RefDialog::RenameRemote { name }) => {
                let new = self.name_value(cx);
                is_valid_ref_name(&new) && new != *name
            }
            Some(RefDialog::AddWorktree { .. }) => self.worktree_problem(cx).is_none(),
            Some(RefDialog::AddSubmodule) => self.submodule_problem(cx).is_none(),
            Some(_) => true,
            None => false,
        }
    }

    /// Where the Add Submodule dialog puts the submodule: the folder typed,
    /// or one named after the repository.
    fn submodule_path(&self, cx: &gpui_kit::App) -> PathBuf {
        let typed = self
            .inputs
            .as_ref()
            .map(|i| {
                i.folder
                    .read(cx)
                    .value()
                    .trim()
                    .trim_matches('/')
                    .to_owned()
            })
            .unwrap_or_default();
        if !typed.is_empty() {
            return PathBuf::from(typed);
        }
        PathBuf::from(submodule_folder(&self.url_value(cx)))
    }

    /// Why the Add Submodule dialog can't add yet: empty when nothing is
    /// typed.
    fn submodule_problem(&self, cx: &gpui_kit::App) -> Option<String> {
        if self.url_value(cx).is_empty() {
            return Some(String::new());
        }
        let path = self.submodule_path(cx);
        if path.as_os_str().is_empty() || path.is_absolute() || path.starts_with("..") {
            return Some("Pick a folder inside this repository.".to_owned());
        }
        if self.root(cx).join(&path).exists() {
            return Some(format!("{} already exists.", path.display()));
        }
        None
    }

    /// Runs a submodule command that may clone or fetch.
    fn run_submodules(
        &mut self,
        name: String,
        operation: impl FnOnce(&bonsai_git::Repository) -> bonsai_git::Result<()> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.model.update(cx, |model, cx| {
            model.run_network(name, move |repository, _, _| operation(repository), cx);
        });
    }

    fn submodule_menu(&self, submodule: &Submodule, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let path = submodule.path.clone();
        let absolute = self.root(cx).join(&path);
        let initialized = submodule.state != SubmoduleState::NotInitialized;
        let label = path.display().to_string();
        let mut items = Vec::new();
        if initialized {
            items.push(
                MenuItem::new("refs.submodule.open", "Open in Tab")
                    .icon(LucideIcon::AppWindow)
                    .on_click(with(&this, {
                        let absolute = absolute.clone();
                        move |_, _, cx| cx.emit(RefsEvent::OpenRepository(absolute.clone()))
                    })),
            );
        }
        items.push(
            MenuItem::new(
                "refs.submodule.update",
                if initialized {
                    "Check Out Recorded Commit"
                } else {
                    "Clone"
                },
            )
            .icon(LucideIcon::RefreshCw)
            .disabled(submodule.state == SubmoduleState::UpToDate)
            .on_click(with(&this, {
                let path = path.clone();
                let label = label.clone();
                move |view, _, cx| {
                    let path = path.clone();
                    view.run_submodules(
                        format!("Update submodule {label}"),
                        move |repository| repository.update_submodules(&[path]),
                        cx,
                    );
                }
            })),
        );
        items.push(
            MenuItem::new("refs.submodule.latest", "Update to Latest from Remote")
                .icon(LucideIcon::CloudDownload)
                .on_click(with(&this, {
                    let path = path.clone();
                    let label = label.clone();
                    move |view, _, cx| {
                        let path = path.clone();
                        view.run_submodules(
                            format!("Update submodule {label} to latest"),
                            move |repository| repository.update_submodules_remote(&[path]),
                            cx,
                        );
                    }
                })),
        );
        items.push(MenuItem::separator());
        if initialized {
            items.push(
                MenuItem::new("refs.submodule.reveal", "Reveal in Finder")
                    .icon(LucideIcon::FolderOpen)
                    .on_click(move |_, _, cx| cx.reveal_path(&absolute)),
            );
        }
        if let Some(url) = submodule.url.clone() {
            items.push(
                MenuItem::new("refs.submodule.copy", "Copy URL")
                    .icon(LucideIcon::Copy)
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                    }),
            );
        }
        items.push(MenuItem::separator());
        items.push(
            MenuItem::new("refs.submodule.remove", "Remove…")
                .icon(LucideIcon::Trash2)
                .on_click(show(&this, RefDialog::RemoveSubmodule { path })),
        );
        items
    }

    /// Why the Add Worktree dialog can't create its worktree yet.
    fn worktree_problem(&self, cx: &gpui_kit::App) -> Option<String> {
        let name = self.name_value(cx);
        if !is_valid_ref_name(&name) {
            return Some(String::new());
        }
        let snapshot = self.snapshot(cx)?;
        let root = canonical(&self.root(cx));
        if let Some(worktree) = snapshot
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(name.as_str()))
        {
            return Some(if canonical(&worktree.path) == root {
                format!("{name} is checked out here. Pick another branch or a new name.")
            } else {
                format!(
                    "{name} is already checked out in {}.",
                    worktree.path.display()
                )
            });
        }
        let folder = self.worktree_folder(cx);
        let taken = std::fs::read_dir(&folder).is_ok_and(|mut entries| entries.next().is_some())
            || folder.is_file();
        taken.then(|| format!("{} already exists and isn't empty.", folder.display()))
    }

    /// Runs the open dialog's action.
    pub fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_confirm(cx) {
            return;
        }
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        self.handle.close(window, cx);
        let value = self.name_value(cx);
        match dialog {
            RefDialog::CreateBranch { start } => {
                let checkout = self.checkout_new;
                let label = format!("Create branch {value}");
                self.run(
                    label,
                    move |repository| repository.create_branch(&value, start.as_deref(), checkout),
                    cx,
                );
            }
            RefDialog::RenameBranch { name } => self.run(
                format!("Rename branch {name} to {value}"),
                move |repository| repository.rename_branch(&name, &value),
                cx,
            ),
            RefDialog::DeleteBranch { name, unmerged } => self.delete_branch(name, unmerged, cx),
            RefDialog::CreateTag { target } => {
                let message = self
                    .inputs
                    .as_ref()
                    .map(|i| i.message.read(cx).value().trim().to_owned())
                    .filter(|m| !m.is_empty());
                self.run(
                    format!("Create tag {value}"),
                    move |repository| {
                        repository.create_tag(&value, target.as_deref(), message.as_deref())
                    },
                    cx,
                );
            }
            RefDialog::DeleteTag { name } => self.run(
                format!("Delete tag {name}"),
                move |repository| repository.delete_tag(&name),
                cx,
            ),
            RefDialog::CheckoutBlocked { target, .. } => self.stash_and_checkout(target, cx),
            RefDialog::StashChanges => {
                let message = self
                    .inputs
                    .as_ref()
                    .map(|i| i.stash_message.read(cx).value().trim().to_owned())
                    .unwrap_or_default();
                let untracked = self.stash_untracked;
                self.run(
                    "Stash changes".to_owned(),
                    move |repository| repository.stash_save(&message, untracked),
                    cx,
                );
            }
            RefDialog::DropStash {
                index,
                oid,
                message,
            } => self.run(
                format!("Drop stash {message}"),
                move |repository| repository.stash_drop(index, &oid),
                cx,
            ),
            RefDialog::ResetHard { target } => self.reset(target, ResetMode::Hard, cx),
            RefDialog::AddRemote => {
                let url = self.url_value(cx);
                self.run(
                    format!("Add remote {value}"),
                    move |repository| repository.add_remote(&value, &url),
                    cx,
                );
            }
            RefDialog::EditRemoteUrl { name, .. } => {
                let url = self.url_value(cx);
                self.run(
                    format!("Change the URL of {name}"),
                    move |repository| repository.set_remote_url(&name, &url),
                    cx,
                );
            }
            RefDialog::RenameRemote { name } => self.run(
                format!("Rename remote {name} to {value}"),
                move |repository| repository.rename_remote(&name, &value),
                cx,
            ),
            RefDialog::RemoveRemote { name } => self.run(
                format!("Remove remote {name}"),
                move |repository| repository.remove_remote(&name),
                cx,
            ),
            RefDialog::AddWorktree { .. } => {
                let folder = self.worktree_folder(cx);
                let exists = self
                    .snapshot(cx)
                    .is_some_and(|s| s.branches.iter().any(|b| b.name == value));
                let what = if exists {
                    NewWorktree::Branch(value.clone())
                } else {
                    NewWorktree::NewBranch {
                        name: value.clone(),
                        start: None,
                    }
                };
                if let Ok(mut slot) = self.open_after.lock() {
                    *slot = Some(folder.clone());
                }
                self.run(
                    format!("Create worktree for {value}"),
                    move |repository| repository.add_worktree(&folder, &what),
                    cx,
                );
            }
            RefDialog::RemoveWorktree { path, dirty } => self.remove_worktree(path, dirty, cx),
            RefDialog::AddSubmodule => {
                let url = self.url_value(cx);
                let path = self.submodule_path(cx);
                self.run_submodules(
                    format!("Add submodule {}", path.display()),
                    move |repository| repository.add_submodule(&url, &path),
                    cx,
                );
            }
            RefDialog::RemoveSubmodule { path } => self.run(
                format!("Remove submodule {}", path.display()),
                move |repository| repository.remove_submodule(&path),
                cx,
            ),
            RefDialog::DeleteRemoteBranch { remote, branch } => {
                self.model.update(cx, |model, cx| {
                    model.run_network(
                        format!("Delete {branch} on {remote}"),
                        move |repository, _, _| repository.delete_remote_branch(&remote, &branch),
                        cx,
                    );
                });
            }
        }
    }

    /// Prepares the inputs when a dialog opens.
    fn prepare_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(inputs) = &self.inputs else {
            return;
        };
        let (initial, initial_url) = match &self.dialog {
            Some(RefDialog::RenameBranch { name } | RefDialog::RenameRemote { name }) => {
                (name.clone(), String::new())
            }
            Some(RefDialog::EditRemoteUrl { url, .. }) => (String::new(), url.clone()),
            Some(RefDialog::AddWorktree {
                branch: Some(branch),
            }) => (branch.clone(), String::new()),
            _ => (String::new(), String::new()),
        };
        let folder = inputs.folder.clone();
        folder.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        let edit_url = matches!(
            self.dialog,
            Some(RefDialog::EditRemoteUrl { .. } | RefDialog::AddSubmodule)
        );
        let (name, url, message) = (
            inputs.name.clone(),
            inputs.url.clone(),
            inputs.message.clone(),
        );
        name.update(cx, |input, cx| {
            input.set_value(initial, window, cx);
            if !edit_url {
                input.focus(window, cx);
            }
        });
        url.update(cx, |input, cx| {
            input.set_value(initial_url, window, cx);
            if edit_url {
                input.focus(window, cx);
            }
        });
        message.update(cx, |input, cx| input.set_value(String::new(), window, cx));
        if matches!(self.dialog, Some(RefDialog::StashChanges)) {
            let stash = inputs.stash_message.clone();
            stash.update(cx, |input, cx| {
                input.set_value(String::new(), window, cx);
                input.focus(window, cx);
            });
        }
    }

    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.inputs.is_some() {
            return;
        }
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(3)
                .placeholder("Message (optional; makes an annotated tag)")
        });
        let url = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://github.com/owner/repository.git")
        });
        let stash_message =
            cx.new(|cx| InputState::new(window, cx).placeholder("Message (optional)"));
        let folder = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Folder (optional; defaults to the path below)")
        });
        let subscriptions = [&name, &url, &stash_message, &folder]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |this, _, event, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.confirm(window, cx),
                    InputEvent::Change => cx.notify(),
                    _ => {}
                })
            })
            .collect();
        self.inputs = Some(Inputs {
            name,
            url,
            message,
            stash_message,
            folder,
            _subscriptions: subscriptions,
        });
    }

    /// "Show in History" for a ref pointing at `oid`.
    fn history_item(oid: Oid, cx: &mut Context<Self>) -> MenuItem {
        let this = cx.entity().downgrade();
        MenuItem::new("refs.show-in-history", "Show in History")
            .icon(LucideIcon::GitGraph)
            .on_click(move |_, _, cx| {
                let _ = this.update(cx, |_, cx| cx.emit(RefsEvent::ShowInHistory(oid.clone())));
            })
    }

    fn local_menu(
        &self,
        branch: &Branch,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let name = branch.name.clone();
        let mut items = vec![Self::history_item(branch.oid.clone(), cx)];
        let elsewhere = self
            .worktree_of(&name, snapshot, cx)
            .map(|w| w.path.clone());
        if let Some(path) = elsewhere.clone() {
            items.push(
                MenuItem::new("refs.open-worktree", "Open Its Worktree")
                    .icon(LucideIcon::FolderGit2)
                    .on_click(with(&this, move |_, _, cx| {
                        cx.emit(RefsEvent::OpenRepository(path.clone()));
                    })),
            );
        } else if !branch.is_head {
            items.push(
                MenuItem::new("refs.checkout", format!("Switch to {name}"))
                    .icon(LucideIcon::Check)
                    .on_click(with(&this, {
                        let name = name.clone();
                        move |view, _, cx| view.checkout(Checkout::Branch(name.clone()), cx)
                    })),
            );
        }
        if elsewhere.is_none() && !branch.is_head {
            items.push(
                MenuItem::new("refs.new-worktree", "Check Out in New Worktree…")
                    .icon(LucideIcon::FolderGit2)
                    .on_click(show(
                        &this,
                        RefDialog::AddWorktree {
                            branch: Some(name.clone()),
                        },
                    )),
            );
        }
        items.push(
            MenuItem::new("refs.new-branch", format!("New Branch from {name}…"))
                .icon(LucideIcon::GitBranchPlus)
                .on_click(show(
                    &this,
                    RefDialog::CreateBranch {
                        start: Some(name.clone()),
                    },
                )),
        );
        items.push(
            MenuItem::new("refs.rename", "Rename…")
                .icon(LucideIcon::Pencil)
                .on_click(show(&this, RefDialog::RenameBranch { name: name.clone() })),
        );
        items.push(
            MenuItem::new("refs.delete", "Delete…")
                .icon(LucideIcon::Trash2)
                .disabled(branch.is_head || elsewhere.is_some())
                .on_click(show(
                    &this,
                    RefDialog::DeleteBranch {
                        name: name.clone(),
                        unmerged: false,
                    },
                )),
        );
        items.push(MenuItem::separator());

        // Remote branches with the same name first.
        let mut candidates: Vec<String> = snapshot
            .remote_branches
            .iter()
            .filter(|r| r.name != "HEAD")
            .map(|r| format!("{}/{}", r.remote, r.name))
            .collect();
        candidates.sort_by_key(|c| (!c.ends_with(&format!("/{name}")), c.clone()));
        candidates.truncate(30);
        if !candidates.is_empty() {
            let current = branch.upstream.as_ref().map(|u| u.name.clone());
            let upstreams: Vec<MenuItem> = candidates
                .into_iter()
                .enumerate()
                .map(|(i, upstream)| {
                    let selected = current.as_deref() == Some(upstream.as_str());
                    let label = if selected {
                        format!("{upstream} ✓")
                    } else {
                        upstream.clone()
                    };
                    MenuItem::new(SharedString::from(format!("refs.upstream.{i}")), label).on_click(
                        with(&this, {
                            let name = name.clone();
                            move |view, _, cx| {
                                let (name, upstream) = (name.clone(), upstream.clone());
                                view.run(
                                    format!("Track {upstream} from {name}"),
                                    move |repository| {
                                        repository.set_upstream(&name, Some(&upstream))
                                    },
                                    cx,
                                );
                            }
                        }),
                    )
                })
                .collect();
            items.push(
                MenuItem::submenu("refs.upstream", "Set Upstream", upstreams)
                    .icon(LucideIcon::Link),
            );
        }
        if branch.upstream.is_some() {
            items.push(
                MenuItem::new("refs.unset-upstream", "Stop Tracking Upstream")
                    .icon(LucideIcon::Unlink)
                    .on_click(with(&this, {
                        let name = name.clone();
                        move |view, _, cx| {
                            let name = name.clone();
                            view.run(
                                format!("Stop tracking upstream of {name}"),
                                move |repository| repository.set_upstream(&name, None),
                                cx,
                            );
                        }
                    })),
            );
        }
        items.push(
            MenuItem::new("refs.new-tag", "New Tag Here…")
                .icon(LucideIcon::Tag)
                .on_click(show(
                    &this,
                    RefDialog::CreateTag {
                        target: Some(name.clone()),
                    },
                )),
        );
        items.extend(self.integrate_items(&name, cx));
        items.push(MenuItem::separator());
        items.push(copy_item(name));
        items
    }

    fn remote_menu(&self, remote: &str, branch: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let full = format!("{remote}/{branch}");
        let target = self.remote_checkout(remote, branch, cx);
        let integrate = self.integrate_items(&full, cx);
        let oid = self.snapshot(cx).and_then(|s| {
            s.remote_branches
                .iter()
                .find(|r| r.remote == remote && r.name == branch)
                .map(|r| r.oid.clone())
        });
        let mut items: Vec<MenuItem> = oid
            .map(|oid| Self::history_item(oid, cx))
            .into_iter()
            .collect();
        items.extend([
            MenuItem::new(
                "refs.remote.checkout",
                format!("Switch to {}", target.label()),
            )
            .icon(LucideIcon::Check)
            .on_click(with(&this, move |view, _, cx| {
                view.checkout(target.clone(), cx);
            })),
            MenuItem::new("refs.remote.new-branch", format!("New Branch from {full}…"))
                .icon(LucideIcon::GitBranchPlus)
                .on_click(show(
                    &this,
                    RefDialog::CreateBranch {
                        start: Some(full.clone()),
                    },
                )),
            MenuItem::separator(),
            MenuItem::new(
                "refs.remote.delete",
                format!("Delete {branch} on {remote}…"),
            )
            .icon(LucideIcon::Trash2)
            .on_click(show(
                &this,
                RefDialog::DeleteRemoteBranch {
                    remote: remote.to_owned(),
                    branch: branch.to_owned(),
                },
            )),
        ]);
        items.extend(integrate);
        items.push(MenuItem::separator());
        items.push(copy_item(full));
        items
    }

    fn remote_header_menu(&self, remote: &str, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let name = remote.to_owned();
        let url = self
            .snapshot(cx)
            .and_then(|s| s.remotes.iter().find(|r| r.name == remote))
            .and_then(|r| r.fetch_url.clone())
            .unwrap_or_default();
        vec![
            MenuItem::new("refs.remote.fetch", format!("Fetch {name}"))
                .icon(LucideIcon::RefreshCw)
                .on_click(with(&this, {
                    let name = name.clone();
                    move |view, _, cx| {
                        let name = name.clone();
                        view.model.update(cx, |model, cx| {
                            model.run_network(
                                format!("Fetch {name}"),
                                move |repository, progress, cancel| {
                                    repository.fetch(
                                        Some(&name),
                                        true,
                                        Some(progress),
                                        Some(cancel),
                                    )
                                },
                                cx,
                            );
                        });
                    }
                })),
            MenuItem::separator(),
            MenuItem::new("refs.remote.edit-url", "Edit URL…")
                .icon(LucideIcon::Link)
                .on_click(show(
                    &this,
                    RefDialog::EditRemoteUrl {
                        name: name.clone(),
                        url: url.clone(),
                    },
                )),
            MenuItem::new("refs.remote.rename", "Rename…")
                .icon(LucideIcon::Pencil)
                .on_click(show(&this, RefDialog::RenameRemote { name: name.clone() })),
            MenuItem::new("refs.remote.remove", "Remove…")
                .icon(LucideIcon::Trash2)
                .on_click(show(&this, RefDialog::RemoveRemote { name: name.clone() })),
            MenuItem::separator(),
            MenuItem::new("refs.remote.copy-url", "Copy URL")
                .icon(LucideIcon::Copy)
                .disabled(url.is_empty())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                }),
        ]
    }

    /// Switching to a remote branch checks out a local branch of the same
    /// name when one exists, and otherwise creates a tracking branch.
    fn remote_checkout(&self, remote: &str, branch: &str, cx: &gpui_kit::App) -> Checkout {
        let exists = self
            .snapshot(cx)
            .is_some_and(|s| s.branches.iter().any(|b| b.name == branch));
        if exists {
            Checkout::Branch(branch.to_owned())
        } else {
            Checkout::Remote {
                remote: remote.to_owned(),
                branch: branch.to_owned(),
                local: branch.to_owned(),
            }
        }
    }

    fn tag_menu(&self, tag: &str, snapshot: &Snapshot, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let name = tag.to_owned();
        let target = snapshot
            .tags
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.target.clone());
        let mut items: Vec<MenuItem> = target
            .map(|oid| Self::history_item(oid, cx))
            .into_iter()
            .collect();
        items.extend([
            MenuItem::new("refs.tag.checkout", "Switch to Tag (Detached)")
                .icon(LucideIcon::Check)
                .on_click(with(&this, {
                    let name = name.clone();
                    move |view, _, cx| view.checkout(Checkout::Detached(name.clone()), cx)
                })),
            MenuItem::new("refs.tag.new-branch", format!("New Branch from {name}…"))
                .icon(LucideIcon::GitBranchPlus)
                .on_click(show(
                    &this,
                    RefDialog::CreateBranch {
                        start: Some(name.clone()),
                    },
                )),
        ]);
        for (i, remote) in snapshot.remotes.iter().enumerate() {
            let remote = remote.name.clone();
            items.push(
                MenuItem::new(
                    SharedString::from(format!("refs.tag.push.{i}")),
                    format!("Push to {remote}"),
                )
                .icon(LucideIcon::Upload)
                .on_click(with(&this, {
                    let name = name.clone();
                    move |view, _, cx| {
                        let (name, remote) = (name.clone(), remote.clone());
                        view.run(
                            format!("Push tag {name} to {remote}"),
                            move |repository| repository.push_tag(&remote, &name),
                            cx,
                        );
                    }
                })),
            );
        }
        items.push(MenuItem::separator());
        items.push(
            MenuItem::new("refs.tag.delete", "Delete…")
                .icon(LucideIcon::Trash2)
                .on_click(show(&this, RefDialog::DeleteTag { name: name.clone() })),
        );
        items.extend(self.integrate_items(&name, cx));
        items.push(MenuItem::separator());
        items.push(copy_item(name));
        items
    }

    fn set_menu(&self, items: Vec<MenuItem>, cx: &mut Context<Self>) {
        self.menu.update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn render_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let Some(open) = self.dialog.clone() else {
            return dialog(
                "refs.dialog",
                &self.handle,
                dialog_popup("refs.dialog.popup", "Branch", cx),
                window,
                cx,
            );
        };
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let current = self
            .snapshot(cx)
            .map(|s| match &s.head {
                Head::Branch { name, .. } => name.clone(),
                Head::Detached { oid } => oid.short().to_owned(),
            })
            .unwrap_or_default();
        let (title, description, confirm, destructive) = match &open {
            RefDialog::CreateBranch { start } => (
                "New Branch".to_owned(),
                format!("Starts at {}.", start.as_deref().unwrap_or(&current)),
                "Create Branch",
                false,
            ),
            RefDialog::RenameBranch { name } => (
                format!("Rename {name}"),
                "The upstream and settings move with the branch.".to_owned(),
                "Rename",
                false,
            ),
            RefDialog::DeleteBranch {
                name,
                unmerged: false,
            } => (
                format!("Delete {name}?"),
                "The branch's commits stay reachable from other branches that contain them."
                    .to_owned(),
                "Delete",
                true,
            ),
            RefDialog::DeleteBranch {
                name,
                unmerged: true,
            } => (
                format!("{name} is not merged"),
                "Its commits are not on its upstream or on HEAD. Deleting it anyway can lose them, though they stay in the reflog for a while."
                    .to_owned(),
                "Delete Anyway",
                true,
            ),
            RefDialog::CreateTag { target } => (
                "New Tag".to_owned(),
                format!("Tags {}.", target.as_deref().unwrap_or(&current)),
                "Create Tag",
                false,
            ),
            RefDialog::DeleteTag { name } => (
                format!("Delete tag {name}?"),
                "This deletes the local tag only. Remote copies stay.".to_owned(),
                "Delete",
                true,
            ),
            RefDialog::ResetHard { target } => (
                format!("Reset {current} to {target}?"),
                "Every uncommitted change is discarded and commits after it leave the branch. The commits stay in the reflog for a while."
                    .to_owned(),
                "Reset and Discard",
                true,
            ),
            RefDialog::AddRemote => (
                "Add Remote".to_owned(),
                "Any URL Git understands works: HTTPS, SSH, or a path to another repository."
                    .to_owned(),
                "Add Remote",
                false,
            ),
            RefDialog::EditRemoteUrl { name, .. } => (
                format!("Edit {name} URL"),
                "Fetches and pushes use the new URL.".to_owned(),
                "Save",
                false,
            ),
            RefDialog::RenameRemote { name } => (
                format!("Rename {name}"),
                "Remote branches and upstreams follow the new name.".to_owned(),
                "Rename",
                false,
            ),
            RefDialog::RemoveRemote { name } => (
                format!("Remove {name}?"),
                "Its remote branches disappear from Bonsai and branches stop tracking them. Nothing on the server changes."
                    .to_owned(),
                "Remove",
                true,
            ),
            RefDialog::DeleteRemoteBranch { remote, branch } => (
                format!("Delete {branch} on {remote}?"),
                "This deletes the branch on the server for everyone. Local branches stay.".to_owned(),
                "Delete on Remote",
                true,
            ),
            RefDialog::CheckoutBlocked { target, blocker } => {
                let n = blocker.paths().len();
                let what = match blocker {
                    CheckoutBlocker::LocalChanges(_) => "changes to",
                    CheckoutBlocker::UntrackedFiles(_) => "untracked",
                };
                (
                    format!("Switch to {}?", target.label()),
                    format!(
                        "Switching would overwrite your {what} {n} file{}. Stash your changes first? You can apply the stash afterwards.",
                        if n == 1 { "" } else { "s" }
                    ),
                    "Stash and Switch",
                    false,
                )
            }
            RefDialog::StashChanges => (
                "Stash Changes".to_owned(),
                "Puts your uncommitted changes aside and cleans the working tree. Apply the stash to bring them back."
                    .to_owned(),
                "Stash",
                false,
            ),
            RefDialog::DropStash { message, .. } => (
                "Drop this stash?".to_owned(),
                format!("\u{201c}{message}\u{201d} is deleted. Its changes can't be applied afterwards."),
                "Drop",
                true,
            ),
            RefDialog::AddWorktree { .. } => (
                "New Worktree".to_owned(),
                "Checks a branch out in its own folder, so you can work on it without switching away from this one. It opens in a new tab."
                    .to_owned(),
                "Create Worktree",
                false,
            ),
            RefDialog::RemoveWorktree { path, dirty: false } => (
                format!("Remove {}?", path.file_name().unwrap_or_default().to_string_lossy()),
                format!(
                    "{} is deleted from disk. Its branch and commits stay in this repository.",
                    path.display()
                ),
                "Remove",
                true,
            ),
            RefDialog::AddSubmodule => (
                "Add Submodule".to_owned(),
                "Clones another repository into a folder of this one, pinned at its current commit. Commit the change to share it."
                    .to_owned(),
                "Add Submodule",
                false,
            ),
            RefDialog::RemoveSubmodule { path } => (
                format!("Remove {}?", path.display()),
                "Its folder and its entry in .gitmodules are removed, staged for you to commit. The other repository is untouched."
                    .to_owned(),
                "Remove",
                true,
            ),
            RefDialog::RemoveWorktree { path, dirty: true } => (
                format!("{} has changes", path.file_name().unwrap_or_default().to_string_lossy()),
                "It has uncommitted changes or untracked files. Removing it anyway deletes them for good."
                    .to_owned(),
                "Remove Anyway",
                true,
            ),
        };
        let with_name = matches!(
            open,
            RefDialog::CreateBranch { .. }
                | RefDialog::RenameBranch { .. }
                | RefDialog::CreateTag { .. }
                | RefDialog::AddRemote
                | RefDialog::RenameRemote { .. }
                | RefDialog::AddWorktree { .. }
        );
        let with_url = matches!(
            open,
            RefDialog::AddRemote | RefDialog::EditRemoteUrl { .. } | RefDialog::AddSubmodule
        );
        let name_value = self.name_value(cx);
        let invalid = with_name && !name_value.is_empty() && !is_valid_ref_name(&name_value);
        let can_confirm = self.can_confirm(cx);
        let inputs = self
            .inputs
            .as_ref()
            .map(|i| (i.name.clone(), i.message.clone()));
        let url_input = self
            .inputs
            .as_ref()
            .map(|i| i.url.clone())
            .filter(|_| with_url);
        let stash_input = self
            .inputs
            .as_ref()
            .map(|i| i.stash_message.clone())
            .filter(|_| matches!(open, RefDialog::StashChanges));
        let submodule = matches!(open, RefDialog::AddSubmodule).then(|| {
            (
                self.inputs.as_ref().map(|i| i.folder.clone()),
                Some(self.submodule_path(cx).display().to_string())
                    .filter(|p| !p.is_empty())
                    .unwrap_or_else(|| "Goes in a folder named after the repository".to_owned()),
                self.submodule_problem(cx).filter(|p| !p.is_empty()),
            )
        });
        let worktree = matches!(open, RefDialog::AddWorktree { .. }).then(|| {
            let snapshot = self.snapshot(cx);
            let hint = if name_value.is_empty() {
                "Type a branch to check out, or a new name to create a branch from the current commit."
                    .to_owned()
            } else if snapshot.is_some_and(|s| s.branches.iter().any(|b| b.name == name_value)) {
                format!("Checks out the existing branch {name_value}.")
            } else {
                format!("Creates the branch {name_value} at {current}.")
            };
            (
                self.inputs.as_ref().map(|i| i.folder.clone()),
                hint,
                self.worktree_folder(cx).display().to_string(),
                self.worktree_problem(cx).filter(|p| !p.is_empty()),
            )
        });
        let paths: Vec<String> = match &open {
            RefDialog::CheckoutBlocked { blocker, .. } => blocker
                .paths()
                .iter()
                .take(8)
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            _ => Vec::new(),
        };
        let popup = dialog_popup("refs.dialog.popup", title.clone(), cx)
            .child(dialog_title("refs.dialog.title", cx).child(title))
            .child(dialog_description("refs.dialog.description", cx).child(description))
            .when_some(inputs.filter(|_| with_name), |this, (name, message)| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(Input::new(&name).aria_label("Name"))
                        .when(invalid, |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(c.destructive)
                                    .child("Names can't contain spaces, `..`, or ~ ^ : ? * [ \\."),
                            )
                        })
                        .when(matches!(open, RefDialog::CreateTag { .. }), |this| {
                            this.child(Textarea::new(&message).aria_label("Tag message"))
                        })
                        .when(matches!(open, RefDialog::CreateBranch { .. }), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .text_sm()
                                    .child(
                                        Checkbox::new("refs.dialog.checkout")
                                            .checked(self.checkout_new)
                                            .aria_label("Switch to the new branch")
                                            .on_change(cx.listener(
                                                |this, checked: &bool, _, cx| {
                                                    this.checkout_new = *checked;
                                                    cx.notify();
                                                },
                                            )),
                                    )
                                    .child("Switch to it"),
                            )
                        }),
                )
            })
            .when_some(stash_input, |this, stash| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(Input::new(&stash).aria_label("Stash message"))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .text_sm()
                                .child(
                                    Checkbox::new("refs.dialog.untracked")
                                        .checked(self.stash_untracked)
                                        .aria_label("Include untracked files")
                                        .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                            this.stash_untracked = *checked;
                                            cx.notify();
                                        })),
                                )
                                .child("Include untracked files"),
                        ),
                )
            })
            .when_some(url_input, |this, url| {
                this.child(Input::new(&url).aria_label("Remote URL"))
            })
            .when_some(submodule, |this, (folder, path, problem)| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .when_some(folder, |this, folder| {
                            this.child(Input::new(&folder).aria_label("Folder"))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .min_w_0()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(
                                    lucide(LucideIcon::Package)
                                        .size(px(14.))
                                        .flex_none()
                                        .text_color(c.muted_foreground),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme.fonts.mono.clone())
                                        .child(path),
                                ),
                        )
                        .when_some(problem, |this, problem| {
                            this.child(div().text_xs().text_color(c.destructive).child(problem))
                        }),
                )
            })
            .when_some(worktree, |this, (folder, hint, path, problem)| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(div().text_xs().text_color(c.muted_foreground).child(hint))
                        .when_some(folder, |this, folder| {
                            this.child(Input::new(&folder).aria_label("Folder"))
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .min_w_0()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(
                                    lucide(LucideIcon::FolderGit2)
                                        .size(px(14.))
                                        .flex_none()
                                        .text_color(c.muted_foreground),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme.fonts.mono.clone())
                                        .child(path),
                                ),
                        )
                        .when_some(problem, |this, problem| {
                            this.child(div().text_xs().text_color(c.destructive).child(problem))
                        }),
                )
            })
            .when(!paths.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_col()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .children(paths),
                )
            })
            .child(
                Confirm::new(
                    "refs.dialog",
                    confirm,
                    |this: &mut Self, window, cx| {
                        this.dialog = None;
                        this.handle.close(window, cx);
                    },
                    Self::confirm,
                )
                .destructive(destructive)
                .enabled(can_confirm)
                .footer(cx),
            );
        let this = cx.entity().downgrade();
        dialog("refs.dialog", &self.handle, popup, window, cx).on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |view, cx| {
                view.dialog = None;
                cx.notify();
            });
            true
        })
    }
}

impl RefsPanel {
    fn render_worktrees(
        &self,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let this = cx.entity().downgrade();
        let root = canonical(&self.root(cx));
        let mut list = div().flex().flex_col().child(
            div()
                .mt(px(12.))
                .flex()
                .items_center()
                .justify_between()
                .child(sidebar_group_label("WORKTREES", cx))
                .child(
                    Button::new("refs.add-worktree")
                        .variant(ButtonVariant::Ghost)
                        .aria_label("New Worktree")
                        .child(
                            lucide(LucideIcon::Plus)
                                .size(px(12.))
                                .text_color(c.sidebar_foreground),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show(RefDialog::AddWorktree { branch: None }, cx);
                        })),
                ),
        );
        let main = snapshot
            .worktrees
            .iter()
            .find(|w| w.main)
            .map(|w| w.path.clone());
        for worktree in snapshot.worktrees.iter().filter(|w| !w.bare) {
            let current = canonical(&worktree.path) == root;
            let label = match (&worktree.branch, &worktree.head) {
                (Some(branch), _) => branch.clone(),
                (None, Some(head)) => format!("detached at {}", head.short()),
                (None, None) => "detached".to_owned(),
            };
            // The folder, unless it's the main one or named the usual way.
            let folder = worktree
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let usual = main.as_deref().is_some_and(|main| {
                worktree.main
                    || default_worktree_folder(main, worktree.branch.as_deref().unwrap_or(""))
                        == worktree.path
            });
            let (icon, trailing) = if worktree.prunable {
                (LucideIcon::TriangleAlert, Some("missing".to_owned()))
            } else if worktree.locked.is_some() {
                (LucideIcon::Lock, (!usual).then_some(folder))
            } else {
                (LucideIcon::FolderGit2, (!usual).then_some(folder))
            };
            let mut item = SidebarItem::new(
                SharedString::from(format!("worktree:{}", worktree.path.display())),
                label,
            )
            .icon(lucide(icon).size(px(16.)).text_color(if worktree.prunable {
                c.warning
            } else {
                c.sidebar_foreground
            }))
            .selected(current);
            if let Some(trailing) = trailing {
                item = item.trailing(
                    div()
                        .max_w(px(96.))
                        .truncate()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(trailing),
                );
            }
            if !current && !worktree.prunable {
                let path = worktree.path.clone();
                item = item.on_activate(with(&this, move |_, _, cx| {
                    cx.emit(RefsEvent::OpenRepository(path.clone()));
                }));
            }
            let menu_worktree = worktree.clone();
            list = list.child(div().child(item).on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    let items = this.worktree_menu(&menu_worktree, current, cx);
                    this.set_menu(items, cx);
                }),
            ));
        }
        list
    }
}

impl RefsPanel {
    fn render_submodules(
        &self,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let this = cx.entity().downgrade();
        let root = self.root(cx);
        let stale = snapshot
            .submodules
            .iter()
            .any(|s| s.state != SubmoduleState::UpToDate);
        let header_button = |id: &'static str, label: &'static str, icon: LucideIcon| {
            Button::new(id)
                .variant(ButtonVariant::Ghost)
                .aria_label(label)
                .child(lucide(icon).size(px(12.)).text_color(c.sidebar_foreground))
        };
        let mut list = div().flex().flex_col().child(
            div()
                .mt(px(12.))
                .flex()
                .items_center()
                .justify_between()
                .child(sidebar_group_label("SUBMODULES", cx))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .when(stale, |this| {
                            this.child(
                                header_button(
                                    "refs.update-submodules",
                                    "Update All Submodules",
                                    LucideIcon::RefreshCw,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.run_submodules(
                                            "Update submodules".to_owned(),
                                            |repository| repository.update_submodules(&[]),
                                            cx,
                                        );
                                    },
                                )),
                            )
                        })
                        .child(
                            header_button("refs.add-submodule", "Add Submodule", LucideIcon::Plus)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show(RefDialog::AddSubmodule, cx);
                                })),
                        ),
                ),
        );
        for submodule in &snapshot.submodules {
            let (icon, color, trailing) = match submodule.state {
                SubmoduleState::NotInitialized => (
                    LucideIcon::PackageOpen,
                    c.muted_foreground,
                    "not cloned".to_owned(),
                ),
                SubmoduleState::UpToDate => (
                    LucideIcon::Package,
                    c.sidebar_foreground,
                    submodule
                        .describe
                        .clone()
                        .filter(|d| !d.starts_with("heads/"))
                        .unwrap_or_else(|| submodule.oid.short().to_string()),
                ),
                SubmoduleState::OtherCommit => (
                    LucideIcon::Package,
                    c.warning,
                    format!("at {}", submodule.oid.short()),
                ),
                SubmoduleState::Conflicted => (
                    LucideIcon::TriangleAlert,
                    c.destructive,
                    "conflicted".to_owned(),
                ),
            };
            let mut item = SidebarItem::new(
                SharedString::from(format!("submodule:{}", submodule.path.display())),
                submodule.path.display().to_string(),
            )
            .icon(lucide(icon).size(px(16.)).text_color(color))
            .trailing(
                div()
                    .max_w(px(96.))
                    .truncate()
                    .text_xs()
                    .text_color(if submodule.state == SubmoduleState::OtherCommit {
                        c.warning
                    } else {
                        c.muted_foreground
                    })
                    .child(trailing),
            );
            if submodule.state == SubmoduleState::NotInitialized {
                let path = submodule.path.clone();
                item = item.on_activate(with(&this, move |view, _, cx| {
                    let path = path.clone();
                    view.run_submodules(
                        format!("Clone submodule {}", path.display()),
                        move |repository| repository.update_submodules(&[path]),
                        cx,
                    );
                }));
            } else {
                let absolute = root.join(&submodule.path);
                item = item.on_activate(with(&this, move |_, _, cx| {
                    cx.emit(RefsEvent::OpenRepository(absolute.clone()));
                }));
            }
            let menu_submodule = submodule.clone();
            list = list.child(div().child(item).on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    let items = this.submodule_menu(&menu_submodule, cx);
                    this.set_menu(items, cx);
                }),
            ));
        }
        list
    }
}

/// `vendor-lib` for `https://example.com/team/vendor-lib.git`.
fn submodule_folder(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or_default()
        .trim_end_matches(".git")
        .to_owned()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// `~/x` as a path in the home folder.
fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// `../orchard-feature-login` for branch `feature/login` of `.../orchard`.
fn default_worktree_folder(main: &Path, branch: &str) -> PathBuf {
    let name = main.file_name().unwrap_or_default().to_string_lossy();
    let branch = branch.replace(['/', '\\'], "-");
    let folder = if branch.is_empty() {
        format!("{name}-worktree")
    } else {
        format!("{name}-{branch}")
    };
    main.parent().unwrap_or(main).join(folder)
}

/// A menu click handler that updates the panel.
fn with(
    this: &WeakEntity<RefsPanel>,
    f: impl Fn(&mut RefsPanel, &mut Window, &mut Context<RefsPanel>) + 'static,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    let this = this.clone();
    move |_, window, cx| {
        let _ = this.update(cx, |view, cx| f(view, window, cx));
    }
}

fn show(
    this: &WeakEntity<RefsPanel>,
    dialog: RefDialog,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static {
    with(this, move |view, _, cx| view.show(dialog.clone(), cx))
}

fn copy_item(name: String) -> MenuItem {
    MenuItem::new("refs.copy", "Copy Name")
        .icon(LucideIcon::Copy)
        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(name.clone())))
}

impl Render for RefsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        if self.open_requested {
            self.open_requested = false;
            self.prepare_inputs(window, cx);
            self.handle.open(window, cx);
        }
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let icon = |name: LucideIcon| lucide(name).size(px(16.)).text_color(c.sidebar_foreground);
        let snapshot = self.snapshot(cx).cloned();
        let mut list = div().flex().flex_col();
        if let Some(snapshot) = &snapshot {
            let this = cx.entity().downgrade();
            list = list.child(
                div()
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(sidebar_group_label("BRANCHES", cx))
                    .child(
                        Button::new("refs.add-branch")
                            .variant(ButtonVariant::Ghost)
                            .aria_label("New Branch")
                            .child(
                                lucide(LucideIcon::Plus)
                                    .size(px(12.))
                                    .text_color(c.sidebar_foreground),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show(RefDialog::CreateBranch { start: None }, cx);
                            })),
                    ),
            );
            for branch in &snapshot.branches {
                let mut item = SidebarItem::new(
                    SharedString::from(format!("branch:{}", branch.name)),
                    branch.name.clone(),
                )
                .icon(icon(LucideIcon::GitBranch))
                .selected(branch.is_head);
                if let Some(upstream) = &branch.upstream {
                    let badge = if upstream.gone {
                        Some("gone".to_owned())
                    } else if upstream.ahead > 0 || upstream.behind > 0 {
                        Some(
                            [
                                (upstream.ahead > 0).then(|| format!("↑{}", upstream.ahead)),
                                (upstream.behind > 0).then(|| format!("↓{}", upstream.behind)),
                            ]
                            .into_iter()
                            .flatten()
                            .collect::<Vec<_>>()
                            .join(" "),
                        )
                    } else {
                        None
                    };
                    if let Some(badge) = badge {
                        item = item.trailing(
                            div()
                                .text_xs()
                                .text_color(c.sidebar_foreground)
                                .child(badge),
                        );
                    }
                }
                if let Some(worktree) = self.worktree_of(&branch.name, snapshot, cx) {
                    let path = worktree.path.clone();
                    if branch.upstream.is_none() {
                        item = item.trailing(
                            lucide(LucideIcon::FolderGit2)
                                .size(px(12.))
                                .text_color(c.muted_foreground),
                        );
                    }
                    item = item.on_activate(with(&this, move |_, _, cx| {
                        cx.emit(RefsEvent::OpenRepository(path.clone()));
                    }));
                } else if !branch.is_head {
                    let target = Checkout::Branch(branch.name.clone());
                    item = item.on_activate(with(&this, move |view, _, cx| {
                        view.checkout(target.clone(), cx);
                    }));
                }
                let branch = branch.clone();
                let snap = snapshot.clone();
                list = list.child(div().child(item).on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, _, _, cx| {
                        let items = this.local_menu(&branch, &snap, cx);
                        this.set_menu(items, cx);
                    }),
                ));
            }

            list = list.child(self.render_worktrees(snapshot, cx));
            if !snapshot.submodules.is_empty() {
                list = list.child(self.render_submodules(snapshot, cx));
            }

            let mut remotes: Vec<&str> = snapshot.remotes.iter().map(|r| r.name.as_str()).collect();
            for branch in &snapshot.remote_branches {
                if !remotes.contains(&branch.remote.as_str()) {
                    remotes.push(&branch.remote);
                }
            }
            list = list.child(
                div()
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(sidebar_group_label("REMOTES", cx))
                    .child(
                        Button::new("refs.add-remote")
                            .variant(ButtonVariant::Ghost)
                            .aria_label("Add Remote")
                            .child(
                                lucide(LucideIcon::Plus)
                                    .size(px(12.))
                                    .text_color(c.sidebar_foreground),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show(RefDialog::AddRemote, cx);
                            })),
                    ),
            );
            for remote in remotes {
                let collapsed = self.collapsed_remotes.contains(remote);
                let key = remote.to_owned();
                let menu_remote = remote.to_owned();
                list = list.child(
                    div()
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                let items = this.remote_header_menu(&menu_remote, cx);
                                this.set_menu(items, cx);
                            }),
                        )
                        .child(
                            SidebarItem::new(
                                SharedString::from(format!("remote:{remote}")),
                                remote.to_owned(),
                            )
                            .icon(icon(if collapsed {
                                LucideIcon::ChevronRight
                            } else {
                                LucideIcon::ChevronDown
                            }))
                            .on_activate(with(
                                &this,
                                move |view, _, cx| {
                                    if !view.collapsed_remotes.remove(&key) {
                                        view.collapsed_remotes.insert(key.clone());
                                    }
                                    cx.notify();
                                },
                            )),
                        ),
                );
                if collapsed {
                    continue;
                }
                for branch in snapshot
                    .remote_branches
                    .iter()
                    .filter(|b| b.remote == remote && b.name != "HEAD")
                {
                    let target = self.remote_checkout(remote, &branch.name, cx);
                    let (r, b) = (remote.to_owned(), branch.name.clone());
                    list = list.child(
                        div()
                            .pl(px(16.))
                            .child(
                                SidebarItem::new(
                                    SharedString::from(format!(
                                        "remote-branch:{remote}/{}",
                                        branch.name
                                    )),
                                    branch.name.clone(),
                                )
                                .icon(icon(LucideIcon::Cloud))
                                .on_activate(with(
                                    &this,
                                    move |view, _, cx| {
                                        view.checkout(target.clone(), cx);
                                    },
                                )),
                            )
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, _, _, cx| {
                                    let items = this.remote_menu(&r, &b, cx);
                                    this.set_menu(items, cx);
                                }),
                            ),
                    );
                }
            }

            if !snapshot.tags.is_empty() {
                list = list
                    .child(div().h(px(12.)))
                    .child(sidebar_group_label("TAGS", cx));
            }
            for tag in &snapshot.tags {
                let name = tag.name.clone();
                let snap = snapshot.clone();
                list = list.child(
                    div()
                        .child(
                            SidebarItem::new(
                                SharedString::from(format!("tag:{}", tag.name)),
                                tag.name.clone(),
                            )
                            .icon(icon(LucideIcon::Tag)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                let items = this.tag_menu(&name, &snap, cx);
                                this.set_menu(items, cx);
                            }),
                        ),
                );
            }

            list = list.child(
                div()
                    .mt(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(sidebar_group_label("STASHES", cx))
                    .child(
                        Button::new("refs.add-stash")
                            .variant(ButtonVariant::Ghost)
                            .aria_label("Stash Changes")
                            .child(
                                lucide(LucideIcon::Plus)
                                    .size(px(12.))
                                    .text_color(c.sidebar_foreground),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show(RefDialog::StashChanges, cx);
                            })),
                    ),
            );
            for stash in &snapshot.stashes {
                let (menu_stash, view_stash) = (stash.clone(), stash.clone());
                list = list.child(
                    div()
                        .child(
                            SidebarItem::new(
                                SharedString::from(format!("stash:{}", stash.oid)),
                                stash.message.clone(),
                            )
                            .icon(icon(LucideIcon::Archive))
                            .on_activate(with(
                                &this,
                                move |panel, _, cx| {
                                    panel.view_stash(&view_stash, cx);
                                },
                            )),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                let items = this.stash_menu(&menu_stash, cx);
                                this.set_menu(items, cx);
                            }),
                        ),
                );
            }
        }
        div()
            .child(context_menu(&self.menu, "Branch actions", list))
            .child(self.render_dialog(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let g = Git::locate(None).unwrap();
        g.run(&g.command(args.iter().copied()).cwd(dir)).unwrap();
    }

    fn head(model: &Entity<RepositoryModel>, cx: &mut gpui_kit::VisualTestContext) -> String {
        model.read_with(cx, |model, _| match &model.snapshot().unwrap().head {
            Head::Branch { name, .. } => name.clone(),
            Head::Detached { .. } => "detached".to_owned(),
        })
    }

    #[gpui_kit::test]
    fn follow_up_dialogs_stash_before_switching_and_force_deletes(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
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
        std::fs::write(root.join("a.txt"), "1").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);
        git(root, &["switch", "-qc", "other"]);
        std::fs::write(root.join("a.txt"), "2").unwrap();
        git(root, &["commit", "-qam", "other"]);
        git(root, &["switch", "-q", "main"]);
        std::fs::write(root.join("a.txt"), "local").unwrap();

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (panel, cx) = cx.add_window_view(|_, cx| RefsPanel::new(model.clone(), cx));
        cx.run_until_parked();

        panel.update(cx, |panel, cx| {
            panel.checkout(Checkout::Branch("other".into()), cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel.dialog(),
                Some(&RefDialog::CheckoutBlocked {
                    target: Checkout::Branch("other".into()),
                    blocker: CheckoutBlocker::LocalChanges(vec!["a.txt".into()]),
                })
            );
        });
        assert_eq!(head(&model, cx), "main");
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert_eq!(head(&model, cx), "other");
        model.read_with(cx, |model, _| {
            assert_eq!(model.snapshot().unwrap().stashes.len(), 1);
        });

        // `work` has a commit no other branch has.
        git(root, &["switch", "-qc", "work"]);
        std::fs::write(root.join("b.txt"), "b").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "work"]);
        git(root, &["switch", "-q", "main"]);
        model.update(cx, super::super::repository::RepositoryModel::refresh);
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            panel.delete_branch("work".into(), false, cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel.dialog(),
                Some(&RefDialog::DeleteBranch {
                    name: "work".into(),
                    unmerged: true,
                })
            );
        });
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            let names: Vec<&str> = model
                .snapshot()
                .unwrap()
                .branches
                .iter()
                .map(|b| b.name.as_str())
                .collect();
            assert_eq!(names, ["main", "other"]);
        });

        // New branch through the dialog, validated as typed.
        panel.update(cx, |panel, cx| {
            panel.show(RefDialog::CreateBranch { start: None }, cx);
        });
        cx.run_until_parked();
        let name = panel.read_with(cx, |panel, _| panel.inputs.as_ref().unwrap().name.clone());
        name.update_in(cx, |input, window, cx| {
            input.set_value("bad name", window, cx);
        });
        panel.read_with(cx, |panel, cx| assert!(!panel.can_confirm(cx)));
        name.update_in(cx, |input, window, cx| input.set_value("fresh", window, cx));
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert_eq!(head(&model, cx), "fresh");
    }

    #[gpui_kit::test]
    fn stashes_save_view_pop_and_drop(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
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
        std::fs::write(root.join("a.txt"), "1").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "base"]);
        std::fs::write(root.join("a.txt"), "2").unwrap();
        std::fs::write(root.join("new.txt"), "new").unwrap();

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (panel, cx) = cx.add_window_view(|_, cx| RefsPanel::new(model.clone(), cx));
        cx.run_until_parked();
        let stashes = |cx: &mut gpui_kit::VisualTestContext| {
            model.read_with(cx, |model, _| model.snapshot().unwrap().stashes.clone())
        };

        panel.update(cx, |panel, cx| panel.show(RefDialog::StashChanges, cx));
        cx.run_until_parked();
        let input = panel.read_with(cx, |panel, _| {
            panel.inputs.as_ref().unwrap().stash_message.clone()
        });
        input.update_in(cx, |input, window, cx| {
            input.set_value("halfway", window, cx);
        });
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        let saved = stashes(cx);
        assert_eq!(saved.len(), 1);
        assert!(saved[0].message.contains("halfway"));
        // Untracked files go with it by default.
        assert!(!root.join("new.txt").exists());

        let shown = Arc::new(Mutex::new(None));
        let _events = cx.update(|_, cx| {
            let shown = shown.clone();
            cx.subscribe(&panel, move |_, event: &RefsEvent, _| {
                *shown.lock().unwrap() = Some(event.clone());
            })
        });
        panel.update(cx, |panel, cx| panel.view_stash(&saved[0], cx));
        assert!(matches!(
            shown.lock().unwrap().as_ref(),
            Some(RefsEvent::ShowDiff(DiffTarget::Range { .. }, _))
        ));

        panel.update(cx, |panel, cx| panel.apply_stash(&saved[0], true, cx));
        cx.run_until_parked();
        assert_eq!(stashes(cx), [] as [bonsai_git::Stash; 0]);
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "2");
        assert!(root.join("new.txt").exists());

        // Dropping asks first and then deletes the stash.
        git(root, &["stash", "push", "-qm", "again"]);
        model.update(cx, RepositoryModel::refresh);
        cx.run_until_parked();
        let again = stashes(cx)[0].clone();
        panel.update(cx, |panel, cx| {
            let items = panel.stash_menu(&again, cx);
            assert_eq!(items.len(), 5);
            panel.show(
                RefDialog::DropStash {
                    index: again.index,
                    oid: again.oid.clone(),
                    message: again.message.clone(),
                },
                cx,
            );
        });
        cx.run_until_parked();
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert_eq!(stashes(cx), [] as [bonsai_git::Stash; 0]);
    }

    #[gpui_kit::test]
    fn adds_opens_and_removes_worktrees(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        // Git reports real paths (macOS temp folders are behind a symlink).
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("orchard");
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
        std::fs::write(root.join("a.txt"), "1").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);
        git(&root, &["branch", "feature/login"]);

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (panel, cx) = cx.add_window_view(|_, cx| RefsPanel::new(model.clone(), cx));
        cx.run_until_parked();
        let opened = Arc::new(Mutex::new(Vec::new()));
        let _events = cx.update(|_, cx| {
            let opened = opened.clone();
            cx.subscribe(&panel, move |_, event: &RefsEvent, _| {
                if let RefsEvent::OpenRepository(path) = event {
                    opened.lock().unwrap().push(path.clone());
                }
            })
        });

        // An existing branch, into the default sibling folder.
        panel.update(cx, |panel, cx| {
            panel.show(
                RefDialog::AddWorktree {
                    branch: Some("feature/login".into()),
                },
                cx,
            );
        });
        cx.run_until_parked();
        let expected = base.join("orchard-feature-login");
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.worktree_folder(cx), expected);
            assert!(panel.can_confirm(cx));
        });
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert_eq!(
            opened.lock().unwrap().as_slice(),
            std::slice::from_ref(&expected)
        );
        assert_eq!(
            std::fs::read_to_string(expected.join("a.txt")).unwrap(),
            "1"
        );

        // `main` is checked out here and `feature/login` there now.
        panel.update(cx, |panel, cx| {
            panel.show(RefDialog::AddWorktree { branch: None }, cx);
        });
        cx.run_until_parked();
        let name = panel.read_with(cx, |panel, _| panel.inputs.as_ref().unwrap().name.clone());
        for taken in ["main", "feature/login"] {
            name.update_in(cx, |input, window, cx| input.set_value(taken, window, cx));
            panel.read_with(cx, |panel, cx| {
                assert!(!panel.can_confirm(cx));
                assert!(panel.worktree_problem(cx).unwrap().contains("checked out"));
            });
        }
        // A new name makes a branch, in the folder typed.
        name.update_in(cx, |input, window, cx| input.set_value("spike", window, cx));
        let folder = panel.read_with(cx, |panel, _| panel.inputs.as_ref().unwrap().folder.clone());
        let typed = base.join("elsewhere");
        folder.update_in(cx, |input, window, cx| {
            input.set_value(typed.to_string_lossy().into_owned(), window, cx);
        });
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert_eq!(opened.lock().unwrap().last(), Some(&typed));
        model.read_with(cx, |model, _| {
            let snapshot = model.snapshot().unwrap();
            let branches: Vec<_> = snapshot
                .worktrees
                .iter()
                .map(|w| w.branch.clone())
                .collect();
            assert_eq!(snapshot.worktrees.len(), 3, "{branches:?}");
            assert!(snapshot.branches.iter().any(|b| b.name == "spike"));
        });

        // A branch checked out elsewhere opens its worktree instead of switching.
        panel.read_with(cx, |panel, cx| {
            let snapshot = panel.snapshot(cx).unwrap();
            let elsewhere = panel.worktree_of("feature/login", snapshot, cx).unwrap();
            assert_eq!(canonical(&elsewhere.path), canonical(&expected));
            assert!(panel.worktree_of("main", snapshot, cx).is_none());
        });

        // Removing one with changes asks before forcing.
        std::fs::write(expected.join("a.txt"), "edited").unwrap();
        panel.update(cx, |panel, cx| {
            panel.remove_worktree(expected.clone(), false, cx);
        });
        cx.run_until_parked();
        panel.read_with(cx, |panel, _| {
            assert_eq!(
                panel.dialog(),
                Some(&RefDialog::RemoveWorktree {
                    path: expected.clone(),
                    dirty: true,
                })
            );
        });
        assert!(expected.exists());
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert!(!expected.exists());
        model.read_with(cx, |model, _| {
            assert_eq!(model.snapshot().unwrap().worktrees.len(), 2);
        });
    }

    #[test]
    fn names_a_submodule_folder_after_its_repository() {
        assert_eq!(submodule_folder("https://example.com/team/lib.git"), "lib");
        assert_eq!(
            submodule_folder("git@example.com:team/vendor-lib.git"),
            "vendor-lib"
        );
        assert_eq!(submodule_folder("../shared/"), "shared");
    }

    #[gpui_kit::test]
    fn adds_and_removes_submodules(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        // Submodules from a local folder need this since Git 2.38.
        let g = Git::locate(None)
            .unwrap()
            .with_env("GIT_CONFIG_COUNT", "1")
            .with_env("GIT_CONFIG_KEY_0", "protocol.file.allow")
            .with_env("GIT_CONFIG_VALUE_0", "always");
        let mut repositories = Vec::new();
        for name in ["library", "app"] {
            let root = base.join(name);
            std::fs::create_dir(&root).unwrap();
            repositories.push(Repository::init(&g, &root, Some("main")).unwrap());
            for (key, value) in [
                ("user.name", "Ada Lovelace"),
                ("user.email", "ada@example.com"),
                ("commit.gpgsign", "false"),
            ] {
                git(&root, &["config", key, value]);
            }
            std::fs::write(root.join("a.txt"), name).unwrap();
            git(&root, &["add", "."]);
            git(&root, &["commit", "-qm", "base"]);
        }
        let app = base.join("app");
        let repository = repositories.pop().unwrap();
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (panel, cx) = cx.add_window_view(|_, cx| RefsPanel::new(model.clone(), cx));
        cx.run_until_parked();

        panel.update(cx, |panel, cx| panel.show(RefDialog::AddSubmodule, cx));
        cx.run_until_parked();
        let url = panel.read_with(cx, |panel, _| panel.inputs.as_ref().unwrap().url.clone());
        panel.read_with(cx, |panel, cx| assert!(!panel.can_confirm(cx)));
        url.update_in(cx, |input, window, cx| {
            input.set_value(
                base.join("library").to_string_lossy().into_owned(),
                window,
                cx,
            );
        });
        panel.read_with(cx, |panel, cx| {
            assert_eq!(panel.submodule_path(cx), PathBuf::from("library"));
            assert!(panel.can_confirm(cx));
        });
        let folder = panel.read_with(cx, |panel, _| panel.inputs.as_ref().unwrap().folder.clone());
        folder.update_in(cx, |input, window, cx| {
            input.set_value("vendor/lib", window, cx);
        });
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        model.update(cx, RepositoryModel::refresh);
        cx.run_until_parked();
        model.read_with(cx, |model, _| {
            let submodules = &model.snapshot().unwrap().submodules;
            assert_eq!(submodules.len(), 1);
            assert_eq!(submodules[0].path, PathBuf::from("vendor/lib"));
            assert_eq!(submodules[0].state, SubmoduleState::UpToDate);
        });
        assert!(app.join("vendor/lib/a.txt").exists());

        panel.update(cx, |panel, cx| {
            panel.show(
                RefDialog::RemoveSubmodule {
                    path: "vendor/lib".into(),
                },
                cx,
            );
        });
        cx.run_until_parked();
        panel.update_in(cx, RefsPanel::confirm);
        cx.run_until_parked();
        assert!(!app.join("vendor/lib").exists());
    }
}
