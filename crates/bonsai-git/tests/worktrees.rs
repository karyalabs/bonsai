use bonsai_git::{Git, NewWorktree, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

#[test]
fn adds_lists_and_removes_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    std::fs::write(root.join("a.txt"), "a\n").unwrap();
    let g = git();
    let run = |args: &[&str]| {
        g.run(&g.command(args.iter().copied()).cwd(&root)).unwrap();
    };
    run(&["add", "."]);
    run(&[
        "-c",
        "user.name=Ada",
        "-c",
        "user.email=ada@example.com",
        "commit",
        "-qm",
        "base",
    ]);
    run(&["branch", "feature"]);

    let login = dir.path().join("repo-login");
    repository
        .add_worktree(
            &login,
            &NewWorktree::NewBranch {
                name: "login".into(),
                start: None,
            },
        )
        .unwrap();
    let feature = dir.path().join("repo-feature");
    repository
        .add_worktree(&feature, &NewWorktree::Branch("feature".into()))
        .unwrap();
    // A branch can be checked out in one worktree only.
    assert!(
        repository
            .add_worktree(
                &dir.path().join("again"),
                &NewWorktree::Branch("main".into())
            )
            .is_err()
    );

    let list = repository.worktrees().unwrap();
    let branches: Vec<_> = list.iter().map(|w| w.branch.as_deref()).collect();
    // Git lists linked worktrees by path.
    assert_eq!(branches, [Some("main"), Some("feature"), Some("login")]);
    assert!(list[0].main && !list[1].main);
    assert!(login.join("a.txt").exists());
    assert_eq!(repository.snapshot().unwrap().worktrees, list);

    // A worktree with changes needs force.
    std::fs::write(feature.join("new.txt"), "x\n").unwrap();
    assert!(repository.remove_worktree(&feature, false).is_err());
    repository.remove_worktree(&feature, true).unwrap();
    repository.remove_worktree(&login, false).unwrap();
    assert_eq!(repository.worktrees().unwrap().len(), 1);
    assert!(!login.exists());
}
