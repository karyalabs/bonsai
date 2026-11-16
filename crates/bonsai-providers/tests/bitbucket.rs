//! Bitbucket Cloud against responses shaped like the 2.0 API's documented
//! ones.

mod recorded;

use bonsai_providers::{
    Bitbucket, BitbucketCredential, Error, Method, NewRepository, Provider, ProviderKind,
    RepositoryQuery,
};
use futures::executor::block_on;
use recorded::{Recorded, json};

fn repo(full: &str, private: bool) -> String {
    let (workspace, slug) = full.split_once('/').unwrap();
    format!(
        r#"{{"type":"repository","full_name":"{full}","name":"{slug}","slug":"{slug}","description":"","is_private":{private},"parent":null,"mainbranch":{{"name":"main","type":"branch"}},"updated_on":"2026-11-14T08:30:00.000000+00:00","workspace":{{"slug":"{workspace}"}},"links":{{"html":{{"href":"https://bitbucket.org/{full}"}},"clone":[{{"name":"https","href":"https://ada@bitbucket.org/{full}.git"}},{{"name":"ssh","href":"git@bitbucket.org:{full}.git"}}]}}}}"#
    )
}

const USER: &str = r#"{"type":"user","username":"ada","display_name":"Ada Lovelace","account_id":"5f","links":{"avatar":{"href":"https://avatar.example/ada.png"}}}"#;

#[test]
fn reads_the_user_with_either_credential() {
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/user",
            json(200, USER),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/user",
            json(200, USER),
        ),
    ]);
    let bitbucket = Bitbucket::new(&client);
    let oauth = BitbucketCredential::OAuth("oauth-token".into());
    let account = block_on(bitbucket.current_user(&oauth.encode())).unwrap();
    assert_eq!(account.kind, ProviderKind::Bitbucket);
    assert_eq!(
        account.avatar_url.as_deref(),
        Some("https://avatar.example/ada.png")
    );
    assert_eq!(oauth.git_username(&account), "x-token-auth");

    let api = BitbucketCredential::ApiToken {
        email: "ada@example.com".into(),
        token: "ATATT3x".into(),
    };
    block_on(bitbucket.current_user(&api.encode())).unwrap();
    assert_eq!(api.git_username(&account), "ada");
    let requests = client.requests();
    assert_eq!(
        requests[0].header_value("authorization"),
        Some("Bearer oauth-token")
    );
    // base64("ada@example.com:ATATT3x")
    assert_eq!(
        requests[1].header_value("authorization"),
        Some("Basic YWRhQGV4YW1wbGUuY29tOkFUQVRUM3g=")
    );
}

#[test]
fn lists_searches_and_pages_repositories() {
    let next = "https://api.bitbucket.org/2.0/repositories?role=member&page=2";
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/repositories?role=member&sort=-updated_on&pagelen=50&q=name%20~%20%22engine%22%20OR%20description%20~%20%22engine%22",
            json(
                200,
                &format!(
                    r#"{{"pagelen":50,"page":1,"next":"{next}","values":[{}]}}"#,
                    repo("analytical/engine", true)
                ),
            ),
        ),
        (
            Method::Get,
            next,
            json(
                200,
                &format!(
                    r#"{{"pagelen":50,"page":2,"values":[{}]}}"#,
                    repo("ada/notes", false)
                ),
            ),
        ),
    ]);
    let bitbucket = Bitbucket::new(&client);
    let query = RepositoryQuery {
        search: "engine".into(),
        cursor: None,
    };
    let page = block_on(bitbucket.repositories("t", &query)).unwrap();
    let engine = &page.items[0];
    assert_eq!(engine.full_name, "analytical/engine");
    assert_eq!(engine.owner, "analytical");
    assert_eq!(
        engine.clone_https,
        "https://bitbucket.org/analytical/engine.git"
    );
    assert_eq!(
        engine.clone_ssh.as_deref(),
        Some("git@bitbucket.org:analytical/engine.git")
    );
    assert_eq!(engine.default_branch.as_deref(), Some("main"));
    assert!(engine.private);
    let last = block_on(bitbucket.repositories(
        "t",
        &RepositoryQuery {
            cursor: page.next,
            ..RepositoryQuery::default()
        },
    ))
    .unwrap();
    assert_eq!(last.items[0].name, "notes");
    assert_eq!(last.next, None);
}

#[test]
fn creates_repositories_and_lists_workspaces() {
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/user/permissions/workspaces?pagelen=100",
            json(
                200,
                r#"{"values":[{"permission":"owner","workspace":{"slug":"ada"}},{"permission":"collaborator","workspace":{"slug":"analytical"}},{"permission":"member","workspace":{"slug":"readonly"}}]}"#,
            ),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/user",
            json(200, USER),
        ),
        (
            Method::Post,
            "https://api.bitbucket.org/2.0/repositories/ada/my-app",
            json(200, &repo("ada/my-app", true)),
        ),
        (
            Method::Post,
            "https://api.bitbucket.org/2.0/repositories/analytical/my-app",
            json(
                400,
                r#"{"type":"error","error":{"message":"Repository with this Slug and Owner already exists.","fields":{"name":["Repository with this Slug and Owner already exists."]}}}"#,
            ),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/repositories/ada/gone",
            json(
                404,
                r#"{"type":"error","error":{"message":"Repository ada/gone not found"}}"#,
            ),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/user",
            json(401, ""),
        ),
    ]);
    let bitbucket = Bitbucket::new(&client);
    assert_eq!(
        block_on(bitbucket.workspaces("t")).unwrap(),
        ["ada", "analytical"]
    );
    let mut new = NewRepository {
        name: "My App".into(),
        description: None,
        private: true,
        owner: None,
    };
    let created = block_on(bitbucket.create_repository("t", &new)).unwrap();
    assert_eq!(created.full_name, "ada/my-app");
    let body: serde_json::Value = serde_json::from_slice(&client.requests()[2].body).unwrap();
    assert_eq!(body["scm"], "git");
    assert_eq!(body["is_private"], true);

    new.owner = Some("analytical".into());
    assert!(matches!(
        block_on(bitbucket.create_repository("t", &new)),
        Err(Error::Rejected(message)) if message.contains("already exists")
    ));
    assert_eq!(
        block_on(bitbucket.repository("t", "ada/gone")),
        Err(Error::NotFound)
    );
    assert_eq!(
        block_on(bitbucket.current_user("t")),
        Err(Error::Unauthorized("bitbucket.org".into()))
    );
}

#[test]
fn signs_in_with_oauth() {
    let client = Recorded::new(vec![(
        Method::Post,
        "https://bitbucket.org/site/oauth2/access_token",
        json(
            200,
            r#"{"access_token":"bb-access","scopes":"repository account","expires_in":7200,"refresh_token":"bb-refresh","token_type":"bearer"}"#,
        ),
    )]);
    let bitbucket = Bitbucket::new(&client);
    assert_eq!(
        bitbucket.authorize_url("key", "s1"),
        "https://bitbucket.org/site/oauth2/authorize?client_id=key&response_type=code&state=s1"
    );
    let tokens = block_on(bitbucket.exchange_code("key", "secret", "code-1")).unwrap();
    assert_eq!(tokens.refresh_token.as_deref(), Some("bb-refresh"));
    let request = &client.requests()[0];
    // base64("key:secret")
    assert_eq!(
        request.header_value("authorization"),
        Some("Basic a2V5OnNlY3JldA==")
    );
    assert_eq!(
        String::from_utf8(request.body.clone()).unwrap(),
        "grant_type=authorization_code&code=code-1"
    );
}
