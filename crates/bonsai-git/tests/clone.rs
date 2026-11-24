use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use bonsai_git::{CancelToken, CloneOptions, CloneProgress, Error, Git, RemoteFailure, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) {
    let mut full = vec![
        "-c",
        "user.name=Bonsai",
        "-c",
        "user.email=bonsai@example.com",
    ];
    full.extend_from_slice(args);
    git().run(&git().command(full).cwd(path)).unwrap();
}

/// A source repository with a few commits.
fn source(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("source");
    Repository::init(&git(), &path, Some("main")).unwrap();
    for i in 0..20 {
        std::fs::write(
            path.join(format!("file{i}.txt")),
            format!("content {i}\n").repeat(50),
        )
        .unwrap();
        run(&path, &["add", "."]);
        run(&path, &["commit", "--quiet", "-m", &format!("commit {i}")]);
    }
    path
}

#[test]
fn clones_a_file_url_with_progress() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let url = format!("file://{}", source.display());
    let destination = dir.path().join("clone");
    let updates: Arc<Mutex<Vec<CloneProgress>>> = Arc::default();
    let sink = updates.clone();

    let repository = Repository::clone_from(
        &git(),
        &url,
        &destination,
        move |p| sink.lock().unwrap().push(p),
        None,
    )
    .unwrap();

    assert!(destination.join("file19.txt").exists());
    assert_eq!(
        repository.work_dir().unwrap().canonicalize().unwrap(),
        destination.canonicalize().unwrap()
    );
    let updates = updates.lock().unwrap();
    assert!(
        updates.iter().any(|p| p.phase == "Receiving objects"),
        "{updates:?}"
    );
    let snapshot = repository.snapshot().unwrap();
    assert_eq!(snapshot.remotes.len(), 1);
    assert_eq!(snapshot.remotes[0].name, "origin");
}

#[test]
fn a_missing_source_is_reported_as_not_found_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let destination = dir.path().join("clone");
    let url = format!("file://{}", dir.path().join("nope").display());
    let error = Repository::clone_from(&git(), &url, &destination, |_| {}, None).unwrap_err();
    assert_eq!(
        error.remote_failure(),
        Some(RemoteFailure::NotFound),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .starts_with("The repository was not found.")
    );
    assert!(!destination.exists());
}

#[test]
fn a_cancelled_clone_removes_its_folder() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    let destination = dir.path().join("clone");
    let token = CancelToken::new();
    token.cancel();
    let error = Repository::clone_from(
        &git(),
        &format!("file://{}", source.display()),
        &destination,
        |_| {},
        Some(token),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Cancelled));
    assert!(!destination.exists());
}

#[test]
fn a_shallow_clone_keeps_every_branch_and_fills_in_history_later() {
    let dir = tempfile::tempdir().unwrap();
    let source = source(dir.path());
    run(&source, &["branch", "side", "HEAD~5"]);
    let destination = dir.path().join("clone");
    let options = CloneOptions {
        shallow: true,
        submodules: false,
    };

    let repository = Repository::clone_with(
        &git(),
        &format!("file://{}", source.display()),
        &destination,
        options,
        |_| {},
        None,
    )
    .unwrap();

    assert!(repository.is_shallow().unwrap());
    let count = |repository: &Repository| {
        let output = git()
            .run(
                &git()
                    .command(["rev-list", "--count", "HEAD"])
                    .cwd(repository.root()),
            )
            .unwrap();
        output.stdout_lossy().trim().parse::<usize>().unwrap()
    };
    assert_eq!(count(&repository), 1);
    let remote = repository.snapshot().unwrap().remote_branches;
    assert!(
        remote.iter().any(|b| b.name == "side"),
        "the other branch came too: {remote:?}"
    );

    repository.fetch_full_history().unwrap();
    assert!(!repository.is_shallow().unwrap());
    assert_eq!(count(&repository), 20);
}
