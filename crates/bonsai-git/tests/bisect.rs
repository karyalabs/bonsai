use std::path::Path;

use bonsai_git::{BisectVerdict, Git, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap()
        .stdout_lossy()
        .trim()
        .to_owned()
}

/// Sixteen commits writing `value.txt`; commit 11 breaks it.
fn fixture() -> (tempfile::TempDir, Repository, Vec<String>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    run(root, &["config", "user.name", "Ada Lovelace"]);
    run(root, &["config", "user.email", "ada@example.com"]);
    run(root, &["config", "commit.gpgsign", "false"]);
    let mut commits = Vec::new();
    for i in 1..=16 {
        let value = if i >= 11 { "broken" } else { "fine" };
        std::fs::write(root.join("value.txt"), format!("{value} {i}\n")).unwrap();
        run(root, &["add", "value.txt"]);
        run(root, &["commit", "-qm", &format!("Commit {i}")]);
        commits.push(run(root, &["rev-parse", "HEAD"]));
    }
    (dir, repository, commits)
}

fn is_broken(root: &Path) -> bool {
    std::fs::read_to_string(root.join("value.txt"))
        .unwrap()
        .starts_with("broken")
}

#[test]
fn finds_the_first_bad_commit_by_hand() {
    let (dir, repository, commits) = fixture();
    let root = dir.path();
    assert_eq!(repository.bisect_state().unwrap(), None);

    let mut state = repository.bisect_start("main", &[&commits[0]]).unwrap();
    assert_eq!(state.candidates, 15);
    assert_eq!(state.steps_left(), 4);
    let mut tests = 0;
    while state.found.is_none() {
        let verdict = if is_broken(root) {
            BisectVerdict::Bad
        } else {
            BisectVerdict::Good
        };
        state = repository.bisect_mark(verdict).unwrap();
        tests += 1;
        assert!(tests <= 5, "a bisect of 15 commits takes about 4 tests");
    }
    let found = state.found.unwrap();
    assert_eq!(found.oid.as_str(), commits[10]);
    assert_eq!(found.summary, "Commit 11");

    repository.bisect_reset().unwrap();
    assert_eq!(repository.bisect_state().unwrap(), None);
    assert_eq!(run(root, &["branch", "--show-current"]), "main");
}

#[test]
fn runs_a_command_at_each_step_and_skips() {
    let (dir, repository, commits) = fixture();
    let root = dir.path();
    repository
        .bisect_start(&commits[15], &[&commits[0]])
        .unwrap();
    let state = repository
        .bisect_mark_commit(&commits[12], BisectVerdict::Bad)
        .unwrap();
    assert_eq!(
        state.bad.as_ref().map(|o| o.as_str().to_owned()),
        Some(commits[12].clone())
    );
    assert_eq!(state.candidates, 12);
    let state = repository.bisect_mark(BisectVerdict::Skip).unwrap();
    assert_eq!(state.skipped.len(), 1);

    let state = repository.bisect_run("! grep -q broken value.txt").unwrap();
    assert_eq!(state.found.unwrap().oid.as_str(), commits[10]);
    repository.bisect_reset().unwrap();
    assert!(!is_broken(root) || run(root, &["rev-parse", "HEAD"]) == commits[15]);
}

#[test]
fn refuses_to_start_during_another_bisect() {
    let (_dir, repository, commits) = fixture();
    repository.bisect_start("main", &[&commits[0]]).unwrap();
    let error = repository.bisect_start("main", &[&commits[0]]).unwrap_err();
    assert!(error.to_string().contains("bisect in progress"), "{error}");
}

#[test]
fn marking_commits_in_history_starts_a_bisect() {
    let (_dir, repository, commits) = fixture();
    let state = repository
        .bisect_mark_commit(&commits[15], BisectVerdict::Bad)
        .unwrap();
    assert!(state.needs_more_marks());
    let state = repository
        .bisect_mark_commit(&commits[3], BisectVerdict::Good)
        .unwrap();
    assert!(!state.needs_more_marks());
    assert_eq!(state.candidates, 12);
    let current = state.current.unwrap();
    assert!(commits[4..15].contains(&current.oid.as_str().to_owned()));
}
