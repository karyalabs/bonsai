use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use bonsai_git::{Git, PullMode, PushRequest, Repository, SyncFailure};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap()
        .stdout_lossy()
}

fn configure(root: &Path) {
    for (key, value) in [
        ("user.name", "Ada"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run(root, &["config", key, value]);
    }
}

fn commit(root: &Path, file: &str, contents: &str) {
    std::fs::write(root.join(file), contents).unwrap();
    run(root, &["add", "--all"]);
    run(root, &["commit", "-qm", file]);
}

/// A bare remote and two clones of it, `a` and `b`, with one commit.
struct Setup {
    _dirs: Vec<tempfile::TempDir>,
    remote: std::path::PathBuf,
    a: Repository,
    b: Repository,
}

fn setup() -> Setup {
    let remote = tempfile::tempdir().unwrap();
    run(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    let a = tempfile::tempdir().unwrap();
    let repo_a = Repository::init(&git(), a.path(), Some("main")).unwrap();
    configure(a.path());
    commit(a.path(), "one.txt", "1");
    repo_a
        .add_remote("origin", remote.path().to_str().unwrap())
        .unwrap();
    repo_a
        .push(
            &PushRequest::new("origin", "main").set_upstream(),
            None,
            None,
        )
        .unwrap();
    let b = tempfile::tempdir().unwrap();
    run(
        b.path(),
        &["clone", "-q", remote.path().to_str().unwrap(), "."],
    );
    configure(b.path());
    let repo_b = Repository::open(&git(), b.path()).unwrap();
    Setup {
        remote: remote.path().to_path_buf(),
        _dirs: vec![remote, a, b],
        a: repo_a,
        b: repo_b,
    }
}

fn upstream(repository: &Repository, branch: &str) -> Option<(String, u32, u32)> {
    repository
        .snapshot()
        .unwrap()
        .branches
        .into_iter()
        .find(|b| b.name == branch)?
        .upstream
        .map(|u| (u.name, u.ahead, u.behind))
}

#[test]
fn fetches_pulls_and_pushes() {
    let s = setup();
    assert_eq!(upstream(&s.a, "main"), Some(("origin/main".into(), 0, 0)));

    commit(s.b.root(), "two.txt", "2");
    s.b.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();

    let updates = Arc::new(Mutex::new(0));
    let counter = updates.clone();
    s.a.fetch(
        None,
        true,
        Some(Arc::new(move |_| *counter.lock().unwrap() += 1)),
        None,
    )
    .unwrap();
    assert_eq!(upstream(&s.a, "main"), Some(("origin/main".into(), 0, 1)));

    s.a.pull(PullMode::FastForwardOnly, None, None).unwrap();
    assert!(s.a.root().join("two.txt").exists());
    assert_eq!(upstream(&s.a, "main"), Some(("origin/main".into(), 0, 0)));
}

#[test]
fn a_rejected_push_explains_itself_and_pull_combines() {
    let s = setup();
    commit(s.b.root(), "theirs.txt", "b");
    s.b.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();
    commit(s.a.root(), "ours.txt", "a");

    let error =
        s.a.push(&PushRequest::new("origin", "main"), None, None)
            .unwrap_err();
    assert_eq!(error.sync_failure(), Some(SyncFailure::Rejected));
    assert!(error.to_string().contains("Pull first"), "{error}");

    let error = s.a.pull(PullMode::FastForwardOnly, None, None).unwrap_err();
    assert_eq!(error.sync_failure(), Some(SyncFailure::Diverged));

    s.a.pull(PullMode::Rebase, None, None).unwrap();
    assert_eq!(upstream(&s.a, "main"), Some(("origin/main".into(), 1, 0)));
    s.a.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();
    assert_eq!(
        run(&s.remote, &["log", "-1", "--format=%s", "main"]).trim(),
        "ours.txt"
    );
}

#[test]
fn pull_keeps_uncommitted_changes_to_the_files_it_updates() {
    let s = setup();
    commit(s.b.root(), "list.txt", "a\nb\nc\nd\ne\n");
    s.b.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();
    s.a.pull(PullMode::FastForwardOnly, None, None).unwrap();

    for (mode, theirs) in [(PullMode::Merge, "A"), (PullMode::Rebase, "B")] {
        let first = format!("{theirs}\nb\nc\nd\ne\n");
        commit(s.b.root(), "list.txt", &first);
        s.b.push(&PushRequest::new("origin", "main"), None, None)
            .unwrap();
        // Unsaved work at the other end of the file the pull changes, and
        // a staged change to another file, which stays staged.
        let ours = std::fs::read_to_string(s.a.root().join("list.txt"))
            .unwrap()
            .replace('e', "mine");
        std::fs::write(s.a.root().join("list.txt"), ours).unwrap();
        std::fs::write(s.a.root().join("one.txt"), theirs).unwrap();
        run(s.a.root(), &["add", "one.txt"]);

        s.a.pull(mode, None, None).unwrap();
        assert_eq!(
            std::fs::read_to_string(s.a.root().join("list.txt")).unwrap(),
            format!("{theirs}\nb\nc\nd\nmine\n")
        );
        assert_eq!(run(s.a.root(), &["stash", "list"]), "");
        assert_eq!(
            run(s.a.root(), &["status", "--porcelain"]),
            " M list.txt\nM  one.txt\n"
        );
        run(s.a.root(), &["reset", "-q", "--hard"]);
    }
}

#[test]
fn a_pull_that_stops_on_conflicts_says_so() {
    let s = setup();
    commit(s.b.root(), "one.txt", "theirs");
    s.b.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();
    commit(s.a.root(), "one.txt", "ours");
    let error = s.a.pull(PullMode::Merge, None, None).unwrap_err();
    assert_eq!(error.sync_failure(), Some(SyncFailure::Conflicts));
    assert!(error.to_string().starts_with("Pulled, but"), "{error}");
}

#[test]
fn changes_that_clash_with_a_pull_wait_in_the_stash() {
    let s = setup();
    commit(s.b.root(), "one.txt", "theirs");
    s.b.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();
    std::fs::write(s.a.root().join("one.txt"), "mine").unwrap();
    let error = s.a.pull(PullMode::Merge, None, None).unwrap_err();
    assert!(error.to_string().contains("keeps a copy"), "{error}");
    assert_eq!(upstream(&s.a, "main"), Some(("origin/main".into(), 0, 0)));
    assert!(run(s.a.root(), &["stash", "list"]).contains("Saved before pull"));
    assert!(
        std::fs::read_to_string(s.a.root().join("one.txt"))
            .unwrap()
            .contains("<<<<<<<")
    );
}

#[test]
fn force_push_with_lease_refuses_unseen_commits() {
    let s = setup();
    // b rewrites history and force pushes.
    commit(s.b.root(), "b.txt", "b");
    s.b.push(&PushRequest::new("origin", "main"), None, None)
        .unwrap();
    // a amends its last commit without fetching b's commit.
    run(s.a.root(), &["commit", "-q", "--amend", "-m", "rewritten"]);
    let error =
        s.a.push(
            &PushRequest::new("origin", "main").force_with_lease(),
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(error.sync_failure(), Some(SyncFailure::StaleLease));

    // After fetching, the lease matches and the force push goes through.
    s.a.fetch(Some("origin"), false, None, None).unwrap();
    s.a.push(
        &PushRequest::new("origin", "main").force_with_lease(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(
        run(&s.remote, &["log", "-1", "--format=%s", "main"]).trim(),
        "rewritten"
    );
}

#[test]
fn publishes_branches_and_manages_remotes() {
    let s = setup();
    s.a.create_branch("topic", None, true).unwrap();
    assert_eq!(upstream(&s.a, "topic"), None);
    let error = s.a.pull(PullMode::Merge, None, None).unwrap_err();
    assert_eq!(error.sync_failure(), Some(SyncFailure::NoUpstream));
    s.a.push(
        &PushRequest::new("origin", "topic").set_upstream(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(upstream(&s.a, "topic"), Some(("origin/topic".into(), 0, 0)));
    s.a.delete_remote_branch("origin", "topic").unwrap();
    assert!(!run(&s.remote, &["branch"]).contains("topic"));

    s.a.rename_remote("origin", "upstream").unwrap();
    s.a.set_remote_url("upstream", "https://example.com/owner/repo.git")
        .unwrap();
    let remotes = s.a.snapshot().unwrap().remotes;
    assert_eq!(remotes.len(), 1);
    assert_eq!(remotes[0].name, "upstream");
    assert_eq!(
        remotes[0].fetch_url.as_deref(),
        Some("https://example.com/owner/repo.git")
    );
    s.a.remove_remote("upstream").unwrap();
    assert_eq!(s.a.snapshot().unwrap().remotes.len(), 0);
}
