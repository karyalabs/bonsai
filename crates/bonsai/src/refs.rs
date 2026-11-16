//! Branches, remote branches and tags in the sidebar, with their actions
//! (CONTEXT.md: Branch, Tag).

use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
};

use bonsai_git::{
    Branch, CheckoutBlocker, Head, MergeMode, ResetMode, Snapshot, is_valid_ref_name,
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Render, SharedString, Styled, Subscription, WeakEntity,
    Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        checkbox::Checkbox,
        context_menu::context_menu,
        dialog::{
            DialogHandle, dialog, dialog_description, dialog_footer, dialog_popup, dialog_title,
        },
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
    inputs: Option<Inputs>,
    checkout_new: bool,
    collapsed_remotes: BTreeSet<String>,
    _events: Subscription,
}

impl RefsPanel {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            if matches!(event, RepositoryEvent::OperationFinished { .. }) {
                let next = this.follow_up.lock().ok().and_then(|mut slot| slot.take());
                if let Some(next) = next {
                    this.show(next, cx);
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
            inputs: None,
            checkout_new: true,
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
            Some(_) => true,
            None => false,
        }
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
            _ => (String::new(), String::new()),
        };
        let edit_url = matches!(self.dialog, Some(RefDialog::EditRemoteUrl { .. }));
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
        let subscriptions = [&name, &url]
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
            _subscriptions: subscriptions,
        });
    }

    fn local_menu(
        &self,
        branch: &Branch,
        snapshot: &Snapshot,
        cx: &mut Context<Self>,
    ) -> Vec<MenuItem> {
        let this = cx.entity().downgrade();
        let name = branch.name.clone();
        let mut items = Vec::new();
        if !branch.is_head {
            items.push(
                MenuItem::new("refs.checkout", format!("Switch to {name}"))
                    .icon(LucideIcon::Check)
                    .on_click(with(&this, {
                        let name = name.clone();
                        move |view, _, cx| view.checkout(Checkout::Branch(name.clone()), cx)
                    })),
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
                .disabled(branch.is_head)
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
        let mut items = vec![
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
        ];
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
        let mut items = vec![
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
        ];
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
        };
        let with_name = matches!(
            open,
            RefDialog::CreateBranch { .. }
                | RefDialog::RenameBranch { .. }
                | RefDialog::CreateTag { .. }
                | RefDialog::AddRemote
                | RefDialog::RenameRemote { .. }
        );
        let with_url = matches!(open, RefDialog::AddRemote | RefDialog::EditRemoteUrl { .. });
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
            .when_some(url_input, |this, url| {
                this.child(Input::new(&url).aria_label("Remote URL"))
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
                dialog_footer(cx)
                    .child(
                        Button::new("refs.dialog.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.dialog = None;
                                this.handle.close(window, cx);
                            })),
                    )
                    .child(
                        Button::new("refs.dialog.confirm")
                            .variant(if destructive {
                                ButtonVariant::Destructive
                            } else {
                                ButtonVariant::Default
                            })
                            .label(confirm)
                            .disabled(!can_confirm)
                            .on_click(cx.listener(|this, _, window, cx| this.confirm(window, cx))),
                    ),
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
                if !branch.is_head {
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
}
