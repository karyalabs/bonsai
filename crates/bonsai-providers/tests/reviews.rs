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

#[test]
fn conversations_merges_and_new_issues_on_each_provider() {
    use bonsai_providers::{ItemKind, MergeMethod, NewIssue};
    let issue = NewIssue {
        title: "Crash".into(),
        body: "Steps".into(),
    };
    let github_client = Recorded::new(vec![
        (
            Method::Get,
            "https://api.github.com/repos/ada/engine/issues/7/comments?per_page=100",
            json(
                200,
                r#"[{"user":{"login":"grace","type":"User"},"body":"Looks good","created_at":"2026-11-12T09:00:00Z"},
                    {"user":{"login":"ci-bot","type":"Bot"},"body":"Coverage 90%","created_at":"2026-11-12T09:01:00Z"}]"#,
            ),
        ),
        (
            Method::Put,
            "https://api.github.com/repos/ada/engine/pulls/7/merge",
            json(200, r#"{"merged":true}"#),
        ),
        (
            Method::Post,
            "https://api.github.com/repos/ada/engine/issues",
            json(
                201,
                r#"{"number":10,"title":"Crash","body":"Steps","user":{"login":"ada"},"state":"open","labels":[],"comments":0,
                   "created_at":"2026-11-16T09:00:00Z","updated_at":"2026-11-16T09:00:00Z","html_url":"https://github.com/ada/engine/issues/10"}"#,
            ),
        ),
    ]);
    let github = GitHub::new(&github_client);
    let comments = block_on(github.comments("t", "ada/engine", ItemKind::PullRequest, 7)).unwrap();
    assert_eq!(comments.len(), 1, "bots are left out");
    assert_eq!(comments[0].author.username, "grace");
    block_on(github.merge_pull_request("t", "ada/engine", 7, MergeMethod::Squash)).unwrap();
    let sent: serde_json::Value =
        serde_json::from_slice(&github_client.requests()[1].body).unwrap();
    assert_eq!(sent["merge_method"], "squash");
    let created = block_on(github.create_issue("t", "ada/engine", &issue)).unwrap();
    assert_eq!(created.number, 10);
    assert!(github_client.finished());

    let gitlab_client = Recorded::new(vec![
        (
            Method::Get,
            "https://gitlab.com/api/v4/projects/team%2Fengine/issues/3/notes?sort=asc&order_by=created_at&per_page=100",
            json(
                200,
                r#"[{"body":"changed the description","system":true,"author":{"username":"ada"},"created_at":"2026-11-12T09:00:00Z"},
                    {"body":"Can't reproduce","system":false,"author":{"username":"grace","name":"Grace Hopper"},"created_at":"2026-11-12T10:00:00Z"}]"#,
            ),
        ),
        (
            Method::Put,
            "https://gitlab.com/api/v4/projects/team%2Fengine/merge_requests/4/merge",
            json(200, r#"{"iid":4}"#),
        ),
    ]);
    let gitlab = GitLab::new(&gitlab_client);
    let notes = block_on(gitlab.comments("t", "team/engine", ItemKind::Issue, 3)).unwrap();
    assert_eq!(notes.len(), 1, "system notes are left out");
    assert_eq!(notes[0].author.name(), "Grace Hopper");
    block_on(gitlab.merge_pull_request("t", "team/engine", 4, MergeMethod::Squash)).unwrap();
    let sent: serde_json::Value =
        serde_json::from_slice(&gitlab_client.requests()[1].body).unwrap();
    assert_eq!(sent["squash"], true);
    assert!(gitlab_client.finished());

    let bitbucket_client = Recorded::new(vec![
        (
            Method::Post,
            "https://api.bitbucket.org/2.0/repositories/analytical/engine/pullrequests/12/merge",
            json(200, r#"{"id":12}"#),
        ),
        (
            Method::Get,
            "https://api.bitbucket.org/2.0/repositories/analytical/engine/pullrequests/12/comments?pagelen=100&sort=created_on",
            json(
                200,
                r#"{"values":[{"user":{"display_name":"Ada Lovelace","nickname":"ada"},"content":{"raw":"Merging"},"deleted":false,"created_on":"2026-11-12T09:00:00Z"},
                              {"user":{"display_name":"Grace Hopper"},"content":{"raw":""},"deleted":true,"created_on":"2026-11-12T09:00:00Z"}]}"#,
            ),
        ),
    ]);
    let bitbucket = Bitbucket::new(&bitbucket_client);
    block_on(bitbucket.merge_pull_request("t", "analytical/engine", 12, MergeMethod::Rebase))
        .unwrap();
    let sent: serde_json::Value =
        serde_json::from_slice(&bitbucket_client.requests()[0].body).unwrap();
    assert_eq!(sent["merge_strategy"], "fast_forward");
    let comments =
        block_on(bitbucket.comments("t", "analytical/engine", ItemKind::PullRequest, 12)).unwrap();
    assert_eq!(comments.len(), 1, "deleted comments are left out");
    assert_eq!(comments[0].body, "Merging");
    assert!(bitbucket_client.finished());
}

#[test]
fn comments_closes_and_reopens_on_each_provider() {
    use bonsai_providers::{Error, ItemKind};
    let github_client = Recorded::new(vec![
        (
            Method::Post,
            "https://api.github.com/repos/ada/engine/issues/7/comments",
            json(
                201,
                r#"{"user":{"login":"ada","type":"User"},"body":"Thanks!","created_at":"2026-11-16T09:00:00Z"}"#,
            ),
        ),
        (
            Method::Patch,
            "https://api.github.com/repos/ada/engine/issues/7",
            json(200, r#"{"number":7,"state":"closed"}"#),
        ),
    ]);
    let github = GitHub::new(&github_client);
    let comment =
        block_on(github.add_comment("t", "ada/engine", ItemKind::PullRequest, 7, "Thanks!"))
            .unwrap();
    assert_eq!(comment.author.username, "ada");
    assert_eq!(comment.body, "Thanks!");
    block_on(github.set_open("t", "ada/engine", ItemKind::PullRequest, 7, false)).unwrap();
    let sent: serde_json::Value =
        serde_json::from_slice(&github_client.requests()[1].body).unwrap();
    assert_eq!(sent["state"], "closed");
    assert!(github_client.finished());

    let gitlab_client = Recorded::new(vec![
        (
            Method::Post,
            "https://gitlab.com/api/v4/projects/team%2Fengine/merge_requests/4/notes",
            json(
                201,
                r#"{"body":"On it","system":false,"author":{"username":"ada","name":"Ada Lovelace"},"created_at":"2026-11-16T09:00:00Z"}"#,
            ),
        ),
        (
            Method::Put,
            "https://gitlab.com/api/v4/projects/team%2Fengine/issues/3",
            json(200, r#"{"iid":3,"state":"opened"}"#),
        ),
    ]);
    let gitlab = GitLab::new(&gitlab_client);
    let note = block_on(gitlab.add_comment("t", "team/engine", ItemKind::PullRequest, 4, "On it"))
        .unwrap();
    assert_eq!(note.author.name(), "Ada Lovelace");
    block_on(gitlab.set_open("t", "team/engine", ItemKind::Issue, 3, true)).unwrap();
    let sent: serde_json::Value =
        serde_json::from_slice(&gitlab_client.requests()[1].body).unwrap();
    assert_eq!(sent["state_event"], "reopen");
    assert!(gitlab_client.finished());

    let bitbucket_client = Recorded::new(vec![
        (
            Method::Post,
            "https://api.bitbucket.org/2.0/repositories/analytical/engine/pullrequests/12/comments",
            json(
                201,
                r#"{"user":{"display_name":"Ada Lovelace","nickname":"ada"},"content":{"raw":"Done"},"created_on":"2026-11-16T09:00:00Z"}"#,
            ),
        ),
        (
            Method::Post,
            "https://api.bitbucket.org/2.0/repositories/analytical/engine/pullrequests/12/decline",
            json(200, r#"{"id":12,"state":"DECLINED"}"#),
        ),
    ]);
    let bitbucket = Bitbucket::new(&bitbucket_client);
    let comment = block_on(bitbucket.add_comment(
        "t",
        "analytical/engine",
        ItemKind::PullRequest,
        12,
        "Done",
    ))
    .unwrap();
    assert_eq!(comment.body, "Done");
    let sent: serde_json::Value =
        serde_json::from_slice(&bitbucket_client.requests()[0].body).unwrap();
    assert_eq!(sent["content"]["raw"], "Done");
    block_on(bitbucket.set_open("t", "analytical/engine", ItemKind::PullRequest, 12, false))
        .unwrap();
    let reopen =
        block_on(bitbucket.set_open("t", "analytical/engine", ItemKind::PullRequest, 12, true));
    assert!(matches!(reopen, Err(Error::Rejected(_))));
    assert!(bitbucket_client.finished());
}

#[test]
fn reads_ci_checks_on_each_provider() {
    use bonsai_providers::CheckState;
    let github_client = Recorded::new(vec![(
        Method::Get,
        "https://api.github.com/repos/ada/engine/commits/abc123/check-runs?per_page=100",
        json(
            200,
            r#"{"total_count":3,"check_runs":[
              {"name":"core","status":"completed","conclusion":"success","html_url":"https://github.com/ada/engine/runs/1"},
              {"name":"macos","status":"in_progress","conclusion":null,"html_url":null},
              {"name":"lint","status":"completed","conclusion":"timed_out","html_url":null}]}"#,
        ),
    )]);
    let checks = block_on(GitHub::new(&github_client).checks("t", "ada/engine", "abc123")).unwrap();
    let states: Vec<_> = checks.iter().map(|c| c.state).collect();
    assert_eq!(
        states,
        [
            CheckState::Success,
            CheckState::Pending,
            CheckState::Failure
        ]
    );
    assert_eq!(
        checks[0].url.as_deref(),
        Some("https://github.com/ada/engine/runs/1")
    );

    let gitlab_client = Recorded::new(vec![(
        Method::Get,
        "https://gitlab.com/api/v4/projects/team%2Fengine/repository/commits/abc123/statuses?per_page=100",
        json(
            200,
            r#"[{"name":"test","status":"failed","target_url":null},{"name":"deploy","status":"manual","target_url":null}]"#,
        ),
    )]);
    let checks =
        block_on(GitLab::new(&gitlab_client).checks("t", "team/engine", "abc123")).unwrap();
    assert_eq!(checks[0].state, CheckState::Failure);
    assert_eq!(checks[1].state, CheckState::Skipped);

    let bitbucket_client = Recorded::new(vec![(
        Method::Get,
        "https://api.bitbucket.org/2.0/repositories/analytical/engine/commit/abc123/statuses?pagelen=100",
        json(
            200,
            r#"{"values":[{"key":"build-1","name":"Pipeline #4","state":"SUCCESSFUL","url":"https://bitbucket.org/x"},{"key":"lint","state":"INPROGRESS"}]}"#,
        ),
    )]);
    let checks =
        block_on(Bitbucket::new(&bitbucket_client).checks("t", "analytical/engine", "abc123"))
            .unwrap();
    assert_eq!(checks[0].name, "Pipeline #4");
    assert_eq!(checks[1].name, "lint");
    assert_eq!(checks[1].state, CheckState::Pending);
}

#[test]
fn lists_changed_files_on_each_provider() {
    use bonsai_providers::FileChange;
    let github_client = Recorded::new(vec![(
        Method::Get,
        "https://api.github.com/repos/ada/engine/pulls/7/files?per_page=100",
        json(
            200,
            r#"[{"filename":"src/sync.rs","status":"modified","additions":3,"deletions":1,"patch":"@@ -1 +1,3 @@\n-a\n+b\n+c\n+d"},
                {"filename":"docs/new.md","previous_filename":"docs/old.md","status":"renamed","additions":0,"deletions":0},
                {"filename":"logo.png","status":"added","additions":0,"deletions":0}]"#,
        ),
    )]);
    let files = block_on(GitHub::new(&github_client).changed_files("t", "ada/engine", 7)).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].change, FileChange::Modified);
    assert!(files[0].patch.as_deref().unwrap().starts_with("@@"));
    assert_eq!(files[1].old_path.as_deref(), Some("docs/old.md"));
    assert_eq!(files[2].change, FileChange::Added);
    assert_eq!(files[2].patch, None);

    let gitlab_client = Recorded::new(vec![(
        Method::Get,
        "https://gitlab.com/api/v4/projects/team%2Fengine/merge_requests/4/diffs?per_page=100",
        json(
            200,
            r#"[{"old_path":"a.rs","new_path":"a.rs","new_file":false,"renamed_file":false,"deleted_file":false,"diff":"@@ -1 +1,2 @@\n-x\n+y\n+z\n"},
                {"old_path":"gone.rs","new_path":"gone.rs","new_file":false,"renamed_file":false,"deleted_file":true,"diff":""}]"#,
        ),
    )]);
    let files = block_on(GitLab::new(&gitlab_client).changed_files("t", "team/engine", 4)).unwrap();
    assert_eq!((files[0].additions, files[0].deletions), (2, 1));
    assert_eq!(files[1].change, FileChange::Removed);
    assert_eq!(files[1].patch, None);

    let bitbucket_client = Recorded::new(vec![(
        Method::Get,
        "https://api.bitbucket.org/2.0/repositories/analytical/engine/pullrequests/12/diffstat?pagelen=100",
        json(
            200,
            r#"{"values":[{"status":"renamed","lines_added":1,"lines_removed":0,"old":{"path":"a.md"},"new":{"path":"b.md"}},
                          {"status":"removed","lines_added":0,"lines_removed":9,"old":{"path":"c.md"},"new":null}]}"#,
        ),
    )]);
    let files =
        block_on(Bitbucket::new(&bitbucket_client).changed_files("t", "analytical/engine", 12))
            .unwrap();
    assert_eq!(files[0].path, "b.md");
    assert_eq!(files[0].old_path.as_deref(), Some("a.md"));
    assert_eq!(files[1].path, "c.md");
    assert_eq!(files[1].deletions, 9);
}
