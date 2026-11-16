use std::path::{Path, PathBuf};

use bonsai_git::{Git, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap();
}

#[test]
fn lists_tracked_files_but_not_untracked_or_ignored_ones() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/ui")).unwrap();
    for (file, text) in [
        ("README.md", "hi\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/ui/a b.rs", "\n"),
        (".gitignore", "*.log\n"),
    ] {
        std::fs::write(root.join(file), text).unwrap();
    }
    run(root, &["add", "--all"]);
    std::fs::write(root.join("new.txt"), "untracked\n").unwrap();
    std::fs::write(root.join("debug.log"), "ignored\n").unwrap();
    // Deleted in the work tree only: still tracked.
    std::fs::remove_file(root.join("README.md")).unwrap();

    let files = repository.tracked_files().unwrap();
    assert_eq!(
        files,
        [".gitignore", "README.md", "src/main.rs", "src/ui/a b.rs"]
            .map(PathBuf::from)
            .to_vec()
    );
}

#[test]
fn lists_a_commits_files() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    let root = dir.path();
    for (key, value) in [
        ("user.name", "Ada"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run(root, &["config", key, value]);
    }
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/old.rs"), "old\n").unwrap();
    run(root, &["add", "--all"]);
    run(root, &["commit", "-qm", "first"]);
    run(root, &["tag", "v1"]);
    run(root, &["rm", "-q", "src/old.rs"]);
    std::fs::write(root.join("new.rs"), "new\n").unwrap();
    run(root, &["add", "--all"]);
    run(root, &["commit", "-qm", "second"]);

    assert_eq!(
        repository.files_at(Some("v1")).unwrap(),
        [PathBuf::from("src/old.rs")]
    );
    assert_eq!(
        repository.files_at(Some("main")).unwrap(),
        [PathBuf::from("new.rs")]
    );
    assert!(repository.files_at(Some("nope")).is_err());
}
