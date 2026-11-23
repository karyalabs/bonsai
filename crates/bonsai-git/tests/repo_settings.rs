use std::path::Path;

use bonsai_git::{Git, RepoSettingsEdit, Repository, SigningChoice};

fn git() -> Git {
    // A stand-in for the global config, so the test doesn't read the machine's.
    Git::locate(None)
        .expect("git is installed")
        .with_env("GIT_CONFIG_GLOBAL", "/dev/null")
        .with_env("GIT_CONFIG_NOSYSTEM", "1")
        .with_env("GIT_CONFIG_COUNT", "2")
        .with_env("GIT_CONFIG_KEY_0", "user.name")
        .with_env("GIT_CONFIG_VALUE_0", "Ada Lovelace")
        .with_env("GIT_CONFIG_KEY_1", "commit.gpgsign")
        .with_env("GIT_CONFIG_VALUE_1", "true")
}

fn repo() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    (dir, repository)
}

#[test]
fn overrides_and_clears_identity_signing_and_pruning() {
    let (_dir, repository) = repo();
    let settings = repository.repo_settings().unwrap();
    assert_eq!(settings.name.local, None);
    assert_eq!(settings.name.inherited.as_deref(), Some("Ada Lovelace"));
    assert_eq!(settings.signing(), SigningChoice::Inherit);
    assert!(settings.inherits_signing());

    let edit = RepoSettingsEdit {
        name: Some("Ada".into()),
        email: Some(" ada@example.com ".into()),
        signing: SigningChoice::Ssh,
        key: Some("~/.ssh/id_ed25519.pub".into()),
        prune: Some(true),
    };
    repository.apply_repo_settings(&edit).unwrap();
    let settings = repository.repo_settings().unwrap();
    assert_eq!(settings.name.effective(), Some("Ada"));
    assert_eq!(settings.email.local.as_deref(), Some("ada@example.com"));
    assert_eq!(settings.signing(), SigningChoice::Ssh);
    assert_eq!(settings.key.local.as_deref(), Some("~/.ssh/id_ed25519.pub"));
    assert_eq!(settings.prune.local.as_deref(), Some("true"));

    repository
        .apply_repo_settings(&RepoSettingsEdit {
            name: None,
            email: Some("  ".into()),
            signing: SigningChoice::Off,
            key: None,
            prune: None,
        })
        .unwrap();
    let settings = repository.repo_settings().unwrap();
    assert_eq!(settings.name.local, None);
    assert_eq!(settings.name.effective(), Some("Ada Lovelace"));
    assert_eq!(settings.email.local, None);
    assert_eq!(settings.signing(), SigningChoice::Off);
    assert_eq!(settings.format.local, None);
    assert_eq!(settings.prune.local, None);
}

#[cfg(unix)]
#[test]
fn lists_hooks_and_turns_them_on_and_off() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, repository) = repo();
    let hooks = dir.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let write = |name: &str, mode: u32| {
        let path: &Path = &hooks.join(name);
        std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };
    write("pre-commit", 0o755);
    write("commit-msg", 0o644);
    write("notes.txt", 0o755);

    let settings = repository.repo_settings().unwrap();
    let listed: Vec<(&str, bool)> = settings
        .hooks
        .iter()
        .map(|h| (h.name.as_str(), h.enabled))
        .collect();
    assert_eq!(listed, [("commit-msg", false), ("pre-commit", true)]);

    repository.set_hook_enabled("pre-commit", false).unwrap();
    repository.set_hook_enabled("commit-msg", true).unwrap();
    let settings = repository.repo_settings().unwrap();
    assert!(
        settings
            .hooks
            .iter()
            .all(|h| h.enabled != (h.name == "pre-commit"))
    );
    assert!(repository.set_hook_enabled("notes.txt", true).is_err());
}
