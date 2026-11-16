//! Publishing a local repository (CONTEXT.md: Publish): create it on a
//! provider, add it as a remote, and push the current branch with its
//! upstream set.

use std::sync::Arc;

use bonsai_git::{Head, PushRequest};
use bonsai_providers::{HttpClient, NewRepository, RemoteRepository};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::FluentBuilder as _, px,
};

use crate::{
    accounts::{Accounts, Connected, provider},
    repository::RepositoryModel,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        checkbox::Checkbox,
        dialog::{dialog_description, dialog_footer, dialog_title},
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

pub enum PublishEvent {
    Dismissed,
    ManageAccounts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Form,
    Creating,
    /// Created; the remote is added and the push queued.
    Done {
        repository: Box<RemoteRepository>,
        remote: String,
        pushed_branch: Option<String>,
    },
}

pub struct PublishSheet {
    model: Entity<RepositoryModel>,
    accounts: Accounts,
    client: Arc<dyn HttpClient>,
    selected: usize,
    name: Entity<InputState>,
    description: Entity<InputState>,
    owner: Entity<InputState>,
    private: bool,
    phase: Phase,
    error: Option<SharedString>,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PublishEvent> for PublishSheet {}

/// A repository name from a folder name: spaces become `-`, and anything
/// providers reject is dropped.
#[must_use]
pub fn default_name(folder: &str) -> String {
    let mut out = String::new();
    for c in folder.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

impl PublishSheet {
    pub fn new(
        model: Entity<RepositoryModel>,
        accounts: Accounts,
        client: Arc<dyn HttpClient>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let folder = model
            .read(cx)
            .repository()
            .root()
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("Repository name");
            input.set_value(default_name(&folder), window, cx);
            input
        });
        let description =
            cx.new(|cx| InputState::new(window, cx).placeholder("Description (optional)"));
        let owner = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Organization, group or workspace (optional)")
        });
        let subscriptions = [&name, &description, &owner]
            .into_iter()
            .map(|input| {
                cx.subscribe(input, |this, _, event: &InputEvent, cx| match event {
                    InputEvent::Change => cx.notify(),
                    InputEvent::PressEnter { .. } => this.publish(cx),
                    _ => {}
                })
            })
            .collect();
        Self {
            model,
            accounts,
            client,
            selected: 0,
            name,
            description,
            owner,
            private: true,
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

    fn account(&self) -> Option<&Connected> {
        self.accounts.list().get(self.selected)
    }

    fn value(input: &Entity<InputState>, cx: &gpui_kit::App) -> String {
        input.read(cx).value().trim().to_owned()
    }

    #[must_use]
    pub fn can_publish(&self, cx: &gpui_kit::App) -> bool {
        self.phase == Phase::Form
            && self.account().is_some()
            && !default_name(&Self::value(&self.name, cx)).is_empty()
    }

    /// Creates the repository, then adds the remote and pushes.
    pub fn publish(&mut self, cx: &mut Context<Self>) {
        if !self.can_publish(cx) {
            return;
        }
        let Some(connected) = self.account().cloned() else {
            return;
        };
        let Some(token) = self.accounts.token(&connected) else {
            self.error = Some("This account has no token. Sign in again in Accounts.".into());
            cx.notify();
            return;
        };
        let new = NewRepository {
            name: default_name(&Self::value(&self.name, cx)),
            description: Some(Self::value(&self.description, cx)).filter(|d| !d.is_empty()),
            private: self.private,
            owner: Some(Self::value(&self.owner, cx)).filter(|o| !o.is_empty()),
        };
        let provider = provider(
            connected.account.kind,
            &connected.account.host,
            self.client.clone(),
        );
        let api = connected.api_token(&token);
        let kind = connected.account.kind;
        self.phase = Phase::Creating;
        self.error = None;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider.create_repository(&api, &new).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(repository) => this.connect(repository, kind.name(), cx),
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

    /// Adds the new repository as `origin` (or after the provider when
    /// `origin` is taken) and pushes the current branch, when it has commits.
    fn connect(&mut self, repository: RemoteRepository, provider: &str, cx: &mut Context<Self>) {
        let snapshot = self.model.read(cx).snapshot().cloned();
        let taken: Vec<String> = snapshot
            .as_ref()
            .map(|s| s.remotes.iter().map(|r| r.name.clone()).collect())
            .unwrap_or_default();
        let remote = if taken.iter().any(|r| r == "origin") {
            provider.to_ascii_lowercase()
        } else {
            "origin".to_owned()
        };
        let branch = match snapshot.map(|s| s.head) {
            Some(Head::Branch { name, oid: Some(_) }) => Some(name),
            _ => None,
        };
        let url = repository.clone_https.clone();
        let (name, push) = (remote.clone(), branch.clone());
        self.model.update(cx, |model, cx| {
            model.run_network(
                format!("Publish to {}", repository.full_name),
                move |git, progress, cancel| {
                    git.add_remote(&name, &url)?;
                    match push {
                        Some(branch) => git.push(
                            &PushRequest::new(name, branch).set_upstream(),
                            Some(progress),
                            Some(cancel),
                        ),
                        None => Ok(()),
                    }
                },
                cx,
            );
        });
        self.phase = Phase::Done {
            repository: Box::new(repository),
            remote,
            pushed_branch: branch,
        };
    }

    fn render_form(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        if self.accounts.list().is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(dialog_title("publish.title", cx).child("Publish Repository"))
                .child(dialog_description("publish.description", cx).child(
                    "Connect a GitHub, GitLab or Bitbucket account to create the remote repository.",
                ))
                .child(
                    dialog_footer(cx)
                        .child(
                            Button::new("publish.cancel")
                                .variant(ButtonVariant::Outline)
                                .label("Cancel")
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(PublishEvent::Dismissed))),
                        )
                        .child(
                            Button::new("publish.accounts")
                                .label("Add Account…")
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(PublishEvent::ManageAccounts);
                                })),
                        ),
                )
                .into_any_element();
        }
        let mut accounts = div().flex().flex_wrap().gap(px(6.));
        for (i, connected) in self.accounts.list().iter().enumerate() {
            accounts = accounts.child(
                Button::new(("publish.account", i))
                    .variant(if i == self.selected {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .size(ButtonSize::Sm)
                    .label(format!(
                        "{} · {}",
                        connected.account.username,
                        connected.account.kind.name()
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = i;
                        cx.notify();
                    })),
            );
        }
        let creating = self.phase == Phase::Creating;
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(dialog_title("publish.title", cx).child("Publish Repository"))
            .child(dialog_description("publish.description", cx).child(
                "Creates the repository, adds it as a remote, and pushes the current branch.",
            ))
            .child(accounts)
            .child(Input::new(&self.name).aria_label("Repository name"))
            .child(Input::new(&self.description).aria_label("Description"))
            .child(Input::new(&self.owner).aria_label("Owner"))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_sm()
                    .child(
                        Checkbox::new("publish.private")
                            .checked(self.private)
                            .aria_label("Private")
                            .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                this.private = *checked;
                                cx.notify();
                            })),
                    )
                    .child("Private"),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_xs().text_color(c.destructive).child(error))
            })
            .child(
                dialog_footer(cx)
                    .child(
                        Button::new("publish.cancel")
                            .variant(ButtonVariant::Outline)
                            .label("Cancel")
                            .disabled(creating)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PublishEvent::Dismissed))),
                    )
                    .child(
                        Button::new("publish.confirm")
                            .label(if creating { "Creating…" } else { "Publish" })
                            .disabled(!self.can_publish(cx))
                            .on_click(cx.listener(|this, _, _, cx| this.publish(cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for PublishSheet {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        match self.phase.clone() {
            Phase::Form | Phase::Creating => self.render_form(cx),
            Phase::Done {
                repository,
                remote,
                pushed_branch,
            } => {
                let web = repository.web_url.clone();
                let summary = match pushed_branch {
                    Some(branch) => format!(
                        "Added it as {remote} and pushing {branch}. Progress shows in the toolbar."
                    ),
                    None => format!(
                        "Added it as {remote}. There are no commits yet: commit, then push to upload them."
                    ),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        dialog_title("publish.done.title", cx)
                            .child(format!("Created {}", repository.full_name)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.colors.muted_foreground)
                            .child(summary),
                    )
                    .child(
                        dialog_footer(cx)
                            .child(
                                Button::new("publish.web")
                                    .variant(ButtonVariant::Outline)
                                    .label("Open in Browser")
                                    .on_click(move |_, _, cx| cx.open_url(&web)),
                            )
                            .child(Button::new("publish.done").label("Done").on_click(
                                cx.listener(|_, _, _, cx| {
                                    cx.emit(PublishEvent::Dismissed);
                                }),
                            )),
                    )
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Mutex};

    use bonsai_git::{Git, Repository};
    use bonsai_keychain::{MemoryStore, Secret, TokenStore};
    use bonsai_providers::{Account, BoxFuture, ProviderKind, Request, Response};
    use gpui_kit::TestAppContext;

    use super::*;
    use crate::accounts::{SignInMethod, accounts_path};

    /// GitHub that creates repositories backed by a local bare repository,
    /// and refuses the name `taken`.
    struct FakeCreate {
        bare: String,
        bodies: Mutex<Vec<serde_json::Value>>,
    }

    impl HttpClient for FakeCreate {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            self.bodies.lock().unwrap().push(body.clone());
            let name = body["name"].as_str().unwrap().to_owned();
            let response = if name == "taken" {
                Response {
                    status: 422,
                    headers: Vec::new(),
                    body: br#"{"message":"Repository creation failed.","errors":[{"message":"name already exists on this account"}]}"#.to_vec(),
                }
            } else {
                Response {
                    status: 201,
                    headers: Vec::new(),
                    body: format!(
                        r#"{{"full_name":"ada/{name}","name":"{name}","owner":{{"login":"ada"}},"private":true,"fork":false,"clone_url":"{}","html_url":"https://github.com/ada/{name}"}}"#,
                        self.bare
                    )
                    .into_bytes(),
                }
            };
            Box::pin(async move { Ok(response) })
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    #[test]
    fn names_come_from_folders() {
        assert_eq!(default_name("My Project (old)"), "My-Project-old");
        assert_eq!(default_name("api.server_v2"), "api.server_v2");
    }

    #[gpui_kit::test]
    fn creates_adds_origin_and_pushes_with_upstream(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("my app");
        let bare = dir.path().join("remote.git");
        std::fs::create_dir(&bare).unwrap();
        git(&bare, &["init", "-q", "--bare"]);
        let repository =
            Repository::init(&Git::locate(None).unwrap(), &root, Some("main")).unwrap();
        for (key, value) in [
            ("user.name", "Ada"),
            ("user.email", "ada@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(&root, &["config", key, value]);
        }
        std::fs::write(root.join("a.txt"), "a").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "first"]);

        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        let mut accounts = Accounts::load(accounts_path(dir.path()), store);
        accounts
            .add(
                Connected {
                    account: Account {
                        kind: ProviderKind::GitHub,
                        host: "github.com".into(),
                        api_url: "https://api.github.com".into(),
                        username: "ada".into(),
                        display_name: None,
                        avatar_url: None,
                    },
                    method: SignInMethod::Token,
                },
                &Secret::new("t"),
            )
            .unwrap();
        let client = Arc::new(FakeCreate {
            bare: bare.to_string_lossy().into_owned(),
            bodies: Mutex::new(Vec::new()),
        });
        let shared: Arc<dyn HttpClient> = client.clone();
        let model = cx.new(|cx| RepositoryModel::new(repository, false, cx));
        let (sheet, cx) = cx.add_window_view(|window, cx| {
            PublishSheet::new(model.clone(), accounts, shared, window, cx)
        });
        cx.run_until_parked();

        // The name defaults from the folder; a taken name stays on the form.
        let name = sheet.read_with(cx, |sheet, _| sheet.name.clone());
        assert_eq!(
            name.read_with(cx, |input, _| input.value().to_string()),
            "my-app"
        );
        name.update_in(cx, |input, window, cx| input.set_value("taken", window, cx));
        sheet.update(cx, PublishSheet::publish);
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.phase(), &Phase::Form);
            assert!(sheet.error().unwrap().contains("already exists"));
        });

        name.update_in(cx, |input, window, cx| {
            input.set_value("my-app", window, cx);
        });
        sheet.update(cx, PublishSheet::publish);
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert!(matches!(
                sheet.phase(),
                Phase::Done { remote, pushed_branch: Some(branch), .. }
                    if remote == "origin" && branch == "main"
            ));
        });
        assert_eq!(client.bodies.lock().unwrap()[1]["private"], true);
        assert_eq!(
            git(&bare, &["rev-parse", "main"]),
            git(&root, &["rev-parse", "main"])
        );
        assert_eq!(
            git(&root, &["rev-parse", "--abbrev-ref", "main@{upstream}"]).trim(),
            "origin/main"
        );
    }
}
