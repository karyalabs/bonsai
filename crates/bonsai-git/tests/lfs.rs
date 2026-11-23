use std::path::Path;

use bonsai_git::{DiffOptions, DiffOutput, DiffTarget, Git, Repository, parse_lfs_patch};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap()
        .stdout_lossy()
}

#[test]
fn tracks_files_with_lfs_and_reads_their_pointers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repository = Repository::init(&git(), root, Some("main")).unwrap();
    if repository.lfs_version().is_none() {
        eprintln!("Git LFS isn't installed; skipping");
        return;
    }
    for (key, value) in [
        ("user.name", "Ada Lovelace"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        run(root, &["config", key, value]);
    }
    repository.lfs_track("*.psd").unwrap();
    assert_eq!(repository.lfs_tracked().unwrap(), ["*.psd"]);

    std::fs::write(root.join("cover.psd"), "abc").unwrap();
    run(root, &["add", "cover.psd"]);
    run(root, &["commit", "-qm", "Add the cover"]);
    assert!(run(root, &["show", "HEAD:cover.psd"]).contains("oid sha256:"));

    std::fs::write(root.join("cover.psd"), "abcdef").unwrap();
    let DiffOutput::Patch(patch) = repository
        .diff(&DiffTarget::Unstaged(Vec::new()), DiffOptions::default())
        .unwrap()
    else {
        panic!("a small patch");
    };
    let (before, after) = parse_lfs_patch(&patch).expect("an LFS pointer patch");
    assert_eq!(before.unwrap().size, 3);
    assert_eq!(after.unwrap().size, 6);

    repository.lfs_untrack("*.psd").unwrap();
    assert_eq!(repository.lfs_tracked().unwrap(), [] as [String; 0]);
}
