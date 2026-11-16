//! Partial patches built from real diffs and applied to real repositories.

use std::path::Path;

use bonsai_git::{
    ApplyMode, DiffOptions, DiffOutput, DiffTarget, Git, LinePosition, LineSide, Repository,
    partial_patch,
};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn run(path: &Path, args: &[&str]) -> String {
    git()
        .run(&git().command(args.iter().copied()).cwd(path))
        .unwrap()
        .stdout_lossy()
}

/// A repository with `file.txt` committed as `base`.
fn repo(base: &str) -> (tempfile::TempDir, Repository) {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    for (key, value) in [
        ("user.name", "Ada"),
        ("user.email", "ada@example.com"),
        ("commit.gpgsign", "false"),
        ("core.autocrlf", "false"),
    ] {
        run(dir.path(), &["config", key, value]);
    }
    std::fs::write(dir.path().join("file.txt"), base).unwrap();
    run(dir.path(), &["add", "--all"]);
    run(dir.path(), &["commit", "-qm", "base"]);
    (dir, repository)
}

fn patch(repository: &Repository, target: DiffTarget) -> String {
    match repository.diff(&target, DiffOptions::default()).unwrap() {
        DiffOutput::Patch(patch) => patch,
        DiffOutput::TooLarge { .. } => unreachable!(),
    }
}

fn unstaged(repository: &Repository) -> String {
    patch(repository, DiffTarget::Unstaged(Vec::new()))
}

fn staged(repository: &Repository) -> String {
    patch(repository, DiffTarget::Staged(Vec::new()))
}

/// The file as the index has it.
fn index(root: &Path) -> Vec<u8> {
    git()
        .run(&git().command(["show", ":file.txt"]).cwd(root))
        .unwrap()
        .stdout
}

fn work_tree(root: &Path) -> Vec<u8> {
    std::fs::read(root.join("file.txt")).unwrap()
}

fn old(line: usize) -> LinePosition {
    LinePosition::new(LineSide::Old, line)
}

fn new(line: usize) -> LinePosition {
    LinePosition::new(LineSide::New, line)
}

fn apply(
    repository: &Repository,
    patch: &str,
    from: LinePosition,
    to: LinePosition,
    mode: ApplyMode,
) {
    let partial = partial_patch(patch, "file.txt", from, to, mode).expect("lines are selected");
    repository
        .apply_patch(&partial, mode)
        .unwrap_or_else(|e| panic!("{e}\n{partial}"));
}

#[test]
fn stages_one_of_two_added_lines() {
    let (dir, repository) = repo("a\nb\nc\n");
    std::fs::write(dir.path().join("file.txt"), "a\nX\nb\nY\nc\n").unwrap();
    let diff = unstaged(&repository);
    apply(&repository, &diff, new(4), new(4), ApplyMode::Stage);
    assert_eq!(index(dir.path()), b"a\nb\nY\nc\n");
    // The work tree keeps both.
    assert_eq!(work_tree(dir.path()), b"a\nX\nb\nY\nc\n");
}

#[test]
fn stages_one_of_two_removed_lines() {
    let (dir, repository) = repo("a\nb\nc\nd\ne\n");
    std::fs::write(dir.path().join("file.txt"), "a\nc\ne\n").unwrap();
    let diff = unstaged(&repository);
    apply(&repository, &diff, old(2), old(2), ApplyMode::Stage);
    assert_eq!(index(dir.path()), b"a\nc\nd\ne\n");
}

#[test]
fn stages_a_changed_line_and_leaves_a_later_hunk() {
    let base = (1..=20).fold(String::new(), |mut base, n| {
        base.push_str("line ");
        base.push_str(&n.to_string());
        base.push('\n');
        base
    });
    let (dir, repository) = repo(&base);
    let changed = base
        .replace("line 3\n", "line three\n")
        .replace("line 18\n", "line eighteen\nextra\n");
    std::fs::write(dir.path().join("file.txt"), &changed).unwrap();
    let diff = unstaged(&repository);
    // Select from the removed "line 3" through the added "line three".
    apply(&repository, &diff, old(3), new(3), ApplyMode::Stage);
    assert_eq!(
        index(dir.path()),
        base.replace("line 3\n", "line three\n").as_bytes()
    );

    // The second hunk then stages on its own, after the first moved nothing.
    let diff = unstaged(&repository);
    apply(&repository, &diff, new(19), new(19), ApplyMode::Stage);
    assert_eq!(
        index(dir.path()),
        base.replace("line 3\n", "line three\n")
            .replace("line 18\n", "line 18\nextra\n")
            .as_bytes()
    );
}

#[test]
fn stages_a_line_added_after_a_last_line_without_newline() {
    let (dir, repository) = repo("a\nb");
    std::fs::write(dir.path().join("file.txt"), "a\nb\nc\nd\n").unwrap();
    let diff = unstaged(&repository);
    // Old "b" without newline becomes "b\n" plus "c" and "d": take b and c.
    apply(&repository, &diff, old(2), new(3), ApplyMode::Stage);
    assert_eq!(index(dir.path()), b"a\nb\nc\n");
}

#[test]
fn keeps_crlf_line_endings() {
    let (dir, repository) = repo("a\r\nb\r\nc\r\n");
    std::fs::write(dir.path().join("file.txt"), "a\r\nX\r\nb\r\nc\r\nY\r\n").unwrap();
    let diff = unstaged(&repository);
    apply(&repository, &diff, new(2), new(2), ApplyMode::Stage);
    assert_eq!(index(dir.path()), b"a\r\nX\r\nb\r\nc\r\n");
}

#[test]
fn unstages_some_staged_lines() {
    let (dir, repository) = repo("a\nb\nc\n");
    std::fs::write(dir.path().join("file.txt"), "a\nX\nb\nY\nc\n").unwrap();
    run(dir.path(), &["add", "file.txt"]);
    let diff = staged(&repository);
    apply(&repository, &diff, new(2), new(2), ApplyMode::Unstage);
    assert_eq!(index(dir.path()), b"a\nb\nY\nc\n");
    assert_eq!(work_tree(dir.path()), b"a\nX\nb\nY\nc\n");
}

#[test]
fn discards_some_lines_and_keeps_the_rest() {
    let (dir, repository) = repo("a\nb\nc\nd\n");
    std::fs::write(dir.path().join("file.txt"), "a\nX\nc\nd\nY\n").unwrap();
    let diff = unstaged(&repository);
    // Undo the "b" → "X" change only.
    apply(&repository, &diff, old(2), new(2), ApplyMode::Discard);
    assert_eq!(work_tree(dir.path()), b"a\nb\nc\nd\nY\n");
    assert_eq!(index(dir.path()), b"a\nb\nc\nd\n");
}

#[test]
fn a_selection_of_context_only_builds_nothing() {
    let (dir, repository) = repo("a\nb\nc\n");
    std::fs::write(dir.path().join("file.txt"), "a\nb\nc\nd\n").unwrap();
    let diff = unstaged(&repository);
    assert!(partial_patch(&diff, "file.txt", old(1), old(2), ApplyMode::Stage).is_none());
    assert!(partial_patch(&diff, "other.txt", new(4), new(4), ApplyMode::Stage).is_none());
}
