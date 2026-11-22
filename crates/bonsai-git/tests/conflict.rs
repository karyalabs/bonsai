use std::path::Path;

use bonsai_git::{Git, Repository, Resolution, Segment};

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

fn commit(root: &Path, text: &str, message: &str) {
    std::fs::write(root.join("app.rs"), text).unwrap();
    run(root, &["add", "app.rs"]);
    run(root, &["commit", "-q", "-m", message]);
}

#[test]
fn resolves_a_real_merge_one_conflict_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    run(root, &["config", "user.name", "Ada Lovelace"]);
    run(root, &["config", "user.email", "ada@example.com"]);
    commit(
        root,
        "one\nshared\ntwo\nshared\nshared\nshared\nthree\n",
        "Start",
    );
    run(root, &["checkout", "-q", "-b", "feature"]);
    commit(
        root,
        "one-f\nshared\ntwo\nshared\nshared\nshared\nthree-f\n",
        "Feature",
    );
    run(root, &["checkout", "-q", "main"]);
    commit(
        root,
        "one-m\nshared\ntwo\nshared\nshared\nshared\nthree-m\n",
        "Main",
    );
    let merge = git()
        .run_unchecked(&git().command(["merge", "-q", "feature"]).cwd(root))
        .unwrap();
    assert!(!merge.status.success(), "conflicts");

    let path = Path::new("app.rs");
    let segments = repository.read_conflicts(path).unwrap();
    let conflicts: Vec<_> = segments
        .iter()
        .filter_map(|s| match s {
            Segment::Conflict(c) => Some(c),
            Segment::Common(_) => None,
        })
        .collect();
    assert_eq!(conflicts.len(), 2);
    assert_eq!(conflicts[0].ours, ["one-m\n"]);
    assert_eq!(conflicts[0].theirs_label, "feature");

    repository
        .write_resolution(path, &segments, &[Some(Resolution::Theirs), None])
        .unwrap();
    let status = run(root, &["status", "--porcelain"]);
    assert!(status.starts_with("UU"), "still conflicted: {status}");
    let segments = repository.read_conflicts(path).unwrap();
    repository
        .write_resolution(path, &segments, &[Some(Resolution::Both)])
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("app.rs")).unwrap(),
        "one-f\nshared\ntwo\nshared\nshared\nshared\nthree-m\nthree-f\n"
    );
    assert_eq!(
        run(root, &["status", "--porcelain"]),
        "M  app.rs",
        "resolved"
    );
}
