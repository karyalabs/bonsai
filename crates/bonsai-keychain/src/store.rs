//! Where tokens are kept.

use std::{collections::HashMap, fmt, sync::Mutex};

/// The Keychain service every Bonsai token is stored under.
pub const SERVICE: &str = "dev.bonsai.token";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The Keychain refused the request: {0}")]
    Keychain(String),
    #[error("The token is not valid text.")]
    NotText,
    #[error("This system has no token store.")]
    Unsupported,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// A token, kept out of `Debug` output and logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The token itself; call only where it is handed to Git or a server.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Which token: a provider host and the account on it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TokenKey {
    pub host: String,
    pub account: String,
}

impl TokenKey {
    #[must_use]
    pub fn new(host: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            host: host.into().to_ascii_lowercase(),
            account: account.into(),
        }
    }

    /// The Keychain account field: `host/account`.
    #[must_use]
    pub fn keychain_account(&self) -> String {
        format!("{}/{}", self.host, self.account)
    }
}

pub trait TokenStore: Send + Sync {
    fn get(&self, key: &TokenKey) -> Result<Option<Secret>>;
    fn set(&self, key: &TokenKey, token: &Secret) -> Result<()>;
    /// Deleting a token that is not there succeeds.
    fn delete(&self, key: &TokenKey) -> Result<()>;
}

/// Tokens in memory, for tests and for systems without a Keychain.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<TokenKey, Secret>>);

impl TokenStore for MemoryStore {
    fn get(&self, key: &TokenKey) -> Result<Option<Secret>> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .cloned())
    }

    fn set(&self, key: &TokenKey, token: &Secret) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), token.clone());
        Ok(())
    }

    fn delete(&self, key: &TokenKey) -> Result<()> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
        Ok(())
    }
}

/// The macOS login Keychain, as generic passwords under [`SERVICE`].
#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, Default)]
pub struct Keychain;

#[cfg(target_os = "macos")]
impl TokenStore for Keychain {
    fn get(&self, key: &TokenKey) -> Result<Option<Secret>> {
        use security_framework::passwords::get_generic_password;
        match get_generic_password(SERVICE, &key.keychain_account()) {
            Ok(bytes) => String::from_utf8(bytes)
                .map(|t| Some(Secret(t)))
                .map_err(|_| Error::NotText),
            Err(error) if error.code() == ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(Error::Keychain(error.to_string())),
        }
    }

    fn set(&self, key: &TokenKey, token: &Secret) -> Result<()> {
        security_framework::passwords::set_generic_password(
            SERVICE,
            &key.keychain_account(),
            token.expose().as_bytes(),
        )
        .map_err(|error| Error::Keychain(error.to_string()))
    }

    fn delete(&self, key: &TokenKey) -> Result<()> {
        match security_framework::passwords::delete_generic_password(
            SERVICE,
            &key.keychain_account(),
        ) {
            Err(error) if error.code() != ITEM_NOT_FOUND => Err(Error::Keychain(error.to_string())),
            _ => Ok(()),
        }
    }
}

/// `errSecItemNotFound`.
#[cfg(target_os = "macos")]
const ITEM_NOT_FOUND: i32 = -25_300;

/// The system's token store: the Keychain on macOS, and elsewhere a store
/// that keeps nothing, so askpass declines and Git uses the user's setup.
#[must_use]
pub fn default_store() -> Box<dyn TokenStore> {
    #[cfg(target_os = "macos")]
    {
        Box::new(Keychain)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(MemoryStore::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_and_hides_tokens_from_debug() {
        let store = MemoryStore::default();
        let key = TokenKey::new("GitHub.com", "ada");
        assert_eq!(key.keychain_account(), "github.com/ada");
        assert_eq!(store.get(&key).unwrap(), None);
        store.set(&key, &Secret::new("ghp_secret")).unwrap();
        let token = store.get(&key).unwrap().unwrap();
        assert_eq!(token.expose(), "ghp_secret");
        assert!(!format!("{token:?}").contains("ghp_secret"));
        store.delete(&key).unwrap();
        store.delete(&key).unwrap();
        assert_eq!(store.get(&key).unwrap(), None);
    }
}
