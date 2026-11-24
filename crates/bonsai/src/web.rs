//! Where a repository lives on the web, for Open on GitHub and the like.

use bonsai_git::{Head, Snapshot};
use bonsai_providers::WebRepository;

/// The site of `remote`, if its URL points at GitHub, GitLab or Bitbucket.
#[must_use]
pub fn remote_web(snapshot: &Snapshot, remote: &str) -> Option<WebRepository> {
    let url = snapshot
        .remotes
        .iter()
        .find(|r| r.name == remote)?
        .fetch_url
        .as_deref()?;
    WebRepository::from_remote(url, None)
}

/// The repository's site: `origin`'s, or else the first remote's that is
/// hosted.
#[must_use]
pub fn repository_web(snapshot: &Snapshot) -> Option<WebRepository> {
    let mut remotes: Vec<_> = snapshot.remotes.iter().collect();
    remotes.sort_by_key(|r| r.name != "origin");
    remotes.into_iter().find_map(|r| {
        r.fetch_url
            .as_deref()
            .and_then(|url| WebRepository::from_remote(url, None))
    })
}

/// What a file link at the work tree points at: the current branch, or the
/// commit when detached.
#[must_use]
pub fn work_tree_revision(snapshot: &Snapshot) -> Option<String> {
    match &snapshot.head {
        Head::Branch { name, oid: Some(_) } => Some(name.clone()),
        Head::Branch { oid: None, .. } => None,
        Head::Detached { oid } => Some(oid.as_str().to_owned()),
    }
}
