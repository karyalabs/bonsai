//! The commit composer under the Changes list: summary, body, amend, signing
//! and recent messages (CONTEXT.md: Commit).

use bonsai_git::{Head, Signing};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonSize, ButtonVariant, style_button},
        input::{Input, InputEvent, InputState, Textarea, TextareaState},
        menu::{Menu, MenuItem, MenuState},
        switch::Switch,
        theme::UiTheme,
        tooltip::text_tooltip,
    },
};

/// Summary length Git tools start to truncate at.
const SUMMARY_SOFT_LIMIT: usize = 50;
const SUMMARY_HARD_LIMIT: usize = 72;
const RECENT_MESSAGES: usize = 10;
const RECENT_AUTHORS: usize = 12;

/// A message with Git's comment lines removed and spacing trimmed, as
/// `--cleanup=strip` leaves it.
#[must_use]
pub fn strip_comments(message: &str) -> String {
    message
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// Adds a trailer such as `Co-authored-by: …` to the end of a commit body,
/// after a blank line unless the body already ends in trailers. A trailer
/// already there isn't added twice.
#[must_use]
pub fn add_trailer(body: &str, trailer: &str) -> String {
    let body = body.trim_end();
    if body.lines().any(|line| line.trim() == trailer) {
        return body.to_owned();
    }
    if body.is_empty() {
        return trailer.to_owned();
    }
    let last = body.lines().last().unwrap_or_default();
    let ends_in_trailer = last.split_once(": ").is_some_and(|(key, _)| {
        !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    });
    let gap = if ends_in_trailer { "\n" } else { "\n\n" };
    format!("{body}{gap}{trailer}")
}

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
    /// The `commit.template` text each new message starts from.
    template: Option<String>,
    /// People who recently committed here, offered as co-authors.
    authors: Vec<(String, String)>,
    authors_menu: Entity<MenuState>,
    /// Commit and Push, and other commit options.
    commit_menu: Entity<MenuState>,
    /// Push once the commit in flight lands.
    push_after: bool,
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
        let recent_menu = cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(360.)));
        let authors_menu = cx.new(|cx| MenuState::new(Vec::new(), cx).width(px(320.)));
        let this = cx.entity().downgrade();
        let commit_menu = cx.new(|cx| {
            MenuState::new(
                [MenuItem::new("composer.commit-push", "Commit and Push")
                    .icon(LucideIcon::Upload)
                    .shortcut("secondary-shift-enter")
                    .on_click(move |_, _, cx| {
                        let _ = this.update(cx, CommitComposer::commit_and_push);
                    })],
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&summary, window, |this, _, event, window, cx| match event {
                InputEvent::PressEnter {
                    secondary: true,
                    shift: true,
                } => this.commit_and_push(cx),
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
                    secondary: true,
                    shift,
                } = event
                {
                    if *shift {
                        this.commit_and_push(cx);
                    } else {
                        this.commit(cx);
                    }
                }
            }),
            cx.subscribe_in(&model, window, |this, _, event, window, cx| match event {
                RepositoryEvent::OperationFinished { name, error, .. }
                    if this.pending.as_ref() == Some(name) =>
                {
                    this.pending = None;
                    let push = std::mem::take(&mut this.push_after);
                    match error {
                        None => {
                            this.amend = false;
                            this.reset_message(window, cx);
                            this.load_details(window, cx);
                            if push {
                                window.dispatch_action(Box::new(crate::commands::Push), cx);
                            }
                        }
                        Some(error) => this.error = Some(error.clone()),
                    }
                    cx.notify();
                }
                // Undoing a commit brings its message back, ready to commit
                // again; the reset leaves the undone commit in ORIG_HEAD.
                RepositoryEvent::OperationFinished {
                    name, error: None, ..
                } if name.as_ref() == "Undo Commit" && this.is_untouched(cx) => {
                    let message = this.model.read(cx).repository().commit_message("ORIG_HEAD");
                    if let Ok(message) = message {
                        this.set_message(&message, window, cx);
                    }
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
            template: None,
            authors: Vec::new(),
            authors_menu,
            commit_menu,
            push_after: false,
            pending: None,
            error: None,
            _subscriptions: subscriptions,
        };
        this.load_details(window, cx);
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

    /// Whether the message is empty, or the template as it was filled in.
    fn is_untouched(&self, cx: &gpui_kit::App) -> bool {
        let message = self.message(cx);
        message.trim().is_empty()
            || self
                .template
                .as_deref()
                .is_some_and(|template| strip_comments(&message) == strip_comments(template))
    }

    /// Empties the message, or fills it with the commit template.
    fn reset_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let template = self.template.clone().unwrap_or_default();
        self.set_message(&template, window, cx);
    }

    /// Reads signing settings and recent messages in the background.
    fn load_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let repository = self.model.read(cx).repository().clone();
        let task = cx.background_spawn(async move {
            (
                repository.commit_signing().ok().flatten(),
                repository
                    .recent_messages(RECENT_MESSAGES)
                    .unwrap_or_default(),
                repository
                    .recent_authors(RECENT_AUTHORS)
                    .unwrap_or_default(),
                repository.commit_template().ok().flatten(),
            )
        });
        cx.spawn_in(window, async move |this, cx| {
            let (signing, recent, authors, template) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.signing = signing;
                this.recent = recent;
                this.authors = authors;
                // A new template fills an untouched message box.
                let untouched = this.is_untouched(cx);
                this.template = template;
                if untouched && !this.amend {
                    this.reset_message(window, cx);
                }
                this.rebuild_recent_menu(cx);
                this.rebuild_authors_menu(cx);
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

    fn rebuild_authors_menu(&mut self, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let heading = MenuItem::label("composer.co-authors", "Add Co-author");
        let items: Vec<MenuItem> = std::iter::once(heading)
            .chain(
                self.authors
                    .iter()
                    .enumerate()
                    .map(|(index, (name, email))| {
                        let trailer = format!("Co-authored-by: {name} <{email}>");
                        let this = this.clone();
                        MenuItem::new(("composer.co-author", index), name.clone())
                            .description(email.clone())
                            .on_click(move |_, window, cx| {
                                let _ = this.update(cx, |this, cx| {
                                    this.add_co_author(&trailer, window, cx);
                                });
                            })
                    }),
            )
            .collect();
        self.authors_menu
            .update(cx, |menu, cx| menu.set_items(items, cx));
    }

    /// Credits someone else on this commit with a `Co-authored-by` trailer,
    /// which GitHub and GitLab show on the commit.
    fn add_co_author(&mut self, trailer: &str, window: &mut Window, cx: &mut Context<Self>) {
        let body = add_trailer(&self.body.read(cx).value(), trailer);
        self.body
            .update(cx, |input, cx| input.set_value(body, window, cx));
        cx.notify();
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
        } else if self.template.is_some() && self.is_untouched(cx) {
            Some("Fill in the commit template to commit.")
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

    /// Commits, then pushes the branch once the commit lands.
    pub fn commit_and_push(&mut self, cx: &mut Context<Self>) {
        if self.blocker(cx).is_some() {
            return;
        }
        self.push_after = true;
        self.commit(cx);
    }

    fn branch_name(&self, cx: &gpui_kit::App) -> Option<String> {
        match &self.model.read(cx).snapshot()?.head {
            Head::Branch { name, .. } => Some(name.clone()),
            Head::Detached { .. } => None,
        }
    }

    fn set_amend(&mut self, amend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = amend;
        let last = self.recent.first().cloned();
        if amend {
            // Prefill the last message unless the user already wrote one.
            if self.is_untouched(cx)
                && let Some(last) = last
            {
                self.set_message(&last, window, cx);
            }
        } else if last.is_some_and(|last| self.message(cx) == last) {
            self.reset_message(window, cx);
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
        let target = self
            .branch_name(cx)
            .map(|name| format!(" to {name}"))
            .unwrap_or_default();
        let label = match (self.amend, staged) {
            (true, _) => "Amend Last Commit".to_owned(),
            (false, 0) => "Commit".to_owned(),
            (false, 1) => format!("Commit 1 File{target}"),
            (false, n) => format!("Commit {n} Files{target}"),
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
                    .gap(px(10.))
                    .child(
                        div()
                            .id("composer.amend.row")
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                Switch::new("composer.amend")
                                    .checked(self.amend)
                                    .aria_label("Amend the last commit")
                                    .on_change(cx.listener(|this, checked: &bool, window, cx| {
                                        this.set_amend(*checked, window, cx);
                                    })),
                            )
                            .child(div().text_sm().child("Amend"))
                            .tooltip(|_, cx| {
                                text_tooltip(
                                    "Replace the last commit with these changes and message".into(),
                                    cx,
                                )
                            }),
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
                    .when(!self.authors.is_empty(), |this| {
                        let theme = theme.clone();
                        this.child(
                            Menu::new(&self.authors_menu, "Add co-author")
                                .trigger_style_with(move |button| {
                                    style_button(
                                        button,
                                        false,
                                        ButtonVariant::Ghost,
                                        ButtonSize::IconSm,
                                        &theme,
                                    )
                                })
                                .trigger(
                                    lucide(LucideIcon::UserPlus)
                                        .size(px(15.))
                                        .text_color(c.muted_foreground),
                                ),
                        )
                    })
                    .when(!self.recent.is_empty(), |this| {
                        let theme = theme.clone();
                        this.child(
                            Menu::new(&self.recent_menu, "Recent messages")
                                .trigger_style_with(move |button| {
                                    style_button(
                                        button,
                                        false,
                                        ButtonVariant::Ghost,
                                        ButtonSize::IconSm,
                                        &theme,
                                    )
                                })
                                .trigger(
                                    lucide(LucideIcon::Clock)
                                        .size(px(15.))
                                        .text_color(c.muted_foreground),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(
                                div()
                                    .id("composer.commit.tip")
                                    .child(
                                        Button::new("composer.commit")
                                            .label(label)
                                            .disabled(blocker.is_some())
                                            .on_click(
                                                cx.listener(|this, _, _, cx| this.commit(cx)),
                                            ),
                                    )
                                    .tooltip(move |_, cx| {
                                        text_tooltip(
                                            blocker
                                                .unwrap_or("Commit (⌘↩) · Commit and Push (⇧⌘↩)")
                                                .into(),
                                            cx,
                                        )
                                    }),
                            )
                            .child({
                                let theme = theme.clone();
                                let disabled = blocker.is_some();
                                Menu::new(&self.commit_menu, "More commit options")
                                    .disabled(disabled)
                                    .trigger_style_with(move |button| {
                                        style_button(
                                            button,
                                            disabled,
                                            ButtonVariant::Default,
                                            ButtonSize::IconSm,
                                            &theme,
                                        )
                                        .ml(px(1.))
                                    })
                                    .trigger(
                                        lucide(LucideIcon::ChevronDown)
                                            .size(px(14.))
                                            .text_color(c.primary_foreground),
                                    )
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use bonsai_git::{Git, Repository};
    use gpui_kit::TestAppContext;

    use super::*;

    #[test]
    fn strips_comments_like_git() {
        assert_eq!(
            strip_comments("Area: fix\n\n# Why?\nBecause.  \n"),
            "Area: fix\n\nBecause."
        );
        assert_eq!(strip_comments("# only a comment\n"), "");
    }

    #[test]
    fn co_authors_go_in_a_trailer_block_once() {
        let ada = "Co-authored-by: Ada Lovelace <ada@example.com>";
        let grace = "Co-authored-by: Grace Hopper <grace@example.com>";
        assert_eq!(add_trailer("", ada), ada);
        assert_eq!(
            add_trailer("Fixes the mill.\n", ada),
            format!("Fixes the mill.\n\n{ada}")
        );
        let once = add_trailer("Fixes the mill.", ada);
        assert_eq!(add_trailer(&once, ada), once);
        assert_eq!(add_trailer(&once, grace), format!("{once}\n{grace}"));
        assert_eq!(
            add_trailer("Note: it works", ada),
            format!("Note: it works\n{ada}")
        );
    }

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
    fn undoing_a_commit_brings_its_message_back(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let repository =
            Repository::init(&Git::locate(None).unwrap(), dir.path(), Some("main")).unwrap();
        git(dir.path(), &["config", "user.name", "Ada Lovelace"]);
        git(dir.path(), &["config", "user.email", "ada@example.com"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-qm", "Start"]);
        std::fs::write(dir.path().join("a.txt"), "b").unwrap();
        git(dir.path(), &["add", "."]);
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (composer, cx) =
            cx.add_window_view(|window, cx| CommitComposer::new(model.clone(), window, cx));
        cx.run_until_parked();
        composer.update_in(cx, |composer, window, cx| {
            composer.set_message("Change a\n\nTo b.", window, cx);
            composer.commit(cx);
        });
        cx.run_until_parked();
        composer.read_with(cx, |composer, cx| assert_eq!(composer.message(cx), ""));
        model.update(cx, |model, cx| assert!(model.undo(cx)));
        cx.run_until_parked();
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.message(cx), "Change a\n\nTo b.");
        });
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

    #[gpui_kit::test]
    fn starts_each_message_from_the_commit_template(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let repository =
            Repository::init(&Git::locate(None).unwrap(), dir.path(), Some("main")).unwrap();
        git(dir.path(), &["config", "user.name", "Bonsai"]);
        git(dir.path(), &["config", "user.email", "bonsai@example.com"]);
        git(dir.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.path().join(".gitmessage"), "Area: \n\n# Why?\n").unwrap();
        git(dir.path(), &["config", "commit.template", ".gitmessage"]);
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        git(dir.path(), &["add", "a.txt"]);
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (composer, cx) =
            cx.add_window_view(|window, cx| CommitComposer::new(model.clone(), window, cx));
        cx.run_until_parked();

        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.message(cx), "Area:\n\n# Why?");
            assert_eq!(
                composer.blocker(cx),
                Some("Fill in the commit template to commit.")
            );
        });
        composer.update_in(cx, |composer, window, cx| {
            composer.set_message("Mill: add a\n\n# Why?\nIt grinds.", window, cx);
            composer.commit(cx);
        });
        cx.run_until_parked();
        composer.read_with(cx, |composer, cx| {
            assert_eq!(composer.error(), None);
            assert_eq!(composer.message(cx), "Area:\n\n# Why?");
        });
        let repository = model.read_with(cx, |model, _| model.repository().clone());
        assert_eq!(
            repository.recent_messages(5).unwrap(),
            ["Mill: add a\n\nIt grinds."]
        );
    }
}
