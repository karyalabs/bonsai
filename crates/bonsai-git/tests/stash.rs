use std::path::{Path, PathBuf};

use bonsai_git::{DiffOptions, DiffOutput, Git, Integration, Repository, stash_diff};

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

fn status(root: &Path) -> String {
    run(root, &["status", "--porcelain"])
}

#[test]
fn saves_with_a_message_and_untracked_files_then_pops() {
    let (dir, repository) = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "edited\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();

    repository.stash_save("work in progress", true).unwrap();
    assert_eq!(status(root), "");
    let stashes = repository.snapshot().unwrap().stashes;
    assert_eq!(stashes.len(), 1);
    assert!(stashes[0].message.contains("work in progress"));

    let contents = repository.stash_contents(&stashes[0].oid).unwrap();
    assert_eq!(contents.tracked, [PathBuf::from("a.txt")]);
    assert_eq!(contents.untracked, [PathBuf::from("new.txt")]);
    let DiffOutput::Patch(patch) = repository
        .diff(&stash_diff(&stashes[0].oid), DiffOptions::default())
        .unwrap()
    else {
        panic!("a small patch");
    };
    assert!(patch.contains("+edited"));

    let outcome = repository.stash_apply(0, &stashes[0].oid, true).unwrap();
    assert_eq!(outcome, Integration::Completed);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "edited\n"
    );
    assert!(root.join("new.txt").exists());
    assert_eq!(
        repository.snapshot().unwrap().stashes,
        [] as [bonsai_git::Stash; 0]
    );
}

#[test]
fn leaves_untracked_files_alone_unless_asked() {
    let (dir, repository) = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "edited\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    repository.stash_save("", false).unwrap();
    assert_eq!(status(root), "?? new.txt\n");
    let stash = &repository.snapshot().unwrap().stashes[0];
    assert_eq!(
        repository.stash_contents(&stash.oid).unwrap().untracked,
        [] as [std::path::PathBuf; 0]
    );
}

#[test]
fn applying_over_a_conflicting_change_keeps_the_stash() {
    let (dir, repository) = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "stashed\n").unwrap();
    repository.stash_save("mine", false).unwrap();
    commit(root, "a.txt", "committed\n", "conflicting edit");

    let stash = repository.snapshot().unwrap().stashes[0].clone();
    let outcome = repository.stash_apply(0, &stash.oid, true).unwrap();
    assert_eq!(outcome, Integration::Conflicts);
    assert!(
        std::fs::read_to_string(root.join("a.txt"))
            .unwrap()
            .contains("<<<<<<<")
    );
    // A pop that conflicts keeps the stash so nothing is lost.
    assert_eq!(repository.snapshot().unwrap().stashes.len(), 1);
}

#[test]
fn drop_checks_the_stash_has_not_moved() {
    let (dir, repository) = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    repository.stash_save("one", false).unwrap();
    let first = repository.snapshot().unwrap().stashes[0].clone();
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    repository.stash_save("two", false).unwrap();

    // Index 0 is now "two", so dropping "one" by its old index is refused.
    assert!(repository.stash_drop(0, &first.oid).is_err());
    repository.stash_drop(1, &first.oid).unwrap();
    let left = repository.snapshot().unwrap().stashes;
    assert_eq!(left.len(), 1);
    assert!(left[0].message.contains("two"));
}

#[test]
fn stashes_only_the_chosen_files() {
    let (dir, repository) = repo();
    let root = dir.path();
    commit(root, "b.txt", "base\n", "add b");
    std::fs::write(root.join("a.txt"), "edited\n").unwrap();
    std::fs::write(root.join("b.txt"), "edited\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    repository
        .stash_paths(
            "just a",
            true,
            &[PathBuf::from("a.txt"), PathBuf::from("new.txt")],
        )
        .unwrap();
    assert_eq!(status(root), " M b.txt\n");
    let stash = &repository.snapshot().unwrap().stashes[0];
    let contents = repository.stash_contents(&stash.oid).unwrap();
    assert_eq!(contents.tracked, [PathBuf::from("a.txt")]);
    assert_eq!(contents.untracked, [PathBuf::from("new.txt")]);
}
