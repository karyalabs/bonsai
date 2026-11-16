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
}
