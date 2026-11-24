//! Clean Up Branches: local branches that are merged, or whose upstream is
//! gone, offered for deletion together.

use std::collections::BTreeSet;

use bonsai_git::{Head, StaleBranch, StaleReason};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    px,
};

use crate::{
    repository::RepositoryModel,
    ui::{
        button::{Button, ButtonVariant},
        checkbox::Checkbox,
        dialog::{dialog_description, dialog_footer, dialog_title},
        theme::UiTheme,
    },
};

pub enum CleanUpEvent {
    /// Deleting started, or the user dismissed the dialog.
    Done,
}

enum Status {
    Loading,
    Ready(Vec<StaleBranch>),
    Failed(SharedString),
}

pub struct CleanUpDialog {
    model: Entity<RepositoryModel>,
    /// The branch merged branches are measured against.
    base: String,
    status: Status,
    selected: BTreeSet<String>,
}

impl EventEmitter<CleanUpEvent> for CleanUpDialog {}

/// The main branch: `main`, `master`, `trunk` or `develop`, or else the
/// current branch.
fn base_branch(model: &RepositoryModel) -> Option<String> {
    let snapshot = model.snapshot()?;
    ["main", "master", "trunk", "develop"]
        .into_iter()
        .find(|name| snapshot.branches.iter().any(|b| b.name == *name))
        .map(str::to_owned)
        .or_else(|| match &snapshot.head {
            Head::Branch { name, .. } => Some(name.clone()),
            Head::Detached { .. } => None,
        })
}

impl CleanUpDialog {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let base = base_branch(model.read(cx)).unwrap_or_else(|| "main".to_owned());
        let mut this = Self {
            model,
            base,
            status: Status::Loading,
            selected: BTreeSet::new(),
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let repository = self.model.read(cx).repository().clone();
        let base = self.base.clone();
        let task = cx.background_spawn(async move { repository.stale_branches(&base) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(branches) => {
                        // Merged branches are safe to delete; ones whose
                        // upstream is gone may hold unmerged work, so
                        // they start unticked.
                        this.selected = branches
                            .iter()
                            .filter(|b| b.reason == StaleReason::Merged)
                            .map(|b| b.name.clone())
                            .collect();
                        Status::Ready(branches)
                    }
                    Err(error) => Status::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        })
        .detach();
    }

    #[must_use]
    pub fn branches(&self) -> &[StaleBranch] {
        match &self.status {
            Status::Ready(branches) => branches,
            _ => &[],
        }
    }

    #[must_use]
    pub fn selected(&self) -> &BTreeSet<String> {
        &self.selected
    }

    pub fn toggle(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.selected.remove(name) {
            self.selected.insert(name.to_owned());
        }
        cx.notify();
    }

    /// Deletes the ticked branches in one undoable operation.
    pub fn delete(&mut self, cx: &mut Context<Self>) {
        let names: Vec<String> = self.selected.iter().cloned().collect();
        if names.is_empty() {
            return;
        }
        let name = match names.as_slice() {
            [one] => format!("Delete {one}"),
            many => format!("Delete {} branches", many.len()),
        };
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| {
                    names
                        .iter()
                        .try_for_each(|name| repository.delete_branch(name, true))
                },
                cx,
            );
        });
        cx.emit(CleanUpEvent::Done);
    }
}

impl Render for CleanUpDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let base = self.base.clone();
        let body = match &self.status {
            Status::Loading => div()
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Looking for branches…")
                .into_any_element(),
            Status::Failed(error) => div()
                .text_sm()
                .text_color(c.destructive)
                .child(error.clone())
                .into_any_element(),
            Status::Ready(branches) if branches.is_empty() => div()
                .text_sm()
                .text_color(c.muted_foreground)
                .child(format!(
                    "Nothing to clean up. Every other branch has work that isn't in {base}."
                ))
                .into_any_element(),
            Status::Ready(branches) => div()
                .id("clean-up.list")
                .max_h(px(320.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(2.))
                .children(branches.iter().enumerate().map(|(i, branch)| {
                    let name = branch.name.clone();
                    let checked = self.selected.contains(&name);
                    let detail = match branch.reason {
                        StaleReason::Merged => format!("Merged into {base}"),
                        StaleReason::UpstreamGone => {
                            "Deleted on the remote; may have unmerged commits".to_owned()
                        }
                    };
                    let warn = branch.reason == StaleReason::UpstreamGone;
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .py(px(6.))
                        .child(
                            Checkbox::new(("clean-up.branch", i))
                                .checked(checked)
                                .aria_label(SharedString::from(name.clone()))
                                .on_change(cx.listener(move |this, _: &bool, _, cx| {
                                    this.toggle(&name, cx);
                                })),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .min_w_0()
                                .child(div().text_sm().truncate().child(branch.name.clone()))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(if warn {
                                            c.warning
                                        } else {
                                            c.muted_foreground
                                        })
                                        .child(detail),
                                ),
                        )
                }))
                .into_any_element(),
        };
        let count = self.selected.len();
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(dialog_title("clean-up.title", cx).child("Clean Up Branches"))
            .child(dialog_description("clean-up.description", cx).child(format!(
                "Local branches merged into {base}, or deleted on the remote. Deleting them here doesn't touch the remote, and ⌘Z brings them back."
            )))
            .child(body)
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("clean-up.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(CleanUpEvent::Done))),
                    )
                    .child(
                        Button::new("clean-up.delete")
                            .variant(ButtonVariant::Destructive)
                            .label(match count {
                                0 => "Delete".to_owned(),
                                1 => "Delete 1 Branch".to_owned(),
                                n => format!("Delete {n} Branches"),
                            })
                            .disabled(count == 0)
                            .on_click(cx.listener(|this, _, _, cx| this.delete(cx))),
                    ),
            )
    }
}
