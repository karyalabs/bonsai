//! The New Issue sheet: a title and a Markdown description, created on the
//! repository's hosted remote.

use std::sync::Arc;

use bonsai_keychain::Secret;
use bonsai_providers::{HttpClient, Issue, NewIssue};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    accounts::{Accounts, provider},
    hosted::Target,
    ui::{
        button::{Button, ButtonVariant},
        dialog::{dialog_description, dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        textarea::{Textarea, TextareaState},
        theme::UiTheme,
    },
};

pub enum NewIssueEvent {
    Dismissed,
    Created(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Form,
    Creating,
    Done(Box<Issue>),
}

pub struct NewIssueSheet {
    target: Target,
    token: Option<Secret>,
    client: Arc<dyn HttpClient>,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    phase: Phase,
    error: Option<SharedString>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<NewIssueEvent> for NewIssueSheet {}

impl NewIssueSheet {
    pub fn new(
        target: Target,
        accounts: &Accounts,
        client: Arc<dyn HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .rows(8)
                .placeholder("Description (Markdown)")
        });
        let subscriptions =
            vec![
                cx.subscribe(&title, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.submit(cx),
                    _ => {}
                }),
            ];
        let token = accounts
            .token(&target.connected)
            .or_else(|| crate::http::replaying().then(|| Secret::new("")));
        Self {
            target,
            token,
            client,
            title,
            body,
            phase: Phase::Form,
            error: None,
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

    #[must_use]
    pub fn title_input(&self) -> &Entity<InputState> {
        &self.title
    }

    #[must_use]
    pub fn can_submit(&self, cx: &gpui_kit::App) -> bool {
        self.phase == Phase::Form && !self.title.read(cx).value().trim().is_empty()
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.can_submit(cx) {
            return;
        }
        let Some(token) = self.token.clone() else {
            self.error = Some("This account has no token. Sign in again in Accounts.".into());
            cx.notify();
            return;
        };
        let new = NewIssue {
            title: self.title.read(cx).value().trim().to_owned(),
            body: self.body.read(cx).value().trim().to_owned(),
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
        self.error = None;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider.create_issue(&api, &full_name, &new).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(issue) => {
                        cx.emit(NewIssueEvent::Created(issue.number));
                        this.phase = Phase::Done(Box::new(issue));
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
}

impl Render for NewIssueSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        if let Phase::Done(issue) = self.phase.clone() {
            let web = issue.web_url.clone();
            return div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    dialog_title("new-issue.done.title", cx)
                        .child(format!("Opened #{}", issue.number)),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child(issue.title.clone()),
                )
                .child(
                    dialog_footer(cx)
                        .child(
                            Button::new("new-issue.web")
                                .variant(ButtonVariant::Outline)
                                .label("Open in Browser")
                                .on_click(move |_, _, cx| cx.open_url(&web)),
                        )
                        .child(Button::new("new-issue.done").label("Done").on_click(
                            cx.listener(|_, _, _, cx| cx.emit(NewIssueEvent::Dismissed)),
                        )),
                )
                .into_any_element();
        }
        let creating = self.phase == Phase::Creating;
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(dialog_title("new-issue.title", cx).child("New Issue"))
            .child(
                dialog_description("new-issue.description", cx)
                    .child(format!("Opens an issue on {}.", self.target.full_name)),
            )
            .child(Input::new(&self.title).aria_label("Title"))
            .child(Textarea::new(&self.body).aria_label("Description"))
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(c.destructive).child(error))
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("new-issue.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .disabled(creating)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(NewIssueEvent::Dismissed))),
                    )
                    .child(
                        Button::new("new-issue.confirm")
                            .label(if creating {
                                "Creating…"
                            } else {
                                "Create Issue"
                            })
                            .disabled(!self.can_submit(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                    ),
            )
            .into_any_element()
    }
}
