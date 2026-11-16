//! GitHub against responses shaped like the REST API's documented ones.

mod recorded;

use bonsai_providers::{
    DevicePoll, Error, GitHub, Method, NewRepository, Provider, ProviderKind, RepositoryQuery,
};
use futures::executor::block_on;
use recorded::{Offline, Recorded, json, with_header};

const USER: &str = r#"{"login":"ada","id":1,"name":"Ada Lovelace","avatar_url":"https://avatars.githubusercontent.com/u/1"}"#;

fn repo(full: &str, private: bool) -> String {
    let (owner, name) = full.split_once('/').unwrap();
    format!(
        r#"{{"id":7,"name":"{name}","full_name":"{full}","owner":{{"login":"{owner}"}},"private":{private},"description":"","fork":false,"archived":false,"default_branch":"main","clone_url":"https://github.com/{full}.git","ssh_url":"git@github.com:{full}.git","html_url":"https://github.com/{full}","updated_at":"2026-11-10T09:00:00Z"}}"#
    )
}

#[test]
fn reads_the_signed_in_user() {
    let client = Recorded::new(vec![(
        Method::Get,
        "https://api.github.com/user",
        json(200, USER),
    )]);
    let github = GitHub::new(&client);
    let account = block_on(github.current_user("gho_token")).unwrap();
    assert_eq!(account.kind, ProviderKind::GitHub);
    assert_eq!(account.username, "ada");
    assert_eq!(account.display_name.as_deref(), Some("Ada Lovelace"));
    assert_eq!(account.host, "github.com");
    let request = &client.requests()[0];
    assert_eq!(
        request.header_value("authorization"),
        Some("Bearer gho_token")
    );
    assert_eq!(request.header_value("user-agent"), Some("Bonsai"));
}

#[test]
fn pages_through_repositories_with_link_headers() {
    let first = format!(
        "[{},{}]",
        repo("ada/engine", true),
        repo("analytical/notes", false)
    );
    let second = format!("[{}]", repo("ada/old", false));
    let next = "https://api.github.com/user/repos?per_page=50&page=2";
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.github.com/user/repos?per_page=50&sort=updated&affiliation=owner,collaborator,organization_member",
            with_header(
                json(200, &first),
                "Link",
                &format!(r#"<{next}>; rel="next", <{next}>; rel="last""#),
            ),
        ),
        (Method::Get, next, json(200, &second)),
    ]);
    let github = GitHub::new(&client);
    let page = block_on(github.repositories("t", &RepositoryQuery::default())).unwrap();
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.items[0].full_name, "ada/engine");
    assert!(page.items[0].private);
    assert_eq!(page.items[0].description, None);
    assert_eq!(
        page.items[1].clone_ssh.as_deref(),
        Some("git@github.com:analytical/notes.git")
    );
    assert_eq!(page.next.as_deref(), Some(next));

    let query = RepositoryQuery {
        cursor: page.next,
        ..RepositoryQuery::default()
    };
    let last = block_on(github.repositories("t", &query)).unwrap();
    assert_eq!(last.items[0].name, "old");
    assert_eq!(last.next, None);
    assert!(client.finished());

    // A cursor for another host is refused before anything is sent.
    let query = RepositoryQuery {
        cursor: Some("https://evil.example/steal".into()),
        ..RepositoryQuery::default()
    };
    assert!(matches!(
        block_on(github.repositories("t", &query)),
        Err(Error::Rejected(_))
    ));
}

#[test]
fn searches_the_account_and_its_organizations() {
    let results = format!(
        r#"{{"total_count":1,"items":[{}]}}"#,
        repo("analytical/engine", false)
    );
    let client = Recorded::new(vec![
        (Method::Get, "https://api.github.com/user", json(200, USER)),
        (
            Method::Get,
            "https://api.github.com/user/orgs?per_page=100",
            json(200, r#"[{"login":"analytical"}]"#),
        ),
        (
            Method::Get,
            "https://api.github.com/search/repositories?per_page=50&sort=updated&q=engine%20in%3Aname%2Cdescription%20fork%3Atrue%20user%3Aada%20org%3Aanalytical",
            json(200, &results),
        ),
    ]);
    let github = GitHub::new(&client);
    let query = RepositoryQuery {
        search: " engine ".into(),
        cursor: None,
    };
    let page = block_on(github.repositories("t", &query)).unwrap();
    assert_eq!(page.items[0].full_name, "analytical/engine");
}

#[test]
fn creates_repositories_for_the_user_or_an_organization() {
    let client = Recorded::new(vec![
        (
            Method::Post,
            "https://api.github.com/user/repos",
            json(201, &repo("ada/fresh", true)),
        ),
        (
            Method::Post,
            "https://api.github.com/orgs/analytical/repos",
            json(
                422,
                r#"{"message":"Repository creation failed.","errors":[{"resource":"Repository","code":"custom","field":"name","message":"name already exists on this account"}]}"#,
            ),
        ),
    ]);
    let github = GitHub::new(&client);
    let mut new = NewRepository {
        name: "fresh".into(),
        description: Some("New".into()),
        private: true,
        owner: None,
    };
    let created = block_on(github.create_repository("t", &new)).unwrap();
    assert_eq!(created.clone_https, "https://github.com/ada/fresh.git");
    let body: serde_json::Value = serde_json::from_slice(&client.requests()[0].body).unwrap();
    assert_eq!(body["name"], "fresh");
    assert_eq!(body["private"], true);

    new.owner = Some("analytical".into());
    assert_eq!(
        block_on(github.create_repository("t", &new)),
        Err(Error::Rejected(
            "Repository creation failed.: name already exists on this account".into()
        ))
    );
}

#[test]
fn maps_errors() {
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.github.com/user",
            json(401, r#"{"message":"Bad credentials"}"#),
        ),
        (
            Method::Get,
            "https://api.github.com/user",
            with_header(
                with_header(
                    json(403, r#"{"message":"API rate limit exceeded"}"#),
                    "x-ratelimit-remaining",
                    "0",
                ),
                "x-ratelimit-reset",
                "1800000000",
            ),
        ),
        (
            Method::Get,
            "https://api.github.com/repos/ada/gone",
            json(404, r#"{"message":"Not Found"}"#),
        ),
        (
            Method::Get,
            "https://api.github.com/user",
            json(502, "<html>bad gateway</html>"),
        ),
        (
            Method::Get,
            "https://api.github.com/user",
            json(200, "{not json"),
        ),
    ]);
    let github = GitHub::new(&client);
    assert_eq!(
        block_on(github.current_user("t")),
        Err(Error::Unauthorized("github.com".into()))
    );
    let limited = block_on(github.current_user("t")).unwrap_err();
    assert_eq!(limited.retry_at(), Some(1_800_000_000));
    assert_eq!(
        block_on(github.repository("t", "ada/gone")),
        Err(Error::NotFound)
    );
    assert_eq!(
        block_on(github.current_user("t")),
        Err(Error::Status {
            host: "github.com".into(),
            status: 502
        })
    );
    assert!(matches!(
        block_on(github.current_user("t")),
        Err(Error::Parse { .. })
    ));

    let offline = GitHub::new(Offline);
    assert!(matches!(
        block_on(offline.current_user("t")),
        Err(Error::Network { .. })
    ));
}

#[test]
fn signs_in_with_the_device_flow() {
    let client = Recorded::new(vec![
        (
            Method::Post,
            "https://github.com/login/device/code",
            json(
                200,
                r#"{"device_code":"3584d83530557fdd1f46af8289938c8ef79f9dc5","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
            ),
        ),
        (
            Method::Post,
            "https://github.com/login/oauth/access_token",
            json(200, r#"{"error":"authorization_pending"}"#),
        ),
        (
            Method::Post,
            "https://github.com/login/oauth/access_token",
            json(200, r#"{"error":"slow_down","interval":10}"#),
        ),
        (
            Method::Post,
            "https://github.com/login/oauth/access_token",
            json(
                200,
                r#"{"access_token":"gho_16C7e42F292c6912E7710c838347Ae178B4a","token_type":"bearer","scope":"repo,read:org"}"#,
            ),
        ),
        (
            Method::Post,
            "https://github.com/login/oauth/access_token",
            json(200, r#"{"error":"access_denied"}"#),
        ),
    ]);
    let github = GitHub::new(&client);
    let code = block_on(github.start_device_flow("Iv1.client")).unwrap();
    assert_eq!(code.user_code, "WDJB-MJHT");
    assert_eq!(code.interval, 5);
    let body = String::from_utf8(client.requests()[0].body.clone()).unwrap();
    assert_eq!(body, "client_id=Iv1.client&scope=repo%20read%3Aorg");

    let poll = || block_on(github.poll_device("Iv1.client", &code.device_code)).unwrap();
    assert_eq!(poll(), DevicePoll::Pending);
    assert_eq!(poll(), DevicePoll::SlowDown(10));
    let token = poll();
    assert!(!format!("{token:?}").contains("gho_"));
    assert_eq!(
        token,
        DevicePoll::Token("gho_16C7e42F292c6912E7710c838347Ae178B4a".into())
    );
    assert_eq!(poll(), DevicePoll::Denied);
}

#[test]
fn enterprise_hosts_use_their_own_api() {
    let client = Recorded::new(vec![(
        Method::Get,
        "https://github.example.com/api/v3/user",
        json(200, USER),
    )]);
    let github = GitHub::enterprise(&client, "GitHub.example.com/");
    let account = block_on(github.current_user("t")).unwrap();
    assert_eq!(account.host, "github.example.com");
    assert_eq!(account.api_url, "https://github.example.com/api/v3");
}
