use std::{
    path::Path,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use bonsai_git::{CancelToken, CommandLog, CommandRecord, Error, Git, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn canonical(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap()
}

#[test]
fn init_creates_a_repository_without_a_remote() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("new-project");
    let repository = Repository::init(&git(), &path, Some("main")).unwrap();

    assert_eq!(canonical(repository.work_dir().unwrap()), canonical(&path));
    assert_eq!(
        canonical(repository.git_dir()),
        canonical(&path.join(".git"))
    );
    assert!(!repository.is_bare());
    let head = std::fs::read_to_string(path.join(".git/HEAD")).unwrap();
    assert_eq!(head.trim(), "ref: refs/heads/main");
    let remotes = git()
        .run(&git().command(["remote"]).cwd(&path))
        .unwrap()
        .stdout_lossy();
    assert_eq!(remotes, "");
}

#[test]
fn discover_finds_the_root_from_a_subfolder() {
    let dir = tempfile::tempdir().unwrap();
    Repository::init(&git(), dir.path(), None).unwrap();
    let nested = dir.path().join("a/b");
    std::fs::create_dir_all(&nested).unwrap();

    let repository = Repository::discover(&git(), &nested).unwrap();
    assert_eq!(canonical(repository.root()), canonical(dir.path()));
}

#[test]
fn open_rejects_a_subfolder_and_a_plain_folder() {
    let dir = tempfile::tempdir().unwrap();
    Repository::init(&git(), dir.path(), None).unwrap();
    let nested = dir.path().join("src");
    std::fs::create_dir_all(&nested).unwrap();
    assert!(matches!(
        Repository::open(&git(), &nested),
        Err(Error::NotARepository(_))
    ));

    let plain = tempfile::tempdir().unwrap();
    assert!(matches!(
        Repository::open(&git(), plain.path()),
        Err(Error::NotARepository(_))
    ));
}

#[test]
fn open_detects_a_bare_repository() {
    let dir = tempfile::tempdir().unwrap();
    git()
        .run(&git().command(["init", "--bare", "--quiet"]).cwd(dir.path()))
        .unwrap();
    let repository = Repository::open(&git(), dir.path()).unwrap();
    assert!(repository.is_bare());
    assert_eq!(canonical(repository.root()), canonical(dir.path()));
}

#[test]
fn a_missing_git_is_reported() {
    let missing = Path::new("/nonexistent/bin/git");
    assert!(matches!(
        Git::locate(Some(missing)),
        Err(Error::GitNotFound)
    ));
}

#[test]
fn failures_carry_git_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let error = git()
        .run(&git().command(["rev-parse", "HEAD"]).cwd(dir.path()))
        .unwrap_err();
    let Error::Failed { record } = &error else {
        panic!("expected a failure, got {error:?}");
    };
    assert_ne!(record.exit_code, Some(0));
    assert!(error.to_string().contains("not a git repository"));
}

#[derive(Default)]
struct Recorder(Mutex<Vec<CommandRecord>>);

impl CommandLog for Recorder {
    fn record(&self, record: &CommandRecord) {
        self.0.lock().unwrap().push(record.clone());
    }
}

#[test]
fn commands_are_logged_with_credentials_redacted() {
    let recorder = Arc::new(Recorder::default());
    let git = git().with_log(recorder.clone());
    let dir = tempfile::tempdir().unwrap();
    Repository::init(&git, dir.path(), None).unwrap();
    git.run(
        &git.command([
            "remote",
            "add",
            "origin",
            "https://user:hunter2@example.com/a.git",
        ])
        .cwd(dir.path()),
    )
    .unwrap();

    let records = recorder.0.lock().unwrap();
    let last = records.last().unwrap();
    assert_eq!(
        last.args,
        ["remote", "add", "origin", "https://***@example.com/a.git"]
    );
    assert_eq!(last.exit_code, Some(0));
    assert!(
        records
            .iter()
            .all(|r| !format!("{r:?}").contains("hunter2"))
    );
}

#[test]
fn stdin_reaches_git() {
    let dir = tempfile::tempdir().unwrap();
    Repository::init(&git(), dir.path(), None).unwrap();
    let output = git()
        .run(
            &git()
                .command(["hash-object", "--stdin"])
                .cwd(dir.path())
                .stdin("hello\n"),
        )
        .unwrap();
    assert_eq!(
        output.stdout_lossy().trim(),
        "ce013625030ba8dba906f756967f9e9ca394464a"
    );
}

#[test]
fn a_cancelled_command_stops() {
    let dir = tempfile::tempdir().unwrap();
    Repository::init(&git(), dir.path(), None).unwrap();
    let token = CancelToken::new();
    let canceller = token.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(100));
        canceller.cancel();
    });
    // A shell alias stands in for a hook or ssh child that outlives the kill.
    let started = Instant::now();
    let result = git().run(
        &git()
            .command(["-c", "alias.wait=!sleep 30", "wait"])
            .cwd(dir.path())
            .cancel(token),
    );
    assert!(matches!(result, Err(Error::Cancelled)));
    assert!(started.elapsed() < Duration::from_secs(10));
}
