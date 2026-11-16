//! The commit composer under the Changes list: summary, body, amend, signing
//! and recent messages (CONTEXT.md: Commit).

use bonsai_git::Signing;
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::Button,
        checkbox::Checkbox,
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        menu::{Menu, MenuItem, MenuState},
        theme::UiTheme,
    },
};

/// Summary length Git tools start to truncate at.
const SUMMARY_SOFT_LIMIT: usize = 50;
const SUMMARY_HARD_LIMIT: usize = 72;
const RECENT_MESSAGES: usize = 10;

/// Joins the summary and body the way `git commit` expects.
#[must_use]
pub fn compose_message(summary: &str, body: &str) -> String {
    let summary = summary.trim();
    let body = body.trim_end();
    if body.trim().is_empty() {
        summary.to_owned()
    } else {
        format!("{summary}\n\n{body}")
    }
}

/// Splits a message into its summary line and body.
#[must_use]
pub fn split_message(message: &str) -> (String, String) {
    let mut lines = message.trim().splitn(2, '\n');
    let summary = lines.next().unwrap_or_default().trim().to_owned();
    let body = lines.next().unwrap_or_default().trim().to_owned();
    (summary, body)
}

pub struct CommitComposer {
    model: Entity<RepositoryModel>,
    summary: Entity<InputState>,
    body: Entity<TextareaState>,
    amend: bool,
    signing: Option<Signing>,
    recent: Vec<String>,
    recent_menu: Entity<MenuState>,
    /// The commit operation in flight, matched against finished operations.
    pending: Option<SharedString>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl CommitComposer {
    pub fn new(
        model: Entity<RepositoryModel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let summary = cx.new(|cx| InputState::new(window, cx).placeholder("Summary"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(4)
                .placeholder("Description")
        });
        let recent_menu = cx.new(|cx| MenuState::new(Vec::new(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&summary, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter {
                    secondary: true, ..
                } => this.commit(cx),
                // Enter in the summary moves on to the description.
                InputEvent::PressEnter { .. } => {
                    this.body.update(cx, |body, cx| body.focus(window, cx));
                }
                InputEvent::Change => {
                    this.error = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe_in(&body, window, |this, _, event, _, cx| {
                if let InputEvent::PressEnter {
                    secondary: true, ..
                } = event
                {
                    this.commit(cx);
                }
            }),
            cx.subscribe_in(&model, window, |this, _, event, window, cx| match event {
                RepositoryEvent::OperationFinished { name, error, .. }
                    if this.pending.as_ref() == Some(name) =>
                {
                    this.pending = None;
                    match error {
                        None => {
                            this.amend = false;
                            this.set_message("", window, cx);
                            this.load_details(cx);
                        }
                        Some(error) => this.error = Some(error.clone()),
                    }
                    cx.notify();
                }
                RepositoryEvent::SnapshotChanged => cx.notify(),
                RepositoryEvent::OperationFinished { .. } => {}
            }),
        ];
        let mut this = Self {
            model,
            summary,
            body,
            amend: false,
            signing: None,
            recent: Vec::new(),
            recent_menu,
            pending: None,
            error: None,
            _subscriptions: subscriptions,
        };
        this.load_details(cx);
        this
    }

    #[must_use]
    pub fn message(&self, cx: &gpui_kit::App) -> String {
        compose_message(&self.summary.read(cx).value(), &self.body.read(cx).value())
    }

    #[must_use]
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    #[must_use]
    pub fn is_amending(&self) -> bool {
        self.amend
    }

    pub fn set_message(&mut self, message: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (summary, body) = split_message(message);
        self.summary
            .update(cx, |input, cx| input.set_value(summary, window, cx));
        self.body
            .update(cx, |input, cx| input.set_value(body, window, cx));
        cx.notify();
    }

    /// Reads signing settings and recent messages in the background.
    fn load_details(&mut self, cx: &mut Context<Self>) {
        let repository = self.model.read(cx).repository().clone();
        let task = cx.background_spawn(async move {
            (
                repository.commit_signing().ok().flatten(),
                repository
                    .recent_messages(RECENT_MESSAGES)
                    .unwrap_or_default(),
            )
        });
        cx.spawn(async move |this, cx| {
            let (signing, recent) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.signing = signing;
                this.recent = recent;
                this.rebuild_recent_menu(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn rebuild_recent_menu(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let items: Vec<MenuItem> = self
            .recent
            .iter()
            .enumerate()
            .map(|(index, message)| {
                let message = message.clone();
                let (summary, _) = split_message(&message);
                let this = this.clone();
                MenuItem::new(("composer.recent", index), summary).on_click(move |_, window, cx| {
                    let _ = this.update(cx, |this, cx| this.set_message(&message, window, cx));
                })
            })
            .collect();
        self.recent_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    fn staged_count(&self, cx: &gpui_kit::App) -> usize {
        self.model
            .read(cx)
            .snapshot()
            .map_or(0, |s| s.changes.iter().filter(|c| c.is_staged()).count())
    }

    /// Why Commit is unavailable, if it is.
    fn blocker(&self, cx: &gpui_kit::App) -> Option<&'static str> {
        if self.pending.is_some() {
            Some("Committing…")
        } else if self.summary.read(cx).value().trim().is_empty() {
            Some("Write a summary to commit.")
        } else if !self.amend && self.staged_count(cx) == 0 {
            Some("Stage changes to commit them.")
        } else {
            None
        }
    }

    /// Commits (or amends) with the current message. The message stays if the
    /// commit fails, for example when a hook rejects it.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        if self.blocker(cx).is_some() {
            return;
        }
        let message = self.message(cx);
        let amend = self.amend;
        let name: SharedString = if amend { "Amend commit" } else { "Commit" }.into();
        self.pending = Some(name.clone());
        self.error = None;
        self.model.update(cx, |model, cx| {
            model.run(
                name,
                move |repository| repository.commit(&message, amend).map(|_| ()),
                cx,
            );
        });
        cx.notify();
    }

    fn set_amend(&mut self, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = amend;
        let last = self.recent.first().cloned();
        if amend {
            // Prefill the last message unless the user already wrote one.
            if self.summary.read(cx).value().trim().is_empty()
                && let Some(last) = last
            {
                self.set_message(&last, window, cx);
            }
        } else if last.is_some_and(|last| self.message(cx) == last) {
            self.set_message("", window, cx);
        }
        cx.notify();
    }
}

impl Render for CommitComposer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let length = self.summary.read(cx).value().chars().count();
        let length_color = if length > SUMMARY_HARD_LIMIT {
            c.destructive
        } else if length > SUMMARY_SOFT_LIMIT {
            c.warning
        } else {
            c.muted_foreground
        };
        let blocker = self.blocker(cx);
        let staged = self.staged_count(cx);
        let label = match (self.amend, staged) {
            (true, _) => "Amend Last Commit".to_owned(),
            (false, 0) => "Commit".to_owned(),
            (false, 1) => "Commit 1 File".to_owned(),
            (false, n) => format!("Commit {n} Files"),
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .border_t_1()
            .border_color(c.border)
            .child(
                div()
                    .relative()
                    .child(Input::new(&self.summary).aria_label("Commit summary"))
                    .when(length > 0, |this| {
                        this.child(
                            div()
                                .absolute()
                                .right(px(10.))
                                .top(px(8.))
                                .text_xs()
                                .text_color(length_color)
                                .child(format!("{length}/{SUMMARY_HARD_LIMIT}")),
                        )
                    }),
            )
            .child(Textarea::new(&self.body).aria_label("Commit description"))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .max_h(px(120.))
                        .overflow_hidden()
                        .text_xs()
                        .font_family(theme.fonts.mono.clone())
                        .text_color(c.destructive)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                Checkbox::new("composer.amend")
                                    .checked(self.amend)
                                    .aria_label("Amend the last commit")
                                    .on_change(cx.listener(|this, checked: &bool, window, cx| {
                                        this.set_amend(*checked, window, cx);
                                    })),
                            )
                            .child(div().text_sm().child("Amend")),
                    )
                    .when_some(self.signing, |this, signing| {
                        this.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(
                                    lucide(LucideIcon::LockKeyhole)
                                        .size(px(12.))
                                        .text_color(c.muted_foreground),
                                )
                                .child(format!("Signed with {}", signing.label())),
                        )
                    })
                    .child(div().flex_1())
                    .when(!self.recent.is_empty(), |this| {
                        this.child(
                            Menu::new(&self.recent_menu, "Recent messages").trigger(
                                lucide(LucideIcon::Clock)
                                    .size(px(14.))
                                    .text_color(c.foreground),
                            ),
                        )
                    })
                    .child(
                        Button::new("composer.commit")
                            .label(label)
                            .disabled(blocker.is_some())
                            .on_click(cx.listener(|this, _, _, cx| this.commit(cx))),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(blocker.unwrap_or("⌘↩ commits.")),
            )
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    #[test]
    fn messages_round_trip_between_fields_and_git() {
        assert_eq!(compose_message(" Fix it ", "  "), "Fix it");
        assert_eq!(
            compose_message("Fix it", "Because.\n\n"),
            "Fix it\n\nBecause."
        );
        assert_eq!(
            split_message("Fix it\n\nBecause.\nReally."),
            ("Fix it".to_owned(), "Because.\nReally.".to_owned())
        );
        assert_eq!(split_message("Only"), ("Only".to_owned(), String::new()));
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let g = Git::locate(None).unwrap();
        g.run(&g.command(args.iter().copied()).cwd(dir)).unwrap();
    }

    #[gpui_kit::test]
    fn commits_keeps_the_message_on_failure_and_amends(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let repository =
            Repository::init(&Git::locate(None).unwrap(), dir.path(), Some("main")).unwrap();
        git(dir.path(), &["config", "user.name", "Bonsai"]);
        git(dir.path(), &["config", "user.email", "bonsai@example.com"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "."]);
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (composer, cx) =
            cx.add_window_view(|window, cx| CommitComposer::new(model.clone(), window, cx));
        cx.run_until_parked();

        // A failing hook keeps the message and shows the hook's output.
        let hook = dir.path().join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'tests failed' >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        composer.update_in(cx, |composer, window, cx| {
            composer.set_message("Add a\n\nFirst file.", window, cx);
            composer.commit(cx);
        });
        cx.run_until_parked();
        composer.read_with(cx, |composer, cx| {
            assert!(composer.error().is_some_and(|e| e.contains("tests failed")));
            assert_eq!(composer.message(cx), "Add a\n\nFirst file.");
        });

        std::fs::remove_file(&hook).unwrap();
        composer.update(cx, CommitComposer::commit);
        cx.run_until_parked();
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.error(), None);
            assert_eq!(composer.message(cx), "");
        });

        // Amend prefills the last message.
        composer.update_in(cx, |composer, window, cx| {
            composer.set_amend(true, window, cx);
        });
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.message(cx), "Add a\n\nFirst file.");
        });
        composer.update_in(cx, |composer, window, cx| {
            composer.set_message("Add a (amended)", window, cx);
            composer.commit(cx);
        });
        cx.run_until_parked();
        let repository = model.read_with(cx, |model, _| model.repository().clone());
        assert_eq!(repository.recent_messages(5).unwrap(), ["Add a (amended)"]);
    }
}
