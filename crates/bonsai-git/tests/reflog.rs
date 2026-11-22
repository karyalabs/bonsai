use std::path::Path;

use bonsai_git::{Git, ReflogAction, Repository};

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

#[test]
fn lists_where_head_has_been_and_which_commits_are_lost() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    assert!(repository.reflog(50).unwrap().is_empty(), "no commits yet");
    for args in [
        &["config", "user.name", "Ada Lovelace"][..],
        &["config", "user.email", "ada@example.com"],
        &["commit", "-q", "--allow-empty", "-m", "One"],
        &["commit", "-q", "--allow-empty", "-m", "Two"],
        &["checkout", "-q", "-b", "feature"],
        &["commit", "-q", "--allow-empty", "-m", "Three"],
        &["checkout", "-q", "main"],
        &["branch", "-q", "-D", "feature"],
        &["commit", "-q", "--allow-empty", "-m", "Four"],
        &["reset", "-q", "--hard", "HEAD~1"],
    ] {
        run(root, args);
    }
    let three = run(root, &["rev-parse", "HEAD@{3}"]);
    let four = run(root, &["rev-parse", "HEAD@{1}"]);

    let entries = repository.reflog(50).unwrap();
    assert_eq!(entries.len(), 7);
    assert_eq!(entries[0].action, ReflogAction::Reset);
    assert_eq!(entries[0].describe(), "Reset to HEAD~1");
    assert_eq!(entries[0].summary, "Two");
    assert!(!entries[0].lost);
    assert_eq!(entries[1].oid.as_str(), four);
    assert!(entries[1].lost, "Four is only in the reflog");
    assert_eq!(entries[2].describe(), "Switched from feature to main");
    assert_eq!(entries[3].oid.as_str(), three);
    assert!(entries[3].lost, "its branch was deleted");
    assert_eq!(entries[3].index, 3);
    assert!(entries.iter().all(|e| e.time > 1_600_000_000));

    assert_eq!(repository.reflog(2).unwrap().len(), 2);
}
