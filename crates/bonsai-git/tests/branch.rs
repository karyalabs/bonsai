use std::path::{Path, PathBuf};

use bonsai_git::{CheckoutBlocker, Git, Head, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    let mut full = vec![
        "-c",
        "user.name=Ada",
        "-c",
        "user.email=ada@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "tag.gpgsign=false",
    ];
    full.extend_from_slice(args);
    git()
        .run(&git().command(full).cwd(path))
        .unwrap()
        .stdout_lossy()
}

fn commit(root: &Path, file: &str, contents: &str, message: &str) {
    std::fs::write(root.join(file), contents).unwrap();
    run(root, &["add", "--all"]);
    run(root, &["commit", "-qm", message]);
}

fn repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    run(dir.path(), &["config", "user.name", "Ada"]);
    run(dir.path(), &["config", "user.email", "ada@example.com"]);
    run(dir.path(), &["config", "commit.gpgsign", "false"]);
    run(dir.path(), &["config", "tag.gpgsign", "false"]);
    commit(dir.path(), "a.txt", "1\n", "base");
    (dir, repository)
}

fn head(repository: &Repository) -> String {
    match repository.snapshot().unwrap().head {
        Head::Branch { name, .. } => name,
        Head::Detached { oid } => format!("detached {}", oid.short()),
    }
}

#[test]
fn creates_renames_and_deletes_branches() {
    let (dir, repository) = repo();
    let root = dir.path();
    repository.create_branch("topic", None, false).unwrap();
    assert_eq!(head(&repository), "main");
    repository
        .create_branch("work", Some("topic"), true)
        .unwrap();
    assert_eq!(head(&repository), "work");
    commit(root, "b.txt", "2\n", "work");

    repository.rename_branch("topic", "renamed").unwrap();
    let names: Vec<String> = repository
        .snapshot()
        .unwrap()
        .branches
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(names, ["main", "renamed", "work"]);

    repository.checkout_branch("main").unwrap();
    // `work` has a commit main lacks.
    let error = repository.delete_branch("work", false).unwrap_err();
    assert!(error.is_not_fully_merged(), "{error}");
    repository.delete_branch("work", true).unwrap();
    repository.delete_branch("renamed", false).unwrap();
    assert_eq!(repository.snapshot().unwrap().branches.len(), 1);
    assert!(repository.create_branch("bad name", None, false).is_err());
}

#[test]
fn checkout_reports_changes_that_would_be_overwritten() {
    let (dir, repository) = repo();
    let root = dir.path();
    repository.create_branch("other", None, true).unwrap();
    commit(root, "a.txt", "other\n", "other");
    std::fs::write(root.join("new.txt"), "x").unwrap();
    run(root, &["add", "new.txt"]);
    run(root, &["commit", "-qm", "add new"]);
    repository.checkout_branch("main").unwrap();

    std::fs::write(root.join("a.txt"), "local edit\n").unwrap();
    let error = repository.checkout_branch("other").unwrap_err();
    assert_eq!(
        error.checkout_blocker(),
        Some(CheckoutBlocker::LocalChanges(vec![PathBuf::from("a.txt")]))
    );
    assert_eq!(head(&repository), "main");

    run(root, &["checkout", "--", "a.txt"]);
    std::fs::write(root.join("new.txt"), "untracked").unwrap();
    let error = repository.checkout_branch("other").unwrap_err();
    assert_eq!(
        error.checkout_blocker(),
        Some(CheckoutBlocker::UntrackedFiles(vec![PathBuf::from(
            "new.txt"
        )]))
    );

    // Stashing clears the way, untracked files included.
    std::fs::write(root.join("a.txt"), "local edit\n").unwrap();
    repository.stash_all("Before switching to other").unwrap();
    repository.checkout_branch("other").unwrap();
    assert_eq!(head(&repository), "other");
    assert_eq!(repository.snapshot().unwrap().stashes.len(), 1);
    assert!(
        repository
            .checkout_branch("missing")
            .unwrap_err()
            .checkout_blocker()
            .is_none()
    );

    repository.checkout_detached("main").unwrap();
    assert!(head(&repository).starts_with("detached "));
}

#[test]
fn tracks_remote_branches_and_pushes_tags() {
    let (dir, repository) = repo();
    let root = dir.path();
    let remote = tempfile::tempdir().unwrap();
    run(remote.path(), &["init", "-q", "--bare"]);
    run(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    run(root, &["push", "-q", "origin", "main", "main:shared"]);
    run(root, &["fetch", "-q", "origin"]);

    repository
        .checkout_remote_branch("origin", "shared", "shared")
        .unwrap();
    commit(root, "c.txt", "3\n", "ahead");
    let snapshot = repository.snapshot().unwrap();
    let shared = snapshot
        .branches
        .iter()
        .find(|b| b.name == "shared")
        .unwrap();
    let upstream = shared.upstream.as_ref().unwrap();
    assert_eq!(
        (upstream.name.as_str(), upstream.ahead, upstream.behind),
        ("origin/shared", 1, 0)
    );

    repository
        .set_upstream("main", Some("origin/main"))
        .unwrap();
    repository.set_upstream("shared", None).unwrap();
    let snapshot = repository.snapshot().unwrap();
    let upstream = |name: &str| {
        snapshot
            .branches
            .iter()
            .find(|b| b.name == name)
            .unwrap()
            .upstream
            .clone()
            .map(|u| u.name)
    };
    assert_eq!(upstream("main").as_deref(), Some("origin/main"));
    assert_eq!(upstream("shared"), None);

    repository.create_tag("light", None, None).unwrap();
    repository
        .create_tag("v1", Some("main"), Some("First release\n\nNotes."))
        .unwrap();
    let tags = repository.snapshot().unwrap().tags;
    let kinds: Vec<(&str, bool)> = tags
        .iter()
        .map(|t| (t.name.as_str(), t.annotated))
        .collect();
    assert_eq!(kinds, [("light", false), ("v1", true)]);
    assert_eq!(
        run(root, &["tag", "-l", "--format=%(contents)", "v1"]).trim(),
        "First release\n\nNotes."
    );

    repository.push_tag("origin", "v1").unwrap();
    assert!(run(remote.path(), &["tag", "-l"]).contains("v1"));
    repository.delete_remote_tag("origin", "v1").unwrap();
    assert!(!run(remote.path(), &["tag", "-l"]).contains("v1"));
    assert!(run(root, &["tag", "-l"]).contains("v1"));
    repository.delete_tag("light").unwrap();
    assert_eq!(repository.snapshot().unwrap().tags.len(), 1);
}
