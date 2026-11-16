//! Parsing the prompts Git passes to `GIT_ASKPASS`.

/// What Git asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    Username,
    Password,
}

/// A credential prompt such as `Password for 'https://ada@github.com': `.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prompt {
    pub kind: PromptKind,
    /// `https` or `http`.
    pub scheme: String,
    /// Host name, lowercase, with a port when the URL has one.
    pub host: String,
    /// The user in the URL, when Git already knows it.
    pub username: Option<String>,
}

impl Prompt {
    /// Parses one of Git's credential prompts; anything else (an SSH
    /// passphrase, a host key question) is `None`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (kind, rest) = if let Some(rest) = text.strip_prefix("Username for ") {
            (PromptKind::Username, rest)
        } else {
            let rest = text.strip_prefix("Password for ")?;
            (PromptKind::Password, rest)
        };
        let url = rest.strip_suffix(':')?.trim();
        let url = url.strip_prefix('\'')?.strip_suffix('\'')?;
        let (scheme, rest) = url.split_once("://")?;
        if !matches!(scheme, "https" | "http") {
            return None;
        }
        let authority = rest.split(['/', '?', '#']).next()?;
        let (username, host) = match authority.rsplit_once('@') {
            Some((user, host)) => (Some(percent_decode(user)?), host),
            None => (None, authority),
        };
        if host.is_empty() {
            return None;
        }
        Some(Self {
            kind,
            scheme: scheme.to_owned(),
            host: host.to_ascii_lowercase(),
            username: username.filter(|u| !u.is_empty()),
        })
    }
}

/// Decodes `%XX` escapes, as Git writes a user name with `@` or `:` in it.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = text.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_username_and_password_prompts() {
        assert_eq!(
            Prompt::parse("Username for 'https://github.com': "),
            Some(Prompt {
                kind: PromptKind::Username,
                scheme: "https".into(),
                host: "github.com".into(),
                username: None,
            })
        );
        assert_eq!(
            Prompt::parse("Password for 'https://ada%40corp@GitLab.example.com:8443': "),
            Some(Prompt {
                kind: PromptKind::Password,
                scheme: "https".into(),
                host: "gitlab.example.com:8443".into(),
                username: Some("ada@corp".into()),
            })
        );
    }

    #[test]
    fn declines_other_prompts() {
        for text in [
            "Enter passphrase for key '/Users/ada/.ssh/id_ed25519': ",
            "Are you sure you want to continue connecting (yes/no)? ",
            "Username for 'ssh://github.com': ",
            "Password for 'https://': ",
            "Password for https://github.com: ",
            "",
        ] {
            assert_eq!(Prompt::parse(text), None, "{text}");
        }
    }
}
