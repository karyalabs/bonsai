use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use bonsai_git::{Git, PurgeBlocker, PurgeStep, PurgeTarget, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    let mut full = vec![
        "-c",
        "user.name=Ada",
        "-c",
        "user.email=ada@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "tag.gpgsign=false",
    ];
    full.extend_from_slice(args);
    git()
        .run(&git().command(full).cwd(path))
        .unwrap()
        .stdout_lossy()
}

fn write(root: &Path, path: &str, contents: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn commit(root: &Path, message: &str) {
    run(root, &["add", "--all"]);
    run(root, &["commit", "-qm", message]);
}

fn refs(root: &Path) -> BTreeMap<String, String> {
    run(root, &["for-each-ref", "--format=%(refname) %(objectname)"])
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .collect()
}

/// Whether any reachable commit or object still has `path`.
fn in_history(root: &Path, path: &str) -> bool {
    !run(root, &["log", "--all", "--format=%H", "--", path])
        .trim()
        .is_empty()
        || run(root, &["rev-list", "--all", "--objects"])
            .lines()
            .any(|l| l.split_once(' ').is_some_and(|(_, p)| p == path))
}

/// main: base, add .env and app, feature branch merged back, .env deleted,
/// an annotated tag, and a commit that only changes .env.
fn fixture() -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    write(root, "README.md", "hello\n");
    commit(root, "base");
    write(root, ".env", "SECRET=1\n");
    write(root, "app.rs", "fn main() {}\n");
    commit(root, "add app and env");
    write(root, ".env", "SECRET=2\n");
    commit(root, "rotate secret");
    run(root, &["tag", "-a", "v1", "-m", "first release"]);
    run(root, &["switch", "-qc", "feature"]);
    write(root, "feature.rs", "// feature\n");
    commit(root, "feature work");
    run(root, &["switch", "-q", "main"]);
    write(root, "app.rs", "fn main() { run() }\n");
    commit(root, "main work");
    run(
        root,
        &["merge", "--no-ff", "-qm", "Merge feature", "feature"],
    );
    run(root, &["rm", "-q", ".env"]);
    commit(root, "remove env");
    (dir, repository)
}

#[test]
fn purges_a_file_committed_then_deleted_and_restores_it() {
    let (dir, repository) = fixture();
    let root = dir.path();
    let backups = tempfile::tempdir().unwrap();
    // The secret is still on disk, untracked and ignored, as is typical.
    write(root, ".env", "SECRET=2\n");
    let original = refs(root);

    let targets = [PurgeTarget::Path(".env".into())];
    let scan = repository.purge_scan(&targets).unwrap();
    assert!(scan.blockers.is_empty(), "{:?}", scan.blockers);
    assert_eq!(scan.commits.len(), 3);
    assert_eq!(scan.paths, [PathBuf::from(".env")]);
    assert_eq!(
        scan.refs,
        ["refs/heads/feature", "refs/heads/main", "refs/tags/v1"]
    );

    let steps = RefCell::new(Vec::new());
    let report = repository
        .purge(&targets, backups.path(), &|step| {
            steps.borrow_mut().push(step);
        })
        .unwrap();
    assert_eq!(
        *steps.borrow(),
        [
            PurgeStep::Backup,
            PurgeStep::Rewrite,
            PurgeStep::Cleanup,
            PurgeStep::Verify
        ]
    );
    assert!(report.backup.exists());
    // "rotate secret" and "remove env" only touched .env.
    assert_eq!(report.commits_removed, 2);
    assert!(report.commits_rewritten >= 4);
    assert_eq!(report.blobs_removed, 2);
    assert_eq!(report.refs.len(), 3);
    assert!(!in_history(root, ".env"));
    assert_eq!(
        run(root, &["log", "--format=%s", "main"])
            .lines()
            .collect::<Vec<_>>(),
        [
            "Merge feature",
            "main work",
            "feature work",
            "add app and env",
            "base"
        ]
    );
    // The tag followed its commit and kept its message.
    assert_eq!(
        run(root, &["cat-file", "-p", "v1"]).lines().last(),
        Some("first release")
    );
    assert_eq!(
        run(root, &["log", "-1", "--format=%s", "v1^{commit}"]).trim(),
        "add app and env"
    );
    // The file stays on disk and the work tree is clean apart from it.
    assert_eq!(
        std::fs::read_to_string(root.join(".env")).unwrap(),
        "SECRET=2\n"
    );
    assert_eq!(run(root, &["status", "--porcelain"]).trim(), "?? .env");

    let restored = repository.restore_backup(&report.backup).unwrap();
    assert_eq!(restored.len(), 3);
    assert_eq!(refs(root), original);
    assert!(in_history(root, ".env"));
    // main's tip deleted .env, so the copy on disk is untracked either way.
    assert_eq!(run(root, &["status", "--porcelain"]).trim(), "?? .env");
}

#[test]
fn purges_directories_globs_and_non_utf8_paths() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    write(root, "secrets/a.key", "a\n");
    write(root, "secrets/nested/b.key", "b\n");
    write(root, "certs/server.pem", "pem\n");
    write(root, "deep/er/client.pem", "pem2\n");
    write(root, "keep.txt", "keep\n");
    write(root, "same-as-pem.txt", "pem\n");
    #[cfg(unix)]
    {
        // APFS refuses non-UTF-8 file names, so the path goes straight into
        // the index.
        use std::os::unix::ffi::OsStrExt as _;
        let blob = git()
            .run(
                &git()
                    .command(["hash-object", "-w", "--stdin"])
                    .cwd(root)
                    .stdin(b"bytes\n".to_vec()),
            )
            .unwrap()
            .stdout_lossy();
        let mut info = format!("100644,{},", blob.trim()).into_bytes();
        info.extend_from_slice(b"latin-\xe9.txt");
        git()
            .run(
                &git()
                    .command([
                        std::ffi::OsStr::new("update-index"),
                        std::ffi::OsStr::new("--add"),
                        std::ffi::OsStr::new("--cacheinfo"),
                        std::ffi::OsStr::from_bytes(&info),
                    ])
                    .cwd(root),
            )
            .unwrap();
    }
    write(root, "sp ace \"quoted\".txt", "q\n");
    // `--ignore-removal` keeps the index-only path staged.
    run(root, &["add", "--ignore-removal", "--", "."]);
    run(root, &["commit", "-qm", "everything"]);
    write(root, "keep.txt", "keep 2\n");
    commit(root, "more");

    let mut targets = vec![
        PurgeTarget::Path("secrets".into()),
        PurgeTarget::Glob("*.pem".into()),
        PurgeTarget::Path("sp ace \"quoted\".txt".into()),
    ];
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        targets.push(PurgeTarget::Path(
            std::ffi::OsStr::from_bytes(b"latin-\xe9.txt").into(),
        ));
    }
    let backups = tempfile::tempdir().unwrap();
    let report = repository.purge(&targets, backups.path(), &|_| {}).unwrap();
    assert_eq!(report.commits_removed, 0);
    let mut paths: Vec<String> = report
        .paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    assert_eq!(paths.len(), if cfg!(unix) { 6 } else { 5 });
    // certs/server.pem shares its contents with same-as-pem.txt.
    assert_eq!(report.blobs_shared, 1);
    let tree = run(root, &["ls-tree", "-r", "--name-only", "main"]);
    assert_eq!(
        tree.lines().collect::<Vec<_>>(),
        ["keep.txt", "same-as-pem.txt"]
    );
    assert_eq!(run(root, &["rev-list", "--count", "main"]).trim(), "2");
    assert!(root.join("secrets/nested/b.key").exists());
}

#[test]
fn drops_stashes_built_on_the_old_history_and_restores_them() {
    let (dir, repository) = fixture();
    let root = dir.path();
    write(root, "app.rs", "fn main() { stashed() }\n");
    run(root, &["stash", "push", "-qm", "wip"]);
    let stash = run(root, &["rev-parse", "stash@{0}"]);

    let targets = [PurgeTarget::Path(".env".into())];
    assert_eq!(repository.purge_scan(&targets).unwrap().stashes, [0]);
    let backups = tempfile::tempdir().unwrap();
    let report = repository.purge(&targets, backups.path(), &|_| {}).unwrap();
    assert_eq!(report.stashes_dropped, 1);
    assert_eq!(run(root, &["stash", "list"]), "");
    assert!(!in_history(root, ".env"));
    assert!(
        run(root, &["for-each-ref", "refs/bonsai/"]).is_empty(),
        "no temporary refs are left behind"
    );

    repository.restore_backup(&report.backup).unwrap();
    assert_eq!(run(root, &["rev-parse", "stash@{0}"]), stash);
    assert!(run(root, &["stash", "list"]).contains("wip"));
}

#[test]
fn a_detached_head_follows_the_rewrite() {
    let (dir, repository) = fixture();
    let root = dir.path();
    run(root, &["switch", "-q", "--detach", "v1"]);
    let backups = tempfile::tempdir().unwrap();
    repository
        .purge(&[PurgeTarget::Path(".env".into())], backups.path(), &|_| {})
        .unwrap();
    assert_eq!(
        run(root, &["rev-parse", "HEAD"]),
        run(root, &["rev-parse", "v1^{commit}"])
    );
    assert_eq!(run(root, &["status", "--porcelain"]).trim(), "?? .env");
}

#[test]
fn scans_report_blockers_and_nothing_found() {
    let (dir, repository) = fixture();
    let root = dir.path();
    let none = repository
        .purge_scan(&[PurgeTarget::Path("missing.txt".into())])
        .unwrap();
    assert!(!none.found());
    let backups = tempfile::tempdir().unwrap();
    assert!(
        repository
            .purge(
                &[PurgeTarget::Path("missing.txt".into())],
                backups.path(),
                &|_| {}
            )
            .is_err()
    );

    write(root, "app.rs", "changed\n");
    let scan = repository
        .purge_scan(&[PurgeTarget::Path(".env".into())])
        .unwrap();
    assert_eq!(scan.blockers, [PurgeBlocker::UncommittedChanges]);
    let error = repository
        .purge(&[PurgeTarget::Path(".env".into())], backups.path(), &|_| {})
        .unwrap_err();
    assert!(error.to_string().contains("Commit or stash"));
    assert_eq!(std::fs::read_dir(backups.path()).unwrap().count(), 0);

    let history = repository.history_paths().unwrap();
    assert_eq!(
        history,
        [".env", "README.md", "app.rs", "feature.rs"].map(PathBuf::from)
    );
}

#[test]
fn stashes_unrelated_to_the_paths_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    write(root, "app.rs", "1\n");
    commit(root, "base");
    run(root, &["switch", "-qc", "secret"]);
    write(root, ".env", "SECRET\n");
    commit(root, "env");
    run(root, &["switch", "-q", "main"]);
    write(root, "app.rs", "2\n");
    run(root, &["stash", "push", "-qm", "older"]);
    write(root, "app.rs", "3\n");
    run(root, &["stash", "push", "-qm", "newer"]);
    let before = run(root, &["stash", "list", "--format=%H %gs"]);

    let targets = [PurgeTarget::Path(".env".into())];
    assert_eq!(
        repository.purge_scan(&targets).unwrap().stashes,
        Vec::<usize>::new()
    );
    let backups = tempfile::tempdir().unwrap();
    let report = repository.purge(&targets, backups.path(), &|_| {}).unwrap();
    assert_eq!(report.stashes_dropped, 0);
    // The branch held only the purged commit, so it now matches main.
    assert_eq!(
        run(root, &["rev-parse", "secret"]),
        run(root, &["rev-parse", "main"])
    );
    assert_eq!(run(root, &["stash", "list", "--format=%H %gs"]), before);
}

#[test]
fn force_pushes_rewritten_branches_with_a_lease() {
    let (dir, repository) = fixture();
    let root = dir.path();
    let remote = tempfile::tempdir().unwrap();
    run(remote.path(), &["init", "-q", "--bare"]);
    run(
        root,
        &["remote", "add", "origin", remote.path().to_str().unwrap()],
    );
    run(root, &["push", "-q", "origin", "main", "feature"]);
    run(root, &["fetch", "-q", "origin"]);
    // Newer Git sets origin/HEAD on fetch; it must stay a symbolic ref.
    run(root, &["remote", "set-head", "origin", "main"]);

    let backups = tempfile::tempdir().unwrap();
    let report = repository
        .purge(&[PurgeTarget::Path(".env".into())], backups.path(), &|_| {})
        .unwrap();
    let rewritten: Vec<bonsai_git::RewrittenBranch> = report
        .refs
        .iter()
        .filter_map(|change| {
            let name = change.name.strip_prefix("refs/remotes/origin/")?;
            Some(bonsai_git::RewrittenBranch {
                name: name.to_owned(),
                old: change.old.as_ref()?.to_string(),
                new: change.new.as_ref()?.to_string(),
            })
        })
        .collect();
    assert_eq!(rewritten.len(), 2, "{rewritten:?}");
    assert_eq!(
        run(root, &["symbolic-ref", "refs/remotes/origin/HEAD"]).trim(),
        "refs/remotes/origin/main"
    );
    repository
        .push_rewritten("origin", &rewritten, None, None)
        .unwrap();
    assert_eq!(
        run(remote.path(), &["rev-parse", "main"]),
        run(root, &["rev-parse", "main"])
    );
    assert!(
        !in_history(remote.path(), ".env") || {
            // The bare remote keeps loose objects until its own gc, but no ref
            // reaches them.
            run(
                remote.path(),
                &["log", "--all", "--format=%H", "--", ".env"],
            )
            .trim()
            .is_empty()
        }
    );

    // Pushing again with the pre-purge lease is refused: the server moved on.
    let main = rewritten.iter().find(|b| b.name == "main").unwrap();
    let feature = rewritten.iter().find(|b| b.name == "feature").unwrap();
    let stale = bonsai_git::RewrittenBranch {
        name: "main".into(),
        old: main.old.clone(),
        new: feature.new.clone(),
    };
    let error = repository
        .push_rewritten("origin", &[stale], None, None)
        .unwrap_err();
    assert_eq!(
        error.sync_failure(),
        Some(bonsai_git::SyncFailure::StaleLease)
    );
}
