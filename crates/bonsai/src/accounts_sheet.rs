//! The Accounts sheet: connected accounts with their status, signing in
//! with a token (or GitHub's device flow when Bonsai has a client ID), and
//! signing out.

use std::{collections::HashMap, sync::Arc, time::Duration};

use bonsai_keychain::{Secret, TokenKey};
use bonsai_providers::{DeviceCode, DevicePoll, Error, GitHub, HttpClient, ProviderKind};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, EventEmitter, IntoElement, ParentElement as _,
    Render, SharedString, Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    accounts::{Accounts, Connected, SignInMethod, default_host, provider},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        dialog::{dialog_description, dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

/// Bonsai's GitHub OAuth app, set at build time. Without it, GitHub signs
/// in with a personal access token only.
pub const GITHUB_CLIENT_ID: Option<&str> = option_env!("BONSAI_GITHUB_CLIENT_ID");

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Checking,
    Connected,
    /// The token was revoked or expired: sign in again.
    Expired,
    /// The token is gone from the Keychain.
    Missing,
    Unreachable(SharedString),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    List,
    Add,
    Device(DeviceCode),
    Working(SharedString),
}

pub enum AccountsEvent {
    Dismissed,
}

pub struct AccountsSheet {
    accounts: Accounts,
    client: Arc<dyn HttpClient>,
    statuses: HashMap<TokenKey, Status>,
    phase: Phase,
    kind: ProviderKind,
    host: Entity<InputState>,
    email: Entity<InputState>,
    /// The pasted token, never shown.
    token: Option<Secret>,
    error: Option<SharedString>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AccountsEvent> for AccountsSheet {}

impl AccountsSheet {
    pub fn new(
        accounts: Accounts,
        client: Arc<dyn HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let host = cx.new(|cx| InputState::new(window, cx).placeholder("github.com"));
        let email = cx.new(|cx| InputState::new(window, cx).placeholder("Atlassian account email"));
        let subscriptions = [&host, &email]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                })
            })
            .collect();
        let mut this = Self {
            accounts,
            client,
            statuses: HashMap::new(),
            phase: Phase::List,
            kind: ProviderKind::GitHub,
            host,
            email,
            token: None,
            error: None,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        this.check_all(cx);
        this
    }

    #[must_use]
    pub fn accounts(&self) -> &[Connected] {
        self.accounts.list()
    }

    #[must_use]
    pub fn status(&self, key: &TokenKey) -> Option<&Status> {
        self.statuses.get(key)
    }

    #[must_use]
    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    #[must_use]
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Checks every account's token against its provider.
    pub fn check_all(&mut self, cx: &mut Context<Self>) {
        for connected in self.accounts.list().to_vec() {
            self.check(connected, cx);
        }
    }

    fn check(&mut self, connected: Connected, cx: &mut Context<Self>) {
        let key = connected.key();
        let Some(token) = self.accounts.token(&connected) else {
            self.statuses.insert(key, Status::Missing);
            return;
        };
        self.statuses.insert(key.clone(), Status::Checking);
        let provider = provider(
            connected.account.kind,
            &connected.account.host,
            self.client.clone(),
        );
        let api = connected.api_token(&token);
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider.current_user(&api).await;
            let _ = this.update(cx, |this, cx| {
                let status = match result {
                    Ok(_) => Status::Connected,
                    Err(Error::Unauthorized(_)) => Status::Expired,
                    Err(error) => Status::Unreachable(error.to_string().into()),
                };
                this.statuses.insert(key, status);
                cx.notify();
            });
        }));
    }

    pub fn show_add(&mut self, kind: ProviderKind, window: &mut Window, cx: &mut Context<Self>) {
        self.kind = kind;
        self.phase = Phase::Add;
        self.token = None;
        self.error = None;
        self.host.update(cx, |input, cx| {
            input.set_value(default_host(kind), window, cx);
        });
        cx.notify();
    }

    pub fn set_token(&mut self, token: Secret, cx: &mut Context<Self>) {
        self.token = Some(token);
        self.error = None;
        cx.notify();
    }

    fn paste_token(&mut self, cx: &mut Context<Self>) {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .map(|t| t.trim().to_owned())
            .unwrap_or_default();
        if text.is_empty() || text.contains(char::is_whitespace) {
            self.error = Some("The clipboard doesn't hold a token.".into());
            cx.notify();
        } else {
            self.set_token(Secret::new(text), cx);
        }
    }

    fn host_value(&self, cx: &gpui_kit::App) -> String {
        let host = self.host.read(cx).value().trim().to_ascii_lowercase();
        let host = host
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_owned();
        if host.is_empty() {
            default_host(self.kind).to_owned()
        } else {
            host
        }
    }

    fn email_value(&self, cx: &gpui_kit::App) -> String {
        self.email.read(cx).value().trim().to_owned()
    }

    #[must_use]
    pub fn can_sign_in(&self, cx: &gpui_kit::App) -> bool {
        self.token.is_some()
            && (self.kind != ProviderKind::Bitbucket || self.email_value(cx).contains('@'))
    }

    /// Checks the pasted token with the provider and saves the account.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if !self.can_sign_in(cx) {
            return;
        }
        let Some(token) = self.token.clone() else {
            return;
        };
        let kind = self.kind;
        let host = self.host_value(cx);
        let method = if kind == ProviderKind::Bitbucket {
            SignInMethod::ApiToken {
                email: self.email_value(cx),
            }
        } else {
            SignInMethod::Token
        };
        self.finish_sign_in(kind, host, method, token, cx);
    }

    fn finish_sign_in(
        &mut self,
        kind: ProviderKind,
        host: String,
        method: SignInMethod,
        token: Secret,
        cx: &mut Context<Self>,
    ) {
        self.phase = Phase::Working(format!("Checking the token with {}…", kind.name()).into());
        self.error = None;
        let provider = provider(kind, &host, self.client.clone());
        // The placeholder account only shapes the API credential.
        let api = match &method {
            SignInMethod::ApiToken { email } => format!("{email}:{}", token.expose()),
            _ => token.expose().to_owned(),
        };
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider.current_user(&api).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(account) => {
                        let connected = Connected { account, method };
                        match this.accounts.add(connected.clone(), &token) {
                            Ok(()) => {
                                this.statuses.insert(connected.key(), Status::Connected);
                                this.phase = Phase::List;
                                this.token = None;
                            }
                            Err(error) => {
                                this.error = Some(error.into());
                                this.phase = Phase::Add;
                            }
                        }
                    }
                    Err(error) => {
                        this.error = Some(match error {
                            Error::Unauthorized(_) => {
                                "That token was refused. Check it hasn't expired and has the scopes listed above.".into()
                            }
                            other => other.to_string().into(),
                        });
                        this.phase = Phase::Add;
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// GitHub's device flow: show a code, open the browser, and poll.
    pub fn start_device_flow(&mut self, cx: &mut Context<Self>) {
        let Some(client_id) = GITHUB_CLIENT_ID else {
            return;
        };
        let host = self.host_value(cx);
        let client = self.client.clone();
        self.phase = Phase::Working("Asking GitHub for a sign-in code…".into());
        self.error = None;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let github = if host == "github.com" {
                GitHub::new(client)
            } else {
                GitHub::enterprise(client, &host)
            };
            let code = match github.start_device_flow(client_id).await {
                Ok(code) => code,
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(error.to_string().into());
                        this.phase = Phase::Add;
                        cx.notify();
                    });
                    return;
                }
            };
            let mut interval = code.interval.max(1);
            let _ = this.update(cx, |this, cx| {
                cx.open_url(&code.verification_uri);
                this.phase = Phase::Device(code.clone());
                cx.notify();
            });
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(interval))
                    .await;
                // Stop when the user left this step.
                let waiting = this
                    .update(cx, |this, _| matches!(this.phase, Phase::Device(_)))
                    .unwrap_or(false);
                if !waiting {
                    return;
                }
                let outcome = match github.poll_device(client_id, &code.device_code).await {
                    Ok(DevicePoll::Pending) => continue,
                    Ok(DevicePoll::SlowDown(seconds)) => {
                        interval = seconds;
                        continue;
                    }
                    Ok(DevicePoll::Token(token)) => Ok(token),
                    Ok(DevicePoll::Expired) => Err("The code expired. Start again.".to_owned()),
                    Ok(DevicePoll::Denied) => {
                        Err("The sign-in was cancelled on GitHub.".to_owned())
                    }
                    Err(error) => Err(error.to_string()),
                };
                let _ = this.update(cx, |this, cx| match outcome {
                    Ok(token) => this.finish_sign_in(
                        ProviderKind::GitHub,
                        host.clone(),
                        SignInMethod::Token,
                        Secret::new(token),
                        cx,
                    ),
                    Err(message) => {
                        this.error = Some(message.into());
                        this.phase = Phase::Add;
                        cx.notify();
                    }
                });
                return;
            }
        }));
        cx.notify();
    }

    pub fn sign_out(&mut self, key: &TokenKey, cx: &mut Context<Self>) {
        if let Err(error) = self.accounts.remove(key) {
            self.error = Some(error.into());
        }
        self.statuses.remove(key);
        cx.notify();
    }

    fn token_page(&self, cx: &gpui_kit::App) -> (String, &'static str) {
        let host = self.host_value(cx);
        match self.kind {
            ProviderKind::GitHub => (
                format!(
                    "https://{host}/settings/tokens/new?scopes=repo,read:org&description=Bonsai"
                ),
                "The page opens with the scopes Bonsai needs, repo and read:org, already ticked.",
            ),
            ProviderKind::GitLab => (
                format!(
                    "https://{host}/-/user_settings/personal_access_tokens?name=Bonsai&scopes=api,read_user,write_repository"
                ),
                "The page opens with the scopes Bonsai needs, api, read_user and write_repository, already ticked.",
            ),
            ProviderKind::Bitbucket => (
                "https://id.atlassian.com/manage-profile/security/api-tokens".to_owned(),
                "Choose Create API token with scopes, then allow reading and writing repositories.",
            ),
        }
    }

    fn render_list(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let mut cards = div().flex().flex_col().gap(px(10.));
        for kind in [
            ProviderKind::GitHub,
            ProviderKind::GitLab,
            ProviderKind::Bitbucket,
        ] {
            cards = cards.child(self.render_provider(kind, &theme, cx));
        }
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(dialog_title("accounts.title", cx).child("Accounts"))
            .child(dialog_description("accounts.description", cx).child(
                "Browse, clone and publish repositories with your accounts. Git uses an account's token for HTTPS remotes on its host.",
            ))
            .child(cards)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(theme.colors.muted_foreground)
                    .child(
                        lucide(LucideIcon::LockKeyhole)
                            .size(px(12.))
                            .text_color(theme.colors.muted_foreground),
                    )
                    .child("Tokens stay in your Keychain."),
            )
            .child(
                dialog_footer(cx).child(
                    Button::new("accounts.done")
                        .label("Done")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountsEvent::Dismissed))),
                ),
            )
            .into_any_element()
    }

    /// One provider's card: its mark, what it covers, its connected accounts
    /// and a button to add one.
    fn render_provider(
        &self,
        kind: ProviderKind,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let c = &theme.colors;
        let (mark, color, caption) = match kind {
            ProviderKind::GitHub => (
                "GH",
                gpui_kit::rgb(0x0024_292f),
                "github.com or GitHub Enterprise",
            ),
            ProviderKind::GitLab => (
                "GL",
                gpui_kit::rgb(0x00e2_4329),
                "gitlab.com or a self-managed GitLab",
            ),
            ProviderKind::Bitbucket => ("BB", gpui_kit::rgb(0x0000_52cc), "bitbucket.org"),
        };
        // GitHub's near-black mark would vanish on a dark card.
        let github_inverted =
            kind == ProviderKind::GitHub && theme.mode == crate::ui::theme::ThemeMode::Dark;
        let connected: Vec<(usize, &Connected)> = self
            .accounts
            .list()
            .iter()
            .enumerate()
            .filter(|(_, connected)| connected.account.kind == kind)
            .collect();
        let header = div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div()
                    .size(px(32.))
                    .flex_none()
                    .rounded(px(8.))
                    .bg(if github_inverted {
                        gpui_kit::rgb(0x00f0_f6fc)
                    } else {
                        color
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .text_color(if github_inverted {
                        color
                    } else {
                        gpui_kit::rgb(0x00ff_ffff)
                    })
                    .child(mark),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(kind.name()),
                    )
                    .child(div().text_xs().text_color(c.muted_foreground).child(
                        if connected.is_empty() {
                            caption.to_owned()
                        } else if connected.len() == 1 {
                            "1 account".to_owned()
                        } else {
                            format!("{} accounts", connected.len())
                        },
                    )),
            )
            .child(
                Button::new(SharedString::from(format!("accounts.add.{}", kind.name())))
                    .variant(if connected.is_empty() {
                        ButtonVariant::Default
                    } else {
                        ButtonVariant::Outline
                    })
                    .size(ButtonSize::Sm)
                    .label(if connected.is_empty() {
                        "Connect"
                    } else {
                        "Add Another"
                    })
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.show_add(kind, window, cx)),
                    ),
            );
        let mut card = div()
            .flex()
            .flex_col()
            .rounded(px(10.))
            .border_1()
            .border_color(c.border)
            .child(div().p(px(12.)).child(header));
        for (i, connected) in connected {
            card = card.child(self.render_account(i, connected, theme, cx));
        }
        card
    }

    fn render_account(
        &self,
        i: usize,
        connected: &Connected,
        theme: &UiTheme,
        cx: &mut Context<Self>,
    ) -> gpui_kit::Div {
        let c = &theme.colors;
        let key = connected.key();
        let account = &connected.account;
        let (status, color, icon) = match self.statuses.get(&key) {
            Some(Status::Connected) => ("Connected".to_owned(), c.success, LucideIcon::CircleCheck),
            Some(Status::Expired) => (
                "Token expired".to_owned(),
                c.destructive,
                LucideIcon::CircleAlert,
            ),
            Some(Status::Missing) => (
                "Token missing".to_owned(),
                c.destructive,
                LucideIcon::CircleAlert,
            ),
            Some(Status::Unreachable(e)) => (e.to_string(), c.warning, LucideIcon::TriangleAlert),
            Some(Status::Checking) | None => (
                "Checking…".to_owned(),
                c.muted_foreground,
                LucideIcon::LoaderCircle,
            ),
        };
        let needs_sign_in = matches!(
            self.statuses.get(&key),
            Some(Status::Expired | Status::Missing)
        );
        let name = account
            .display_name
            .clone()
            .unwrap_or_else(|| account.username.clone());
        let kind = account.kind;
        let host = account.host.clone();
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(12.))
            .py(px(10.))
            .border_t_1()
            .border_color(c.border)
            .child(crate::avatar::avatar(
                &name,
                &format!("{}@{}", account.username, account.host),
                px(28.),
                theme.mode == crate::ui::theme::ThemeMode::Dark,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(px(6.))
                            .child(div().text_sm().child(name.clone()))
                            .when(name != account.username, |this| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(c.muted_foreground)
                                        .child(format!("@{}", account.username)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .child(lucide(icon).size(px(11.)).flex_none().text_color(color))
                            .child(div().text_color(color).child(status))
                            .child(format!("· {}", account.host)),
                    ),
            )
            .when(needs_sign_in, |this| {
                this.child(
                    Button::new(("accounts.again", i))
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Sign In")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_add(kind, window, cx);
                            this.host.update(cx, |input, cx| {
                                input.set_value(host.clone(), window, cx);
                            });
                        })),
                )
            })
            .child(
                Button::new(("accounts.sign-out", i))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .label("Sign Out")
                    .on_click(cx.listener(move |this, _, _, cx| this.sign_out(&key, cx))),
            )
    }

    fn render_add(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let (page, scopes) = self.token_page(cx);
        let kind = self.kind;
        let pasted = self
            .token
            .as_ref()
            .map(|token| token.expose().chars().count());
        let device = kind == ProviderKind::GitHub && GITHUB_CLIENT_ID.is_some();
        let step = |n: usize, done: bool, title: String, body: gpui_kit::AnyElement| {
            div()
                .flex()
                .gap(px(12.))
                .child(
                    div()
                        .size(px(22.))
                        .flex_none()
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .when(done, |this| {
                            this.bg(c.success).child(
                                lucide(LucideIcon::Check)
                                    .size(px(12.))
                                    .text_color(c.success_foreground),
                            )
                        })
                        .when(!done, |this| {
                            this.bg(c.muted)
                                .text_color(c.foreground)
                                .child(n.to_string())
                        }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(body),
                )
        };
        let where_field = if kind == ProviderKind::Bitbucket {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child("Atlassian account email"),
                )
                .child(Input::new(&self.email).aria_label("Email"))
        } else {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .child(div().text_xs().text_color(c.muted_foreground).child("Host"))
                .child(Input::new(&self.host).aria_label("Host"))
        };
        let create = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(div().text_xs().text_color(c.muted_foreground).child(scopes))
            .child(
                div().flex().child(
                    Button::new("accounts.create-token")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label(format!("Open {} Token Page", kind.name()))
                        .on_click(move |_, _, cx| cx.open_url(&page)),
                ),
            );
        let paste = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(
                Button::new("accounts.paste")
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label(if pasted.is_some() {
                        "Paste Again"
                    } else {
                        "Paste Token"
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.paste_token(cx))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(
                        lucide(LucideIcon::KeyRound)
                            .size(px(12.))
                            .text_color(c.muted_foreground),
                    )
                    .child(match pasted {
                        Some(count) => format!("Token pasted, {count} characters, hidden"),
                        None => "Bonsai reads it from the clipboard".to_owned(),
                    }),
            );
        div()
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(dialog_title("accounts.add.title", cx).child(format!("Connect {}", kind.name())))
            .child(
                dialog_description("accounts.add.description", cx).child(format!(
                    "Bonsai signs in to {} with {} and keeps it in your Keychain.",
                    kind.name(),
                    if kind == ProviderKind::Bitbucket {
                        "an API token"
                    } else {
                        "a personal access token"
                    }
                )),
            )
            .child(where_field)
            .child(step(
                1,
                false,
                format!("Create a token on {}", kind.name()),
                create.into_any_element(),
            ))
            .child(step(
                2,
                pasted.is_some(),
                "Copy it, then paste it here".to_owned(),
                paste.into_any_element(),
            ))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .text_color(c.destructive)
                        .child(
                            lucide(LucideIcon::CircleAlert)
                                .size(px(12.))
                                .text_color(c.destructive),
                        )
                        .child(error),
                )
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("accounts.back")
                            .variant(ButtonVariant::Outline)
                            .label("Back")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.phase = Phase::List;
                                this.token = None;
                                this.error = None;
                                cx.notify();
                            })),
                    )
                    .when(device, |this| {
                        this.child(
                            Button::new("accounts.browser")
                                .variant(ButtonVariant::Outline)
                                .label("Use Browser")
                                .on_click(cx.listener(|this, _, _, cx| this.start_device_flow(cx))),
                        )
                    })
                    .child(
                        Button::new("accounts.sign-in")
                            .label("Sign In")
                            .disabled(!self.can_sign_in(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.sign_in(cx))),
                    ),
            )
            .into_any_element()
    }

    fn render_device(&self, code: &DeviceCode, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let user_code = code.user_code.clone();
        let uri = code.verification_uri.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(dialog_title("accounts.device.title", cx).child("Finish in Your Browser"))
            .child(
                dialog_description("accounts.device.description", cx)
                    .child("Enter this code on GitHub. Bonsai signs in as soon as you approve it."),
            )
            .child(
                div()
                    .font_family(theme.fonts.mono.clone())
                    .text_xl()
                    .child(code.user_code.clone()),
            )
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("accounts.device.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.phase = Phase::Add;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("accounts.device.copy")
                            .variant(ButtonVariant::Outline)
                            .label("Copy Code")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(user_code.clone()));
                            }),
                    )
                    .child(
                        Button::new("accounts.device.open")
                            .label("Open GitHub")
                            .on_click(move |_, _, cx| cx.open_url(&uri)),
                    ),
            )
            .into_any_element()
    }
}

impl Render for AccountsSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        match self.phase.clone() {
            Phase::List => self.render_list(cx),
            Phase::Add => self.render_add(cx),
            Phase::Device(code) => self.render_device(&code, cx),
            Phase::Working(message) => div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(dialog_title("accounts.working.title", cx).child("Accounts"))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.colors.muted_foreground)
                        .child(message),
                )
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use bonsai_keychain::{MemoryStore, TokenStore};
    use bonsai_providers::{BoxFuture, Request, Response};
    use gpui_kit::TestAppContext;

    use super::*;
    use crate::accounts::accounts_path;

    /// Answers /user by the bearer token: `good` is Ada, anything else 401.
    struct FakeGitHub(Mutex<Vec<String>>);

    impl HttpClient for FakeGitHub {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
            self.0.lock().unwrap().push(request.url.clone());
            let ok = request.header_value("authorization") == Some("Bearer good");
            Box::pin(async move {
                Ok(if ok {
                    Response {
                        status: 200,
                        headers: Vec::new(),
                        body: br#"{"login":"ada","name":"Ada Lovelace","avatar_url":null}"#
                            .to_vec(),
                    }
                } else {
                    Response {
                        status: 401,
                        headers: Vec::new(),
                        body: br#"{"message":"Bad credentials"}"#.to_vec(),
                    }
                })
            })
        }
    }

    #[gpui_kit::test]
    fn signs_in_with_a_token_checks_status_and_signs_out(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        let client: Arc<dyn HttpClient> = Arc::new(FakeGitHub(Mutex::new(Vec::new())));
        let path = accounts_path(dir.path());
        let accounts = Accounts::load(path.clone(), store.clone());
        let (sheet, cx) = cx
            .add_window_view(|window, cx| AccountsSheet::new(accounts, client.clone(), window, cx));

        sheet.update_in(cx, |sheet, window, cx| {
            sheet.show_add(ProviderKind::GitHub, window, cx);
            assert!(!sheet.can_sign_in(cx));
            sheet.set_token(Secret::new("bad"), cx);
            sheet.sign_in(cx);
        });
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.phase(), &Phase::Add);
            assert!(sheet.error().unwrap().contains("refused"));
            assert_eq!(sheet.accounts(), []);
        });

        sheet.update(cx, |sheet, cx| {
            sheet.set_token(Secret::new("good"), cx);
            sheet.sign_in(cx);
        });
        cx.run_until_parked();
        let key = TokenKey::new("github.com", "ada");
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.phase(), &Phase::List);
            assert_eq!(sheet.accounts()[0].account.username, "ada");
            assert_eq!(sheet.status(&key), Some(&Status::Connected));
        });
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "good");
        assert!(!std::fs::read_to_string(&path).unwrap().contains("good"));

        // A revoked token shows as expired on the next check.
        store.set(&key, &Secret::new("revoked")).unwrap();
        sheet.update(cx, AccountsSheet::check_all);
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.status(&key), Some(&Status::Expired));
        });

        sheet.update(cx, |sheet, cx| sheet.sign_out(&key, cx));
        assert_eq!(store.get(&key).unwrap(), None);
        sheet.read_with(cx, |sheet, _| assert_eq!(sheet.accounts(), []));
    }

    #[gpui_kit::test]
    fn the_workspace_opens_the_sheet(cx: &mut TestAppContext) {
        cx.update(|cx| {
            crate::ui::theme::init(cx);
            crate::commands::install(cx);
        });
        let dir = tempfile::tempdir().unwrap();
        let (workspace, cx) = cx.add_window_view(|window, cx| {
            crate::workspace::Workspace::new(
                crate::session::Session::default(),
                Some(dir.path().join("session.json")),
                false,
                window,
                cx,
            )
        });
        workspace.update_in(cx, |workspace, window, cx| {
            let accounts =
                Accounts::load(accounts_path(dir.path()), Arc::new(MemoryStore::default()));
            workspace.show_accounts_with(
                accounts,
                Arc::new(FakeGitHub(Mutex::new(Vec::new()))),
                window,
                cx,
            );
        });
        cx.run_until_parked();
        let sheet = workspace.read_with(cx, |w, _| w.accounts_sheet().cloned().unwrap());
        sheet.update(cx, |_, cx| cx.emit(AccountsEvent::Dismissed));
        cx.run_until_parked();
        workspace.read_with(cx, |w, _| assert!(w.accounts_sheet().is_none()));
    }
}
