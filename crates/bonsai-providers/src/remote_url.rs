//! Which hosted repository a Git remote URL points at.

/// The host and `owner/name` (the full namespace path on GitLab) of a
/// remote URL in any of Git's forms: `https://host/owner/name.git`,
/// `git@host:owner/name.git` or `ssh://git@host:22/owner/name`.
#[must_use]
pub fn hosted_repository(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if scheme == "file" {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        // A port is not part of the host's identity.
        (host.split(':').next()?, path)
    } else {
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') || authority.is_empty() {
            return None;
        }
        (
            authority.rsplit_once('@').map_or(authority, |(_, h)| h),
            path,
        )
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    // GitLab serves the same repository under `/-/`-free paths only.
    (!host.is_empty() && path.contains('/') && !path.contains("/-/"))
        .then(|| (host.to_ascii_lowercase(), path.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::hosted_repository;

    #[test]
    fn reads_every_url_form() {
        let expect = |url: &str, host: &str, path: &str| {
            assert_eq!(
                hosted_repository(url),
                Some((host.to_owned(), path.to_owned())),
                "{url}"
            );
        };
        expect(
            "https://github.com/Ada/Engine.git",
            "github.com",
            "Ada/Engine",
        );
        expect("git@github.com:ada/engine.git", "github.com", "ada/engine");
        expect(
            "ssh://git@gitlab.example.com:2222/team/sub/engine",
            "gitlab.example.com",
            "team/sub/engine",
        );
        expect(
            "https://ada@bitbucket.org/analytical/engine.git",
            "bitbucket.org",
            "analytical/engine",
        );
        assert_eq!(hosted_repository("/srv/git/engine.git"), None);
        assert_eq!(hosted_repository("file:///srv/git/engine.git"), None);
        assert_eq!(hosted_repository("https://github.com/ada"), None);
    }
}
