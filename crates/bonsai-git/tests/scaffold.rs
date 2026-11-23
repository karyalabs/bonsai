use bonsai_git::{Error, Git, IgnoreTemplate, License, NewRepository, Repository};

#[test]
fn creates_a_repository_with_staged_starting_files() {
    let git = Git::locate(None).expect("git is installed");
    let dir = tempfile::tempdir().unwrap();
    let new = NewRepository {
        path: dir.path().join("mill"),
        branch: Some("trunk".into()),
        ignore: IgnoreTemplate::Rust,
        readme: true,
        license: License::Mit,
        holder: "Ada Lovelace".into(),
        year: 2026,
    };
    let repository = Repository::create(&git, &new).unwrap();
    let readme = std::fs::read_to_string(new.path.join("README.md")).unwrap();
    assert_eq!(readme, "# mill\n");
    let staged = git
        .run(
            &git.command(["diff", "--cached", "--name-only"])
                .cwd(repository.root()),
        )
        .unwrap()
        .stdout_lossy();
    assert_eq!(staged, ".gitignore\nLICENSE\nREADME.md\n");
    let head = git
        .run(&git.command(["symbolic-ref", "HEAD"]).cwd(repository.root()))
        .unwrap()
        .stdout_lossy();
    assert_eq!(head.trim(), "refs/heads/trunk");

    let again = Repository::create(&git, &new).unwrap_err();
    assert!(matches!(again, Error::Refused(_)), "{again}");
}
