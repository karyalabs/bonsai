//! GitLab against responses shaped like the REST v4 API's documented ones.

mod recorded;

use bonsai_providers::{
    Error, GitLab, Method, NewRepository, Pkce, Provider, ProviderKind, RepositoryQuery,
};
use futures::executor::block_on;
use recorded::{Recorded, json, with_header};

fn project(path: &str, visibility: &str, fork: bool) -> String {
    let (namespace, name) = path.rsplit_once('/').unwrap();
    let fork = if fork { r#"{"id":1}"# } else { "null" };
    format!(
        r#"{{"id":3,"name":"{name}","path":"{name}","path_with_namespace":"{path}","namespace":{{"id":9,"full_path":"{namespace}"}},"description":null,"visibility":"{visibility}","forked_from_project":{fork},"archived":false,"default_branch":"main","http_url_to_repo":"https://gitlab.com/{path}.git","ssh_url_to_repo":"git@gitlab.com:{path}.git","web_url":"https://gitlab.com/{path}","last_activity_at":"2026-11-12T10:00:00.000Z"}}"#
    )
}

#[test]
fn reads_the_user_and_lists_projects_with_search_and_pages() {
    let next = "https://gitlab.com/api/v4/projects?membership=true&page=2";
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://gitlab.com/api/v4/user",
            json(
                200,
                r#"{"id":1,"username":"ada","name":"Ada Lovelace","avatar_url":null}"#,
            ),
        ),
        (
            Method::Get,
            "https://gitlab.com/api/v4/projects?membership=true&order_by=last_activity_at&sort=desc&per_page=50&search_namespaces=true&search=analytical%20engine",
            with_header(
                json(
                    200,
                    &format!(
                        "[{},{}]",
                        project("ada/analytical-engine", "internal", false),
                        project("difference/team/engine", "public", true)
                    ),
                ),
                "Link",
                &format!(r#"<{next}>; rel="next""#),
            ),
        ),
    ]);
    let gitlab = GitLab::new(&client);
    let account = block_on(gitlab.current_user("glpat-x")).unwrap();
    assert_eq!(account.kind, ProviderKind::GitLab);
    assert_eq!(account.username, "ada");
    assert_eq!(gitlab.git_username(&account), "oauth2");

    let query = RepositoryQuery {
        search: "analytical engine".into(),
        cursor: None,
    };
    let page = block_on(gitlab.repositories("glpat-x", &query)).unwrap();
    assert_eq!(page.items[0].full_name, "ada/analytical-engine");
    assert!(page.items[0].private, "internal counts as private");
    assert!(!page.items[0].fork);
    assert_eq!(page.items[1].owner, "difference/team");
    assert!(page.items[1].fork && !page.items[1].private);
    assert_eq!(page.next.as_deref(), Some(next));
    assert_eq!(
        client.requests()[0].header_value("authorization"),
        Some("Bearer glpat-x")
    );
}

#[test]
fn creates_in_a_group_and_explains_rejections() {
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://gitlab.example.com/api/v4/namespaces/difference%2Fteam",
            json(200, r#"{"id":42,"full_path":"difference/team"}"#),
        ),
        (
            Method::Post,
            "https://gitlab.example.com/api/v4/projects",
            json(201, &project("difference/team/fresh", "private", false)),
        ),
        (
            Method::Post,
            "https://gitlab.example.com/api/v4/projects",
            json(
                400,
                r#"{"message":{"name":["has already been taken"],"path":["has already been taken"]}}"#,
            ),
        ),
        (
            Method::Get,
            "https://gitlab.example.com/api/v4/projects/ada%2Fgone",
            json(404, r#"{"message":"404 Project Not Found"}"#),
        ),
        (
            Method::Get,
            "https://gitlab.example.com/api/v4/user",
            with_header(json(429, "Retry later"), "RateLimit-Reset", "1800000000"),
        ),
        (
            Method::Get,
            "https://gitlab.example.com/api/v4/user",
            json(401, r#"{"message":"401 Unauthorized"}"#),
        ),
    ]);
    let gitlab = GitLab::self_managed(&client, "gitlab.example.com");
    let mut new = NewRepository {
        name: "fresh".into(),
        description: None,
        private: true,
        owner: Some("difference/team".into()),
    };
    let created = block_on(gitlab.create_repository("t", &new)).unwrap();
    assert_eq!(created.full_name, "difference/team/fresh");
    let body: serde_json::Value = serde_json::from_slice(&client.requests()[1].body).unwrap();
    assert_eq!(body["namespace_id"], 42);
    assert_eq!(body["visibility"], "private");

    new.owner = None;
    assert_eq!(
        block_on(gitlab.create_repository("t", &new)),
        Err(Error::Rejected(
            "name has already been taken; path has already been taken".into()
        ))
    );
    assert_eq!(
        block_on(gitlab.repository("t", "ada/gone")),
        Err(Error::NotFound)
    );
    assert_eq!(
        block_on(gitlab.current_user("t")).unwrap_err().retry_at(),
        Some(1_800_000_000)
    );
    assert_eq!(
        block_on(gitlab.current_user("t")),
        Err(Error::Unauthorized("gitlab.example.com".into()))
    );
}

#[test]
fn signs_in_with_pkce_and_refreshes() {
    let client = Recorded::new(vec![
        (
            Method::Post,
            "https://gitlab.com/oauth/token",
            json(
                200,
                r#"{"access_token":"de6780bc506a0446309bd9362820ba8aed28aa506c71eedbe1c5c4f9dd350e54","token_type":"Bearer","expires_in":7200,"refresh_token":"8257e65c97202ed1726cf9571600918f3bffb2544b26e00a61df9897668c33a1","created_at":1607635748}"#,
            ),
        ),
        (
            Method::Post,
            "https://gitlab.com/oauth/token",
            json(
                200,
                r#"{"access_token":"new","token_type":"Bearer","expires_in":7200,"refresh_token":"next"}"#,
            ),
        ),
        (
            Method::Post,
            "https://gitlab.com/oauth/token",
            json(
                400,
                r#"{"error":"invalid_grant","error_description":"The provided authorization grant is invalid"}"#,
            ),
        ),
    ]);
    let gitlab = GitLab::new(&client);
    let pkce = Pkce::from_parts("verifier-123".into(), "state-9".into());
    let url = gitlab.authorize_url("app-id", "http://127.0.0.1:47011/callback", &pkce);
    assert!(url.starts_with("https://gitlab.com/oauth/authorize?client_id=app-id&redirect_uri=http%3A%2F%2F127.0.0.1%3A47011%2Fcallback&response_type=code&state=state-9"));
    assert!(url.ends_with(&format!(
        "code_challenge={}&code_challenge_method=S256",
        pkce.challenge
    )));

    let tokens = block_on(gitlab.exchange_code(
        "app-id",
        "http://127.0.0.1:47011/callback",
        "code-1",
        &pkce.verifier,
    ))
    .unwrap();
    assert_eq!(tokens.expires_in, Some(7200));
    assert!(!format!("{tokens:?}").contains("de6780"));
    let body = String::from_utf8(client.requests()[0].body.clone()).unwrap();
    assert!(body.contains("code_verifier=verifier-123"));
    assert!(body.contains("grant_type=authorization_code"));

    let refreshed = block_on(gitlab.refresh(
        "app-id",
        "http://127.0.0.1:47011/callback",
        tokens.refresh_token.as_deref().unwrap(),
    ))
    .unwrap();
    assert_eq!(refreshed.access_token, "new");
    assert_eq!(
        block_on(gitlab.refresh("app-id", "http://127.0.0.1:47011/callback", "stale")),
        Err(Error::Rejected(
            "The provided authorization grant is invalid".into()
        ))
    );
}
