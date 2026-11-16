//! Git hosting provider APIs for Bonsai (ADR 3): one [`Provider`] trait,
//! implemented for GitHub, GitLab and Bitbucket, over an [`HttpClient`]
//! the app supplies.

mod bitbucket;
mod github;
mod gitlab;
mod http;
mod oauth;
mod types;

pub use bitbucket::{Bitbucket, BitbucketCredential};
pub use github::{DeviceCode, DevicePoll, GitHub};
pub use gitlab::GitLab;
pub use http::{BoxFuture, HttpClient, Method, Request, Response, form_encode, percent_encode};
pub use oauth::{Pkce, Tokens};
pub use types::{
    Account, Capabilities, Error, NewRepository, Page, Provider, ProviderKind, RemoteRepository,
    RepositoryQuery, Result,
};
