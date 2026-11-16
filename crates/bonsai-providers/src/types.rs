//! What every provider shares (CONTEXT.md: Provider, Account, Remote
//! repository).

use serde::{Deserialize, Serialize};

use crate::BoxFuture;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderKind {
    GitHub,
    GitLab,
    Bitbucket,
}

impl ProviderKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Bitbucket => "Bitbucket",
        }
    }
}

/// One signed-in identity on one provider host. The token is kept in the
/// Keychain under (`host`, `username`), not here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub kind: ProviderKind,
    /// The web host, such as `github.com` or `gitlab.example.com`.
    pub host: String,
    /// The API base URL, such as `https://api.github.com`.
    pub api_url: String,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

/// A repository a provider account can see.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRepository {
    /// `owner/name`, or the full namespace path on GitLab.
    pub full_name: String,
    pub owner: String,
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub default_branch: Option<String>,
    pub clone_https: String,
    pub clone_ssh: Option<String>,
    pub web_url: String,
    /// When it last changed, as the provider reports it (RFC 3339).
    pub updated_at: Option<String>,
}

/// One page of results and where the next one starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Pass back in [`RepositoryQuery::cursor`] for the next page.
    pub next: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepositoryQuery {
    /// Text to search names and descriptions for; empty lists everything.
    pub search: String,
    /// The page to fetch, from [`Page::next`].
    pub cursor: Option<String>,
}

/// A repository to create, for publishing a local one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRepository {
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    /// An organization or group to create it in; `None` is the account.
    pub owner: Option<String>,
}

/// Where a pull request or issue stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemState {
    Open,
    /// An open pull request marked as not ready for review.
    Draft,
    Merged,
    Closed,
}

impl ItemState {
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, Self::Open | Self::Draft)
    }
}

/// Which pull requests or issues to list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum StateFilter {
    #[default]
    Open,
    /// Closed and merged.
    Closed,
    All,
}

impl StateFilter {
    #[must_use]
    pub fn keeps(self, state: ItemState) -> bool {
        match self {
            Self::Open => state.is_open(),
            Self::Closed => !state.is_open(),
            Self::All => true,
        }
    }
}

/// A person on the provider: an author or a reporter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

impl Person {
    #[must_use]
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.username)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    /// Six hex digits, without `#`, when the provider has colors.
    pub color: Option<String>,
}

/// A pull request (a merge request on GitLab).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    /// Markdown, possibly empty.
    pub body: String,
    pub author: Person,
    pub state: ItemState,
    /// The branch with the changes.
    pub head: String,
    /// The repository the head branch is in, when it is a fork.
    pub head_fork: Option<String>,
    /// The branch it merges into.
    pub base: String,
    pub labels: Vec<Label>,
    /// Comments, when the list reports them.
    pub comments: Option<u32>,
    /// RFC 3339.
    pub created_at: String,
    pub updated_at: String,
    pub web_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: Person,
    /// `Open` or `Closed`.
    pub state: ItemState,
    pub labels: Vec<Label>,
    pub comments: Option<u32>,
    pub created_at: String,
    pub updated_at: String,
    pub web_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemQuery {
    pub state: StateFilter,
    /// The page to fetch, from [`Page::next`].
    pub cursor: Option<String>,
}

/// A pull request to open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPullRequest {
    pub title: String,
    pub body: String,
    /// The branch with the changes, in the same repository.
    pub head: String,
    pub base: String,
    pub draft: bool,
}

/// What a provider can do, so the UI hides what it can't.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub search: bool,
    pub create: bool,
    pub create_in_organization: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("Could not reach {host}: {message}")]
    Network { host: String, message: String },
    #[error("The sign-in has expired or was revoked. Sign in to {0} again.")]
    Unauthorized(String),
    #[error("{0} is rate limiting requests. Try again later.")]
    RateLimited(String, Option<u64>),
    #[error("Not found, or this account can't see it.")]
    NotFound,
    /// The server refused the request and said why, such as a name that is
    /// already taken.
    #[error("{0}")]
    Rejected(String),
    #[error("{host} answered with HTTP {status}.")]
    Status { host: String, status: u16 },
    #[error("Unexpected response from {host}: {message}")]
    Parse { host: String, message: String },
}

impl Error {
    /// For rate limits: when the limit resets, in seconds since the epoch.
    #[must_use]
    pub fn retry_at(&self) -> Option<u64> {
        match self {
            Self::RateLimited(_, at) => *at,
            _ => None,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A Git hosting provider's API, for one host.
pub trait Provider: Send + Sync {
    fn kind(&self) -> ProviderKind;

    fn capabilities(&self) -> Capabilities;

    /// The account the token belongs to.
    fn current_user<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Account>>;

    /// Repositories the account owns, collaborates on, or sees through its
    /// organizations, most recently updated first.
    fn repositories<'a>(
        &'a self,
        token: &'a str,
        query: &'a RepositoryQuery,
    ) -> BoxFuture<'a, Result<Page<RemoteRepository>>>;

    fn repository<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
    ) -> BoxFuture<'a, Result<RemoteRepository>>;

    fn create_repository<'a>(
        &'a self,
        token: &'a str,
        new: &'a NewRepository,
    ) -> BoxFuture<'a, Result<RemoteRepository>>;

    /// The user name Git sends with a token over HTTPS.
    fn git_username(&self, account: &Account) -> String;

    /// Pull requests into `full_name`, most recently updated first.
    fn pull_requests<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<PullRequest>>>;

    /// Issues of `full_name`, most recently updated first. [`Error::NotFound`]
    /// when the repository has no issue tracker.
    fn issues<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<Issue>>>;

    fn create_pull_request<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        new: &'a NewPullRequest,
    ) -> BoxFuture<'a, Result<PullRequest>>;

    /// The ref a pull request's head can be fetched from, for one from a
    /// fork, such as `pull/7/head`. `None` when the provider has none.
    fn pull_request_ref(&self, number: u64) -> Option<String>;
}
