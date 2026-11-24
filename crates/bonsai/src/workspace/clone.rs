//! The Clone Repository dialog: a URL, a destination, progress and cancel.

use std::path::PathBuf;

use bonsai_git::{
    CancelToken, CloneOptions, CloneProgress, Git, Repository, clone_name, is_empty_or_missing,
};
use futures::{StreamExt as _, channel::mpsc};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled, Subscription, Window, div, prelude::FluentBuilder as _, px,
};

use crate::ui::{
    button::{Button, ButtonVariant},
    checkbox::Checkbox,
    dialog::{dialog_description, dialog_footer, dialog_title},
    input::{Input, InputEvent, InputState},
    progress::Progress,
    theme::UiTheme,
};

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloneEvent {
    /// The clone finished at this path; open it.
    Cloned(PathBuf),
    /// The user chose another destination folder; remember it.
    ParentChanged(PathBuf),
    /// The user dismissed the dialog.
    Dismissed,
}

enum Phase {
    Editing,
    Running {
        progress: Option<CloneProgress>,
        cancel: CancelToken,
    },
    Failed(SharedString),
}

enum Message {
    Progress(CloneProgress),
    Done(Result<PathBuf, String>),
}

pub struct CloneDialog {
    git: Git,
    url: Entity<InputState>,
    name: Entity<InputState>,
    parent: PathBuf,
    /// Set once the user types a name, so the URL stops overwriting it.
    name_edited: bool,
    options: CloneOptions,
    phase: Phase,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CloneEvent> for CloneDialog {}

impl CloneDialog {
    pub fn new(git: Git, parent: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx.new(|cx| {
            InputState::new(window, cx).placeholder("https://github.com/owner/repository.git")
        });
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("repository"));
        let subscriptions = vec![
            cx.subscribe_in(&url, window, |this, url, event, window, cx| match event {
                InputEvent::Change => {
                    if !this.name_edited
                        && let Some(derived) = clone_name(&url.read(cx).value())
                    {
                        this.name
                            .update(cx, |name, cx| name.set_value(derived, window, cx));
                    }
                    this.clear_failure();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.start(cx),
                _ => {}
            }),
            cx.subscribe_in(&name, window, |this, _, event, _, cx| match event {
                InputEvent::Change => {
                    this.name_edited = true;
                    this.clear_failure();
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.start(cx),
                _ => {}
            }),
        ];
        url.update(cx, |url, cx| url.focus(window, cx));
        Self {
            git,
            url,
            name,
            parent,
            name_edited: false,
            options: CloneOptions::default(),
            phase: Phase::Editing,
            _subscriptions: subscriptions,
        }
    }

    /// Puts the cursor in the URL field, or the name when a URL is set.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let field = if self.url.read(cx).value().is_empty() {
            &self.url
        } else {
            &self.name
        };
        field.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Sets the URL, as a paste or a provider's repository browser would.
    pub fn set_url(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.url
            .update(cx, |input, cx| input.set_value(url.to_owned(), window, cx));
        if !self.name_edited
            && let Some(derived) = clone_name(url)
        {
            self.name
                .update(cx, |name, cx| name.set_value(derived, window, cx));
        }
        cx.notify();
    }

    /// Clone only recent history, and whether to bring submodules.
    pub fn set_options(&mut self, options: CloneOptions, cx: &mut Context<Self>) {
        self.options = options;
        cx.notify();
    }

    #[must_use]
    pub fn is_running(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }

    /// Where the clone would go, or why it cannot start.
    pub fn destination(&self, cx: &gpui_kit::App) -> Result<PathBuf, &'static str> {
        let url = self.url.read(cx).value();
        if url.trim().is_empty() {
            return Err("Enter the repository's URL.");
        }
        let name = self.name.read(cx).value();
        let name = name.trim();
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err("Enter a folder name.");
        }
        let destination = self.parent.join(name);
        if !is_empty_or_missing(&destination) {
            return Err("A folder with this name already exists there.");
        }
        Ok(destination)
    }

    fn clear_failure(&mut self) {
        if matches!(self.phase, Phase::Failed(_)) {
            self.phase = Phase::Editing;
        }
    }

    /// Starts the clone when the form is valid.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        if self.is_running() {
            return;
        }
        let Ok(destination) = self.destination(cx) else {
            return;
        };
        let url = self.url.read(cx).value().trim().to_owned();
        let cancel = CancelToken::new();
        self.phase = Phase::Running {
            progress: None,
            cancel: cancel.clone(),
        };
        cx.notify();

        let (sender, mut receiver) = mpsc::unbounded();
        let git = self.git.clone();
        let options = self.options;
        cx.background_spawn(async move {
            let progress = sender.clone();
            let result = Repository::clone_with(
                &git,
                &url,
                &destination,
                options,
                move |update| {
                    let _ = progress.unbounded_send(Message::Progress(update));
                },
                Some(cancel),
            )
            .map(|repository| repository.root().to_path_buf())
            .map_err(|error| error.to_string());
            let _ = sender.unbounded_send(Message::Done(result));
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Some(message) = receiver.next().await {
                let Ok(()) = this.update(cx, |this, cx| this.receive(message, cx)) else {
                    return;
                };
            }
        })
        .detach();
    }

    fn receive(&mut self, message: Message, cx: &mut Context<Self>) {
        match message {
            Message::Progress(update) => {
                if let Phase::Running { progress, .. } = &mut self.phase {
                    *progress = Some(update);
                }
            }
            Message::Done(Ok(path)) => {
                self.phase = Phase::Editing;
                cx.emit(CloneEvent::Cloned(path));
            }
            Message::Done(Err(error)) => {
                // A cancelled clone goes back to the form quietly.
                self.phase = if matches!(self.phase, Phase::Running { ref cancel, .. } if cancel.is_cancelled())
                {
                    Phase::Editing
                } else {
                    Phase::Failed(error.into())
                };
            }
        }
        cx.notify();
    }

    /// Stops a running clone; its partial folder is removed.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Phase::Running { cancel, .. } = &self.phase {
            cancel.cancel();
        }
        cx.notify();
    }

    fn choose_parent(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(parent) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.parent.clone_from(&parent);
                this.clear_failure();
                cx.emit(CloneEvent::ParentChanged(parent));
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for CloneDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let running = self.is_running();
        let validity = self.destination(cx);
        let field_label = |text: &'static str| {
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(text)
        };
        let status = match &self.phase {
            Phase::Running { progress, .. } => {
                let (value, label) =
                    progress
                        .as_ref()
                        .map_or((0., "Connecting…".to_owned()), |p| {
                            let label = match (p.done, p.total) {
                                (Some(done), Some(total)) => {
                                    format!("{} {done} of {total}", p.phase)
                                }
                                _ => p.phase.clone(),
                            };
                            (f64::from(p.overall().unwrap_or(0.)) * 100., label)
                        });
                Some(
                    Progress::new("clone.progress")
                        .value(value)
                        .label(label)
                        .into_any_element(),
                )
            }
            Phase::Failed(error) => Some(
                div()
                    .id("clone.error")
                    .max_h(px(160.))
                    .overflow_y_scroll()
                    .text_sm()
                    .text_color(c.destructive)
                    .child(error.clone())
                    .into_any_element(),
            ),
            Phase::Editing => None,
        };
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(dialog_title("clone.title", cx).child("Clone Repository"))
            .child(dialog_description("clone.description", cx).child(
                "Any HTTPS or SSH URL works, from GitHub, GitLab, Bitbucket or your own server.",
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(field_label("URL"))
                    .child(Input::new(&self.url).aria_label("Repository URL")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(field_label("Clone into"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(c.muted_foreground)
                                    .child(self.parent.to_string_lossy().into_owned()),
                            )
                            .child(
                                Button::new("clone.choose")
                                    .variant(ButtonVariant::Outline)
                                    .label("Choose…")
                                    .disabled(running)
                                    .on_click(cx.listener(|this, _, _, cx| this.choose_parent(cx))),
                            ),
                    )
                    .child(Input::new(&self.name).aria_label("Folder name")),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .text_sm()
                    .child(option_row(
                        Checkbox::new("clone.shallow")
                            .checked(self.options.shallow)
                            .disabled(running)
                            .aria_label("Recent history only")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.options.shallow = *checked;
                                cx.notify();
                            })),
                        "Recent history only",
                        "Faster for big repositories. History can fetch the rest later.",
                        c.muted_foreground,
                    ))
                    .child(option_row(
                        Checkbox::new("clone.submodules")
                            .checked(self.options.submodules)
                            .disabled(running)
                            .aria_label("Include submodules")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.options.submodules = *checked;
                                cx.notify();
                            })),
                        "Include submodules",
                        "Clones the repositories this one depends on, too.",
                        c.muted_foreground,
                    )),
            )
            .when_some(status, gpui_kit::ParentElement::child)
            .when(!running, |this| {
                this.when_some(
                    validity
                        .as_ref()
                        .err()
                        .filter(|_| !self.url.read(cx).value().is_empty()),
                    |this, reason| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(c.muted_foreground)
                                .child(*reason),
                        )
                    },
                )
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("clone.cancel")
                            .variant(ButtonVariant::Outline)
                            .label(if running { "Stop" } else { "Cancel" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.is_running() {
                                    this.cancel(cx);
                                } else {
                                    cx.emit(CloneEvent::Dismissed);
                                }
                            })),
                    )
                    .child(
                        Button::new("clone.start")
                            .label(if running { "Cloning…" } else { "Clone" })
                            .disabled(running || validity.is_err())
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    ),
            )
    }
}

/// A checkbox with a label and a quieter line explaining it.
fn option_row(
    checkbox: Checkbox,
    label: &'static str,
    detail: &'static str,
    muted: gpui_kit::Rgba,
) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap(px(8.))
        .child(div().pt(px(2.)).child(checkbox))
        .child(
            div()
                .flex()
                .flex_col()
                .child(label)
                .child(div().text_xs().text_color(muted).child(detail)),
        )
}
