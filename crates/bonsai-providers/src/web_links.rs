//! Links to a repository's pages on its hosting site, for Open on GitHub
//! and the like.

use crate::{ProviderKind, http::percent_encode, remote_url::hosted_repository};

/// A repository's home on GitHub, GitLab or Bitbucket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebRepository {
    pub kind: ProviderKind,
    /// Such as `https://github.com/ada/engine`.
    pub home: String,
}

/// Encodes each segment of `path`, keeping the slashes between them.
fn encode_path(path: &str) -> String {
    path.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

impl WebRepository {
    /// The site a remote URL points at. `kind` names the provider for a
    /// host whose name doesn't say, such as a company's own GitLab; without
    /// it such hosts are `None`.
    #[must_use]
    pub fn from_remote(url: &str, kind: Option<ProviderKind>) -> Option<Self> {
        let (host, path) = hosted_repository(url)?;
        let kind = kind.or_else(|| {
            if host.contains("github") {
                Some(ProviderKind::GitHub)
            } else if host.contains("gitlab") {
                Some(ProviderKind::GitLab)
            } else if host.contains("bitbucket") {
                Some(ProviderKind::Bitbucket)
            } else {
                None
            }
        })?;
        Some(Self {
            kind,
            home: format!("https://{host}/{path}"),
        })
    }

    /// "Open on GitHub" and the like.
    #[must_use]
    pub fn open_label(&self) -> String {
        format!("Open on {}", self.kind.name())
    }

    /// GitLab puts a repository's pages under `/-/`.
    fn pages(&self) -> String {
        match self.kind {
            ProviderKind::GitLab => format!("{}/-", self.home),
            ProviderKind::GitHub | ProviderKind::Bitbucket => self.home.clone(),
        }
    }

    #[must_use]
    pub fn commit(&self, oid: &str) -> String {
        match self.kind {
            ProviderKind::Bitbucket => format!("{}/commits/{oid}", self.home),
            ProviderKind::GitHub | ProviderKind::GitLab => format!("{}/commit/{oid}", self.pages()),
        }
    }

    /// A branch's files.
    #[must_use]
    pub fn branch(&self, name: &str) -> String {
        let name = encode_path(name);
        match self.kind {
            ProviderKind::Bitbucket => format!("{}/src/{name}", self.home),
            ProviderKind::GitHub | ProviderKind::GitLab => format!("{}/tree/{name}", self.pages()),
        }
    }

    /// A file at `revision`, scrolled to `line` when given.
    #[must_use]
    pub fn file(&self, revision: &str, path: &str, line: Option<u32>) -> String {
        let path = encode_path(path);
        let revision = encode_path(revision);
        match self.kind {
            ProviderKind::Bitbucket => {
                let anchor = line.map(|n| format!("#lines-{n}")).unwrap_or_default();
                format!("{}/src/{revision}/{path}{anchor}", self.home)
            }
            ProviderKind::GitHub | ProviderKind::GitLab => {
                let anchor = line.map(|n| format!("#L{n}")).unwrap_or_default();
                format!("{}/blob/{revision}/{path}{anchor}", self.pages())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_to_github_pages() {
        let web = WebRepository::from_remote("git@github.com:ada/engine.git", None).unwrap();
        assert_eq!(web.open_label(), "Open on GitHub");
        assert_eq!(
            web.commit("abc"),
            "https://github.com/ada/engine/commit/abc"
        );
        assert_eq!(
            web.branch("feature/new tabs"),
            "https://github.com/ada/engine/tree/feature/new%20tabs"
        );
        assert_eq!(
            web.file("abc", "src/main.rs", Some(12)),
            "https://github.com/ada/engine/blob/abc/src/main.rs#L12"
        );
    }

    #[test]
    fn links_to_gitlab_and_bitbucket_pages() {
        let lab =
            WebRepository::from_remote("https://gitlab.com/team/sub/engine.git", None).unwrap();
        assert_eq!(
            lab.commit("abc"),
            "https://gitlab.com/team/sub/engine/-/commit/abc"
        );
        assert_eq!(
            lab.file("main", "a.rs", None),
            "https://gitlab.com/team/sub/engine/-/blob/main/a.rs"
        );
        let bit = WebRepository::from_remote("git@bitbucket.org:ada/engine.git", None).unwrap();
        assert_eq!(
            bit.commit("abc"),
            "https://bitbucket.org/ada/engine/commits/abc"
        );
        assert_eq!(
            bit.branch("main"),
            "https://bitbucket.org/ada/engine/src/main"
        );
        assert_eq!(
            bit.file("abc", "a.rs", Some(3)),
            "https://bitbucket.org/ada/engine/src/abc/a.rs#lines-3"
        );
    }

    #[test]
    fn needs_a_kind_for_an_unknown_host() {
        let url = "git@git.example.com:team/engine.git";
        assert_eq!(WebRepository::from_remote(url, None), None);
        let web = WebRepository::from_remote(url, Some(ProviderKind::GitLab)).unwrap();
        assert_eq!(
            web.branch("main"),
            "https://git.example.com/team/engine/-/tree/main"
        );
        assert_eq!(WebRepository::from_remote("/tmp/local", None), None);
    }
}
