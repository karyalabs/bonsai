//! Git hosting provider APIs for Bonsai (ADR 3): one [`Provider`] trait,
//! implemented for GitHub, GitLab and Bitbucket, over an [`HttpClient`]
//! the app supplies.

mod bitbucket;
mod github;
mod gitlab;
mod http;
mod oauth;
mod remote_url;
mod types;
mod web_links;

pub use bitbucket::{Bitbucket, BitbucketCredential};
pub use github::{DeviceCode, DevicePoll, GitHub};
pub use gitlab::GitLab;
pub use http::{BoxFuture, HttpClient, Method, Request, Response, form_encode, percent_encode};
pub use oauth::{Pkce, Tokens};
pub use remote_url::hosted_repository;
pub use types::{
    Account, Capabilities, ChangedFile, Check, CheckState, Comment, Error, FileChange, Issue,
    ItemEdit, ItemKind, ItemQuery, ItemState, Label, LineComment, LineTarget, MergeMethod,
    NewIssue, NewPullRequest, NewRepository, Page, Person, Provider, ProviderKind, PullRequest,
    RemoteRepository, RepositoryQuery, Result, StateFilter, Verdict, count_lines,
};
pub use web_links::WebRepository;
