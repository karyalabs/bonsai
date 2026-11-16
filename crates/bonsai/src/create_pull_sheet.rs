//! The New Pull Request sheet (CONTEXT.md: Pull request): pick the base,
//! write a title and description, and create it on the provider. A branch
//! with commits the remote lacks is pushed first, with its upstream set.

use std::sync::Arc;

use bonsai_git::{Head, PushRequest, Snapshot};
use bonsai_keychain::Secret;
use bonsai_providers::{HttpClient, NewPullRequest, PullRequest};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    accounts::{Accounts, provider},
    hosted::Target,
    repository::{RepositoryEvent, RepositoryModel},
    ui::{
        button::{Button, ButtonVariant},
        checkbox::Checkbox,
        dialog::{dialog_description, dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        textarea::{Textarea, TextareaState},
        theme::UiTheme,
    },
};

pub enum CreatePullEvent {
    Dismissed,
    /// Created; the list shows it.
    Created(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Form,
    Pushing,
    Creating,
    Done(Box<PullRequest>),
}

/// A title from a branch name: `feature/sync-bar` reads "Sync bar".
#[must_use]
pub fn title_from_branch(branch: &str) -> String {
    let last = branch.rsplit('/').next().unwrap_or(branch);
    let words = last.replace(['-', '_'], " ");
    let mut chars = words.trim().chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The branch a pull request most likely merges into: the remote's `main`,
/// `master`, `develop` or `trunk`, in that order.
#[must_use]
pub fn default_base(snapshot: &Snapshot, remote: &str, head: &str) -> String {
    ["main", "master", "develop", "trunk"]
        .into_iter()
        .find(|name| {
            *name != head
                && snapshot
                    .remote_branches
                    .iter()
                    .any(|b| b.remote == remote && b.name == *name)
        })
        .unwrap_or("main")
        .to_owned()
}

/// Whether `head` has commits the remote lacks: it has no upstream on
/// `remote`, or is ahead of it.
#[must_use]
pub fn needs_push(snapshot: &Snapshot, remote: &str, head: &str) -> bool {
    let tracked = snapshot
        .branches
        .iter()
        .find(|b| b.name == head)
        .and_then(|b| b.upstream.as_ref());
    match tracked {
        Some(upstream) => {
            upstream.gone || upstream.ahead > 0 || !upstream.name.starts_with(&format!("{remote}/"))
        }
        None => true,
    }
}

pub struct CreatePullSheet {
    model: Entity<RepositoryModel>,
    target: Target,
    token: Option<Secret>,
    client: Arc<dyn HttpClient>,
    head: Option<String>,
    base: Entity<InputState>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    draft: bool,
    phase: Phase,
    error: Option<SharedString>,
    /// The push queued before creating, by its operation name.
    push: Option<SharedString>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<CreatePullEvent> for CreatePullSheet {}

impl CreatePullSheet {
    pub fn new(
        model: Entity<RepositoryModel>,
        target: Target,
        accounts: &Accounts,
        client: Arc<dyn HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let snapshot = model.read(cx).snapshot().cloned();
        let head = match snapshot.as_ref().map(|s| &s.head) {
            Some(Head::Branch { name, oid: Some(_) }) => Some(name.clone()),
            _ => None,
        };
        let base_name = match (&snapshot, &head) {
            (Some(snapshot), Some(head)) => default_base(snapshot, &target.remote, head),
            _ => "main".to_owned(),
        };
        let base = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Base branch");
            input.set_value(base_name, window, cx);
            input
        });
        let title = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Title");
            input.set_value(
                head.as_deref().map(title_from_branch).unwrap_or_default(),
                window,
                cx,
            );
            input
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(6)
                .placeholder("Description (Markdown)")
        });
        let mut subscriptions: Vec<Subscription> = [&base, &title]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        subscriptions.push(
            cx.subscribe(&model, |this, _, event: &RepositoryEvent, cx| {
                if let RepositoryEvent::OperationFinished { name, error, .. } = event
                    && this.push.as_ref() == Some(name)
                {
                    this.push = None;
                    match error {
                        None => this.create(cx),
                        Some(error) => {
                            this.error = Some(error.clone());
                            this.phase = Phase::Form;
                            cx.notify();
                        }
                    }
                }
            }),
        );
        let token = accounts
            .token(&target.connected)
            .or_else(|| crate::http::replaying().then(|| Secret::new("")));
        Self {
            model,
            target,
            token,
            client,
            head,
            base,
            title,
            body,
            draft: false,
            phase: Phase::Form,
            error: None,
            push: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    #[must_use]
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn set_draft(&mut self, draft: bool, cx: &mut Context<Self>) {
        self.draft = draft;
        cx.notify();
    }

    fn value(input: &Entity<InputState>, cx: &gpui_kit::App) -> String {
        input.read(cx).value().trim().to_owned()
    }

    #[must_use]
    pub fn can_submit(&self, cx: &gpui_kit::App) -> bool {
        let base = Self::value(&self.base, cx);
        self.phase == Phase::Form
            && self.head.as_deref().is_some_and(|head| head != base)
            && !base.is_empty()
            && !Self::value(&self.title, cx).is_empty()
    }

    /// Pushes the branch when the remote lacks commits, then creates the
    /// pull request.
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let Some(head) = self.head.clone() else {
            return;
        };
        if self.token.is_none() {
            self.error = Some("This account has no token. Sign in again in Accounts.".into());
            cx.notify();
            return;
        }
        self.error = None;
        let push = self
            .model
            .read(cx)
            .snapshot()
            .is_some_and(|s| needs_push(s, &self.target.remote, &head));
        if !push {
            self.create(cx);
            return;
        }
        let name: SharedString = format!("Push {head}").into();
        let remote = self.target.remote.clone();
        self.push = Some(name.clone());
        self.phase = Phase::Pushing;
        self.model.update(cx, |model, cx| {
            model.run_network(
                name,
                move |repository, progress, cancel| {
                    repository.push(
                        &PushRequest::new(remote, head).set_upstream(),
                        Some(progress),
                        Some(cancel),
                    )
                },
                cx,
            );
        });
        cx.notify();
    }

    fn create(&mut self, cx: &mut Context<Self>) {
        let (Some(head), Some(token)) = (self.head.clone(), self.token.clone()) else {
            return;
        };
        let new = NewPullRequest {
            title: Self::value(&self.title, cx),
            body: self.body.read(cx).value().trim().to_owned(),
            head,
            base: Self::value(&self.base, cx),
            draft: self.draft,
        };
        let connected = &self.target.connected;
        let provider = provider(
            connected.account.kind,
            &connected.account.host,
            self.client.clone(),
        );
        let api = connected.api_token(&token);
        let full_name = self.target.full_name.clone();
        self.phase = Phase::Creating;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider.create_pull_request(&api, &full_name, &new).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(pull) => {
                        cx.emit(CreatePullEvent::Created(pull.number));
                        this.phase = Phase::Done(Box::new(pull));
                    }
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        this.phase = Phase::Form;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_form(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let busy = matches!(self.phase, Phase::Pushing | Phase::Creating);
        let pushes = self.head.as_deref().is_some_and(|head| {
            self.model
                .read(cx)
                .snapshot()
                .is_some_and(|s| needs_push(s, &self.target.remote, head))
        });
        let description = match &self.head {
            Some(head) if pushes => format!(
                "Pushes {head} to {}, then opens a pull request on {}.",
                self.target.remote, self.target.full_name
            ),
            Some(_) => format!("Opens a pull request on {}.", self.target.full_name),
            None => "Switch to a branch with commits to open a pull request.".to_owned(),
        };
        let label = |text: &'static str| {
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(c.muted_foreground)
                .child(text)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(dialog_title("create-pull.title", cx).child("New Pull Request"))
            .child(dialog_description("create-pull.description", cx).child(description))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(label("Merge into"))
                            .child(Input::new(&self.base).aria_label("Base branch")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(label("From"))
                            .child(
                                div()
                                    .h(px(32.))
                                    .px(px(10.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(8.))
                                    .bg(c.muted)
                                    .text_sm()
                                    .font_family(theme.fonts.mono.clone())
                                    .truncate()
                                    .child(self.head.clone().unwrap_or_else(|| "—".into())),
                            ),
                    ),
            )
            .child(label("Title"))
            .child(Input::new(&self.title).aria_label("Title"))
            .child(label("Description"))
            .child(Textarea::new(&self.body).aria_label("Description"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_sm()
                    .child(
                        Checkbox::new("create-pull.draft")
                            .checked(self.draft)
                            .aria_label("Create as draft")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.set_draft(*checked, cx);
                            })),
                    )
                    .child("Create as draft"),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(c.destructive).child(error))
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("create-pull.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .disabled(busy)
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(CreatePullEvent::Dismissed)),
                            ),
                    )
                    .child(
                        Button::new("create-pull.confirm")
                            .label(match self.phase {
                                Phase::Pushing => "Pushing…",
                                Phase::Creating => "Creating…",
                                _ if pushes => "Push and Create",
                                _ => "Create Pull Request",
                            })
                            .disabled(!self.can_submit(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for CreatePullSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let Phase::Done(pull) = self.phase.clone() else {
            return self.render_form(cx);
        };
        let web = pull.web_url.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                dialog_title("create-pull.done.title", cx)
                    .child(format!("Opened #{}", pull.number)),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.colors.muted_foreground)
                    .child(format!("{} → {}: {}", pull.head, pull.base, pull.title)),
            )
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("create-pull.web")
                            .variant(ButtonVariant::Outline)
                            .label("Open in Browser")
                            .on_click(move |_, _, cx| cx.open_url(&web)),
                    )
                    .child(
                        Button::new("create-pull.done").label("Done").on_click(
                            cx.listener(|_, _, _, cx| cx.emit(CreatePullEvent::Dismissed)),
                        ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::title_from_branch;

    #[test]
    fn titles_come_from_branch_names() {
        assert_eq!(title_from_branch("feature/sync-bar"), "Sync bar");
        assert_eq!(title_from_branch("fix_empty_repo"), "Fix empty repo");
        assert_eq!(title_from_branch("main"), "Main");
        assert_eq!(title_from_branch(""), "");
    }
}
