//! GitLab.com and self-managed GitLab: OAuth with PKCE through a loopback
//! redirect, or a personal access token, and REST v4.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    Account, BoxFuture, Capabilities, Error, HttpClient, NewRepository, Page, Provider,
    ProviderKind, RemoteRepository, RepositoryQuery, Request, Response, Result, form_encode,
    github::next_link, oauth::Tokens, percent_encode,
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
