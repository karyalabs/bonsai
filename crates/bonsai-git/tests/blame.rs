use std::path::Path;

use bonsai_git::{Git, Repository};

fn git() -> Git {
    Git::locate(None).expect("git is installed")
}

fn commit(root: &Path, name: &str, email: &str, message: &str) {
    let g = git();
    g.run(&g.command(["add", "-A"]).cwd(root)).unwrap();
    let user = format!("user.name={name}");
    let mail = format!("user.email={email}");
    g.run(
        &g.command(["-c", &user, "-c", &mail, "commit", "-q", "-m", message])
            .cwd(root),
    )
    .unwrap();
}

#[test]
fn blames_each_line_on_the_commit_that_last_changed_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let repository = Repository::init(&git(), &root, Some("main")).unwrap();
    std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    commit(&root, "Ada", "ada@example.com", "Start the notes");
    std::fs::write(root.join("notes.txt"), "one\nTWO\nthree\n").unwrap();
    commit(&root, "Grace", "grace@example.com", "Shout two");
    std::fs::write(root.join("notes.txt"), "one\nTWO\nthree\nfour\n").unwrap();

    let blame = repository.blame(Path::new("notes.txt"), None).unwrap();
    let texts: Vec<&str> = blame.lines.iter().map(|l| l.text.as_str()).collect();
    assert_eq!(texts, ["one", "TWO", "three", "four"]);
    let authors: Vec<&str> = (0..4)
        .map(|i| blame.commit_of(i).unwrap().author_name.as_str())
        .collect();
    assert_eq!(authors, ["Ada", "Grace", "Ada", "Not Committed Yet"]);
    assert!(blame.commit_of(3).unwrap().is_uncommitted());
    assert_eq!(blame.commit_of(1).unwrap().summary, "Shout two");
    assert_eq!(
        blame.commit_of(1).unwrap().author_email,
        "grace@example.com"
    );
    assert!(blame.starts_run(0) && blame.starts_run(1) && blame.starts_run(2));
    assert_eq!(blame.lines[2].original_line, 3);
    let (oldest, newest) = blame.time_range();
    assert!(oldest > 0 && oldest <= newest);

    // At a commit, the work tree's change is not there.
    let at_head = repository
        .blame(Path::new("notes.txt"), Some("HEAD"))
        .unwrap();
    assert_eq!(at_head.lines.len(), 3);
    assert!(at_head.commits.iter().all(|c| !c.is_uncommitted()));
    assert!(
        repository
            .blame(Path::new("missing.txt"), Some("HEAD"))
            .is_err()
    );
}
