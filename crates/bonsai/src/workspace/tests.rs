use bonsai_git::{Git, Repository};
use gpui_kit::{TestAppContext, VisualTestContext};

use super::{Section, Workspace};
use crate::{commands, session::Session};

fn repository(parent: &std::path::Path, name: &str) -> std::path::PathBuf {
    let path = parent.join(name);
    Repository::init(&Git::locate(None).unwrap(), &path, Some("main")).unwrap();
    path
}

fn setup(
    cx: &mut TestAppContext,
    session: Session,
    session_path: Option<std::path::PathBuf>,
) -> (gpui_kit::Entity<Workspace>, &mut VisualTestContext) {
    cx.update(|cx| {
        crate::ui::theme::init(cx);
        commands::install(cx);
    });
    let (workspace, cx) =
        cx.add_window_view(|window, cx| Workspace::new(session, session_path, false, window, cx));
    cx.run_until_parked();
    (workspace, cx)
}

#[gpui_kit::test]
fn opens_tabs_once_and_saves_them(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let alpha = repository(dir.path(), "alpha");
    let beta = repository(dir.path(), "beta");
    let session_path = dir.path().join("session.json");
    let (workspace, cx) = setup(cx, Session::default(), Some(session_path.clone()));

    workspace.update(cx, |workspace, cx| {
        workspace.open_path(&alpha, true, cx).unwrap();
        workspace.open_path(&beta.join("."), true, cx).unwrap();
        workspace.open_path(&alpha, true, cx).unwrap();
        workspace.sync_session_tabs(cx);
    });
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.tab_titles(), ["alpha", "beta"]);
        assert_eq!(
            workspace.active_index(),
            0,
            "reopening switches to the existing tab"
        );
    });
    let saved = Session::load(&session_path);
    assert_eq!(saved.tabs.len(), 2);
    assert_eq!(saved.active_tab, 0);
}

#[gpui_kit::test]
fn restores_tabs_and_skips_missing_ones(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let alpha = repository(dir.path(), "alpha");
    let beta = repository(dir.path(), "beta");
    let session = Session {
        tabs: vec![alpha, dir.path().join("gone"), beta],
        active_tab: 1,
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.tab_titles(), ["alpha", "beta"]);
        assert_eq!(workspace.active_index(), 1);
    });
}

#[gpui_kit::test]
fn keyboard_commands_switch_tabs_sections_and_close(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let session = Session {
        tabs: vec![
            repository(dir.path(), "a"),
            repository(dir.path(), "b"),
            repository(dir.path(), "c"),
        ],
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);

    cx.dispatch_action(commands::NextTab);
    cx.dispatch_action(commands::NextTab);
    workspace.read_with(cx, |w, _| assert_eq!(w.active_index(), 2));
    cx.dispatch_action(commands::NextTab);
    workspace.read_with(cx, |w, _| assert_eq!(w.active_index(), 0, "wraps around"));
    cx.dispatch_action(commands::PreviousTab);
    workspace.read_with(cx, |w, _| assert_eq!(w.active_index(), 2));

    cx.dispatch_action(commands::ShowHistory);
    workspace.read_with(cx, |w, _| assert_eq!(w.section(), Section::History));

    cx.dispatch_action(commands::ToggleSidebar);
    workspace.read_with(cx, |w, _| assert!(!w.session().sidebar_open));

    cx.dispatch_action(commands::CloseTab);
    workspace.read_with(cx, |w, _| {
        assert_eq!(w.tab_titles(), ["a", "b"]);
        assert_eq!(w.active_index(), 1);
    });
}

#[gpui_kit::test]
fn the_command_palette_opens(cx: &mut TestAppContext) {
    let (workspace, cx) = setup(cx, Session::default(), None);
    cx.dispatch_action(commands::ShowCommandPalette);
    cx.run_until_parked();
    workspace.read_with(cx, |w, _| assert!(w.palette.is_open()));
}

#[test]
fn every_palette_command_is_handled_by_the_workspace() {
    // Quit is handled by the app; everything else must have a listener in
    // `Workspace::render`. Keep this list in step with the `on_action` calls.
    let handled = [
        "file.open",
        "file.new",
        "app.accounts",
        "app.settings",
        "repository.terminal",
        "file.clone",
        "file.new-tab",
        "file.close-tab",
        "window.next-tab",
        "window.previous-tab",
        "view.command-palette",
        "view.toggle-sidebar",
        "view.changes",
        "view.history",
        "view.activity",
        "view.pull-requests",
        "view.issues",
        "repository.refresh",
        "repository.commit",
        "repository.commit-push",
        "repository.fetch",
        "repository.pull",
        "repository.push",
        "repository.purge",
        "repository.publish",
        "repository.create-pull-request",
        "repository.new-issue",
        "repository.stash",
        "repository.switch-branch",
        "repository.new-branch",
    ];
    for command in commands::COMMANDS {
        assert!(
            command.id == "app.quit" || handled.contains(&command.id),
            "{} has no handler",
            command.id
        );
    }
}

#[gpui_kit::test]
fn opening_remembers_recents_and_new_tab_shows_welcome(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let alpha = repository(dir.path(), "alpha");
    let beta = repository(dir.path(), "beta");
    let session_path = dir.path().join("session.json");
    let (workspace, cx) = setup(cx, Session::default(), Some(session_path.clone()));

    workspace.update(cx, |workspace, cx| {
        workspace.open_paths(&[alpha.clone(), beta.clone()], cx);
    });
    let saved = Session::load(&session_path);
    assert_eq!(saved.recents.len(), 2);
    assert!(saved.recents[0].ends_with("beta"));

    cx.dispatch_action(commands::ShowWelcome);
    workspace.read_with(cx, |workspace, _| assert!(workspace.is_showing_welcome()));
    workspace.update(cx, |workspace, cx| workspace.open_recent(&alpha, cx));
    workspace.read_with(cx, |workspace, _| {
        assert!(!workspace.is_showing_welcome());
        assert_eq!(workspace.active_index(), 0);
    });
    assert!(Session::load(&session_path).recents[0].ends_with("alpha"));

    workspace.update(cx, |workspace, cx| {
        workspace.forget_recent(&saved.recents[0], cx);
    });
    assert_eq!(Session::load(&session_path).recents.len(), 1);
}

#[gpui_kit::test]
fn clones_from_a_url_and_opens_the_result(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let source = repository(dir.path(), "source");
    let git = Git::locate(None).unwrap();
    std::fs::write(source.join("README.md"), "hello\n").unwrap();
    for args in [
        vec!["add", "."],
        vec![
            "-c",
            "user.name=B",
            "-c",
            "user.email=b@example.com",
            "commit",
            "-qm",
            "first",
        ],
    ] {
        git.run(&git.command(args).cwd(&source)).unwrap();
    }
    let parent = dir.path().join("clones");
    std::fs::create_dir(&parent).unwrap();
    let session = Session {
        clone_parent: Some(parent.clone()),
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);

    cx.dispatch_action(commands::CloneRepository);
    let dialog = workspace.read_with(cx, |workspace, _| {
        workspace.clone_dialog().cloned().unwrap()
    });
    let url = format!("file://{}", source.display());
    cx.update(|window, cx| dialog.update(cx, |dialog, cx| dialog.set_url(&url, window, cx)));
    let destination = dialog
        .read_with(cx, super::clone::CloneDialog::destination)
        .unwrap();
    assert_eq!(destination, parent.join("source"));

    dialog.update(cx, super::clone::CloneDialog::start);
    for _ in 0..200 {
        cx.run_until_parked();
        if workspace.read_with(cx, |workspace, _| workspace.tab_titles().len()) == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    workspace.read_with(cx, |workspace, _| {
        assert_eq!(workspace.tab_titles(), ["source"]);
        assert!(workspace.clone_dialog().is_none());
    });
    assert!(parent.join("source/README.md").exists());
}

#[gpui_kit::test]
fn a_failed_operation_offers_details_in_the_activity_log(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let session = Session {
        tabs: vec![repository(dir.path(), "a")],
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);
    let model = workspace.read_with(cx, |w, _| w.active_model().unwrap().clone());
    model.update(cx, |model, cx| {
        model.run(
            "Delete a missing branch",
            |repository| repository.delete_branch("missing", false),
            cx,
        );
    });
    cx.run_until_parked();
    workspace.read_with(cx, |w, cx| {
        assert!(w.message.is_some());
        assert_eq!(w.message_details, w.message);
        // The error shows as a toast, not a banner.
        assert_eq!(w.toasted, w.message);
        assert!(w.toasts.read(cx).contains("workspace.message"));
    });

    workspace.update(cx, Workspace::show_message_details);
    workspace.read_with(cx, |w, cx| {
        assert_eq!(w.section(), Section::Activity);
        let activity = w.tabs[w.active].activity.read(cx);
        assert!(activity.selected().is_some());
    });
    cx.dispatch_action(commands::ShowChanges);
    cx.dispatch_action(commands::ShowActivity);
    workspace.read_with(cx, |w, _| assert_eq!(w.section(), Section::Activity));
}

#[gpui_kit::test]
fn switch_branch_lists_the_branches_and_switches(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = repository(dir.path(), "a");
    let git = Git::locate(None).unwrap();
    let run = |args: &[&str]| {
        let mut full = vec![
            "-c",
            "user.name=Ada",
            "-c",
            "user.email=ada@example.com",
            "-c",
            "commit.gpgsign=false",
        ];
        full.extend_from_slice(args);
        git.run(&git.command(full).cwd(&path)).unwrap();
    };
    run(&["commit", "-q", "--allow-empty", "-m", "base"]);
    run(&["branch", "feature"]);
    let session = Session {
        tabs: vec![path.clone()],
        ..Session::default()
    };
    let (workspace, cx) = setup(cx, session, None);
    cx.dispatch_action(commands::SwitchBranch);
    cx.run_until_parked();
    let state = workspace.read_with(cx, |w, _| {
        assert!(w.branch_handle.is_open());
        w.branch_picker.as_ref().unwrap().0.clone()
    });
    state.update_in(cx, |state, window, cx| state.set_query("feat", window, cx));
    cx.run_until_parked();
    let value = state.read_with(cx, |state, _| state.highlighted().unwrap().value.clone());
    assert_eq!(value.as_ref(), "local:feature");
    state.update(cx, |_, cx| {
        cx.emit(crate::ui::command::CommandEvent::Confirm(value));
    });
    cx.run_until_parked();
    workspace.read_with(cx, |w, cx| {
        assert!(w.branch_picker.is_none());
        let snapshot = w.active_model().unwrap().read(cx).snapshot().unwrap();
        assert_eq!(
            super::BranchSummary::from_snapshot(snapshot).label.as_ref(),
            "feature"
        );
    });
}

#[test]
fn errors_split_into_a_title_and_details() {
    let (title, rest) = super::split_message("Push failed\nremote rejected");
    assert_eq!(title, "Push failed");
    assert_eq!(rest, "remote rejected");
    assert_eq!(
        super::success_title("Push main to origin").as_deref(),
        Some("Push main to origin")
    );
    assert_eq!(super::success_title("Stage a.rs"), None);
}

mod hosted {
    use std::sync::Arc;

    use bonsai_git::{Git, Head, Repository};
    use bonsai_keychain::{MemoryStore, Secret, TokenStore};
    use bonsai_providers::{
        Account, BoxFuture, HttpClient, ItemState, ProviderKind, Request, Response,
    };
    use gpui_kit::TestAppContext;

    use super::setup;
    use crate::{
        accounts::{Accounts, Connected, SignInMethod, accounts_path},
        commands,
        create_pull_sheet::Phase as CreatePhase,
        hosted::{HostedEnv, HostedView},
        session::Session,
    };

    /// Answers each request with the first body whose key the URL contains.
    struct Canned(Vec<(&'static str, String)>);

    impl HttpClient for Canned {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
            let body = self
                .0
                .iter()
                .find(|(key, _)| request.url.contains(key))
                .map(|(_, body)| body.clone());
            Box::pin(async move {
                body.map(|body| Response {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into_bytes(),
                })
                .ok_or_else(|| format!("unexpected request: {}", request.url))
            })
        }
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Ada Lovelace",
                "-c",
                "user.email=ada@example.com",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn pull(number: u64, head: &str, fork: &str, draft: bool) -> String {
        format!(
            r#"{{"number":{number},"title":"Pull {number}","body":"About **{head}**.","user":{{"login":"grace"}},"state":"open","draft":{draft},"merged_at":null,"head":{{"ref":"{head}","sha":"abc{number}","repo":{{"full_name":"{fork}"}}}},"base":{{"ref":"main","repo":{{"full_name":"ada/engine"}}}},"labels":[{{"name":"ui","color":"a2eeef"}}],"created_at":"2026-11-10T09:00:00Z","updated_at":"2026-11-12T09:00:00Z","html_url":"https://github.com/ada/engine/pull/{number}"}}"#
        )
    }

    #[gpui_kit::test]
    fn lists_pull_requests_and_checks_them_out(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // The hosted repository, with a branch and a fork's pull ref.
        let upstream = root.join("upstream");
        Repository::init(&Git::locate(None).unwrap(), &upstream, Some("main")).unwrap();
        git(&upstream, &["commit", "-q", "--allow-empty", "-m", "Start"]);
        git(&upstream, &["switch", "-q", "-c", "feature/sync"]);
        git(&upstream, &["commit", "-q", "--allow-empty", "-m", "Sync"]);
        git(&upstream, &["switch", "-q", "main"]);
        git(
            &upstream,
            &["commit", "-q", "--allow-empty", "-m", "From a fork"],
        );
        let fork = git(&upstream, &["rev-parse", "HEAD"]);
        git(&upstream, &["update-ref", "refs/pull/6/head", &fork]);
        git(&upstream, &["reset", "-q", "--hard", "HEAD~1"]);

        let local = root.join("engine");
        git(
            &root,
            &["clone", "-q", upstream.to_str().unwrap(), "engine"],
        );
        let hosted = "https://github.com/ada/engine.git";
        git(&local, &["remote", "set-url", "origin", hosted]);
        git(
            &local,
            &[
                "config",
                &format!("url.{}.insteadOf", upstream.display()),
                hosted,
            ],
        );

        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        let mut accounts = Accounts::load(accounts_path(&root), store.clone());
        accounts
            .add(
                Connected {
                    account: Account {
                        kind: ProviderKind::GitHub,
                        host: "github.com".into(),
                        api_url: "https://api.github.com".into(),
                        username: "ada".into(),
                        display_name: None,
                        avatar_url: None,
                    },
                    method: SignInMethod::Token,
                },
                &Secret::new("t"),
            )
            .unwrap();
        let client = Canned(vec![
            (
                "/repos/ada/engine/pulls",
                format!(
                    "[{},{}]",
                    pull(7, "feature/sync", "ada/engine", false),
                    pull(6, "layout", "grace/engine", true)
                ),
            ),
            (
                "/repos/ada/engine/issues",
                r#"[{"number":9,"title":"Crash on empty repo","body":"","user":{"login":"margaret"},"state":"open","labels":[],"comments":3,"created_at":"2026-11-01T09:00:00Z","updated_at":"2026-11-02T09:00:00Z","html_url":"https://github.com/ada/engine/issues/9"}]"#.to_owned(),
            ),
        ]);

        let (workspace, cx) = setup(cx, Session::default(), None);
        workspace.update(cx, |workspace, cx| {
            workspace.set_hosted_env(HostedEnv {
                accounts_path: accounts_path(&root),
                store,
                client: Arc::new(client),
            });
            workspace.open_path(&local, true, cx).unwrap();
        });
        cx.run_until_parked();
        cx.dispatch_action(commands::ShowPullRequests);
        cx.run_until_parked();
        let view = workspace.read_with(cx, |w, _| w.hosted_view()).unwrap();
        view.read_with(cx, |view, _| {
            assert_eq!(view.error(), None);
            let numbers: Vec<u64> = view.items().iter().map(|i| i.number).collect();
            assert_eq!(numbers, [7, 6]);
            assert_eq!(view.items()[1].state, ItemState::Draft);
            assert_eq!(view.selected().map(|i| i.number), Some(7));
        });

        // A pull request from the same repository: its own tracking branch.
        view.update(cx, HostedView::check_out);
        cx.run_until_parked();
        let head = || {
            let repository = Repository::open(&Git::locate(None).unwrap(), &local).unwrap();
            match repository.snapshot().unwrap().head {
                Head::Branch { name, .. } => name,
                Head::Detached { .. } => "detached".into(),
            }
        };
        assert_eq!(head(), "feature/sync");
        assert_eq!(
            git(
                &local,
                &["rev-parse", "--abbrev-ref", "feature/sync@{upstream}"]
            ),
            "origin/feature/sync"
        );

        // One from a fork: fetched by its pull ref into pr-6.
        view.update(cx, |view, cx| view.select(6, cx));
        view.update(cx, HostedView::check_out);
        cx.run_until_parked();
        assert_eq!(head(), "pr-6");
        assert_eq!(git(&local, &["rev-parse", "HEAD"]), fork);

        cx.dispatch_action(commands::ShowIssues);
        cx.run_until_parked();
        let issues = workspace.read_with(cx, |w, _| w.hosted_view()).unwrap();
        issues.read_with(cx, |view, _| {
            assert_eq!(view.items().len(), 1);
            assert_eq!(view.items()[0].comments, Some(3));
            assert!(view.items()[0].pull.is_none());
        });
    }

    /// GitHub that lists no pull requests and records the one created.
    struct Creates(std::sync::Mutex<Vec<serde_json::Value>>);

    impl HttpClient for Creates {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
            let body = if request.method == bonsai_providers::Method::Post {
                let sent: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                self.0.lock().unwrap().push(sent.clone());
                format!(
                    r#"{{"number":8,"title":{},"body":"","user":{{"login":"ada"}},"state":"open","draft":{},"merged_at":null,"head":{{"ref":{},"repo":{{"full_name":"ada/engine"}}}},"base":{{"ref":{},"repo":{{"full_name":"ada/engine"}}}},"created_at":"2026-11-16T09:00:00Z","updated_at":"2026-11-16T09:00:00Z","html_url":"https://github.com/ada/engine/pull/8"}}"#,
                    sent["title"], sent["draft"], sent["head"], sent["base"]
                )
            } else {
                "[]".to_owned()
            };
            Box::pin(async move {
                Ok(Response {
                    status: if request.method == bonsai_providers::Method::Post {
                        201
                    } else {
                        200
                    },
                    headers: Vec::new(),
                    body: body.into_bytes(),
                })
            })
        }
    }

    #[gpui_kit::test]
    fn pushes_the_branch_and_opens_a_pull_request(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let upstream = root.join("upstream.git");
        std::fs::create_dir(&upstream).unwrap();
        git(&upstream, &["init", "-q", "--bare", "-b", "main"]);
        let local = root.join("engine");
        Repository::init(&Git::locate(None).unwrap(), &local, Some("main")).unwrap();
        git(&local, &["commit", "-q", "--allow-empty", "-m", "Start"]);
        let hosted = "https://github.com/ada/engine.git";
        git(&local, &["remote", "add", "origin", hosted]);
        git(
            &local,
            &[
                "config",
                &format!("url.{}.insteadOf", upstream.display()),
                hosted,
            ],
        );
        git(&local, &["push", "-q", "-u", "origin", "main"]);
        git(&local, &["switch", "-q", "-c", "feature/empty-repo-fix"]);
        git(&local, &["commit", "-q", "--allow-empty", "-m", "Fix it"]);

        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        let mut accounts = Accounts::load(accounts_path(&root), store.clone());
        accounts
            .add(
                Connected {
                    account: Account {
                        kind: ProviderKind::GitHub,
                        host: "github.com".into(),
                        api_url: "https://api.github.com".into(),
                        username: "ada".into(),
                        display_name: None,
                        avatar_url: None,
                    },
                    method: SignInMethod::Token,
                },
                &Secret::new("t"),
            )
            .unwrap();
        let client = Arc::new(Creates(std::sync::Mutex::new(Vec::new())));
        let (workspace, cx) = setup(cx, Session::default(), None);
        workspace.update(cx, |workspace, cx| {
            workspace.set_hosted_env(HostedEnv {
                accounts_path: accounts_path(&root),
                store,
                client: client.clone(),
            });
            workspace.open_path(&local, true, cx).unwrap();
        });
        cx.run_until_parked();
        cx.dispatch_action(commands::CreatePullRequest);
        cx.run_until_parked();
        let sheet = workspace
            .read_with(cx, |w, _| w.create_pull_sheet().cloned())
            .expect("the sheet opens");
        sheet.update(cx, |sheet, cx| {
            assert!(sheet.can_submit(cx));
            sheet.set_draft(true, cx);
            sheet.submit(cx);
        });
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.error(), None);
            assert!(matches!(sheet.phase(), CreatePhase::Done(pull) if pull.number == 8));
        });
        let sent = client.0.lock().unwrap()[0].clone();
        assert_eq!(sent["head"], "feature/empty-repo-fix");
        assert_eq!(sent["base"], "main");
        assert_eq!(sent["title"], "Empty repo fix");
        assert_eq!(sent["draft"], true);
        // Pushed first, with its upstream set.
        assert_eq!(
            git(&local, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
            "origin/feature/empty-repo-fix"
        );
        let list = workspace.read_with(cx, |w, _| w.hosted_view()).unwrap();
        list.read_with(cx, |list, _| {
            assert_eq!(list.target().unwrap().remote, "origin");
        });
    }

    /// Answers by method and URL substring, and records what it was sent.
    struct Routes {
        routes: Vec<(bonsai_providers::Method, &'static str, String)>,
        sent: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
    }

    impl HttpClient for Routes {
        fn send(&self, request: Request) -> BoxFuture<'_, Result<Response, String>> {
            if !request.body.is_empty() {
                self.sent.lock().unwrap().push((
                    request.url.clone(),
                    serde_json::from_slice(&request.body).unwrap(),
                ));
            }
            let body = self
                .routes
                .iter()
                .find(|(method, key, _)| *method == request.method && request.url.contains(key))
                .map(|(_, _, body)| body.clone());
            Box::pin(async move {
                body.map(|body| Response {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into_bytes(),
                })
                .ok_or_else(|| format!("unexpected request: {}", request.url))
            })
        }
    }

    fn ada(root: &std::path::Path, store: &Arc<dyn TokenStore>) {
        let mut accounts = Accounts::load(accounts_path(root), store.clone());
        accounts
            .add(
                Connected {
                    account: Account {
                        kind: ProviderKind::GitHub,
                        host: "github.com".into(),
                        api_url: "https://api.github.com".into(),
                        username: "ada".into(),
                        display_name: None,
                        avatar_url: None,
                    },
                    method: SignInMethod::Token,
                },
                &Secret::new("t"),
            )
            .unwrap();
    }

    #[gpui_kit::test]
    fn reads_the_conversation_merges_and_opens_issues(cx: &mut TestAppContext) {
        use bonsai_providers::Method;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let local = root.join("engine");
        Repository::init(&Git::locate(None).unwrap(), &local, Some("main")).unwrap();
        git(
            &local,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/ada/engine.git",
            ],
        );
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        ada(&root, &store);
        let client = Arc::new(Routes {
            routes: vec![
                (Method::Get, "/pulls?", format!("[{}]", pull(7, "feature/sync", "ada/engine", false))),
                (
                    Method::Get,
                    "/issues/7/comments",
                    r#"[{"user":{"login":"grace","type":"User"},"body":"Ship it","created_at":"2026-11-12T09:00:00Z"}]"#.into(),
                ),
                (Method::Put, "/pulls/7/merge", r#"{"merged":true}"#.into()),
                (Method::Get, "/issues?", "[]".into()),
                (
                    Method::Post,
                    "/repos/ada/engine/issues",
                    r#"{"number":10,"title":"Crash","body":"","user":{"login":"ada"},"state":"open","labels":[],"comments":0,"created_at":"2026-11-16T09:00:00Z","updated_at":"2026-11-16T09:00:00Z","html_url":"https://github.com/ada/engine/issues/10"}"#.into(),
                ),
            ],
            sent: std::sync::Mutex::new(Vec::new()),
        });
        let (workspace, cx) = setup(cx, Session::default(), None);
        workspace.update(cx, |workspace, cx| {
            workspace.set_hosted_env(HostedEnv {
                accounts_path: accounts_path(&root),
                store,
                client: client.clone(),
            });
            workspace.open_path(&local, true, cx).unwrap();
        });
        cx.run_until_parked();
        cx.dispatch_action(commands::ShowPullRequests);
        cx.run_until_parked();
        let view = workspace.read_with(cx, |w, _| w.hosted_view()).unwrap();
        view.update(cx, HostedView::load_conversation);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let Some(crate::hosted::Conversation::Loaded(comments)) = view.conversation(7) else {
                panic!("comments load");
            };
            assert_eq!(comments[0].body, "Ship it");
        });

        view.update(cx, |view, cx| {
            view.set_merge_method(bonsai_providers::MergeMethod::Squash, cx);
            view.merge(cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected().unwrap().state, ItemState::Merged);
        });
        let merged = client.sent.lock().unwrap()[0].clone();
        assert!(merged.0.ends_with("/pulls/7/merge"));
        assert_eq!(merged.1["merge_method"], "squash");

        cx.dispatch_action(commands::NewIssue);
        cx.run_until_parked();
        let sheet = workspace
            .read_with(cx, |w, _| w.new_issue_sheet().cloned())
            .expect("the sheet opens");
        let title = sheet.read_with(cx, |sheet, _| sheet.title_input().clone());
        title.update_in(cx, |input, window, cx| input.set_value("Crash", window, cx));
        sheet.update(cx, crate::new_issue_sheet::NewIssueSheet::submit);
        cx.run_until_parked();
        sheet.read_with(cx, |sheet, _| {
            assert_eq!(sheet.error(), None);
            assert!(matches!(
                sheet.phase(),
                crate::new_issue_sheet::Phase::Done(issue) if issue.number == 10
            ));
        });
        assert_eq!(client.sent.lock().unwrap()[1].1["title"], "Crash");
    }

    #[gpui_kit::test]
    fn comments_on_closes_and_reopens_pull_requests(cx: &mut TestAppContext) {
        use bonsai_providers::Method;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let local = root.join("engine");
        Repository::init(&Git::locate(None).unwrap(), &local, Some("main")).unwrap();
        git(
            &local,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/ada/engine.git",
            ],
        );
        let store: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
        ada(&root, &store);
        let client = Arc::new(Routes {
            routes: vec![
                (Method::Get, "/pulls?", format!("[{}]", pull(7, "feature/sync", "ada/engine", false))),
                (Method::Get, "/issues/7/comments", "[]".into()),
                (
                    Method::Post,
                    "/issues/7/comments",
                    r#"{"user":{"login":"ada","type":"User"},"body":"Looks good","created_at":"2026-11-16T09:00:00Z"}"#.into(),
                ),
                (Method::Patch, "/issues/7", r#"{"number":7}"#.into()),
                (Method::Post, "/pulls/7/reviews", r#"{"id":1}"#.into()),
                (
                    Method::Get,
                    "/pulls/7/files",
                    r#"[{"filename":"src/sync.rs","status":"modified","additions":2,"deletions":1,"patch":"@@ -1,2 +1,3 @@\n-old\n+new\n+more"}]"#.into(),
                ),
                (
                    Method::Get,
                    "/commits/abc7/check-runs",
                    r#"{"check_runs":[{"name":"core","status":"completed","conclusion":"success","html_url":null},{"name":"macos","status":"in_progress","conclusion":null,"html_url":null}]}"#.into(),
                ),
            ],
            sent: std::sync::Mutex::new(Vec::new()),
        });
        let (workspace, cx) = setup(cx, Session::default(), None);
        workspace.update(cx, |workspace, cx| {
            workspace.set_hosted_env(HostedEnv {
                accounts_path: accounts_path(&root),
                store,
                client: client.clone(),
            });
            workspace.open_path(&local, true, cx).unwrap();
        });
        cx.run_until_parked();
        cx.dispatch_action(commands::ShowPullRequests);
        cx.run_until_parked();
        let view = workspace.read_with(cx, |w, _| w.hosted_view()).unwrap();
        view.update(cx, HostedView::load_conversation);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let Some(crate::hosted::Checks::Loaded(checks)) = view.checks(7) else {
                panic!("checks load");
            };
            assert_eq!(checks.len(), 2);
            assert_eq!(checks[1].state, bonsai_providers::CheckState::Pending);
        });
        view.update(cx, |view, cx| view.show_files(true, cx));
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let Some(crate::hosted::Files::Loaded(files)) = view.files(7) else {
                panic!("files load");
            };
            assert_eq!(files[0].path, "src/sync.rs");
            assert_eq!(files[0].additions, 2);
        });
        view.update(cx, |view, cx| view.show_files(false, cx));
        let composer = view
            .read_with(cx, |view, _| view.composer().cloned())
            .expect("the comment box is shown");
        composer.update_in(cx, |input, window, cx| {
            input.set_value("Looks good", window, cx);
        });
        view.update_in(cx, HostedView::comment);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            let Some(crate::hosted::Conversation::Loaded(comments)) = view.conversation(7) else {
                panic!("comments load");
            };
            assert_eq!(comments.len(), 1);
            assert_eq!(comments[0].body, "Looks good");
        });
        assert_eq!(
            composer.read_with(cx, |input, _| input.value().to_string()),
            ""
        );
        assert_eq!(client.sent.lock().unwrap()[0].1["body"], "Looks good");

        view.update_in(cx, |view, window, cx| {
            view.review(bonsai_providers::Verdict::Approve, window, cx);
        });
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.reviewed(7), Some(bonsai_providers::Verdict::Approve));
        });
        assert_eq!(client.sent.lock().unwrap()[1].1["event"], "APPROVE");

        view.update(cx, HostedView::toggle_open);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected().unwrap().state, ItemState::Closed);
        });
        view.update(cx, HostedView::toggle_open);
        cx.run_until_parked();
        view.read_with(cx, |view, _| {
            assert_eq!(view.selected().unwrap().state, ItemState::Open);
        });
        let sent = client.sent.lock().unwrap();
        assert_eq!(sent[2].1["state"], "closed");
        assert_eq!(sent[3].1["state"], "open");
    }
}
