//! Where a repository lives on the web, for Open on GitHub and the like.

use bonsai_git::{Head, Snapshot};
use bonsai_providers::{ProviderKind, WebRepository, hosted_repository};
use gpui_kit::{App, Global};

/// The hosts of the signed-in accounts and their providers, so a server
/// whose name doesn't say, such as a company's own GitLab, still links.
#[derive(Clone, Debug, Default)]
pub struct AccountHosts(pub Vec<(String, ProviderKind)>);

impl Global for AccountHosts {}

/// The site `url` points at, using a signed-in account's provider for its
/// host when there is one.
fn web_for(url: &str, cx: &App) -> Option<WebRepository> {
    let hosts = cx.try_global::<AccountHosts>().map_or(&[][..], |h| &h.0);
    web_with_hosts(url, hosts)
}

fn web_with_hosts(url: &str, hosts: &[(String, ProviderKind)]) -> Option<WebRepository> {
    let kind = hosted_repository(url).and_then(|(host, _)| {
        hosts
            .iter()
            .find(|(h, _)| *h == host)
            .map(|(_, kind)| *kind)
    });
    WebRepository::from_remote(url, kind)
}

/// The site of `remote`, if its URL points at GitHub, GitLab or Bitbucket.
#[must_use]
pub fn remote_web(snapshot: &Snapshot, remote: &str, cx: &App) -> Option<WebRepository> {
    let url = snapshot
        .remotes
        .iter()
        .find(|r| r.name == remote)?
        .fetch_url
        .as_deref()?;
    web_for(url, cx)
}

/// The repository's site: `origin`'s, or else the first remote's that is
/// hosted.
#[must_use]
pub fn repository_web(snapshot: &Snapshot, cx: &App) -> Option<WebRepository> {
    let mut remotes: Vec<_> = snapshot.remotes.iter().collect();
    remotes.sort_by_key(|r| r.name != "origin");
    remotes
        .into_iter()
        .find_map(|r| r.fetch_url.as_deref().and_then(|url| web_for(url, cx)))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signed_in_account_names_a_self_hosted_site() {
        let url = "git@git.example.com:team/engine.git";
        assert_eq!(web_with_hosts(url, &[]), None);
        let hosts = [("git.example.com".to_owned(), ProviderKind::GitLab)];
        let web = web_with_hosts(url, &hosts).unwrap();
        assert_eq!(web.open_label(), "Open on GitLab");
        assert_eq!(
            web.commit("abc"),
            "https://git.example.com/team/engine/-/commit/abc"
        );
        // A host that names its site needs no account.
        assert!(web_with_hosts("git@github.com:ada/engine.git", &[]).is_some());
    }
}
