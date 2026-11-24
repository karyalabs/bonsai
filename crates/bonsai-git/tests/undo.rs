use std::path::{Path, PathBuf};

use bonsai_git::{Git, Repository, Undo, UndoStep, plan_undo};

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
        ("user.name", "Ada Lovelace"),
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

/// Runs `operation` the way the app does, and plans its undo.
fn undoable(repository: &Repository, operation: impl FnOnce(&Repository)) -> Undo {
    let root = repository.root().to_path_buf();
    let before = repository.ref_state().unwrap();
    let worktree = status(&root);
    operation(repository);
    let after = repository.ref_state().unwrap();
    Undo {
        name: "Test".into(),
        steps: plan_undo(&before, &after, status(&root) != worktree, None),
    }
}

#[test]
fn undoes_and_redoes_a_commit_keeping_its_changes_staged() {
    let (dir, repository) = repo();
    let root = dir.path();
    let base = run(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("a.txt"), "edited\n").unwrap();
    run(root, &["add", "a.txt"]);
    let undo = undoable(&repository, |r| {
        r.commit("Edit a", false).unwrap();
    });
    let committed = run(root, &["rev-parse", "HEAD"]);

    let redo = repository.apply_undo(&undo).unwrap().expect("redoable");
    assert_eq!(run(root, &["rev-parse", "HEAD"]), base);
    assert_eq!(status(root), "M  a.txt\n");

    repository.apply_undo(&redo).unwrap();
    assert_eq!(run(root, &["rev-parse", "HEAD"]), committed);
    assert_eq!(status(root), "");
}

#[test]
fn undoes_creating_and_switching_to_a_branch() {
    let (dir, repository) = repo();
    let root = dir.path();
    let undo = undoable(&repository, |_| {
        run(root, &["checkout", "-q", "-b", "topic"]);
    });
    repository.apply_undo(&undo).unwrap();
    assert_eq!(run(root, &["branch", "--show-current"]), "main\n");
    assert_eq!(run(root, &["branch", "--list", "topic"]), "");
}

#[test]
fn brings_back_a_deleted_branch_and_tag() {
    let (dir, repository) = repo();
    let root = dir.path();
    run(root, &["branch", "topic"]);
    run(root, &["tag", "-a", "-m", "First", "v1"]);
    let tag = run(root, &["rev-parse", "refs/tags/v1"]);
    let undo = undoable(&repository, |_| {
        run(root, &["branch", "-D", "topic"]);
        run(root, &["tag", "-d", "v1"]);
    });
    repository.apply_undo(&undo).unwrap();
    assert_eq!(run(root, &["branch", "--list", "topic"]), "  topic\n");
    assert_eq!(run(root, &["rev-parse", "refs/tags/v1"]), tag);
}

#[test]
fn restores_discarded_and_deleted_files_and_redoes_the_discard() {
    let (dir, repository) = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "precious\n").unwrap();
    std::fs::create_dir(root.join("notes")).unwrap();
    std::fs::write(root.join("notes/idea.txt"), "idea\n").unwrap();
    let files = repository
        .back_up_files(&[PathBuf::from("a.txt"), PathBuf::from("notes")])
        .unwrap();
    assert_eq!(files.len(), 2);
    repository.discard(&[PathBuf::from("a.txt")]).unwrap();
    repository
        .delete_untracked(&[PathBuf::from("notes/idea.txt")])
        .unwrap();
    assert_eq!(status(root), "");

    let undo = Undo {
        name: "Discard".into(),
        steps: vec![UndoStep::RestoreFiles { files }],
    };
    let redo = repository.apply_undo(&undo).unwrap().expect("redoable");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "precious\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes/idea.txt")).unwrap(),
        "idea\n"
    );
    repository.apply_undo(&redo).unwrap();
    assert_eq!(status(root), "");
}

#[test]
fn undoes_stash_drop_and_stash_save() {
    let (dir, repository) = repo();
    let root = dir.path();
    std::fs::write(root.join("a.txt"), "wip\n").unwrap();
    let save = undoable(&repository, |r| r.stash_save("wip", false).unwrap());
    assert_eq!(status(root), "");
    let stash = repository.snapshot().unwrap().stashes[0].clone();
    let drop = undoable(&repository, |r| r.stash_drop(0, &stash.oid).unwrap());
    assert_eq!(repository.snapshot().unwrap().stashes.len(), 0);

    repository.apply_undo(&drop).unwrap();
    let restored = repository.snapshot().unwrap().stashes;
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].oid, stash.oid);

    assert_eq!(repository.apply_undo(&save).unwrap(), None);
    assert_eq!(status(root), " M a.txt\n");
    assert_eq!(repository.snapshot().unwrap().stashes.len(), 0);
}

#[test]
fn refuses_once_the_branch_has_moved() {
    let (dir, repository) = repo();
    let root = dir.path();
    let undo = undoable(&repository, |_| commit(root, "b.txt", "b\n", "Add b"));
    commit(root, "c.txt", "c\n", "Add c");
    let error = repository.apply_undo(&undo).unwrap_err().to_string();
    assert!(error.contains("has changed since"), "{error}");
    assert!(root.join("c.txt").exists());
}

#[test]
fn undoes_amending_an_earlier_commit_keeping_the_changes_staged() {
    let (dir, repository) = repo();
    let root = dir.path();
    commit(root, "b.txt", "b\n", "Add b");
    commit(root, "c.txt", "c\n", "Add c");
    let before = run(root, &["rev-parse", "HEAD"]);
    std::fs::write(root.join("b.txt"), "b, amended\n").unwrap();
    run(root, &["add", "b.txt"]);
    let undo = undoable(&repository, |r| {
        r.amend_commit("HEAD~1").unwrap();
    });
    assert_eq!(run(root, &["show", "HEAD~1:b.txt"]), "b, amended\n");

    repository.apply_undo(&undo).unwrap().expect("redoable");
    assert_eq!(run(root, &["rev-parse", "HEAD"]), before);
    assert_eq!(status(root), "M  b.txt\n");
    assert_eq!(
        std::fs::read_to_string(root.join("b.txt")).unwrap(),
        "b, amended\n"
    );
}
