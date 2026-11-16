use std::path::{Path, PathBuf};

use bonsai_git::{ChangeKind, ConflictKind, EntryStatus, Git, Head, InProgress, Oid, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

/// Runs git in `dir` with a fixed identity, panicking on failure.
fn sh(dir: &Path, args: &[&str]) -> String {
    let git = git();
    let mut all = vec![
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
    ];
    all.extend_from_slice(args);
    git.run(&git.command(all).cwd(dir))
        .unwrap()
        .stdout_lossy()
        .trim()
        .to_owned()
}

fn write(dir: &Path, path: &str, contents: &str) {
    let path = dir.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    (dir, repository)
}

fn commit(dir: &Path, path: &str, contents: &str, message: &str) {
    write(dir, path, contents);
    sh(dir, &["add", "--", path]);
    sh(dir, &["commit", "-q", "-m", message]);
}

#[test]
fn an_unborn_branch_has_no_commit() {
    let (dir, repository) = repo();
    write(dir.path(), "README.md", "hi");
    let snapshot = repository.snapshot().unwrap();
    assert_eq!(
        snapshot.head,
        Head::Branch {
            name: "main".into(),
            oid: None
        }
    );
    assert_eq!(snapshot.changes.len(), 1);
    assert_eq!(snapshot.changes[0].kind, ChangeKind::Untracked);
    assert_eq!(snapshot.branches.len(), 0);
    assert_eq!(snapshot.remotes.len(), 0);
    assert_eq!(snapshot.operation, None);
}

#[test]
fn reads_changes_refs_remotes_and_stashes() {
    let (dir, repository) = repo();
    let d = dir.path();
    commit(d, "a.txt", "one\n", "one");
    commit(d, "b.txt", "two\n", "two");
    sh(d, &["tag", "light"]);
    sh(d, &["tag", "-a", "v1", "-m", "release"]);
    sh(d, &["branch", "feature/x"]);

    // A bare remote to track.
    let remote = tempfile::tempdir().unwrap();
    sh(remote.path(), &["init", "--bare", "-q"]);
    sh(
        d,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    sh(d, &["push", "-q", "-u", "origin", "main"]);
    commit(d, "c.txt", "three\n", "three");

    write(d, "a.txt", "one changed\n");
    sh(d, &["stash", "push", "-q", "-m", "tidy"]);
    write(d, "a.txt", "staged\n");
    sh(d, &["add", "a.txt"]);
    write(d, "a.txt", "staged then edited\n");
    sh(d, &["mv", "b.txt", "renamed.txt"]);
    write(d, "new dir/untracked.txt", "x");

    let snapshot = repository.snapshot().unwrap();
    let Head::Branch {
        name,
        oid: Some(oid),
    } = &snapshot.head
    else {
        panic!("expected main, got {:?}", snapshot.head);
    };
    assert_eq!(name, "main");
    assert_eq!(oid, &Oid::new(sh(d, &["rev-parse", "HEAD"])));

    let a = snapshot
        .changes
        .iter()
        .find(|c| c.path == Path::new("a.txt"))
        .unwrap();
    assert!(a.is_staged() && a.is_unstaged());
    let renamed = snapshot
        .changes
        .iter()
        .find(|c| c.path == Path::new("renamed.txt"))
        .unwrap();
    assert_eq!(renamed.original_path.as_deref(), Some(Path::new("b.txt")));
    assert!(matches!(
        renamed.kind,
        ChangeKind::Tracked {
            index: Some(EntryStatus::Renamed),
            ..
        }
    ));
    assert!(
        snapshot.changes.iter().any(
            |c| c.path == Path::new("new dir/untracked.txt") && c.kind == ChangeKind::Untracked
        )
    );

    let main = snapshot.branches.iter().find(|b| b.name == "main").unwrap();
    assert!(main.is_head);
    let upstream = main.upstream.as_ref().unwrap();
    assert_eq!(upstream.name, "origin/main");
    assert_eq!((upstream.ahead, upstream.behind), (1, 0));
    assert!(
        snapshot
            .branches
            .iter()
            .any(|b| b.name == "feature/x" && !b.is_head)
    );
    assert_eq!(snapshot.remote_branches.len(), 1);
    assert_eq!(snapshot.remote_branches[0].remote, "origin");

    let v1 = snapshot.tags.iter().find(|t| t.name == "v1").unwrap();
    let light = snapshot.tags.iter().find(|t| t.name == "light").unwrap();
    assert!(v1.annotated && !light.annotated);
    assert_eq!(
        v1.target, light.target,
        "annotated tags are peeled to the commit"
    );

    assert_eq!(snapshot.remotes.len(), 1);
    assert_eq!(snapshot.remotes[0].name, "origin");
    assert_eq!(snapshot.stashes.len(), 1);
    assert!(snapshot.stashes[0].message.contains("tidy"));
}

#[test]
fn a_detached_head_is_reported() {
    let (dir, repository) = repo();
    commit(dir.path(), "a.txt", "1", "one");
    commit(dir.path(), "a.txt", "2", "two");
    sh(dir.path(), &["checkout", "-q", "--detach", "HEAD~1"]);
    let snapshot = repository.snapshot().unwrap();
    assert_eq!(
        snapshot.head,
        Head::Detached {
            oid: Oid::new(sh(dir.path(), &["rev-parse", "HEAD"]))
        }
    );
}

fn conflicting_branches(dir: &Path) {
    commit(dir, "f.txt", "base\n", "base");
    sh(dir, &["checkout", "-q", "-b", "other"]);
    commit(dir, "f.txt", "theirs\n", "theirs");
    sh(dir, &["checkout", "-q", "main"]);
    commit(dir, "f.txt", "ours\n", "ours");
}

#[test]
fn a_conflicted_merge_is_in_progress() {
    let (dir, repository) = repo();
    conflicting_branches(dir.path());
    let git = git();
    let merge = git
        .run_unchecked(
            &git.command([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@e",
                "merge",
                "other",
            ])
            .cwd(dir.path()),
        )
        .unwrap();
    assert!(!merge.status.success());

    let snapshot = repository.snapshot().unwrap();
    assert_eq!(snapshot.operation, Some(InProgress::Merge));
    let conflict = snapshot
        .changes
        .iter()
        .find(|c| c.path == Path::new("f.txt"))
        .unwrap();
    assert_eq!(
        conflict.kind,
        ChangeKind::Conflicted(ConflictKind::BothModified)
    );
    assert!(!conflict.is_staged());
}

#[test]
fn a_stopped_rebase_is_in_progress() {
    let (dir, repository) = repo();
    conflicting_branches(dir.path());
    let git = git();
    let rebase = git
        .run_unchecked(
            &git.command([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@e",
                "rebase",
                "other",
            ])
            .cwd(dir.path()),
        )
        .unwrap();
    assert!(!rebase.status.success());
    assert_eq!(
        repository.snapshot().unwrap().operation,
        Some(InProgress::Rebase)
    );
}

#[test]
fn a_bare_repository_has_refs_but_no_changes() {
    let (dir, _) = repo();
    commit(dir.path(), "a.txt", "1", "one");
    let bare = tempfile::tempdir().unwrap();
    let bare_path: PathBuf = bare.path().join("b.git");
    sh(
        dir.path(),
        &["clone", "-q", "--bare", ".", bare_path.to_str().unwrap()],
    );
    let repository = Repository::open(&git(), &bare_path).unwrap();
    let snapshot = repository.snapshot().unwrap();
    assert!(matches!(snapshot.head, Head::Branch { ref name, oid: Some(_) } if name == "main"));
    assert_eq!(snapshot.changes.len(), 0);
    assert_eq!(snapshot.branches.len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_paths_survive() {
    use std::os::unix::ffi::OsStrExt as _;
    let (dir, repository) = repo();
    let name = std::ffi::OsStr::from_bytes(b"caf\xe9.txt");
    std::fs::write(dir.path().join(name), "x").unwrap();
    let snapshot = repository.snapshot().unwrap();
    assert_eq!(snapshot.changes[0].path, Path::new(name));
}
