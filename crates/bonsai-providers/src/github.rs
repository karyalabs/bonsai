//! GitHub and GitHub Enterprise Server: OAuth device flow and REST v3.

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::json;

use crate::{
    Account, BoxFuture, Capabilities, ChangedFile, Check, CheckState, Comment, Error, FileChange,
    HttpClient, Issue, ItemKind, ItemQuery, ItemState, Label, LineComment, MergeMethod, NewIssue,
    NewPullRequest, NewRepository, Page, Person, Provider, ProviderKind, PullRequest,
    RemoteRepository, RepositoryQuery, Request, Response, Result, StateFilter, Verdict,
    form_encode, percent_encode,
};

const PER_PAGE: u32 = 50;
/// The scopes Bonsai asks for: private repositories and organization lists.
pub const SCOPES: &str = "repo read:org";

pub struct GitHub<C> {
    client: C,
    /// `github.com`, or an Enterprise Server host.
    host: String,
    api: String,
}

impl<C: HttpClient> GitHub<C> {
    /// github.com.
    pub fn new(client: C) -> Self {
        Self {
            client,
            host: "github.com".to_owned(),
            api: "https://api.github.com".to_owned(),
        }
    }

    /// A GitHub Enterprise Server host, such as `github.example.com`.
    pub fn enterprise(client: C, host: &str) -> Self {
        let host = host.trim_end_matches('/').to_ascii_lowercase();
        Self {
            client,
            api: format!("https://{host}/api/v3"),
            host,
        }
    }

    async fn send(&self, request: Request, token: Option<&str>) -> Result<Response> {
        let mut request = request
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "Bonsai");
        if let Some(token) = token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = self
            .client
            .send(request)
            .await
            .map_err(|message| Error::Network {
                host: self.host.clone(),
                message,
            })?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(self.error(&response))
        }
    }

    fn error(&self, response: &Response) -> Error {
        let message = serde_json::from_slice::<ErrorBody>(&response.body).ok();
        let remaining = response.header("x-ratelimit-remaining");
        match response.status {
            401 => Error::Unauthorized(self.host.clone()),
            403 | 429 if remaining == Some("0") || response.status == 429 => Error::RateLimited(
                "GitHub".to_owned(),
                response
                    .header("x-ratelimit-reset")
                    .and_then(|r| r.parse().ok()),
            ),
            404 => Error::NotFound,
            403 | 422 => match message {
                Some(body) => Error::Rejected(body.describe()),
                None => self.status(response.status),
            },
            status => self.status(status),
        }
    }

    fn status(&self, status: u16) -> Error {
        Error::Status {
            host: self.host.clone(),
            status,
        }
    }

    fn parse<'de, T: Deserialize<'de>>(&self, body: &'de [u8]) -> Result<T> {
        serde_json::from_slice(body).map_err(|error| Error::Parse {
            host: self.host.clone(),
            message: error.to_string(),
        })
    }

    fn web(&self) -> String {
        format!("https://{}", self.host)
    }

    /// Starts the device flow: show [`DeviceCode::user_code`] and open
    /// [`DeviceCode::verification_uri`], then [`Self::poll_device`] every
    /// `interval` seconds.
    pub async fn start_device_flow(&self, client_id: &str) -> Result<DeviceCode> {
        let body = form_encode(&[("client_id", client_id), ("scope", SCOPES)]);
        let response = self
            .send(
                Request::post(format!("{}/login/device/code", self.web()), body)
                    .header("Content-Type", "application/x-www-form-urlencoded"),
                None,
            )
            .await?;
        self.parse(&response.body)
    }

    pub async fn poll_device(&self, client_id: &str, device_code: &str) -> Result<DevicePoll> {
        let body = form_encode(&[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ]);
        let response = self
            .send(
                Request::post(format!("{}/login/oauth/access_token", self.web()), body)
                    .header("Content-Type", "application/x-www-form-urlencoded"),
                None,
            )
            .await?;
        let poll: TokenResponse = self.parse(&response.body)?;
        Ok(match (poll.access_token, poll.error.as_deref()) {
            (Some(token), _) => DevicePoll::Token(token),
            (None, Some("authorization_pending")) => DevicePoll::Pending,
            (None, Some("slow_down")) => DevicePoll::SlowDown(poll.interval.unwrap_or(10)),
            (None, Some("expired_token")) => DevicePoll::Expired,
            (None, Some("access_denied")) => DevicePoll::Denied,
            (None, _) => {
                return Err(Error::Rejected(poll.error_description.unwrap_or_else(
                    || "GitHub did not finish the sign-in.".to_owned(),
                )));
            }
        })
    }

    async fn organizations(&self, token: &str) -> Result<Vec<String>> {
        let response = self
            .send(
                Request::get(format!("{}/user/orgs?per_page=100", self.api)),
                Some(token),
            )
            .await?;
        let orgs: Vec<Login> = self.parse(&response.body)?;
        Ok(orgs.into_iter().map(|o| o.login).collect())
    }

    async fn list(&self, token: &str, query: &RepositoryQuery) -> Result<Page<RemoteRepository>> {
        let search = query.search.trim();
        let url = if let Some(cursor) = &query.cursor {
            // Cursors are the API's own next-page links, on this host only.
            if !cursor.starts_with(&format!("{}/", self.api)) {
                return Err(Error::Rejected(
                    "That page link is not from GitHub.".to_owned(),
                ));
            }
            cursor.clone()
        } else if search.is_empty() {
            format!(
                "{}/user/repos?per_page={PER_PAGE}&sort=updated&affiliation=owner,collaborator,organization_member",
                self.api
            )
        } else {
            // Search the account's repositories and its organizations'.
            let user = self.current_user(token).await?;
            let mut q = format!(
                "{search} in:name,description fork:true user:{}",
                user.username
            );
            for org in self.organizations(token).await? {
                let _ = write!(q, " org:{org}");
            }
            format!(
                "{}/search/repositories?per_page={PER_PAGE}&sort=updated&q={}",
                self.api,
                percent_encode(&q)
            )
        };
        let is_search = url.starts_with(&format!("{}/search/", self.api));
        let response = self.send(Request::get(url), Some(token)).await?;
        let items: Vec<Repo> = if is_search {
            self.parse::<SearchResults>(&response.body)?.items
        } else {
            self.parse(&response.body)?
        };
        Ok(Page {
            items: items.into_iter().map(Repo::into_remote).collect(),
            next: response.header("link").and_then(next_link),
        })
    }
}

impl<C: HttpClient> GitHub<C> {
    fn repo_url(&self, full_name: &str) -> Result<String> {
        let (owner, name) = full_name.split_once('/').ok_or(Error::NotFound)?;
        Ok(format!(
            "{}/repos/{}/{}",
            self.api,
            percent_encode(owner),
            percent_encode(name)
        ))
    }

    /// The first page's URL, or the cursor, checked to be on this host.
    fn page_url(&self, first: String, cursor: Option<&String>) -> Result<String> {
        match cursor {
            Some(cursor) if cursor.starts_with(&format!("{}/", self.api)) => Ok(cursor.clone()),
            Some(_) => Err(Error::Rejected(
                "That page link is not from GitHub.".to_owned(),
            )),
            None => Ok(first),
        }
    }

    async fn list_pulls(
        &self,
        token: &str,
        full_name: &str,
        query: &ItemQuery,
    ) -> Result<Page<PullRequest>> {
        let first = format!(
            "{}/pulls?state={}&sort=updated&direction=desc&per_page={PER_PAGE}",
            self.repo_url(full_name)?,
            state_param(query.state)
        );
        let url = self.page_url(first, query.cursor.as_ref())?;
        let response = self.send(Request::get(url), Some(token)).await?;
        let pulls: Vec<Pull> = self.parse(&response.body)?;
        Ok(Page {
            items: pulls
                .into_iter()
                .map(|pull| pull.into_pull_request(full_name))
                .collect(),
            next: response.header("link").and_then(next_link),
        })
    }

    async fn list_issues(
        &self,
        token: &str,
        full_name: &str,
        query: &ItemQuery,
    ) -> Result<Page<Issue>> {
        let first = format!(
            "{}/issues?state={}&sort=updated&direction=desc&per_page={PER_PAGE}",
            self.repo_url(full_name)?,
            state_param(query.state)
        );
        let url = self.page_url(first, query.cursor.as_ref())?;
        let response = self.send(Request::get(url), Some(token)).await?;
        let issues: Vec<IssueBody> = self.parse(&response.body)?;
        Ok(Page {
            // GitHub lists pull requests as issues too.
            items: issues
                .into_iter()
                .filter(|issue| issue.pull_request.is_none())
                .map(IssueBody::into_issue)
                .collect(),
            next: response.header("link").and_then(next_link),
        })
    }

    async fn open_pull(
        &self,
        token: &str,
        full_name: &str,
        new: &NewPullRequest,
    ) -> Result<PullRequest> {
        let body = json!({
            "title": new.title,
            "body": new.body,
            "head": new.head,
            "base": new.base,
            "draft": new.draft,
        });
        let response = self
            .send(
                Request::post(
                    format!("{}/pulls", self.repo_url(full_name)?),
                    body.to_string(),
                )
                .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
        Ok(self
            .parse::<Pull>(&response.body)?
            .into_pull_request(full_name))
    }
}

impl<C: HttpClient> GitHub<C> {
    async fn list_comments(
        &self,
        token: &str,
        full_name: &str,
        number: u64,
    ) -> Result<Vec<Comment>> {
        // Issue comments are a pull request's conversation too.
        let url = format!(
            "{}/issues/{number}/comments?per_page=100",
            self.repo_url(full_name)?
        );
        let response = self.send(Request::get(url), Some(token)).await?;
        let comments: Vec<CommentBody> = self.parse(&response.body)?;
        Ok(comments
            .into_iter()
            .filter(|c| {
                c.user
                    .as_ref()
                    .is_none_or(|u| u.kind.as_deref() != Some("Bot"))
            })
            .map(CommentBody::into_comment)
            .collect())
    }

    async fn post_comment(
        &self,
        token: &str,
        full_name: &str,
        number: u64,
        body: &str,
    ) -> Result<Comment> {
        let response = self
            .send(
                Request::post(
                    format!("{}/issues/{number}/comments", self.repo_url(full_name)?),
                    json!({ "body": body }).to_string(),
                )
                .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
        Ok(self.parse::<CommentBody>(&response.body)?.into_comment())
    }

    async fn update_state(
        &self,
        token: &str,
        full_name: &str,
        number: u64,
        open: bool,
    ) -> Result<()> {
        // The issues endpoint closes pull requests too.
        self.send(
            Request::patch(
                format!("{}/issues/{number}", self.repo_url(full_name)?),
                json!({ "state": if open { "open" } else { "closed" } }).to_string(),
            )
            .header("Content-Type", "application/json"),
            Some(token),
        )
        .await?;
        Ok(())
    }

    async fn merge(
        &self,
        token: &str,
        full_name: &str,
        number: u64,
        method: MergeMethod,
    ) -> Result<()> {
        let method = match method {
            MergeMethod::Merge => "merge",
            MergeMethod::Squash => "squash",
            MergeMethod::Rebase => "rebase",
        };
        self.send(
            Request::put(
                format!("{}/pulls/{number}/merge", self.repo_url(full_name)?),
                json!({ "merge_method": method }).to_string(),
            )
            .header("Content-Type", "application/json"),
            Some(token),
        )
        .await?;
        Ok(())
    }

    async fn open_issue(&self, token: &str, full_name: &str, new: &NewIssue) -> Result<Issue> {
        let response = self
            .send(
                Request::post(
                    format!("{}/issues", self.repo_url(full_name)?),
                    json!({ "title": new.title, "body": new.body }).to_string(),
                )
                .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
        Ok(self.parse::<IssueBody>(&response.body)?.into_issue())
    }
}

#[derive(Deserialize)]
struct CommentUser {
    login: String,
    avatar_url: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
struct CommentBody {
    user: Option<CommentUser>,
    body: Option<String>,
    created_at: String,
}

impl CommentBody {
    fn into_comment(self) -> Comment {
        Comment {
            author: self
                .user
                .map(|u| Person {
                    username: u.login,
                    display_name: None,
                    avatar_url: u.avatar_url,
                })
                .unwrap_or_default(),
            body: self.body.unwrap_or_default(),
            created_at: self.created_at,
        }
    }
}

fn state_param(state: StateFilter) -> &'static str {
    match state {
        StateFilter::Open => "open",
        StateFilter::Closed => "closed",
        StateFilter::All => "all",
    }
}

impl<C: HttpClient> Provider for GitHub<C> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitHub
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            search: true,
            create: true,
            create_in_organization: true,
        }
    }

    fn current_user<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<Account>> {
        Box::pin(async move {
            let response = self
                .send(Request::get(format!("{}/user", self.api)), Some(token))
                .await?;
            let user: User = self.parse(&response.body)?;
            Ok(Account {
                kind: ProviderKind::GitHub,
                host: self.host.clone(),
                api_url: self.api.clone(),
                username: user.login,
                display_name: user.name.filter(|n| !n.is_empty()),
                avatar_url: user.avatar_url,
            })
        })
    }

    fn repositories<'a>(
        &'a self,
        token: &'a str,
        query: &'a RepositoryQuery,
    ) -> BoxFuture<'a, Result<Page<RemoteRepository>>> {
        Box::pin(self.list(token, query))
    }

    fn repository<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
    ) -> BoxFuture<'a, Result<RemoteRepository>> {
        Box::pin(async move {
            let (owner, name) = full_name.split_once('/').ok_or(Error::NotFound)?;
            let response = self
                .send(
                    Request::get(format!(
                        "{}/repos/{}/{}",
                        self.api,
                        percent_encode(owner),
                        percent_encode(name)
                    )),
                    Some(token),
                )
                .await?;
            Ok(self.parse::<Repo>(&response.body)?.into_remote())
        })
    }

    fn create_repository<'a>(
        &'a self,
        token: &'a str,
        new: &'a NewRepository,
    ) -> BoxFuture<'a, Result<RemoteRepository>> {
        Box::pin(async move {
            let url = match &new.owner {
                Some(org) => format!("{}/orgs/{}/repos", self.api, percent_encode(org)),
                None => format!("{}/user/repos", self.api),
            };
            let body = json!({
                "name": new.name,
                "description": new.description,
                "private": new.private,
                "auto_init": false,
            });
            let response = self
                .send(
                    Request::post(url, body.to_string()).header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(self.parse::<Repo>(&response.body)?.into_remote())
        })
    }

    fn git_username(&self, _: &Account) -> String {
        "x-access-token".to_owned()
    }

    fn pull_requests<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<PullRequest>>> {
        Box::pin(self.list_pulls(token, full_name, query))
    }

    fn issues<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<Issue>>> {
        Box::pin(self.list_issues(token, full_name, query))
    }

    fn create_pull_request<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        new: &'a NewPullRequest,
    ) -> BoxFuture<'a, Result<PullRequest>> {
        Box::pin(self.open_pull(token, full_name, new))
    }

    fn comments<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        _: ItemKind,
        number: u64,
    ) -> BoxFuture<'a, Result<Vec<Comment>>> {
        Box::pin(self.list_comments(token, full_name, number))
    }

    fn merge_pull_request<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
        method: MergeMethod,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.merge(token, full_name, number, method))
    }

    fn create_issue<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        new: &'a NewIssue,
    ) -> BoxFuture<'a, Result<Issue>> {
        Box::pin(self.open_issue(token, full_name, new))
    }

    fn review<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
        verdict: Verdict,
        body: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let event = match verdict {
                Verdict::Approve => "APPROVE",
                Verdict::RequestChanges => "REQUEST_CHANGES",
            };
            self.send(
                Request::post(
                    format!("{}/pulls/{number}/reviews", self.repo_url(full_name)?),
                    json!({ "event": event, "body": body }).to_string(),
                )
                .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
            Ok(())
        })
    }

    fn line_comments<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<Vec<LineComment>>> {
        Box::pin(async move {
            let url = format!(
                "{}/pulls/{number}/comments?per_page=100",
                self.repo_url(full_name)?
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let comments: Vec<ReviewComment> = self.parse(&response.body)?;
            Ok(comments
                .into_iter()
                .map(|c| {
                    let left = c.side.as_deref() == Some("LEFT");
                    LineComment {
                        path: c.path,
                        line: if left { None } else { c.line },
                        old_line: if left { c.line } else { None },
                        comment: CommentBody {
                            user: c.user,
                            body: c.body,
                            created_at: c.created_at,
                        }
                        .into_comment(),
                    }
                })
                .collect())
        })
    }

    fn changed_files<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
    ) -> BoxFuture<'a, Result<Vec<ChangedFile>>> {
        Box::pin(async move {
            let url = format!(
                "{}/pulls/{number}/files?per_page=100",
                self.repo_url(full_name)?
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let files: Vec<PullFile> = self.parse(&response.body)?;
            Ok(files
                .into_iter()
                .map(|file| ChangedFile {
                    change: match file.status.as_str() {
                        "added" => FileChange::Added,
                        "removed" => FileChange::Removed,
                        "renamed" => FileChange::Renamed,
                        _ => FileChange::Modified,
                    },
                    path: file.filename,
                    old_path: file.previous_filename,
                    additions: file.additions,
                    deletions: file.deletions,
                    patch: file.patch,
                })
                .collect())
        })
    }

    fn checks<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        sha: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Check>>> {
        Box::pin(async move {
            let url = format!(
                "{}/commits/{}/check-runs?per_page=100",
                self.repo_url(full_name)?,
                percent_encode(sha)
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let runs: CheckRuns = self.parse(&response.body)?;
            Ok(runs
                .check_runs
                .into_iter()
                .map(|run| Check {
                    state: match (run.status.as_str(), run.conclusion.as_deref()) {
                        ("completed", Some("success")) => CheckState::Success,
                        (
                            "completed",
                            Some("failure" | "timed_out" | "action_required" | "startup_failure"),
                        ) => CheckState::Failure,
                        ("completed", _) => CheckState::Skipped,
                        _ => CheckState::Pending,
                    },
                    name: run.name,
                    url: run.html_url,
                })
                .collect())
        })
    }

    fn add_comment<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        _: ItemKind,
        number: u64,
        body: &'a str,
    ) -> BoxFuture<'a, Result<Comment>> {
        Box::pin(self.post_comment(token, full_name, number, body))
    }

    fn set_open<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        _: ItemKind,
        number: u64,
        open: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(self.update_state(token, full_name, number, open))
    }

    fn pull_request_ref(&self, number: u64) -> Option<String> {
        Some(format!("pull/{number}/head"))
    }
}

/// The URL marked `rel="next"` in a `Link` header.
pub(crate) fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, params) = part.split_once(';')?;
        params
            .split(';')
            .any(|p| p.trim() == "rel=\"next\"")
            .then(|| {
                url.trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .to_owned()
            })
    })
}

/// What the device flow shows the user.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds until the code expires.
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub enum DevicePoll {
    /// The user has not finished yet; poll again after the interval.
    Pending,
    /// Polling too fast: wait this many seconds between polls from now on.
    SlowDown(u64),
    Token(String),
    Expired,
    Denied,
}

impl std::fmt::Debug for DevicePoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("Pending"),
            Self::SlowDown(s) => write!(f, "SlowDown({s})"),
            Self::Token(_) => f.write_str("Token(***)"),
            Self::Expired => f.write_str("Expired"),
            Self::Denied => f.write_str("Denied"),
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
    #[serde(default)]
    errors: Vec<ErrorDetail>,
}

#[derive(Deserialize)]
struct ErrorDetail {
    message: Option<String>,
}

impl ErrorBody {
    fn describe(self) -> String {
        match self.errors.into_iter().find_map(|e| e.message) {
            Some(detail) => format!("{}: {detail}", self.message),
            None => self.message,
        }
    }
}

#[derive(Deserialize)]
struct User {
    login: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct Author {
    login: String,
    avatar_url: Option<String>,
}

impl Author {
    fn into_person(self) -> Person {
        Person {
            username: self.login,
            display_name: None,
            avatar_url: self.avatar_url,
        }
    }
}

#[derive(Deserialize)]
struct LabelBody {
    name: String,
    color: Option<String>,
}

impl LabelBody {
    fn into_label(self) -> Label {
        Label {
            name: self.name,
            color: self.color.filter(|c| c.len() == 6),
        }
    }
}

#[derive(Deserialize)]
struct ReviewComment {
    path: String,
    /// `None` when the line is no longer in the diff.
    line: Option<u32>,
    side: Option<String>,
    user: Option<CommentUser>,
    body: Option<String>,
    created_at: String,
}

#[derive(Deserialize)]
struct PullFile {
    filename: String,
    previous_filename: Option<String>,
    status: String,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
    patch: Option<String>,
}

#[derive(Deserialize)]
struct CheckRuns {
    check_runs: Vec<CheckRun>,
}

#[derive(Deserialize)]
struct CheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
    html_url: Option<String>,
}

#[derive(Deserialize)]
struct FullName {
    full_name: String,
}

#[derive(Deserialize)]
struct PullRef {
    #[serde(rename = "ref")]
    name: String,
    sha: Option<String>,
    repo: Option<FullName>,
}

#[derive(Deserialize)]
struct Pull {
    number: u64,
    title: String,
    body: Option<String>,
    user: Option<Author>,
    state: String,
    #[serde(default)]
    draft: bool,
    merged_at: Option<String>,
    head: PullRef,
    base: PullRef,
    #[serde(default)]
    labels: Vec<LabelBody>,
    comments: Option<u32>,
    created_at: String,
    updated_at: String,
    html_url: String,
}

impl Pull {
    fn into_pull_request(self, full_name: &str) -> PullRequest {
        let state = match (self.state.as_str(), self.merged_at.is_some(), self.draft) {
            (_, true, _) => ItemState::Merged,
            ("closed", ..) => ItemState::Closed,
            (_, _, true) => ItemState::Draft,
            _ => ItemState::Open,
        };
        PullRequest {
            number: self.number,
            title: self.title,
            body: self.body.unwrap_or_default(),
            author: self.user.map(Author::into_person).unwrap_or_default(),
            state,
            head_fork: self
                .head
                .repo
                .map(|r| r.full_name)
                .filter(|r| !r.eq_ignore_ascii_case(full_name)),
            head: self.head.name,
            head_sha: self.head.sha,
            base: self.base.name,
            labels: self.labels.into_iter().map(LabelBody::into_label).collect(),
            comments: self.comments,
            created_at: self.created_at,
            updated_at: self.updated_at,
            web_url: self.html_url,
        }
    }
}

#[derive(Deserialize)]
struct IssueBody {
    number: u64,
    title: String,
    body: Option<String>,
    user: Option<Author>,
    state: String,
    #[serde(default)]
    labels: Vec<LabelBody>,
    comments: Option<u32>,
    created_at: String,
    updated_at: String,
    html_url: String,
    pull_request: Option<serde_json::Value>,
}

impl IssueBody {
    fn into_issue(self) -> Issue {
        Issue {
            number: self.number,
            title: self.title,
            body: self.body.unwrap_or_default(),
            author: self.user.map(Author::into_person).unwrap_or_default(),
            state: if self.state == "closed" {
                ItemState::Closed
            } else {
                ItemState::Open
            },
            labels: self.labels.into_iter().map(LabelBody::into_label).collect(),
            comments: self.comments,
            created_at: self.created_at,
            updated_at: self.updated_at,
            web_url: self.html_url,
        }
    }
}

#[derive(Deserialize)]
struct SearchResults {
    items: Vec<Repo>,
}

#[derive(Deserialize)]
struct Repo {
    full_name: String,
    name: String,
    owner: Login,
    description: Option<String>,
    private: bool,
    fork: bool,
    #[serde(default)]
    archived: bool,
    default_branch: Option<String>,
    clone_url: String,
    ssh_url: Option<String>,
    html_url: String,
    updated_at: Option<String>,
}

impl Repo {
    fn into_remote(self) -> RemoteRepository {
        RemoteRepository {
            full_name: self.full_name,
            owner: self.owner.login,
            name: self.name,
            description: self.description.filter(|d| !d.is_empty()),
            private: self.private,
            fork: self.fork,
            archived: self.archived,
            default_branch: self.default_branch,
            clone_https: self.clone_url,
            clone_ssh: self.ssh_url,
            web_url: self.html_url,
            updated_at: self.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::next_link;

    #[test]
    fn finds_the_next_page_link() {
        let link = r#"<https://api.github.com/user/repos?page=3>; rel="next", <https://api.github.com/user/repos?page=9>; rel="last""#;
        assert_eq!(
            next_link(link).as_deref(),
            Some("https://api.github.com/user/repos?page=3")
        );
        assert_eq!(
            next_link(r#"<https://api.github.com/user/repos?page=1>; rel="prev""#),
            None
        );
    }
}
