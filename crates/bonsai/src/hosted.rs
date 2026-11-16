//! Pull requests and issues of the repository's hosted remote
//! (CONTEXT.md: Pull request): a list beside the sidebar, filtered by state and
//! paged, and a detail pane with the description, Open in Browser and, for
//! pull requests, Check Out.

use std::{
    fmt::Write as _,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use bonsai_git::Snapshot;
use bonsai_keychain::{Secret, TokenStore};
use bonsai_providers::{
    Comment, HttpClient, Issue, ItemKind, ItemQuery, ItemState, Label, MergeMethod, Person,
    Provider, ProviderKind, PullRequest, StateFilter, hosted_repository,
};
use gpui_icons::{LucideIcon, lucide};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Task, Window, div, prelude::FluentBuilder as _, px, rgb,
};

use crate::{
    accounts::{Accounts, Connected, provider},
    activity_panel::ago,
    repository::RepositoryModel,
    ui::{
        button::{Button, ButtonSize, ButtonVariant},
        empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle},
        markdown::markdown,
        spinner::Spinner,
        tag::Tag,
        textarea::{Textarea, TextareaState},
        theme::UiTheme,
        toggle_group::{ToggleGroup, ToggleGroupItem},
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostedKind {
    PullRequests,
    Issues,
}

impl HostedKind {
    fn noun(self) -> &'static str {
        match self {
            Self::PullRequests => "pull requests",
            Self::Issues => "issues",
        }
    }
}

/// Where accounts and tokens come from, and how requests go out.
#[derive(Clone)]
pub struct HostedEnv {
    pub accounts_path: PathBuf,
    pub store: Arc<dyn TokenStore>,
    pub client: Arc<dyn HttpClient>,
}

pub enum HostedEvent {
    ManageAccounts,
    CreatePullRequest,
    CreateIssue,
}

/// The remote whose pull requests and issues are shown, and the account
/// that reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub remote: String,
    pub host: String,
    pub full_name: String,
    pub connected: Connected,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Availability {
    /// No remote on a host Bonsai knows how to read.
    NoRemote,
    /// A hosted remote, but no account on its host.
    NoAccount(String),
    Ready(Target),
}

/// The remote to read: `origin` first, then the others in order, the first
/// with an account on its host.
fn availability(snapshot: Option<&Snapshot>, accounts: &[Connected]) -> Availability {
    let Some(snapshot) = snapshot else {
        return Availability::NoRemote;
    };
    let mut remotes: Vec<_> = snapshot.remotes.iter().collect();
    remotes.sort_by_key(|r| r.name != "origin");
    let mut first_host = None;
    for remote in remotes {
        let Some((host, full_name)) = remote.fetch_url.as_deref().and_then(hosted_repository)
        else {
            continue;
        };
        if let Some(connected) = accounts
            .iter()
            .find(|c| c.account.host.eq_ignore_ascii_case(&host))
        {
            return Availability::Ready(Target {
                remote: remote.name.clone(),
                host,
                full_name,
                connected: connected.clone(),
            });
        }
        first_host.get_or_insert(host);
    }
    first_host.map_or(Availability::NoRemote, Availability::NoAccount)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullInfo {
    pub head: String,
    pub head_fork: Option<String>,
    pub base: String,
}

/// A pull request or an issue, as the list and detail show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: Person,
    pub state: ItemState,
    pub labels: Vec<Label>,
    pub comments: Option<u32>,
    pub created_at: String,
    pub updated_at: String,
    pub web_url: String,
    pub pull: Option<PullInfo>,
}

impl From<PullRequest> for Item {
    fn from(p: PullRequest) -> Self {
        Self {
            number: p.number,
            title: p.title,
            body: p.body,
            author: p.author,
            state: p.state,
            labels: p.labels,
            comments: p.comments,
            created_at: p.created_at,
            updated_at: p.updated_at,
            web_url: p.web_url,
            pull: Some(PullInfo {
                head: p.head,
                head_fork: p.head_fork,
                base: p.base,
            }),
        }
    }
}

impl From<Issue> for Item {
    fn from(i: Issue) -> Self {
        Self {
            number: i.number,
            title: i.title,
            body: i.body,
            author: i.author,
            state: i.state,
            labels: i.labels,
            comments: i.comments,
            created_at: i.created_at,
            updated_at: i.updated_at,
            web_url: i.web_url,
            pull: None,
        }
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Reads an RFC 3339 time such as `2026-11-12T09:00:00Z` or
/// `2026-11-02T09:00:00.000000+00:00`.
#[must_use]
pub fn parse_time(text: &str) -> Option<SystemTime> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.parse::<i64>().ok();
    let days = days_from_civil(number(0..4)?, number(5..7)?, number(8..10)?);
    let mut seconds = days * 86_400 + number(11..13)? * 3_600 + number(14..16)? * 60;
    seconds += number(17..19)?;
    // The offset follows the seconds and any fraction.
    let rest = text
        .get(19..)?
        .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if let Some(sign @ ('+' | '-')) = rest.chars().next() {
        let hours: i64 = rest.get(1..3)?.parse().ok()?;
        let minutes: i64 = rest.get(4..6)?.parse().ok()?;
        let offset = hours * 3_600 + minutes * 60;
        seconds -= if sign == '+' { offset } else { -offset };
    }
    let seconds = u64::try_from(seconds).ok()?;
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
}

fn when(text: &str) -> String {
    parse_time(text).map_or_else(|| text.get(..10).unwrap_or(text).to_owned(), ago)
}

fn comment_count(n: u32) -> String {
    if n == 1 {
        "1 comment".to_owned()
    } else {
        format!("{n} comments")
    }
}

fn state_label(state: ItemState) -> &'static str {
    match state {
        ItemState::Open => "Open",
        ItemState::Draft => "Draft",
        ItemState::Merged => "Merged",
        ItemState::Closed => "Closed",
    }
}

fn state_icon(kind: HostedKind, state: ItemState) -> LucideIcon {
    match (kind, state) {
        (HostedKind::Issues, ItemState::Closed) => LucideIcon::CircleCheck,
        (HostedKind::Issues, _) => LucideIcon::CircleDot,
        (_, ItemState::Open) => LucideIcon::GitPullRequest,
        (_, ItemState::Draft) => LucideIcon::GitPullRequestDraft,
        (_, ItemState::Merged) => LucideIcon::GitMerge,
        (_, ItemState::Closed) => LucideIcon::GitPullRequestClosed,
    }
}

fn label_color(label: &Label) -> Option<gpui_kit::Rgba> {
    let hex = label.color.as_deref()?.trim_start_matches('#');
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
        .map(rgb)
}

const FILTERS: [(StateFilter, &str); 3] = [
    (StateFilter::Open, "Open"),
    (StateFilter::Closed, "Closed"),
    (StateFilter::All, "All"),
];

pub struct HostedView {
    kind: HostedKind,
    env: HostedEnv,
    model: Entity<RepositoryModel>,
    availability: Availability,
    filter: StateFilter,
    items: Vec<Item>,
    next: Option<String>,
    loading: bool,
    error: Option<SharedString>,
    selected: Option<u64>,
    /// Ignores results from a request that has since been replaced.
    generation: u64,
    started: bool,
    /// Each item's comments, by number, loaded when it is first shown.
    conversations: std::collections::HashMap<u64, Conversation>,
    merge_method: MergeMethod,
    /// The pull request being merged.
    merging: Option<u64>,
    /// Why the last merge, comment or close failed, by item.
    action_error: Option<(u64, SharedString)>,
    /// The new comment, created on first render.
    composer: Option<Entity<TextareaState>>,
    /// The item whose comment is being sent.
    posting: Option<u64>,
    /// The item being closed or reopened.
    changing_state: Option<u64>,
    tasks: Vec<Task<()>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conversation {
    Loading,
    Loaded(Vec<Comment>),
    Failed(SharedString),
}

const MERGE_METHODS: [(MergeMethod, &str, &str); 3] = [
    (MergeMethod::Merge, "Merge", "Merge Pull Request"),
    (MergeMethod::Squash, "Squash", "Squash and Merge"),
    (MergeMethod::Rebase, "Rebase", "Rebase and Merge"),
];

impl EventEmitter<HostedEvent> for HostedView {}

impl HostedView {
    #[must_use]
    pub fn new(
        kind: HostedKind,
        model: Entity<RepositoryModel>,
        env: HostedEnv,
        _: &mut Context<Self>,
    ) -> Self {
        Self {
            kind,
            env,
            model,
            availability: Availability::NoRemote,
            filter: StateFilter::Open,
            items: Vec::new(),
            next: None,
            loading: false,
            error: None,
            selected: None,
            generation: 0,
            started: false,
            conversations: std::collections::HashMap::new(),
            merge_method: MergeMethod::Merge,
            merging: None,
            action_error: None,
            composer: None,
            posting: None,
            changing_state: None,
            tasks: Vec::new(),
        }
    }

    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
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
    pub fn kind(&self) -> HostedKind {
        self.kind
    }

    /// The remote and account the list reads, when there is one.
    #[must_use]
    pub fn target(&self) -> Option<&Target> {
        match &self.availability {
            Availability::Ready(target) => Some(target),
            _ => None,
        }
    }

    #[must_use]
    pub fn selected(&self) -> Option<&Item> {
        let number = self.selected?;
        self.items.iter().find(|item| item.number == number)
    }

    fn accounts(&self) -> Accounts {
        Accounts::load(self.env.accounts_path.clone(), self.env.store.clone())
    }

    /// Shown again: rereads the accounts and remotes, and loads the list
    /// the first time or when the repository it reads has changed.
    pub fn activate(&mut self, cx: &mut Context<Self>) {
        let accounts = self.accounts();
        let next = availability(self.model.read(cx).snapshot(), accounts.list());
        if next != self.availability || !self.started {
            self.availability = next;
            self.started = true;
            self.selected = None;
            self.reload(cx);
        }
    }

    pub fn set_filter(&mut self, filter: StateFilter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.items.clear();
        self.next = None;
        self.fetch(None, cx);
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if let Some(next) = self.next.take() {
            self.fetch(Some(next), cx);
        }
    }

    pub fn select(&mut self, number: u64, cx: &mut Context<Self>) {
        self.selected = Some(number);
        cx.notify();
    }

    /// The selected item's comments, if they have been asked for.
    #[must_use]
    pub fn conversation(&self, number: u64) -> Option<&Conversation> {
        self.conversations.get(&number)
    }

    pub fn set_merge_method(&mut self, method: MergeMethod, cx: &mut Context<Self>) {
        self.merge_method = method;
        cx.notify();
    }

    /// The target, its API credential and its provider, or why there are none.
    fn session(&self) -> Result<(Target, String, Box<dyn Provider>), SharedString> {
        let Availability::Ready(target) = self.availability.clone() else {
            return Err("No account reads this repository.".into());
        };
        let token = match self.accounts().token(&target.connected) {
            Some(token) => token,
            None if crate::http::replaying() => Secret::new(""),
            None => return Err("This account has no token. Sign in again in Accounts.".into()),
        };
        let api = target.connected.api_token(&token);
        let provider = provider(
            target.connected.account.kind,
            &target.connected.account.host,
            self.env.client.clone(),
        );
        Ok((target, api, provider))
    }

    /// Loads the selected item's comments the first time it is shown.
    pub fn load_conversation(&mut self, cx: &mut Context<Self>) {
        let Some(number) = self.selected else {
            return;
        };
        if self.conversations.contains_key(&number) || self.selected().is_none() {
            return;
        }
        let (target, api, provider) = match self.session() {
            Ok(session) => session,
            Err(error) => {
                self.conversations
                    .insert(number, Conversation::Failed(error));
                return;
            }
        };
        self.conversations.insert(number, Conversation::Loading);
        let kind = self.item_kind();
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider
                .comments(&api, &target.full_name, kind, number)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.conversations.insert(
                    number,
                    match result {
                        Ok(comments) => Conversation::Loaded(comments),
                        Err(error) => Conversation::Failed(error.to_string().into()),
                    },
                );
                cx.notify();
            });
        }));
    }

    fn item_kind(&self) -> ItemKind {
        match self.kind {
            HostedKind::PullRequests => ItemKind::PullRequest,
            HostedKind::Issues => ItemKind::Issue,
        }
    }

    /// The new comment's editor.
    #[must_use]
    pub fn composer(&self) -> Option<&Entity<TextareaState>> {
        self.composer.as_ref()
    }

    fn ensure_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.composer.is_none() {
            let composer = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .rows(3)
                    .placeholder("Leave a comment (Markdown)")
            });
            cx.observe(&composer, |_, _, cx| cx.notify()).detach();
            self.composer = Some(composer);
        }
    }

    /// Sends the composer's text as a comment on the selected item, and adds
    /// it to the conversation.
    pub fn comment(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(number), Some(composer)) = (self.selected, self.composer.clone()) else {
            return;
        };
        let body = composer.read(cx).value().trim().to_owned();
        if body.is_empty() || self.posting.is_some() {
            return;
        }
        let (target, api, provider) = match self.session() {
            Ok(session) => session,
            Err(error) => {
                self.action_error = Some((number, error));
                cx.notify();
                return;
            }
        };
        let kind = self.item_kind();
        self.posting = Some(number);
        self.action_error = None;
        self.tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = provider
                .add_comment(&api, &target.full_name, kind, number, &body)
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.posting = None;
                match result {
                    Ok(comment) => {
                        if let Some(Conversation::Loaded(comments)) =
                            this.conversations.get_mut(&number)
                        {
                            comments.push(comment);
                        }
                        if let Some(item) = this.items.iter_mut().find(|i| i.number == number) {
                            item.comments = item.comments.map(|n| n + 1);
                        }
                        composer.update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    Err(error) => this.action_error = Some((number, error.to_string().into())),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Whether the selected item can be closed, or reopened, from Bonsai.
    fn can_change_state(&self, item: &Item) -> bool {
        match item.state {
            ItemState::Open | ItemState::Draft => true,
            ItemState::Merged => false,
            // Bitbucket can't reopen a declined pull request.
            ItemState::Closed => {
                !(item.pull.is_some()
                    && matches!(&self.availability, Availability::Ready(t)
                    if t.connected.account.kind == ProviderKind::Bitbucket))
            }
        }
    }

    /// Closes the selected item when it is open, and reopens it when closed.
    pub fn toggle_open(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected().cloned() else {
            return;
        };
        if !self.can_change_state(&item) || self.changing_state.is_some() {
            return;
        }
        let (target, api, provider) = match self.session() {
            Ok(session) => session,
            Err(error) => {
                self.action_error = Some((item.number, error));
                cx.notify();
                return;
            }
        };
        let number = item.number;
        let open = !item.state.is_open();
        let kind = self.item_kind();
        self.changing_state = Some(number);
        self.action_error = None;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider
                .set_open(&api, &target.full_name, kind, number, open)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.changing_state = None;
                match result {
                    Ok(()) => {
                        if let Some(item) = this.items.iter_mut().find(|i| i.number == number) {
                            item.state = if open {
                                ItemState::Open
                            } else {
                                ItemState::Closed
                            };
                        }
                    }
                    Err(error) => this.action_error = Some((number, error.to_string().into())),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Merges the selected pull request on the provider, with the chosen
    /// method, then reloads the list.
    pub fn merge(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected().cloned() else {
            return;
        };
        if item.pull.is_none() || item.state != ItemState::Open || self.merging.is_some() {
            return;
        }
        let (target, api, provider) = match self.session() {
            Ok(session) => session,
            Err(error) => {
                self.action_error = Some((item.number, error));
                cx.notify();
                return;
            }
        };
        let number = item.number;
        let method = self.merge_method;
        self.merging = Some(number);
        self.action_error = None;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = provider
                .merge_pull_request(&api, &target.full_name, number, method)
                .await;
            let _ = this.update(cx, |this, cx| {
                this.merging = None;
                match result {
                    Ok(()) => {
                        if let Some(item) = this.items.iter_mut().find(|i| i.number == number) {
                            item.state = ItemState::Merged;
                        }
                        // Fetch so the merge shows in History.
                        let remote = target.remote.clone();
                        this.model.update(cx, |model, cx| {
                            model.run_network(
                                format!("Fetch {remote}"),
                                move |repository, progress, cancel| {
                                    repository.fetch(
                                        Some(&remote),
                                        false,
                                        Some(progress),
                                        Some(cancel),
                                    )
                                },
                                cx,
                            );
                        });
                    }
                    Err(error) => this.action_error = Some((number, error.to_string().into())),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn fetch(&mut self, cursor: Option<String>, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.error = None;
        let Availability::Ready(target) = self.availability.clone() else {
            self.loading = false;
            cx.notify();
            return;
        };
        let token = match self.accounts().token(&target.connected) {
            Some(token) => token,
            None if crate::http::replaying() => Secret::new(""),
            None => {
                self.loading = false;
                self.error = Some("This account has no token. Sign in again in Accounts.".into());
                cx.notify();
                return;
            }
        };
        self.loading = true;
        let api = target.connected.api_token(&token);
        let provider = provider(
            target.connected.account.kind,
            &target.connected.account.host,
            self.env.client.clone(),
        );
        let query = ItemQuery {
            state: self.filter,
            cursor,
        };
        let kind = self.kind;
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = match kind {
                HostedKind::PullRequests => provider
                    .pull_requests(&api, &target.full_name, &query)
                    .await
                    .map(|page| (page.items.into_iter().map(Item::from).collect(), page.next)),
                HostedKind::Issues => provider
                    .issues(&api, &target.full_name, &query)
                    .await
                    .map(|page| (page.items.into_iter().map(Item::from).collect(), page.next)),
            };
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok((items, next)) => {
                        let items: Vec<Item> = items;
                        this.items.extend(items);
                        this.next = next;
                        if this.selected.is_none() {
                            this.selected = this.items.first().map(|item| item.number);
                        }
                    }
                    Err(bonsai_providers::Error::NotFound) if kind == HostedKind::Issues => {
                        this.error = Some("This repository has no issue tracker.".into());
                    }
                    Err(error) => this.error = Some(error.to_string().into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Whether the selected pull request can be checked out: one from a
    /// fork needs a ref the provider publishes, which Bitbucket has not.
    fn can_check_out(&self, item: &Item) -> bool {
        let (Some(pull), Availability::Ready(target)) = (&item.pull, &self.availability) else {
            return false;
        };
        pull.head_fork.is_none() || target.connected.account.kind != ProviderKind::Bitbucket
    }

    /// Fetches the selected pull request's branch and switches to it. One
    /// from the same repository becomes its own tracking branch; one from a
    /// fork becomes `pr-<number>`.
    pub fn check_out(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.selected().cloned() else {
            return;
        };
        let (Some(pull), Availability::Ready(target)) = (item.pull.clone(), &self.availability)
        else {
            return;
        };
        if !self.can_check_out(&item) {
            return;
        }
        let remote = target.remote.clone();
        let number = item.number;
        let name = format!("Switch to #{number}");
        if pull.head_fork.is_none() {
            let exists = self
                .model
                .read(cx)
                .snapshot()
                .is_some_and(|s| s.branches.iter().any(|b| b.name == pull.head));
            let head = pull.head;
            self.model.update(cx, |model, cx| {
                model.run_network(
                    name,
                    move |repository, progress, cancel| {
                        repository.fetch(Some(&remote), false, Some(progress), Some(cancel))?;
                        if exists {
                            repository.checkout_branch(&head)
                        } else {
                            repository.checkout_remote_branch(&remote, &head, &head)
                        }
                    },
                    cx,
                );
            });
            return;
        }
        let provider = provider(
            target.connected.account.kind,
            &target.connected.account.host,
            self.env.client.clone(),
        );
        let Some(source) = provider.pull_request_ref(number) else {
            return;
        };
        let local = format!("pr-{number}");
        self.model.update(cx, |model, cx| {
            model.run_network(
                name,
                move |repository, progress, cancel| {
                    repository.fetch_into_branch(
                        &remote,
                        &format!("refs/{source}"),
                        &local,
                        Some(progress),
                        Some(cancel),
                    )
                },
                cx,
            );
        });
    }

    fn render_row(&self, item: &Item, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let selected = self.selected == Some(item.number);
        let number = item.number;
        let color = state_color(item.state, cx);
        let mut meta = format!(
            "#{} · {} · {}",
            item.number,
            item.author.name(),
            when(&item.updated_at)
        );
        if let Some(comments) = item.comments.filter(|n| *n > 0) {
            let _ = write!(meta, " · {}", comment_count(comments));
        }
        div()
            .id(("hosted.row", item.number))
            .flex()
            .items_start()
            .gap(px(10.))
            .px(px(12.))
            .py(px(8.))
            .cursor_pointer()
            .border_b_1()
            .border_color(c.border)
            .when(selected, |this| this.bg(c.accent))
            .when(!selected, |this| {
                this.hover(|this| this.bg(c.accent.opacity(0.5)))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.select(number, cx)))
            .child(
                div().pt(px(2.)).child(
                    lucide(state_icon(self.kind, item.state))
                        .size(px(16.))
                        .text_color(color),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_sm().truncate().child(item.title.clone()))
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .text_color(c.muted_foreground)
                            .child(meta),
                    )
                    .when_some(item.pull.as_ref(), |this, pull| {
                        this.child(
                            div()
                                .text_xs()
                                .truncate()
                                .font_family(theme.fonts.mono.clone())
                                .text_color(c.muted_foreground)
                                .child(format!("{} → {}", pull.head, pull.base)),
                        )
                    }),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let noun = self.kind.noun();
        let (icon, title, description, accounts): (_, SharedString, SharedString, bool) =
            match &self.availability {
                Availability::NoRemote => (
                    LucideIcon::Cloud,
                    format!("No {noun} here").into(),
                    "This repository has no remote on GitHub, GitLab or Bitbucket.".into(),
                    false,
                ),
                Availability::NoAccount(host) => (
                    LucideIcon::UserRound,
                    format!("Connect {host}").into(),
                    format!("Add an account for {host} to see this repository's {noun}.").into(),
                    true,
                ),
                Availability::Ready(_) => {
                    if let Some(error) = &self.error {
                        (
                            LucideIcon::TriangleAlert,
                            format!("Bonsai could not load the {noun}").into(),
                            error.clone(),
                            false,
                        )
                    } else if self.items.is_empty() && !self.loading {
                        (
                            state_icon(self.kind, ItemState::Open),
                            format!("No {noun}").into(),
                            match self.filter {
                                StateFilter::Open => format!("There are no open {noun}."),
                                StateFilter::Closed => format!("There are no closed {noun}."),
                                StateFilter::All => format!("This repository has no {noun} yet."),
                            }
                            .into(),
                            false,
                        )
                    } else {
                        return None;
                    }
                }
            };
        let theme = UiTheme::read(cx);
        Some(
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.))
                .child(
                    Empty::new().w_full().max_w(px(360.)).header(
                        EmptyHeader::new()
                            .media(
                                EmptyMedia::new()
                                    .with_variant(EmptyMediaVariant::Icon)
                                    .child(
                                        lucide(icon)
                                            .size(px(16.))
                                            .text_color(theme.colors.foreground),
                                    ),
                            )
                            .title(EmptyTitle::new().child(title))
                            .description(EmptyDescription::new().child(description)),
                    ),
                )
                .when(accounts, |this| {
                    this.flex_col().gap(px(12.)).child(
                        Button::new("hosted.accounts")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .label("Accounts…")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(HostedEvent::ManageAccounts)),
                            ),
                    )
                })
                .into_any_element(),
        )
    }

    /// The selected item's description and actions, for the detail pane.
    pub fn render_detail(view: &Entity<Self>, cx: &mut App) -> AnyElement {
        let this = view.read(cx);
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let Some(item) = this.selected().cloned() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(c.muted_foreground)
                .child(match this.kind {
                    HostedKind::PullRequests => "Select a pull request",
                    HostedKind::Issues => "Select an issue",
                })
                .into_any_element();
        };
        let kind = this.kind;
        let can_check_out = this.can_check_out(&item);
        let color = state_color(item.state, cx);
        let web = item.web_url.clone();
        let mut meta = format!(
            "{} opened {} · updated {}",
            item.author.name(),
            when(&item.created_at),
            when(&item.updated_at)
        );
        if let Some(comments) = item.comments {
            let _ = write!(meta, " · {}", comment_count(comments));
        }
        let mut labels = div().flex().flex_wrap().gap(px(6.));
        for (i, label) in item.labels.iter().enumerate() {
            let mut tag = Tag::new(("hosted.label", i), label.name.clone());
            if let Some(color) = label_color(label) {
                tag = tag.dot(color);
            }
            labels = labels.child(tag);
        }
        let body = if item.body.trim().is_empty() {
            div()
                .text_sm()
                .italic()
                .text_color(c.muted_foreground)
                .child("No description.")
                .into_any_element()
        } else {
            markdown(("hosted.body", item.number), item.body.clone(), cx).into_any_element()
        };
        let check_out = view.clone();
        let toggle = view.clone();
        let can_change_state = this.can_change_state(&item);
        let changing_state = this.changing_state == Some(item.number);
        let noun = match kind {
            HostedKind::PullRequests => "Pull Request",
            HostedKind::Issues => "Issue",
        };
        let toggle_label = match (item.state.is_open(), changing_state) {
            (true, false) => format!("Close {noun}"),
            (true, true) => "Closing…".to_owned(),
            (false, false) => format!("Reopen {noun}"),
            (false, true) => "Reopening…".to_owned(),
        };
        let posting = this.posting == Some(item.number);
        let reply = this.composer.clone().map(|composer| {
            let empty = composer.read(cx).value().trim().is_empty();
            let send = view.clone();
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(Textarea::new(&composer).aria_label("New comment"))
                .child(
                    div().flex().justify_end().child(
                        Button::new("hosted.comment")
                            .size(ButtonSize::Sm)
                            .label(if posting { "Commenting…" } else { "Comment" })
                            .disabled(empty || posting)
                            .on_click(move |_, window, cx| {
                                send.update(cx, |this, cx| this.comment(window, cx));
                            }),
                    ),
                )
        });
        let mergeable = item.pull.is_some() && item.state == ItemState::Open;
        let merging = this.merging == Some(item.number);
        let merge_method = this.merge_method;
        let action_error = this
            .action_error
            .as_ref()
            .filter(|(n, _)| *n == item.number)
            .map(|(_, e)| e.clone());
        let merge_row = mergeable.then(|| {
            let selected = MERGE_METHODS
                .iter()
                .position(|(m, _, _)| *m == merge_method)
                .map(|i| SharedString::from(i.to_string()));
            let mut group = ToggleGroup::new("hosted.merge-method")
                .aria_label("How to merge")
                .segmented(true)
                .value(selected);
            for (i, (_, short, _)) in MERGE_METHODS.iter().enumerate() {
                group = group.item(
                    ToggleGroupItem::new(
                        SharedString::from(format!("hosted.merge-method.{i}")),
                        i.to_string(),
                    )
                    .aria_label(*short)
                    .child(div().text_xs().child(*short)),
                );
            }
            let on_method = view.clone();
            let group = group.on_change(move |next, _, cx| {
                if let Some((method, _, _)) = next
                    .first()
                    .and_then(|i| i.parse::<usize>().ok())
                    .and_then(|i| MERGE_METHODS.get(i))
                {
                    on_method.update(cx, |this, cx| this.set_merge_method(*method, cx));
                }
            });
            let label = MERGE_METHODS
                .iter()
                .find(|(m, _, _)| *m == merge_method)
                .map_or("Merge Pull Request", |(_, _, label)| label);
            let on_merge = view.clone();
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .p(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(c.border)
                .child(
                    lucide(LucideIcon::GitMerge)
                        .size(px(16.))
                        .text_color(c.success),
                )
                .child(div().flex_1().text_sm().child("Ready to merge"))
                .child(group)
                .child(
                    Button::new("hosted.merge")
                        .size(ButtonSize::Sm)
                        .label(if merging { "Merging…" } else { label })
                        .disabled(merging)
                        .on_click(move |_, _, cx| on_merge.update(cx, Self::merge)),
                )
        });
        let conversation = match this.conversation(item.number) {
            None | Some(Conversation::Loading) => div()
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Loading comments…")
                .into_any_element(),
            Some(Conversation::Failed(error)) => div()
                .text_sm()
                .text_color(c.destructive)
                .child(error.clone())
                .into_any_element(),
            Some(Conversation::Loaded(comments)) if comments.is_empty() => div()
                .text_sm()
                .text_color(c.muted_foreground)
                .child("No comments yet.")
                .into_any_element(),
            Some(Conversation::Loaded(comments)) => {
                let mut list = div().flex().flex_col().gap(px(10.));
                for (i, comment) in comments.iter().enumerate() {
                    list = list.child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .p(px(12.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(c.border)
                            .child(
                                div()
                                    .flex()
                                    .gap(px(6.))
                                    .text_xs()
                                    .child(
                                        div()
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .child(comment.author.name().to_owned()),
                                    )
                                    .child(
                                        div()
                                            .text_color(c.muted_foreground)
                                            .child(when(&comment.created_at)),
                                    ),
                            )
                            .child(markdown(
                                SharedString::from(format!("hosted.comment.{}.{i}", item.number)),
                                comment.body.clone(),
                                cx,
                            )),
                    );
                }
                list.into_any_element()
            }
        };
        div()
            .id("hosted.detail")
            .size_full()
            .overflow_y_scroll()
            .p(px(20.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(4.))
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(999.))
                            .bg(color.opacity(0.15))
                            .text_color(color)
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child(
                                lucide(state_icon(kind, item.state))
                                    .size(px(12.))
                                    .text_color(color),
                            )
                            .child(state_label(item.state)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(c.muted_foreground)
                            .child(format!("#{}", item.number)),
                    ),
            )
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(item.title.clone()),
            )
            .child(div().text_xs().text_color(c.muted_foreground).child(meta))
            .when_some(item.pull.clone(), |this, pull| {
                let head = match &pull.head_fork {
                    Some(fork) => format!("{fork}:{}", pull.head),
                    None => pull.head.clone(),
                };
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .text_xs()
                        .font_family(theme.fonts.mono.clone())
                        .child(branch_chip(head, cx))
                        .child(
                            lucide(LucideIcon::ArrowRight)
                                .size(px(12.))
                                .text_color(c.muted_foreground),
                        )
                        .child(branch_chip(pull.base, cx)),
                )
            })
            .when(!item.labels.is_empty(), |this| this.child(labels))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .when(item.pull.is_some(), |this| {
                        this.child(
                            Button::new("hosted.check-out")
                                .variant(ButtonVariant::Default)
                                .size(ButtonSize::Sm)
                                .child(
                                    lucide(LucideIcon::GitBranch)
                                        .size(px(14.))
                                        .text_color(c.primary_foreground),
                                )
                                .label("Check Out")
                                .disabled(!can_check_out)
                                .on_click(move |_, _, cx| {
                                    check_out.update(cx, Self::check_out);
                                }),
                        )
                    })
                    .child(
                        Button::new("hosted.browser")
                            .variant(ButtonVariant::Outline)
                            .size(ButtonSize::Sm)
                            .child(
                                lucide(LucideIcon::ExternalLink)
                                    .size(px(14.))
                                    .text_color(c.foreground),
                            )
                            .label("Open in Browser")
                            .on_click(move |_, _, cx| cx.open_url(&web)),
                    )
                    .when(can_change_state, |this| {
                        this.child(
                            Button::new("hosted.toggle-open")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .label(toggle_label)
                                .disabled(changing_state)
                                .on_click(move |_, _, cx| toggle.update(cx, Self::toggle_open)),
                        )
                    }),
            )
            .children(merge_row)
            .when_some(action_error, |this, error| {
                this.child(div().text_xs().text_color(c.destructive).child(error))
            })
            .child(div().h(px(1.)).bg(c.border))
            .child(body)
            .child(
                div()
                    .pt(px(6.))
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("Conversation"),
            )
            .child(conversation)
            .children(reply)
            .into_any_element()
    }
}

fn state_color(state: ItemState, cx: &App) -> gpui_kit::Hsla {
    let c = &UiTheme::read(cx).colors;
    match state {
        ItemState::Open => c.success,
        ItemState::Draft => c.muted_foreground,
        ItemState::Merged => c.info,
        ItemState::Closed => c.destructive,
    }
    .into()
}

fn branch_chip(name: String, cx: &App) -> impl IntoElement {
    let c = &UiTheme::read(cx).colors;
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(c.muted)
        .text_color(c.foreground)
        .child(name)
}

impl Render for HostedView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.load_conversation(cx);
        self.ensure_composer(window, cx);
        let theme = UiTheme::read(cx).clone();
        let c = &theme.colors;
        let selected = FILTERS
            .iter()
            .position(|(f, _)| *f == self.filter)
            .map(|i| SharedString::from(i.to_string()));
        let mut filter = ToggleGroup::new("hosted.filter")
            .aria_label("Which to show")
            .segmented(true)
            .value(selected);
        for (i, (_, label)) in FILTERS.iter().enumerate() {
            filter = filter.item(
                ToggleGroupItem::new(
                    SharedString::from(format!("hosted.filter.{i}")),
                    i.to_string(),
                )
                .aria_label(*label)
                .child(div().text_xs().child(*label)),
            );
        }
        let view = cx.entity();
        let filter = filter.on_change(move |next, _, cx| {
            if let Some((state, _)) = next
                .first()
                .and_then(|i| i.parse::<usize>().ok())
                .and_then(|i| FILTERS.get(i))
            {
                view.update(cx, |this, cx| this.set_filter(*state, cx));
            }
        });
        let ready = matches!(self.availability, Availability::Ready(_));
        let title = match &self.availability {
            Availability::Ready(target) => target.full_name.clone(),
            _ => String::new(),
        };
        let header = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .px(px(12.))
            .py(px(10.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .text_color(c.muted_foreground)
                            .child(title),
                    )
                    .when(self.loading, |this| {
                        this.child(Spinner::new("hosted.loading"))
                    })
                    .when(ready, |this| {
                        let kind = self.kind;
                        this.child(
                            Button::new("hosted.create")
                                .variant(ButtonVariant::Outline)
                                .size(ButtonSize::Sm)
                                .child(
                                    lucide(match kind {
                                        HostedKind::PullRequests => {
                                            LucideIcon::GitPullRequestCreate
                                        }
                                        HostedKind::Issues => LucideIcon::CirclePlus,
                                    })
                                    .size(px(14.))
                                    .text_color(c.foreground),
                                )
                                .label("New")
                                .aria_label(match kind {
                                    HostedKind::PullRequests => "New Pull Request",
                                    HostedKind::Issues => "New Issue",
                                })
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(match kind {
                                        HostedKind::PullRequests => HostedEvent::CreatePullRequest,
                                        HostedKind::Issues => HostedEvent::CreateIssue,
                                    });
                                })),
                        )
                    })
                    .child(
                        Button::new("hosted.reload")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::IconSm)
                            .child(
                                lucide(LucideIcon::RefreshCw)
                                    .size(px(14.))
                                    .text_color(c.foreground),
                            )
                            .aria_label("Reload")
                            .disabled(!ready || self.loading)
                            .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
                    ),
            )
            .child(filter);
        let status = self.render_status(cx);
        let mut list = div()
            .id("hosted.list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .flex()
            .flex_col();
        let items = self.items.clone();
        for item in &items {
            list = list.child(self.render_row(item, cx));
        }
        if self.next.is_some() && !self.loading {
            list = list.child(
                div().p(px(12.)).child(
                    Button::new("hosted.more")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("Load More")
                        .on_click(cx.listener(|this, _, _, cx| this.load_more(cx))),
                ),
            );
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(ready, |this| this.child(header))
            .child(match status {
                Some(status) if items.is_empty() || self.error.is_some() => status,
                _ => list.into_any_element(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rfc3339_times() {
        let at = |s| {
            parse_time(s)
                .unwrap()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        };
        assert_eq!(at("1970-01-01T00:00:00Z"), 0);
        assert_eq!(at("2026-11-12T09:00:00Z"), 1_794_474_000);
        assert_eq!(at("2026-11-02T09:00:00.000000+00:00"), 1_793_610_000);
        assert_eq!(at("2026-11-12T10:30:00+01:30"), 1_794_474_000);
        assert_eq!(parse_time("yesterday"), None);
    }
}
