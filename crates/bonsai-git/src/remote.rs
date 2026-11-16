//! Recognizes why a network operation (clone, fetch, pull, push) failed, so the
//! app can say what to do instead of showing Git's raw message alone.

/// A recognized cause of a failed network operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteFailure {
    /// The server rejected or never received credentials.
    Authentication,
    /// SSH rejected the key, or no key was offered.
    SshKey,
    /// The SSH host key is unknown or changed.
    HostKey,
    /// The repository does not exist, or the account cannot see it.
    NotFound,
    /// The host could not be reached.
    Network,
}

impl RemoteFailure {
    /// Classifies Git's stderr; `None` when nothing is recognized.
    #[must_use]
    pub fn classify(stderr: &str) -> Option<Self> {
        let text = stderr.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|n| text.contains(n));
        if has(&[
            "host key verification failed",
            "remote host identification has changed",
        ]) {
            Some(Self::HostKey)
        } else if has(&[
            "permission denied (publickey",
            "no supported authentication methods",
        ]) {
            Some(Self::SshKey)
        } else if has(&[
            "authentication failed",
            "could not read username",
            "could not read password",
            "terminal prompts disabled",
            "invalid username or password",
            "http basic: access denied",
            "returned error: 403",
        ]) {
            Some(Self::Authentication)
        } else if has(&[
            "repository not found",
            "does not appear to be a git repository",
            "' not found",
            "returned error: 404",
        ]) {
            Some(Self::NotFound)
        } else if has(&[
            "could not resolve host",
            "failed to connect",
            "connection timed out",
            "connection refused",
            "network is unreachable",
            "operation timed out",
        ]) {
            Some(Self::Network)
        } else {
            None
        }
    }

    /// What to tell the user, in one or two sentences.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::Authentication => {
                "The server did not accept your credentials. Sign in to the account in \
                 Settings › Accounts, or check the token's permissions."
            }
            Self::SshKey => {
                "The server rejected your SSH key. Add your public key to the account, or \
                 check that ssh-agent has the key loaded."
            }
            Self::HostKey => {
                "SSH could not verify the server's identity. Connect once with `ssh` in \
                 Terminal to trust the host, or check ~/.ssh/known_hosts if its key changed."
            }
            Self::NotFound => {
                "The repository was not found. Check the URL, and that your account can \
                 see the repository."
            }
            Self::Network => "The server could not be reached. Check the URL and your connection.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_failures() {
        let cases = [
            (
                "fatal: Authentication failed for 'https://github.com/a/b.git/'",
                Some(RemoteFailure::Authentication),
            ),
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                Some(RemoteFailure::Authentication),
            ),
            (
                "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.",
                Some(RemoteFailure::SshKey),
            ),
            (
                "Host key verification failed.",
                Some(RemoteFailure::HostKey),
            ),
            (
                "remote: Repository not found.\nfatal: repository 'https://x/y/' not found",
                Some(RemoteFailure::NotFound),
            ),
            (
                "fatal: '/tmp/nope' does not appear to be a git repository",
                Some(RemoteFailure::NotFound),
            ),
            (
                "fatal: unable to access 'https://nohost/': Could not resolve host: nohost",
                Some(RemoteFailure::Network),
            ),
            ("fatal: destination path 'x' already exists", None),
        ];
        for (stderr, expected) in cases {
            assert_eq!(RemoteFailure::classify(stderr), expected, "{stderr}");
        }
    }
}
