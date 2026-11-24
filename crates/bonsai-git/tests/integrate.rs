use std::path::{Path, PathBuf};

use bonsai_git::{
    ChangeKind, Git, InProgress, Integration, MergeMode, OperationAction, Repository, ResetMode,
    Side,
};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
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
    for (key, value) in [
        ("user.name", "Ada"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run(dir.path(), &["config", key, value]);
    }
    commit(dir.path(), "a.txt", "base\n", "base");
    (dir, repository)
}

fn subjects(root: &Path) -> Vec<String> {
    run(root, &["log", "--format=%s"])
        .lines()
        .map(str::to_owned)
        .collect()
}

/// main and topic both change a.txt.
fn diverged() -> (tempfile::TempDir, Repository) {
    let (dir, repository) = repo();
    let root = dir.path();
    run(root, &["switch", "-qc", "topic"]);
    commit(root, "a.txt", "topic\n", "topic edit");
    run(root, &["switch", "-q", "main"]);
    commit(root, "a.txt", "main\n", "main edit");
    (dir, repository)
}

fn conflicted(repository: &Repository) -> Vec<PathBuf> {
    repository
        .snapshot()
        .unwrap()
        .changes
        .into_iter()
        .filter(|c| matches!(c.kind, ChangeKind::Conflicted(_)))
        .map(|c| c.path)
        .collect()
}

#[test]
fn merges_fast_forward_and_with_a_merge_commit() {
    let (dir, repository) = repo();
    let root = dir.path();
    run(root, &["switch", "-qc", "topic"]);
    commit(root, "b.txt", "b\n", "topic work");
    run(root, &["switch", "-q", "main"]);

    assert_eq!(
        repository.merge("topic", MergeMode::NoFastForward).unwrap(),
        Integration::Completed
    );
    assert_eq!(subjects(root)[0], "Merge branch 'topic'");
    run(root, &["reset", "-q", "--hard", "HEAD~1"]);
    assert_eq!(
        repository
            .merge("topic", MergeMode::FastForwardOnly)
            .unwrap(),
        Integration::Completed
    );
    assert_eq!(subjects(root)[0], "topic work");

    let (dir, repository) = diverged();
    assert!(
        repository
            .merge("topic", MergeMode::FastForwardOnly)
            .is_err()
    );
    assert_eq!(repository.in_progress(), None);
    let _ = dir;
}

#[test]
fn a_conflicted_merge_resolves_with_a_side_and_continues() {
    let (dir, repository) = diverged();
    let root = dir.path();
    assert_eq!(
        repository.merge("topic", MergeMode::Default).unwrap(),
        Integration::Conflicts
    );
    assert_eq!(repository.in_progress(), Some(InProgress::Merge));
    assert_eq!(conflicted(&repository), [PathBuf::from("a.txt")]);

    repository
        .resolve_with(&[PathBuf::from("a.txt")], Side::Theirs)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "topic\n"
    );
    assert_eq!(conflicted(&repository), [] as [std::path::PathBuf; 0]);
    assert_eq!(
        repository
            .resolve_operation(OperationAction::Continue)
            .unwrap(),
        Integration::Completed
    );
    assert_eq!(repository.in_progress(), None);
    assert_eq!(subjects(root)[0], "Merge branch 'topic'");
}

#[test]
fn a_conflicted_rebase_aborts_or_continues() {
    let (dir, repository) = diverged();
    let root = dir.path();
    run(root, &["switch", "-q", "topic"]);
    let before = run(root, &["rev-parse", "HEAD"]);
    assert_eq!(repository.rebase("main").unwrap(), Integration::Conflicts);
    assert_eq!(repository.in_progress(), Some(InProgress::Rebase));
    repository
        .resolve_operation(OperationAction::Abort)
        .unwrap();
    assert_eq!(repository.in_progress(), None);
    assert_eq!(run(root, &["rev-parse", "HEAD"]), before);

    assert_eq!(repository.rebase("main").unwrap(), Integration::Conflicts);
    std::fs::write(root.join("a.txt"), "both\n").unwrap();
    repository.mark_resolved(&[PathBuf::from("a.txt")]).unwrap();
    assert_eq!(
        repository
            .resolve_operation(OperationAction::Continue)
            .unwrap(),
        Integration::Completed
    );
    assert_eq!(subjects(root), ["topic edit", "main edit", "base"]);
    assert_eq!(
        Side::Ours.label(Some(InProgress::Rebase)),
        "Keep Upstream Version"
    );
}

#[test]
fn cherry_picks_reverts_and_resets() {
    let (dir, repository) = repo();
    let root = dir.path();
    run(root, &["switch", "-qc", "topic"]);
    commit(root, "b.txt", "b\n", "add b");
    let picked = run(root, &["rev-parse", "HEAD"]);
    run(root, &["switch", "-q", "main"]);
    assert_eq!(
        repository.cherry_pick(&[picked.trim()]).unwrap(),
        Integration::Completed
    );
    assert!(root.join("b.txt").exists());
    let head = run(root, &["rev-parse", "HEAD"]);
    assert_eq!(
        repository.revert(head.trim()).unwrap(),
        Integration::Completed
    );
    assert!(!root.join("b.txt").exists());
    assert!(subjects(root)[0].starts_with("Revert \"add b\""));

    repository.reset("HEAD~1", ResetMode::Soft).unwrap();
    assert_eq!(subjects(root)[0], "add b");
    assert!(
        !root.join("b.txt").exists(),
        "soft reset keeps the work tree"
    );
    repository.reset("HEAD", ResetMode::Hard).unwrap();
    assert!(root.join("b.txt").exists());
    repository.reset("HEAD~1", ResetMode::Mixed).unwrap();
    assert_eq!(subjects(root), ["base"]);
    assert!(root.join("b.txt").exists());

    // A conflicting cherry-pick can be skipped.
    run(root, &["switch", "-qc", "other", "main"]);
    std::fs::remove_file(root.join("b.txt")).unwrap();
    commit(root, "a.txt", "other\n", "other edit");
    let other = run(root, &["rev-parse", "HEAD"]);
    run(root, &["switch", "-q", "main"]);
    commit(root, "a.txt", "main again\n", "main again");
    assert_eq!(
        repository.cherry_pick(&[other.trim()]).unwrap(),
        Integration::Conflicts
    );
    assert_eq!(repository.in_progress(), Some(InProgress::CherryPick));
    repository.resolve_operation(OperationAction::Skip).unwrap();
    assert_eq!(repository.in_progress(), None);
    assert_eq!(subjects(root)[0], "main again");
}

#[test]
fn cherry_picks_and_reverts_several_commits() {
    let (dir, repository) = repo();
    let root = dir.path();
    run(root, &["switch", "-qc", "topic"]);
    commit(root, "b.txt", "b\n", "add b");
    commit(root, "b.txt", "b2\n", "edit b");
    let picks: Vec<String> = run(root, &["rev-list", "--reverse", "main..topic"])
        .lines()
        .map(str::to_owned)
        .collect();
    run(root, &["switch", "-q", "main"]);
    let oldest_first: Vec<&str> = picks.iter().map(String::as_str).collect();
    assert_eq!(
        repository.cherry_pick(&oldest_first).unwrap(),
        Integration::Completed
    );
    assert_eq!(subjects(root), ["edit b", "add b", "base"]);

    // Newest first, so the second revert applies on top of the first.
    let newest_first: Vec<String> = run(root, &["rev-list", "-2", "HEAD"])
        .lines()
        .map(str::to_owned)
        .collect();
    let newest_first: Vec<&str> = newest_first.iter().map(String::as_str).collect();
    assert_eq!(
        repository.revert_commits(&newest_first).unwrap(),
        Integration::Completed
    );
    assert!(!root.join("b.txt").exists());
    assert_eq!(subjects(root).len(), 5);
}
