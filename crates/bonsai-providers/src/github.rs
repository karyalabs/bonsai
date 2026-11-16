//! GitHub and GitHub Enterprise Server: OAuth device flow and REST v3.

use serde::Deserialize;
use serde_json::json;

use crate::{
    Account, BoxFuture, Capabilities, Error, HttpClient, NewRepository, Page, Provider,
    ProviderKind, RemoteRepository, RepositoryQuery, Request, Response, Result, form_encode,
    percent_encode,
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
                q.push_str(&format!(" org:{org}"));
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
}

/// The URL marked `rel="next"` in a `Link` header.
fn next_link(link: &str) -> Option<String> {
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
