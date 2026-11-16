//! Answering Git's credential prompts for connected accounts.

use std::fmt::Write as _;

use crate::{Prompt, PromptKind, Secret, TokenKey, TokenStore};

/// The environment variable that tells the askpass process which accounts
/// are connected. It holds host and user names only, never a token.
pub const ACCOUNTS_ENV: &str = "BONSAI_ASKPASS_ACCOUNTS";

/// A connected account askpass may answer for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRef {
    /// Host, lowercase, with a port when not the default.
    pub host: String,
    /// The account's name, which keys its token.
    pub account: String,
    /// The user name Git sends with the token: `x-access-token` for GitHub,
    /// `oauth2` for GitLab, the account name for Bitbucket.
    pub git_username: String,
}

/// `host user git-user` per line, each field percent-encoded.
#[must_use]
pub fn encode_accounts(accounts: &[AccountRef]) -> String {
    accounts
        .iter()
        .map(|a| {
            [&a.host, &a.account, &a.git_username]
                .map(|f| encode(f))
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[must_use]
pub fn decode_accounts(text: &str) -> Vec<AccountRef> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ').map(decode);
            Some(AccountRef {
                host: fields.next()??.to_ascii_lowercase(),
                account: fields.next()??,
                git_username: fields.next()??,
            })
        })
        .collect()
}

fn encode(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for byte in field.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~:@".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn decode(field: &str) -> Option<String> {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(field.get(i + 1..i + 3)?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok().filter(|s| !s.is_empty())
}

/// What to print for `prompt`, or `None` to decline so Git falls back to
/// the user's own credential setup. Only HTTPS prompts are answered (plain
/// HTTP only on loopback, for local servers), and a password only goes to
/// the host its account belongs to.
#[must_use]
pub fn answer(prompt: &str, accounts: &[AccountRef], store: &dyn TokenStore) -> Option<Secret> {
    let prompt = Prompt::parse(prompt)?;
    if prompt.scheme != "https" && !is_loopback(&prompt.host) {
        return None;
    }
    // When Git already chose a user, answer only for the matching account.
    // Without one, two accounts on the host leave Git's intent unclear.
    let account = if let Some(user) = &prompt.username {
        accounts
            .iter()
            .find(|a| a.host == prompt.host && (a.git_username == *user || a.account == *user))?
    } else {
        let mut candidates = accounts.iter().filter(|a| a.host == prompt.host);
        let first = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        first
    };
    match prompt.kind {
        PromptKind::Username => Some(Secret::new(account.git_username.clone())),
        PromptKind::Password => store
            .get(&TokenKey::new(&account.host, &account.account))
            .ok()
            .flatten(),
    }
}

/// `localhost`, `127.x.x.x` or `[::1]`, with or without a port.
fn is_loopback(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or_default()
    } else {
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    name == "localhost"
        || name == "::1"
        || name
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStore;

    fn accounts() -> Vec<AccountRef> {
        vec![
            AccountRef {
                host: "github.com".into(),
                account: "ada".into(),
                git_username: "x-access-token".into(),
            },
            AccountRef {
                host: "gitlab.example.com:8443".into(),
                account: "ada lovelace".into(),
                git_username: "oauth2".into(),
            },
        ]
    }

    #[test]
    fn accounts_survive_the_environment() {
        let encoded = encode_accounts(&accounts());
        assert!(!encoded.contains("ada lovelace"));
        assert_eq!(decode_accounts(&encoded), accounts());
        assert_eq!(decode_accounts("broken line\n\n"), []);
    }

    #[test]
    fn answers_only_for_connected_hosts() {
        let store = MemoryStore::default();
        store
            .set(&TokenKey::new("github.com", "ada"), &Secret::new("ghp_1"))
            .unwrap();
        let accounts = accounts();
        let ask = |prompt: &str| answer(prompt, &accounts, &store).map(|s| s.expose().to_owned());
        assert_eq!(
            ask("Username for 'https://github.com': ").as_deref(),
            Some("x-access-token")
        );
        assert_eq!(
            ask("Password for 'https://x-access-token@github.com': ").as_deref(),
            Some("ghp_1")
        );
        // Another host, another user, plain HTTP, or no stored token: decline.
        assert_eq!(
            ask("Password for 'https://x-access-token@evil.example': "),
            None
        );
        assert_eq!(ask("Password for 'https://someone@github.com': "), None);
        assert_eq!(
            ask("Password for 'http://x-access-token@github.com': "),
            None
        );
        assert_eq!(
            ask("Password for 'https://oauth2@gitlab.example.com:8443': "),
            None
        );
        assert!(
            is_loopback("127.0.0.1:8080") && is_loopback("[::1]:9") && is_loopback("localhost")
        );
        assert!(!is_loopback("github.com") && !is_loopback("10.0.0.1:80"));
        assert_eq!(ask("Enter passphrase for key 'id_ed25519': "), None);
    }

    #[test]
    fn two_accounts_on_one_host_need_the_user_in_the_url() {
        let mut accounts = accounts();
        accounts.push(AccountRef {
            host: "github.com".into(),
            account: "work".into(),
            git_username: "work".into(),
        });
        let store = MemoryStore::default();
        store
            .set(
                &TokenKey::new("github.com", "work"),
                &Secret::new("ghp_work"),
            )
            .unwrap();
        assert_eq!(
            answer("Username for 'https://github.com': ", &accounts, &store),
            None
        );
        assert_eq!(
            answer(
                "Password for 'https://work@github.com': ",
                &accounts,
                &store
            )
            .map(|s| s.expose().to_owned())
            .as_deref(),
            Some("ghp_work")
        );
    }
}
