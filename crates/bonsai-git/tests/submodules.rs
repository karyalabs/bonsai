use std::path::{Path, PathBuf};

use bonsai_git::{Git, Oid, Repository, SubmoduleState};

/// Git that may clone submodules from a local folder, which Git refuses by
/// default since 2.38.
fn git() -> Git {
    Git::locate(None)
        .expect("git is installed")
        .with_env("GIT_CONFIG_COUNT", "1")
        .with_env("GIT_CONFIG_KEY_0", "protocol.file.allow")
        .with_env("GIT_CONFIG_VALUE_0", "always")
}

fn run(path: &Path, args: &[&str]) -> String {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap()
        .stdout_lossy()
        .trim()
        .to_owned()
}

fn repo(path: &Path) -> Repository {
    std::fs::create_dir_all(path).unwrap();
    let repository = Repository::init(&git(), path, Some("main")).unwrap();
    for (key, value) in [
        ("user.name", "Ada Lovelace"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run(path, &["config", key, value]);
    }
    repository
}

#[test]
fn adds_lists_updates_and_removes_submodules() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    repo(&library);
    std::fs::write(library.join("lib.txt"), "one\n").unwrap();
    run(&library, &["add", "."]);
    run(&library, &["commit", "-qm", "One"]);
    let first = run(&library, &["rev-parse", "HEAD"]);

    let app = dir.path().join("app");
    let repository = repo(&app);
    std::fs::write(app.join("app.txt"), "app\n").unwrap();
    run(&app, &["add", "."]);
    run(&app, &["commit", "-qm", "App"]);
    assert_eq!(repository.submodules().unwrap(), []);

    let url = library.to_string_lossy().into_owned();
    repository
        .add_submodule(&url, Path::new("vendor/library"))
        .unwrap();
    run(&app, &["commit", "-qm", "Add library"]);
    let submodules = repository.submodules().unwrap();
    assert_eq!(submodules.len(), 1);
    assert_eq!(submodules[0].path, PathBuf::from("vendor/library"));
    assert_eq!(submodules[0].url.as_deref(), Some(url.as_str()));
    assert_eq!(submodules[0].oid.as_str(), first);
    assert_eq!(submodules[0].state, SubmoduleState::UpToDate);
    assert_eq!(repository.snapshot().unwrap().submodules, submodules);

    // A new commit upstream: Update to Latest checks it out, and the
    // submodule shows as on another commit until that is committed.
    std::fs::write(library.join("lib.txt"), "two\n").unwrap();
    run(&library, &["commit", "-qam", "Two"]);
    let second = run(&library, &["rev-parse", "HEAD"]);
    repository.update_submodules_remote(&[]).unwrap();
    let submodule = &repository.submodules().unwrap()[0];
    assert_eq!(submodule.oid.as_str(), second);
    assert_eq!(submodule.state, SubmoduleState::OtherCommit);
    let path = Path::new("vendor/library");
    let forward =
        repository.submodule_move(path, Some(Oid::new(&first)), Some(Oid::new(&second)), false);
    assert!(forward.readable);
    let summaries: Vec<&str> = forward.added.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["Two"]);
    assert_eq!(forward.removed, []);
    let back =
        repository.submodule_move(path, Some(Oid::new(&second)), Some(Oid::new(&first)), false);
    assert_eq!(back.added, []);
    assert_eq!(back.removed.len(), 1);

    // Update goes back to the recorded commit.
    repository
        .update_submodules(&[PathBuf::from("vendor/library")])
        .unwrap();
    let submodule = &repository.submodules().unwrap()[0];
    assert_eq!(submodule.oid.as_str(), first);
    assert_eq!(submodule.state, SubmoduleState::UpToDate);

    repository
        .remove_submodule(Path::new("vendor/library"))
        .unwrap();
    run(&app, &["commit", "-qm", "Remove library"]);
    assert_eq!(repository.submodules().unwrap(), []);
    assert!(!app.join("vendor/library").exists());
}

#[test]
fn a_fresh_clone_lists_submodules_as_not_initialized() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    repo(&library);
    run(&library, &["commit", "-q", "--allow-empty", "-m", "One"]);
    let app = dir.path().join("app");
    let repository = repo(&app);
    repository
        .add_submodule(&library.to_string_lossy(), Path::new("lib"))
        .unwrap();
    run(&app, &["commit", "-qm", "Add lib"]);

    let clone = dir.path().join("clone");
    run(
        dir.path(),
        &[
            "clone",
            "-q",
            &app.to_string_lossy(),
            &clone.to_string_lossy(),
        ],
    );
    let cloned = Repository::open(&git(), &clone).unwrap();
    assert_eq!(
        cloned.submodules().unwrap()[0].state,
        SubmoduleState::NotInitialized
    );
    cloned.update_submodules(&[]).unwrap();
    assert_eq!(
        cloned.submodules().unwrap()[0].state,
        SubmoduleState::UpToDate
    );
}
