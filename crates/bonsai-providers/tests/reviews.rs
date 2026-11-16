//! Pull requests and issues on each provider, against responses shaped like
//! their documented ones.

mod recorded;

use bonsai_providers::{
    Bitbucket, GitHub, GitLab, ItemQuery, ItemState, Method, NewPullRequest, Provider, StateFilter,
};
use futures::executor::block_on;
use recorded::{Recorded, json, with_header};

const GITHUB_PULLS: &str = r#"[
  {"number":7,"title":"Add the sync bar","body":"Fetches in the background.","user":{"login":"grace","avatar_url":"https://avatars.example/grace"},
   "state":"open","draft":false,"merged_at":null,"head":{"ref":"feature/sync","repo":{"full_name":"ada/engine"}},
   "base":{"ref":"main","repo":{"full_name":"ada/engine"}},"labels":[{"name":"ui","color":"a2eeef"}],
   "created_at":"2026-11-10T09:00:00Z","updated_at":"2026-11-12T09:00:00Z","html_url":"https://github.com/ada/engine/pull/7"},
  {"number":6,"title":"Try a new layout","body":null,"user":{"login":"linus","avatar_url":null},
   "state":"open","draft":true,"merged_at":null,"head":{"ref":"layout","repo":{"full_name":"linus/engine"}},
   "base":{"ref":"main","repo":{"full_name":"ada/engine"}},"labels":[],
   "created_at":"2026-11-09T09:00:00Z","updated_at":"2026-11-11T09:00:00Z","html_url":"https://github.com/ada/engine/pull/6"}
]"#;

#[test]
fn github_lists_pull_requests_and_issues() {
    let next = "https://api.github.com/repositories/7/pulls?state=open&page=2";
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.github.com/repos/ada/engine/pulls?state=open&sort=updated&direction=desc&per_page=50",
            with_header(
                json(200, GITHUB_PULLS),
                "Link",
                &format!(r#"<{next}>; rel="next""#),
            ),
        ),
        (
            Method::Get,
            "https://api.github.com/repos/ada/engine/issues?state=closed&sort=updated&direction=desc&per_page=50",
            json(
                200,
                r#"[
  {"number":9,"title":"Crash on empty repo","body":"Steps…","user":{"login":"margaret"},"state":"closed",
   "labels":[{"name":"bug","color":"d73a4a"}],"comments":3,"created_at":"2026-11-01T09:00:00Z","updated_at":"2026-11-02T09:00:00Z",
   "html_url":"https://github.com/ada/engine/issues/9"},
  {"number":7,"title":"Add the sync bar","user":{"login":"grace"},"state":"closed","labels":[],"comments":0,
   "created_at":"2026-11-10T09:00:00Z","updated_at":"2026-11-12T09:00:00Z","html_url":"https://github.com/ada/engine/pull/7",
   "pull_request":{"url":"https://api.github.com/repos/ada/engine/pulls/7"}}
]"#,
            ),
        ),
        (
            Method::Post,
            "https://api.github.com/repos/ada/engine/pulls",
            json(
                201,
                r#"{"number":8,"title":"Polish","body":"","user":{"login":"ada"},"state":"open","draft":true,"merged_at":null,
   "head":{"ref":"polish","repo":{"full_name":"ada/engine"}},"base":{"ref":"main","repo":{"full_name":"ada/engine"}},
   "created_at":"2026-11-16T09:00:00Z","updated_at":"2026-11-16T09:00:00Z","html_url":"https://github.com/ada/engine/pull/8"}"#,
            ),
        ),
    ]);
    let github = GitHub::new(&client);
    let page = block_on(github.pull_requests("t", "ada/engine", &ItemQuery::default())).unwrap();
    assert_eq!(page.next.as_deref(), Some(next));
    let [sync, layout] = page.items.as_slice() else {
        panic!("two pull requests")
    };
    assert_eq!((sync.number, sync.state), (7, ItemState::Open));
    assert_eq!(
        (sync.head.as_str(), sync.base.as_str()),
        ("feature/sync", "main")
    );
    assert_eq!(sync.head_fork, None);
    assert_eq!(sync.labels[0].color.as_deref(), Some("a2eeef"));
    assert_eq!(layout.state, ItemState::Draft);
    assert_eq!(layout.head_fork.as_deref(), Some("linus/engine"));
    assert_eq!(layout.body, "");

    let issues = block_on(github.issues(
        "t",
        "ada/engine",
        &ItemQuery {
            state: StateFilter::Closed,
            cursor: None,
        },
    ))
    .unwrap();
    // The pull request GitHub lists as an issue is left out.
    assert_eq!(issues.items.len(), 1);
    assert_eq!(issues.items[0].state, ItemState::Closed);
    assert_eq!(issues.items[0].comments, Some(3));

    let created = block_on(github.create_pull_request(
        "t",
        "ada/engine",
        &NewPullRequest {
            title: "Polish".into(),
            body: String::new(),
            head: "polish".into(),
            base: "main".into(),
            draft: true,
        },
    ))
    .unwrap();
    assert_eq!((created.number, created.state), (8, ItemState::Draft));
    let sent: serde_json::Value = serde_json::from_slice(&client.requests()[2].body).unwrap();
    assert_eq!(sent["head"], "polish");
    assert_eq!(sent["draft"], true);
    assert_eq!(github.pull_request_ref(7).as_deref(), Some("pull/7/head"));
    assert!(client.finished());
}

#[test]
fn gitlab_reads_merge_requests_with_closed_and_merged_together() {
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://gitlab.com/api/v4/projects/team%2Fsub%2Fengine/merge_requests?state=all&order_by=updated_at&sort=desc&per_page=50",
            json(
                200,
                r#"[
  {"iid":3,"title":"Merged one","description":"Done","author":{"username":"ada","name":"Ada Lovelace","avatar_url":null},
   "state":"merged","draft":false,"source_branch":"a","target_branch":"main","source_project_id":1,"target_project_id":1,
   "labels":["backend"],"user_notes_count":2,"created_at":"2026-11-01T09:00:00Z","updated_at":"2026-11-03T09:00:00Z",
   "web_url":"https://gitlab.com/team/sub/engine/-/merge_requests/3"},
  {"iid":4,"title":"Still open","description":null,"author":{"username":"grace","name":"","avatar_url":null},
   "state":"opened","draft":true,"source_branch":"b","target_branch":"main","source_project_id":9,"target_project_id":1,
   "labels":[],"user_notes_count":0,"created_at":"2026-11-01T09:00:00Z","updated_at":"2026-11-02T09:00:00Z",
   "web_url":"https://gitlab.com/team/sub/engine/-/merge_requests/4"}
]"#,
            ),
        ),
        (
            Method::Post,
            "https://gitlab.com/api/v4/projects/team%2Fsub%2Fengine/merge_requests",
            json(
                201,
                r#"{"iid":5,"title":"Draft: Polish","description":"","author":{"username":"ada"},"state":"opened","draft":true,
   "source_branch":"polish","target_branch":"main","created_at":"2026-11-16T09:00:00Z","updated_at":"2026-11-16T09:00:00Z",
   "web_url":"https://gitlab.com/team/sub/engine/-/merge_requests/5"}"#,
            ),
        ),
    ]);
    let gitlab = GitLab::new(&client);
    let closed = block_on(gitlab.pull_requests(
        "t",
        "team/sub/engine",
        &ItemQuery {
            state: StateFilter::Closed,
            cursor: None,
        },
    ))
    .unwrap();
    assert_eq!(closed.items.len(), 1);
    assert_eq!(closed.items[0].state, ItemState::Merged);
    assert_eq!(closed.items[0].labels[0].name, "backend");
    assert_eq!(closed.items[0].author.name(), "Ada Lovelace");

    let created = block_on(gitlab.create_pull_request(
        "t",
        "team/sub/engine",
        &NewPullRequest {
            title: "Polish".into(),
            body: String::new(),
            head: "polish".into(),
            base: "main".into(),
            draft: true,
        },
    ))
    .unwrap();
    assert_eq!(created.state, ItemState::Draft);
    let sent: serde_json::Value = serde_json::from_slice(&client.requests()[1].body).unwrap();
    assert_eq!(sent["title"], "Draft: Polish");
    assert_eq!(sent["source_branch"], "polish");
    assert!(client.finished());
}

#[test]
fn bitbucket_reads_pull_requests_and_issue_states() {
    let client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/repositories/analytical/engine/pullrequests?pagelen=50&sort=-updated_on&state=MERGED&state=DECLINED&state=SUPERSEDED",
            json(
                200,
                r#"{"values":[{"id":12,"title":"Old idea","description":"","author":{"display_name":"Ada Lovelace","nickname":"ada",
   "links":{"avatar":{"href":"https://avatar.example/ada.png"}}},"state":"DECLINED","source":{"branch":{"name":"idea"},
   "repository":{"full_name":"analytical/engine"}},"destination":{"branch":{"name":"main"}},"comment_count":1,
   "created_on":"2026-11-01T09:00:00.000000+00:00","updated_on":"2026-11-02T09:00:00.000000+00:00",
   "links":{"html":{"href":"https://bitbucket.org/analytical/engine/pull-requests/12"}}}],"next":null}"#,
            ),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/repositories/analytical/engine/issues?pagelen=50&sort=-updated_on&q=state%20%3D%20%22new%22%20OR%20state%20%3D%20%22open%22%20OR%20state%20%3D%20%22on%20hold%22",
            json(
                200,
                r#"{"values":[{"id":2,"title":"Docs","content":{"raw":"Write them"},"reporter":{"display_name":"Grace Hopper"},
   "state":"on hold","created_on":"2026-11-01T09:00:00Z","updated_on":"2026-11-01T09:00:00Z",
   "links":{"html":{"href":"https://bitbucket.org/analytical/engine/issues/2"}}}]}"#,
            ),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/repositories/analytical/quiet/issues?pagelen=50&sort=-updated_on",
            json(
                404,
                r#"{"type":"error","error":{"message":"Repository has no issue tracker."}}"#,
            ),
        ),
    ]);
    let bitbucket = Bitbucket::new(&client);
    let closed = block_on(bitbucket.pull_requests(
        "t",
        "analytical/engine",
        &ItemQuery {
            state: StateFilter::Closed,
            cursor: None,
        },
    ))
    .unwrap();
    let pull = &closed.items[0];
    assert_eq!((pull.number, pull.state), (12, ItemState::Closed));
    assert_eq!(pull.author.username, "ada");
    assert_eq!(pull.comments, Some(1));
    assert_eq!(pull.head_fork, None);

    let issues =
        block_on(bitbucket.issues("t", "analytical/engine", &ItemQuery::default())).unwrap();
    assert_eq!(issues.items[0].state, ItemState::Open);
    assert_eq!(issues.items[0].author.name(), "Grace Hopper");
    assert_eq!(issues.items[0].body, "Write them");

    let none = block_on(bitbucket.issues(
        "t",
        "analytical/quiet",
        &ItemQuery {
            state: StateFilter::All,
            cursor: None,
        },
    ));
    assert_eq!(none.unwrap_err(), bonsai_providers::Error::NotFound);
    assert_eq!(bitbucket.pull_request_ref(12), None);
    assert!(client.finished());
}
