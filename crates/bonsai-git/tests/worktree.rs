use std::path::{Path, PathBuf};

use bonsai_git::{ChangeKind, EntryStatus, Git, IgnoreRule, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn commit_all(path: &Path, message: &str) {
    let g = git();
    g.run(&g.command(["add", "--all"]).cwd(path)).unwrap();
    g.run(
        &g.command([
            "-c",
            "user.name=Bonsai",
            "-c",
            "user.email=bonsai@example.com",
            "commit",
            "-qm",
            message,
        ])
        .cwd(path),
    )
    .unwrap();
}

fn kinds(repository: &Repository) -> Vec<(String, ChangeKind)> {
    let mut changes: Vec<_> = repository
        .snapshot()
        .unwrap()
        .changes
        .into_iter()
        .map(|c| (c.path.to_string_lossy().into_owned(), c.kind))
        .collect();
    changes.sort_by(|a, b| a.0.cmp(&b.0));
    changes
}

fn tracked(index: Option<EntryStatus>, work_tree: Option<EntryStatus>) -> ChangeKind {
    ChangeKind::Tracked { index, work_tree }
}

#[test]
fn stage_unstage_discard_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    std::fs::write(root.join("kept.txt"), "one\n").unwrap();
    std::fs::write(root.join("weird name [1].txt"), "one\n").unwrap();
    commit_all(&root, "first");

    std::fs::write(root.join("kept.txt"), "two\n").unwrap();
    std::fs::write(root.join("weird name [1].txt"), "two\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    std::fs::create_dir(root.join("scratch")).unwrap();
    std::fs::write(root.join("scratch/a.tmp"), "x").unwrap();

    let paths = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
    repository
        .stage(&paths(&["kept.txt", "weird name [1].txt", "new.txt"]))
        .unwrap();
    assert_eq!(
        kinds(&repository),
        [
            (
                "kept.txt".into(),
                tracked(Some(EntryStatus::Modified), None)
            ),
            ("new.txt".into(), tracked(Some(EntryStatus::Added), None)),
            ("scratch/a.tmp".into(), ChangeKind::Untracked),
            (
                "weird name [1].txt".into(),
                tracked(Some(EntryStatus::Modified), None)
            ),
        ]
    );

    repository
        .unstage(&paths(&["kept.txt", "new.txt"]))
        .unwrap();
    let after = kinds(&repository);
    assert_eq!(after[0].1, tracked(None, Some(EntryStatus::Modified)));
    assert_eq!(after[1].1, ChangeKind::Untracked);

    repository.discard(&paths(&["kept.txt"])).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("kept.txt")).unwrap(),
        "one\n"
    );

    repository
        .delete_untracked(&paths(&["new.txt", "scratch/a.tmp"]))
        .unwrap();
    assert!(!root.join("new.txt").exists());
    assert!(!root.join("scratch/a.tmp").exists());

    repository.unstage_all().unwrap();
    repository.stage_all().unwrap();
    assert_eq!(kinds(&repository).len(), 1);
}

#[test]
fn unstage_works_before_the_first_commit() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    std::fs::write(root.join("b.txt"), "b").unwrap();
    repository.stage_all().unwrap();
    assert!(repository.head_is_unborn().unwrap());

    repository.unstage(&[PathBuf::from("a.txt")]).unwrap();
    let after = kinds(&repository);
    assert_eq!(after[0].1, ChangeKind::Untracked);
    assert_eq!(after[1].1, tracked(Some(EntryStatus::Added), None));
    repository.unstage_all().unwrap();
    assert!(
        kinds(&repository)
            .iter()
            .all(|c| c.1 == ChangeKind::Untracked)
    );
}

#[test]
fn ignore_appends_once_and_hides_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    std::fs::write(root.join(".gitignore"), "target/").unwrap();
    std::fs::write(root.join("debug.log"), "x").unwrap();
    std::fs::write(root.join(".env"), "SECRET=1").unwrap();

    repository
        .ignore(&IgnoreRule::Extension("log".into()))
        .unwrap();
    repository.ignore(&IgnoreRule::Path(".env".into())).unwrap();
    repository.ignore(&IgnoreRule::Path(".env".into())).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join(".gitignore")).unwrap(),
        "target/\n*.log\n/.env\n"
    );
    assert_eq!(
        kinds(&repository),
        [(".gitignore".into(), ChangeKind::Untracked)]
    );
}

#[test]
fn ten_thousand_changes_stage_in_one_call() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    let mut paths = Vec::new();
    for i in 0..10_000 {
        let name = PathBuf::from(format!("f{i:05}.txt"));
        std::fs::write(root.join(&name), "x").unwrap();
        paths.push(name);
    }
    let started = std::time::Instant::now();
    repository.stage(&paths).unwrap();
    let snapshot = repository.snapshot().unwrap();
    assert_eq!(snapshot.changes.len(), 10_000);
    assert!(snapshot.changes.iter().all(bonsai_git::Change::is_staged));
    assert!(started.elapsed().as_secs() < 20, "{:?}", started.elapsed());
}

#[test]
fn restores_a_file_as_an_earlier_commit_had_it() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    let file = dir.path().join("notes.txt");
    std::fs::write(&file, "first\n").unwrap();
    commit_all(dir.path(), "first");
    std::fs::write(&file, "second\n").unwrap();
    commit_all(dir.path(), "second");
    std::fs::remove_file(&file).unwrap();
    commit_all(dir.path(), "remove");

    // A file the work tree no longer has comes back as untracked.
    repository
        .restore_from("HEAD~2", &[PathBuf::from("notes.txt")])
        .unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\n");
    assert_eq!(
        kinds(&repository),
        [("notes.txt".to_owned(), ChangeKind::Untracked)]
    );

    // A tracked file changes in the work tree only.
    std::fs::write(dir.path().join("other.txt"), "now\n").unwrap();
    commit_all(dir.path(), "other");
    std::fs::write(dir.path().join("other.txt"), "edited\n").unwrap();
    repository
        .restore_from("HEAD", &[PathBuf::from("other.txt")])
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("other.txt")).unwrap(),
        "now\n"
    );
}
