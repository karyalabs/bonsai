//! The detail pane's diff: loads a patch in the background and shows it with
//! GPUI Kit's diff, unified or side by side.

use bonsai_git::{DiffOptions, DiffOutput, DiffTarget, Whitespace};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled, Subscription, Window,
    component::diff::{Diff, DiffFile, DiffMode, DiffState},
    div,
    prelude::FluentBuilder as _,
    px,
};

use crate::{
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
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
                        .map(Ok)
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
                this.status = match result {
                    Ok(Ok(files)) if files.is_empty() => {
                        this.state
                            .update(cx, |state, cx| state.set_files(Vec::new(), cx));
                        PaneStatus::Empty
                    }
                    Ok(Ok(files)) => {
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            PaneStatus::Ready => Diff::new(&self.state).into_any_element(),
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
            .child(div().flex_1().min_h_0().child(body))
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
}
