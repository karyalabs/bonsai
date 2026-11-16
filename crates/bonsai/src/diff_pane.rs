//! The detail pane's diff: loads a patch in the background and shows it with
//! GPUI Kit's diff, unified or side by side.

use bonsai_git::{
    ApplyMode, DiffOptions, DiffOutput, DiffTarget, LinePosition, LineSide, Whitespace,
    partial_patch,
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled, Subscription, Window,
    component::diff::{DiffFile, DiffLineRange, DiffMode, DiffSide, DiffState},
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    confirm::{self, Confirm},
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        dialog::{DialogHandle, dialog},
        diff::{DiffAction, DiffIntent, DiffView},
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
    },
};

/// What the pane shows instead of a diff, or that a diff is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneStatus {
    Nothing,
    Loading,
    Ready,
    /// Nothing differs, for example after `-w` hid every change.
    Empty,
    TooLarge {
        bytes: usize,
    },
    Failed(SharedString),
}

pub struct DiffPane {
    model: Entity<RepositoryModel>,
    state: Entity<DiffState>,
    target: Option<DiffTarget>,
    title: SharedString,
    status: PaneStatus,
    whitespace: Whitespace,
    /// The patch on screen, for building partial patches from a selection.
    patch: Option<String>,
    discard: DialogHandle,
    /// The partial patch the open Discard Lines dialog will reverse.
    pending_discard: Option<String>,
    /// Why the last line action could not run.
    error: Option<SharedString>,
    /// Ignores results for a target that has since changed.
    generation: u64,
    _events: Subscription,
}

impl DiffPane {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| DiffState::new(Vec::new(), cx));
        let events = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            // Work tree diffs follow the files; commit diffs never change.
            if matches!(event, RepositoryEvent::SnapshotChanged)
                && matches!(
                    this.target,
                    Some(
                        DiffTarget::Unstaged(_) | DiffTarget::Staged(_) | DiffTarget::Untracked(_)
                    )
                )
            {
                this.load(false, cx);
            }
        });
        Self {
            model,
            state,
            target: None,
            title: SharedString::default(),
            status: PaneStatus::Nothing,
            whitespace: Whitespace::Show,
            patch: None,
            discard: DialogHandle::new(false),
            pending_discard: None,
            error: None,
            generation: 0,
            _events: events,
        }
    }

    #[must_use]
    pub fn status(&self) -> &PaneStatus {
        &self.status
    }

    #[must_use]
    pub fn state(&self) -> &Entity<DiffState> {
        &self.state
    }

    #[must_use]
    pub fn target(&self) -> Option<&DiffTarget> {
        self.target.as_ref()
    }

    /// Shows `target`, or the empty state for `None`.
    pub fn show(
        &mut self,
        target: Option<DiffTarget>,
        title: SharedString,
        cx: &mut Context<Self>,
    ) {
        if self.target == target {
            return;
        }
        self.target = target;
        self.title = title;
        self.load(false, cx);
    }

    fn load(&mut self, any_size: bool, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        let Some(target) = self.target.clone() else {
            self.status = PaneStatus::Nothing;
            self.state
                .update(cx, |state, cx| state.set_files(Vec::new(), cx));
            cx.notify();
            return;
        };
        if self.status != PaneStatus::Ready {
            self.status = PaneStatus::Loading;
        }
        let repository = self.model.read(cx).repository().clone();
        let options = DiffOptions {
            whitespace: self.whitespace,
            max_bytes: if any_size {
                None
            } else {
                DiffOptions::default().max_bytes
            },
            ..DiffOptions::default()
        };
        let task = cx.background_spawn(async move {
            repository
                .diff(&target, options)
                .map_err(|e| e.to_string())
                .and_then(|output| match output {
                    DiffOutput::Patch(patch) => DiffFile::parse(&patch)
                        .map(|files| Ok((files, patch)))
                        .map_err(|e| format!("Bonsai could not read this diff: {e:?}")),
                    DiffOutput::TooLarge { bytes } => Ok(Err(bytes)),
                })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.patch = None;
                this.status = match result {
                    Ok(Ok((files, _))) if files.is_empty() => {
                        this.state
                            .update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        PaneStatus::Empty
                    }
                    Ok(Ok((files, patch))) => {
                        this.patch = Some(patch);
                        this.state
                            .update(cx, |state, cx| state.set_files(files, cx));
                        PaneStatus::Ready
                    }
                    Ok(Err(bytes)) => PaneStatus::TooLarge { bytes },
                    Err(error) => PaneStatus::Failed(error.into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_whitespace(&mut self, whitespace: Whitespace, cx: &mut Context<Self>) {
        if self.whitespace != whitespace {
            self.whitespace = whitespace;
            self.load(false, cx);
        }
    }

    /// The line actions the shown diff offers: none for commits, untracked
    /// files, or while whitespace is hidden (the patch would not apply).
    fn line_actions(&self) -> Vec<DiffAction> {
        if self.whitespace != Whitespace::Show {
            return Vec::new();
        }
        match self.target {
            Some(DiffTarget::Unstaged(_)) => vec![
                DiffAction::new("stage", "Stage Lines").icon(LucideIcon::Plus),
                DiffAction::new("discard", "Discard Lines…")
                    .icon(LucideIcon::Undo2)
                    .destructive(true),
            ],
            Some(DiffTarget::Staged(_)) => {
                vec![DiffAction::new("unstage", "Unstage Lines").icon(LucideIcon::Minus)]
            }
            _ => Vec::new(),
        }
    }

    /// Stages, unstages or (after confirming) discards the selected lines.
    pub fn act(&mut self, intent: &DiffIntent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(range) = &intent.range else { return };
        let mode = match intent.action.as_ref() {
            "stage" => ApplyMode::Stage,
            "unstage" => ApplyMode::Unstage,
            "discard" => ApplyMode::Discard,
            _ => return,
        };
        let Some(partial) = self.partial(range, mode) else {
            self.error = Some("Select at least one added or removed line.".into());
            cx.notify();
            return;
        };
        self.error = None;
        if mode == ApplyMode::Discard {
            self.pending_discard = Some(partial);
            self.discard.open(window, cx);
            cx.notify();
            return;
        }
        self.apply(partial, mode, cx);
    }

    fn partial(&self, range: &DiffLineRange, mode: ApplyMode) -> Option<String> {
        let side = |side: DiffSide| match side {
            DiffSide::Original => LineSide::Old,
            DiffSide::Modified => LineSide::New,
        };
        partial_patch(
            self.patch.as_deref()?,
            range.path(),
            LinePosition::new(side(range.side()), range.start()),
            LinePosition::new(side(range.end_side()), range.end()),
            mode,
        )
    }

    fn apply(&mut self, partial: String, mode: ApplyMode, cx: &mut Context<Self>) {
        let name = match mode {
            ApplyMode::Stage => "Stage lines",
            ApplyMode::Unstage => "Unstage lines",
            ApplyMode::Discard => "Discard lines",
        };
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.apply_patch(&partial, mode),
                cx,
            );
        });
    }

    fn render_discard(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let popup = confirm::popup(
            "diff.discard",
            "Discard these lines?",
            "The selected changes go back to the staged or committed version. This can't be undone.",
            cx,
        )
        .child(
            Confirm::new(
                "diff.discard",
                "Discard",
                |this: &mut Self, window, cx| {
                    this.pending_discard = None;
                    this.discard.close(window, cx);
                },
                |this, window, cx| {
                    this.discard.close(window, cx);
                    if let Some(partial) = this.pending_discard.take() {
                        this.apply(partial, ApplyMode::Discard, cx);
                    }
                },
            )
            .footer(cx),
        );
        let this = cx.entity().downgrade();
        dialog("diff.discard", &self.discard, popup, window, cx).on_cancel(move |_, _, cx| {
            let _ = this.update(cx, |pane, cx| {
                pane.pending_discard = None;
                cx.notify();
            });
            true
        })
    }

    fn render_message(
        icon: LucideIcon,
        title: &str,
        detail: Option<String>,
        theme: &UiTheme,
    ) -> gpui_kit::Div {
        let c = &theme.colors;
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .p(px(24.))
            .child(lucide(icon).size(px(20.)).text_color(c.muted_foreground))
            .child(
                div()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(title.to_owned()),
            )
            .when_some(detail, |this, detail| {
                this.child(
                    div()
                        .max_w(px(420.))
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child(detail),
                )
            })
    }
}

impl Render for DiffPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let body = match &self.status {
            PaneStatus::Nothing => Self::render_message(
                LucideIcon::FileDiff,
                "Nothing selected",
                Some("Select a change or a commit to see its diff.".into()),
                &theme,
            )
            .into_any_element(),
            PaneStatus::Loading => div()
                .p(px(16.))
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Loading the diff…")
                .into_any_element(),
            PaneStatus::Empty => Self::render_message(
                LucideIcon::Equal,
                "No differences",
                (self.whitespace != Whitespace::Show)
                    .then(|| "Whitespace changes are hidden.".to_owned()),
                &theme,
            )
            .into_any_element(),
            PaneStatus::TooLarge { bytes } => Self::render_message(
                LucideIcon::TriangleAlert,
                "This diff is large",
                Some(format!(
                    "It is {} MB. Showing it may take a moment.",
                    bytes.div_ceil(1_048_576)
                )),
                &theme,
            )
            .child(
                Button::new("diff.load-anyway")
                    .variant(ButtonVariant::Outline)
                    .label("Show Anyway")
                    .on_click(cx.listener(|this, _, _, cx| this.load(true, cx))),
            )
            .into_any_element(),
            PaneStatus::Failed(error) => Self::render_message(
                LucideIcon::TriangleAlert,
                "Bonsai could not load this diff",
                Some(error.to_string()),
                &theme,
            )
            .into_any_element(),
            PaneStatus::Ready => {
                let actions = self.line_actions();
                let view = cx.entity().downgrade();
                DiffView::new("diff.view", &self.state)
                    .read_only(actions.is_empty())
                    .selection_actions(actions)
                    .on_intent(move |intent, window, cx| {
                        let _ = view.update(cx, |pane, cx| pane.act(intent, window, cx));
                    })
                    .into_any_element()
            }
        };
        let mode = self.state.read(cx).mode();
        let layout_state = self.state.clone();
        let toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .px(px(12.))
            .py(px(6.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Button::new("diff.whitespace")
                            .variant(ButtonVariant::Ghost)
                            .label(match self.whitespace {
                                Whitespace::Show => "Whitespace: Shown",
                                Whitespace::IgnoreChanges => "Whitespace: Amount Ignored",
                                Whitespace::IgnoreAll => "Whitespace: Ignored",
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                let next = match this.whitespace {
                                    Whitespace::Show => Whitespace::IgnoreAll,
                                    Whitespace::IgnoreAll | Whitespace::IgnoreChanges => {
                                        Whitespace::Show
                                    }
                                };
                                this.set_whitespace(next, cx);
                            })),
                    )
                    .child(
                        ToggleGroup::new("diff.layout")
                            .aria_label("Diff layout")
                            .segmented(true)
                            .value(vec![SharedString::from(if mode == DiffMode::Split {
                                "split"
                            } else {
                                "unified"
                            })])
                            .item(
                                ToggleGroupItem::new("diff.layout.unified", "unified")
                                    .aria_label("Unified")
                                    .child("Unified"),
                            )
                            .item(
                                ToggleGroupItem::new("diff.layout.split", "split")
                                    .aria_label("Side by side")
                                    .child("Side by Side"),
                            )
                            .on_change(move |next, _, cx| {
                                let Some(value) = next.first() else { return };
                                let mode = if value == "split" {
                                    DiffMode::Split
                                } else {
                                    DiffMode::Unified
                                };
                                layout_state.update(cx, |state, cx| state.set_mode(mode, cx));
                            }),
                    ),
            );
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(self.target.is_some(), |this| this.child(toolbar))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .px(px(12.))
                        .py(px(6.))
                        .text_sm()
                        .text_color(c.destructive)
                        .child(error),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_discard(window, cx))
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    #[gpui_kit::test]
    fn loads_work_tree_diffs_and_follows_the_files(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        std::fs::write(dir.path().join("notes.md"), "# Notes\n").unwrap();
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let pane = cx.new(|cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();

        pane.update(cx, |pane, cx| {
            pane.show(
                Some(DiffTarget::Untracked("notes.md".into())),
                "notes.md".into(),
                cx,
            );
        });
        cx.run_until_parked();
        pane.read_with(cx, |pane, cx| {
            assert_eq!(pane.status(), &PaneStatus::Ready);
            let state = pane.state().read(cx);
            assert_eq!(state.files().len(), 1);
            assert_eq!(state.files()[0].additions(), 1);
        });

        // Staging the file empties the untracked diff on the next snapshot.
        model.update(cx, |model, cx| {
            model.run("Stage", |r| r.stage(&["notes.md".into()]), cx);
        });
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| assert_eq!(pane.status(), &PaneStatus::Ready));

        pane.update(cx, |pane, cx| pane.show(None, SharedString::default(), cx));
        cx.run_until_parked();
        pane.read_with(cx, |pane, _| {
            assert_eq!(pane.status(), &PaneStatus::Nothing);
        });
    }

    #[gpui_kit::test]
    fn stages_and_discards_selected_lines(cx: &mut TestAppContext) {
        use crate::ui::diff::DiffIntent;
        use gpui_kit::component::diff::{DiffLineRange, DiffSide};

        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let git = Git::locate(None).unwrap();
        let repository = Repository::init(&git, dir.path(), Some("main")).unwrap();
        let run = |args: &[&str]| {
            git.run(&git.command(args.iter().copied()).cwd(dir.path()))
                .unwrap()
                .stdout_lossy()
        };
        run(&["config", "user.name", "Ada"]);
        run(&["config", "user.email", "ada@example.com"]);
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\n").unwrap();
        run(&["add", "."]);
        run(&["-c", "commit.gpgsign=false", "commit", "-qm", "base"]);
        std::fs::write(dir.path().join("f.txt"), "a\nX\nb\nY\nc\n").unwrap();

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (pane, cx) = cx.add_window_view(|_, cx| DiffPane::new(model.clone(), cx));
        cx.run_until_parked();
        pane.update(cx, |pane, cx| {
            pane.show(Some(DiffTarget::Unstaged(Vec::new())), "f.txt".into(), cx);
        });
        cx.run_until_parked();
        let intent = |action: &str, line: usize| DiffIntent {
            action: action.into(),
            path: "f.txt".into(),
            range: Some(DiffLineRange::new("f.txt", DiffSide::Modified, line, line)),
        };

        pane.update_in(cx, |pane, window, cx| {
            assert_eq!(pane.line_actions().len(), 2);
            pane.act(&intent("stage", 2), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(run(&["show", ":f.txt"]), "a\nX\nb\nc\n");

        // Discarding asks first, then reverts only the chosen line.
        pane.update_in(cx, |pane, window, cx| {
            pane.act(&intent("discard", 4), window, cx);
            assert!(pane.pending_discard.is_some());
            assert!(pane.discard.is_open());
            let partial = pane.pending_discard.take().unwrap();
            pane.apply(partial, ApplyMode::Discard, cx);
        });
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "a\nX\nb\nc\n"
        );

        // A staged diff offers Unstage Lines only.
        pane.update(cx, |pane, cx| {
            pane.show(Some(DiffTarget::Staged(Vec::new())), "f.txt".into(), cx);
        });
        cx.run_until_parked();
        pane.update_in(cx, |pane, window, cx| {
            assert_eq!(pane.line_actions().len(), 1);
            pane.act(&intent("unstage", 2), window, cx);
        });
        cx.run_until_parked();
        assert_eq!(run(&["show", ":f.txt"]), "a\nb\nc\n");
    }
}
