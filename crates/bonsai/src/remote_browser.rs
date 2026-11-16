//! Browsing an account's remote repositories (CONTEXT.md: Remote
//! repository), with search, paging, the ones already cloned marked, and
//! one-click clone.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use bonsai_providers::{HttpClient, RemoteRepository, RepositoryQuery};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::{
    accounts::{Accounts, Connected, provider},
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        checkbox::Checkbox,
        input::{Input, InputEvent, InputState},
        theme::UiTheme,
    },
};

pub enum RemoteBrowserEvent {
    /// Clone this URL.
    Clone(String),
    /// Already cloned here: open it.
    Open(PathBuf),
    ManageAccounts,
}

/// `host/path` for a clone URL in any of Git's forms, lowercase and without
/// `.git`, so HTTPS and SSH URLs of one repository compare equal.
#[must_use]
pub fn repository_key(url: &str) -> Option<String> {
    let url = url.trim();
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        // ssh://host:22/path: the port is not part of the identity.
        let host = if url.starts_with("ssh://") {
            host.split(':').next()?
        } else {
            host
        };
        (host, path)
    } else {
        // scp-like: user@host:path
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        (
            authority.rsplit_once('@').map_or(authority, |(_, h)| h),
            path,
        )
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!host.is_empty() && !path.is_empty()).then(|| {
        format!(
            "{}/{}",
            host.to_ascii_lowercase(),
            path.to_ascii_lowercase()
        )
    })
}

/// The remote URLs of local repositories, by [`repository_key`].
fn local_clones(paths: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut found = HashMap::new();
    for path in paths {
        let Ok(output) = std::process::Command::new("git")
            .args(["config", "--get-regexp", r"^remote\..*\.url$"])
            .current_dir(path)
            .output()
        else {
            continue;
        };
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            if let Some(key) = line
                .split_once(' ')
                .and_then(|(_, url)| repository_key(url))
            {
                found.entry(key).or_insert_with(|| path.clone());
            }
        }
    }
    found
}

pub struct RemoteBrowser {
    accounts: Accounts,
    client: Arc<dyn HttpClient>,
    selected: usize,
    search: Entity<InputState>,
    repositories: Vec<RemoteRepository>,
    next: Option<String>,
    loading: bool,
    error: Option<SharedString>,
    use_ssh: bool,
    local: HashMap<String, PathBuf>,
    /// Ignores results from a search that has since changed.
    generation: u64,
    tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<RemoteBrowserEvent> for RemoteBrowser {}

impl RemoteBrowser {
    pub fn new(
        accounts: Accounts,
        client: Arc<dyn HttpClient>,
        recents: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search repositories"));
        let subscriptions = vec![cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.reload_after(Duration::from_millis(300), cx);
            }
        })];
        let mut this = Self {
            accounts,
            client,
            selected: 0,
            search,
            repositories: Vec::new(),
            next: None,
            loading: false,
            error: None,
            use_ssh: false,
            local: HashMap::new(),
            generation: 0,
            tasks: Vec::new(),
            _subscriptions: subscriptions,
        };
        let scan = cx.background_spawn(async move { local_clones(&recents) });
        this.tasks.push(cx.spawn(async move |this, cx| {
            let local = scan.await;
            let _ = this.update(cx, |this, cx| {
                this.local = local;
                cx.notify();
            });
        }));
        this.reload(cx);
        this
    }

    #[must_use]
    pub fn repositories(&self) -> &[RemoteRepository] {
        &self.repositories
    }

    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    #[must_use]
    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    #[must_use]
    pub fn local_path(&self, repository: &RemoteRepository) -> Option<&PathBuf> {
        [Some(&repository.clone_https), repository.clone_ssh.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|url| self.local.get(&repository_key(url)?))
    }

    fn account(&self) -> Option<&Connected> {
        self.accounts.list().get(self.selected)
    }

    pub fn select_account(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.reload(cx);
    }

    fn reload_after(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.reload(cx);
                }
            });
        }));
    }

    /// Fetches the first page for the current account and search.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.repositories.clear();
        self.next = None;
        self.fetch(None, cx);
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.next.take() {
            self.fetch(Some(next), cx);
        }
    }

    fn fetch(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.error = None;
        let Some(connected) = self.account().cloned() else {
            self.loading = false;
            cx.notify();
            return;
        };
        let Some(token) = self.accounts.token(&connected) else {
            self.loading = false;
            self.error = Some("This account has no token. Sign in again in Accounts.".into());
            cx.notify();
            return;
        };
        self.loading = true;
        let api = connected.api_token(&token);
        let provider = provider(
            connected.account.kind,
            &connected.account.host,
            self.client.clone(),
        );
        let query = RepositoryQuery {
            search: self.search.read(cx).value().trim().to_owned(),
            cursor,
        };
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider.repositories(&api, &query).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(page) => {
                        this.repositories.extend(page.items);
                        this.next = page.next;
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_row(
        &self,
        index: usize,
        repository: &RemoteRepository,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let local = self.local_path(repository).cloned();
        let url = if self.use_ssh {
            repository
                .clone_ssh
                .clone()
                .unwrap_or_else(|| repository.clone_https.clone())
        } else {
            repository.clone_https.clone()
        };
        let mut meta = Vec::new();
        if repository.private {
            meta.push("Private".to_owned());
        }
        if repository.fork {
            meta.push("Fork".to_owned());
        }
        if repository.archived {
            meta.push("Archived".to_owned());
        }
        if let Some(updated) = &repository.updated_at {
            meta.push(format!("Updated {}", updated.get(..10).unwrap_or(updated)));
        }
        div()
            .id(("remote.row", index))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(10.))
            .py(px(8.))
            .rounded(theme.radius.md)
            .hover(|this| this.bg(c.accent))
            .child(
                lucide(if repository.private {
                    LucideIcon::Lock
                } else {
                    LucideIcon::BookMarked
                })
                .size(px(16.))
                .text_color(c.muted_foreground),
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
                            .truncate()
                            .child(repository.full_name.clone()),
                    )
                    .when_some(repository.description.clone(), |this, description| {
                        this.child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(c.muted_foreground)
                                .child(description),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(meta.join(" · ")),
                    ),
            )
            .child(match local {
                Some(path) => Button::new(("remote.open", index))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Open")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(RemoteBrowserEvent::Open(path.clone()));
                    })),
                None => Button::new(("remote.clone", index))
                    .variant(ButtonVariant::Outline)
                    .size(ButtonSize::Sm)
                    .label("Clone…")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(RemoteBrowserEvent::Clone(url.clone()));
                    })),
            })
    }
}

impl Render for RemoteBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(c.muted_foreground)
                    .child("YOUR REPOSITORIES"),
            )
            .child(
                Button::new("remote.accounts")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Sm)
                    .label("Accounts…")
                    .on_click(
                        cx.listener(|_, _, _, cx| cx.emit(RemoteBrowserEvent::ManageAccounts)),
                    ),
            );
        if self.accounts.list().is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(header)
                .child(
                    div()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child("Connect GitHub, GitLab or Bitbucket to see your repositories here and clone them in one click."),
                )
                .into_any_element();
        }
        let mut tabs = div().flex().flex_wrap().gap(px(6.));
        for (i, connected) in self.accounts.list().iter().enumerate() {
            tabs = tabs.child(
                Button::new(("remote.account", i))
                    .variant(if i == self.selected {
                        ButtonVariant::Secondary
                    } else {
                        ButtonVariant::Ghost
                    })
                    .size(ButtonSize::Sm)
                    .label(format!(
                        "{} · {}",
                        connected.account.username, connected.account.host
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_account(i, cx))),
            );
        }
        let mut list = div().flex().flex_col().gap(px(2.));
        let repositories = self.repositories.clone();
        for (i, repository) in repositories.iter().enumerate() {
            list = list.child(self.render_row(i, repository, cx));
        }
        let status = if self.loading {
            Some("Loading…".to_owned())
        } else if self.repositories.is_empty() && self.error.is_none() {
            Some("No repositories match.".to_owned())
        } else {
            None
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(header)
            .child(tabs)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .child(Input::new(&self.search).aria_label("Search repositories")),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_xs()
                            .child(
                                Checkbox::new("remote.ssh")
                                    .checked(self.use_ssh)
                                    .aria_label("Clone with SSH")
                                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                        this.use_ssh = *checked;
                                        cx.notify();
                                    })),
                            )
                            .child("SSH"),
                    ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(c.destructive).child(error))
            })
            .child(list)
            .when_some(status, |this, status| {
                this.child(div().text_sm().text_color(c.muted_foreground).child(status))
            })
            .when(self.next.is_some() && !self.loading, |this| {
                this.child(
                    Button::new("remote.more")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Load More")
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::repository_key;

    #[test]
    fn https_and_ssh_urls_of_one_repository_match() {
        let key = Some("github.com/ada/engine".to_owned());
        for url in [
            "https://github.com/ada/engine.git",
            "https://ada@GitHub.com/Ada/Engine",
            "git@github.com:ada/engine.git",
            "ssh://git@github.com:22/ada/engine.git",
            "https://github.com/ada/engine/",
        ] {
            assert_eq!(repository_key(url), key, "{url}");
        }
        assert_eq!(
            repository_key("https://gitlab.com/group/sub/app.git").as_deref(),
            Some("gitlab.com/group/sub/app")
        );
        assert_eq!(repository_key("/Users/ada/repo"), None);
        assert_eq!(repository_key("../relative"), None);
    }
}

#[cfg(test)]
mod browse_tests {
    use std::sync::Mutex;

    use bonsai_keychain::{MemoryStore, Secret, TokenStore};
    use bonsai_providers::{Account, BoxFuture, ProviderKind, Request, Response};
    use gpui_kit::TestAppContext;

    use super::*;
    use crate::accounts::{SignInMethod, accounts_path};

    struct Pages(Mutex<Vec<String>>);

    fn repo(full: &str) -> String {
        format!(
            r#"{{"full_name":"{full}","name":"x","owner":{{"login":"ada"}},"description":"About {full}","private":true,"fork":false,"default_branch":"main","clone_url":"https://github.com/{full}.git","ssh_url":"git@github.com:{full}.git","html_url":"https://github.com/{full}","updated_at":"2026-11-10T09:00:00Z"}}"#
        )
    }

    impl HttpClient for Pages {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
            self.0.lock().unwrap().push(request.url.clone());
            let page_two = request.url.contains("page=2");
            Box::pin(async move {
                Ok(if page_two {
                    Response {
                        status: 200,
                        headers: Vec::new(),
                        body: format!("[{}]", repo("ada/third")).into_bytes(),
                    }
                } else {
                    Response {
                        status: 200,
                        headers: vec![(
                            "Link".into(),
                            r#"<https://api.github.com/user/repos?page=2>; rel="next""#.into(),
                        )],
                        body: format!("[{},{}]", repo("ada/engine"), repo("ada/notes"))
                            .into_bytes(),
                    }
                })
            })
        }
    }

    #[gpui_kit::test]
    fn lists_pages_and_marks_local_clones(cx: &mut TestAppContext) {
        cx.update(crate::ui::theme::init);
        let dir = tempfile::tempdir().unwrap();
        // A local clone of ada/notes, by its SSH URL.
        let local = dir.path().join("notes");
        std::fs::create_dir(&local).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&local)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:Ada/notes.git"]);

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
        let client = Arc::new(Pages(Mutex::new(Vec::new())));
        let shared: Arc<dyn HttpClient> = client.clone();
        let (browser, cx) = cx.add_window_view(|window, cx| {
            RemoteBrowser::new(accounts, shared, vec![local.clone()], window, cx)
        });
        cx.run_until_parked();
        browser.read_with(cx, |browser, _| {
            let names: Vec<&str> = browser
                .repositories()
                .iter()
                .map(|r| r.full_name.as_str())
                .collect();
            assert_eq!(names, ["ada/engine", "ada/notes"]);
            assert_eq!(browser.local_path(&browser.repositories()[0]), None);
            assert_eq!(browser.local_path(&browser.repositories()[1]), Some(&local));
        });
        browser.update(cx, RemoteBrowser::load_more);
        cx.run_until_parked();
        browser.read_with(cx, |browser, _| {
            assert_eq!(browser.repositories().len(), 3);
            assert!(!browser.is_loading());
        });
        assert_eq!(client.0.lock().unwrap().len(), 2);
    }
}
