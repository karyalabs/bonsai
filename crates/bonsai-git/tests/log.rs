use std::{fmt::Write as _, path::Path, time::Instant};

use bonsai_git::{Git, LogScope, Repository};

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
    ];
    full.extend_from_slice(args);
    git()
        .run(&git().command(full).cwd(path))
        .unwrap()
        .stdout_lossy()
}

#[test]
fn pages_cover_the_history_in_topological_order() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    assert_eq!(repository.log(&LogScope::All, 0, 10).unwrap(), []);
    assert_eq!(repository.log(&LogScope::Head, 0, 10).unwrap(), []);

    run(dir.path(), &["commit", "--allow-empty", "-qm", "base"]);
    run(dir.path(), &["switch", "-qc", "feature"]);
    run(
        dir.path(),
        &["commit", "--allow-empty", "-qm", "feature work"],
    );
    run(dir.path(), &["switch", "-q", "main"]);
    run(dir.path(), &["commit", "--allow-empty", "-qm", "main work"]);
    run(
        dir.path(),
        &["merge", "--no-ff", "-qm", "Merge feature", "feature"],
    );

    let all = repository.log(&LogScope::All, 0, 100).unwrap();
    let summaries: Vec<&str> = all.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries.len(), 4);
    assert_eq!(summaries[0], "Merge feature");
    assert_eq!(summaries[3], "base");
    assert_eq!(all[0].parents.len(), 2);
    assert_eq!(all[0].author_name, "Ada");

    let first = repository.log(&LogScope::All, 0, 2).unwrap();
    let second = repository.log(&LogScope::All, 2, 2).unwrap();
    assert_eq!([first, second].concat(), all);

    let feature = repository
        .log(&LogScope::Refs(vec!["feature".into()]), 0, 100)
        .unwrap();
    assert_eq!(feature.len(), 2);
}

/// `cargo test -p bonsai-git --test log -- --ignored` times a 100k-commit walk.
#[test]
#[ignore = "benchmark: builds a 100,000-commit repository"]
fn walks_one_hundred_thousand_commits() {
    let dir = tempfile::tempdir().unwrap();
    let repository = Repository::init(&git(), dir.path(), Some("main")).unwrap();
    // fast-import builds the history in a second or two.
    let mut input = String::new();
    for i in 0..100_000_u64 {
        let message = format!("commit {i}");
        let _ = write!(
            input,
            "commit refs/heads/main\nmark :{}\ncommitter Ada <ada@example.com> {} +0000\ndata {}\n{message}\n",
            i + 1,
            1_600_000_000 + i,
            message.len()
        );
        if i > 0 {
            let _ = writeln!(input, "from :{i}");
        }
        input.push('\n');
    }
    git()
        .run(
            &git()
                .command(["fast-import", "--quiet"])
                .cwd(dir.path())
                .stdin(input),
        )
        .unwrap();

    let started = Instant::now();
    let page = repository.log(&LogScope::All, 0, 500).unwrap();
    let first_page = started.elapsed();
    let started = Instant::now();
    let all = repository.log(&LogScope::All, 0, 100_000).unwrap();
    let whole = started.elapsed();
    assert_eq!(page.len(), 500);
    assert_eq!(all.len(), 100_000);
    eprintln!("first page of 500: {first_page:?}; all 100,000: {whole:?}");
    assert!(first_page.as_secs() < 5);
}
