//! The Edit Commits sheet: an interactive rebase. Drag commits to reorder
//! them, and pick, reword, squash, fix up or drop each one (CONTEXT.md:
//! Operation in progress).

use bonsai_git::{Oid, RebaseAction, RebaseStep};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    confirm::Confirm,
    purge_sheet::callout,
    repository::RepositoryModel,
    ui::{
        dialog::{dialog_description, dialog_title},
        input::InputEvent,
        sortable::{Sortable, SortableGroup, SortableRow},
        textarea::{Textarea, TextareaState},
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
    },
};

const ACTIONS: [RebaseAction; 5] = [
    RebaseAction::Pick,
    RebaseAction::Reword,
    RebaseAction::Squash,
    RebaseAction::Fixup,
    RebaseAction::Drop,
];

pub enum RebaseEvent {
    Dismissed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Loading,
    Ready,
    Failed(String),
}

pub struct RebaseSheet {
    model: Entity<RepositoryModel>,
    /// The commit the edited ones sit on; `None` edits from the first commit.
    base: Option<Oid>,
    status: Status,
    /// In the order they will be applied, oldest first.
    steps: Vec<RebaseStep>,
    original: Vec<Oid>,
    /// The step whose message is open in the editor.
    editing: Option<usize>,
    message: Entity<TextareaState>,
    /// Applied to commits once the steps load, from a History shortcut.
    preset: Vec<(Oid, RebaseAction)>,
}

impl EventEmitter<RebaseEvent> for RebaseSheet {}

impl RebaseSheet {
    /// Edits the commits from `from` (included) up to HEAD.
    pub fn new(
        model: Entity<RepositoryModel>,
        base: Option<Oid>,
        preset: Vec<(Oid, RebaseAction)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let repository = model.read(cx).repository().clone();
        let task_base = base.clone();
        let task = cx.background_spawn(async move {
            repository
                .rebase_steps(task_base.as_ref().map(Oid::as_str))
                .map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(steps) if steps.is_empty() => {
                        this.status =
                            Status::Failed("There are no commits to edit after this one.".into());
                    }
                    Ok(steps) => {
                        this.original = steps.iter().map(|s| s.oid.clone()).collect();
                        this.steps = steps;
                        this.status = Status::Ready;
                        for (oid, action) in std::mem::take(&mut this.preset) {
                            let Some(index) = this.steps.iter().position(|s| s.oid == oid) else {
                                this.status = Status::Failed(
                                    "That commit isn't on the current branch.".into(),
                                );
                                break;
                            };
                            this.set_action(index, action, window, cx);
                        }
                    }
                    Err(error) => this.status = Status::Failed(error),
                }
                cx.notify();
            });
        })
        .detach();
        let message = cx.new(|cx| TextareaState::new(window, cx).rows(4));
        cx.subscribe(&message, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sync_message(cx);
                cx.notify();
            }
        })
        .detach();
        Self {
            model,
            base,
            status: Status::Loading,
            steps: Vec::new(),
            original: Vec::new(),
            editing: None,
            message,
            preset,
        }
    }

    #[must_use]
    pub fn steps(&self) -> &[RebaseStep] {
        &self.steps
    }

    pub fn set_action(
        &mut self,
        index: usize,
        action: RebaseAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(step) = self.steps.get_mut(index) else {
            return;
        };
        step.action = action;
        if action == RebaseAction::Reword {
            self.edit_message(index, window, cx);
        } else {
            step.new_message = None;
            if self.editing == Some(index) {
                self.editing = None;
            }
        }
        cx.notify();
    }

    /// Copies the editor's text into the step it edits.
    fn sync_message(&mut self, cx: &gpui_kit::App) {
        if let Some(step) = self.editing.and_then(|i| self.steps.get_mut(i)) {
            let text = self.message.read(cx).value().to_string();
            step.new_message = (text.trim() != step.message.trim()).then_some(text);
        }
    }

    fn edit_message(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_message(cx);
        let Some(step) = self.steps.get(index) else {
            return;
        };
        let text = step
            .new_message
            .clone()
            .unwrap_or_else(|| step.message.clone());
        self.editing = Some(index);
        self.message.update(cx, |input, cx| {
            input.set_value(text, window, cx);
            input.focus(window, cx);
        });
    }

    /// Moves the step at `from` to `to` (counted after it leaves `from`).
    pub fn move_step(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.steps.len() || to >= self.steps.len() {
            return;
        }
        let editing = self.editing.map(|i| self.steps[i].oid.clone());
        let step = self.steps.remove(from);
        self.steps.insert(to, step);
        self.editing = editing.and_then(|oid| self.steps.iter().position(|s| s.oid == oid));
        cx.notify();
    }

    /// Why the plan can't run, if it can't.
    fn problem(&self) -> Option<String> {
        let first = self.steps.iter().find(|s| s.action != RebaseAction::Drop);
        match first {
            None if self.base.is_none() => {
                return Some("A branch needs at least one commit. Keep one.".into());
            }
            Some(step) if step.action.joins() => {
                return Some(format!(
                    "\u{201c}{}\u{201d} has no commit above it to join. Pick it, or move it down.",
                    step.summary()
                ));
            }
            _ => {}
        }
        self.steps
            .iter()
            .find(|s| {
                s.action == RebaseAction::Reword
                    && s.new_message
                        .as_deref()
                        .is_some_and(|m| m.trim().is_empty())
            })
            .map(|_| "A reworded commit needs a message.".to_owned())
    }

    fn changed(&self) -> bool {
        self.steps.iter().any(|s| {
            s.action != RebaseAction::Pick
                && !(s.action == RebaseAction::Reword && s.new_message.is_none())
        }) || self.steps.iter().map(|s| &s.oid).ne(self.original.iter())
    }

    #[must_use]
    pub fn can_confirm(&self) -> bool {
        self.status == Status::Ready && self.changed() && self.problem().is_none()
    }

    /// Starts the rebase and closes the sheet.
    pub fn confirm(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.sync_message(cx);
        if !self.can_confirm() {
            return;
        }
        let (base, steps) = (self.base.clone(), self.steps.clone());
        let count = steps.len();
        self.model.update(cx, |model, cx| {
            model.run(
                format!("Edit {count} commit{}", if count == 1 { "" } else { "s" }),
                move |repository| {
                    repository
                        .rebase_interactive(base.as_ref().map(Oid::as_str), &steps)
                        .map(drop)
                },
                cx,
            );
        });
        cx.emit(RebaseEvent::Dismissed);
    }

    /// Commits that are already on the branch's upstream.
    fn pushed(&self, cx: &gpui_kit::App) -> Option<(usize, String)> {
        let snapshot = self.model.read(cx).snapshot()?;
        let branch = snapshot.branches.iter().find(|b| b.is_head)?;
        let upstream = branch.upstream.as_ref().filter(|u| !u.gone)?;
        let ahead = usize::try_from(upstream.ahead).unwrap_or(usize::MAX);
        let pushed = self.original.len().saturating_sub(ahead);
        (pushed > 0).then(|| (pushed, upstream.name.clone()))
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> SortableRow {
        let step = self.steps[index].clone();
        let editing = self.editing == Some(index);
        let this = cx.entity().downgrade();
        let label = step.summary().to_owned();
        SortableRow::new(
            SharedString::from(format!("rebase.row.{}", step.oid)),
            label,
            move |_, cx| {
                let theme = UiTheme::read(cx).clone();
                let c = &theme.colors;
                let dropped = step.action == RebaseAction::Drop;
                let summary = step
                    .new_message
                    .as_deref()
                    .and_then(|m| m.lines().next())
                    .unwrap_or_else(|| step.summary())
                    .to_owned();
                let selected = ACTIONS
                    .iter()
                    .position(|a| *a == step.action)
                    .map(|i| SharedString::from(i.to_string()));
                let mut group =
                    ToggleGroup::new(SharedString::from(format!("rebase.action.{}", step.oid)))
                        .aria_label("What happens to this commit")
                        .segmented(true)
                        .value(selected);
                for (i, action) in ACTIONS.iter().enumerate() {
                    group = group.item(
                        ToggleGroupItem::new(
                            SharedString::from(format!("rebase.action.{}.{i}", step.oid)),
                            i.to_string(),
                        )
                        .aria_label(action.label())
                        .child(div().text_xs().child(action.label())),
                    );
                }
                let on_action = this.clone();
                let group = group.on_change(move |next, window, cx| {
                    let Some(action) = next
                        .first()
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| ACTIONS.get(i).copied())
                    else {
                        return;
                    };
                    let _ = on_action.update(cx, |sheet, cx| {
                        sheet.set_action(index, action, window, cx);
                    });
                });
                let on_edit = this.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(40.))
                    .px(px(8.))
                    .rounded(px(6.))
                    .when(editing, |this| this.bg(c.accent))
                    .child(
                        lucide(LucideIcon::GripVertical)
                            .size(px(14.))
                            .flex_none()
                            .text_color(c.muted_foreground),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(60.))
                            .font_family(theme.fonts.mono.clone())
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(step.oid.short().to_owned()),
                    )
                    .child(div().flex_none().child(group))
                    .when(step.action.joins(), |this| {
                        this.child(
                            lucide(LucideIcon::CornerLeftUp)
                                .size(px(14.))
                                .flex_none()
                                .text_color(c.primary),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("rebase.summary.{}", step.oid)))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(dropped, |this| {
                                this.line_through().text_color(c.muted_foreground)
                            })
                            .when(step.action == RebaseAction::Fixup, |this| {
                                this.text_color(c.muted_foreground)
                            })
                            .when(step.new_message.is_some(), |this| {
                                this.text_color(c.primary)
                            })
                            .when(step.action == RebaseAction::Reword, |this| {
                                this.cursor_pointer().on_click(move |_, window, cx| {
                                    let _ = on_edit.update(cx, |sheet, cx| {
                                        sheet.edit_message(index, window, cx);
                                    });
                                })
                            })
                            .child(summary),
                    )
                    .into_any_element()
            },
        )
    }
}

impl Render for RebaseSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_message(cx);
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let mut body = div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(dialog_title("rebase.title", cx).child("Edit Commits"))
            .child(dialog_description("rebase.description", cx).child(
                "Drag to reorder, oldest at the top. Squash and Fixup fold a commit into the one above it. Your uncommitted changes are set aside and put back.",
            ));
        match &self.status {
            Status::Loading => {
                body = body.child(
                    div()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child("Reading commits…"),
                );
            }
            Status::Failed(error) => {
                body = body.child(callout(
                    LucideIcon::CircleAlert,
                    c.destructive,
                    &[error.as_str()],
                    &theme,
                ));
            }
            Status::Ready => {
                let rows: Vec<SortableRow> = (0..self.steps.len())
                    .map(|i| self.render_row(i, cx))
                    .collect();
                let this = cx.entity().downgrade();
                body = body.child(
                    div()
                        .id("rebase.list")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .rounded(px(8.))
                        .border_1()
                        .border_color(c.border)
                        .p(px(4.))
                        .child(
                            Sortable::new("rebase.sortable", "Commits")
                                .gap(px(2.))
                                .group(
                                    SortableGroup::new("rebase.group", "Commits")
                                        .header(|_, _| div())
                                        .rows(rows),
                                )
                                .on_reorder(move |change, _, cx| {
                                    let _ = this.update(cx, |sheet, cx| {
                                        sheet.move_step(change.from.index, change.to.index, cx);
                                    });
                                }),
                        ),
                );
                if let Some(index) = self.editing {
                    let short = self.steps[index].oid.short().to_owned();
                    body = body.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(c.muted_foreground)
                                    .child(format!("New message for {short}")),
                            )
                            .child(Textarea::new(&self.message).aria_label("Commit message")),
                    );
                }
                let kept = self
                    .steps
                    .iter()
                    .filter(|s| s.action != RebaseAction::Drop && !s.action.joins())
                    .count();
                let mut counts = vec![format!("{} commits become {kept}", self.steps.len())];
                for (action, verb) in [
                    (RebaseAction::Reword, "reworded"),
                    (RebaseAction::Squash, "squashed"),
                    (RebaseAction::Fixup, "fixed up"),
                    (RebaseAction::Drop, "dropped"),
                ] {
                    let n = self.steps.iter().filter(|s| s.action == action).count();
                    if n > 0 {
                        counts.push(format!("{n} {verb}"));
                    }
                }
                body = body.child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(counts.join(" · ")),
                );
                if let Some(problem) = self.problem() {
                    body = body.child(div().text_xs().text_color(c.destructive).child(problem));
                }
                if let Some((pushed, upstream)) = self.pushed(cx) {
                    let line = format!(
                        "{pushed} of these commits {} already on {upstream}. Rewriting them means force pushing, which changes history for anyone who pulled them.",
                        if pushed == 1 { "is" } else { "are" }
                    );
                    body = body.child(callout(
                        LucideIcon::TriangleAlert,
                        c.warning,
                        &[line.as_str()],
                        &theme,
                    ));
                }
            }
        }
        let count = self.steps.len();
        let pushed = self.status == Status::Ready && self.pushed(cx).is_some();
        body.child(
            Confirm::new(
                "rebase",
                if count > 0 {
                    format!(
                        "Rewrite {count} Commit{}",
                        if count == 1 { "" } else { "s" }
                    )
                } else {
                    "Rewrite".to_owned()
                },
                |_: &mut Self, _, cx| cx.emit(RebaseEvent::Dismissed),
                Self::confirm,
            )
            // Red only when it rewrites commits others may have.
            .destructive(pushed)
            .enabled(self.can_confirm())
            .footer(cx),
        )
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let g = Git::locate(None).unwrap();
        g.run(&g.command(args.iter().copied()).cwd(dir))
            .unwrap()
            .stdout_lossy()
    }

    #[gpui_kit::test]
    fn plans_and_runs_an_interactive_rebase(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repository = Repository::init(&Git::locate(None).unwrap(), root, Some("main")).unwrap();
        for (key, value) in [
            ("user.name", "Ada Lovelace"),
            ("user.email", "ada@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(root, &["config", key, value]);
        }
        for name in ["base", "one", "two", "three"] {
            std::fs::write(root.join(name), name).unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", name]);
        }
        let base = Oid::new(git(root, &["rev-parse", "HEAD~3"]).trim());
        let three = Oid::new(git(root, &["rev-parse", "HEAD"]).trim());

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        cx.run_until_parked();
        let (sheet, cx) = cx.add_window_view(|window, cx| {
            RebaseSheet::new(
                model.clone(),
                Some(base),
                vec![(three, RebaseAction::Drop)],
                window,
                cx,
            )
        });
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            let plan: Vec<(&str, RebaseAction)> = sheet
                .steps()
                .iter()
                .map(|s| (s.summary(), s.action))
                .collect();
            assert_eq!(
                plan,
                [
                    ("one", RebaseAction::Pick),
                    ("two", RebaseAction::Pick),
                    ("three", RebaseAction::Drop),
                ]
            );
            assert!(sheet.can_confirm());
        });

        // Squashing the first commit has nothing to join.
        sheet.update_in(cx, |sheet, window, cx| {
            sheet.set_action(0, RebaseAction::Squash, window, cx);
        });
        sheet.read_with(cx, |sheet, _| assert!(!sheet.can_confirm()));
        // two moves first and is reworded; one folds into it.
        sheet.update_in(cx, |sheet, window, cx| {
            sheet.set_action(0, RebaseAction::Fixup, window, cx);
            sheet.move_step(1, 0, cx);
            sheet.set_action(0, RebaseAction::Reword, window, cx);
        });
        let message = sheet.read_with(cx, |sheet, _| sheet.message.clone());
        message.update_in(cx, |input, window, cx| {
            input.set_value("Two, with one folded in", window, cx);
        });
        let dismissed = std::rc::Rc::new(std::cell::Cell::new(false));
        let _events = cx.update(|_, cx| {
            let dismissed = dismissed.clone();
            cx.subscribe(&sheet, move |_, event: &RebaseEvent, _| {
                if matches!(event, RebaseEvent::Dismissed) {
                    dismissed.set(true);
                }
            })
        });
        sheet.update_in(cx, RebaseSheet::confirm);
        cx.run_until_parked();
        assert!(dismissed.get());
        assert_eq!(
            git(root, &["log", "--format=%s"])
                .lines()
                .collect::<Vec<_>>(),
            ["Two, with one folded in", "base"]
        );
        assert!(root.join("one").exists() && !root.join("three").exists());
    }
}
