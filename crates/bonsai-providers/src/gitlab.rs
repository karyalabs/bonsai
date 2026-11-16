//! GitLab.com and self-managed GitLab: OAuth with PKCE through a loopback
//! redirect, or a personal access token, and REST v4.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    Account, BoxFuture, Capabilities, Comment, Error, HttpClient, Issue, ItemKind, ItemQuery,
    ItemState, Label, MergeMethod, NewIssue, NewPullRequest, NewRepository, Page, Person, Provider,
    ProviderKind, PullRequest, RemoteRepository, RepositoryQuery, Request, Response, Result,
    StateFilter, form_encode, github::next_link, oauth::Tokens, percent_encode,
};

const PER_PAGE: u32 = 50;
/// The scopes Bonsai asks for: the API (projects, create) and Git over HTTPS.
pub const SCOPES: &str = "api read_user write_repository";

pub struct GitLab<C> {
    client: C,
    host: String,
    api: String,
}

impl<C: HttpClient> GitLab<C> {
    /// gitlab.com.
    pub fn new(client: C) -> Self {
        Self::self_managed(client, "gitlab.com")
    }

    /// A self-managed instance, such as `gitlab.example.com`.
    pub fn self_managed(client: C, host: &str) -> Self {
        let host = host.trim_end_matches('/').to_ascii_lowercase();
        Self {
            client,
            api: format!("https://{host}/api/v4"),
            host,
        }
    }

    /// The page to open in the browser to sign in. The user returns to
    /// `redirect_uri` (a loopback address) with `code` and `state`.
    #[must_use]
    pub fn authorize_url(&self, client_id: &str, redirect_uri: &str, pkce: &crate::Pkce) -> String {
        format!(
            "https://{}/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&state={}&scope={}&code_challenge={}&code_challenge_method=S256",
            self.host,
            percent_encode(client_id),
            percent_encode(redirect_uri),
            percent_encode(&pkce.state),
            percent_encode(SCOPES),
            percent_encode(&pkce.challenge),
        )
    }

    /// Exchanges the code from the redirect for tokens.
    pub async fn exchange_code(
        &self,
        client_id: &str,
        redirect_uri: &str,
        code: &str,
        verifier: &str,
    ) -> Result<Tokens> {
        self.token(&[
            ("client_id", client_id),
            ("code", code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ])
        .await
    }

    /// Trades a refresh token for a new access token; GitLab's expire after
    /// two hours.
    pub async fn refresh(
        &self,
        client_id: &str,
        redirect_uri: &str,
        refresh: &str,
    ) -> Result<Tokens> {
        self.token(&[
            ("client_id", client_id),
            ("refresh_token", refresh),
            ("grant_type", "refresh_token"),
            ("redirect_uri", redirect_uri),
        ])
        .await
    }

    async fn token(&self, fields: &[(&str, &str)]) -> Result<Tokens> {
        let response = self
            .send(
                Request::post(
                    format!("https://{}/oauth/token", self.host),
                    form_encode(fields),
                )
                .header("Content-Type", "application/x-www-form-urlencoded"),
                None,
            )
            .await?;
        self.parse(&response.body)
    }

    async fn send(&self, request: Request, token: Option<&str>) -> Result<Response> {
        let mut request = request
            .header("Accept", "application/json")
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
        match response.status {
            401 => Error::Unauthorized(self.host.clone()),
            429 => Error::RateLimited(
                "GitLab".to_owned(),
                response
                    .header("ratelimit-reset")
                    .and_then(|r| r.parse().ok()),
            ),
            404 => Error::NotFound,
            400 | 403 | 409 | 422 => serde_json::from_slice::<Value>(&response.body)
                .ok()
                .and_then(|body| describe(&body))
                .map_or_else(|| self.status(response.status), Error::Rejected),
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

    async fn list(&self, token: &str, query: &RepositoryQuery) -> Result<Page<RemoteRepository>> {
        let url = if let Some(cursor) = &query.cursor {
            if !cursor.starts_with(&format!("{}/", self.api)) {
                return Err(Error::Rejected(
                    "That page link is not from GitLab.".to_owned(),
                ));
            }
            cursor.clone()
        } else {
            let mut url = format!(
                "{}/projects?membership=true&order_by=last_activity_at&sort=desc&per_page={PER_PAGE}",
                self.api
            );
            let search = query.search.trim();
            if !search.is_empty() {
                url.push_str("&search_namespaces=true&search=");
                url.push_str(&percent_encode(search));
            }
            url
        };
        let response = self.send(Request::get(url), Some(token)).await?;
        let projects: Vec<Project> = self.parse(&response.body)?;
        Ok(Page {
            items: projects.into_iter().map(Project::into_remote).collect(),
            next: response.header("link").and_then(next_link),
        })
    }

    async fn namespace_id(&self, token: &str, path: &str) -> Result<u64> {
        let response = self
            .send(
                Request::get(format!("{}/namespaces/{}", self.api, percent_encode(path))),
                Some(token),
            )
            .await?;
        Ok(self.parse::<Namespace>(&response.body)?.id)
    }
}

/// GitLab's `message` is a string, or an object of field errors such as
/// `{"name": ["has already been taken"]}`.
fn describe(body: &Value) -> Option<String> {
    let message = body
        .get("message")
        .or_else(|| body.get("error_description"))
        .or_else(|| body.get("error"))?;
    match message {
        Value::String(text) => Some(text.clone()),
        Value::Object(fields) => Some(
            fields
                .iter()
                .map(|(field, problems)| {
                    let problems = match problems {
                        Value::Array(list) => list
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", "),
                        other => other.to_string(),
                    };
                    format!("{field} {problems}")
                })
                .collect::<Vec<_>>()
                .join("; "),
        ),
        other => Some(other.to_string()),
    }
}

impl<C: HttpClient> Provider for GitLab<C> {
    fn kind(&self) -> ProviderKind {
        ProviderKind::GitLab
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
                kind: ProviderKind::GitLab,
                host: self.host.clone(),
                api_url: self.api.clone(),
                username: user.username,
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
            let response = self
                .send(
                    Request::get(format!(
                        "{}/projects/{}",
                        self.api,
                        percent_encode(full_name)
                    )),
                    Some(token),
                )
                .await?;
            Ok(self.parse::<Project>(&response.body)?.into_remote())
        })
    }

    fn create_repository<'a>(
        &'a self,
        token: &'a str,
        new: &'a NewRepository,
    ) -> BoxFuture<'a, Result<RemoteRepository>> {
        Box::pin(async move {
            let mut body = json!({
                "name": new.name,
                "path": new.name,
                "description": new.description,
                "visibility": if new.private { "private" } else { "public" },
                "initialize_with_readme": false,
            });
            if let Some(group) = &new.owner {
                body["namespace_id"] = json!(self.namespace_id(token, group).await?);
            }
            let response = self
                .send(
                    Request::post(format!("{}/projects", self.api), body.to_string())
                        .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(self.parse::<Project>(&response.body)?.into_remote())
        })
    }

    fn git_username(&self, _: &Account) -> String {
        "oauth2".to_owned()
    }

    fn pull_requests<'a>(
        &'a self,
        token: &'a str,
        full_name: &'a str,
        query: &'a ItemQuery,
    ) -> BoxFuture<'a, Result<Page<PullRequest>>> {
        Box::pin(async move {
            // "closed" leaves out merged ones, so Closed reads everything
            // and keeps what isn't open.
            let state = match query.state {
                StateFilter::Open => "opened",
                StateFilter::Closed | StateFilter::All => "all",
            };
            let first = format!(
                "{}/merge_requests?state={state}&order_by=updated_at&sort=desc&per_page={PER_PAGE}",
                self.project_url(full_name)
            );
            let url = self.page_url(first, query.cursor.as_ref())?;
            let response = self.send(Request::get(url), Some(token)).await?;
            let requests: Vec<MergeRequest> = self.parse(&response.body)?;
            Ok(Page {
                items: requests
                    .into_iter()
                    .map(MergeRequest::into_pull_request)
                    .filter(|pull| query.state.keeps(pull.state))
                    .collect(),
                next: response.header("link").and_then(next_link),
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
            let state = match query.state {
                StateFilter::Open => "opened",
                StateFilter::Closed => "closed",
                StateFilter::All => "all",
            };
            let first = format!(
                "{}/issues?state={state}&order_by=updated_at&sort=desc&per_page={PER_PAGE}",
                self.project_url(full_name)
            );
            let url = self.page_url(first, query.cursor.as_ref())?;
            let response = self.send(Request::get(url), Some(token)).await?;
            let issues: Vec<IssueBody> = self.parse(&response.body)?;
            Ok(Page {
                items: issues.into_iter().map(IssueBody::into_issue).collect(),
                next: response.header("link").and_then(next_link),
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
            let title = if new.draft {
                format!("Draft: {}", new.title)
            } else {
                new.title.clone()
            };
            let body = json!({
                "source_branch": new.head,
                "target_branch": new.base,
                "title": title,
                "description": new.body,
            });
            let response = self
                .send(
                    Request::post(
                        format!("{}/merge_requests", self.project_url(full_name)),
                        body.to_string(),
                    )
                    .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(self
                .parse::<MergeRequest>(&response.body)?
                .into_pull_request())
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
                "{}/{}/{number}/notes?sort=asc&order_by=created_at&per_page=100",
                self.project_url(full_name),
                item_path(kind)
            );
            let response = self.send(Request::get(url), Some(token)).await?;
            let notes: Vec<Note> = self.parse(&response.body)?;
            Ok(notes
                .into_iter()
                // System notes are GitLab's own ("added 1 commit").
                .filter(|note| !note.system)
                .map(Note::into_comment)
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
            // The project decides merge commit or fast-forward; a request
            // can only ask to squash.
            let body = json!({ "squash": method == MergeMethod::Squash });
            self.send(
                Request::put(
                    format!(
                        "{}/merge_requests/{number}/merge",
                        self.project_url(full_name)
                    ),
                    body.to_string(),
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
            let body = json!({ "title": new.title, "description": new.body });
            let response = self
                .send(
                    Request::post(
                        format!("{}/issues", self.project_url(full_name)),
                        body.to_string(),
                    )
                    .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(self.parse::<IssueBody>(&response.body)?.into_issue())
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
                "{}/{}/{number}/notes",
                self.project_url(full_name),
                item_path(kind)
            );
            let response = self
                .send(
                    Request::post(url, json!({ "body": body }).to_string())
                        .header("Content-Type", "application/json"),
                    Some(token),
                )
                .await?;
            Ok(self.parse::<Note>(&response.body)?.into_comment())
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
            let url = format!(
                "{}/{}/{number}",
                self.project_url(full_name),
                item_path(kind)
            );
            let event = if open { "reopen" } else { "close" };
            self.send(
                Request::put(url, json!({ "state_event": event }).to_string())
                    .header("Content-Type", "application/json"),
                Some(token),
            )
            .await?;
            Ok(())
        })
    }

    fn pull_request_ref(&self, number: u64) -> Option<String> {
        Some(format!("merge-requests/{number}/head"))
    }
}

impl<C: HttpClient> GitLab<C> {
    fn project_url(&self, full_name: &str) -> String {
        format!("{}/projects/{}", self.api, percent_encode(full_name))
    }

    fn page_url(&self, first: String, cursor: Option<&String>) -> Result<String> {
        match cursor {
            Some(cursor) if cursor.starts_with(&format!("{}/", self.api)) => Ok(cursor.clone()),
            Some(_) => Err(Error::Rejected(
                "That page link is not from GitLab.".to_owned(),
            )),
            None => Ok(first),
        }
    }
}

impl User {
    fn into_person(self) -> Person {
        Person {
            username: self.username,
            display_name: self.name.filter(|n| !n.is_empty()),
            avatar_url: self.avatar_url,
        }
    }
}

fn labels(names: Vec<String>) -> Vec<Label> {
    names
        .into_iter()
        .map(|name| Label { name, color: None })
        .collect()
}

#[derive(Deserialize)]
struct Note {
    body: String,
    author: Option<User>,
    #[serde(default)]
    system: bool,
    created_at: String,
}

impl Note {
    fn into_comment(self) -> Comment {
        Comment {
            author: self.author.map(User::into_person).unwrap_or_default(),
            body: self.body,
            created_at: self.created_at,
        }
    }
}

fn item_path(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::PullRequest => "merge_requests",
        ItemKind::Issue => "issues",
    }
}

#[derive(Deserialize)]
struct MergeRequest {
    iid: u64,
    title: String,
    description: Option<String>,
    author: Option<User>,
    state: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    work_in_progress: bool,
    source_branch: String,
    target_branch: String,
    source_project_id: Option<u64>,
    target_project_id: Option<u64>,
    #[serde(default)]
    labels: Vec<String>,
    user_notes_count: Option<u32>,
    created_at: String,
    updated_at: String,
    web_url: String,
}

impl MergeRequest {
    fn into_pull_request(self) -> PullRequest {
        let state = match self.state.as_str() {
            "merged" => ItemState::Merged,
            "closed" | "locked" => ItemState::Closed,
            _ if self.draft || self.work_in_progress => ItemState::Draft,
            _ => ItemState::Open,
        };
        PullRequest {
            number: self.iid,
            title: self.title,
            body: self.description.unwrap_or_default(),
            author: self.author.map(User::into_person).unwrap_or_default(),
            state,
            head: self.source_branch,
            head_fork: match (self.source_project_id, self.target_project_id) {
                (Some(source), Some(target)) if source != target => {
                    Some(format!("project {source}"))
                }
                _ => None,
            },
            base: self.target_branch,
            labels: labels(self.labels),
            comments: self.user_notes_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
            web_url: self.web_url,
        }
    }
}

#[derive(Deserialize)]
struct IssueBody {
    iid: u64,
    title: String,
    description: Option<String>,
    author: Option<User>,
    state: String,
    #[serde(default)]
    labels: Vec<String>,
    user_notes_count: Option<u32>,
    created_at: String,
    updated_at: String,
    web_url: String,
}

impl IssueBody {
    fn into_issue(self) -> Issue {
        Issue {
            number: self.iid,
            title: self.title,
            body: self.description.unwrap_or_default(),
            author: self.author.map(User::into_person).unwrap_or_default(),
            state: if self.state == "closed" {
                ItemState::Closed
            } else {
                ItemState::Open
            },
            labels: labels(self.labels),
            comments: self.user_notes_count,
            created_at: self.created_at,
            updated_at: self.updated_at,
            web_url: self.web_url,
        }
    }
}

#[derive(Deserialize)]
struct User {
    username: String,
    name: Option<String>,
    avatar_url: Option<String>,
}

#[derive(Deserialize)]
struct Namespace {
    id: u64,
}

#[derive(Deserialize)]
struct ProjectNamespace {
    full_path: String,
}

#[derive(Deserialize)]
struct Project {
    path: String,
    path_with_namespace: String,
    namespace: ProjectNamespace,
    description: Option<String>,
    visibility: Option<String>,
    #[serde(rename = "forked_from_project")]
    forked_from: Option<Value>,
    #[serde(default)]
    archived: bool,
    default_branch: Option<String>,
    http_url_to_repo: String,
    ssh_url_to_repo: Option<String>,
    web_url: String,
    last_activity_at: Option<String>,
}

impl Project {
    fn into_remote(self) -> RemoteRepository {
        RemoteRepository {
            full_name: self.path_with_namespace,
            owner: self.namespace.full_path,
            name: self.path,
            description: self.description.filter(|d| !d.is_empty()),
            // Internal projects are visible to every signed-in user of the
            // instance, not the public: treat them as private.
            private: self.visibility.as_deref() != Some("public"),
            fork: self.forked_from.is_some_and(|f| !f.is_null()),
            archived: self.archived,
            default_branch: self.default_branch,
            clone_https: self.http_url_to_repo,
            clone_ssh: self.ssh_url_to_repo,
            web_url: self.web_url,
            updated_at: self.last_activity_at,
        }
    }
}
