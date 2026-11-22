use std::path::Path;

use bonsai_git::{Git, Repository};

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

fn commit(root: &Path, file: &str, text: &str, message: &str) {
    std::fs::write(root.join(file), text).unwrap();
    run(root, &["add", file]);
    run(root, &["commit", "-q", "-m", message]);
}

#[test]
fn compares_commits_and_changes_on_each_side() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    run(root, &["config", "user.name", "Ada Lovelace"]);
    run(root, &["config", "user.email", "ada@example.com"]);
    commit(root, "README.md", "hi\n", "Start");
    let start = run(root, &["rev-parse", "HEAD"]);
    run(root, &["checkout", "-q", "-b", "feature"]);
    commit(root, "a.rs", "a\nb\n", "Add a");
    commit(root, "README.md", "hello\n", "Reword the readme");
    commit(root, "c.rs", "c\n", "Add c");
    run(root, &["checkout", "-q", "main"]);
    commit(root, "main.rs", "fn main() {}\n", "Add main");

    let comparison = repository.compare("main", "feature", 2).unwrap();
    assert_eq!(
        comparison.head_oid.as_str(),
        run(root, &["rev-parse", "feature"])
    );
    assert_eq!(comparison.merge_base.unwrap().as_str(), start);
    assert_eq!(comparison.ahead_count, 3);
    assert_eq!(comparison.behind_count, 1);
    let ahead: Vec<_> = comparison
        .ahead
        .iter()
        .map(|c| c.summary.as_str())
        .collect();
    assert_eq!(
        ahead,
        ["Add c", "Reword the readme"],
        "newest first, limited"
    );
    assert_eq!(comparison.behind[0].summary, "Add main");
    assert_eq!(comparison.files_changed, 3);
    assert_eq!(comparison.insertions, 4);
    assert_eq!(comparison.deletions, 1);

    let same = repository.compare("main", "main", 10).unwrap();
    assert_eq!((same.ahead_count, same.behind_count), (0, 0));
    assert!(same.ahead.is_empty() && same.behind.is_empty());
    assert_eq!(same.files_changed, 0);

    assert!(repository.compare("main", "nope", 10).is_err());
}
