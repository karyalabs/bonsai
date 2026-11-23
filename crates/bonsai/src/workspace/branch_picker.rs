//! Switch Branch (⌘B): a searchable list of local and remote branches,
//! opened from the title bar's branch button.

use std::fmt::Write as _;

use bonsai_git::Snapshot;
use gpui_icons::LucideIcon;
use gpui_kit::{AppContext as _, Context, IntoElement, Window};

use super::Workspace;
use crate::{
    commands,
    refs::{Checkout, RefDialog},
    ui::{
        command::{CommandEvent, CommandItem, CommandState, command_dialog_popup},
        dialog::dialog_with,
    },
};

const NEW_BRANCH: &str = "new-branch";

/// What a picked row does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchChoice {
    Switch(Checkout),
    NewBranch,
}

/// The rows for `snapshot`: New Branch, local branches (current first), then
/// remote branches that have no local branch of the same name.
#[must_use]
pub fn branch_items(snapshot: &Snapshot) -> Vec<CommandItem> {
    let mut items = vec![
        CommandItem::new(NEW_BRANCH, "New Branch…")
            .icon(LucideIcon::GitBranchPlus)
            .group("Actions")
            .shortcut("secondary-shift-b"),
    ];
    let mut local: Vec<_> = snapshot.branches.iter().collect();
    local.sort_by_key(|b| (!b.is_head, b.name.clone()));
    for branch in local {
        let mut description = Vec::new();
        if branch.is_head {
            description.push("current".to_owned());
        }
        if let Some(up) = &branch.upstream {
            if up.gone {
                description.push(format!("{} is gone", up.name));
            } else {
                let mut sync = up.name.clone();
                if up.ahead > 0 {
                    let _ = write!(sync, " ↑{}", up.ahead);
                }
                if up.behind > 0 {
                    let _ = write!(sync, " ↓{}", up.behind);
                }
                description.push(sync);
            }
        }
        items.push(
            CommandItem::new(format!("local:{}", branch.name), branch.name.clone())
                .icon(if branch.is_head {
                    LucideIcon::Check
                } else {
                    LucideIcon::GitBranch
                })
                .group("Branches")
                .description(description.join(" · "))
                .disabled(branch.is_head),
        );
    }
    for remote in &snapshot.remote_branches {
        if remote.name == "HEAD" || snapshot.branches.iter().any(|b| b.name == remote.name) {
            continue;
        }
        let full = format!("{}/{}", remote.remote, remote.name);
        items.push(
            CommandItem::new(
                format!("remote:{}\u{1f}{}", remote.remote, remote.name),
                full,
            )
            .icon(LucideIcon::Cloud)
            .group("Remote Branches")
            .description(format!("Creates {} tracking it", remote.name)),
        );
    }
    items
}

/// Reads a row's value back.
#[must_use]
pub fn parse_choice(value: &str) -> Option<BranchChoice> {
    if value == NEW_BRANCH {
        return Some(BranchChoice::NewBranch);
    }
    if let Some(name) = value.strip_prefix("local:") {
        return Some(BranchChoice::Switch(Checkout::Branch(name.to_owned())));
    }
    let (remote, branch) = value.strip_prefix("remote:")?.split_once('\u{1f}')?;
    Some(BranchChoice::Switch(Checkout::Remote {
        remote: remote.to_owned(),
        branch: branch.to_owned(),
        local: branch.to_owned(),
    }))
}

impl Workspace {
    /// Opens Switch Branch for the active repository.
    pub fn show_branch_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active).filter(|_| !self.welcome) else {
            return;
        };
        let Some(snapshot) = tab.model.read(cx).snapshot() else {
            return;
        };
        let items = branch_items(snapshot);
        let refs = tab.refs.clone();
        let state = cx.new(|cx| {
            let mut state = CommandState::new(items, window, cx);
            state.set_placeholder("Switch to a branch…", window, cx);
            state
        });
        let handle = self.branch_handle.clone();
        let subscription = cx.subscribe_in(
            &state,
            window,
            move |this, _, event: &CommandEvent, window, cx| match event {
                CommandEvent::Confirm(value) => {
                    handle.close(window, cx);
                    this.branch_picker = None;
                    window.focus(&this.focus, cx);
                    match parse_choice(value) {
                        Some(BranchChoice::Switch(target)) => {
                            refs.update(cx, |refs, cx| refs.checkout(target, cx));
                        }
                        Some(BranchChoice::NewBranch) => {
                            refs.update(cx, |refs, cx| {
                                refs.show(RefDialog::CreateBranch { start: None }, cx);
                            });
                        }
                        None => {}
                    }
                    cx.notify();
                }
                CommandEvent::Cancel => {
                    handle.close(window, cx);
                    this.branch_picker = None;
                    cx.notify();
                }
                CommandEvent::Query(_) => {}
            },
        );
        self.branch_picker = Some((state, subscription));
        self.branch_handle.open(window, cx);
        cx.notify();
    }

    pub(super) fn on_switch_branch(
        &mut self,
        _: &commands::SwitchBranch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_branch_picker(window, cx);
    }

    pub(super) fn on_new_branch(
        &mut self,
        _: &commands::NewBranch,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = self.tabs.get(self.active).filter(|_| !self.welcome) {
            tab.refs.update(cx, |refs, cx| {
                refs.show(RefDialog::CreateBranch { start: None }, cx);
            });
        }
    }

    pub(super) fn render_branch_picker(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let state = self.branch_picker.as_ref().map(|(state, _)| state.clone());
        let this = cx.entity().downgrade();
        dialog_with(
            "workspace.branches",
            &self.branch_handle,
            move |window, cx| {
                let state = state.expect("the picker is built before it opens");
                state.update(cx, |state, cx| state.focus(window, cx));
                command_dialog_popup("workspace.branches.popup", &state, cx)
            },
            window,
            cx,
        )
        .on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |this, cx| {
                this.branch_picker = None;
                cx.notify();
            });
            true
        })
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Branch, Head, Oid, RemoteBranch, Tracking};

    use super::*;

    fn oid() -> Oid {
        Oid::new("a".repeat(40))
    }

    #[test]
    fn lists_current_first_and_remote_only_branches() {
        let snapshot = Snapshot {
            head: Head::Branch {
                name: "main".into(),
                oid: Some(oid()),
            },
            branches: vec![
                Branch {
                    name: "feature".into(),
                    oid: oid(),
                    is_head: false,
                    upstream: None,
                },
                Branch {
                    name: "main".into(),
                    oid: oid(),
                    is_head: true,
                    upstream: Some(Tracking {
                        name: "origin/main".into(),
                        gone: false,
                        ahead: 1,
                        behind: 2,
                    }),
                },
            ],
            remote_branches: vec![
                RemoteBranch {
                    remote: "origin".into(),
                    name: "main".into(),
                    oid: oid(),
                },
                RemoteBranch {
                    remote: "origin".into(),
                    name: "release/1.0".into(),
                    oid: oid(),
                },
            ],
            changes: Vec::new(),
            tags: Vec::new(),
            remotes: Vec::new(),
            stashes: Vec::new(),
            worktrees: Vec::new(),
            submodules: Vec::new(),
            operation: None,
        };
        let items = branch_items(&snapshot);
        let values: Vec<&str> = items.iter().map(|i| i.value.as_ref()).collect();
        assert_eq!(
            values,
            [
                NEW_BRANCH,
                "local:main",
                "local:feature",
                "remote:origin\u{1f}release/1.0"
            ]
        );
        assert_eq!(
            parse_choice(values[3]),
            Some(BranchChoice::Switch(Checkout::Remote {
                remote: "origin".into(),
                branch: "release/1.0".into(),
                local: "release/1.0".into(),
            }))
        );
        assert_eq!(
            parse_choice("local:feature"),
            Some(BranchChoice::Switch(Checkout::Branch("feature".into())))
        );
    }
}
