use std::path::{Path, PathBuf};

use bonsai_git::{DiffOptions, DiffOutput, DiffTarget, Git, Oid, Repository, Whitespace};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    let mut full = vec![
        "-c",
        "user.name=Bonsai",
        "-c",
        "user.email=bonsai@example.com",
    ];
    full.extend_from_slice(args);
    git()
        .run(&git().command(full).cwd(path))
        .unwrap()
        .stdout_lossy()
}

fn patch(repository: &Repository, target: &DiffTarget) -> String {
    match repository.diff(target, DiffOptions::default()).unwrap() {
        DiffOutput::Patch(patch) => patch,
        DiffOutput::TooLarge { bytes } => panic!("unexpectedly large: {bytes}"),
    }
}

fn fixture() -> (tempfile::TempDir, PathBuf, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    std::fs::write(root.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
    std::fs::write(
        root.join("old-name.txt"),
        "a long enough line to detect a rename\n".repeat(5),
    )
    .unwrap();
    std::fs::write(root.join("delete-me.txt"), "bye\n").unwrap();
    std::fs::write(root.join("no-newline.txt"), "end").unwrap();
    std::fs::write(root.join("image.bin"), [0_u8, 159, 146, 150, 0, 1, 2]).unwrap();
    run(&root, &["add", "."]);
    run(&root, &["commit", "-qm", "first"]);
    (dir, root, repository)
}

#[test]
fn unstaged_and_staged_diffs_cover_renames_deletions_and_newlines() {
    let (_dir, root, repository) = fixture();
    std::fs::write(root.join("keep.txt"), "one\nzwei\nthree\n").unwrap();
    std::fs::write(root.join("no-newline.txt"), "end\n").unwrap();
    std::fs::remove_file(root.join("delete-me.txt")).unwrap();
    run(&root, &["mv", "old-name.txt", "new-name.txt"]);

    let unstaged = patch(&repository, &DiffTarget::Unstaged(Vec::new()));
    assert!(unstaged.contains("-two\n+zwei"), "{unstaged}");
    assert!(unstaged.contains("deleted file mode"), "{unstaged}");
    assert!(
        unstaged.contains("\\ No newline at end of file"),
        "{unstaged}"
    );

    let staged = patch(&repository, &DiffTarget::Staged(Vec::new()));
    assert!(staged.contains("rename from old-name.txt"), "{staged}");
    assert!(staged.contains("rename to new-name.txt"), "{staged}");

    let only_keep = patch(&repository, &DiffTarget::Unstaged(vec!["keep.txt".into()]));
    assert!(!only_keep.contains("delete-me.txt"));
}

#[test]
fn binary_and_unicode_changes() {
    let (_dir, root, repository) = fixture();
    std::fs::write(root.join("image.bin"), [0_u8, 1, 2, 3, 4, 5]).unwrap();
    std::fs::write(root.join("keep.txt"), "one\nдва 二 🌳\nthree\n").unwrap();
    let unstaged = patch(&repository, &DiffTarget::Unstaged(Vec::new()));
    assert!(unstaged.contains("GIT binary patch") || unstaged.contains("Binary files"));
    assert!(unstaged.contains("+два 二 🌳"), "{unstaged}");
}

#[test]
fn untracked_commit_and_range_diffs() {
    let (_dir, root, repository) = fixture();
    std::fs::write(root.join("fresh.txt"), "hello\n").unwrap();
    let untracked = patch(&repository, &DiffTarget::Untracked("fresh.txt".into()));
    assert!(untracked.contains("+++ b/fresh.txt"), "{untracked}");
    assert!(untracked.contains("+hello"));

    let first = Oid::new(run(&root, &["rev-parse", "HEAD"]).trim());
    let root_commit = patch(&repository, &DiffTarget::Commit(first.clone()));
    assert!(root_commit.contains("new file mode"), "{root_commit}");

    std::fs::write(root.join("keep.txt"), "uno\ntwo\nthree\n").unwrap();
    run(&root, &["commit", "-qam", "second"]);
    let second = Oid::new(run(&root, &["rev-parse", "HEAD"]).trim());
    let commit = patch(&repository, &DiffTarget::Commit(second.clone()));
    assert!(commit.contains("-one\n+uno"), "{commit}");
    assert!(!commit.contains("new file mode"));

    let range = patch(
        &repository,
        &DiffTarget::Range {
            from: first,
            to: second,
        },
    );
    assert_eq!(range, commit);
}

#[test]
fn whitespace_modes_and_size_limit() {
    let (_dir, root, repository) = fixture();
    std::fs::write(root.join("keep.txt"), "one\ntwo  \nthree\n").unwrap();
    let show = patch(&repository, &DiffTarget::Unstaged(Vec::new()));
    assert!(show.contains("+two  "));
    let ignored = repository
        .diff(
            &DiffTarget::Unstaged(Vec::new()),
            DiffOptions {
                whitespace: Whitespace::IgnoreChanges,
                ..DiffOptions::default()
            },
        )
        .unwrap();
    assert_eq!(ignored, DiffOutput::Patch(String::new()));

    let limited = repository
        .diff(
            &DiffTarget::Unstaged(Vec::new()),
            DiffOptions {
                max_bytes: Some(10),
                ..DiffOptions::default()
            },
        )
        .unwrap();
    assert!(matches!(limited, DiffOutput::TooLarge { bytes } if bytes > 10));
}
