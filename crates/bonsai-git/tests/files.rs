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

#[test]
fn stops_tracking_and_ignores_a_file_but_keeps_it_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    let root = dir.path();
    std::fs::write(root.join(".env"), "TOKEN=1\n").unwrap();
    std::fs::write(root.join("main.rs"), "\n").unwrap();
    run(root, &["add", "--all"]);

    repository.stop_tracking(&[PathBuf::from(".env")]).unwrap();
    assert_eq!(
        repository.tracked_files().unwrap(),
        [PathBuf::from("main.rs")]
    );
    assert!(root.join(".env").exists());
    assert_eq!(
        std::fs::read_to_string(root.join(".gitignore")).unwrap(),
        "/.env\n"
    );
}

#[test]
fn searches_file_contents_in_the_work_tree_and_at_a_revision() {
    let git = bonsai_git::Git::locate(None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let repository = bonsai_git::Repository::init(&git, dir.path(), Some("main")).unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/mill.rs"),
        "fn grind() {}\n// Grind slowly\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("logo.bin"), b"grind\x00\x01").unwrap();
    repository.stage_all().unwrap();
    git.run(
        &git.command([
            "-c",
            "user.name=Ada Lovelace",
            "-c",
            "user.email=ada@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "Add the mill",
        ])
        .cwd(dir.path()),
    )
    .unwrap();
    std::fs::write(dir.path().join("src/mill.rs"), "fn mill() {}\n").unwrap();

    let lower = repository.grep("grind", Some("HEAD"), 100).unwrap();
    let lines: Vec<usize> = lower.hits.iter().map(|hit| hit.line).collect();
    assert_eq!(lines, [1, 2]);
    assert_eq!(lower.hits[0].path, std::path::PathBuf::from("src/mill.rs"));
    let exact = repository.grep("Grind", Some("HEAD"), 100).unwrap();
    assert_eq!(exact.hits.len(), 1);
    assert_eq!(exact.hits[0].text, "// Grind slowly");
    assert_eq!(repository.grep("grind", None, 100).unwrap().hits.len(), 0);
    assert_eq!(repository.grep("mill", None, 100).unwrap().hits.len(), 1);
}
