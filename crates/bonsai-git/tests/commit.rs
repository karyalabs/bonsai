use std::path::Path;

use bonsai_git::{Error, Git, Repository, Signing};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn config(path: &Path, key: &str, value: &str) {
    git()
        .run(&git().command(["config", key, value]).cwd(path))
        .unwrap();
}

fn repository() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    config(dir.path(), "user.name", "Bonsai");
    config(dir.path(), "user.email", "bonsai@example.com");
    config(dir.path(), "commit.gpgsign", "false");
    config(dir.path(), "gpg.format", "openpgp");
    (dir, repository)
}

#[test]
fn commits_amends_and_lists_recent_messages() {
    let (dir, repository) = repository();
    assert_eq!(repository.last_commit_message().unwrap(), None);
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    repository.stage_all().unwrap();
    let first = repository
        .commit("Add a\n\nThe first file.\n# a comment is stripped\n", false)
        .unwrap();
    assert_eq!(
        repository.last_commit_message().unwrap().as_deref(),
        Some("Add a\n\nThe first file.")
    );

    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    repository.stage_all().unwrap();
    repository.commit("Add b", false).unwrap();
    let amended = repository.commit("Add b, better", true).unwrap();
    assert_ne!(amended, first);
    assert_eq!(
        repository.recent_messages(5).unwrap(),
        ["Add b, better", "Add a\n\nThe first file."]
    );
    assert!(repository.snapshot().unwrap().changes.is_empty());
}

#[test]
fn an_empty_index_or_failing_hook_is_an_error_with_its_output() {
    let (dir, repository) = repository();
    let error = repository.commit("Nothing", false).unwrap_err();
    assert!(matches!(error, Error::Failed { .. }), "{error}");

    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    repository.stage_all().unwrap();
    let hook = dir.path().join(".git/hooks/pre-commit");
    std::fs::write(&hook, "#!/bin/sh\necho 'lint failed: a.txt' >&2\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let error = repository.commit("Blocked", false).unwrap_err();
    assert!(error.to_string().contains("lint failed: a.txt"), "{error}");
    assert!(repository.head_is_unborn().unwrap());
}

#[test]
fn reports_signing_configuration() {
    let (dir, repository) = repository();
    assert_eq!(repository.commit_signing().unwrap(), None);
    config(dir.path(), "commit.gpgsign", "true");
    assert_eq!(repository.commit_signing().unwrap(), Some(Signing::OpenPgp));
    config(dir.path(), "gpg.format", "ssh");
    assert_eq!(repository.commit_signing().unwrap(), Some(Signing::Ssh));
}
