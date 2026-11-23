//! The bar shown while bisecting: what is being tested, how many steps are
//! left, Good, Bad and Skip, a command to run at each step, and the first
//! bad commit once found (CONTEXT.md: Bisect).

use bonsai_git::{BisectState, BisectVerdict, InProgress, Oid};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BisectEvent {
    /// Show this commit in History.
    ShowCommit(Oid),
}

pub struct BisectBar {
    model: Entity<RepositoryModel>,
    state: Option<BisectState>,
    /// The command field, once Run Command is chosen.
    command: Option<Entity<InputState>>,
    load: Option<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<BisectEvent> for BisectBar {}

fn short(oid: &Oid) -> String {
    oid.as_str().chars().take(7).collect()
}

impl BisectBar {
    pub fn new(model: Entity<RepositoryModel>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
            // Finding the culprit leaves HEAD where it is, so the snapshot
            // may not change: reload after every operation too.
            if matches!(
                event,
                RepositoryEvent::SnapshotChanged | RepositoryEvent::OperationFinished { .. }
            ) {
                this.reload(cx);
            }
        });
        let mut this = Self {
            model,
            state: None,
            command: None,
            load: None,
            subscriptions: vec![subscription],
        };
        this.reload(cx);
        this
    }

    #[must_use]
    pub fn state(&self) -> Option<&BisectState> {
        self.state.as_ref()
    }

    /// Reads the bisect state when the snapshot says one is in progress.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let bisecting = self
            .model
            .read(cx)
            .snapshot()
            .is_some_and(|s| s.operation == Some(InProgress::Bisect));
        if !bisecting {
            if self.state.take().is_some() {
                self.command = None;
                cx.notify();
            }
            return;
        }
        let repository = self.model.read(cx).repository().clone();
        let read = cx.background_spawn(async move { repository.bisect_state() });
        self.load = Some(cx.spawn(async move |this, cx| {
            let state = read.await.ok().flatten();
            let _ = this.update(cx, |this, cx| {
                this.state = state;
                cx.notify();
            });
        }));
    }

    /// Marks the commit being tested.
    pub fn mark(&mut self, verdict: BisectVerdict, cx: &mut Context<Self>) {
        let name = match verdict {
            BisectVerdict::Good => "Bisect: Mark Good",
            BisectVerdict::Bad => "Bisect: Mark Bad",
            BisectVerdict::Skip => "Bisect: Skip",
        };
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.bisect_mark(verdict).map(drop),
                cx,
            );
        });
    }

    /// Marks a commit picked in History, starting a bisect if needed.
    pub fn mark_commit(&mut self, oid: Oid, verdict: BisectVerdict, cx: &mut Context<Self>) {
        let name = format!(
            "Bisect: Mark {} {}",
            short(&oid),
            if verdict == BisectVerdict::Good {
                "Good"
            } else {
                "Bad"
            }
        );
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| {
                    repository
                        .bisect_mark_commit(oid.as_str(), verdict)
                        .map(drop)
                },
                cx,
            );
        });
    }

    /// Ends the bisect and goes back to where it started.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        self.command = None;
        self.model.update(cx, |model, cx| {
            model.run("Stop Bisect", bonsai_git::Repository::bisect_reset, cx);
        });
    }

    /// Shows the command field.
    pub fn ask_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command.is_some() {
            return;
        }
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Command that fails on bad commits, e.g. cargo test parser")
        });
        input.update(cx, |input, cx| input.focus(window, cx));
        self.subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.run_command(cx);
                }
            },
        ));
        self.command = Some(input);
        cx.notify();
    }

    /// Runs the command at each step until the first bad commit is found.
    pub fn run_command(&mut self, cx: &mut Context<Self>) {
        let Some(input) = &self.command else {
            return;
        };
        let command = input.read(cx).value().trim().to_owned();
        if command.is_empty() {
            return;
        }
        self.model.update(cx, |model, cx| {
            model.run(
                format!("Bisect: Run {command}"),
                move |repository| repository.bisect_run(&command).map(drop),
                cx,
            );
        });
    }

    fn busy(&self, cx: &gpui_kit::App) -> bool {
        self.model.read(cx).running().is_some()
    }
}

impl Render for BisectBar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(state) = self.state.clone() else {
            return div().into_any_element();
        };
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let busy = self.busy(cx);
        let mono = theme.fonts.mono.clone();
        let commit_label = |oid: &Oid, summary: &str| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w_0()
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(mono.clone())
                        .text_color(c.muted_foreground)
                        .child(short(oid)),
                )
                .child(div().min_w_0().truncate().child(summary.to_owned()))
        };
        let button = |id: &'static str, label: &'static str| {
            Button::new(id)
                .size(ButtonSize::Sm)
                .variant(ButtonVariant::Outline)
                .label(label)
                .disabled(busy)
        };

        let (icon, icon_color, headline, detail): (
            LucideIcon,
            _,
            SharedString,
            gpui_kit::AnyElement,
        ) = if let Some(found) = &state.found {
            (
                LucideIcon::Crosshair,
                c.destructive,
                "Found the first bad commit".into(),
                commit_label(&found.oid, &found.summary).into_any_element(),
            )
        } else if state.needs_more_marks() {
            let missing = if state.bad.is_none() {
                "Mark a bad commit in History, or mark the commit you're on."
            } else {
                "Now mark a good commit in History: one from before the problem."
            };
            (
                LucideIcon::Search,
                c.foreground,
                "Bisecting".into(),
                div()
                    .text_color(c.muted_foreground)
                    .child(missing)
                    .into_any_element(),
            )
        } else {
            let steps = state.steps_left();
            let headline = format!(
                "Bisecting · about {steps} step{} left",
                if steps == 1 { "" } else { "s" }
            );
            let detail = match &state.current {
                Some(current) => div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(c.muted_foreground)
                            .child("Testing"),
                    )
                    .child(commit_label(&current.oid, &current.summary))
                    .into_any_element(),
                None => div().into_any_element(),
            };
            (LucideIcon::Search, c.foreground, headline.into(), detail)
        };

        let mut actions = div().flex().items_center().gap(px(4.)).flex_shrink_0();
        if let Some(found) = &state.found {
            let oid = found.oid.clone();
            actions = actions
                .child(button("bisect.show", "Show Commit").on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(BisectEvent::ShowCommit(oid.clone()))),
                ))
                .child(
                    Button::new("bisect.finish")
                        .size(ButtonSize::Sm)
                        .label("Finish")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                );
        } else {
            let can_test = state.current.is_some();
            actions = actions
                .child(
                    button("bisect.good", "Good")
                        .disabled(busy || !can_test)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mark(BisectVerdict::Good, cx);
                        })),
                )
                .child(
                    button("bisect.bad", "Bad")
                        .disabled(busy || !can_test)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mark(BisectVerdict::Bad, cx);
                        })),
                )
                .when(!state.needs_more_marks(), |this| {
                    this.child(
                        button("bisect.skip", "Skip").on_click(
                            cx.listener(|this, _, _, cx| this.mark(BisectVerdict::Skip, cx)),
                        ),
                    )
                    .child(button("bisect.run", "Run Command…").on_click(cx.listener(
                        |this, _, window, cx| {
                            this.ask_command(window, cx);
                        },
                    )))
                })
                .child(
                    Button::new("bisect.stop")
                        .size(ButtonSize::Sm)
                        .variant(ButtonVariant::Ghost)
                        .label("Stop")
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                );
        }

        let command_row = self
            .command
            .clone()
            .filter(|_| state.found.is_none())
            .map(|input| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&input).aria_label("Bisect command")),
                    )
                    .child(
                        Button::new("bisect.run.go")
                            .size(ButtonSize::Sm)
                            .label("Run")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.run_command(cx))),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .flex_shrink_0()
                            .child("Exit 0 is good, 125 skips, anything else is bad."),
                    )
            });

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .py(px(8.))
            .bg(c.muted)
            .border_b_1()
            .border_color(c.border)
            .text_sm()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        lucide(icon)
                            .size(px(16.))
                            .flex_none()
                            .text_color(icon_color),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(headline),
                    )
                    .child(div().flex_1().min_w_0().child(detail))
                    .child(actions),
            )
            .children(command_row)
            .into_any_element()
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
            .trim()
            .to_owned()
    }

    #[gpui_kit::test]
    fn bisects_from_history_marks_to_the_first_bad_commit(cx: &mut TestAppContext) {
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
        let mut commits = Vec::new();
        for i in 1..=8 {
            let value = if i >= 6 { "broken" } else { "fine" };
            std::fs::write(root.join("value.txt"), format!("{value} {i}")).unwrap();
            git(root, &["add", "."]);
            git(root, &["commit", "-qm", &format!("Commit {i}")]);
            commits.push(Oid::new(git(root, &["rev-parse", "HEAD"])));
        }

        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let bar = cx.new(|cx| BisectBar::new(model.clone(), cx));
        cx.run_until_parked();
        bar.read_with(cx, |bar, _| assert!(bar.state().is_none()));

        bar.update(cx, |bar, cx| {
            bar.mark_commit(commits[7].clone(), BisectVerdict::Bad, cx);
            bar.mark_commit(commits[0].clone(), BisectVerdict::Good, cx);
        });
        cx.run_until_parked();
        for _ in 0..4 {
            let done = bar.read_with(cx, |bar, _| {
                let state = bar.state().expect("bisecting");
                assert!(!state.needs_more_marks());
                state.found.is_some()
            });
            if done {
                break;
            }
            let broken = std::fs::read_to_string(root.join("value.txt"))
                .unwrap()
                .starts_with("broken");
            bar.update(cx, |bar, cx| {
                bar.mark(
                    if broken {
                        BisectVerdict::Bad
                    } else {
                        BisectVerdict::Good
                    },
                    cx,
                );
            });
            cx.run_until_parked();
        }
        bar.read_with(cx, |bar, _| {
            let found = bar.state().and_then(|s| s.found.clone()).expect("found");
            assert_eq!(found.oid, commits[5]);
        });

        bar.update(cx, BisectBar::stop);
        cx.run_until_parked();
        bar.read_with(cx, |bar, _| assert!(bar.state().is_none()));
        assert_eq!(git(root, &["branch", "--show-current"]), "main");
    }
}
