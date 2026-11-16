//! Connected accounts (CONTEXT.md: Account): who is signed in where. The
//! list (no secrets) lives in `accounts.json` beside the session; tokens
//! live in the Keychain under (host, username). Askpass reads the same
//! list, so a new sign-in reaches Git at once.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use bonsai_keychain::{AccountRef, Secret, TokenKey, TokenStore};
use bonsai_providers::{
    Account, Bitbucket, BitbucketCredential, GitHub, GitLab, HttpClient, Provider, ProviderKind,
};
use serde::{Deserialize, Serialize};

/// How the account signed in, which decides what Git and the API send.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SignInMethod {
    /// A personal access token, or a GitHub device-flow token.
    #[default]
    Token,
    /// An OAuth access token (GitLab, Bitbucket).
    OAuth,
    /// A Bitbucket API token, which the API pairs with the account's email.
    ApiToken { email: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connected {
    pub account: Account,
    #[serde(default)]
    pub method: SignInMethod,
}

impl Connected {
    /// The user name Git sends with the token over HTTPS.
    #[must_use]
    pub fn git_username(&self) -> String {
        match (self.account.kind, &self.method) {
            (ProviderKind::GitHub, _) => "x-access-token".to_owned(),
            (ProviderKind::GitLab, _) => "oauth2".to_owned(),
            (ProviderKind::Bitbucket, SignInMethod::OAuth) => "x-token-auth".to_owned(),
            (ProviderKind::Bitbucket, _) => self.account.username.clone(),
        }
    }

    #[must_use]
    pub fn key(&self) -> TokenKey {
        TokenKey::new(&self.account.host, &self.account.username)
    }

    /// The credential string the provider's API calls take.
    #[must_use]
    pub fn api_token(&self, token: &Secret) -> String {
        match &self.method {
            SignInMethod::ApiToken { email } => BitbucketCredential::ApiToken {
                email: email.clone(),
                token: token.expose().to_owned(),
            }
            .encode(),
            _ => token.expose().to_owned(),
        }
    }
}

/// The provider for a host.
pub fn provider<C: HttpClient + 'static>(
    kind: ProviderKind,
    host: &str,
    client: C,
) -> Box<dyn Provider> {
    match kind {
        ProviderKind::GitHub if host == "github.com" => Box::new(GitHub::new(client)),
        ProviderKind::GitHub => Box::new(GitHub::enterprise(client, host)),
        ProviderKind::GitLab => Box::new(GitLab::self_managed(client, host)),
        ProviderKind::Bitbucket => Box::new(Bitbucket::new(client)),
    }
}

/// The default host for each provider.
#[must_use]
pub fn default_host(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::GitHub => "github.com",
        ProviderKind::GitLab => "gitlab.com",
        ProviderKind::Bitbucket => "bitbucket.org",
    }
}

#[must_use]
pub fn accounts_path(data_dir: &Path) -> PathBuf {
    data_dir.join("accounts.json")
}

pub struct Accounts {
    path: PathBuf,
    list: Vec<Connected>,
    store: Arc<dyn TokenStore>,
}

impl Accounts {
    /// Reads the list; a missing or unreadable file is no accounts.
    pub fn load(path: PathBuf, store: Arc<dyn TokenStore>) -> Self {
        let list = read(&path);
        Self { path, list, store }
    }

    #[must_use]
    pub fn list(&self) -> &[Connected] {
        &self.list
    }

    #[must_use]
    pub fn store(&self) -> &Arc<dyn TokenStore> {
        &self.store
    }

    /// The account's token from the Keychain, if it is still there.
    #[must_use]
    pub fn token(&self, connected: &Connected) -> Option<Secret> {
        self.store.get(&connected.key()).ok().flatten()
    }

    /// Adds or replaces an account and stores its token.
    pub fn add(&mut self, connected: Connected, token: &Secret) -> Result<(), String> {
        self.store
            .set(&connected.key(), token)
            .map_err(|e| e.to_string())?;
        self.list.retain(|c| c.key() != connected.key());
        self.list.push(connected);
        self.list.sort_by(|a, b| {
            (a.account.kind.name(), &a.account.host, &a.account.username).cmp(&(
                b.account.kind.name(),
                &b.account.host,
                &b.account.username,
            ))
        });
        self.save()
    }

    /// Signs out: forgets the account and deletes its token.
    pub fn remove(&mut self, key: &TokenKey) -> Result<(), String> {
        self.store.delete(key).map_err(|e| e.to_string())?;
        self.list.retain(|c| c.key() != *key);
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let write = || -> std::io::Result<()> {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temp = self.path.with_extension("json.tmp");
            std::fs::write(&temp, serde_json::to_vec_pretty(&self.list)?)?;
            std::fs::rename(temp, &self.path)
        };
        write().map_err(|e| format!("Bonsai could not save the accounts list: {e}"))
    }
}

fn read(path: &Path) -> Vec<Connected> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// What askpass may answer for, from the accounts file.
#[must_use]
pub fn askpass_accounts(path: &Path) -> Vec<AccountRef> {
    read(path)
        .into_iter()
        .map(|c| AccountRef {
            git_username: c.git_username(),
            host: c.account.host,
            account: c.account.username,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use bonsai_keychain::MemoryStore;

    use super::*;

    fn connected(kind: ProviderKind, host: &str, user: &str, method: SignInMethod) -> Connected {
        Connected {
            account: Account {
                kind,
                host: host.into(),
                api_url: String::new(),
                username: user.into(),
                display_name: None,
                avatar_url: None,
            },
            method,
        }
    }

    #[test]
    fn adds_replaces_and_signs_out_keeping_tokens_out_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = accounts_path(dir.path());
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        let mut accounts = Accounts::load(path.clone(), store.clone());
        let github = connected(
            ProviderKind::GitHub,
            "github.com",
            "ada",
            SignInMethod::Token,
        );
        let bitbucket = connected(
            ProviderKind::Bitbucket,
            "bitbucket.org",
            "ada",
            SignInMethod::ApiToken {
                email: "ada@example.com".into(),
            },
        );
        accounts
            .add(github.clone(), &Secret::new("ghp_one"))
            .unwrap();
        accounts
            .add(bitbucket.clone(), &Secret::new("ATATT"))
            .unwrap();
        accounts
            .add(github.clone(), &Secret::new("ghp_two"))
            .unwrap();

        let reloaded = Accounts::load(path.clone(), store.clone());
        assert_eq!(reloaded.list().len(), 2);
        assert_eq!(reloaded.token(&github).unwrap().expose(), "ghp_two");
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(!file.contains("ghp_") && !file.contains("ATATT"));

        assert_eq!(
            bitbucket.api_token(&Secret::new("ATATT")),
            "ada@example.com:ATATT"
        );
        let refs = askpass_accounts(&path);
        assert_eq!(
            refs[0].git_username, "ada",
            "Bitbucket API tokens use the username"
        );
        assert_eq!(refs[1].git_username, "x-access-token");

        accounts.remove(&github.key()).unwrap();
        assert_eq!(accounts.token(&github), None);
        assert_eq!(Accounts::load(path, store).list(), [bitbucket]);
    }
}
