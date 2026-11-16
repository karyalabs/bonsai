//! Bitbucket Cloud: OAuth 2 or an Atlassian API token, and the 2.0 API.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    Account, BoxFuture, Capabilities, ChangedFile, Check, CheckState, Comment, Error, FileChange,
    HttpClient, Issue, ItemKind, ItemQuery, ItemState, LineComment, LineTarget, MergeMethod,
    NewIssue, NewPullRequest, NewRepository, Page, Person, Provider, ProviderKind, PullRequest,
    RemoteRepository, RepositoryQuery, Request, Response, Result, StateFilter, Verdict,
    form_encode, oauth::Tokens, percent_encode,
};

const HOST: &str = "bitbucket.org";
const API: &str = "https://api.bitbucket.org/2.0";
const PER_PAGE: u32 = 50;

/// How a request authenticates.
#[derive(Clone, PartialEq, Eq)]
pub enum BitbucketCredential {
    /// An OAuth access token. Git sends it as the password for the user
    /// `x-token-auth`.
    OAuth(String),
    /// An Atlassian API token: the API takes the account's email and the
    /// token; Git takes the Bitbucket username and the token.
    ApiToken { email: String, token: String },
}

impl std::fmt::Debug for BitbucketCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OAuth(_) => f.write_str("OAuth(***)"),
            Self::ApiToken { email, .. } => write!(f, "ApiToken({email}, ***)"),
        }
    }
}

impl BitbucketCredential {
    /// The single string [`Provider`] calls take: the OAuth token, or
    /// `email:token` for an API token (OAuth tokens never contain `:`).
    #[must_use]
    pub fn encode(&self) -> String {
        match self {
            Self::OAuth(token) => token.clone(),
            Self::ApiToken { email, token } => format!("{email}:{token}"),
        }
    }

    #[must_use]
    pub fn decode(text: &str) -> Self {
        match text.split_once(':') {
            Some((email, token)) => Self::ApiToken {
                email: email.to_owned(),
                token: token.to_owned(),
            },
            None => Self::OAuth(text.to_owned()),
        }
    }

    /// The user name Git sends over HTTPS.
    #[must_use]
    pub fn git_username(&self, account: &Account) -> String {
        match self {
            Self::OAuth(_) => "x-token-auth".to_owned(),
            Self::ApiToken { .. } => account.username.clone(),
        }
    }

    /// The password Git sends over HTTPS.
    #[must_use]
    pub fn git_password(&self) -> &str {
        match self {
            Self::OAuth(token) | Self::ApiToken { token, .. } => token,
        }
    }

    fn authorization(&self) -> String {
        match self {
            Self::OAuth(token) => format!("Bearer {token}"),
            Self::ApiToken { email, token } => {
                format!("Basic {}", STANDARD.encode(format!("{email}:{token}")))
            }
        }
    }
}

pub struct Bitbucket<C> {
    client: C,
}

impl<C: HttpClient> Bitbucket<C> {
    pub fn new(client: C) -> Self {
        Self { client }
    }

    /// The page to open in the browser to sign in with OAuth.
    #[must_use]
    pub fn authorize_url(&self, client_id: &str, state: &str) -> String {
        format!(
            "https://{HOST}/site/oauth2/authorize?client_id={}&response_type=code&state={}",
            percent_encode(client_id),
            percent_encode(state)
        )
    }

    /// Exchanges the redirect's code for tokens. Bitbucket's OAuth
    /// consumers authenticate with their secret.
    pub async fn exchange_code(&self, client_id: &str, secret: &str, code: &str) -> Result<Tokens> {
        self.token(
            client_id,
            secret,
            &[("grant_type", "authorization_code"), ("code", code)],
        )
        .await
    }

    pub async fn refresh(&self, client_id: &str, secret: &str, refresh: &str) -> Result<Tokens> {
        self.token(
            client_id,
            secret,
            &[("grant_type", "refresh_token"), ("refresh_token", refresh)],
        )
        .await
    }

    async fn token(
        &self,
        client_id: &str,
        secret: &str,
        fields: &[(&str, &str)],
    ) -> Result<Tokens> {
        let request = Request::post(
            format!("https://{HOST}/site/oauth2/access_token"),
            form_encode(fields),
        )
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header(
            "Authorization",
            format!("Basic {}", STANDARD.encode(format!("{client_id}:{secret}"))),
        );
        let response = self.send(request, None).await?;
        parse(&response.body)
    }

    async fn send(&self, request: Request, token: Option<&str>) -> Result<Response> {
        let mut request = request
            .header("Accept", "application/json")
            .header("User-Agent", "Bonsai");
        if let Some(token) = token {
            request = request.header(
                "Authorization",
                BitbucketCredential::decode(token).authorization(),
            );
        }
        let response = self
            .client
            .send(request)
            .await
            .map_err(|message| Error::Network {
                host: HOST.to_owned(),
                message,
            })?;
        if response.is_success() {
            Ok(response)
        } else {
            Err(error(&response))
        }
    }

    /// Workspaces the account can create repositories in, by slug.
    pub async fn workspaces(&self, token: &str) -> Result<Vec<String>> {
        let response = self
            .send(
                Request::get(format!("{API}/user/permissions/workspaces?pagelen=100")),
                Some(token),
            )
            .await?;
        let page: Values<WorkspacePermission> = parse(&response.body)?;
        Ok(page
            .values
            .into_iter()
            .filter(|p| p.permission != "member")
            .map(|p| p.workspace.slug)
            .collect())
    }

    async fn list(&self, token: &str, query: &RepositoryQuery) -> Result<Page<RemoteRepository>> {
        let url = if let Some(cursor) = &query.cursor {
            if !cursor.starts_with(&format!("{API}/")) {
                return Err(Error::Rejected(
                    "That page link is not from Bitbucket.".to_owned(),
                ));
            }
            cursor.clone()
        } else {
            let mut url =
                format!("{API}/repositories?role=member&sort=-updated_on&pagelen={PER_PAGE}");
            let search = query.search.trim();
            if !search.is_empty() {
                let escaped = search.replace('\\', "\\\\").replace('"', "\\\"");
                url.push_str("&q=");
                url.push_str(&percent_encode(&format!(
                    "name ~ \"{escaped}\" OR description ~ \"{escaped}\""
                )));
            }
            url
        };
        let response = self.send(Request::get(url), Some(token)).await?;
        let page: Values<Repo> = parse(&response.body)?;
        Ok(Page {
            items: page.values.into_iter().map(Repo::into_remote).collect(),
            next: page.next,
        })
    }
}

fn error(response: &Response) -> Error {
    let message = serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|body| {
            let error = body.get("error")?;
            let message = error.get("message")?.as_str()?.to_owned();
            // Field errors arrive as {"fields": {"name": ["..."]}}.
            let fields = error
                .get("fields")
                .and_then(Value::as_object)
                .map(|fields| {
                    fields
                        .iter()
                        .flat_map(|(_, problems)| {
                            problems
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some(if fields.is_empty() {
                message
            } else {
                format!("{message}: {}", fields.join(", "))
            })
        });
    match response.status {
        401 => Error::Unauthorized(HOST.to_owned()),
        429 => Error::RateLimited("Bitbucket".to_owned(), None),
        404 => Error::NotFound,
        400 | 403 | 409 => message.map_or(
            Error::Status {
                host: HOST.to_owned(),
                status: response.status,
            },
            Error::Rejected,
        ),
        status => Error::Status {
            host: HOST.to_owned(),
            status,
        },
    }
}

fn parse<'de, T: Deserialize<'de>>(body: &'de [u8]) -> Result<T> {
    serde_json::from_slice(body).map_err(|error| Error::Parse {
        host: HOST.to_owned(),
        message: error.to_string(),
    })
}

impl<C: HttpClient> Provider for Bitbucket<C> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Bitbucket
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
                .send(Request::get(format!("{API}/user")), Some(token))
                .await?;
            let user: User = parse(&response.body)?;
            Ok(Account {
                kind: ProviderKind::Bitbucket,
                host: HOST.to_owned(),
                api_url: API.to_owned(),
                username: user.username,
                display_name: user.display_name.filter(|n| !n.is_empty()),
                avatar_url: user.links.and_then(|l| l.avatar).map(|a| a.href),
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
            let (workspace, slug) = full_name.split_once('/').ok_or(Error::NotFound)?;
            let response = self
                .send(
                    Request::get(format!(
                        "{API}/repositories/{}/{}",
                        percent_encode(workspace),
                        percent_encode(slug)
                    )),
                    Some(token),
                )
                .await?;
            Ok(parse::<Repo>(&response.body)?.into_remote())
        })
    }

    /// Creates the repository in `new.owner`, or in the workspace named
    /// after the account when there is none.
    fn create_repository<'a>(
        &'a self,
        token: &'a str,
        new: &'a NewRepository,
    ) -> BoxFuture<'a, Result<RemoteRepository>> {
        Box::pin(async move {
            let workspace = match &new.owner {
                Some(workspace) => workspace.clone(),
                None => self.current_user(token).await?.username,
            };
            let slug = slug(&new.name);
            let body = json!({
                "scm": "git",
                "name": new.name,
                "description": new.description.clone().unwrap_or_default(),
                "is_private": new.private,
            });
            let response = self
                .send(
                    Request::post(
                        format!(
                            "{API}/repositories/{}/{}",
                            percent_encode(&workspace),
                            percent_encode(&slug)
                        ),
                        body.to_string(),
                    )
                    .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(parse::<Repo>(&response.body)?.into_remote())
        })
    }

    /// The username, which Git pairs with an API token. OAuth sign-ins use
    /// [`BitbucketCredential::git_username`] instead.
    fn git_username(&self, account: &Account) -> String {
        account.username.clone()
    }

    fn pull_requests<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<PullRequest>>> {
        Box::pin(async move {
            let states: &[&str] = match query.state {
                StateFilter::Open => &["OPEN"],
                StateFilter::Closed => &["MERGED", "DECLINED", "SUPERSEDED"],
                StateFilter::All => &["OPEN", "MERGED", "DECLINED", "SUPERSEDED"],
            };
            let mut first = format!(
                "{}/pullrequests?pagelen={PER_PAGE}&sort=-updated_on",
                repo_url(full_name)?
            );
            for state in states {
                first.push_str("&state=");
                first.push_str(state);
            }
            let url = page_url(first, query.cursor.as_ref())?;
            let response = self.send(Request::get(url), Some(token)).await?;
            let page: Values<Pull> = parse(&response.body)?;
            Ok(Page {
                items: page
                    .values
                    .into_iter()
                    .map(|pull| pull.into_pull_request(full_name))
                    .collect(),
                next: page.next,
            })
        })
    }

    fn issues<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<Issue>>> {
        Box::pin(async move {
            let mut first = format!(
                "{}/issues?pagelen={PER_PAGE}&sort=-updated_on",
                repo_url(full_name)?
            );
            let states: &[&str] = match query.state {
                StateFilter::Open => &OPEN_ISSUE,
                StateFilter::Closed => &CLOSED_ISSUE,
                StateFilter::All => &[],
            };
            if !states.is_empty() {
                let q = states
                    .iter()
                    .map(|s| format!("state = \"{s}\""))
                    .collect::<Vec<_>>()
                    .join(" OR ");
                first.push_str("&q=");
                first.push_str(&percent_encode(&q));
            }
            let url = page_url(first, query.cursor.as_ref())?;
            let response = self.send(Request::get(url), Some(token)).await?;
            let page: Values<IssueBody> = parse(&response.body)?;
            Ok(Page {
                items: page.values.into_iter().map(IssueBody::into_issue).collect(),
                next: page.next,
            })
        })
    }

    fn create_pull_request<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        new: &'a NewPullRequest,
    ) -> BoxFuture<'a, Result<PullRequest>> {
        Box::pin(async move {
            let body = json!({
                "title": new.title,
                "description": new.body,
                "source": { "branch": { "name": new.head } },
                "destination": { "branch": { "name": new.base } },
                "draft": new.draft,
            });
            let response = self
                .send(
                    Request::post(
                        format!("{}/pullrequests", repo_url(full_name)?),
                        body.to_string(),
                    )
                    .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(parse::<Pull>(&response.body)?.into_pull_request(full_name))
        })
    }

    fn comments<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        kind: ItemKind,
        number: u64,
    ) -> BoxFuture<'a, Result<Vec<Comment>>> {
        Box::pin(async move {
            let url = format!(
                "{}/{}/{number}/comments?pagelen=100&sort=created_on",
                repo_url(full_name)?,
                item_path(kind)
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let page: Values<CommentBody> = parse(&response.body)?;
            Ok(page
                .values
                .into_iter()
                // Line comments show in Files.
                .filter(|c| !c.deleted && c.inline.is_none())
                .map(CommentBody::into_comment)
                .collect())
        })
    }

    fn merge_pull_request<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
        method: MergeMethod,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let strategy = match method {
                MergeMethod::Merge => "merge_commit",
                MergeMethod::Squash => "squash",
                MergeMethod::Rebase => "fast_forward",
            };
            self.send(
                Request::post(
                    format!("{}/pullrequests/{number}/merge", repo_url(full_name)?),
                    json!({ "merge_strategy": strategy }).to_string(),
                )
                .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
            Ok(())
        })
    }

    fn create_issue<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        new: &'a NewIssue,
    ) -> BoxFuture<'a, Result<Issue>> {
        Box::pin(async move {
            let body = json!({ "title": new.title, "content": { "raw": new.body } });
            let response = self
                .send(
                    Request::post(format!("{}/issues", repo_url(full_name)?), body.to_string())
                        .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(parse::<IssueBody>(&response.body)?.into_issue())
        })
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
            let action = match verdict {
                Verdict::Approve => "approve",
                Verdict::RequestChanges => "request-changes",
            };
            self.send(
                Request::post(
                    format!("{}/pullrequests/{number}/{action}", repo_url(full_name)?),
                    "{}",
                )
                .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
            if !body.trim().is_empty() {
                self.add_comment(token, full_name, ItemKind::PullRequest, number, body)
                    .await?;
            }
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
                "{}/pullrequests/{number}/comments?pagelen=100&sort=created_on",
                repo_url(full_name)?
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let page: Values<CommentBody> = parse(&response.body)?;
            // A reply names its parent; its thread is the first comment's.
            let parents: std::collections::HashMap<u64, Option<u64>> = page
                .values
                .iter()
                .map(|c| (c.id, c.parent.as_ref().map(|p| p.id)))
                .collect();
            let root = |mut id: u64| {
                for _ in 0..32 {
                    match parents.get(&id).copied().flatten() {
                        Some(parent) => id = parent,
                        None => break,
                    }
                }
                id
            };
            let places: std::collections::HashMap<u64, Place> = page
                .values
                .iter()
                .filter_map(|c| {
                    let inline = c.inline.as_ref()?;
                    Some((
                        c.id,
                        Place {
                            path: inline.path.clone(),
                            line: inline.to,
                            old_line: inline.to.is_none().then_some(inline.from).flatten(),
                            resolved: c.resolution.as_ref().is_some_and(|r| !r.is_null()),
                        },
                    ))
                })
                .collect();
            Ok(page
                .values
                .into_iter()
                .filter(|c| !c.deleted)
                .filter_map(|c| {
                    let thread = root(c.id);
                    let place = places.get(&thread)?.clone();
                    Some(LineComment {
                        path: place.path,
                        line: place.line,
                        old_line: place.old_line,
                        thread: thread.to_string(),
                        resolved: place.resolved,
                        comment: c.into_comment(),
                    })
                })
                .collect())
        })
    }

    fn reply_to_thread<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
        thread: &'a str,
        body: &'a str,
    ) -> BoxFuture<'a, Result<Comment>> {
        Box::pin(async move {
            let parent: u64 = thread.parse().map_err(|_| Error::NotFound)?;
            let response = self
                .send(
                    Request::post(
                        format!("{}/pullrequests/{number}/comments", repo_url(full_name)?),
                        json!({ "content": { "raw": body }, "parent": { "id": parent } })
                            .to_string(),
                    )
                    .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(parse::<CommentBody>(&response.body)?.into_comment())
        })
    }

    fn add_line_comment<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
        _: &'a str,
        target: &'a LineTarget,
        body: &'a str,
    ) -> BoxFuture<'a, Result<LineComment>> {
        Box::pin(async move {
            let mut inline = json!({ "path": target.path });
            match (target.new_line, target.old_line) {
                (Some(line), _) => inline["to"] = json!(line),
                (None, Some(line)) => inline["from"] = json!(line),
                (None, None) => return Err(Error::Rejected("Pick a line to comment on.".into())),
            }
            let response = self
                .send(
                    Request::post(
                        format!("{}/pullrequests/{number}/comments", repo_url(full_name)?),
                        json!({ "content": { "raw": body }, "inline": inline }).to_string(),
                    )
                    .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            let comment: CommentBody = parse(&response.body)?;
            Ok(LineComment {
                path: target.path.clone(),
                line: target.new_line,
                old_line: target
                    .new_line
                    .is_none()
                    .then_some(target.old_line)
                    .flatten(),
                thread: comment.id.to_string(),
                resolved: false,
                comment: comment.into_comment(),
            })
        })
    }

    fn set_thread_resolved<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        number: u64,
        thread: &'a str,
        resolved: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let id: u64 = thread.parse().map_err(|_| Error::NotFound)?;
            let url = format!(
                "{}/pullrequests/{number}/comments/{id}/resolve",
                repo_url(full_name)?
            );
            let request = if resolved {
                Request::post(url, "{}").header("Content-Type", "application/json")
            } else {
                Request::delete(url)
            };
            self.send(request, Some(token)).await?;
            Ok(())
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
                "{}/pullrequests/{number}/diffstat?pagelen=100",
                repo_url(full_name)?
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let page: Values<DiffStat> = parse(&response.body)?;
            Ok(page
                .values
                .into_iter()
                .filter_map(|stat| {
                    let old = stat.old.map(|p| p.path);
                    let new = stat.new.map(|p| p.path);
                    let change = match stat.status.as_str() {
                        "added" => FileChange::Added,
                        "removed" => FileChange::Removed,
                        "renamed" => FileChange::Renamed,
                        _ => FileChange::Modified,
                    };
                    Some(ChangedFile {
                        path: new.clone().or_else(|| old.clone())?,
                        old_path: (change == FileChange::Renamed).then_some(old).flatten(),
                        change,
                        additions: stat.lines_added,
                        deletions: stat.lines_removed,
                        // The diff endpoint redirects; the list is enough here.
                        patch: None,
                    })
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
                "{}/commit/{}/statuses?pagelen=100",
                repo_url(full_name)?,
                percent_encode(sha)
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let page: Values<BuildStatus> = parse(&response.body)?;
            Ok(page
                .values
                .into_iter()
                .map(|status| Check {
                    state: match status.state.as_str() {
                        "SUCCESSFUL" => CheckState::Success,
                        "FAILED" => CheckState::Failure,
                        "STOPPED" => CheckState::Skipped,
                        _ => CheckState::Pending,
                    },
                    name: status.name.unwrap_or(status.key),
                    url: status.url,
                })
                .collect())
        })
    }

    fn add_comment<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        kind: ItemKind,
        number: u64,
        body: &'a str,
    ) -> BoxFuture<'a, Result<Comment>> {
        Box::pin(async move {
            let url = format!(
                "{}/{}/{number}/comments",
                repo_url(full_name)?,
                item_path(kind)
            );
            let response = self
                .send(
                    Request::post(url, json!({ "content": { "raw": body } }).to_string())
                        .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(parse::<CommentBody>(&response.body)?.into_comment())
        })
    }

    fn set_open<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        kind: ItemKind,
        number: u64,
        open: bool,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let repo = repo_url(full_name)?;
            let request = match (kind, open) {
                (ItemKind::PullRequest, false) => {
                    Request::post(format!("{repo}/pullrequests/{number}/decline"), "{}")
                }
                (ItemKind::PullRequest, true) => {
                    return Err(Error::Rejected(
                        "Bitbucket can't reopen a declined pull request.".to_owned(),
                    ));
                }
                (ItemKind::Issue, open) => Request::put(
                    format!("{repo}/issues/{number}"),
                    json!({ "state": if open { "open" } else { "resolved" } }).to_string(),
                ),
            };
            self.send(
                request.header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
            Ok(())
        })
    }

    fn pull_request_ref(&self, _: u64) -> Option<String> {
        None
    }
}

#[derive(Deserialize)]
struct DiffStat {
    status: String,
    #[serde(default)]
    lines_added: u32,
    #[serde(default)]
    lines_removed: u32,
    old: Option<DiffPath>,
    new: Option<DiffPath>,
}

#[derive(Deserialize)]
struct DiffPath {
    path: String,
}

#[derive(Deserialize)]
struct CommitHash {
    hash: String,
}

#[derive(Deserialize)]
struct BuildStatus {
    key: String,
    name: Option<String>,
    state: String,
    url: Option<String>,
}

fn item_path(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::PullRequest => "pullrequests",
        ItemKind::Issue => "issues",
    }
}

const OPEN_ISSUE: [&str; 3] = ["new", "open", "on hold"];
const CLOSED_ISSUE: [&str; 5] = ["resolved", "invalid", "duplicate", "wontfix", "closed"];

fn repo_url(full_name: &str) -> Result<String> {
    let (workspace, slug) = full_name.split_once('/').ok_or(Error::NotFound)?;
    Ok(format!(
        "{API}/repositories/{}/{}",
        percent_encode(workspace),
        percent_encode(slug)
    ))
}

fn page_url(first: String, cursor: Option<&String>) -> Result<String> {
    match cursor {
        Some(cursor) if cursor.starts_with(&format!("{API}/")) => Ok(cursor.clone()),
        Some(_) => Err(Error::Rejected(
            "That page link is not from Bitbucket.".to_owned(),
        )),
        None => Ok(first),
    }
}

/// A user as pull requests and issues show them: Bitbucket leaves out the
/// username there.
#[derive(Deserialize)]
struct Participant {
    display_name: Option<String>,
    nickname: Option<String>,
    links: Option<UserLinks>,
}

impl Participant {
    fn into_person(self) -> Person {
        Person {
            username: self
                .nickname
                .or_else(|| self.display_name.clone())
                .unwrap_or_default(),
            display_name: self.display_name.filter(|n| !n.is_empty()),
            avatar_url: self.links.and_then(|l| l.avatar).map(|a| a.href),
        }
    }
}

#[derive(Deserialize)]
struct HtmlLinks {
    html: Href,
}

#[derive(Deserialize)]
struct FullName {
    full_name: String,
}

#[derive(Deserialize)]
struct Endpoint {
    branch: Branch,
    commit: Option<CommitHash>,
    repository: Option<FullName>,
}

#[derive(Deserialize)]
struct Pull {
    id: u64,
    title: String,
    description: Option<String>,
    author: Option<Participant>,
    state: String,
    #[serde(default)]
    draft: bool,
    source: Endpoint,
    destination: Endpoint,
    comment_count: Option<u32>,
    created_on: String,
    updated_on: String,
    links: HtmlLinks,
}

impl Pull {
    fn into_pull_request(self, full_name: &str) -> PullRequest {
        let state = match self.state.as_str() {
            "MERGED" => ItemState::Merged,
            "DECLINED" | "SUPERSEDED" => ItemState::Closed,
            _ if self.draft => ItemState::Draft,
            _ => ItemState::Open,
        };
        PullRequest {
            number: self.id,
            title: self.title,
            body: self.description.unwrap_or_default(),
            author: self
                .author
                .map(Participant::into_person)
                .unwrap_or_default(),
            state,
            head_sha: self.source.commit.map(|c| c.hash),
            head: self.source.branch.name,
            head_fork: self
                .source
                .repository
                .map(|r| r.full_name)
                .filter(|r| !r.eq_ignore_ascii_case(full_name)),
            base: self.destination.branch.name,
            labels: Vec::new(),
            comments: self.comment_count,
            created_at: self.created_on,
            updated_at: self.updated_on,
            web_url: self.links.html.href,
        }
    }
}

#[derive(Deserialize)]
struct CommentBody {
    #[serde(default)]
    id: u64,
    parent: Option<CommentId>,
    resolution: Option<serde_json::Value>,
    user: Option<Participant>,
    content: Option<Content>,
    #[serde(default)]
    deleted: bool,
    created_on: String,
    inline: Option<Inline>,
}

/// Where a thread of line comments sits.
#[derive(Clone)]
struct Place {
    path: String,
    line: Option<u32>,
    old_line: Option<u32>,
    resolved: bool,
}

#[derive(Deserialize)]
struct CommentId {
    id: u64,
}

#[derive(Deserialize)]
struct Inline {
    path: String,
    from: Option<u32>,
    to: Option<u32>,
}

impl CommentBody {
    fn into_comment(self) -> Comment {
        Comment {
            author: self.user.map(Participant::into_person).unwrap_or_default(),
            body: self.content.and_then(|c| c.raw).unwrap_or_default(),
            created_at: self.created_on,
        }
    }
}

#[derive(Deserialize)]
struct Content {
    raw: Option<String>,
}

#[derive(Deserialize)]
struct IssueBody {
    id: u64,
    title: String,
    content: Option<Content>,
    reporter: Option<Participant>,
    state: String,
    created_on: String,
    updated_on: String,
    links: HtmlLinks,
}

impl IssueBody {
    fn into_issue(self) -> Issue {
        Issue {
            number: self.id,
            title: self.title,
            body: self.content.and_then(|c| c.raw).unwrap_or_default(),
            author: self
                .reporter
                .map(Participant::into_person)
                .unwrap_or_default(),
            state: if OPEN_ISSUE.contains(&self.state.as_str()) {
                ItemState::Open
            } else {
                ItemState::Closed
            },
            labels: Vec::new(),
            comments: None,
            created_at: self.created_on,
            updated_at: self.updated_on,
            web_url: self.links.html.href,
        }
    }
}

/// Bitbucket's slug for a name: lowercase, with runs of other characters
/// turned into one `-`.
#[must_use]
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[derive(Deserialize)]
struct Values<T> {
    values: Vec<T>,
    next: Option<String>,
}

#[derive(Deserialize)]
struct Href {
    href: String,
}

#[derive(Deserialize)]
struct UserLinks {
    avatar: Option<Href>,
}

#[derive(Deserialize)]
struct User {
    username: String,
    display_name: Option<String>,
    links: Option<UserLinks>,
}

#[derive(Deserialize)]
struct Slug {
    slug: String,
}

#[derive(Deserialize)]
struct WorkspacePermission {
    permission: String,
    workspace: Slug,
}

#[derive(Deserialize)]
struct NamedHref {
    name: String,
    href: String,
}

#[derive(Deserialize)]
struct RepoLinks {
    html: Href,
    #[serde(default)]
    clone: Vec<NamedHref>,
}

#[derive(Deserialize)]
struct Branch {
    name: String,
}

#[derive(Deserialize)]
struct Repo {
    full_name: String,
    slug: String,
    description: Option<String>,
    is_private: bool,
    parent: Option<Value>,
    mainbranch: Option<Branch>,
    links: RepoLinks,
    updated_on: Option<String>,
    workspace: Slug,
}

impl Repo {
    fn into_remote(self) -> RemoteRepository {
        let clone = |name: &str| {
            self.links
                .clone
                .iter()
                .find(|l| l.name == name)
                .map(|l| l.href.clone())
        };
        // Bitbucket puts the viewer's name in the HTTPS URL; leave it out so
        // the URL works for whoever signs in.
        let https = clone("https").map_or_else(
            || format!("https://{HOST}/{}.git", self.full_name),
            |url| strip_user(&url),
        );
        let ssh = clone("ssh");
        RemoteRepository {
            owner: self.workspace.slug,
            name: self.slug,
            description: self.description.filter(|d| !d.is_empty()),
            private: self.is_private,
            fork: self.parent.is_some_and(|p| !p.is_null()),
            archived: false,
            default_branch: self.mainbranch.map(|b| b.name),
            clone_https: https,
            clone_ssh: ssh,
            web_url: self.links.html.href,
            updated_at: self.updated_on,
            full_name: self.full_name,
        }
    }
}

fn strip_user(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            format!("{scheme}://{host}/{path}")
        }
        None => url.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_and_urls() {
        assert_eq!(slug(" My Repo (v2)! "), "my-repo-v2");
        assert_eq!(slug("api_server.rs"), "api_server.rs");
        assert_eq!(
            strip_user("https://ada@bitbucket.org/team/app.git"),
            "https://bitbucket.org/team/app.git"
        );
        let api = BitbucketCredential::decode("ada@example.com:ATATT3x");
        assert_eq!(api.git_password(), "ATATT3x");
        assert!(!format!("{api:?}").contains("ATATT"));
        assert_eq!(BitbucketCredential::decode(&api.encode()), api);
        assert_eq!(
            BitbucketCredential::decode("oauthtoken"),
            BitbucketCredential::OAuth("oauthtoken".into())
        );
    }
}
