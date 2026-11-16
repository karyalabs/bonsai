use std::path::{Path, PathBuf};

use bonsai_git::{Git, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap();
}

#[test]
fn lists_tracked_files_but_not_untracked_or_ignored_ones() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src/ui")).unwrap();
    for (file, text) in [
        ("README.md", "hi\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/ui/a b.rs", "\n"),
        (".gitignore", "*.log\n"),
    ] {
        std::fs::write(root.join(file), text).unwrap();
    }
    run(root, &["add", "--all"]);
    std::fs::write(root.join("new.txt"), "untracked\n").unwrap();
    std::fs::write(root.join("debug.log"), "ignored\n").unwrap();
    // Deleted in the work tree only: still tracked.
    std::fs::remove_file(root.join("README.md")).unwrap();

    let files = repository.tracked_files().unwrap();
    assert_eq!(
        files,
        [".gitignore", "README.md", "src/main.rs", "src/ui/a b.rs"]
            .map(PathBuf::from)
            .to_vec()
    );
}
